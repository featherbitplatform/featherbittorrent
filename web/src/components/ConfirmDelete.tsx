/**
 * The dialog that asks *how* a torrent is removed.
 *
 * Removal means two different things and the server will not guess which one was meant: the files
 * stay, or the files go with the torrent. So the dialog is the choice, spelled as three buttons that
 * say what they do, and the only thing that decides which request leaves the page is which one was
 * pressed. Escape and Cancel are the third answer, the one that sends nothing at all: the torrent is
 * still in the list when the dialog goes away and the page never spoke to the server about it.
 *
 * It makes the removal itself, through the same client the screen reads with, because a refusal is
 * worth showing where the question was asked: a `torrent_not_found` that closed the dialog would read
 * as a removal that worked. Only a 204 closes it, and it closes by being dropped from the tree -
 * `useModalDialog` hands the focus back to the Remove button that opened it on the way out.
 *
 * @module components/ConfirmDelete
 */
import { useState } from 'react'
import { X } from 'lucide-react'
import { errorMessage } from '../api/client'
import type { Api } from '../api/client'
import { displaySafe } from '../format'
import { announcePolite } from '../live'
import { useModalDialog } from '../modal'
import Banner from './Banner'
import Button from './Button'

/** The sentence the page speaks through its polite region when a torrent is gone: the removal is done,
 * and where the toolbar was acting on it there is nothing left to act on. */
const REMOVED_COPY = 'Torrent removed.'

export interface ConfirmDeleteProps {
  /** The torrent to remove, addressed the way every other call addresses it. */
  id: string
  /** Its name, quoted back so the person knows which torrent this answer is about. */
  name: string
  client: Api
  /** Close it without answering: Cancel, Escape, and the trip home after a removal. */
  onClose(): void
  /** Says which torrent went away, after the server agreed to let it go. */
  onRemoved(goneId: string): void
}

/**
 * Ask which of the two removals is meant. `onRemoved` fires once, on a 204, with the id that is gone;
 * every refusal leaves the dialog open with the server's reason inside it.
 */
export default function ConfirmDelete({ id, name, client, onClose, onRemoved }: ConfirmDeleteProps) {
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const { ref, onKeyDown } = useModalDialog(onClose, '[data-cancel]')

  async function remove(deleteFiles: boolean): Promise<void> {
    if (busy) return
    setBusy(true)
    setError(null)
    try {
      await client.remove(id, deleteFiles)
    } catch (cause) {
      // The dialog keeps the question and answers it with the server's own words, because "the
      // dialog closed" and "the torrent is gone" are not the same claim.
      setError(errorMessage(cause))
      setBusy(false)
      return
    }
    announcePolite(REMOVED_COPY)
    onRemoved(id)
    onClose()
  }

  return (
    <dialog
      ref={ref}
      onKeyDown={onKeyDown}
      aria-labelledby="confirm-delete-title"
      className="w-[min(460px,calc(100vw-32px))] rounded-overlay border border-rule bg-raised text-body shadow-[0_24px_64px_rgba(0,0,0,0.5)] backdrop:bg-canvas/72"
    >
      <div className="flex items-start justify-between gap-3 border-b border-ink-700 px-5.5 py-4.5">
        <h2 id="confirm-delete-title" className="text-[16px] font-semibold text-strong">
          Remove torrent
        </h2>
        <button
          type="button"
          aria-label="Close"
          onClick={onClose}
          className="-mr-2 flex h-9 w-9 flex-none cursor-pointer items-center justify-center rounded-control text-muted hover:bg-ink-700 hover:text-strong"
        >
          <X className="h-4.5 w-4.5" aria-hidden="true" />
        </button>
      </div>

      <div className="flex flex-col gap-2 px-5.5 py-5">
        <p className="break-all font-mono text-[13px] text-strong">{displaySafe(name)}</p>
        <p className="text-[13px]">
          The torrent leaves the list and the download stops with it. The only question is what
          happens to the files it wrote.
        </p>
        {error !== null && <Banner tone="danger">{error}</Banner>}
      </div>

      <div className="flex flex-wrap items-center justify-end gap-2.5 border-t border-ink-700 px-5.5 py-4">
        <Button data-cancel variant="secondary" onClick={onClose} disabled={busy}>
          Cancel
        </Button>
        <Button
          variant="secondary"
          disabled={busy}
          onClick={() => {
            void remove(false)
          }}
        >
          Remove and keep files
        </Button>
        <Button
          variant="danger"
          disabled={busy}
          onClick={() => {
            void remove(true)
          }}
        >
          Remove and delete files
        </Button>
      </div>
    </dialog>
  )
}
