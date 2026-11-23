// @vitest-environment jsdom
/**
 * Every modal `<dialog>` carries the one shared phone class string, so each fills a phone screen
 * instead of sitting under Chrome's own `max-width` and `max-height` caps.
 *
 * @module modal.test
 */
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AddTorrent from './components/AddTorrent'
import ConfirmDelete from './components/ConfirmDelete'
import Devices from './components/settings/Devices'
import TorrentActions from './components/TorrentActions'
import TorrentLimits from './components/TorrentLimits'
import { installDialogShim } from './dialog-shim'
import { PHONE_FULLSCREEN } from './modal'
import type { Api, TorrentInfo } from './api/client'

afterEach(cleanup)
installDialogShim()

function dialogClass(): string {
  const found = document.querySelector('dialog')
  expect(found, 'no dialog is open').not.toBeNull()
  return (found as HTMLDialogElement).className
}

function stub(): Api {
  return {
    devices: vi.fn(async () => [{ name: 'phone', fingerprint: 'SHA256:aaa' }]),
    roots: vi.fn(async () => ['/downloads']),
    list: vi.fn(async (path: string) => ({ path, dirs: [] })),
  } as unknown as Api
}

describe('modal dialogs', () => {
  it('every dialog carries the shared phone class', async () => {
    const client = stub()
    const noop = (): void => {}

    render(<AddTorrent client={client} onAdded={noop} onClose={noop} />)
    expect(dialogClass()).toContain(PHONE_FULLSCREEN)
    cleanup()

    render(<ConfirmDelete id="a" name="n" client={client} onClose={noop} onRemoved={noop} />)
    expect(dialogClass()).toContain(PHONE_FULLSCREEN)
    cleanup()

    render(<TorrentLimits id="a" client={client} onClose={noop} />)
    expect(dialogClass()).toContain(PHONE_FULLSCREEN)
    cleanup()

    render(
      <TorrentActions
        torrent={{ id: 'a', queue_position: 1, auto_managed: true, state: 'downloading' } as TorrentInfo}
        client={client}
        onError={noop}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'More' }))
    fireEvent.click(screen.getByRole('menuitem', { name: /move files/i }))
    expect(dialogClass(), 'Move files').toContain(PHONE_FULLSCREEN)
    cleanup()

    render(<Devices client={client} />)
    fireEvent.click(await screen.findByRole('button', { name: 'Remove phone' }))
    expect(dialogClass(), 'Remove device').toContain(PHONE_FULLSCREEN)
  })
})
