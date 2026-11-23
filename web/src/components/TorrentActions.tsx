/**
 * The `More` menu for one torrent: queue moves, recheck, reannounce, limits and move.
 *
 * It is self-contained on purpose. The desktop toolbar mounts it beside Pause and Remove, and the
 * phone view mounts it again from the same props, so nothing here reads the screen around it: the
 * torrent, the client and one `onError` for a refusal are the whole contract.
 *
 * Queue moves are absolute positions computed from the torrent's own `queue_position`; the server
 * clamps, so "bottom" is the largest position it accepts. They exist only for a torrent the queue
 * manages. A move of the files never overwrites (the client always asks `fail_if_exists`), and a
 * refusal of any action goes to `onError` as the server's own words.
 *
 * @module components/TorrentActions
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import { MoreHorizontal } from 'lucide-react'
import { errorMessage } from '../api/client'
import type { Api, TorrentInfo } from '../api/client'
import { useModalDialog } from '../modal'
import Button from './Button'
import FolderPicker from './FolderPicker'
import TorrentLimits from './TorrentLimits'

const QUEUE_BOTTOM = 2147483647

export interface TorrentActionsProps {
  torrent: TorrentInfo
  client: Api
  onError(message: string): void
}

const ITEM =
  'flex min-h-11 w-full cursor-pointer items-center px-3 text-left text-[13px] text-body hover:bg-ink-700 hover:text-strong md:min-h-9'

function MoveDialog({
  client,
  onCancel,
  onMove,
}: {
  client: Api
  onCancel(): void
  onMove(path: string): void
}) {
  const [path, setPath] = useState('')
  const onPathChange = useCallback((next: string) => {
    setPath(next)
  }, [])
  const { ref, onKeyDown } = useModalDialog(onCancel, '[data-cancel]')
  return (
    <dialog
      ref={ref}
      onKeyDown={onKeyDown}
      aria-labelledby="move-files-title"
      className="w-[min(460px,calc(100vw-32px))] rounded-overlay border border-rule bg-raised text-body shadow-[0_24px_64px_rgba(0,0,0,0.5)] backdrop:bg-canvas/72"
    >
      <div className="border-b border-ink-700 px-5.5 py-4.5">
        <h2 id="move-files-title" className="text-[16px] font-semibold text-strong">
          Move files
        </h2>
      </div>
      <div className="px-5.5 py-5">
        <FolderPicker client={client} value={path} onPathChange={onPathChange} />
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2.5 border-t border-ink-700 px-5.5 py-4">
        <Button data-cancel variant="secondary" onClick={onCancel}>
          Cancel
        </Button>
        <Button
          disabled={path === ''}
          onClick={() => {
            onMove(path)
          }}
        >
          Move
        </Button>
      </div>
    </dialog>
  )
}

export function TorrentActions({ torrent, client, onError }: TorrentActionsProps) {
  const [menuOpen, setMenuOpen] = useState(false)
  const [limitsOpen, setLimitsOpen] = useState(false)
  const [moveOpen, setMoveOpen] = useState(false)
  const [moving, setMoving] = useState(false)
  const rootRef = useRef<HTMLDivElement>(null)
  const id = torrent.id
  const queued = torrent.auto_managed && torrent.queue_position >= 0

  useEffect(() => {
    if (!menuOpen) return
    function away(event: MouseEvent): void {
      if (rootRef.current !== null && !rootRef.current.contains(event.target as Node)) {
        setMenuOpen(false)
      }
    }
    document.addEventListener('mousedown', away)
    return () => {
      document.removeEventListener('mousedown', away)
    }
  }, [menuOpen])

  async function run(call: () => Promise<void>): Promise<void> {
    setMenuOpen(false)
    try {
      await call()
    } catch (cause) {
      onError(errorMessage(cause))
    }
  }

  async function move(path: string): Promise<void> {
    setMoveOpen(false)
    setMoving(true)
    try {
      await client.moveStorage(id, path)
    } catch (cause) {
      onError(errorMessage(cause))
    } finally {
      setMoving(false)
    }
  }

  const item = (label: string, onClick: () => void) => (
    <button key={label} type="button" role="menuitem" className={ITEM} onClick={onClick}>
      {label}
    </button>
  )

  return (
    <div
      ref={rootRef}
      className="relative"
      onKeyDown={(event) => {
        if (event.key === 'Escape' && menuOpen) setMenuOpen(false)
      }}
    >
      <Button
        size="sm"
        variant="secondary"
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onClick={() => {
          setMenuOpen((open) => !open)
        }}
      >
        <MoreHorizontal className="h-4 w-4" aria-hidden="true" />
        More
      </Button>
      {moving && <span className="ml-2 text-[12px] text-muted">Moving...</span>}
      {menuOpen && (
        <div
          role="menu"
          className="absolute right-0 z-20 mt-1 flex min-w-44 flex-col rounded-card border border-rule bg-raised py-1 shadow-[0_12px_32px_rgba(0,0,0,0.45)]"
        >
          {queued && (
            <>
              {item('Move up', () => void run(() => client.setQueuePosition(id, torrent.queue_position - 1)))}
              {item('Move down', () => void run(() => client.setQueuePosition(id, torrent.queue_position + 1)))}
              {item('Move to top', () => void run(() => client.setQueuePosition(id, 0)))}
              {item('Move to bottom', () => void run(() => client.setQueuePosition(id, QUEUE_BOTTOM)))}
              <div role="separator" className="my-1 border-t border-ink-700" />
            </>
          )}
          {item('Recheck', () => void run(() => client.recheck(id)))}
          {item('Reannounce', () => void run(() => client.reannounce(id)))}
          {item('Limits...', () => {
            setMenuOpen(false)
            setLimitsOpen(true)
          })}
          {item('Move files...', () => {
            setMenuOpen(false)
            setMoveOpen(true)
          })}
        </div>
      )}
      {limitsOpen && (
        <TorrentLimits
          id={id}
          client={client}
          onClose={() => {
            setLimitsOpen(false)
          }}
        />
      )}
      {moveOpen && (
        <MoveDialog
          client={client}
          onCancel={() => {
            setMoveOpen(false)
          }}
          onMove={(path) => {
            void move(path)
          }}
        />
      )}
    </div>
  )
}

export default TorrentActions
