// @vitest-environment jsdom
/**
 * The per-torrent actions menu, against a fake client that records what the page asked of it.
 *
 * Each rule is about what leaves the page: queue moves are absolute positions, the queue items exist
 * only for a torrent that is in the queue, limits leave as bytes per second, and a move is only ever
 * the folder the picker named.
 *
 * @module components/TorrentActions.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import TorrentActions from './TorrentActions'
import { ApiError } from '../api/client'
import type { Api, TorrentInfo } from '../api/client'
import { installDialogShim } from '../dialog-shim'

afterEach(cleanup)
installDialogShim()

function torrent(over: Partial<TorrentInfo> = {}): TorrentInfo {
  return { id: 'abc123', queue_position: 3, auto_managed: true, state: 'downloading', ...over } as TorrentInfo
}

function fake(over: Partial<Record<keyof Api, unknown>> = {}) {
  const client = {
    setQueuePosition: vi.fn(async () => {}),
    recheck: vi.fn(async () => {}),
    reannounce: vi.fn(async () => {}),
    setTorrentLimits: vi.fn(async () => {}),
    moveStorage: vi.fn(async () => {}),
    roots: vi.fn(async () => ['/downloads']),
    list: vi.fn(async (path: string) => ({ path, dirs: ['movies'] })),
    ...over,
  }
  return client as unknown as Api & typeof client
}

function open(client: Api, t: TorrentInfo = torrent(), onError = vi.fn()) {
  render(<TorrentActions torrent={t} client={client} onError={onError} />)
  fireEvent.click(screen.getByRole('button', { name: 'More' }))
  return onError
}

describe('TorrentActions', () => {
  it('queue moves send absolute positions', () => {
    const client = fake()
    open(client)
    fireEvent.click(screen.getByRole('menuitem', { name: 'Move up' }))
    fireEvent.click(screen.getByRole('button', { name: 'More' }))
    fireEvent.click(screen.getByRole('menuitem', { name: 'Move down' }))
    fireEvent.click(screen.getByRole('button', { name: 'More' }))
    fireEvent.click(screen.getByRole('menuitem', { name: 'Move to top' }))
    fireEvent.click(screen.getByRole('button', { name: 'More' }))
    fireEvent.click(screen.getByRole('menuitem', { name: 'Move to bottom' }))
    expect(client.setQueuePosition.mock.calls).toEqual([
      ['abc123', 2],
      ['abc123', 4],
      ['abc123', 0],
      ['abc123', 2147483647],
    ])
  })

  it('queue controls show only for a queued torrent', () => {
    for (const over of [{ auto_managed: false }, { queue_position: -1 }]) {
      open(fake(), torrent(over))
      expect(screen.queryByRole('menuitem', { name: 'Move up' })).toBeNull()
      expect(screen.queryByRole('menuitem', { name: 'Move to bottom' })).toBeNull()
      expect(screen.getByRole('menuitem', { name: 'Recheck' })).toBeTruthy()
      cleanup()
    }
  })

  it('recheck and reannounce call the server', () => {
    const client = fake()
    open(client)
    fireEvent.click(screen.getByRole('menuitem', { name: 'Recheck' }))
    fireEvent.click(screen.getByRole('button', { name: 'More' }))
    fireEvent.click(screen.getByRole('menuitem', { name: 'Reannounce' }))
    expect(client.recheck).toHaveBeenCalledTimes(1)
    expect(client.recheck).toHaveBeenCalledWith('abc123')
    expect(client.reannounce).toHaveBeenCalledTimes(1)
    expect(client.reannounce).toHaveBeenCalledWith('abc123')
  })

  it('torrent limits open empty and send bytes per second', async () => {
    const client = fake()
    open(client)
    fireEvent.click(screen.getByRole('menuitem', { name: /limits/i }))
    const down = screen.getByLabelText(/download/i) as HTMLInputElement
    const up = screen.getByLabelText(/upload/i) as HTMLInputElement
    expect(down.value).toBe('')
    expect(up.value).toBe('')
    expect(screen.getByText('Sets a new limit. 0 removes it.')).toBeTruthy()
    fireEvent.change(down, { target: { value: '500' } })
    fireEvent.change(up, { target: { value: '0' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => {
      expect(client.setTorrentLimits).toHaveBeenCalledWith('abc123', 512000, null)
    })
  })

  it('move uses the picker and never overwrites', async () => {
    let finish: () => void = () => {}
    const client = fake({
      moveStorage: vi.fn(
        () =>
          new Promise<void>((resolve) => {
            finish = resolve
          }),
      ),
    })
    open(client)
    fireEvent.click(screen.getByRole('menuitem', { name: /move files/i }))
    const folder = await waitFor(() => {
      const found = document.querySelector('[data-folder="/downloads"]')
      expect(found).not.toBeNull()
      return found as HTMLElement
    })
    fireEvent.click(folder)
    fireEvent.click(screen.getByRole('button', { name: 'Move' }))
    expect(client.moveStorage).toHaveBeenCalledWith('abc123', '/downloads')
    expect(await screen.findByText('Moving...')).toBeTruthy()
    finish()
    await waitFor(() => {
      expect(screen.queryByText('Moving...')).toBeNull()
    })
  })

  it('a refused move shows the reason in the banner', async () => {
    const client = fake({
      moveStorage: vi.fn(async () => {
        throw new ApiError(409, 'destination_exists', 'that folder already holds this torrent')
      }),
    })
    const onError = open(client)
    fireEvent.click(screen.getByRole('menuitem', { name: /move files/i }))
    await waitFor(() => {
      expect(document.querySelector('[data-folder="/downloads"]')).not.toBeNull()
    })
    fireEvent.click(screen.getByRole('button', { name: 'Move' }))
    await waitFor(() => {
      expect(onError).toHaveBeenCalledWith('that folder already holds this torrent')
    })
  })
})
