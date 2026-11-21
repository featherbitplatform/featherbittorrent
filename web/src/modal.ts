/**
 * The two things a `<dialog>` does not do by itself: keep the keyboard inside itself, and give the
 * focus back when it goes.
 *
 * `showModal()` puts the element in the top layer, blocks the page behind it and moves the focus
 * inside - a browser takes it from whatever held it - but Tab still walks out of the dialog into the
 * table underneath, and a dialog that is unmounted rather than closed, which is how a screen closes
 * one, never hands the focus back on its own. This hook does both, so the Add dialog and the remove
 * dialog are one behaviour rather than two copies of it, and each dialog's own tests are about the
 * dialog and not about the trap.
 *
 * Opening and the reading of the opener happen in the same layout effect, in that order, and the
 * order is the whole point: by the time `showModal()` returns the focus is already inside the dialog,
 * so an opener read after it is a control of the dialog's own - disconnected the moment the dialog
 * goes away, which leaves the focus on `body` and the person back at the top of the page. A layout
 * effect is also the only place a dialog can be opened before it paints. The opener is therefore
 * captured on the line above `showModal()`, and restored in the cleanup of the same effect.
 *
 * @module modal
 */
import { useCallback, useEffect, useLayoutEffect, useRef } from 'react'
import type { KeyboardEvent, RefObject } from 'react'

/** What a dialog component needs from this hook: the element to attach to, and its `onKeyDown`. */
export interface ModalDialog {
  ref: RefObject<HTMLDialogElement | null>
  onKeyDown(event: KeyboardEvent<HTMLDialogElement>): void
}

/**
 * Everything a Tab key can land on, in the order the keyboard reaches them. Exported because
 * `dialog-shim` has to answer `showModal()` with the same element a browser would have focused.
 */
export const FOCUSABLE_SELECTOR =
  'a[href], area, button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), iframe, object, embed, [tabindex]:not([tabindex="-1"]), [contenteditable="true"]'

function focusableWithin(root: HTMLDialogElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR))
}

/**
 * Open a `<dialog>` as a modal, hold the keyboard inside it, and hand the focus back when it goes.
 *
 * The component that calls this renders a `<dialog>` that is mounted only while it is open: `onClose`
 * is how it is told to go away, and unmounting is what returns the focus. Escape is intercepted
 * before the browser sees it - otherwise it reaches whatever is behind the dialog too - and is
 * reported as `onClose` and nothing else, so a dialog that closes on Escape has asked the server
 * nothing by construction. A component that cannot be interrupted mid-request guards its own
 * `onClose`, which is why Escape and the close button go quiet together with Cancel.
 *
 * @param onClose What to do when the person asks out: Escape, or the component's own close control.
 * @param initialFocus Selector for the control to start in. Without it, and when nothing matches it,
 *   the focus stays where `showModal()` put it: the first control in the dialog.
 */
export function useModalDialog(onClose: () => void, initialFocus?: string): ModalDialog {
  const ref = useRef<HTMLDialogElement>(null)
  // The latest `onClose`, read at Escape time rather than baked into the effects below: a dialog
  // whose parent re-renders with a new callback must not reopen, refocus, or close itself.
  const onCloseRef = useRef(onClose)

  useEffect(() => {
    onCloseRef.current = onClose
  }, [onClose])

  useLayoutEffect(() => {
    const root = ref.current
    if (root === null) return
    // The line above `showModal()`, not below it: `showModal` has already taken the focus by then,
    // and what it took it from is the thing this dialog owes the focus back to.
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null
    root.showModal()
    const target = initialFocus === undefined ? null : root.querySelector<HTMLElement>(initialFocus)
    if (target !== null) target.focus()

    return () => {
      // jsdom has no `close` (see `dialog-shim`), and a dialog dropped from the tree is closed by
      // the removal itself, so both cases are tolerated here rather than in the app source.
      if (typeof root.close === 'function') root.close()
      // Never hand the focus to a control of this dialog's own: it went away with the dialog, and
      // focusing a detached node is how the page ends up with the focus on nothing.
      if (opener !== null && opener.isConnected && !root.contains(opener)) opener.focus()
    }
  }, [initialFocus])

  const onKeyDown = useCallback((event: KeyboardEvent<HTMLDialogElement>) => {
    const root = ref.current
    if (root === null) return
    if (event.key === 'Escape') {
      event.preventDefault()
      onCloseRef.current()
      return
    }
    if (event.key !== 'Tab') return

    const items = focusableWithin(root)
    if (items.length === 0) {
      event.preventDefault()
      return
    }
    const first = items[0] as HTMLElement
    const last = items[items.length - 1] as HTMLElement
    const active = document.activeElement

    if (!root.contains(active) || active === root) {
      // Focus left (or the dialog itself holds it, which is where a browser starts a modal): put it
      // where Tab was heading instead of letting the keyboard out into the table.
      event.preventDefault()
      ;(event.shiftKey ? last : first).focus()
      return
    }
    if (event.shiftKey ? active === first : active === last) {
      event.preventDefault()
      ;(event.shiftKey ? last : first).focus()
    }
  }, [])

  return { ref, onKeyDown }
}
