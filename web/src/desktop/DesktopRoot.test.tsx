// @vitest-environment jsdom
/**
 * The desktop page around `App`: it connects to the default instance through the shell, and a launch
 * token This computer no longer accepts (the engine restarted with a new secret) is answered by asking
 * the shell again, never by the sign-in screen, whose routes This computer does not have.
 *
 * @module desktop/DesktopRoot.test
 */
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { useEffect } from 'react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { useConnection } from '../connection'
import type { Connection } from '../connection'
import { UNREACHABLE_COPY } from '../live'

const shell = vi.hoisted(() => ({
  connect: vi.fn(),
  listInstances: vi.fn(),
  probeInstance: vi.fn(),
  savePassword: vi.fn(),
  seen: [] as Connection[],
  mounts: 0,
}))
vi.mock('./bridge', () => ({
  connect: shell.connect,
  listInstances: shell.listInstances,
  probeInstance: shell.probeInstance,
  savePassword: shell.savePassword,
  addInstance: vi.fn(),
  removeInstance: vi.fn(),
  setDefaultInstance: vi.fn(),
  forgetPassword: vi.fn(),
  LOCAL_ID: 'local',
}))
vi.mock('../App', () => ({
  default: function FakeApp() {
    const connection = useConnection()
    shell.seen.push(connection)
    useEffect(() => {
      shell.mounts += 1
    }, [])
    const endpoint = connection.endpoint
    return (
      <div>
        <div>
          workspace {endpoint.kind === 'bearer' ? `${endpoint.base} ${endpoint.token ?? 'signed out'}` : ''}
        </div>
        <button type="button" onClick={() => void connection.api.torrents().catch(() => {})}>
          read torrents
        </button>
      </div>
    )
  },
}))

import { DesktopRoot } from './DesktopRoot'

const BASE = 'http://127.0.0.1:53211'
const PI = 'http://pi.local:8090'
const bearer = (token: string) => ({ kind: 'bearer' as const, base: BASE, token })
const LOCAL = { id: 'local', name: 'This computer', url: '', saved_password: false }
const PI_ITEM = { id: 'pi', name: 'Pi at home', url: PI, saved_password: false }
const list = (fallback: string) => ({ default: fallback, items: [LOCAL, PI_ITEM], keychain: true })

beforeEach(() => {
  shell.listInstances.mockResolvedValue({ default: 'local', items: [LOCAL], keychain: true })
  shell.probeInstance.mockResolvedValue({ reachable: true, cors_ok: true })
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
  shell.connect.mockReset()
  shell.listInstances.mockReset()
  shell.probeInstance.mockReset()
  shell.savePassword.mockReset()
  shell.seen.length = 0
  shell.mounts = 0
})

/** What a `401` does to a session: the client ends the generation it was sent under. */
function rejectToken(connection: Connection): void {
  act(() => {
    connection.session.endSession(connection.session.generation())
  })
}

it('connects_to_this_computer_and_renders_the_app_on_its_endpoint', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first'))
  render(<DesktopRoot />)
  await screen.findByText(`workspace ${BASE} first`)
  expect(shell.connect).toHaveBeenCalledWith('local')
})

it('a_rejected_launch_token_reconnects_without_signing_in', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first')).mockResolvedValueOnce(bearer('second'))
  render(<DesktopRoot />)
  await screen.findByText(`workspace ${BASE} first`)
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await screen.findByText(`workspace ${BASE} second`)
  expect(shell.connect).toHaveBeenLastCalledWith('local')
  expect(screen.queryByRole('button', { name: 'Sign in' }), 'the sign-in screen showed').toBeNull()
})

it('a_failed_reconnect_shows_cannot_reach_the_server', async () => {
  shell.connect
    .mockResolvedValueOnce(bearer('first'))
    .mockRejectedValueOnce('This computer is not running.')
    .mockResolvedValueOnce(bearer('third'))
  render(<DesktopRoot />)
  await screen.findByText(`workspace ${BASE} first`)
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await screen.findByText(UNREACHABLE_COPY)
  fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
  await screen.findByText(`workspace ${BASE} third`)
})

it('the_same_token_rejected_again_is_unreachable_not_a_loop', async () => {
  shell.connect.mockResolvedValue(bearer('stale'))
  render(<DesktopRoot />)
  await screen.findByText(`workspace ${BASE} stale`)
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await screen.findByText(UNREACHABLE_COPY)
  await waitFor(() => expect(shell.connect).toHaveBeenCalledTimes(2))
})

it('no_endpoint_at_startup_shows_cannot_reach_the_server', async () => {
  shell.connect.mockRejectedValueOnce('This computer is not running.')
  render(<DesktopRoot />)
  await screen.findByText(UNREACHABLE_COPY)
})

/** Opens the switcher and picks `name`. */
function switchTo(name: RegExp): void {
  fireEvent.click(screen.getByRole('button', { name: /^Instance:/ }))
  fireEvent.click(screen.getByRole('menuitemradio', { name }))
}

it('switching_remounts_with_a_new_connection', async () => {
  shell.listInstances.mockResolvedValue(list('local'))
  shell.connect
    .mockResolvedValueOnce(bearer('first'))
    .mockResolvedValueOnce({ kind: 'bearer', base: PI, token: null })
    .mockResolvedValueOnce(bearer('again'))
  render(<DesktopRoot />)
  await screen.findByText(`workspace ${BASE} first`)
  const before = shell.seen[shell.seen.length - 1]!
  expect(shell.mounts).toBe(1)

  switchTo(/Pi at home/)
  await screen.findByText(`workspace ${PI} signed out`)
  expect(shell.connect).toHaveBeenLastCalledWith('pi')
  expect(shell.probeInstance).toHaveBeenCalledWith('pi')
  const after = shell.seen[shell.seen.length - 1]!
  expect(after, 'the remote instance reused the old connection').not.toBe(before)
  expect(shell.mounts, 'App was not remounted for the new instance').toBe(2)
  expect(screen.getByRole('button', { name: 'Instance: Pi at home' })).toBeTruthy()

  switchTo(/This computer/)
  await screen.findByText(`workspace ${BASE} again`)
  expect(shell.mounts).toBe(3)
})

it('default_instance_unreachable_still_shows_switcher', async () => {
  shell.listInstances.mockResolvedValue(list('pi'))
  shell.probeInstance.mockResolvedValue({ reachable: false, cors_ok: false })
  shell.connect.mockResolvedValueOnce(bearer('local token'))
  render(<DesktopRoot />)
  await screen.findByText(UNREACHABLE_COPY)
  expect(shell.probeInstance).toHaveBeenCalledWith('pi')
  expect(shell.connect, 'a server that is down was dialled anyway').not.toHaveBeenCalled()
  expect(screen.getByRole('button', { name: 'Instance: Pi at home' })).toBeTruthy()
  expect(screen.queryByText(/cors_origins/)).toBeNull()

  switchTo(/This computer/)
  await screen.findByText(`workspace ${BASE} local token`)
  expect(shell.connect).toHaveBeenCalledWith('local')
})

it('a_remote_instance_that_stops_answering_shows_unreachable_with_the_switcher', async () => {
  shell.listInstances.mockResolvedValue(list('pi'))
  shell.connect.mockResolvedValueOnce({ kind: 'bearer', base: PI, token: 'session' })
  render(<DesktopRoot />)
  await screen.findByText(`workspace ${PI} session`)
  shell.probeInstance.mockResolvedValue({ reachable: false, cors_ok: false })
  vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('Failed to fetch')))
  fireEvent.click(screen.getByRole('button', { name: 'read torrents' }))
  await screen.findByText(UNREACHABLE_COPY)
  expect(screen.getByRole('button', { name: 'Instance: Pi at home' })).toBeTruthy()
  expect(screen.queryByText(/^workspace/), 'the workspace stayed up on a dead server').toBeNull()
})

it('cors_refusal_shows_the_origin_to_add', async () => {
  shell.listInstances.mockResolvedValue(list('pi'))
  shell.connect.mockResolvedValueOnce({ kind: 'bearer', base: PI, token: null })
  render(<DesktopRoot />)
  await screen.findByText(`workspace ${PI} signed out`)
  // The server answers, but not to this origin: from inside the webview that is a network error.
  shell.probeInstance.mockResolvedValue({ reachable: true, cors_ok: false })
  vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('Failed to fetch')))
  fireEvent.click(screen.getByRole('button', { name: 'read torrents' }))
  await screen.findByText(UNREACHABLE_COPY)
  const hint = screen.getByText(/cors_origins/)
  expect(hint.textContent).toBe(`Add this to the server's cors_origins: ${window.location.origin}`)
  expect(screen.getByRole('button', { name: 'Instance: Pi at home' })).toBeTruthy()
})

it('a_failed_request_to_a_server_that_still_answers_keeps_the_workspace', async () => {
  shell.listInstances.mockResolvedValue(list('pi'))
  shell.connect.mockResolvedValueOnce({ kind: 'bearer', base: PI, token: 'session' })
  render(<DesktopRoot />)
  await screen.findByText(`workspace ${PI} session`)
  vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('Failed to fetch')))
  fireEvent.click(screen.getByRole('button', { name: 'read torrents' }))
  await waitFor(() => expect(shell.probeInstance).toHaveBeenCalledTimes(2))
  expect(screen.getByText(`workspace ${PI} session`)).toBeTruthy()
})
