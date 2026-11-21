// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import DetailPane from './DetailPane'
import { ApiError } from '../api/client'
import type { Api, FileInfo, PeerInfo, PiecesInfo, TrackerInfo } from '../api/client'

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
    expect(done.includes('100%') && done.includes('high'), 'a finished file did not read 100%').toBe(
      true,
    )
    expect(empty.includes('0 B') && empty.includes('100%'), 'a zero-length file did not read 100%').toBe(
      true,
    )
    expect(rowText(container, 4).includes('low'), 'the priority word is missing').toBe(true)

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
  const context = {
    fillStyle: '',
    fillRect(x: number, y: number) {
      drawn.push({ x, y, style: this.fillStyle })
    },
  }
  HTMLCanvasElement.prototype.getContext = vi.fn(() => context) as unknown as typeof realGetContext

  it('many_pieces_draw_on_one_canvas', async () => {
    drawn.length = 0
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
