// @vitest-environment jsdom
/**
 * The links and files the app was opened with: the queue, and the Add page it drains into. The shell's
 * calls are stand-ins; every instance is reached over a stubbed `fetch` that answers by address, so
 * "the folders came from the other instance" is a claim about the requests that left the page.
 *
 * @module desktop/OpenedQueue.test
 */
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { createConnection } from '../connection'
import type { Connection } from '../connection'
import { installDialogShim } from '../dialog-shim'
import type { OpenedItem } from './bridge'

const shell = vi.hoisted(() => ({
  connect: vi.fn(),
  takeOpened: vi.fn(),
  handler: null as ((item: OpenedItem) => void) | null,
}))
vi.mock('./bridge', () => ({
  connect: shell.connect,
  takeOpened: shell.takeOpened,
  onOpened: (handler: (item: OpenedItem) => void) => {
    shell.handler = handler
    return Promise.resolve(() => {
      shell.handler = null
    })
  },
}))

import { OpenedHost } from './OpenedAdd'
import { OpenedQueue } from './OpenedQueue'

installDialogShim()

const LOCAL = 'http://127.0.0.1:53211'
const PI = 'http://pi.local:8090'
const MAGNET = 'magnet:?xt=urn:btih:5a8062c076fa85e8056451c0d9aa04349ae27909&dn=Sintel'
const OTHER = 'magnet:?xt=urn:btih:aaaa&dn=Other'
const INSTANCES = [
  { id: 'local', name: 'This computer', url: '', saved_password: false },
  { id: 'pi', name: 'Pi at home', url: PI, saved_password: false },
]

interface Seen {
  url: string
  method: string
  body: string
}

let seen: Seen[] = []
let piSignIns: number[] = []

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
}

/** Two servers by address: This computer's roots and the Pi's, the Pi's sign-in answering each status in `piSignIns` in turn. */
async function fakeFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const url = String(input)
  const method = init?.method ?? 'GET'
  seen.push({ url, method, body: typeof init?.body === 'string' ? init.body : '' })
  const base = url.startsWith(PI) ? PI : LOCAL
  const path = url.slice(base.length)
  if (path === '/api/v1/fs/roots') return json(200, [base === PI ? '/pi-media' : '/downloads'])
  if (path.startsWith('/api/v1/fs/list')) return json(200, { path: '/', dirs: [] })
  if (path === '/api/v1/auth/session' && method === 'POST') {
    const status = piSignIns.shift() ?? 401
    return status === 200
      ? json(200, { token: 'pi-token', subject: 'alice' })
      : json(status, { error: { code: 'unauthorized', message: 'no' } })
  }
  if (path.split('?')[0] === '/api/v1/torrents' && method === 'POST') return json(201, { id: 'a1', name: 'Sintel' })
  return json(404, { error: { code: 'not_found', message: 'nothing' } })
}

let local: Connection

beforeEach(() => {
  seen = []
  piSignIns = []
  vi.stubGlobal('fetch', fakeFetch)
  local = createConnection({ kind: 'bearer', base: LOCAL, token: 'launch' })
  shell.connect.mockImplementation((id: string) =>
    Promise.resolve(
      id === 'pi'
        ? { kind: 'bearer', base: PI, token: null }
        : { kind: 'bearer', base: LOCAL, token: 'launch-again' },
    ),
  )
  shell.takeOpened.mockResolvedValue([])
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
  shell.connect.mockReset()
  shell.takeOpened.mockReset()
  shell.handler = null
})

function renderHost() {
  return render(<OpenedHost instances={INSTANCES} shown={{ id: 'local', connection: local }} />)
}

/** The Add dialog's own button, not the in-place sign-in's. */
function addButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: 'Add' })
}

function magnetField(): HTMLInputElement {
  return screen.getByLabelText('Magnet link')
}

function adds(): Seen[] {
  return seen.filter((call) => call.method === 'POST' && /\/api\/v1\/torrents(\?|$)/.test(call.url))
}

it('opened_magnet_opens_add_with_the_connected_instance', async () => {
  shell.takeOpened.mockResolvedValueOnce([{ kind: 'magnet', uri: MAGNET }])
  renderHost()

  await screen.findByText('Add torrent')
  expect(magnetField().value).toBe(MAGNET)
  expect((screen.getByLabelText('Add to') as HTMLSelectElement).value).toBe('local')
  await screen.findByText('/downloads')
  expect(shell.connect).not.toHaveBeenCalled()

  await waitFor(() => expect(addButton().disabled).toBe(false))
  fireEvent.click(addButton())
  await waitFor(() => expect(screen.queryByText('Add torrent')).toBeNull())
  expect(adds()).toHaveLength(1)
  expect(adds()[0]?.url).toBe(`${LOCAL}/api/v1/torrents`)
  expect(JSON.parse(adds()[0]?.body ?? '{}')).toMatchObject({ magnet: MAGNET, save_path: '/downloads' })
})

it('changing_instance_reloads_folders', async () => {
  piSignIns = [200]
  renderHost()
  await waitFor(() => expect(shell.handler).not.toBeNull())
  act(() => shell.handler?.({ kind: 'magnet', uri: MAGNET }))
  await screen.findByText('/downloads')

  fireEvent.change(screen.getByLabelText('Add to'), { target: { value: 'pi' } })
  expect(shell.connect).toHaveBeenCalledWith('pi')
  // The Pi needs a sign-in first: the form stands where the folders were.
  await screen.findByLabelText('User name')
  expect(screen.queryByText('/downloads')).toBeNull()
  fireEvent.change(screen.getByLabelText('User name'), { target: { value: 'alice' } })
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'hunter2' } })
  fireEvent.click(screen.getByRole('button', { name: 'Sign in' }))

  await screen.findByText('/pi-media')
  expect(seen.some((call) => call.url === `${PI}/api/v1/fs/roots`)).toBe(true)
  await waitFor(() => expect(addButton().disabled).toBe(false))
  fireEvent.click(addButton())
  await waitFor(() => expect(adds()).toHaveLength(1))
  expect(adds()[0]?.url).toBe(`${PI}/api/v1/torrents`)
  expect(JSON.parse(adds()[0]?.body ?? '{}')).toMatchObject({ magnet: MAGNET, save_path: '/pi-media' })
})

it('pending_link_survives_failed_sign_in', async () => {
  piSignIns = [401, 200]
  shell.takeOpened.mockResolvedValueOnce([{ kind: 'magnet', uri: MAGNET }])
  renderHost()
  await screen.findByText('/downloads')

  fireEvent.change(screen.getByLabelText('Add to'), { target: { value: 'pi' } })
  await screen.findByLabelText('User name')
  fireEvent.change(screen.getByLabelText('User name'), { target: { value: 'alice' } })
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'wrong' } })
  fireEvent.click(screen.getByRole('button', { name: 'Sign in' }))

  await screen.findByText('Wrong user name or password.')
  // Still on the Add page, with the link where it was and Add shut until there is a folder.
  expect(screen.getByText('Add torrent')).toBeTruthy()
  expect(magnetField().value).toBe(MAGNET)
  expect(addButton().disabled).toBe(true)
  expect(adds()).toHaveLength(0)

  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'hunter2' } })
  fireEvent.click(screen.getByRole('button', { name: 'Sign in' }))
  await screen.findByText('/pi-media')
  expect(magnetField().value).toBe(MAGNET)
  await waitFor(() => expect(addButton().disabled).toBe(false))
  fireEvent.click(addButton())
  await waitFor(() => expect(screen.queryByText('Add torrent')).toBeNull())
  expect(adds()).toHaveLength(1)
})

it('cancel_drops_only_the_current_item', async () => {
  shell.takeOpened.mockResolvedValueOnce([
    { kind: 'magnet', uri: MAGNET },
    { kind: 'magnet', uri: OTHER },
  ])
  renderHost()
  await screen.findByText('/downloads')
  expect(magnetField().value).toBe(MAGNET)

  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  // The next one opens, and nothing was added.
  await waitFor(() => expect(magnetField().value).toBe(OTHER))
  expect(adds()).toHaveLength(0)

  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  await waitFor(() => expect(screen.queryByText('Add torrent')).toBeNull())

  // The queue itself: a drop names one item, and a second drop of the same id takes nothing else.
  const queue = new OpenedQueue()
  queue.push({ kind: 'magnet', uri: MAGNET })
  queue.push({ kind: 'magnet', uri: OTHER })
  const first = queue.current()
  expect(first?.item).toEqual({ kind: 'magnet', uri: MAGNET })
  queue.drop(first?.id ?? 0)
  queue.drop(first?.id ?? 0)
  expect(queue.size()).toBe(1)
  expect(queue.current()?.item).toEqual({ kind: 'magnet', uri: OTHER })
})

it('an_opened_file_is_picked_and_a_refused_one_is_said', async () => {
  shell.takeOpened.mockResolvedValueOnce([
    { kind: 'refused', name: 'Huge.torrent', message: 'Huge.torrent is too large to be a torrent.' },
    { kind: 'file', name: 'Sintel.torrent', bytes_b64: btoa('d4:infoee') },
  ])
  renderHost()
  await screen.findByText('Huge.torrent is too large to be a torrent.')
  expect(screen.queryByText('Add torrent')).toBeNull()
  fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }))

  await screen.findByText('Add torrent')
  expect(screen.getByRole('tab', { name: 'Torrent file' }).getAttribute('aria-selected')).toBe('true')
  await screen.findByText(/Sintel\.torrent/)
  await screen.findByText('/downloads')
  await waitFor(() => expect(addButton().disabled).toBe(false))
  fireEvent.click(addButton())
  await waitFor(() => expect(adds()).toHaveLength(1))
  expect(adds()[0]?.url).toContain(`${LOCAL}/api/v1/torrents?`)
})

it('a_failed_connect_still_shows_the_dialog_with_cancel', async () => {
  shell.connect.mockImplementation((id: string) =>
    id === 'pi' ? Promise.reject('Pi at home gives no answer.') : Promise.resolve({ kind: 'bearer', base: LOCAL, token: 'x' }),
  )
  shell.takeOpened.mockResolvedValueOnce([{ kind: 'magnet', uri: MAGNET }])
  // The window shows the Pi, which is down: the dialog connects on its own and is refused.
  render(<OpenedHost instances={INSTANCES} shown={{ id: 'pi', connection: null }} />)
  await screen.findByText('Pi at home gives no answer.')
  expect(screen.getByText('Add torrent')).toBeTruthy()
  expect(magnetField().value).toBe(MAGNET)
  expect(addButton().disabled).toBe(true)

  // Another instance can be picked from there.
  fireEvent.change(screen.getByLabelText('Add to'), { target: { value: 'local' } })
  await screen.findByText('/downloads')
  expect(screen.queryByText('Pi at home gives no answer.')).toBeNull()

  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  await waitFor(() => expect(screen.queryByText('Add torrent')).toBeNull())
})
