// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LAST_SECTION_KEY } from './components/settings/sections'
import App from './App'
import { UNREACHABLE_COPY } from './live'
import { SESSION_ENDED, session } from './session'
import { ConnectionContext, createConnection } from './connection'
import { installDialogShim } from './dialog-shim'

// The `api` and `session` singletons read `globalThis.fetch` the moment their modules load,
// so the controllable fetch must exist before this file's imports run.
const world = vi.hoisted((): { behavior: null | ((url: string) => Promise<unknown>) } => {
  const state: { behavior: null | ((url: string) => Promise<unknown>) } = { behavior: null }
  globalThis.fetch = ((input: unknown) => {
    const behavior = state.behavior
    if (behavior === null) throw new Error(`nothing was set up to answer ${String(input)}`)
    return behavior(String(input))
  }) as typeof globalThis.fetch
  return state
})

const answer = (body: unknown): Promise<unknown> =>
  Promise.resolve({ ok: true, status: 200, statusText: 'OK', json: async () => body })

const SESSION_BODY = {
  version: '0.13.5',
  download_limit: 0,
  upload_limit: 0,
  download_rate: 1_000_000,
  upload_rate: 250_000,
  listen_addrs: ['0.0.0.0:51497'],
  torrents: 0,
  dht_nodes: 120,
}

const SETTINGS_BODY = {
  speed: { download_kib: null, upload_kib: null, active_downloads: null, active_seeds: null, active_total: null },
  downloads: {},
}

/** Every read fails the way a dead server makes fetch fail: the promise itself. */
const dead = (): Promise<unknown> => Promise.reject(new TypeError('fetch failed'))

/** The same server, answering: signed in, an empty table, a session row. */
function alive(url: string): Promise<unknown> {
  if (url.includes('/auth/')) return answer({ auth: true, subject: 'alice', csrf: 'csrf-value' })
  if (url.endsWith('/api/v1/settings')) return answer(SETTINGS_BODY)
  if (url.includes('/torrents')) return answer([])
  if (url.includes('/api/v1/session')) return answer(SESSION_BODY)
  return answer({})
}

beforeEach(() => {
  // These tests are about the Speed section; Downloads opens first on a fresh page.
  window.localStorage.setItem(LAST_SECTION_KEY, 'speed')
})
afterEach(() => {
  cleanup()
  window.localStorage.clear()
})
installDialogShim()

describe('the page as a whole', () => {
  it('the_live_regions_outlive_the_screens_in_the_page', async () => {
    world.behavior = dead
    const { container } = render(<App />)

    const polite = container.querySelector('[aria-live="polite"]')
    const assertive = container.querySelector('[aria-live="assertive"]')
    expect(
      polite !== null && assertive !== null,
      'the page mounted without its two live regions',
    ).toBe(true)

    // loading to unreachable: the words must land in the region that was already mounted,
    // not in a fresh node built together with the new screen.
    await waitFor(() => screen.getByRole('button', { name: 'Try again' }))
    expect(
      assertive !== null && assertive.textContent === UNREACHABLE_COPY,
      'the unreachable copy arrived in a fresh assertive region instead of the one already mounted',
    ).toBe(true)
    expect(
      polite !== null && polite.textContent === '',
      'the unreachable copy also went to the polite region',
    ).toBe(true)

    // unreachable to signedIn: the whole screen is replaced, the regions are not.
    world.behavior = alive
    await act(async () => {
      await session.refresh()
    })
    expect(
      polite !== null && assertive !== null && document.contains(polite) && document.contains(assertive),
      'a screen change replaced one of the live regions',
    ).toBe(true)
    expect(screen.getByRole('button', { name: 'Sign out' }) !== null, 'the signed-in screen never arrived').toBe(true)
    expect(
      polite !== null && polite.textContent === '',
      'a live session was announced as ended',
    ).toBe(true)
    expect(
      assertive !== null && assertive.textContent === '',
      'a signed-in server kept shouting in the assertive region',
    ).toBe(true)

    // signedIn to signedOut: the notice rides in the same polite node.
    act(() => {
      session.endSession(session.generation())
    })
    expect(
      polite !== null && assertive !== null && document.contains(polite) && document.contains(assertive),
      'a screen change replaced one of the live regions',
    ).toBe(true)
    expect(
      polite !== null && polite.textContent === SESSION_ENDED,
      'the ended session never reached the polite region that was already mounted',
    ).toBe(true)
    expect(
      assertive !== null && assertive.textContent === '',
      'the ended session interrupted the person in the assertive region',
    ).toBe(true)

    world.behavior = null
  })

  it('try_again_says_it_is_trying', async () => {
    world.behavior = dead
    const { unmount } = render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Try again' }))

    // The next attempt hangs, so the button must show it is working and refuse to start another.
    let rejectAttempt: (err: Error) => void = () => {}
    world.behavior = () =>
      new Promise((_resolve, reject) => {
        rejectAttempt = reject
      })

    const button = screen.getByRole<HTMLButtonElement>('button', { name: 'Try again' })
    fireEvent.click(button)
    expect(button.disabled, 'Try again kept working while the retry was in flight').toBe(true)
    expect(button.textContent, 'the button did not say what it was doing').toBe('Trying...')

    await act(async () => {
      rejectAttempt(new TypeError('fetch failed'))
      await Promise.resolve()
    })
    expect(button.disabled, 'the failed retry left the button dead').toBe(false)
    expect(button.textContent, 'the failed retry did not offer itself again').toBe('Try again')

    unmount()
    world.behavior = null
  })
})

describe('the screens in the desktop frame', () => {
  /** Whether every element from `from` up to (not including) `top` is `display: contents`. */
  function boxless(from: Element | null, top: Element): string[] {
    const boxes: string[] = []
    for (let at = from; at !== null && at !== top; at = at.parentElement) {
      if (!at.classList.contains('contents')) boxes.push(at.outerHTML.slice(0, 80))
    }
    return boxes
  }

  it('the_torrents_root_is_laid_out_by_the_frame_not_by_a_wrapper', async () => {
    // The frame sizes `.h-dvh` as `height: 100%` of its page column; a box of auto height between
    // the two leaves the percentage nothing to resolve against and the screen shrinks to its content.
    world.behavior = alive
    const { container } = render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Add torrent' }))
    const root = screen.getByRole('button', { name: 'Add torrent' }).closest('.h-dvh')
    expect(root, 'the Torrents root lost h-dvh, which the browser build sizes itself by').not.toBeNull()
    expect(root!.className.split(' '), 'the Torrents root classes changed').toEqual(
      expect.arrayContaining(['flex', 'h-dvh', 'min-h-0', 'flex-col', 'overflow-hidden']),
    )
    expect(boxless(root!.parentElement, container), 'a box sits between the frame and the Torrents root').toEqual([])

    // The wrapper that keeps the table mounted under Settings still hides it there.
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    expect(root!.parentElement!.hasAttribute('hidden'), 'the Torrents wrapper is not hidden under Settings').toBe(true)
    const settings = screen.getByRole('heading', { name: 'Speed' }).closest('.min-h-dvh')
    expect(settings, 'the Settings root lost min-h-dvh').not.toBeNull()
    expect(boxless(settings!.parentElement, container), 'a box sits between the frame and the Settings root').toEqual([])
    world.behavior = null
  })
})

describe('the Settings screen in the page', () => {
  it('settings_replaces_the_torrents_and_back_returns', async () => {
    world.behavior = alive
    render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Settings' }))
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    expect(screen.getByRole('heading', { name: 'Speed' }) !== null, 'Settings did not open').toBe(true)
    expect(screen.queryByRole('button', { name: 'Add torrent' }), 'the table stayed under Settings').toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'Back to torrents' }))
    await waitFor(() => screen.getByRole('button', { name: 'Add torrent' }))
    expect(screen.queryByRole('heading', { name: 'Speed' }), 'Settings stayed after Back').toBeNull()
    world.behavior = null
  })

  it('the filter survives a trip to Settings', async () => {
    world.behavior = alive
    render(<App />)
    await waitFor(() => screen.getByRole('button', { name: /^Paused/ }))
    fireEvent.click(screen.getByRole('button', { name: /^Paused/ }))
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    fireEvent.click(screen.getByRole('button', { name: 'Back to torrents' }))
    await waitFor(() => screen.getByRole('button', { name: /^Paused/ }))
    expect(
      screen.getByRole('button', { name: /^Paused/ }).getAttribute('aria-pressed'),
      'the filter was reset by the trip',
    ).toBe('true')
    world.behavior = null
  })

  it('focus moves into Settings and back', async () => {
    world.behavior = alive
    render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Settings' }))
    const opener = screen.getByRole('button', { name: 'Settings' })
    opener.focus()
    fireEvent.click(opener)
    expect(document.activeElement, 'focus did not move to the Settings title').toBe(
      screen.getByRole('heading', { name: 'Settings' }),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Back to torrents' }))
    await waitFor(() => {
      expect(document.activeElement, 'focus did not return to the Settings button').toBe(
        screen.getByRole('button', { name: 'Settings' }),
      )
    })
    world.behavior = null
  })

  it('browser back closes Settings', async () => {
    world.behavior = alive
    const push = vi.spyOn(window.history, 'pushState')
    render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Settings' }))
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    expect(push, 'opening Settings must push one history entry').toHaveBeenCalledTimes(1)
    act(() => {
      window.dispatchEvent(new PopStateEvent('popstate'))
    })
    expect(
      screen.queryByRole('heading', { name: 'Speed' }),
      'the browser back did not close Settings',
    ).toBeNull()
    await waitFor(() => screen.getByRole('button', { name: 'Add torrent' }))
    push.mockRestore()
    world.behavior = null
  })
})

describe('leaving Settings with unsaved changes', () => {
  /** Settings open on Speed with the download limit edited and not saved. */
  async function dirtySettings(): Promise<void> {
    world.behavior = alive
    render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Settings' }))
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    const field = (await screen.findByLabelText('Download limit KiB/s')) as HTMLInputElement
    await waitFor(() => expect(field.value).toBe('0'))
    fireEvent.change(field, { target: { value: '64' } })
  }

  it('header_show_torrents_with_unsaved_changes_asks', async () => {
    await dirtySettings()
    // The header's Add torrent asks for the Torrents screen back; the table is hidden under Settings here.
    fireEvent.click(screen.getByRole('button', { name: 'Add torrent', hidden: true }))
    expect(screen.getByText('You have unsaved changes in Speed.')).toBeTruthy()
    expect(screen.getByRole('heading', { name: 'Speed', level: 2 }), 'Settings closed without asking').toBeTruthy()
    // Discarding answers the question and then does what was asked: Settings closes.
    fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }))
    expect(screen.queryByRole('heading', { name: 'Speed', level: 2 })).toBeNull()
    world.behavior = null
  })

  it('browser_back_with_unsaved_changes_asks', async () => {
    const push = vi.spyOn(window.history, 'pushState')
    await dirtySettings()
    expect(push).toHaveBeenCalledTimes(1)
    act(() => {
      window.dispatchEvent(new PopStateEvent('popstate'))
    })
    expect(screen.getByText('You have unsaved changes in Speed.')).toBeTruthy()
    expect(screen.getByRole('heading', { name: 'Speed', level: 2 }), 'Settings closed without asking').toBeTruthy()
    // The entry the browser just left is put back, so a later Back still lands on the same place.
    expect(push, 'the history entry was not put back').toHaveBeenCalledTimes(2)
    fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }))
    expect(screen.queryByRole('heading', { name: 'Speed', level: 2 })).toBeNull()
    push.mockRestore()
    world.behavior = null
  })
})

describe('the Add dialog over the torrents screen', () => {
  it('add torrent from settings opens the dialog on the torrents screen', async () => {
    world.behavior = alive
    render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Settings' }))
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    expect(
      screen.getByRole('heading', { name: 'Speed' }) !== null,
      'Settings did not open',
    ).toBe(true)

    // The desktop app portals the header's actions into the title bar, where Settings does not
    // cover them, so this Add torrent click is one a person really can make with Settings up.
    // Here the same button sits in the wrapper hidden under Settings, so the role query has to
    // include hidden elements to reach it.
    fireEvent.click(screen.getByRole('button', { name: 'Add torrent', hidden: true }))

    expect(
      screen.queryByRole('heading', { name: 'Speed' }),
      'pressing Add left Settings on screen',
    ).toBeNull()
    const dialog = screen.getByRole('dialog', { name: 'Add torrent' })
    expect(dialog.hasAttribute('open'), 'the Add dialog mounted without opening').toBe(true)
    expect(dialog.closest('[hidden]'), 'the Add dialog opened inside a hidden subtree').toBeNull()
    const active = document.activeElement
    expect(
      active !== null && active !== document.body && dialog.contains(active),
      'the focus did not land inside the Add dialog',
    ).toBe(true)
    world.behavior = null
  })
})

describe('the page on a desktop connection', () => {
  it('app_reads_the_connection_from_context', async () => {
    const seen: Array<{ url: string; auth: string | null }> = []
    const fetchFn = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      seen.push({ url, auth: new Headers(init?.headers).get('authorization') })
      if (url.endsWith('/api/v1/torrents')) return answer([])
      if (url.endsWith('/api/v1/session')) return answer(SESSION_BODY)
      return answer({})
    }) as unknown as typeof fetch
    const connection = createConnection({ kind: 'bearer', base: 'http://127.0.0.1:53211', token: 'launch-secret' }, fetchFn)
    world.behavior = (url) => Promise.reject(new Error(`the page used the browser singletons for ${url}`))

    render(
      <ConnectionContext.Provider value={connection}>
        <App />
      </ConnectionContext.Provider>,
    )

    await waitFor(() => screen.getByRole('button', { name: 'Add torrent' }))
    await waitFor(() => {
      expect(
        seen.some((call) => call.url === 'http://127.0.0.1:53211/api/v1/torrents'),
        'the page never asked the fake base for the torrents',
      ).toBe(true)
    })
    expect(seen.every((call) => call.auth === 'Bearer launch-secret'), 'a desktop call went out without the token').toBe(true)
    expect(seen.some((call) => call.url.includes('/auth/web-session')), 'the desktop asked about a browser cookie').toBe(false)
    expect(screen.queryByRole('button', { name: 'Sign out' }), 'the launch-secret desktop offered a sign-out').toBeNull()
    world.behavior = null
  })
})
