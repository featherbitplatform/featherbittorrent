/**
 * A torrent full screen, for a phone: Back, the actions at the top, then the detail tabs.
 *
 * It is a fixed overlay over the list (not a native dialog, so `ConfirmDelete` and the actions' own
 * dialogs can open above it). The screen owns the selection, the history entry and the action
 * calls; this component only lays them out. Every tap target is at least 44 px: the buttons carry
 * `min-h-11`, and the detail pane is asked for its full-screen sizing.
 *
 * @module components/TorrentFullView
 */
import { ArrowLeft } from 'lucide-react'
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
  onRemoved,
}: TorrentFullViewProps) {
  const paused = torrent.rowState === 'paused'
  return (
    <div role="dialog" aria-label="Torrent" className="fixed inset-0 z-30 flex flex-col bg-canvas">
      <div className="flex shrink-0 items-center gap-2 border-b border-rule px-2 py-1">
        <button
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
        />
      </div>
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
