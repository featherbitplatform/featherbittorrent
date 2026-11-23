// @vitest-environment jsdom
/**
 * The desktop page around `App`: it connects to the default instance through the shell, and a launch
 * token This computer no longer accepts (the engine restarted with a new secret) is answered by asking
 * the shell again, never by the sign-in screen, whose routes This computer does not have.
 *
 * @module desktop/DesktopRoot.test
 */
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { useConnection } from '../connection'
import type { Connection } from '../connection'
import { UNREACHABLE_COPY } from '../live'

const shell = vi.hoisted(() => ({
  connect: vi.fn(),
  setTrayInstance: vi.fn(() => Promise.resolve()),
  seen: [] as Connection[],
}))
vi.mock('./bridge', () => ({
  connect: shell.connect,
  LOCAL_ID: 'local',
  THIS_COMPUTER: 'This computer',
  setTrayInstance: shell.setTrayInstance,
}))
vi.mock('./CloseChoice', () => ({ CloseChoiceHost: () => null }))
vi.mock('../App', () => ({
  default: function FakeApp() {
    const connection = useConnection()
    shell.seen.push(connection)
    return <div>workspace {connection.endpoint.kind === 'bearer' ? connection.endpoint.token : ''}</div>
  },
}))

import { DesktopRoot } from './DesktopRoot'

const BASE = 'http://127.0.0.1:53211'
const bearer = (token: string) => ({ kind: 'bearer' as const, base: BASE, token })

afterEach(() => {
  cleanup()
  shell.connect.mockReset()
  shell.seen.length = 0
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
  await screen.findByText('workspace first')
  expect(shell.connect).toHaveBeenCalledWith('local')
})

it('the_tray_names_this_computer_once_connected', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first'))
  render(<DesktopRoot />)
  await screen.findByText('workspace first')
  await waitFor(() => expect(shell.setTrayInstance).toHaveBeenCalledWith('This computer'))
})

it('a_rejected_launch_token_reconnects_without_signing_in', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first')).mockResolvedValueOnce(bearer('second'))
  render(<DesktopRoot />)
  await screen.findByText('workspace first')
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await screen.findByText('workspace second')
  expect(shell.connect).toHaveBeenLastCalledWith('local')
  expect(screen.queryByRole('button', { name: 'Sign in' }), 'the sign-in screen showed').toBeNull()
})

it('a_failed_reconnect_shows_cannot_reach_the_server', async () => {
  shell.connect
    .mockResolvedValueOnce(bearer('first'))
    .mockRejectedValueOnce('This computer is not running.')
    .mockResolvedValueOnce(bearer('third'))
  render(<DesktopRoot />)
  await screen.findByText('workspace first')
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await screen.findByText(UNREACHABLE_COPY)
  fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
  await screen.findByText('workspace third')
})

it('the_same_token_rejected_again_is_unreachable_not_a_loop', async () => {
  shell.connect.mockResolvedValue(bearer('stale'))
  render(<DesktopRoot />)
  await screen.findByText('workspace stale')
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await screen.findByText(UNREACHABLE_COPY)
  await waitFor(() => expect(shell.connect).toHaveBeenCalledTimes(2))
})

it('no_endpoint_at_startup_shows_cannot_reach_the_server', async () => {
  shell.connect.mockRejectedValueOnce('This computer is not running.')
  render(<DesktopRoot />)
  await screen.findByText(UNREACHABLE_COPY)
})
