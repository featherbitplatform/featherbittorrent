// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import { useState } from 'react'
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

const rowsOf = (n: number): TorrentRow[] =>
  Array.from({ length: n }, (_, i) =>
    row(`t${i}`, 'downloading', {
      name: `big torrent ${i}`,
      progress: i / n,
      download_rate: 1_000 + i,
    }),
  )

const visibleIds = (container: HTMLElement): string[] =>
  Array.from(container.querySelectorAll('tbody tr[data-row]')).map((tr) =>
    tr.getAttribute('data-row') ?? '',
  )

const joined = (ids: string[]): string => ids.join(',')

/** jsdom lays nothing out, so `scrollTop` is as read-only as `clientHeight`: the test hands the box its scroll position the same way it hands the table its height. */
const setScrollTop = (container: HTMLElement, value: number): HTMLElement => {
  const box = container.querySelector('table')?.parentElement as HTMLElement
  Object.defineProperty(box, 'scrollTop', { value, configurable: true, writable: true })
  fireEvent.scroll(box)
  return box
}

/** A parent that owns `selectedId`, the way `Torrents` does. */
function PickingTable({ rows, initial }: { rows: TorrentRow[]; initial: string | null }) {
  const [selected, setSelected] = useState<string | null>(initial)
  return <TorrentTable rows={rows} selectedId={selected} onPick={setSelected} viewportHeight={900} />
}

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
    // Nothing is sorted until a header is clicked, so the drawn order is the order these rows
    // were listed in and window maths can be read straight off the ids.
    const rows = rowsOf(2_000)
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
    setScrollTop(container, 4_000)
    const ids = visibleIds(container)
    expect(ids[0] === 't94' && ids.at(-1) === 't128', 'the window did not follow the scroll').toBe(true)
    const spacer = container.querySelector('[data-spacer="top"]') as HTMLElement | null
    expect(
      spacer !== null && (spacer.getAttribute('style') ?? '').includes('3760px'),
      'the scrolled table lost the spacer that keeps the scrollbar honest',
    ).toBe(true)
    expect(paintedIn < 100, `two thousand rows took ${paintedIn} ms to paint`).toBe(true)
  })

  it('scrolling_moves_the_window', () => {
    const { container } = render(
      <TorrentTable rows={rowsOf(2_000)} selectedId={null} viewportHeight={900} />,
    )
    setScrollTop(container, 40 * 1_000)

    const ids = visibleIds(container)
    expect(ids.length < 100, 'the table rendered every row').toBe(true)
    // The window follows the scroll: deep into the list, row 1000 is the point being looked at,
    // reachable within the overscan the window keeps around it.
    const first = Number(ids[0]?.slice(1))
    expect(
      ids.includes('t1000') && first >= 1_000 - 6 - 1 && first <= 1_000,
      'scrolling did not move the window to row 1000',
    ).toBe(true)
  })

  it('every_row_is_one_height', () => {
    const rows = [
      row('a', 'downloading'),
      row('b', 'error', { error: 'x'.repeat(255) }),
      row('c', 'seeding'),
    ]
    const { container } = render(<TorrentTable rows={rows} selectedId={null} viewportHeight={900} />)

    // The spacer arithmetic only stays honest if a row is exactly ROW_HEIGHT tall whatever it
    // holds: separate borders that add nothing, and the rule drawn inside the row's own height.
    const table = container.querySelector('table') as HTMLElement
    expect(
      table.className.includes('border-separate') && table.className.includes('border-spacing-0'),
      'collapsing borders can make a 40px row render 40.5px',
    ).toBe(true)
    for (const tr of Array.from(container.querySelectorAll('tbody tr[data-row]'))) {
      expect(tr.getAttribute('style'), 'a body row is not exactly 40px tall').toBe('height: 40px;')
      for (const td of Array.from(tr.querySelectorAll('td'))) {
        expect(
          td.className.includes('shadow-[inset_0_-1px_0_var(--color-ink-750)'),
          'the row rule was drawn where it can add height',
        ).toBe(true)
      }
    }

    // A 255-character error is one line of dots with the whole sentence in its title, not a
    // paragraph that wraps the row past 40px.
    const stateCell = container.querySelector('[data-row="b"] td:nth-child(6)') as HTMLElement
    const whole = `error: ${'x'.repeat(255)}`
    expect(
      stateCell.title === whole &&
        stateCell.className.includes('truncate') &&
        stateCell.className.includes('max-w-['),
      'a long error wrapped the row instead of truncating with the full text in the title',
    ).toBe(true)

    // Every cell truncates rather than wraps: one line, whatever the server put in it.
    const nameCell = container.querySelector('[data-row="b"] td:nth-child(1)') as HTMLElement
    const numberCell = container.querySelector('[data-row="b"] td:nth-child(3)') as HTMLElement
    expect(
      nameCell.className.includes('truncate') && numberCell.className.includes('truncate'),
      'a cell was left free to wrap and change the row height',
    ).toBe(true)
  })
})

describe('the keyboard', () => {
  it('rows_are_reachable_from_the_keyboard', () => {
    const rows = rowsOf(2_000)
    const { container } = render(<PickingTable rows={rows} initial={null} />)

    const rowA = container.querySelector('[data-row="t0"]') as HTMLElement
    expect(rowA.getAttribute('tabindex') === '0', 'a row cannot be focused').toBe(true)
    expect(
      rowA.className.includes('focus-visible:outline-accent-light'),
      'the focused row has no accent-coloured focus ring',
    ).toBe(true)

    rowA.focus()
    expect(document.activeElement === rowA, 'the row did not take focus').toBe(true)
    fireEvent.keyDown(rowA, { key: 'Enter' })
    expect(
      container.querySelector('[data-row="t0"][aria-selected="true"]') !== null,
      'Enter on a row did not select it',
    ).toBe(true)

    // The arrows move the selection along the drawn order and the box scrolls to follow it:
    // from row 1499 down to row 1500, which no window was showing, is the case that matters.
    const deep = render(<PickingTable rows={rows} initial={'t1499'} />)
    expect(
      visibleIds(deep.container).includes('t1499'),
      'a selection made off screen was never scrolled into view',
    ).toBe(true)
    const deepRow = deep.container.querySelector('[data-row="t1499"]') as HTMLElement
    fireEvent.keyDown(deepRow, { key: 'ArrowDown' })
    const moved = visibleIds(deep.container)
    expect(
      moved.includes('t1500') &&
        deep.container.querySelector('[data-row="t1500"][aria-selected="true"]') !== null &&
        moved.length < 100,
      'the arrows moved the selection out of the window without scrolling it in',
    ).toBe(true)

    // Space selects too - and does not scroll the page underneath, which is why it is prevented.
    const rowB = container.querySelector('[data-row="t1"]') as HTMLElement
    rowB.focus()
    fireEvent.keyDown(rowB, { key: ' ' })
    expect(
      container.querySelector('[data-row="t1"][aria-selected="true"]') !== null,
      'Space on a row did not select it',
    ).toBe(true)
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
  const serverOrder = 'a,b,c,d'

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

    // Nothing is sorted until a header says so: the rows start in the order the server listed them.
    expect(joined(visibleIds(container)), 'the table sorted before anyone asked').toBe(serverOrder)
    expect(
      screen.getByText('In server order') !== null &&
        container.querySelectorAll('thead th[aria-sort="none"]').length === 6,
      'the table claimed a sort it had not been given',
    ).toBe(true)

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

      // The third click hands the order back to the server: no column is in charge any more.
      fireEvent.click(button)
      expect(
        joined(visibleIds(container)),
        `clicking ${column.label} a third time did not return to server order`,
      ).toBe(serverOrder)
      expect(
        screen.getByText('In server order') !== null &&
          container.querySelectorAll('thead th[aria-sort="none"]').length === 6,
        `a third click on ${column.label} left a column in charge`,
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

    // The screen is exactly one viewport tall with a shrinkable chain all the way to the table:
    // no ancestor left unbounded, or the scroll box grows to its content and never scrolls.
    const root = container.firstElementChild as HTMLElement
    for (const token of ['h-dvh', 'flex-col', 'overflow-hidden']) {
      expect(root.className.includes(token), `the screen does not bound its height (${token} missing)`).toBe(true)
    }
    const main = screen.getByRole('main')
    expect(main.className.includes('min-h-0'), 'the table column cannot shrink').toBe(true)

    // The filters are the column that folds: a scrolling row of the same buttons on phones,
    // `md:` turning it back into the sidebar, decided in CSS not JS.
    const nav = screen.getByRole('navigation')
    for (const token of ['flex-row', 'overflow-x-auto', 'md:flex-col', 'md:w-60']) {
      expect(
        nav.className.includes(token),
        `the filter column does not become a row at phone width (${token} missing)`,
      ).toBe(true)
    }

    // The table keeps its own horizontal scroll instead of squashing columns to nothing.
    expect(
      container.querySelector('table') !== null &&
        (container.querySelector('table')?.getAttribute('class') ?? '').includes('min-w-[760px]'),
      'the table has no floor width to scroll sideways down to',
    ).toBe(true)
  })
})
