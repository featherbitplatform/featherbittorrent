// @vitest-environment jsdom
/**
 * The phone's torrent cards: name, progress bar, state with its percent, and the one speed that matters.
 *
 * @module components/TorrentCards.test
 */
import { cleanup, render } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import TorrentCards from './TorrentCards'
import type { TorrentRow } from '../live'

afterEach(cleanup)

function card(id: string, over: Partial<TorrentRow>): TorrentRow {
  return {
    id,
    name: `torrent ${id}`,
    progress: 0.62,
    download_rate: 4_000_000,
    upload_rate: 320_000,
    num_peers: 0,
    num_seeds: 0,
    state: 'downloading',
    rowState: 'downloading',
    ...over,
  } as TorrentRow
}

describe('a torrent card', () => {
  it('a card shows name, progress, state and the speed that matters', () => {
    const rows = [
      card('d', { name: 'down‮load', rowState: 'downloading' }),
      card('s', { name: 'seed', rowState: 'seeding', progress: 1 }),
      card('t', {
        name: 'stall',
        rowState: 'stalled',
        progress: 0.18,
        num_peers: 0,
      }),
    ]
    const { container } = render(<TorrentCards rows={rows} onPick={() => {}} />)
    const cardOf = (id: string): HTMLElement =>
      container.querySelector(`[data-card="${id}"]`) as HTMLElement

    const d = cardOf('d')
    // The name passes through displaySafe: the bidi override is gone.
    expect(d.textContent, 'the name was not made display-safe').toContain('download')
    expect(d.textContent).not.toContain('‮')
    expect(d.textContent, 'a downloading card lost its state and percent').toContain(
      'downloading 62%',
    )
    expect(d.textContent, 'a downloading card must show the download rate').toContain('/s down')
    expect(d.textContent).not.toContain('up')
    expect(d.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')).toBe('62')

    const s = cardOf('s')
    expect(s.textContent, 'a seeding card must show the upload rate').toContain('/s up')
    expect(s.textContent).not.toContain('down')

    const t = cardOf('t')
    expect(t.textContent, 'a stalled card must show the peer count').toContain('7 peers')
  })

  it('every state card shows its percent and only the right speed', () => {
    const rows = [
      card('s', { rowState: 'seeding', progress: 1 }),
      card('p', { rowState: 'paused', progress: 0.5 }),
      card('e', { rowState: 'error', progress: 0.3 }),
      card('c', { rowState: 'checking', progress: 0.9, checking_progress: 0.25 }),
    ]
    const { container } = render(<TorrentCards rows={rows} onPick={() => {}} />)
    const text = (id: string): string =>
      (container.querySelector(`[data-card="${id}"]`) as HTMLElement).textContent
    expect(text('s'), 'a seeding card lost its percent').toContain('seeding 100%')
    expect(text('p')).toContain('paused 50%')
    expect(text('e')).toContain('error 30%')
    expect(text('c'), 'a checking card must show the check fraction').toContain('checking 25%')
    for (const id of ['p', 'e', 'c']) {
      expect(text(id), `a ${id} card showed a speed`).not.toMatch(/\/s|peers/)
    }
  })

  it('a card name is cut to one line', () => {
    const { container } = render(<TorrentCards rows={[card('a', {})]} onPick={() => {}} />)
    const name = container.querySelector('[data-card="a"] > span') as HTMLElement
    expect(name.className, 'the name can wrap and grow the card').toContain('truncate')
  })
})
