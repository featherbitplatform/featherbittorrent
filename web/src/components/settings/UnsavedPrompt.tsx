/**
 * The question asked before leaving a Settings section that holds changes: keep them or drop them.
 *
 * Escape closes the question and stays where the person was; while a save is on the wire it cannot
 * be closed.
 *
 * @module components/settings/UnsavedPrompt
 */
import { MODAL_DIALOG, useModalDialog } from '../../modal'
import Button from '../Button'

export interface UnsavedPromptProps {
  /** The title of the section with changes. */
  section: string
  busy: boolean
  /** Escape, or the stay-here answer. */
  onStay(): void
  onSave(): void
  onDiscard(): void
}

export default function UnsavedPrompt({ section, busy, onStay, onSave, onDiscard }: UnsavedPromptProps) {
  // Escape stays, except while a save is on the wire.
  const { ref, onKeyDown } = useModalDialog(() => {
    if (!busy) onStay()
  }, '[data-save]')
  return (
    <dialog
      ref={ref}
      onKeyDown={onKeyDown}
      aria-labelledby="unsaved-title"
      className={`w-[min(460px,calc(100vw-32px))] rounded-overlay border border-rule bg-raised text-body shadow-[0_24px_64px_rgba(0,0,0,0.5)] backdrop:bg-canvas/72 ${MODAL_DIALOG}`}
    >
      <div className="px-5.5 py-5">
        <p id="unsaved-title" className="text-[15px] font-semibold text-strong">
          You have unsaved changes in {section}.
        </p>
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2.5 border-t border-ink-700 px-5.5 py-4">
        <Button data-save disabled={busy} onClick={onSave}>
          Save changes
        </Button>
        <Button variant="secondary" disabled={busy} onClick={onDiscard}>
          Discard changes
        </Button>
      </div>
    </dialog>
  )
}
