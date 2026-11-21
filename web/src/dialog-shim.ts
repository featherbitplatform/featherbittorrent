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
 * @module dialog-shim
 */

/**
 * Gives every `HTMLDialogElement` in the environment `showModal` and `close`, opening and shutting the
 * `open` attribute the way the browser does. Idempotent, so a test file can install it at the top
 * without caring who else installed it.
 */
export function installDialogShim(): void {
  const proto = window.HTMLDialogElement?.prototype
  if (!proto) return

  if (!proto.showModal) {
    Object.defineProperty(proto, 'showModal', {
      configurable: true,
      writable: true,
      value: function showModal(): void {
        this.setAttribute('open', '')
      },
    })
  }

  if (!proto.close) {
    Object.defineProperty(proto, 'close', {
      configurable: true,
      writable: true,
      value: function close(): void {
        this.removeAttribute('open')
      },
    })
  }
}
