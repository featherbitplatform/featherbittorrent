/**
 * The two methods a jsdom `<dialog>` does not have, for the tests that drive a real dialog.
 *
 * jsdom 31 builds a `<dialog>`, keeps its `open` attribute and hides a closed one from the
 * accessibility tree, but it never implements `showModal` or `close`. A component that calls the real
 * methods - which is the only thing a browser gives a dialog, and what `modal.ts` does - would
 * therefore sit shut in front of every test. This installs the two missing pieces and nothing else, so
 * the component under test is written exactly as it is written for the browser. The dialog's own
 * meaning stays the component's job: the two buttons that answer the question, Escape, the fields a
 * refusal has to leave where they were.
 *
 * `showModal` moves the focus, and that part matters more than it looks: it is the whole reason a
 * dialog has to read the control that opened it *before* calling it, so a stub that left the focus
 * where it was would let a component pass every test with an opener it can never hand the focus back
 * to. This stub takes the focus to the first control inside the dialog, the way the browser does.
 *
 * @module dialog-shim
 */
import { FOCUSABLE_SELECTOR } from './modal'

/**
 * Gives every `HTMLDialogElement` in the environment `showModal` and `close`: opening sets `open` and
 * takes the focus to the first control inside it, shutting clears `open`. Idempotent, so a test file
 * can install it at the top without caring who else installed it.
 */
export function installDialogShim(): void {
  const proto = window.HTMLDialogElement?.prototype
  if (!proto) return

  if (!proto.showModal) {
    Object.defineProperty(proto, 'showModal', {
      configurable: true,
      writable: true,
      value: function showModal(this: HTMLDialogElement): void {
        this.setAttribute('open', '')
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
        this.removeAttribute('open')
      },
    })
  }
}
