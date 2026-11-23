// @vitest-environment jsdom
/**
 * Every modal `<dialog>` carries the one shared class string: centred in the window (Tailwind's
 * preflight zeroes the `margin: auto` a browser centres a modal with), and filling a phone screen
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
import { MODAL_CENTRED, MODAL_DIALOG, PHONE_FULLSCREEN } from './modal'
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

/** The open dialog's classes, which must hold the shared modal string: centred, and full screen on a phone. */
function expectSharedModal(what: string): void {
  const classes = dialogClass()
  expect(classes, `${what} does not carry the shared modal classes`).toContain(MODAL_DIALOG)
  expect(classes.split(' '), `${what} is not centred (preflight pins it to the top left)`).toContain('m-auto')
}

describe('modal dialogs', () => {
  const noop = (): void => {}

  it('the shared string centres and keeps the phone classes', () => {
    expect(MODAL_DIALOG.split(' ')).toContain(MODAL_CENTRED)
    expect(MODAL_DIALOG).toContain(PHONE_FULLSCREEN)
    // The phone's `m-0` is what overrides the centring at 640 px and below.
    expect(PHONE_FULLSCREEN.split(' ')).toContain('max-[640px]:m-0')
  })

  it('Add torrent is a shared modal', () => {
    render(<AddTorrent client={stub()} onAdded={noop} onClose={noop} />)
    expectSharedModal('Add torrent')
  })

  it('Remove torrent is a shared modal', () => {
    render(<ConfirmDelete id="a" name="n" client={stub()} onClose={noop} onRemoved={noop} />)
    expectSharedModal('Remove torrent')
  })

  it('Limits is a shared modal', () => {
    render(<TorrentLimits id="a" client={stub()} onClose={noop} />)
    expectSharedModal('Limits')
  })

  it('Move files is a shared modal', () => {
    render(
      <TorrentActions
        torrent={{ id: 'a', queue_position: 1, auto_managed: true, state: 'downloading' } as TorrentInfo}
        client={stub()}
        onError={noop}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'More' }))
    fireEvent.click(screen.getByRole('menuitem', { name: /move files/i }))
    expectSharedModal('Move files')
  })

  it('Remove device is a shared modal', async () => {
    render(<Devices client={stub()} />)
    fireEvent.click(await screen.findByRole('button', { name: 'Remove phone' }))
    expectSharedModal('Remove device')
  })
})
