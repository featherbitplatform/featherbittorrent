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
import { useEffect, useRef, useState } from 'react'
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
interface Base {
  down: number
  up: number
  shownDown: string
  shownUp: string
}

/** KiB/s for display, rounded up so a limit that is not zero never reads as 0 (which means unlimited). */
const kib = (bytes: number): string => String(Math.ceil(bytes / 1024))

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
  // The rates as the server last held them: bytes per second, and the KiB/s text shown for them. A
  // rate whose field is still showing that text is sent back as the exact bytes, never as the
  // rounded KiB/s, so saving the queue never rewrites a limit the person did not touch.
  const [base, setBase] = useState<Base | null>(null)
  // Set synchronously: a second Enter or click in the same tick must not send a second save.
  const savingRef = useRef(false)

  useEffect(() => {
    let cancelled = false
    Promise.all([client.session(), client.queueSettings()]).then(
      ([session, loaded]) => {
        if (cancelled) return
        setQueue(loaded)
        const shownDown = kib(session.download_limit)
        const shownUp = kib(session.upload_limit)
        setBase({ down: session.download_limit, up: session.upload_limit, shownDown, shownUp })
        setValues({
          down: shownDown,
          up: shownUp,
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
    if (values === null || queue === null || base === null || savingRef.current) return
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
    const rate = (kibs: number | undefined, text: string, shown: string, original: number): number | null => {
      if (text === shown) return original > 0 ? original : null
      return kibs ? kibs * 1024 : null
    }
    const count = (n: number | undefined): number | null => (n ? n : null)
    const down = rate(parsed.down, values.down, base.shownDown, base.down)
    const up = rate(parsed.up, values.up, base.shownUp, base.up)
    const nextQueue = {
      ...queue,
      active_downloads: count(parsed.downloads),
      active_seeds: count(parsed.seeds),
      active_limit: count(parsed.total),
    }
    savingRef.current = true
    setBusy(true)
    let limitsApplied = false
    try {
      await client.setSessionLimits(down, up)
      limitsApplied = true
      setBase({ down: down ?? 0, up: up ?? 0, shownDown: values.down, shownUp: values.up })
      await client.setQueueSettings(nextQueue)
      setQueue(nextQueue)
      setSaved(true)
    } catch (cause) {
      const reason = errorMessage(cause)
      setProblem({
        key: null,
        text: limitsApplied
          ? `The speed limits were applied; the queue counts were not: ${reason}`
          : reason,
      })
    } finally {
      savingRef.current = false
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
