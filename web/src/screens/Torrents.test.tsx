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
      <Torrents signedIn signOutError={null} onSignOut={() => {}} client={client} />
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
})
