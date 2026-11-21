/**
 * The folder tree the Add dialog saves into.
 *
 * The server owns the filesystem, so this is not a browser of `/`. It starts at the first root
 * `roots()` named - the download folder the server adds to by itself - and it only ever asks `list()`
 * about a path a row handed it, which is how "only the folders the server allows" is enforced rather
 * than promised: there is no box to type a path into, so there is no path to type wrong. A folder is
 * listed the first time it is opened and not again while the dialog stays open, so a tree that grows
 * is a tree that asked once per folder, and a 403 shows the server's reason instead of a guess about
 * what was done wrong. `list()` answers the bare *names* of the folders inside a path
 * (`crates/fbtt-core/src/fs_list.rs`), never their absolute paths, so a child's path is built here by
 * [`childPath`] joining the name onto its parent - taking the name for a path of its own would ask
 * the server about `music space` and be told it is outside the allowed folders.
 *
 * The picked folder and everything above it stay open, so a pick is visible instead of remembered
 * somewhere off screen. The two notes the prototype draws are here too: `default` on the folder the
 * server picks when asked to choose, `allowed root` on a root that is browsable but is not that one.
 *
 * @module components/FolderPicker
 */
import { useCallback, useEffect, useState } from 'react'
import { Folder, FolderOpen } from 'lucide-react'
import { errorMessage } from '../api/client'
import type { Api } from '../api/client'
import { displaySafe } from '../format'
import { childPath } from '../paths'
import Banner from './Banner'

/** The note on the row that is the server's configured download folder. */
const DEFAULT_NOTE = 'default'

/** The note on a root that is browsable but is not the default one. */
const ROOT_NOTE = 'allowed root'

/** Why the tree is not the whole filesystem, said where a person can read it. */
const HINT = 'Only the folders the server allows are listed.'

/** What an empty folder says, so a row that opens onto nothing is not read as a folder that failed. */
const EMPTY_NOTE = 'No folders inside'

/** The row indent per level: the prototype puts a nested path 20px further in than its parent. */
function indentOf(depth: number): number {
  return 12 + depth * 20
}

/** The folders a path runs through on the way down, outermost first. */
function ancestorsOf(path: string): string[] {
  const parts = path.split('/').filter(Boolean)
  return parts.slice(0, -1).map((_, index) => `/${parts.slice(0, index + 1).join('/')}`)
}

interface RowProps {
  client: Api
  path: string
  depth: number
  /** The row is the server's default download folder. */
  isDefault: boolean
  /** The row is one of the roots `roots()` named. */
  isRoot: boolean
  selected: string
  open: boolean
  /** The open set, handed down so a parent's render and its child's agree on what is expanded. */
  isOpen(path: string): boolean
  onPick(path: string): void
}

/** One folder and, once it is open, the folders `list()` says are inside it. */
function FolderRow({
  client,
  path,
  depth,
  isDefault,
  isRoot,
  selected,
  open,
  isOpen,
  onPick,
}: RowProps) {
  const [dirs, setDirs] = useState<string[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const isSelected = selected === path

  // Listed once per folder for as long as it stays open: the effect re-runs on open and on a new
  // client, never on a pick elsewhere in the tree.
  useEffect(() => {
    if (!open) return
    let cancelled = false
    let attempt = 0
    async function list(): Promise<void> {
      const current = attempt + 1
      attempt = current
      try {
        const listing = await client.list(path)
        if (cancelled || attempt !== current) return
        setDirs(listing.dirs)
        setError(null)
      } catch (cause) {
        if (cancelled || attempt !== current) return
        setError(errorMessage(cause))
      }
    }
    void list()
    return () => {
      cancelled = true
    }
  }, [client, open, path])

  const note = isDefault ? DEFAULT_NOTE : isRoot ? ROOT_NOTE : null

  return (
    <li>
      <button
        type="button"
        data-folder={path}
        aria-pressed={isSelected}
        onClick={() => {
          onPick(path)
        }}
        style={{ paddingLeft: `${indentOf(depth)}px` }}
        className={`flex min-h-10 w-full cursor-pointer items-center gap-2.5 border-t border-ink-750 py-0 pr-3 text-left transition-colors ${
          isSelected
            ? 'bg-accent-soft text-violet-300'
            : 'bg-transparent text-body hover:bg-ink-700 hover:text-strong'
        }`}
      >
        {open ? (
          <FolderOpen className="h-3.75 w-3.75 flex-none" aria-hidden="true" />
        ) : (
          <Folder className="h-3.75 w-3.75 flex-none" aria-hidden="true" />
        )}
        <span className="min-w-0 truncate font-mono text-[12px]">{displaySafe(path)}</span>
        {note !== null && <span className="ml-auto flex-none text-[12px] text-muted">{note}</span>}
      </button>
      {open && error !== null && (
        <div className="border-t border-ink-750 px-3 py-2">
          <Banner tone="danger">{error}</Banner>
        </div>
      )}
      {open && dirs !== null && dirs.length === 0 && (
        <p
          className="border-t border-ink-750 py-2 font-mono text-[12px] text-muted"
          style={{ paddingLeft: `${indentOf(depth + 1)}px` }}
        >
          {EMPTY_NOTE}
        </p>
      )}
      {open && dirs !== null && dirs.length > 0 && (
        <ul role="list">
          {dirs.map((name: string) => {
            // `list()` named the folder; only this join makes it a path the server recognises.
            const child = childPath(path, name)
            return (
              <FolderRow
                key={child}
                client={client}
                path={child}
                depth={depth + 1}
                isDefault={false}
                isRoot={false}
                selected={selected}
                open={isOpen(child)}
                isOpen={isOpen}
                onPick={onPick}
              />
            )
          })}
        </ul>
      )}
    </li>
  )
}

export interface FolderPickerProps {
  client: Api
  /** The path the dialog will send, or '' until the server has named one. */
  value: string
  /** Called once when the tree opens (with the server's default folder) and after every pick. */
  onPathChange(path: string): void
}

/**
 * The roots `roots()` named, each one opening onto whatever `list()` says is inside it.
 *
 * `onPathChange` has to be stable across renders (the caller keeps it in a `useCallback`): this
 * component's roots request depends on it, and a new function identity on every render would ask the
 * server for the roots again while somebody was opening a folder.
 */
export default function FolderPicker({ client, value, onPathChange }: FolderPickerProps) {
  const [roots, setRoots] = useState<string[] | null>(null)
  const [rootsError, setRootsError] = useState<string | null>(null)
  const [openSet, setOpenSet] = useState<ReadonlySet<string>>(new Set())

  /** Open this folder and every folder above it, so a pick is visible instead of remembered off screen. */
  const expand = useCallback((path: string): void => {
    setOpenSet((current: ReadonlySet<string>) => {
      const next = new Set(current)
      for (const folder of ancestorsOf(path)) next.add(folder)
      next.add(path)
      return next
    })
  }, [])

  useEffect(() => {
    let cancelled = false
    async function load(): Promise<void> {
      try {
        const list = await client.roots()
        if (cancelled) return
        setRoots(list)
        if (list.length > 0) {
          // The dialog starts inside the server's default folder, not at the roof of the filesystem,
          // and it starts *inside* it: the folder is picked and opened in the same breath.
          const first = list[0] as string
          expand(first)
          onPathChange(first)
        }
      } catch (cause) {
        if (cancelled) return
        setRootsError(errorMessage(cause))
      }
    }
    void load()
    return () => {
      cancelled = true
    }
  }, [client, onPathChange, expand])

  // Nothing was named, so there is nothing to draw: the server's own reason is the whole tree, and
  // the dialog keeps its Add button shut because there is no folder to send.
  if (rootsError !== null) return <Banner tone="danger">{rootsError}</Banner>
  if (roots === null)
    return <p className="px-3 py-2 font-mono text-[12px] text-muted">Reading the folders...</p>

  const isOpen = (path: string): boolean => openSet.has(path)

  function pick(path: string): void {
    if (path === value) {
      // Clicking the picked folder again closes its list: the row is the only handle on it.
      setOpenSet((current: ReadonlySet<string>) => {
        const next = new Set(current)
        next.delete(path)
        return next
      })
      return
    }
    expand(path)
    onPathChange(path)
  }

  return (
    <div className="flex flex-col gap-2">
      <span className="text-[13px] text-body">Save to</span>
      <div className="overflow-hidden rounded-card border border-rule bg-sunken">
        <ul role="list">
          {roots.map((root: string, index: number) => (
            <FolderRow
              key={root}
              client={client}
              path={root}
              depth={0}
              isDefault={index === 0}
              isRoot
              selected={value}
              open={isOpen(root)}
              isOpen={isOpen}
              onPick={pick}
            />
          ))}
        </ul>
      </div>
      <span className="text-[12px] text-muted">{HINT}</span>
    </div>
  )
}
