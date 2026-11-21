// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import TorrentTable from './TorrentTable'
import Torrents from '../screens/Torrents'
import type { Api, TorrentInfo } from '../api/client'
import type { RowState, TorrentRow } from '../live'

afterEach(cleanup)

function torrent(id: string, over: Partial<TorrentInfo> = {}): TorrentInfo {
  return {
    id,
    name: over.name ?? `torrent ${id}`,
    progress: 0.4,
    download_rate: 0,
    upload_rate: 0,
    num_peers: 0,
    num_seeds: 0,
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

function row(id: string, rowState: RowState, over: Partial<TorrentInfo> = {}): TorrentRow {
  return { ...torrent(id, over), rowState }
}

const visibleIds = (container: HTMLElement): string[] =>
  Array.from(container.querySelectorAll('tbody tr[data-row]')).map((tr) =>
    tr.getAttribute('data-row') ?? '',
  )

const joined = (ids: string[]): string => ids.join(',')

const SESSION_BODY = {
  version: '0.13.5',
  download_limit: 0,
  upload_limit: 0,
  download_rate: 1_000_000,
  upload_rate: 250_000,
  listen_addrs: ['0.0.0.0:51497'],
  torrents: 4,
  dht_nodes: 120,
}

describe('the windowed table', () => {
  it('two_thousand_rows_render_under_a_hundred', () => {
    // Each progress value is unique and rises with the index, so the default progress order is
    // also mount order and window maths can be read straight off the ids.
    const rows = Array.from({ length: 2_000 }, (_, i) =>
      row(`t${i}`, 'downloading', {
        name: `big torrent ${i}`,
        progress: i / 2_000,
        download_rate: 1_000 + i,
      }),
    )
    // The name of this test is a promise: two thousand rows go in, and the paint must stay under
    // a blink. The timing check runs last so the row-count message above is what a non-windowed
    // table gets reported for.
    const started = Date.now()
    const { container } = render(
      <TorrentTable rows={rows} selectedId={null} viewportHeight={900} />,
    )
    const paintedIn = Date.now() - started

    const rendered = container.querySelectorAll('tbody tr[data-row]').length
    expect(rendered < 100, 'the table rendered every row').toBe(true)

    // It is the right window, not just a small one: the top rows are in it and the far ones are not.
    expect(container.querySelector('[data-row="t0"]') !== null, 'the window cut off the first row').toBe(true)
    expect(container.querySelector('[data-row="t1999"]') === null, 'the table rendered every row').toBe(true)

    // Scrolling moves the window, and the top spacer grows by exactly the height scrolled away.
    // jsdom lays nothing out, so `scrollTop` is as read-only as `clientHeight`: the test hands the
    // box its scroll position the same way it hands the table its height.
    const box = container.querySelector('table')?.parentElement as HTMLElement
    Object.defineProperty(box, 'scrollTop', { value: 4_000, configurable: true })
    fireEvent.scroll(box)
    const ids = visibleIds(container)
    expect(ids[0] === 't94' && ids.at(-1) === 't128', 'the window did not follow the scroll').toBe(true)
    const spacer = container.querySelector('[data-spacer="top"]') as HTMLElement | null
    expect(
      spacer !== null && (spacer.getAttribute('style') ?? '').includes('3760px'),
      'the scrolled table lost the spacer that keeps the scrollbar honest',
    ).toBe(true)
    expect(paintedIn < 100, `two thousand rows took ${paintedIn} ms to paint`).toBe(true)
  })
})

describe('headers and the filter box', () => {
  // Four rows whose order differs for every column, so a header wired to the wrong field
  // cannot pass by accident: the orders below are each a different permutation.
  const rows = [
    row('a', 'downloading', { name: 'alpha', progress: 0.25, download_rate: 100, upload_rate: 5, num_peers: 1 }),
    row('b', 'seeding', { name: 'Beta', progress: 0.5, download_rate: 4_000, upload_rate: 900, num_peers: 3 }),
    row('c', 'stalled', { name: 'gamma', progress: 0.75, download_rate: 400, upload_rate: 40, num_peers: 2 }),
    row('d', 'paused', { name: 'delta', progress: 0.1, download_rate: 20, upload_rate: 200, num_peers: 5 }),
  ]

  const columns: { label: string; order: string[] }[] = [
    { label: 'Name', order: ['a', 'b', 'd', 'c'] },
    { label: 'Progress', order: ['d', 'a', 'b', 'c'] },
    { label: 'Down', order: ['d', 'a', 'c', 'b'] },
    { label: 'Up', order: ['a', 'c', 'd', 'b'] },
    { label: 'Peers', order: ['a', 'c', 'b', 'd'] },
    { label: 'State', order: ['a', 'd', 'b', 'c'] },
  ]

  it('sorts_by_any_column_and_filters_by_name', () => {
    const { container } = render(<TorrentTable rows={rows} selectedId={null} viewportHeight={900} />)
    expect(container.querySelectorAll('thead th').length, 'the table has the wrong columns').toBe(6)

    for (const column of columns) {
      const button = screen.getByRole('button', { name: column.label })

      fireEvent.click(button)
      expect(
        joined(visibleIds(container)),
        `clicking ${column.label} once did not order it ascending`,
      ).toBe(column.order.join(','))
      const ascending = container.querySelectorAll('thead th[aria-sort="ascending"]')
      expect(
        ascending.length === 1 && (ascending[0].textContent ?? '').includes(column.label),
        `the ${column.label} column did not mark itself ascending`,
      ).toBe(true)

      // The next click reverses the same column; the toolbar says so out loud.
      fireEvent.click(button)
      expect(
        joined(visibleIds(container)),
        `clicking ${column.label} twice did not reverse it`,
      ).toBe([...column.order].reverse().join(','))
      expect(
        container.querySelectorAll('thead th[aria-sort="descending"]').length === 1 &&
          (screen.getByText(/^Sorted by/).textContent ?? '').includes('(reverse)'),
        `the reversed ${column.label} column did not say so`,
      ).toBe(true)
    }

    // The filter narrows by displayed name, case-insensitively, and only the matching row survives.
    const search = screen.getByRole('searchbox')
    fireEvent.change(search, { target: { value: 'beta' } })
    expect(joined(visibleIds(container)), 'the filter did not narrow to the row it matches').toBe('b')

    fireEvent.change(search, { target: { value: 'GAM' } })
    expect(joined(visibleIds(container)), 'a capitalised filter found nothing it should have').toBe('c')

    fireEvent.change(search, { target: { value: '' } })
    expect(visibleIds(container).length === 4, 'clearing the filter lost rows').toBe(true)
  })
})

describe('phone width', () => {
  it('the_layout_stacks_at_phone_width', async () => {
    const client = {
      torrents: async () => [],
      session: async () => SESSION_BODY,
    } as unknown as Api
    const { container } = render(
      <Torrents signedIn signOutError={null} onSignOut={() => {}} client={client} />,
    )
    // Let the mount effects' reads land before looking at anything.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
      await new Promise((resolve) => setTimeout(resolve, 0))
    })

    // The filters are the column that folds: the same buttons, in a scrolling row on phones,
    // declared one breakpoint down rather than watched from JS.
    const nav = screen.getByRole('navigation')
    for (const token of ['max-md:flex-row', 'max-md:w-full', 'max-md:overflow-x-auto', 'max-md:border-b']) {
      expect(
        nav.className.includes(token),
        `the filter column does not become a row at phone width (${token} missing)`,
      ).toBe(true)
    }
    expect(
      nav.parentElement !== null && nav.parentElement.className.includes('flex-wrap'),
      'the shell never wraps the sidebar above the table',
    ).toBe(true)

    // The table keeps its own horizontal scroll instead of squashing columns to nothing.
    expect(
      container.querySelector('table') !== null &&
        (container.querySelector('table')?.getAttribute('class') ?? '').includes('min-w-[760px]'),
      'the table has no floor width to scroll sideways down to',
    ).toBe(true)
  })
})
