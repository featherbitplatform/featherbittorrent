/**
 * The Add torrent dialog.
 *
 * Two sources, the way the prototype draws them: a magnet link pasted in, or a `.torrent` dropped on
 * the dashed box or chosen with the file chooser. They are not two ways of writing the same request -
 * the magnet goes up as JSON, the file goes up as the bytes it is under `application/x-bittorrent`,
 * because a torrent file whose metainfo went through a text round trip is not a torrent file any
 * more - and whichever tab is showing is the source that gets added. The folder tree below them is
 * the same for both, and so is the tick that starts the torrent paused.
 *
 * What this page can rule out by itself never reaches the server: a file with nothing in it, a file
 * too big to be a torrent file, a drop of several files at once, a name that cannot be a torrent
 * file's name, a field that cannot be a magnet link. Those are refusals here, said in the dialog, with
 * no request spent finding out what the browser already knew - and a drop that is refused leaves the
 * file picked before it, still picked. What only the server knows - that this torrent is already in the list -
 * comes back as its own words inside this dialog, which keeps every field exactly as it was typed: an
 * error that emptied the form made the person rebuild the thing they had just asked for.
 *
 * The dialog is a real `<dialog>`: `useModalDialog` opens it before paint, holds the keyboard inside
 * it, and hands the focus back to the header button that opened it when it goes away. Escape closes it
 * and asks the server nothing - and while its request is on the wire the dialog cannot be closed at
 * all, by Escape, by the close button or by Cancel, because the answer is still owed.
 *
 * @module components/AddTorrent
 */
import { useCallback, useRef, useState } from 'react'
import type { ChangeEvent, DragEvent, FormEvent } from 'react'
import { Upload, X } from 'lucide-react'
import { errorMessage } from '../api/client'
import type { Api } from '../api/client'
import { displaySafe, formatBytes } from '../format'
import { announcePolite } from '../live'
import { useModalDialog } from '../modal'
import Banner from './Banner'
import Button from './Button'
import FolderPicker from './FolderPicker'

/**
 * The largest file this page offers to upload. The server takes a 32 MiB body (`crates/fbtt-server/
 * src/server.rs`) and a `.torrent` is a few hundred KiB of metainfo, so a file this size is an
 * archive or a video somebody mistook for one - and it is worth saying so before that much has been
 * read into memory and put on the wire.
 */
const MAX_TORRENT_BYTES = 10 * 1024 * 1024

/** Why a file will not be sent, said to the person holding it. */
const TOO_LARGE_COPY = 'That file is too large to be a torrent.'
const EMPTY_FILE_COPY = 'That file is empty.'

/** Why a name will not be sent: a torrent file is recognised by its suffix, whatever case it wears. */
const NOT_TORRENT_COPY = 'That is not a .torrent file.'

/** Why a drop of several files at once is a selection or a folder, not the one torrent this takes. */
const ONE_FILE_COPY = 'Drop one torrent file at a time.'

/** Why a field will not be sent. The server parses magnet links; this only refuses what cannot be one. */
const NOT_A_MAGNET_COPY = 'That is not a magnet link.'

/** The sentence the page speaks through its polite region when the server took the torrent. */
const ADDED_COPY = 'Torrent added.'

/** The prefix every magnet link starts with, matched the way people type it: case is not part of it. */
function looksLikeMagnet(value: string): boolean {
  return value.toLowerCase().startsWith('magnet:?')
}

/** Whether a name ends the way a torrent file's name ends, case included or not as typed. */
function isTorrentName(name: string): boolean {
  return name.toLowerCase().endsWith('.torrent')
}

/** What is wrong with this already-accepted file, in the person's language, or null when it is worth uploading. */
function fileProblem(file: File | null): string | null {
  if (file === null) return null
  if (file.size === 0) return EMPTY_FILE_COPY
  if (file.size > MAX_TORRENT_BYTES) return TOO_LARGE_COPY
  return null
}

const SOURCES = [
  { key: 'magnet', label: 'Magnet' },
  { key: 'file', label: 'Torrent file' },
] as const

type Source = (typeof SOURCES)[number]['key']

export interface AddTorrentProps {
  client: Api
  /** Says what was added, after the announcement and before the dialog closes. */
  onAdded(name: string): void
  onClose(): void
}

/**
 * Add a torrent from a magnet link or a `.torrent` file, into a folder the server named.
 *
 * The magnet field is uncontrolled on purpose. What a person pasted stays theirs until the server
 * says yes, and a refusal has to leave it in the field to be fixed or resent.
 */
export default function AddTorrent({ client, onAdded, onClose }: AddTorrentProps) {
  const [source, setSource] = useState<Source>('magnet')
  const [path, setPath] = useState('')
  const [file, setFile] = useState<File | null>(null)
  const [startPaused, setStartPaused] = useState(false)
  const [refusal, setRefusal] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const magnetRef = useRef<HTMLInputElement>(null)
  // The submit guard, on a ref rather than on `busy`: a second press in the same tick reads state the
  // first has set but React has not yet painted, and two adds would put the same torrent on the wire
  // twice. A ref is true the moment the request starts, and it is what Close, Cancel and Escape read.
  const submittingRef = useRef(false)

  // Escape and the close X answer the way Cancel already does: not at all while a request is owed an
  // answer. The ref, not the state, because the guard has to hold from the instant the request starts.
  function requestClose(): void {
    if (submittingRef.current) return
    onClose()
  }

  const { ref, onKeyDown } = useModalDialog(requestClose, '#add-magnet')
  const onPathChange = useCallback((next: string) => {
    setPath(next)
  }, [])

  const problem = source === 'file' ? fileProblem(file) : null
  // On the magnet tab a refusal is said when Add is pressed, because the field is what it is and
  // grey-buttoning a half-typed link is worse than naming it. On the file tab the box already knows
  // what is wrong, so the button goes shut with the reason.
  const canAdd =
    !busy && path !== '' && (source === 'magnet' || (file !== null && problem === null))

  function pickFile(next: File | null): void {
    setFile(next)
    // A pick is checked as it lands: the box says what is wrong with the file before anybody can
    // press Add, and the button goes shut with it.
    setRefusal(fileProblem(next))
  }

  /** Take a file the person offered, or say why it is not taken - leaving the earlier pick alone. */
  function takeFile(next: File | null): void {
    if (next !== null && !isTorrentName(next.name)) {
      setRefusal(NOT_TORRENT_COPY)
      return
    }
    pickFile(next)
  }

  function onDrop(event: DragEvent<HTMLDivElement>): void {
    event.preventDefault()
    const files = event.dataTransfer?.files
    if (files !== undefined && files.length > 1) {
      // Several at once is a folder or a multiselect, not the one torrent this dialog takes; the
      // file already picked, if there was one, is still the file that will be sent.
      setRefusal(ONE_FILE_COPY)
      return
    }
    takeFile(files?.[0] ?? null)
  }

  async function add(): Promise<void> {
    if (submittingRef.current || busy || path === '') return
    const magnet = (magnetRef.current?.value ?? '').trim()
    const picked = source === 'file' ? file : null
    if (source === 'magnet' && !looksLikeMagnet(magnet)) {
      setRefusal(NOT_A_MAGNET_COPY)
      return
    }
    if (source === 'file') {
      if (picked === null) return
      const wrong = fileProblem(picked)
      if (wrong !== null) {
        setRefusal(wrong)
        return
      }
    }

    submittingRef.current = true
    setBusy(true)
    setRefusal(null)
    try {
      const added =
        picked === null
          ? await client.addMagnet(magnet, path, startPaused)
          : await client.addFile(await picked.arrayBuffer(), path, startPaused)
      announcePolite(ADDED_COPY)
      onAdded(added.name)
      onClose()
    } catch (cause) {
      // The server's own words, here, with every field left exactly as it was.
      setRefusal(errorMessage(cause))
    } finally {
      submittingRef.current = false
      setBusy(false)
    }
  }

  return (
    <dialog
      ref={ref}
      onKeyDown={onKeyDown}
      aria-labelledby="add-torrent-title"
      className="w-[min(720px,calc(100vw-32px))] rounded-overlay border border-rule bg-raised text-body shadow-[0_24px_64px_rgba(0,0,0,0.5)] backdrop:bg-canvas/72"
    >
      <div className="flex items-center justify-between gap-3 border-b border-ink-700 px-5.5 py-4.5">
        <h2 id="add-torrent-title" className="text-[16px] font-semibold text-strong">
          Add torrent
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

      <form
        onSubmit={(event: FormEvent<HTMLFormElement>) => {
          event.preventDefault()
          void add()
        }}
        className="flex flex-col gap-5 px-5.5 py-5"
      >
        <div
          role="tablist"
          aria-label="Source"
          className="flex items-center gap-1 self-start rounded-card border border-ink-700 bg-sunken p-1"
        >
          {SOURCES.map((tab: (typeof SOURCES)[number]) => (
            <button
              key={tab.key}
              type="button"
              role="tab"
              id={`add-source-${tab.key}`}
              aria-controls="add-source-panel"
              aria-selected={source === tab.key}
              tabIndex={source === tab.key ? 0 : -1}
              onClick={() => {
                setSource(tab.key)
                // The reason the file box holds comes back with the tab, recomputed from the file as
                // it is now; on the magnet tab there is no file problem to show.
                setRefusal(tab.key === 'file' ? fileProblem(file) : null)
              }}
              className={`min-h-8.5 cursor-pointer rounded-control px-3.5 text-[13px] transition-colors ${
                source === tab.key
                  ? 'bg-ink-700 text-strong'
                  : 'bg-transparent text-muted hover:text-strong'
              }`}
            >
              {tab.label}
            </button>
          ))}
        </div>

        <div
          role="tabpanel"
          id="add-source-panel"
          aria-labelledby={`add-source-${source}`}
          className="flex flex-col"
        >
          {source === 'magnet' ? (
            <div className="flex flex-col gap-2">
              <label htmlFor="add-magnet" className="text-[13px] text-body">
                Magnet link
              </label>
              <input
                ref={magnetRef}
                id="add-magnet"
                type="text"
                spellCheck={false}
                autoComplete="off"
                placeholder="magnet:?xt=urn:btih:..."
                className="min-h-10 box-border rounded-control border border-rule bg-sunken px-3 font-mono text-[12px] text-strong outline-none transition-colors focus-visible:border-accent-light focus-visible:ring-2 focus-visible:ring-accent-soft"
              />
              <span className="text-[12px] text-muted">
                The file list appears once the metadata arrives from peers.
              </span>
            </div>
          ) : (
            <div
              data-drop-zone
              onDragOver={(event: DragEvent<HTMLDivElement>) => {
                // Without this the browser opens the file instead of taking it.
                event.preventDefault()
              }}
              onDrop={onDrop}
              className="flex min-h-35 flex-col items-center justify-center gap-2.5 rounded-card border border-dashed border-rule-strong bg-sunken px-4 py-4 text-center"
            >
              <Upload className="h-6 w-6 text-accent-light" aria-hidden="true" />
              <span className="text-[13px] text-body">
                Drop a <span className="font-mono">.torrent</span> file here
              </span>
              <label className="inline-flex min-h-9 cursor-pointer items-center rounded-control border border-rule bg-surface px-3.5 text-[13px] text-strong transition-colors hover:bg-ink-700">
                Choose file
                <input
                  type="file"
                  accept=".torrent,application/x-bittorrent"
                  className="sr-only"
                  onChange={(event: ChangeEvent<HTMLInputElement>) => {
                    takeFile(event.target.files?.[0] ?? null)
                  }}
                />
              </label>
              {file !== null && (
                <span className="font-mono text-[12px] text-muted">
                  {`${displaySafe(file.name)} ${formatBytes(file.size)}`}
                </span>
              )}
            </div>
          )}
        </div>

        <FolderPicker client={client} value={path} onPathChange={onPathChange} />

        {refusal !== null && <Banner tone="danger">{refusal}</Banner>}

        <div className="flex flex-wrap items-center gap-2.5 border-t border-ink-700 pt-4">
          <label className="flex items-center gap-2 text-[13px] text-body">
            <input
              type="checkbox"
              checked={startPaused}
              onChange={(event: ChangeEvent<HTMLInputElement>) => {
                setStartPaused(event.target.checked)
              }}
              className="h-4 w-4 accent-accent"
            />
            Start paused
          </label>
          <div className="ml-auto flex items-center gap-2.5">
            <Button variant="secondary" onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button type="submit" disabled={!canAdd}>
              {busy ? 'Adding...' : 'Add'}
            </Button>
          </div>
        </div>
      </form>
    </dialog>
  )
}
