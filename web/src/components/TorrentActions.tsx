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
import type { KeyboardEvent } from 'react'
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
  /** Whether a move of this torrent's files is on the wire, when the screen keeps that across selections; absent, the component tracks its own. */
  moving?: boolean
  /** Told when a move starts and when it settles, so the screen can keep the indicator while another torrent is selected. */
  onMoving?(moving: boolean): void
}

const ITEM =
  'flex min-h-11 w-full cursor-pointer items-center px-3 text-left text-[13px] text-body hover:bg-ink-700 hover:text-strong md:min-h-9'

function MenuItem({
  label,
  onClick,
  disabled = false,
}: {
  label: string
  onClick(): void
  disabled?: boolean
}) {
  return (
    <button
      type="button"
      role="menuitem"
      disabled={disabled}
      className={ITEM + ' disabled:cursor-default disabled:opacity-55'}
      onClick={onClick}
    >
      {label}
    </button>
  )
}

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

export function TorrentActions({
  torrent,
  client,
  onError,
  moving: movingProp,
  onMoving,
}: TorrentActionsProps) {
  const [menuOpen, setMenuOpen] = useState(false)
  const [limitsOpen, setLimitsOpen] = useState(false)
  const [moveOpen, setMoveOpen] = useState(false)
  const [localMoving, setMoving] = useState(false)
  const moving = movingProp ?? localMoving
  const menuRef = useRef<HTMLDivElement>(null)
  const dialogWasOpen = useRef(false)
  const rootRef = useRef<HTMLDivElement>(null)
  const id = torrent.id
  // -1 would mean "not in the queue" (libtorrent's convention); the server sends a place from 0, so this only guards the odd one out.
  const queued = torrent.auto_managed && torrent.queue_position >= 0
  const atTop = torrent.queue_position === 0

  useEffect(() => {
    if (!menuOpen) return
    function away(event: MouseEvent): void {
      if (rootRef.current !== null && !rootRef.current.contains(event.target as Node)) {
        setMenuOpen(false)
      }
    }
    document.addEventListener('pointerdown', away)
    return () => {
      document.removeEventListener('pointerdown', away)
    }
  }, [menuOpen])

  /** The More button, found from the wrapper: the shared Button takes no ref. */
  function moreButton(): HTMLElement | null {
    return rootRef.current?.querySelector<HTMLElement>('[aria-haspopup="menu"]') ?? null
  }

  // The first item takes the focus when the menu opens.
  useEffect(() => {
    if (menuOpen)
      menuRef.current?.querySelector<HTMLElement>('[role="menuitem"]:not(:disabled)')?.focus()
  }, [menuOpen])

  // When a dialog opened from the menu goes away its opener (a menu item) is gone: the focus goes to More.
  useEffect(() => {
    if (limitsOpen || moveOpen) {
      dialogWasOpen.current = true
    } else if (dialogWasOpen.current) {
      dialogWasOpen.current = false
      moreButton()?.focus()
    }
  }, [limitsOpen, moveOpen])

  function closeMenu(): void {
    setMenuOpen(false)
    moreButton()?.focus()
  }

  function onMenuKeyDown(event: KeyboardEvent<HTMLDivElement>): void {
    const items = Array.from(
      menuRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]:not(:disabled)') ?? [],
    )
    const at = items.indexOf(document.activeElement as HTMLElement)
    let next: number | null = null
    if (event.key === 'ArrowDown') next = (at + 1) % items.length
    else if (event.key === 'ArrowUp') next = (at <= 0 ? items.length : at) - 1
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = items.length - 1
    if (next === null) return
    event.preventDefault()
    items[next]?.focus()
  }

  async function run(call: () => Promise<void>): Promise<void> {
    closeMenu()
    try {
      await call()
    } catch (cause) {
      onError(errorMessage(cause))
    }
  }

  async function move(path: string): Promise<void> {
    setMoveOpen(false)
    setMoving(true)
    onMoving?.(true)
    try {
      await client.moveStorage(id, path)
    } catch (cause) {
      onError(errorMessage(cause))
    } finally {
      setMoving(false)
      onMoving?.(false)
    }
  }

  return (
    <div
      ref={rootRef}
      className="relative"
      onKeyDown={(event) => {
        if (event.key === 'Escape' && menuOpen) {
          event.stopPropagation()
          closeMenu()
        }
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
          ref={menuRef}
          role="menu"
          onKeyDown={onMenuKeyDown}
          className="absolute right-0 z-20 mt-1 flex min-w-44 flex-col rounded-card border border-rule bg-raised py-1 shadow-[0_12px_32px_rgba(0,0,0,0.45)]"
        >
          {queued && (
            <>
              <MenuItem
                label="Move up"
                disabled={atTop}
                onClick={() =>
                  void run(() => client.setQueuePosition(id, torrent.queue_position - 1))
                }
              />
              <MenuItem
                label="Move down"
                onClick={() =>
                  void run(() => client.setQueuePosition(id, torrent.queue_position + 1))
                }
              />
              <MenuItem
                label="Move to top"
                disabled={atTop}
                onClick={() => void run(() => client.setQueuePosition(id, 0))}
              />
              <MenuItem
                label="Move to bottom"
                onClick={() => void run(() => client.setQueuePosition(id, QUEUE_BOTTOM))}
              />
              <div role="separator" className="my-1 border-t border-ink-700" />
            </>
          )}
          <MenuItem label="Recheck" onClick={() => void run(() => client.recheck(id))} />
          <MenuItem label="Reannounce" onClick={() => void run(() => client.reannounce(id))} />
          <MenuItem
            label="Limits..."
            onClick={() => {
              setMenuOpen(false)
              setLimitsOpen(true)
            }}
          />
          <MenuItem
            label="Move files..."
            onClick={() => {
              setMenuOpen(false)
              setMoveOpen(true)
            }}
          />
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
