/**
 * The question the app asks the first time its window is closed: quit, or keep running in the tray.
 *
 * The shell asks it only when there is a tray (without one, closing quits and there is nothing to
 * choose), by sending `close-requested` instead of closing. The answer is saved through the shell
 * first and then applied by asking the shell to close the window again, which now follows the saved
 * choice; a save the shell refuses keeps the window and says why. Escape and the close X leave the
 * window open and save nothing, so the question comes back on the next close.
 *
 * @module desktop/CloseChoice
 */
import { useEffect, useRef, useState } from 'react'
import { X } from 'lucide-react'
import Banner from '../components/Banner'
import Button from '../components/Button'
import { PHONE_FULLSCREEN, useModalDialog } from '../modal'
import { closeWindow, onCloseRequested, setCloseAction } from './bridge'
import type { CloseAction } from './bridge'

/** The shell's refusal, as one line: Tauri rejects a command with its error string. */
function refusal(cause: unknown): string {
  if (typeof cause === 'string') return cause
  if (cause instanceof Error) return cause.message
  return 'The choice could not be saved.'
}

export interface CloseChoiceProps {
  /** Put the question away: after an answer was applied, or when the person walked away from it. */
  onClose(): void
}

/** Asks what closing the window should do, saves the answer and applies it. */
export function CloseChoice({ onClose }: CloseChoiceProps) {
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const submittingRef = useRef(false)

  function requestClose(): void {
    if (submittingRef.current) return
    onClose()
  }

  const { ref, onKeyDown } = useModalDialog(requestClose, '[data-default]')

  async function choose(action: CloseAction): Promise<void> {
    if (submittingRef.current) return
    submittingRef.current = true
    setBusy(true)
    setError(null)
    try {
      await setCloseAction(action)
      await closeWindow()
    } catch (cause) {
      setError(refusal(cause))
      submittingRef.current = false
      setBusy(false)
      return
    }
    submittingRef.current = false
    onClose()
  }

  return (
    <dialog
      ref={ref}
      onKeyDown={onKeyDown}
      aria-labelledby="close-choice-title"
      aria-describedby="close-choice-note"
      className={`w-[min(460px,calc(100vw-32px))] rounded-overlay border border-rule bg-raised text-body shadow-[0_24px_64px_rgba(0,0,0,0.5)] backdrop:bg-canvas/72 ${PHONE_FULLSCREEN}`}
    >
      <div className="flex items-start justify-between gap-3 border-b border-ink-700 px-5.5 py-4.5">
        <h2 id="close-choice-title" className="text-[16px] font-semibold text-strong">
          What should closing the window do?
        </h2>
        <button
          type="button"
          aria-label="Close"
          disabled={busy}
          onClick={requestClose}
          className="-mr-2 flex h-9 w-9 flex-none cursor-pointer items-center justify-center rounded-control text-muted hover:bg-ink-700 hover:text-strong disabled:cursor-default disabled:opacity-55"
        >
          <X className="h-4.5 w-4.5" aria-hidden="true" />
        </button>
      </div>

      <div className="flex flex-col gap-2 px-5.5 py-5">
        <p id="close-choice-note" className="text-[13px]">
          You can change this later in Settings.
        </p>
        {error !== null && <Banner tone="danger">{error}</Banner>}
      </div>

      <div className="flex flex-wrap items-center justify-end gap-2.5 border-t border-ink-700 px-5.5 py-4">
        <Button
          variant="secondary"
          disabled={busy}
          onClick={() => {
            void choose('quit')
          }}
        >
          Close completely
        </Button>
        <Button
          data-default
          disabled={busy}
          onClick={() => {
            void choose('tray')
          }}
        >
          Keep running in the tray
        </Button>
      </div>
    </dialog>
  )
}

/** Listens for the shell's `close-requested` and shows {@link CloseChoice} while it is being answered. */
export function CloseChoiceHost() {
  const [asking, setAsking] = useState(false)

  useEffect(() => {
    let stop: (() => void) | null = null
    let gone = false
    onCloseRequested(() => setAsking(true)).then(
      (unlisten) => {
        if (gone) unlisten()
        else stop = unlisten
      },
      () => {
        // No event channel means the shell never asks; closing then follows the saved choice alone.
      },
    )
    return () => {
      gone = true
      stop?.()
    }
  }, [])

  return asking ? <CloseChoice onClose={() => setAsking(false)} /> : null
}
