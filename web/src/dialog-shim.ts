/**
 * The parts a jsdom `<dialog>` does not implement, for the tests that drive a real dialog.
 *
 * jsdom builds a `<dialog>` and keeps its `open` attribute, but implements none of the interesting
 * behaviour: no `showModal`, no `close`, no focus moving on open, and no Escape dismissal. A dialog
 * written the way a browser tells you to write it - answering the element's own `cancel` and `close`
 * events instead of key handlers - would therefore never close in a test at all. This stub supplies
 * the browser's side of that contract:
 *
 * - `showModal` marks the element open, remembers it as the top-most modal, and moves the focus to
 *   the first control inside it, the way a browser takes the focus into a modal.
 * - `close` on a closed element does nothing and fires nothing, as in Chrome; on an open one it
 *   clears `open`, leaves the modal list, and dispatches the bubbling `close` event.
 * - a keydown carrying `Escape` is answered the way Chrome answers it once the page has had its
 *   chance: a cancelable `cancel` event dispatched on the top-most open modal dialog and, if
 *   nothing prevented that event, a `close()` on the element.
 *
 * The Escape hook listens on `document` in the bubble phase on purpose: that is where every handler
 * the page itself put on the event - React's delegated listeners included - has already run, so the
 * dialog decides what to do with `cancel` before the stub decides whether the element closes.
 *
 * @module dialog-shim
 */
import { FOCUSABLE_SELECTOR } from './modal'

/** The dialogs the stub below has opened as modal, most recent last: the one an Escape belongs to. */
const openedModals: HTMLDialogElement[] = []

let escapeHooked = false

export function installDialogShim(): void {
  const proto = window.HTMLDialogElement?.prototype
  if (!proto) return

  if (!proto.showModal) {
    Object.defineProperty(proto, 'showModal', {
      configurable: true,
      writable: true,
      value: function showModal(this: HTMLDialogElement): void {
        this.setAttribute('open', '')
        const seen = openedModals.indexOf(this)
        if (seen !== -1) openedModals.splice(seen, 1)
        openedModals.push(this)
        const first = this.querySelector<HTMLElement>(FOCUSABLE_SELECTOR)
        if (first !== null) first.focus()
      },
    })
  }

  if (!proto.close) {
    Object.defineProperty(proto, 'close', {
      configurable: true,
      writable: true,
      value: function close(this: HTMLDialogElement): void {
        // Chrome: a dialog that is already closed ignores `close()` and fires nothing.
        if (!this.hasAttribute('open')) return
        this.removeAttribute('open')
        const seen = openedModals.indexOf(this)
        if (seen !== -1) openedModals.splice(seen, 1)
        this.dispatchEvent(new Event('close', { bubbles: true, cancelable: false }))
      },
    })
  }

  if (!escapeHooked) {
    escapeHooked = true
    document.addEventListener('keydown', (event: KeyboardEvent): void => {
      if (event.key !== 'Escape') return
      // Top-most modal first, and one that is no longer on the page or no longer open is not a
      // modal at all, whatever the list remembers.
      for (let index = openedModals.length - 1; index >= 0; index -= 1) {
        const dialog = openedModals[index] as HTMLDialogElement
        if (!dialog.isConnected || !dialog.hasAttribute('open')) {
          openedModals.splice(index, 1)
          continue
        }
        const cancel = new Event('cancel', { bubbles: true, cancelable: true })
        dialog.dispatchEvent(cancel)
        if (!cancel.defaultPrevented) dialog.close()
        return
      }
    })
  }
}
