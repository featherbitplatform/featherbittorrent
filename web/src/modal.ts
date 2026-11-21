/**
 * The three things a `<dialog>` needs a hook for here: keep the keyboard inside it, give the focus
 * back when it goes, and keep the element and the screen's idea of "open" in step with each other.
 *
 * `showModal()` puts the element in the top layer, blocks the page behind it and moves the focus
 * inside - a browser takes it from whatever held it - but Tab still walks out of the dialog into the
 * table underneath, and a dialog that is unmounted rather than closed, which is how a screen closes
 * one, never hands the focus back on its own.
 *
 * The third job is the one a key handler cannot do. Escape is the browser's close request, not a
 * key the page is handed: Chrome answers it with the dialog's own `cancel` event and closes the
 * element itself unless that event is prevented (its guidance says outright not to write an Esc
 * handler for a modal). A page that watches only keydown and never listens to `cancel` or `close`
 * lets the element close under it while React still thinks it is open - and the next click on the
 * opener then sets the same state, nothing re-runs `showModal()`, and a mounted-but-closed dialog
 * never opens again. So this hook listens to the dialog's own events instead: `cancel` is prevented
 * and reported as `onClose`, so the owner's state - not the browser - is what ends the dialog and
 * element and state go away through the one channel; and `close` is reported as `onClose` too, so
 * even a close that never went through the state leaves no stale "open" behind. A component that
 * cannot be interrupted mid-request has a guarded `onClose`: preventing the `cancel` event is what
 * actually keeps the dialog open on Escape while its request is on the wire.
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
 * Open a `<dialog>` as a modal, hold the keyboard inside it, and close it - however it was asked
 * out - through the owner's `onClose`.
 *
 * The component that calls this renders a `<dialog>` that is mounted only while it is open: `onClose`
 * is how it is told to go away, and unmounting is what returns the focus. Every close request -
 * Escape, or anything else the browser offers - arrives as the element's `cancel` event, is prevented
 * here, and is reported as `onClose` and nothing else, so a dialog that closes on Escape has asked
 * the server nothing by construction and the screen's state ends it like every other path does. The
 * element's `close` event is reported the same way, as insurance for the case where the element
 * closed without the state moving: an "open" that outlives its dialog is one that cannot be reopened.
 *
 * @param onClose What to do when the person asks out: Escape, the close control, Cancel, or the trip
 *   home after a request. A guarded one (busy means "not yet") is what makes Escape go quiet.
 * @param initialFocus Selector for the control to start in. Without it, and when nothing matches it,
 *   the focus stays where `showModal()` put it: the first control in the dialog.
 */
export function useModalDialog(onClose: () => void, initialFocus?: string): ModalDialog {
  const ref = useRef<HTMLDialogElement>(null)
  // The latest `onClose`, read at close-request time rather than baked into the effects below: a
  // dialog whose parent re-renders with a new callback must not reopen, refocus, or close itself.
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

    // Escape (and every other close request the browser offers) arrives here, as the element's own
    // event. Preventing it is what stops the element closing itself; the owner's `onClose` is what
    // actually ends the dialog, exactly as it does for the buttons. A guarded `onClose` doing
    // nothing is the "cannot be interrupted" case, and the prevention keeps it open regardless.
    const answerCancel = (event: Event): void => {
      event.preventDefault()
      onCloseRef.current()
    }
    // A close that did not come through the state - the element closed under us somehow - is
    // reported through the same channel, because a state that outlives its element is the one bug
    // that makes a dialog impossible to reopen.
    const reportClose = (): void => {
      onCloseRef.current()
    }
    root.addEventListener('cancel', answerCancel)
    root.addEventListener('close', reportClose)

    return () => {
      // The listeners go first: the `close` this very cleanup causes by closing the element is the
      // screen putting the dialog away on purpose, and it must not re-enter `onClose` a second time.
      root.removeEventListener('cancel', answerCancel)
      root.removeEventListener('close', reportClose)
      // jsdom has no `close` (see `dialog-shim`), and a dialog dropped from the tree is closed by
      // the removal itself, so both cases are tolerated here rather than in the app source.
      if (typeof root.close === 'function') root.close()
      // Never hand the focus to a control of this dialog's own: it went away with the dialog, and
      // focusing a detached node is how the page ends up with the focus on nothing.
      if (opener !== null && opener.isConnected && !root.contains(opener)) opener.focus()
    }
  }, [initialFocus])

  // No Escape branch, on purpose: a modal's Esc is the browser's close request and surfaces as the
  // element's `cancel` event, which the layout effect above answers. What is left for the keyboard
  // is the trap, which is the one behaviour a native modal really does not give you.
  const onKeyDown = useCallback((event: KeyboardEvent<HTMLDialogElement>) => {
    const root = ref.current
    if (root === null) return
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
