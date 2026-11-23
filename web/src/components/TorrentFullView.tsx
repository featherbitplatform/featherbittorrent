/**
 * A torrent full screen, for a phone: Back, the actions at the top, then the detail tabs.
 *
 * It is a fixed overlay over the list (not a native dialog, so `ConfirmDelete` and the actions' own
 * dialogs can open above it). The screen owns the selection, the history entry and the action
 * calls; this component only lays them out. Every tap target is at least 44 px: the buttons carry
 * `min-h-11`, and the detail pane is asked for its full-screen sizing.
 *
 * It is modal all the same: `aria-modal`, the screen makes the page behind it `inert`, the focus
 * starts on Back when it opens, and the screen hands it back to the card that opened it on close.
 * An action's refusal is drawn here, under the action row, because the screen's own banner is
 * behind this overlay where nobody can see it.
 *
 * @module components/TorrentFullView
 */
import { useEffect, useRef } from 'react'
import { ArrowLeft } from 'lucide-react'
import Banner from './Banner'
import Button from './Button'
import DetailPane from './DetailPane'
import TorrentActions from './TorrentActions'
import type { Api } from '../api/client'
import { displaySafe } from '../format'
import type { TorrentRow } from '../live'

export interface TorrentFullViewProps {
  torrent: TorrentRow
  client: Api
  tick: number
  /** An action is on the wire: Pause, Resume and Remove go quiet. */
  busy: boolean
  /** The Back button: the screen goes through `history.back()`. */
  onBack(): void
  onAct(kind: 'pause' | 'resume'): void
  onRemove(): void
  moving: boolean
  onMoving(on: boolean): void
  onError(why: string): void
  /** An action the menu sent was accepted. */
  onDone(): void
  /** The latest action's refusal, or null. */
  error: string | null
  /** The pane closed itself over a 404. */
  onRemoved(id: string): void
}

export default function TorrentFullView({
  torrent,
  client,
  tick,
  busy,
  onBack,
  onAct,
  onRemove,
  moving,
  onMoving,
  onError,
  onDone,
  error,
  onRemoved,
}: TorrentFullViewProps) {
  const paused = torrent.rowState === 'paused'
  const backRef = useRef<HTMLButtonElement>(null)
  // Opening moves the focus into the view, onto the way out of it.
  useEffect(() => {
    backRef.current?.focus()
  }, [])
  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Torrent"
      className="fixed inset-0 z-30 flex flex-col bg-canvas"
    >
      <div className="flex shrink-0 items-center gap-2 border-b border-rule px-2 py-1">
        <button
          ref={backRef}
          type="button"
          aria-label="Back"
          onClick={onBack}
          className="flex h-11 w-11 shrink-0 cursor-pointer items-center justify-center rounded-control text-body hover:bg-ink-700"
        >
          <ArrowLeft className="h-5 w-5" aria-hidden="true" />
        </button>
        <span className="min-w-0 flex-1 truncate text-[14px] text-strong">
          {displaySafe(torrent.name) || displaySafe(torrent.id)}
        </span>
      </div>
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-rule px-3 py-2 [&_button]:min-h-11">
        <Button
          variant="secondary"
          disabled={paused || busy}
          onClick={() => {
            onAct('pause')
          }}
        >
          Pause
        </Button>
        <Button
          variant="secondary"
          disabled={!paused || busy}
          onClick={() => {
            onAct('resume')
          }}
        >
          Resume
        </Button>
        <Button variant="danger" disabled={busy} onClick={onRemove}>
          Remove
        </Button>
        <TorrentActions
          key={torrent.id}
          torrent={torrent}
          client={client}
          moving={moving}
          onMoving={onMoving}
          onError={onError}
          onDone={onDone}
        />
      </div>
      {error !== null && (
        <div className="shrink-0 px-3 pt-2">
          <Banner tone="danger">{error}</Banner>
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-col">
        <DetailPane
          id={torrent.id}
          client={client}
          tick={tick}
          name={torrent.name}
          fullScreen
          onDismiss={onBack}
          onRemoved={onRemoved}
        />
      </div>
    </div>
  )
}
