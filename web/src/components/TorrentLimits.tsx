/**
 * The dialog that sets one torrent's speed limits.
 *
 * The fields are in KiB/s, the unit a person thinks in, and leave as bytes per second, the unit the
 * server counts in. Both start empty: this page does not know what limit the torrent has now, so the
 * dialog never pretends to show it. A blank or `0` field sends `null`, which the server reads as
 * unlimited. A server refusal stays inside the dialog, in the same banner the removal dialog uses.
 *
 * @module components/TorrentLimits
 */
import { useRef, useState } from 'react'
import { X } from 'lucide-react'
import { errorMessage } from '../api/client'
import type { Api } from '../api/client'
import { useModalDialog } from '../modal'
import Banner from './Banner'
import Button from './Button'
import TextField from './TextField'

export interface TorrentLimitsProps {
  id: string
  client: Api
  onClose(): void
}

/** `''` and `0` mean no limit; a whole number of KiB/s is that many times 1024 bytes; anything else is not a limit. */
function toBps(text: string): number | null | undefined {
  const trimmed = text.trim()
  if (trimmed === '') return null
  if (!/^\d{1,9}$/.test(trimmed)) return undefined
  const kib = Number(trimmed)
  return kib === 0 ? null : kib * 1024
}

export default function TorrentLimits({ id, client, onClose }: TorrentLimitsProps) {
  const [down, setDown] = useState('')
  const [up, setUp] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const submittingRef = useRef(false)

  function requestClose(): void {
    if (submittingRef.current) return
    onClose()
  }
  const { ref, onKeyDown } = useModalDialog(requestClose, '[data-cancel]')

  async function save(): Promise<void> {
    if (submittingRef.current) return
    const downBps = toBps(down)
    const upBps = toBps(up)
    if (downBps === undefined || upBps === undefined) {
      setError('A limit is a whole number of KiB/s.')
      return
    }
    submittingRef.current = true
    setBusy(true)
    setError(null)
    try {
      await client.setTorrentLimits(id, downBps, upBps)
    } catch (cause) {
      setError(errorMessage(cause))
      submittingRef.current = false
      setBusy(false)
      return
    }
    onClose()
  }

  return (
    <dialog
      ref={ref}
      onKeyDown={onKeyDown}
      aria-labelledby="torrent-limits-title"
      className="w-[min(420px,calc(100vw-32px))] rounded-overlay border border-rule bg-raised text-body shadow-[0_24px_64px_rgba(0,0,0,0.5)] backdrop:bg-canvas/72"
    >
      <div className="flex items-start justify-between gap-3 border-b border-ink-700 px-5.5 py-4.5">
        <h2 id="torrent-limits-title" className="text-[16px] font-semibold text-strong">
          Torrent limits
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
      <div className="flex flex-col gap-3 px-5.5 py-5">
        <TextField
          label="Download limit (KiB/s)"
          inputMode="numeric"
          value={down}
          onChange={(event) => {
            setDown(event.target.value)
          }}
        />
        <TextField
          label="Upload limit (KiB/s)"
          inputMode="numeric"
          value={up}
          onChange={(event) => {
            setUp(event.target.value)
          }}
        />
        <p className="text-[12px] text-muted">Sets a new limit. 0 removes it.</p>
        {error !== null && <Banner tone="danger">{error}</Banner>}
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2.5 border-t border-ink-700 px-5.5 py-4">
        <Button data-cancel variant="secondary" onClick={requestClose} disabled={busy}>
          Cancel
        </Button>
        <Button
          disabled={busy}
          onClick={() => {
            void save()
          }}
        >
          Save
        </Button>
      </div>
    </dialog>
  )
}
