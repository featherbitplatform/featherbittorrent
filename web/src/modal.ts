/**
 * The two things a `<dialog>` does not do by itself: keep the keyboard inside itself, and give the
 * focus back when it goes away.
 *
 * `showModal()` puts the element in the top layer and blocks the page behind it, but Tab still walks
 * out of it into the table underneath, and a dialog that is unmounted rather than closed - which is
 * how a screen closes one, by dropping it from the tree - never returns the focus to the button that
 * opened it. This hook does both, so the Add dialog and the remove dialog are the same behaviour
 * rather than two copies of it, and each dialog's own tests are about the dialog and not about the
 * trap.
 *
 * Opening happens in a layout effect and the rest in a passive one, and the split is the point. The
 * layout effect is what keeps a dialog from painting shut for one frame; the opener, by contrast,
 * has to be read *after* everything that mounts the dialog has had its say about focus - a screen
 * that focuses the row it just acted on, or a test harness that focuses the button that opened the
 * dialog, is still running its own layout effect when this component's has finished. Capture the
 * opener any earlier and the focus is handed back to whatever held it before the dialog was even
 * mounted.
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

/** Everything a Tab key can land on, in the order the keyboard reaches them. */
const FOCUSABLE =
  'a[href], area, button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), iframe, object, embed, [tabindex]:not([tabindex="-1"]), [contenteditable="true"]'

function focusableWithin(root: HTMLDialogElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE))
}

/**
 * Open a `<dialog>` as a modal, hold the keyboard inside it, and hand the focus back when it goes.
 *
 * The component that calls this renders a `<dialog>` that is mounted only while it is open: `onClose`
 * is how it is told to go away, and unmounting is what returns the focus. Escape is intercepted
 * before the browser sees it - otherwise it reaches whatever is behind the dialog too - and is
 * reported as `onClose` and nothing else, so a dialog that closes on Escape has asked the server
 * nothing by construction.
 *
 * @param onClose What to do when the person asks out: Escape, or the component's own close control.
 * @param initialFocus Selector for the control to start in. Without it the first control in the dialog
 *   gets the focus.
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
    ref.current?.showModal()
  }, [])

  useEffect(() => {
    const root = ref.current
    if (root === null) return
    const opener = document.activeElement
    const target =
      (initialFocus === undefined ? null : root.querySelector<HTMLElement>(initialFocus)) ??
      focusableWithin(root)[0] ??
      root
    target.focus()

    return () => {
      // jsdom has no `close` (see `dialog-shim`), and a dialog dropped from the tree after Escape is
      // already closed, so both cases are tolerated here rather than in the app source.
      if (typeof root.close === 'function') root.close()
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus()
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
