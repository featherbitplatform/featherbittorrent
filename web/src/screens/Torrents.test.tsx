// @vitest-environment jsdom
/**
 * The Torrents screen's side of the actions: the toolbar over the table, the dialogs the toolbar and
 * the header open, and the refresh loop the actions wake.
 *
 * The toolbar belongs to the selection, so the tests here are about who is chosen and what the row
 * says it is doing: nothing chosen means nothing to act on, a paused row cannot be paused again and a
 * running one cannot be resumed, and the id that goes to the server is the id that was chosen. Then
 * the two things a person would notice when they are wrong: a table that only updates a second after
 * a pause (the loop is woken, and the test reads that wake at zero milliseconds), and a torrent that
 * stays selected after it was removed (the pane closes and the selection goes with it).
 *
 * The loop is driven on fake timers, so "immediately" is a count of advanced milliseconds rather than
 * a race against the wall clock. The dialogs are the real ones, mounted the way the screen mounts
 * them, because the point of this file is the wiring.
 *
 * @module screens/Torrents.test
 */
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import Torrents from './Torrents'
import { ApiError } from '../api/client'
import type { Api, PiecesInfo, SessionInfo, TorrentInfo } from '../api/client'
import { installDialogShim } from '../dialog-shim'

afterEach(cleanup)
afterEach(() => {
  vi.useRealTimers()
})
installDialogShim()

const SCREEN_SESSION: SessionInfo = {
  version: '0.13.5',
  download_limit: 0,
  upload_limit: 0,
  download_rate: 1_000_000,
  upload_rate: 250_000,
  listen_addrs: ['0.0.0.0:51497'],
  torrents: 2,
  dht_nodes: 120,
}

const PIECES: PiecesInfo = { num_pieces: 8, have: 'ff' }

function torrentInfo(id: string, over: Partial<TorrentInfo> = {}): TorrentInfo {
  return {
    id,
    name: `torrent ${id}`,
    progress: 0.4,
    download_rate: 1_000,
    upload_rate: 0,
    num_peers: 2,
    num_seeds: 1,
    state: 'downloading',
    active_time: 0,
    auto_managed: true,
    has_metadata: true,
    is_seed: false,
    layers_missing: 0,
    need_save_resume: false,
    num_web_seeds: 0,
    queue_position: 0,
    save_path: '/downloads',
    seeding_time: 0,
    total_downloaded: 0,
    total_uploaded: 0,
    total_wanted: 100,
    total_wasted: 0,
    wanted_done: 40,
    ...over,
  }
}

interface Calls {
  torrents: number
  pause: string[]
  resume: string[]
  remove: string[]
}

/** A client that counts what the screen asks it to do. Every read answers at once; the actions answer however the test says. */
function fakeClient(options: {
  rows?: TorrentInfo[]
  roots?: string[]
  pause?: (id: string) => Promise<void>
  resume?: (id: string) => Promise<void>
  remove?: (id: string, deleteFiles: boolean) => Promise<void>
  moveStorage?: (id: string, path: string) => Promise<void>
}): { client: Api; calls: Calls } {
  const rows = options.rows ?? []
  const calls: Calls = { torrents: 0, pause: [], resume: [], remove: [] }
  const client = {
    torrents: async (): Promise<TorrentInfo[]> => {
      calls.torrents += 1
      return rows
    },
    torrent: async (id: string): Promise<TorrentInfo> =>
      rows.find((row) => row.id === id) as TorrentInfo,
    files: async () => [],
    peers: async () => [],
    trackers: async () => [],
    pieces: async (): Promise<PiecesInfo> => PIECES,
    session: async (): Promise<SessionInfo> => SCREEN_SESSION,
    pause: (id: string): Promise<void> => {
      calls.pause.push(id)
      return options.pause?.(id) ?? Promise.resolve()
    },
    resume: (id: string): Promise<void> => {
      calls.resume.push(id)
      return options.resume?.(id) ?? Promise.resolve()
    },
    remove: (id: string, deleteFiles: boolean): Promise<void> => {
      calls.remove.push(`${id}:${String(deleteFiles)}`)
      return options.remove?.(id, deleteFiles) ?? Promise.resolve()
    },
    moveStorage: (id: string, path: string): Promise<void> =>
      options.moveStorage?.(id, path) ?? Promise.resolve(),
    addMagnet: async () => torrentInfo('added'),
    addFile: async () => torrentInfo('added'),
    roots: async (): Promise<string[]> => options.roots ?? ['/downloads'],
    list: async (path: string) => ({ path, dirs: [] }),
  }
  return { client: client as unknown as Api, calls }
}

function openScreen(client: Api): { container: HTMLElement; polite: HTMLElement } {
  const { container } = render(
    <>
      <div role="status" aria-live="polite" />
      <Torrents signedIn signOutError={null} onSignOut={() => {}} onSettings={() => {}} client={client} />
    </>,
  )
  return {
    container,
    polite: container.querySelector('[aria-live="polite"]') as HTMLElement,
  }
}

/** Let everything the screen just fired land before looking at the DOM. */
async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0))
    await new Promise((resolve) => setTimeout(resolve, 0))
  })
}

/** The same, on timers the test is holding: only the real microtasks are let through. */
async function drain(): Promise<void> {
  await act(async () => {
    for (let round = 0; round < 20; round += 1) await Promise.resolve()
  })
}

const row = (container: HTMLElement, id: string): Element =>
  container.querySelector(`tbody tr[data-row="${id}"]`) as Element

const pane = (container: HTMLElement): Element | null =>
  container.querySelector('section[aria-label="Details"]')

const button = (name: string): HTMLButtonElement =>
  screen.getByRole('button', { name }) as HTMLButtonElement

const enabled = (name: string): boolean => button(name).disabled === false

describe('the toolbar over the table', () => {
  it('the_toolbar_acts_on_the_selected_row', async () => {
    const { client, calls } = fakeClient({
      rows: [torrentInfo('a'), torrentInfo('b', { state: 'paused', download_rate: 0 })],
    })
    const { container } = openScreen(client)
    await flush()

    // Nothing chosen: nothing to act on, and the toolbar says so by being shut.
    expect(
      !enabled('Pause') && !enabled('Resume') && !enabled('Remove'),
      'the toolbar offered its actions while nothing was selected',
    ).toBe(true)

    // A row that is running: pause is the thing to do, resume is not.
    fireEvent.click(row(container, 'a'))
    await flush()
    expect(
      enabled('Pause') && !enabled('Resume') && enabled('Remove'),
      'the toolbar offered Resume for a row that is already running',
    ).toBe(true)

    // A paused row: the other way round - Pause has nothing left to do for it.
    fireEvent.click(row(container, 'b'))
    await flush()
    expect(
      !enabled('Pause') && enabled('Resume') && enabled('Remove'),
      'the toolbar offered Pause for a row that is already paused',
    ).toBe(true)

    // And the action goes to the torrent that is chosen, not to some other row.
    fireEvent.click(button('Resume'))
    await flush()
    expect(
      calls.resume.join(',') === 'b' && calls.pause.length === 0,
      'the toolbar sent the action for a torrent nobody selected',
    ).toBe(true)
  })

  it('an_action_wakes_the_refresh_loop_before_the_next_second', async () => {
    vi.useFakeTimers()
    const { client, calls } = fakeClient({ rows: [torrentInfo('a')] })
    const { container } = openScreen(client)
    await drain()
    expect(calls.torrents, 'the screen did not read the table once when it mounted').toBe(1)

    fireEvent.click(row(container, 'a'))
    await drain()
    fireEvent.click(button('Pause'))
    await drain()
    expect(calls.pause.join(','), 'the Pause button did not pause the selected torrent').toBe('a')

    // The wake schedules a read at zero: nothing has been advanced but a read is due, so the row
    // changes when the person acts and not a second later.
    expect(calls.torrents, 'one action asked the server for the table more than once').toBe(1)
    act(() => {
      vi.advanceTimersByTime(0)
    })
    await drain()
    expect(
      calls.torrents === 2,
      'the action did not wake the refresh loop for a read at once',
    ).toBe(true)

    // The once a second cadence carries on from there: the wake borrowed a read, it did not
    // replace the schedule.
    act(() => {
      vi.advanceTimersByTime(1_000)
    })
    await drain()
    expect(calls.torrents, 'waking the loop broke the once a second cadence').toBe(3)
  })

  it('a_refused_action_says_why', async () => {
    const { client } = fakeClient({
      rows: [torrentInfo('a')],
      pause: () => Promise.reject(new ApiError(400, 'bad_request', 'that torrent cannot be paused')),
    })
    const { container, polite } = openScreen(client)
    await flush()
    fireEvent.click(row(container, 'a'))
    await flush()
    fireEvent.click(button('Pause'))
    await flush()
    expect(
      (container.textContent ?? '').includes('that torrent cannot be paused') &&
        polite.textContent === 'that torrent cannot be paused' &&
        enabled('Pause'),
      'a refused pause said nothing about why or left the toolbar unable to try again',
    ).toBe(true)
  })
})

describe('the dialogs the screen opens', () => {
  it('the_add_button_opens_the_add_dialog', async () => {
    const { client, calls } = fakeClient({ rows: [], roots: ['/downloads'] })
    const { container } = openScreen(client)
    await flush()

    const add = button('Add torrent')
    expect(add.disabled === false, 'the header Add torrent button still answers nothing').toBe(true)
    fireEvent.click(add)
    await drain()

    const dialog = screen.getByRole('dialog')
    expect(
      (dialog.textContent ?? '').includes('Add torrent') &&
        dialog.querySelector('[data-folder="/downloads"]') !== null,
      'the add dialog did not open with the folder the server named',
    ).toBe(true)

    // Cancel closes it, and the dialog's own server calls are the only ones the screen made.
    fireEvent.click(button('Cancel'))
    await drain()
    expect(
      screen.queryByRole('dialog') === null && calls.remove.length === 0,
      'Cancel left the add dialog open',
    ).toBe(true)
    expect(container.querySelector('dialog'), 'the add dialog element stayed in the page').toBeNull()
  })

  it('a_removed_torrent_clears_the_selection', async () => {
    const { client, calls } = fakeClient({ rows: [torrentInfo('a'), torrentInfo('b')] })
    const { container, polite } = openScreen(client)
    await flush()

    fireEvent.click(row(container, 'a'))
    await flush()
    expect(pane(container) !== null, 'selecting a row did not open the detail pane').toBe(true)

    fireEvent.click(button('Remove'))
    await drain()
    expect(
      screen.getByRole('button', { name: 'Remove and delete files' }) !== null,
      'the Remove button removed the torrent without asking how',
    ).toBe(true)

    fireEvent.click(button('Remove and delete files'))
    await flush()
    expect(
      calls.remove.join(',') === 'a:true' &&
        screen.queryByRole('dialog') === null &&
        pane(container) === null &&
        polite.textContent === 'Torrent removed.',
      'the removal did not close the dialog and the pane or say so through the polite region',
    ).toBe(true)

    // The selection went with the torrent: the toolbar has nothing left to act on.
    expect(
      !enabled('Pause') && !enabled('Resume') && !enabled('Remove'),
      'the toolbar still acted on a torrent that was removed',
    ).toBe(true)
  })

  it('moving survives selecting another torrent', async () => {
    let finish: () => void = () => {}
    const { client } = fakeClient({
      rows: [torrentInfo('a'), torrentInfo('b')],
      moveStorage: () =>
        new Promise<void>((resolve) => {
          finish = resolve
        }),
    })
    const { container } = openScreen(client)
    await flush()
    fireEvent.click(row(container, 'a'))
    await flush()
    fireEvent.click(button('More'))
    fireEvent.click(screen.getByRole('menuitem', { name: /move files/i }))
    await flush()
    fireEvent.click(button('Move'))
    await flush()
    expect(screen.queryByText('Moving...') !== null, 'the move showed no indicator').toBe(true)
    fireEvent.click(row(container, 'b'))
    await flush()
    expect(screen.queryByText('Moving...') === null, 'another torrent showed the indicator').toBe(true)
    fireEvent.click(row(container, 'a'))
    await flush()
    expect(screen.queryByText('Moving...') !== null, 'the indicator was lost on reselecting').toBe(true)
    await act(async () => {
      finish()
      await Promise.resolve()
    })
    await flush()
    expect(screen.queryByText('Moving...') === null, 'the indicator outlived the request').toBe(true)
  })
})

/** Make `matchMedia('(max-width: 640px)')` answer `matches`, and hand back a way to flip it. */
function phoneWidth(matches: boolean): (next: boolean) => void {
  const listeners = new Set<() => void>()
  const state = { matches }
  vi.stubGlobal('matchMedia', (query: string) => ({
    get matches() {
      return state.matches
    },
    media: query,
    addEventListener: (_: string, fn: () => void) => listeners.add(fn),
    removeEventListener: (_: string, fn: () => void) => listeners.delete(fn),
  }))
  return (next) => {
    state.matches = next
    listeners.forEach((fn) => {
      fn()
    })
  }
}

describe('the phone layout', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  const cards = (container: HTMLElement): NodeListOf<Element> =>
    container.querySelectorAll('[data-card]')

  it('the phone shows cards and the desktop the table', async () => {
    const { client } = fakeClient({
      rows: [torrentInfo('a'), torrentInfo('b')],
    })
    phoneWidth(true)
    const phone = openScreen(client)
    await flush()
    expect(cards(phone.container).length, 'the phone did not show cards').toBe(2)
    expect(phone.container.querySelector('table'), 'the phone still drew the table').toBeNull()
    cleanup()

    phoneWidth(false)
    const desktop = openScreen(client)
    await flush()
    expect(cards(desktop.container).length, 'the desktop drew cards').toBe(0)
    expect(desktop.container.querySelector('table'), 'the desktop lost its table').not.toBeNull()
  })

  it('tapping a card opens the full view and pushes history', async () => {
    const { client } = fakeClient({ rows: [torrentInfo('a')] })
    phoneWidth(true)
    const push = vi.spyOn(window.history, 'pushState')
    const { container } = openScreen(client)
    await flush()
    fireEvent.click(cards(container)[0])
    await flush()
    expect(push, 'opening the full view must push history exactly once').toHaveBeenCalledTimes(1)
    const view = screen.getByRole('dialog', { name: 'Torrent' })
    expect(view.textContent).toContain('Pause')
    expect(view.textContent).toContain('Remove')
    for (const tab of ['Files', 'Peers', 'Trackers', 'Pieces']) {
      expect(
        screen.getByRole('tab', { name: tab }),
        `the full view lost its ${tab} tab`,
      ).toBeTruthy()
    }
  })

  it('back returns to the list', async () => {
    const { client } = fakeClient({ rows: [torrentInfo('a')] })
    phoneWidth(true)
    let signedOut = 0
    const { container } = render(
      <Torrents
        signedIn
        signOutError={null}
        onSignOut={() => {
          signedOut += 1
        }}
        client={client}
      />,
    )
    await flush()
    fireEvent.click(cards(container)[0])
    await flush()
    expect(screen.queryByRole('dialog', { name: 'Torrent' })).not.toBeNull()

    // The browser's back: a popstate closes the view.
    act(() => {
      window.dispatchEvent(new PopStateEvent('popstate'))
    })
    expect(
      screen.queryByRole('dialog', { name: 'Torrent' }),
      'popstate did not close the view',
    ).toBeNull()
    expect(cards(container).length).toBe(1)
    expect(signedOut, 'back signed the person out').toBe(0)

    // The on-screen Back button goes through history.back, so the history stays balanced.
    fireEvent.click(cards(container)[0])
    await flush()
    const back = vi.spyOn(window.history, 'back').mockImplementation(() => {})
    fireEvent.click(screen.getByRole('button', { name: 'Back' }))
    expect(back, 'the Back button must call history.back once').toHaveBeenCalledTimes(1)
  })

  it('filters become chips', async () => {
    const { client } = fakeClient({
      rows: [torrentInfo('a'), torrentInfo('b', { state: 'paused', download_rate: 0 })],
    })
    phoneWidth(true)
    openScreen(client)
    await flush()
    const paused = screen.getByRole('button', { name: /Paused/ })
    expect(paused.textContent, 'a chip lost its count').toContain('1')
    expect(screen.getByRole('button', { name: /^All/ }).textContent).toContain('2')
    fireEvent.click(paused)
    expect(
      document.querySelectorAll('[data-card]').length,
      'choosing a chip did not narrow the cards',
    ).toBe(1)
  })

  it('a double back stays in the app', async () => {
    const { client } = fakeClient({ rows: [torrentInfo('a')] })
    phoneWidth(true)
    const { container } = openScreen(client)
    await flush()
    fireEvent.click(cards(container)[0])
    await flush()
    const back = vi.spyOn(window.history, 'back').mockImplementation(() => {})
    const target = screen.getByRole('button', { name: 'Back' })
    act(() => {
      target.click()
      target.click()
    })
    expect(back, 'a double tap on Back walked out of the app').toHaveBeenCalledTimes(1)
  })

  it('resizing to desktop keeps the history balanced', async () => {
    const { client } = fakeClient({ rows: [torrentInfo('a')] })
    const flip = phoneWidth(true)
    const { container } = openScreen(client)
    await flush()
    fireEvent.click(cards(container)[0])
    await flush()
    const back = vi.spyOn(window.history, 'back').mockImplementation(() => {})
    act(() => {
      flip(false)
    })
    expect(back, 'the stranded history entry was not consumed').toHaveBeenCalledTimes(1)
    expect(pane(container), 'the desktop lost the selection').not.toBeNull()
    const push = vi.spyOn(window.history, 'pushState')
    act(() => {
      flip(true)
    })
    expect(back).toHaveBeenCalledTimes(1)
    expect(push).not.toHaveBeenCalled()
  })

  it('a torrent that disappears closes the full view and the history', async () => {
    const rows = [torrentInfo('a')]
    const { client } = fakeClient({ rows })
    phoneWidth(true)
    const { container } = openScreen(client)
    await flush()
    fireEvent.click(cards(container)[0])
    await flush()
    const back = vi.spyOn(window.history, 'back').mockImplementation(() => {})
    rows.splice(0, 1)
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 1200))
    })
    expect(screen.queryByRole('dialog', { name: 'Torrent' }), 'the full view stayed open').toBeNull()
    expect(back, 'the history entry was not given back').toHaveBeenCalledTimes(1)
  })

  it('the live change event flips cards and table', async () => {
    const { client } = fakeClient({ rows: [torrentInfo('a')] })
    const flip = phoneWidth(true)
    const { container } = openScreen(client)
    await flush()
    expect(cards(container).length).toBe(1)
    act(() => {
      flip(false)
    })
    expect(container.querySelector('table'), 'the table did not come back on resize').not.toBeNull()
    expect(cards(container).length).toBe(0)
    act(() => {
      flip(true)
    })
    expect(cards(container).length, 'the cards did not come back on resize').toBe(1)
  })

  it('pause and remove work in the full view', async () => {
    const { client, calls } = fakeClient({ rows: [torrentInfo('a')] })
    phoneWidth(true)
    const { container } = openScreen(client)
    await flush()
    fireEvent.click(cards(container)[0])
    await flush()
    vi.spyOn(window.history, 'back').mockImplementation(() => {})
    fireEvent.click(button('Pause'))
    await flush()
    expect(calls.pause, 'Pause did not reach the torrent').toEqual(['a'])
    fireEvent.click(button('Remove'))
    fireEvent.click(button('Remove and delete files'))
    await flush()
    expect(calls.remove, 'Remove did not go through the confirm flow').toEqual(['a:true'])
  })

  it('a popstate leaves history alone', async () => {
    const { client } = fakeClient({ rows: [torrentInfo('a')] })
    phoneWidth(true)
    const { container } = openScreen(client)
    await flush()
    fireEvent.click(cards(container)[0])
    await flush()
    const back = vi.spyOn(window.history, 'back').mockImplementation(() => {})
    act(() => {
      window.dispatchEvent(new PopStateEvent('popstate'))
    })
    expect(back, 'a popstate went back a second time').not.toHaveBeenCalled()
    expect(screen.queryByRole('dialog', { name: 'Torrent' })).toBeNull()
  })

  it('opening a card right after back stays open', async () => {
    const { client } = fakeClient({ rows: [torrentInfo('a'), torrentInfo('b')] })
    phoneWidth(true)
    const { container } = openScreen(client)
    await flush()
    fireEvent.click(cards(container)[0])
    await flush()
    vi.spyOn(window.history, 'back').mockImplementation(() => {})
    fireEvent.click(screen.getByRole('button', { name: 'Back' }))
    await flush()
    // Another card is opened before the first back's popstate has arrived.
    fireEvent.click(cards(container)[1])
    await flush()
    act(() => {
      window.dispatchEvent(new PopStateEvent('popstate', { state: { fbttFull: 1 } }))
    })
    expect(
      screen.queryByRole('dialog', { name: 'Torrent' }),
      'a stale popstate closed the view that was just opened',
    ).not.toBeNull()
  })

  it('unmounting gives the history entry back', async () => {
    const { client } = fakeClient({ rows: [torrentInfo('a')] })
    phoneWidth(true)
    const { container, unmount } = render(
      <Torrents signedIn signOutError={null} onSignOut={() => {}} client={client} />,
    )
    await flush()
    fireEvent.click(cards(container)[0])
    await flush()
    const back = vi.spyOn(window.history, 'back').mockImplementation(() => {})
    unmount()
    expect(back, 'unmounting left the pushed entry behind').toHaveBeenCalledTimes(1)
  })
})
