/**
 * The Server section of Settings: the rate limits and the queue counts.
 *
 * The five numbers and their words are the terminal's settings screen's: download and upload limit in
 * KiB/s, then active downloads, active seeds and active total, `0` or nothing meaning unlimited. The
 * rates are read from the session row (bytes per second) and shown in KiB/s; the queue is read as a
 * whole object, only its three counts are edited, and the object goes back whole so the fields this
 * screen never shows (slow-torrent rules, grace, interval) are not reset. One Save applies everything
 * at once, and a value that is not a plain non-negative whole number is refused in the form with
 * nothing sent, so half-typed text never quietly becomes unlimited.
 *
 * @module components/settings/ServerSettings
 */
import { useEffect, useState } from 'react'
import { api, errorMessage } from '../../api/client'
import type { Api } from '../../api/client'
import Banner from '../Banner'
import Button from '../Button'
import TextField from '../TextField'

type Queue = Awaited<ReturnType<Api['queueSettings']>>

const FIELDS = [
  { key: 'down', label: 'Download limit KiB/s' },
  { key: 'up', label: 'Upload limit KiB/s' },
  { key: 'downloads', label: 'Active downloads' },
  { key: 'seeds', label: 'Active seeds' },
  { key: 'total', label: 'Active total' },
] as const

type Key = (typeof FIELDS)[number]['key']
type Values = Record<Key, string>

/** Nothing typed is 0 (unlimited); otherwise digits only, and a safe integer so no precision is lost on the wire. */
function parse(text: string): number | null {
  const trimmed = text.trim()
  if (trimmed === '') return 0
  if (!/^\d+$/.test(trimmed)) return null
  const value = Number(trimmed)
  return Number.isSafeInteger(value) ? value : null
}

export default function ServerSettings({ client = api }: { client?: Api }) {
  const [queue, setQueue] = useState<Queue | null>(null)
  const [values, setValues] = useState<Values | null>(null)
  const [problem, setProblem] = useState<{ key: Key | null; text: string } | null>(null)
  const [saved, setSaved] = useState(false)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    let cancelled = false
    Promise.all([client.session(), client.queueSettings()]).then(
      ([session, loaded]) => {
        if (cancelled) return
        setQueue(loaded)
        setValues({
          down: String(Math.round(session.download_limit / 1024)),
          up: String(Math.round(session.upload_limit / 1024)),
          downloads: String(loaded.active_downloads ?? 0),
          seeds: String(loaded.active_seeds ?? 0),
          total: String(loaded.active_limit ?? 0),
        })
      },
      (cause: unknown) => {
        if (!cancelled) setProblem({ key: null, text: errorMessage(cause) })
      },
    )
    return () => {
      cancelled = true
    }
  }, [client])

  async function save(): Promise<void> {
    if (values === null || queue === null) return
    setSaved(false)
    const parsed: Partial<Record<Key, number>> = {}
    for (const { key, label } of FIELDS) {
      const value = parse(values[key])
      const bytes = value === null ? null : key === 'down' || key === 'up' ? value * 1024 : value
      if (value === null || bytes === null || !Number.isSafeInteger(bytes)) {
        setProblem({ key, text: `${label}: enter a whole number, 0 or nothing for unlimited.` })
        return
      }
      parsed[key] = value
    }
    setProblem(null)
    // 0 is unlimited, which the API spells null for a rate and for a queue count alike.
    const rate = (kib: number | undefined): number | null => (kib ? kib * 1024 : null)
    const count = (n: number | undefined): number | null => (n ? n : null)
    setBusy(true)
    try {
      await client.setSessionLimits(rate(parsed.down), rate(parsed.up))
      await client.setQueueSettings({
        ...queue,
        active_downloads: count(parsed.downloads),
        active_seeds: count(parsed.seeds),
        active_limit: count(parsed.total),
      })
      setSaved(true)
    } catch (cause) {
      setProblem({ key: null, text: errorMessage(cause) })
    } finally {
      setBusy(false)
    }
  }

  if (values === null) {
    return problem === null ? (
      <p className="text-[13px] text-muted">Loading...</p>
    ) : (
      <Banner tone="danger">{problem.text}</Banner>
    )
  }

  return (
    <form
      className="flex max-w-md flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault()
        void save()
      }}
    >
      {FIELDS.map(({ key, label }) => (
        <TextField
          key={key}
          label={label}
          inputMode="numeric"
          className="font-mono"
          value={values[key]}
          aria-invalid={problem?.key === key}
          onChange={(event) => {
            setValues({ ...values, [key]: event.target.value })
            setSaved(false)
          }}
        />
      ))}
      <p className="text-[12px] text-muted">0 or nothing means unlimited</p>
      {problem !== null && (
        <p role="alert" className="text-[13px] text-error">
          {problem.text}
        </p>
      )}
      {saved && <p className="text-[13px] text-muted">Saved</p>}
      <div>
        <Button type="submit" disabled={busy}>
          Save
        </Button>
      </div>
    </form>
  )
}
