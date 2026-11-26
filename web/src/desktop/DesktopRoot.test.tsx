// @vitest-environment jsdom
/**
 * The desktop page around `App`: it connects to the default instance through the shell, and a launch
 * token This computer no longer accepts (the engine restarted with a new secret) is answered by asking
 * the shell again, never by the sign-in screen, whose routes This computer does not have. The page is
 * rendered inside a stand-in for the title bar's slots, which is where the switcher goes.
 *
 * @module desktop/DesktopRoot.test
 */
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { Fragment, useEffect, useMemo, useState, useSyncExternalStore } from 'react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { TitleBarSlotsContext } from '../components/titleBarSlots'
import type { TitleBarSlots } from '../components/titleBarSlots'
import { useConnection } from '../connection'
import { useSavePassword } from '../savePassword'
import { useDesktopDownload } from '../desktopDownload'
import { useDesktopSettings } from '../desktopSettings'
import { effectiveBindings, KEY_BINDINGS, setOverrides } from '../keys'
import type { Connection } from '../connection'
import { UNREACHABLE_COPY } from '../live'
import { installDialogShim } from '../dialog-shim'

const shell = vi.hoisted(() => ({
  connect: vi.fn(),
  listInstances: vi.fn(),
  probeInstance: vi.fn(),
  savePassword: vi.fn(),
  chooseDownloadPlace: vi.fn(() => Promise.resolve('place-1' as string | null)),
  saveDownload: vi.fn(() => Promise.resolve()),
  cancelDownload: vi.fn(() => Promise.resolve()),
  setTrayInstance: vi.fn(() => Promise.resolve()),
  takeOpened: vi.fn(() => Promise.resolve([] as unknown[])),
  getShortcuts: vi.fn(() => Promise.resolve([])),
  startupProblem: vi.fn(() => Promise.resolve({ state: 'fine' } as unknown)),
  finishApply: null as ((problem: string | null) => Promise<void>) | null,
  seen: [] as Connection[],
  mounts: 0,
  hostMounts: 0,
}))
vi.mock('./bridge', () => ({
  connect: shell.connect,
  listInstances: shell.listInstances,
  probeInstance: shell.probeInstance,
  savePassword: shell.savePassword,
  chooseDownloadPlace: shell.chooseDownloadPlace,
  saveDownload: shell.saveDownload,
  cancelDownload: shell.cancelDownload,
  addInstance: vi.fn(),
  removeInstance: vi.fn(),
  setDefaultInstance: vi.fn(),
  forgetPassword: vi.fn(),
  LOCAL_ID: 'local',
  THIS_COMPUTER: 'This computer',
  setTrayInstance: shell.setTrayInstance,
  takeOpened: shell.takeOpened,
  onOpened: () => Promise.resolve(() => {}),
  startupProblem: shell.startupProblem,
  quitApp: vi.fn(),
  getShortcuts: shell.getShortcuts,
}))
vi.mock('./DesktopSettings', () => ({
  INSTANCES_SECTION: 'settings-desktop-instances',
  SERVING_SECTION: 'settings-desktop-serving',
  WindowSection: () => null,
  InstancesSection: () => null,
  DesktopShortcuts: () => null,
  ServingSection: function FakeServingSection({
    onServingApplying,
    onServingChanged,
  }: {
    onServingApplying(): void
    onServingChanged(problem: string | null): Promise<void>
  }) {
    return (
      <>
        <button type="button" onClick={() => void onServingChanged(null)}>
          serving applied
        </button>
        <button
          type="button"
          onClick={() => {
            onServingApplying()
            shell.finishApply = onServingChanged
          }}
        >
          start apply
        </button>
      </>
    )
  },
}))
vi.mock('./CloseChoice', () => ({
  CloseChoiceHost: function FakeCloseChoiceHost() {
    useEffect(() => {
      shell.hostMounts += 1
    }, [])
    return <div>close question ready</div>
  },
}))
vi.mock('../App', () => ({
  default: function FakeApp() {
    const connection = useConnection()
    shell.seen.push(connection)
    useEffect(() => {
      shell.mounts += 1
    }, [])
    const endpoint = connection.endpoint
    const offer = useSavePassword()
    const save = useDesktopDownload()
    const downloadState = useSyncExternalStore(
      save === null ? noSubscribers : save.subscribe,
      () => save?.current('a:0')?.state ?? 'nothing',
    )
    const desktop = useDesktopSettings()
    const request = desktop?.request ?? null
    return (
      <div>
        {request !== null && <div>settings at {request.section}</div>}
        {desktop?.sections.map((section) => <Fragment key={section.id}>{section.render()}</Fragment>)}
        <div>
          workspace {endpoint.kind === 'bearer' ? `${endpoint.base} ${endpoint.token ?? 'signed out'}` : ''}
        </div>
        <button type="button" onClick={() => void connection.api.torrents().catch(() => {})}>
          read torrents
        </button>
        {save !== null && (
          <button
            type="button"
            onClick={() =>
              void save.choose('movie.mkv').then((handle) =>
                handle === null
                  ? Promise.resolve()
                  : save.run('a:0', handle, 'http://127.0.0.1:53211/stream'),
              )
            }
          >
            save a file
          </button>
        )}
        {save !== null && <span>download {downloadState}</span>}
        {offer !== null && (
          <button type="button" onClick={() => void offer.save('alice', 'hunter2')}>
            keep password
          </button>
        )}
      </div>
    )
  },
}))

import { DesktopRoot } from './DesktopRoot'

installDialogShim()

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
  shell.chooseDownloadPlace.mockClear()
  shell.saveDownload.mockClear()
  shell.cancelDownload.mockClear()
  shell.setTrayInstance.mockClear()
  shell.takeOpened.mockReset()
  shell.takeOpened.mockResolvedValue([])
  shell.startupProblem.mockReset()
  shell.startupProblem.mockResolvedValue({ state: 'fine' })
  shell.seen.length = 0
  shell.mounts = 0
  shell.hostMounts = 0
})

/** A stand-in for `DesktopChrome`: the title bar's switcher slot, then the page. */
function Frame({ children }: { children: ReactNode }) {
  const [switcher, setSwitcher] = useState<HTMLElement | null>(null)
  const slots = useMemo<TitleBarSlots>(() => ({ switcher, lead: null, actions: null }), [switcher])
  return (
    <TitleBarSlotsContext.Provider value={slots}>
      <div data-testid="title-bar-switcher" ref={setSwitcher} />
      <div data-testid="page">{children}</div>
    </TitleBarSlotsContext.Provider>
  )
}

function renderRoot() {
  return render(
    <Frame>
      <DesktopRoot />
    </Frame>,
  )
}

/** A `subscribe` for `useSyncExternalStore` where the context has no store: it never calls back. */
function noSubscribers(notify: () => void): () => void {
  void notify
  return () => {}
}

/** What a `401` does to a session: the client ends the generation it was sent under. */
function rejectToken(connection: Connection): void {
  act(() => {
    connection.session.endSession(connection.session.generation())
  })
}

it('connects_to_this_computer_and_renders_the_app_on_its_endpoint', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  expect(shell.connect).toHaveBeenCalledWith('local')
})

it('the_players_download_goes_through_the_shell_for_the_instance_shown', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  fireEvent.click(screen.getByRole('button', { name: 'save a file' }))
  await waitFor(() =>
    expect(shell.saveDownload).toHaveBeenCalledWith(
      'local',
      'place-1',
      `${BASE}/stream`,
      expect.any(Function),
    ),
  )
  expect(shell.chooseDownloadPlace).toHaveBeenCalledWith('movie.mkv')
})

it('switching instances keeps a running download', async () => {
  // The store of running downloads lives above the per-instance page: switching away and back
  // neither loses a running download nor shows one instance's download as another instance's.
  shell.listInstances.mockResolvedValue(list('local'))
  shell.chooseDownloadPlace.mockResolvedValueOnce('place-1')
  let finishSave: () => void = () => {}
  shell.saveDownload.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        finishSave = resolve
      }),
  )
  shell.connect
    .mockResolvedValueOnce(bearer('first'))
    .mockResolvedValueOnce({ kind: 'bearer', base: PI, token: null })
    .mockResolvedValueOnce(bearer('again'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  fireEvent.click(screen.getByRole('button', { name: 'save a file' }))
  await screen.findByText('download saving')
  // The download keeps running on This computer while the page shows the Pi.
  switchTo(/Pi at home/)
  await screen.findByText(`workspace ${PI} signed out`)
  expect(
    screen.getByText('download nothing') !== null,
    'the running download leaked onto the other instance',
  ).toBe(true)
  switchTo(/This computer/)
  await screen.findByText(`workspace ${BASE} again`)
  expect(
    screen.getByText('download saving') !== null,
    'switching away and back lost the running download',
  ).toBe(true)
  act(() => {
    finishSave()
  })
  await screen.findByText('download saved')
})

it('the_page_column_hands_its_height_to_the_screen', async () => {
  // The frame sizes `h-dvh` screens as `height: 100%`: the box the screens go into has to be a
  // flex item that takes the rest of the window and a flex column itself, or the percentage is void.
  shell.connect.mockResolvedValueOnce(bearer('first'))
  renderRoot()
  const app = (await screen.findByText(`workspace ${BASE} first`)).parentElement!
  const column = app.parentElement!
  expect(column.className.split(' ')).toEqual(
    expect.arrayContaining(['flex', 'min-h-0', 'flex-1', 'flex-col', 'overflow-auto']),
  )
  expect(column.parentElement, 'the page column is not a child of the frame').toBe(screen.getByTestId('page'))
})

it('a_rejected_launch_token_reconnects_without_signing_in', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first')).mockResolvedValueOnce(bearer('second'))
  renderRoot()
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
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await screen.findByText(UNREACHABLE_COPY)
  fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
  await screen.findByText(`workspace ${BASE} third`)
})

it('the_same_token_rejected_again_is_unreachable_not_a_loop', async () => {
  shell.connect.mockResolvedValue(bearer('stale'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} stale`)
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await screen.findByText(UNREACHABLE_COPY)
  await waitFor(() => expect(shell.connect).toHaveBeenCalledTimes(2))
})

it('no_endpoint_at_startup_shows_cannot_reach_the_server', async () => {
  shell.connect.mockRejectedValueOnce('This computer is not running.')
  renderRoot()
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
  renderRoot()
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
  renderRoot()
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
  renderRoot()
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
  renderRoot()
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
  renderRoot()
  await screen.findByText(`workspace ${PI} session`)
  vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('Failed to fetch')))
  fireEvent.click(screen.getByRole('button', { name: 'read torrents' }))
  await waitFor(() => expect(shell.probeInstance).toHaveBeenCalledTimes(2))
  expect(screen.getByText(`workspace ${PI} session`)).toBeTruthy()
})

it('failed_password_save_is_shown', async () => {
  shell.listInstances.mockResolvedValue(list('pi'))
  shell.connect.mockResolvedValueOnce({ kind: 'bearer', base: PI, token: null })
  shell.savePassword.mockRejectedValueOnce('The keychain did not save the password: locked')
  renderRoot()
  await screen.findByText(`workspace ${PI} signed out`)
  fireEvent.click(screen.getByRole('button', { name: 'keep password' }))
  await screen.findByText('The keychain did not save the password: locked')
  expect(shell.savePassword).toHaveBeenCalledWith('pi', 'alice', 'hunter2')
  fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }))
  expect(screen.queryByText('The keychain did not save the password: locked')).toBeNull()
})

it('the_tray_names_this_computer_once_connected', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  await waitFor(() => expect(shell.setTrayInstance).toHaveBeenCalledWith('This computer'))
})

it('the_switcher_sits_in_the_title_bar', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  const switcher = screen.getByRole('button', { name: 'Instance: This computer' })
  expect(screen.getByTestId('title-bar-switcher').contains(switcher), 'the switcher is not in the title bar').toBe(
    true,
  )
  expect(screen.getByTestId('page').contains(switcher), 'the switcher is drawn in the page as well').toBe(false)
})

it('the_tray_gets_the_instance_name_and_the_close_question_survives_a_switch', async () => {
  shell.listInstances.mockResolvedValue(list('local'))
  shell.connect
    .mockResolvedValueOnce(bearer('first'))
    .mockResolvedValueOnce({ kind: 'bearer', base: PI, token: 'session' })
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  await waitFor(() => expect(shell.setTrayInstance).toHaveBeenLastCalledWith('This computer'))
  expect(screen.getByText('close question ready')).toBeTruthy()
  expect(shell.hostMounts).toBe(1)

  switchTo(/Pi at home/)
  await screen.findByText(`workspace ${PI} session`)
  await waitFor(() => expect(shell.setTrayInstance).toHaveBeenLastCalledWith('Pi at home'))
  expect(shell.setTrayInstance, 'the tray was handed a raw instance id').not.toHaveBeenCalledWith('pi')
  expect(screen.getByText('close question ready')).toBeTruthy()
  expect(shell.hostMounts, 'the close question was remounted by the switch').toBe(1)
})

it('the_close_question_is_ready_while_connecting_and_unreachable', async () => {
  let answer: (value: unknown) => void = () => {}
  shell.connect.mockReturnValueOnce(new Promise((resolve) => (answer = resolve)))
  renderRoot()
  await waitFor(() => expect(shell.connect).toHaveBeenCalled())
  expect(screen.getByText('close question ready')).toBeTruthy()
  await act(async () => {
    answer(Promise.reject('This computer is not running.'))
  })
  await screen.findByText(UNREACHABLE_COPY)
  expect(screen.getByText('close question ready')).toBeTruthy()
  expect(shell.hostMounts).toBe(1)
})

it('the_tray_names_the_instance_shown_in_every_phase', async () => {
  shell.listInstances.mockResolvedValue(list('local'))
  shell.connect.mockResolvedValueOnce(bearer('first'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  await waitFor(() => expect(shell.setTrayInstance).toHaveBeenLastCalledWith('This computer'))

  // Switched to an instance that is down: the window shows the Pi, so the tray names the Pi.
  shell.probeInstance.mockResolvedValue({ reachable: false, cors_ok: false })
  switchTo(/Pi at home/)
  await screen.findByText(UNREACHABLE_COPY)
  await waitFor(() => expect(shell.setTrayInstance).toHaveBeenLastCalledWith('Pi at home'))
})

it('an_opened_link_survives_a_switch', async () => {
  vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('Failed to fetch')))
  const magnet = 'magnet:?xt=urn:btih:aaaa'
  shell.takeOpened.mockResolvedValueOnce([{ kind: 'magnet', uri: magnet }])
  shell.listInstances.mockResolvedValue(list('local'))
  shell.connect.mockResolvedValueOnce(bearer('first')).mockResolvedValue({ kind: 'bearer', base: PI, token: 'session' })
  renderRoot()
  const field = (await screen.findByLabelText('Magnet link')) as HTMLInputElement
  expect(field.value).toBe(magnet)

  switchTo(/Pi at home/)
  await screen.findByText(`workspace ${PI} session`)
  expect((screen.getByLabelText('Magnet link') as HTMLInputElement).value).toBe(magnet)
})

it('manage_instances_opens_the_instances_section', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  fireEvent.click(screen.getByRole('button', { name: /^Instance:/ }))
  fireEvent.click(screen.getByRole('menuitem', { name: 'Manage instances' }))
  await screen.findByText('settings at settings-desktop-instances')
  expect(screen.queryByRole('heading', { name: 'Instances', level: 1 }), 'the standalone manager opened').toBeNull()
})

it('serving_link_opens_serving', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first')).mockResolvedValueOnce(bearer('restarted'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  fireEvent.click(screen.getByRole('button', { name: 'serving applied' }))
  await screen.findByText('settings at settings-desktop-serving')
})

it('a_serving_change_reconnects_this_computer_at_once', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first')).mockResolvedValueOnce(bearer('restarted'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  fireEvent.click(screen.getByRole('button', { name: 'serving applied' }))
  await screen.findByText(`workspace ${BASE} restarted`)
  await screen.findByText('settings at settings-desktop-serving')
  expect(shell.connect).toHaveBeenCalledTimes(2)
})

it('a_serving_change_that_kept_the_secret_does_not_remount', async () => {
  shell.connect.mockResolvedValue(bearer('same'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} same`)
  fireEvent.click(screen.getByRole('button', { name: 'serving applied' }))
  await waitFor(() => expect(shell.connect).toHaveBeenCalledTimes(2))
  expect(shell.mounts).toBe(1)
})

it('the_saved_shortcuts_are_applied_at_start', async () => {
  const binding = KEY_BINDINGS.find((b) => b.where === 'Torrent list')!
  shell.getShortcuts.mockResolvedValueOnce([{ where: binding.where, action: binding.action, keys: ['F9'] }] as never)
  shell.connect.mockResolvedValueOnce(bearer('first'))
  renderRoot()
  await waitFor(() =>
    expect(effectiveBindings().find((b) => b.where === binding.where && b.action === binding.action)?.keys).toEqual([
      'F9',
    ]),
  )
  setOverrides([])
})

it('a_failed_request_during_a_serving_apply_ends_on_settings_at_serving', async () => {
  shell.connect.mockResolvedValueOnce(bearer('first')).mockResolvedValueOnce(bearer('restarted'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  fireEvent.click(screen.getByRole('button', { name: 'start apply' }))
  // The old engine is down: a request fails, and the new one refuses the old secret.
  vi.stubGlobal('fetch', vi.fn(() => Promise.reject(new TypeError('Failed to fetch'))))
  fireEvent.click(screen.getByRole('button', { name: 'read torrents' }))
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(shell.connect, 'no reconnect while the apply runs').toHaveBeenCalledTimes(1)
  await act(async () => {
    await shell.finishApply!(null)
  })
  await screen.findByText(`workspace ${BASE} restarted`)
  await screen.findByText('settings at settings-desktop-serving')
  expect(screen.queryByText(UNREACHABLE_COPY)).toBeNull()
})

it('a_serving_apply_that_ends_unreachable_reconnects_to_settings_at_serving', async () => {
  shell.connect
    .mockResolvedValueOnce(bearer('first'))
    .mockResolvedValueOnce(bearer('first'))
    .mockRejectedValueOnce('This computer is not running.')
    .mockResolvedValueOnce(bearer('restarted'))
  renderRoot()
  await screen.findByText(`workspace ${BASE} first`)
  fireEvent.click(screen.getByRole('button', { name: 'start apply' }))
  const finish = shell.finishApply!
  // Another answer clears the apply (same secret: nothing to do), then the window lands on Cannot
  // reach the server, and only then does the first apply answer.
  fireEvent.click(screen.getByRole('button', { name: 'serving applied' }))
  await waitFor(() => expect(shell.connect).toHaveBeenCalledTimes(2))
  rejectToken(shell.seen[shell.seen.length - 1]!)
  await screen.findByText(UNREACHABLE_COPY)
  await act(async () => {
    await finish('The address is in use.')
  })
  await screen.findByText(`workspace ${BASE} restarted`)
  await screen.findByText('settings at settings-desktop-serving')
})

it('the_startup_problem_wins_over_everything', async () => {
  shell.startupProblem.mockResolvedValue({ state: 'problem', message: 'featherbittorrent is already running.' })
  shell.takeOpened.mockResolvedValueOnce([{ kind: 'magnet', uri: 'magnet:?xt=urn:btih:aaaa' }])
  shell.connect.mockRejectedValue('featherbittorrent is already running.')
  renderRoot()
  await screen.findByRole('alertdialog', { name: 'featherbittorrent could not start' })
  expect(screen.getByText('featherbittorrent is already running.')).toBeTruthy()
  expect(screen.queryByLabelText('Magnet link'), 'no opened-link dialog over it').toBeNull()
  expect(screen.queryByText(UNREACHABLE_COPY), 'no screen under it').toBeNull()
  expect(screen.queryByText('close question ready'), 'no close question over it').toBeNull()
})
