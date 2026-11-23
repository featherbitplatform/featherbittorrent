// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import DetailPane from './DetailPane'
import Torrents from '../screens/Torrents'
import { ApiError } from '../api/client'
import type { Api, FileInfo, PeerInfo, PiecesInfo, TorrentInfo, TrackerInfo } from '../api/client'

afterEach(cleanup)

function file(index: number, over: Partial<FileInfo> = {}): FileInfo {
  return {
    index,
    path: `file-${index}.bin`,
    len: 1_048_576,
    downloaded: 524_288,
    priority: 'normal',
    pad: false,
    ...over,
  }
}

function peer(over: Partial<PeerInfo> = {}): PeerInfo {
  return {
    addr: '203.0.113.24:51413',
    client: 'Transmission 4.0',
    download_rate: 1_048_576,
    upload_rate: 0,
    am_choking: false,
    am_interested: true,
    encrypted: false,
    incoming: false,
    peer_choking: false,
    peer_interested: true,
    progress: 0.5,
    source: 'tracker',
    total_downloaded: 0,
    total_uploaded: 0,
    transport: 'tcp',
    ...over,
  }
}

function tracker(over: Partial<TrackerInfo> = {}): TrackerInfo {
  return {
    info_hash: '0'.repeat(40),
    tier: 0,
    url: 'udp://tracker.example.org:6969/announce',
    ...over,
  }
}

const PIECES_10: PiecesInfo = { num_pieces: 10, have: 'c0' }

/** Let everything the pane just fired land before looking at the DOM. */
async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0))
    await new Promise((resolve) => setTimeout(resolve, 0))
  })
}

/** A client whose four detail calls record which of them went for which torrent. */
function spyClient(responses: {
  files?: (id: string) => Promise<FileInfo[]>
  peers?: (id: string) => Promise<PeerInfo[]>
  trackers?: (id: string) => Promise<TrackerInfo[]>
  pieces?: (id: string) => Promise<PiecesInfo>
}): { client: Api; calls: string[] } {
  const calls: string[] = []
  const record = <T,>(tab: string, fn: (id: string) => Promise<T>): ((id: string) => Promise<T>) =>
    vi.fn((id: string) => {
      calls.push(`${tab}:${id}`)
      return fn(id)
    })
  const client = {
    files: record('files', responses.files ?? (async () => [file(0)])),
    peers: record('peers', responses.peers ?? (async () => [])),
    trackers: record('trackers', responses.trackers ?? (async () => [])),
    pieces: record('pieces', responses.pieces ?? (async () => PIECES_10)),
  }
  return { client: client as unknown as Api, calls }
}

const pane = (container: HTMLElement): HTMLElement | null =>
  container.querySelector('section[aria-label="Details"]')

const rowText = (container: HTMLElement, index: number): string =>
  (container.querySelector(`tbody tr[data-row="${index}"]`)?.textContent ?? '')

/** One torrent, the way `Torrents` receives it: the pane-height test drives the real screen, not the pane alone. */
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

const SCREEN_SESSION = {
  version: '0.13.5',
  download_limit: 0,
  upload_limit: 0,
  download_rate: 1_000_000,
  upload_rate: 250_000,
  listen_addrs: ['0.0.0.0:51497'],
  torrents: 1,
  dht_nodes: 120,
}

describe('what the pane reads', () => {
  it('loads_the_selected_tab_only', async () => {
    const { client, calls } = spyClient({
      files: async (id) => [file(0, { path: `a-${id}.bin` })],
      peers: async () => [],
    })
    const { rerender } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    expect(calls.join(','), 'the pane fetched tabs it is not showing on the first read').toBe(
      'files:a',
    )

    // Switching tabs fetches the new tab and nothing else.
    fireEvent.click(screen.getByRole('tab', { name: 'Peers' }))
    await flush()
    expect(calls.join(','), 'the pane fetched tabs it is not showing when switching tabs').toBe(
      'files:a,peers:a',
    )

    // The page's tick refreshes the tab being shown, once, and only it.
    rerender(<DetailPane id="a" client={client} tick={1} />)
    await flush()
    expect(calls.join(','), 'the pane fetched tabs it is not showing on the tick').toBe(
      'files:a,peers:a,peers:a',
    )

    // A new torrent reads the same tab for the new id - and still only that tab.
    rerender(<DetailPane id="b" client={client} tick={1} />)
    await flush()
    expect(calls.join(','), 'the pane fetched tabs it is not showing when the torrent changed').toBe(
      'files:a,peers:a,peers:a,peers:b',
    )
  })

  it('a_removed_torrent_closes_the_pane', async () => {
    const { client, calls } = spyClient({
      files: async () => {
        throw new ApiError(404, 'torrent_not_found', 'no torrent with that id')
      },
    })
    const removed: string[] = []
    const tree = (tick: number) => (
      <>
        <div role="status" aria-live="polite" />
        <DetailPane
          id="a"
          client={client}
          tick={tick}
          onRemoved={(goneId) => {
            removed.push(goneId)
          }}
        />
      </>
    )
    const { container, rerender } = render(tree(0))
    await flush()
    const polite = container.querySelector('[aria-live="polite"]') as HTMLElement
    expect(
      pane(container) === null &&
        polite.textContent === 'That torrent was removed.' &&
        calls.length === 1 &&
        removed[0] === 'a',
      'the pane kept asking for a removed torrent',
    ).toBe(true)

    // And it stays closed: later ticks ask nothing more.
    rerender(tree(1))
    await flush()
    rerender(tree(2))
    await flush()
    expect(calls.length, 'the pane kept asking for a removed torrent').toBe(1)

    // The composite above is the promise; these say which part of it broke.
    expect(pane(container), 'the pane stayed open on a torrent that answered 404').toBeNull()
    expect(polite.textContent, 'the removal was not announced through the polite region').toBe(
      'That torrent was removed.',
    )
  })

  it('a_stale_answer_is_not_shown', async () => {
    // Torrent a's answer is deliberately slower than the switch away from it.
    let resolveA: (files: FileInfo[]) => void = () => {}
    const promiseA = new Promise<FileInfo[]>((resolve) => {
      resolveA = resolve
    })
    const { client } = spyClient({
      files: (id) => (id === 'a' ? promiseA : Promise.resolve([file(0, { path: 'b-file.bin' })])),
    })
    const { container, rerender } = render(<DetailPane id="a" client={client} tick={0} />)

    // Switch torrents while a's answer is still in flight; b's answer lands first.
    rerender(<DetailPane id="b" client={client} tick={0} />)
    await flush()
    const section = pane(container) as HTMLElement
    expect(section.textContent?.includes('b-file.bin'), 'the new torrent lost its own answer').toBe(
      true,
    )

    // Now a's late answer arrives. It must not paint at all - not over b, not as b.
    await act(async () => {
      resolveA([file(0, { path: 'a-file.bin' })])
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
    const text = (pane(container) as HTMLElement).textContent ?? ''
    expect(
      text.includes('b-file.bin') && !text.includes('a-file.bin'),
      'the pane showed data for the wrong torrent',
    ).toBe(true)
  })

  it('missing_metadata_is_a_wait_not_an_error', async () => {
    // A magnet whose metadata has not arrived answers `409 no_metadata`. The first read says that;
    // the next one already knows the file list.
    let asked = 0
    const { client } = spyClient({
      files: async () => {
        asked += 1
        if (asked === 1)
          throw new ApiError(409, 'no_metadata', "the torrent's metadata is not known yet")
        return [file(0, { path: 'now-known.bin' })]
      },
    })
    const { container, rerender } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()

    const waiting = screen.getByText("Waiting for the torrent's metadata.")
    expect(
      waiting.className.includes('text-muted') && !waiting.className.includes('text-error'),
      'the metadata wait was drawn in the error colour',
    ).toBe(true)
    const text = (pane(container) as HTMLElement).textContent ?? ''
    expect(
      text.includes("Waiting for the torrent's metadata.") &&
        !text.includes("the torrent's metadata is not known yet"),
      'the pane quoted the server instead of saying what it waits for',
    ).toBe(true)

    // A wait keeps asking: the next tick reads again, and the tab fills itself in with no trace
    // of the wait left behind.
    rerender(<DetailPane id="a" client={client} tick={1} />)
    await flush()
    const after = (pane(container) as HTMLElement).textContent ?? ''
    expect(
      after.includes('now-known.bin') && !after.includes('metadata'),
      'the pane stopped asking once a read answered 409',
    ).toBe(true)
    expect(asked === 2, 'the metadata wait did not keep polling').toBe(true)
  })

  it('a_slow_answer_still_paints', async () => {
    // Pieces for a big torrent on a slow link: the answer takes longer than two ticks.
    let resolvePieces: (pieces: PiecesInfo) => void = () => {}
    const slow = new Promise<PiecesInfo>((resolve) => {
      resolvePieces = resolve
    })
    const { client, calls } = spyClient({ pieces: () => slow })
    const { container, rerender } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    // The mount read (Files) already landed; now the slow tab is asked for.
    fireEvent.click(screen.getByRole('tab', { name: 'Pieces' }))
    await flush()

    // Two ticks pass while the read is still in flight: a tick is the same question, not a new
    // one, so it starts nothing - and it supersedes nothing either.
    rerender(<DetailPane id="a" client={client} tick={1} />)
    await flush()
    rerender(<DetailPane id="a" client={client} tick={2} />)
    await flush()
    expect(
      calls.join(',') === 'files:a,pieces:a',
      'a tick while a read was in flight started a second one',
    ).toBe(true)

    // The slow answer lands. It is the answer for the torrent and tab still on screen, so it
    // paints - the pane did not discard it just because two ticks went by meanwhile.
    await act(async () => {
      resolvePieces(PIECES_10)
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
    const text = (pane(container) as HTMLElement).textContent ?? ''
    expect(
      text.includes('2 of 8 pieces') && !text.includes('Loading'),
      'the slow answer never painted',
    ).toBe(true)

    // And only after it landed does the next tick ask again: the pane refreshes, it stampedes.
    rerender(<DetailPane id="a" client={client} tick={3} />)
    await flush()
    expect(
      calls.join(',') === 'files:a,pieces:a,pieces:a',
      'the pane stopped refreshing after the slow answer painted',
    ).toBe(true)
  })

  it('the_close_button_clears_the_selection', async () => {
    const { client } = spyClient({})
    let dismissed = 0
    render(
      <DetailPane
        id="a"
        client={client}
        tick={0}
        onDismiss={() => {
          dismissed += 1
        }}
      />,
    )
    await flush()
    fireEvent.click(screen.getByRole('button', { name: 'Close details' }))
    expect(dismissed === 1, 'the pane has no close button that clears the selection').toBe(true)
  })
})

describe('file priorities', () => {
  type Prio = FileInfo['priority']
  function prioClient(initial: FileInfo[], fail?: ApiError) {
    let current = initial
    const sent: Array<{ id: string; priorities: string[] }> = []
    const client = {
      files: vi.fn(async () => current),
      peers: vi.fn(async () => []),
      trackers: vi.fn(async () => []),
      pieces: vi.fn(async () => PIECES_10),
      setFilePriorities: vi.fn(async (id: string, priorities: string[]) => {
        if (fail !== undefined) throw fail
        sent.push({ id, priorities })
        current = current.map((f, i) => ({ ...f, priority: priorities[i] as Prio }))
      }),
    } as unknown as Api
    return { client, sent }
  }
  const choose = (name: string, value: string): void => {
    fireEvent.change(screen.getByLabelText(`Priority of ${name}`), { target: { value } })
  }

  it("a file's priority can be changed", async () => {
    const { client, sent } = prioClient([
      file(0, { priority: 'high' }),
      file(1),
      file(2, { priority: 'low' }),
    ])
    const { container, rerender } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-1.bin', 'skip')
    await flush()
    expect(
      JSON.stringify(sent) === JSON.stringify([{ id: 'a', priorities: ['high', 'skip', 'low'] }]),
      'the change did not send the whole list with only that file changed',
    ).toBe(true)
    rerender(<DetailPane id="a" client={client} tick={1} />)
    await flush()
    expect(
      (screen.getByLabelText('Priority of file-1.bin') as HTMLSelectElement).value === 'skip',
      'the cell does not show the new priority after a refresh',
    ).toBe(true)
    expect(container.querySelectorAll('tbody select').length === 3, 'a row has no select').toBe(true)
  })

  it('a second change waits for the first', async () => {
    const { client, sent } = prioClient([file(0), file(1), file(2)])
    let release: () => void = () => {}
    const real = client.setFilePriorities as unknown as (id: string, p: string[]) => Promise<void>
    client.setFilePriorities = vi.fn(async (id: string, p: string[]) => {
      await new Promise<void>((resolve) => {
        release = resolve
      })
      return real(id, p)
    }) as unknown as Api['setFilePriorities']
    render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-0.bin', 'high')
    await flush()
    expect(
      (screen.getByLabelText('Priority of file-2.bin') as HTMLSelectElement).disabled,
      'the other selects stay live while a change is in flight',
    ).toBe(true)
    choose('file-2.bin', 'low')
    await flush()
    release()
    await flush()
    expect(
      sent.length === 1 && vi.mocked(client.setFilePriorities).mock.calls.length === 1,
      'a second change was sent while the first was in flight',
    ).toBe(true)
  })

  it('the new priority shows at once', async () => {
    const { client } = prioClient([file(0), file(1)])
    render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-1.bin', 'high')
    await flush()
    expect(
      (screen.getByLabelText('Priority of file-1.bin') as HTMLSelectElement).value === 'high',
      'the select snapped back before the next refresh',
    ).toBe(true)
  })

  it('a refresh during a change keeps the new value', async () => {
    const old = [file(0), file(1)]
    const sent: string[][] = []
    let release: () => void = () => {}
    const client = {
      files: vi.fn(async () => old),
      peers: vi.fn(async () => []),
      trackers: vi.fn(async () => []),
      pieces: vi.fn(async () => PIECES_10),
      setFilePriorities: vi.fn(async (_id: string, p: string[]) => {
        await new Promise<void>((resolve) => {
          release = resolve
        })
        sent.push(p)
      }),
    } as unknown as Api
    const { rerender } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-1.bin', 'high')
    await flush()
    rerender(<DetailPane id="a" client={client} tick={1} />)
    await flush()
    release()
    await flush()
    expect(
      (screen.getByLabelText('Priority of file-1.bin') as HTMLSelectElement).value === 'high',
      'a refresh that was in flight snapped the new value back',
    ).toBe(true)
  })

  it('after a change, the next refresh wins', async () => {
    let current = [file(0), file(1)]
    const client = {
      files: vi.fn(async () => current),
      peers: vi.fn(async () => []),
      trackers: vi.fn(async () => []),
      pieces: vi.fn(async () => PIECES_10),
      setFilePriorities: vi.fn(async () => {}),
    } as unknown as Api
    const { rerender } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-1.bin', 'high')
    await flush()
    current = [file(0), file(1, { priority: 'low' })]
    rerender(<DetailPane id="a" client={client} tick={1} />)
    await flush()
    expect(
      (screen.getByLabelText('Priority of file-1.bin') as HTMLSelectElement).value === 'low',
      'a stale choice hid what the server now says',
    ).toBe(true)
  })

  it('a list with gaps is refused', async () => {
    const { client } = prioClient([file(0), file(2)])
    const { container } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-0.bin', 'high')
    await flush()
    expect(
      (pane(container)?.textContent ?? '').includes('gaps') &&
        vi.mocked(client.setFilePriorities).mock.calls.length === 0,
      'a gappy list was sent or the refusal was not shown',
    ).toBe(true)
  })

  it('switching torrents clears a pending change', async () => {
    const { client } = prioClient([file(0), file(1)])
    client.setFilePriorities = vi.fn(() => new Promise<void>(() => {})) as unknown as Api['setFilePriorities']
    const { rerender } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-1.bin', 'high')
    await flush()
    rerender(<DetailPane id="b" client={client} tick={0} />)
    await flush()
    expect(
      (screen.getByLabelText('Priority of file-1.bin') as HTMLSelectElement).disabled,
      'the other torrent inherited the pending change',
    ).toBe(false)
  })

  it('pad files keep their priority', async () => {
    const { client, sent } = prioClient([
      file(0),
      file(1, { pad: true, priority: 'skip' }),
      file(2),
    ])
    render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-2.bin', 'high')
    await flush()
    expect(
      sent.length === 1 && JSON.stringify(sent[0]?.priorities) === JSON.stringify(['normal', 'skip', 'high']),
      'the pad file lost its place or its priority in the sent list',
    ).toBe(true)
  })

  it('an unknown priority is sent as normal', async () => {
    const { client, sent } = prioClient([file(0, { priority: 'unknown' }), file(1)])
    render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-1.bin', 'low')
    await flush()
    expect(
      sent.length === 1 && JSON.stringify(sent[0]?.priorities) === JSON.stringify(['normal', 'low']),
      'unknown was sent as it is',
    ).toBe(true)
  })

  it("a refused change shows the server's reason", async () => {
    const { client } = prioClient(
      [file(0), file(1)],
      new ApiError(409, 'no_metadata', 'metadata\u202e is not known\u0007 yet'),
    )
    const { container } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    choose('file-1.bin', 'skip')
    await flush()
    const text = pane(container)?.textContent ?? ''
    expect(text.includes('metadata is not known yet'), 'the refusal was not shown, or not cleaned').toBe(true)
    expect(
      (screen.getByLabelText('Priority of file-1.bin') as HTMLSelectElement).value === 'normal',
      'the cell moved although the server refused',
    ).toBe(true)
  })
})

describe('the files tab', () => {
  const files = [
    file(0, { path: 'ubuntu.iso', len: 5_800_000_000, downloaded: 3_596_000_000 }),
    file(1, { path: 'padding.pad', pad: true }),
    file(2, { path: 'README.txt', len: 4_096, downloaded: 4_096, priority: 'high' }),
    file(3, { path: 'empty.txt', len: 0, downloaded: 0, priority: 'skip' }),
    file(4, { path: 'a\u202e\u0007.txt', len: 1_048_576, downloaded: 0, priority: 'low' }),
  ]

  it('files_show_size_progress_and_priority', async () => {
    const { client } = spyClient({ files: async () => files })
    const { container } = render(<DetailPane id="a" client={client} tick={0} />)
    await flush()

    // The pad file is hidden, as the TUI hides it: five entries in, four real files out.
    const rows = container.querySelectorAll('tbody tr[data-row]')
    expect(rows.length === 4, 'a pad file was shown').toBe(true)

    const first = rowText(container, 0)
    expect(
      first.includes('ubuntu.iso') && first.includes('5.4 GiB') && first.includes('62%'),
      'the first row did not state its size and progress the way the TUI does',
    ).toBe(true)

    // A file at or past its length reads 100 %, and so does a zero-length one (the TUI's
    // `share`: an empty file is whole, not NaN).
    const done = rowText(container, 2)
    const empty = rowText(container, 3)
    expect(done.includes('100%'), 'a finished file did not read 100%').toBe(true)
    expect(empty.includes('0 B') && empty.includes('100%'), 'a zero-length file did not read 100%').toBe(
      true,
    )
    const selected = (name: string): string =>
      (screen.getByLabelText(`Priority of ${name}`) as HTMLSelectElement).value
    expect(
      selected('README.txt') === 'high' &&
        selected('empty.txt') === 'skip' &&
        selected('ubuntu.iso') === 'normal' &&
        selected('a.txt') === 'low',
      'a select does not show its file priority',
    ).toBe(true)

    // Remote paths are display-safe: the bidi override and the bell are gone, the name reads true.
    const clean = rowText(container, 4)
    expect(
      clean.includes('a.txt') && !clean.includes('\u202e'),
      'a hostile path was drawn as it came from the server',
    ).toBe(true)

    // Read-only, as this milestone promises: no buttons and no play in the list.
    const paneEl = pane(container) as HTMLElement
    expect(
      paneEl.querySelectorAll('tbody button').length === 0 &&
        !paneEl.textContent?.includes('Play in browser'),
      'the pane offered to play a file in this milestone',
    ).toBe(true)

    // Pane rows carry the height the window arithmetic multiplies.
    expect(
      (rows[0] as HTMLElement).getAttribute('style') === 'height: 28px;',
      'a pane row is not the height the window counts in',
    ).toBe(true)
  })

  it('long_file_lists_draw_a_window', async () => {
    const many = Array.from({ length: 3_000 }, (_, i) => file(i))
    const { client } = spyClient({ files: async () => many })
    const { container } = render(
      <DetailPane id="a" client={client} tick={0} viewportHeight={224} />,
    )
    await flush()
    const rendered = container.querySelectorAll('tbody tr[data-row]').length
    expect(rendered < 100, 'the files list rendered every file').toBe(true)
    expect(
      container.querySelector('[data-row="0"]') !== null &&
        container.querySelector('[data-row="2999"]') === null,
      'the files window showed the whole list instead of the top of it',
    ).toBe(true)
  })
})

describe('the peers tab', () => {
  it('peers_show_address_client_and_rates', async () => {
    const { client } = spyClient({
      peers: async () => [
        peer({ addr: '198.51.100.7:6881', download_rate: 1_048_576, upload_rate: 2_097_152 }),
        peer({ addr: '[2001:db8::42]:6881', client: null, download_rate: 0, upload_rate: 0 }),
        peer({ addr: '192.0.2.9:6881', client: '\u200etransfer\u0001\u0000 client' }),
      ],
    })
    const { container } = render(<DetailPane id="a" client={client} tick={0} />)
    fireEvent.click(screen.getByRole('tab', { name: 'Peers' }))
    await flush()

    const first = (container.querySelector('tbody tr[data-row="198.51.100.7:6881"]') as HTMLElement)
      .textContent ?? ''
    expect(
      first.includes('Transmission 4.0') &&
        first.includes('1.0 MiB/s') &&
        first.includes('2.0 MiB/s'),
      'the peer row did not state its client and rates',
    ).toBe(true)

    // A peer that never introduced itself gets blank, not a word it did not say.
    const quiet = container.querySelector('tbody tr[data-row="[2001:db8::42]:6881"]') as HTMLElement
    const cells = quiet.querySelectorAll('td')
    expect(
      (cells[1]?.textContent ?? '') === '' && (cells[2]?.textContent ?? '') === '0 B/s',
      'a peer with no client name was given one',
    ).toBe(true)

    // Client names are remote text: the invisible characters are gone.
    const dirty = (container.querySelector('tbody tr[data-row="192.0.2.9:6881"]') as HTMLElement)
      .textContent ?? ''
    expect(
      dirty.includes('transfer client') && !dirty.includes('\u200e'),
      'a peer client name was drawn with its invisible characters',
    ).toBe(true)
  })
})

describe('the trackers tab', () => {
  it('trackers_show_the_status_the_tui_gives', async () => {
    const { client } = spyClient({
      trackers: async () => [
        tracker({ seeders: 38, leechers: 12 }),
        tracker({
          url: 'http://tracker.example.net:2710/announce',
          tier: 1,
          last_error: 'tracker refused: banned',
        }),
      ],
    })
    const { container } = render(<DetailPane id="a" client={client} tick={0} />)
    fireEvent.click(screen.getByRole('tab', { name: 'Trackers' }))
    await flush()

    const url = 'udp://tracker.example.org:6969/announce'
    const ok = (container.querySelector(`tbody tr[data-row="${url}"]`) as HTMLElement).textContent ?? ''
    expect(
      ok.includes('ok') && ok.includes('38/12'),
      'a tracker that accepted the announce was not read as ok with its swarm',
    ).toBe(true)

    const bad = (container.querySelector(
      'tbody tr[data-row="http://tracker.example.net:2710/announce"]',
    ) as HTMLElement)
    const text = bad.textContent ?? ''
    expect(
      text.includes('tracker refused: banned') && text.includes('-/-'),
      'a refused announce lost its reason or its dash counts',
    ).toBe(true)
    expect(
      bad.querySelector('span.flex')?.className.includes('text-error') === true,
      'a tracker error was not drawn in the error colour',
    ).toBe(true)
  })
})

describe('the pieces tab', () => {
  // jsdom has no 2D canvas context; hand the canvas one that records what it is told to draw.
  const realGetContext = HTMLCanvasElement.prototype.getContext
  const drawn: Array<{ x: number; y: number; style: string }> = []
  const scaled: Array<{ x: number; y: number }> = []
  const context = {
    fillStyle: '',
    fillRect(x: number, y: number) {
      drawn.push({ x, y, style: this.fillStyle })
    },
    scale(x: number, y: number) {
      scaled.push({ x, y })
    },
  }
  HTMLCanvasElement.prototype.getContext = vi.fn(() => context) as unknown as typeof realGetContext

  it('many_pieces_draw_on_one_canvas', async () => {
    drawn.length = 0
    scaled.length = 0
    // 5,000 pieces; 'c0' bytes have their top two bits set, so 1,250 of them are had.
    const pieces: PiecesInfo = { num_pieces: 5_000, have: 'c0'.repeat(625) }
    const { client } = spyClient({ pieces: async () => pieces })
    const { container } = render(<DetailPane id="a" client={client} tick={0} />)
    fireEvent.click(screen.getByRole('tab', { name: 'Pieces' }))
    await flush()

    const canvases = container.querySelectorAll('canvas')
    expect(canvases.length === 1, 'more than 2,000 pieces did not draw on exactly one canvas').toBe(
      true,
    )
    expect(
      container.querySelectorAll('[data-piece]').length === 0,
      'past 2,000 pieces every one still got its own element',
    ).toBe(true)
    expect(drawn.length === 5_000, 'the canvas did not draw every piece').toBe(true)
    const have = drawn.filter((cell) => cell.style === '#863bff').length
    const missing = drawn.filter((cell) => cell.style === '#262629').length
    expect(
      have === 1_250 && missing === 3_750,
      'the canvas did not draw the accent violet and the missing grey from the tokens',
    ).toBe(true)
    const label = '1,250 of 5,000 pieces'
    expect(
      canvases[0]?.getAttribute('aria-label') === label &&
        (pane(container) as HTMLElement).textContent?.includes(label),
      'the piece canvas had no exact count as its accessible name and its text',
    ).toBe(true)
    // Device pixels, not CSS pixels: the backing store is the CSS size times the display ratio,
    // and one `ctx.scale` maps drawing back into CSS coordinates. Here the ratio is 1 (jsdom),
    // so the mapping is called exactly once at 1:1 - a canvas that skipped the ratio entirely
    // would never call it at all.
    expect(
      scaled.length === 1 &&
        scaled[0]?.x === 1 &&
        scaled[0]?.y === 1 &&
        canvases[0]?.width === 80 * 12 &&
        canvases[0]?.height === 63 * 12,
      'the canvas backing store was not the CSS size mapped through the display ratio',
    ).toBe(true)
  })

  it('a_short_bitfield_does_not_throw', async () => {
    // 100 claimed pieces, but the hex can only describe so much, and not all of it is hex.
    const cases: { pieces: PiecesInfo; described: number; message: string }[] = [
      {
        // Odd length: '0g' is one byte whose second nibble is not hex (so the byte describes
        // "not had"), and '8' dangles with no pair - nothing to describe.
        pieces: { num_pieces: 100, have: '0g8' },
        described: 8,
        message: 'a short dirty bitfield drew pieces it cannot describe',
      },
      {
        // One dangling character: nothing at all is describable.
        pieces: { num_pieces: 100, have: 'z' },
        described: 0,
        message: 'a single dangling hex character described a bitfield',
      },
      {
        // Two million claimed, four bytes of hex answer: the grid is 32 pieces, not two
        // million, and it never becomes a canvas of bits nobody handed it.
        pieces: { num_pieces: 2_000_000, have: 'c0c0c0c0' },
        described: 32,
        message: 'a two-million-piece claim on four bytes of hex drew more than 32 pieces',
      },
    ]
    for (const testCase of cases) {
      const { client } = spyClient({ pieces: async () => testCase.pieces })
      const { container } = render(<DetailPane id="a" client={client} tick={0} />)
      const piecesTab = Array.from(container.querySelectorAll('[role="tab"]')).find(
        (button) => button.textContent === 'Pieces',
      ) as HTMLElement
      fireEvent.click(piecesTab)
      await flush()
      const cells = container.querySelectorAll('[data-piece]').length
      expect(cells === testCase.described, testCase.message).toBe(true)
      expect(
        (pane(container) as HTMLElement).textContent?.includes(`of ${testCase.described} pieces`),
        testCase.message,
      ).toBe(true)
      cleanup()
    }
  })

  afterEach(() => {
    HTMLCanvasElement.prototype.getContext = realGetContext
  })
})

describe('the tabs on the keyboard', () => {
  it('tabs_follow_the_arrow_keys', async () => {
    const { client } = spyClient({})
    render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    const tabs = ['Files', 'Peers', 'Trackers', 'Pieces'].map((label) =>
      screen.getByRole('tab', { name: label }),
    )

    // Roving tabindex: exactly the chosen tab is in the Tab order, so `Tab` walks the strip the
    // way it walks the TUI's tabs line - one stop, the one that answers to the arrows.
    expect(
      tabs.map((button) => button.getAttribute('tabindex')).join(',') === '0,-1,-1,-1',
      'every tab sat in the Tab order at once',
    ).toBe(true)

    // One panel, one id: switching tabs swaps what the panel shows, it never swaps which element
    // the tabs control. Every tab points at the same panel, and the panel names the chosen tab.
    const panel = document.getElementById('detail-panel')
    expect(panel !== null, 'the pane has no panel with the id every tab points at').toBe(true)
    expect(
      tabs.every((button) => button.getAttribute('aria-controls') === 'detail-panel'),
      'a tab does not control the panel the others control',
    ).toBe(true)
    expect(
      panel?.getAttribute('aria-labelledby') === 'detail-tab-files',
      'the panel did not name the tab it belongs to',
    ).toBe(true)

    // ArrowRight moves the focus and the choice together, in the same key press.
    tabs[0]?.focus()
    const took = fireEvent.keyDown(tabs[0] as HTMLElement, { key: 'ArrowRight' })
    expect(took === false, 'ArrowRight scrolled the page instead of taking the key').toBe(true)
    expect(
      tabs[1]?.getAttribute('aria-selected') === 'true' && document.activeElement === tabs[1],
      'ArrowRight did not move the focus and the choice together',
    ).toBe(true)
    expect(
      tabs.map((button) => button.getAttribute('tabindex')).join(',') === '-1,0,-1,-1',
      'the tab that took the choice did not take its place in the Tab order',
    ).toBe(true)
    expect(
      document.getElementById('detail-panel')?.getAttribute('aria-labelledby') ===
        'detail-tab-peers',
      'the panel kept naming the tab that is no longer chosen',
    ).toBe(true)
    await flush()

    fireEvent.keyDown(tabs[1] as HTMLElement, { key: 'ArrowRight' })
    await flush()
    fireEvent.keyDown(tabs[2] as HTMLElement, { key: 'ArrowRight' })
    await flush()
    expect(
      document.activeElement === tabs[3] && tabs[3]?.getAttribute('aria-selected') === 'true',
      'ArrowRight did not walk to the last tab',
    ).toBe(true)

    // Wrapping at both ends, the way the arrows wrap in the TUI.
    fireEvent.keyDown(tabs[3] as HTMLElement, { key: 'ArrowRight' })
    await flush()
    expect(
      document.activeElement === tabs[0] && tabs[0]?.getAttribute('aria-selected') === 'true',
      'ArrowRight at the last tab did not wrap to the first',
    ).toBe(true)
    fireEvent.keyDown(tabs[0] as HTMLElement, { key: 'ArrowLeft' })
    await flush()
    expect(
      document.activeElement === tabs[3] && tabs[3]?.getAttribute('aria-selected') === 'true',
      'ArrowLeft at the first tab did not wrap to the last',
    ).toBe(true)

    // Home and End jump to the ends.
    fireEvent.keyDown(tabs[3] as HTMLElement, { key: 'Home' })
    await flush()
    expect(
      document.activeElement === tabs[0] && tabs[0]?.getAttribute('aria-selected') === 'true',
      'Home did not move to the first tab',
    ).toBe(true)
    fireEvent.keyDown(tabs[0] as HTMLElement, { key: 'End' })
    await flush()
    expect(
      document.activeElement === tabs[3] && tabs[3]?.getAttribute('aria-selected') === 'true',
      'End did not move to the last tab',
    ).toBe(true)

    // A key the pattern does not claim stays the page's: the tabs do not swallow it.
    expect(fireEvent.keyDown(tabs[3] as HTMLElement, { key: 'x' })).toBe(true)
  })

  it('arrow_keys_on_close_do_not_switch_tabs', async () => {
    const { client } = spyClient({})
    render(<DetailPane id="a" client={client} tick={0} />)
    await flush()
    const tabs = ['Files', 'Peers', 'Trackers', 'Pieces'].map((label) =>
      screen.getByRole('tab', { name: label }),
    )
    const close = screen.getByRole('button', { name: 'Close details' })

    // The Close button lives inside the tablist, and arrows on a focused button belong to that
    // button: no tab moves, no key is taken, and the focus stays where the person put it.
    close.focus()
    const taken = fireEvent.keyDown(close, { key: 'ArrowRight' })
    expect(
      taken === true &&
        tabs[0]?.getAttribute('aria-selected') === 'true' &&
        tabs[1]?.getAttribute('aria-selected') === 'false' &&
        document.activeElement === close,
      'arrow keys on the close button switched tabs behind its own key press',
    ).toBe(true)

    // Home and End are the tablist's too, and hold just the same on the close button.
    const jumped = fireEvent.keyDown(close, { key: 'End' })
    expect(
      jumped === true &&
        tabs[0]?.getAttribute('aria-selected') === 'true' &&
        tabs[3]?.getAttribute('aria-selected') === 'false' &&
        document.activeElement === close,
      'Home and End on the close button moved the tabs',
    ).toBe(true)
  })
})

describe('what the pane shares with the table', () => {
  it('the_pane_has_its_own_height', async () => {
    // The real screen: the pane only has its share of the column if the column gives it one,
    // and the table box only shrinks if the box is the one making room.
    const { client: detail } = spyClient({})
    const client = {
      ...detail,
      torrents: async () => [torrentInfo('a')],
      session: async () => SCREEN_SESSION,
    } as unknown as Api
    const { container } = render(
      <Torrents signedIn signOutError={null} onSignOut={() => {}} client={client} />,
    )
    await flush()
    expect(pane(container) === null, 'the pane was drawn when nothing was selected').toBe(true)

    fireEvent.click(container.querySelector('tbody tr[data-row="a"]') as HTMLElement)
    await flush()
    const section = pane(container) as HTMLElement
    expect(section !== null, 'clicking a row did not open the pane').toBe(true)

    // The pane takes a fixed share of the screen: 40 % of the column, never less than 220 px so
    // it is a pane and not a tab strip, never more than 60 % so the table keeps its rows.
    for (const token of ['h-[40%]', 'min-h-[220px]', 'max-h-[60%]']) {
      expect(
        section.className.includes(token),
        `the pane has no share of the screen (${token} missing)`,
      ).toBe(true)
    }
    // The table gives up the room from its own side: its scroll box keeps `flex-1` and `min-h-0`
    // above the pane, so it shrinks and scrolls instead of growing and squeezing the pane out.
    const box = container.querySelector('table')?.parentElement as HTMLElement
    for (const token of ['flex-1', 'min-h-0', 'overflow-auto']) {
      expect(
        box.className.includes(token),
        `the table box does not shrink to make room for the pane (${token} missing)`,
      ).toBe(true)
    }
  })
})
