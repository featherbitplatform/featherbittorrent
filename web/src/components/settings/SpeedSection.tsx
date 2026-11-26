/**
 * The Speed section of Settings: the two rate limits and, under a `Queue` sub-heading, the three
 * counts of active torrents.
 *
 * The numbers and their words are the terminal's settings screen's: KiB/s for the rates, `0` or
 * nothing meaning unlimited. One Save sends all five to `PUT /api/v1/settings/speed`, and a value
 * that is not a plain non-negative whole number is refused in the form with nothing sent, so
 * half-typed text never quietly becomes unlimited.
 *
 * A server older than the settings routes answers `404` to `GET /api/v1/settings`. The section then
 * says so and keeps working through the engine routes as it always did: the rates come from the
 * session row (bytes per second) and the queue is read and written back as a whole object, so the
 * fields this page never shows (slow-torrent rules, grace, interval) are not reset.
 *
 * @module components/settings/SpeedSection
 */
import { useEffect, useRef, useState } from 'react'
import { ApiError, errorMessage } from '../../api/client'
import type { Api, SpeedSettings } from '../../api/client'
import { useConnection } from '../../connection'
import Banner from '../Banner'
import Button from '../Button'
import TextField from '../TextField'
import { useSectionDirty } from './useSectionDirty'

/** The line shown when the server has no settings routes. */
export const TOO_OLD = 'This server is too old to change its settings here. Update it to change them.'

type Queue = Awaited<ReturnType<Api['queueSettings']>>

const RATES = [
  { key: 'down', label: 'Download limit KiB/s' },
  { key: 'up', label: 'Upload limit KiB/s' },
] as const
const COUNTS = [
  { key: 'downloads', label: 'Active downloads' },
  { key: 'seeds', label: 'Active seeds' },
  { key: 'total', label: 'Active total' },
] as const

type Key = (typeof RATES)[number]['key'] | (typeof COUNTS)[number]['key']
type Values = Record<Key, string>

/** What an old server's engine routes held when the form was filled, for sending untouched rates back exactly. */
interface Legacy {
  queue: Queue
  down: number
  up: number
}

/** KiB/s for display, rounded up so a limit that is not zero never reads as 0 (which means unlimited). */
const kib = (bytes: number): string => String(Math.ceil(bytes / 1024))

/** Nothing typed is 0 (unlimited); otherwise digits only, and a safe integer so no precision is lost on the wire. */
function parse(text: string): number | null {
  const trimmed = text.trim()
  if (trimmed === '') return 0
  if (!/^\d+$/.test(trimmed)) return null
  const value = Number(trimmed)
  return Number.isSafeInteger(value) ? value : null
}

const text = (n: number | null): string => String(n ?? 0)
const orNull = (n: number): number | null => (n === 0 ? null : n)

function fromSettings(speed: SpeedSettings): Values {
  return {
    down: text(speed.download_kib),
    up: text(speed.upload_kib),
    downloads: text(speed.active_downloads),
    seeds: text(speed.active_seeds),
    total: text(speed.active_total),
  }
}

const same = (a: Values, b: Values): boolean =>
  a.down === b.down && a.up === b.up && a.downloads === b.downloads && a.seeds === b.seeds && a.total === b.total

export default function SpeedSection({ client: given }: { client?: Api }) {
  const connection = useConnection()
  const client = given ?? connection.api
  const [values, setValues] = useState<Values | null>(null)
  // The values as the server last held them: what "unsaved" is measured against.
  const [base, setBase] = useState<Values | null>(null)
  const [legacy, setLegacy] = useState<Legacy | null>(null)
  const [problem, setProblem] = useState<{ key: Key | null; text: string } | null>(null)
  const [loadFailed, setLoadFailed] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)
  const [busy, setBusy] = useState(false)
  // Set synchronously: a second Enter or click in the same tick must not send a second save.
  const savingRef = useRef(false)

  useEffect(() => {
    let cancelled = false
    function show(v: Values): void {
      setValues(v)
      setBase(v)
    }
    client.settings().then(
      (all) => {
        if (cancelled) return
        // An answer without the speed object is not a settings answer at all.
        if (typeof all?.speed !== 'object' || all.speed === null) setLoadFailed('The server sent no speed settings.')
        else show(fromSettings(all.speed))
      },
      (cause: unknown) => {
        if (cancelled) return
        if (!(cause instanceof ApiError) || cause.status !== 404) {
          setLoadFailed(errorMessage(cause))
          return
        }
        // An older server: the engine routes, as before.
        Promise.all([client.session(), client.queueSettings()]).then(
          ([session, queue]) => {
            if (cancelled) return
            setLegacy({ queue, down: session.download_limit, up: session.upload_limit })
            show({
              down: kib(session.download_limit),
              up: kib(session.upload_limit),
              downloads: text(queue.active_downloads ?? null),
              seeds: text(queue.active_seeds ?? null),
              total: text(queue.active_limit ?? null),
            })
          },
          (inner: unknown) => {
            if (!cancelled) setLoadFailed(errorMessage(inner))
          },
        )
      },
    )
    return () => {
      cancelled = true
    }
  }, [client])

  const dirty = values !== null && base !== null && !same(values, base)

  /** Send everything. True when the server kept it; false (with the reason shown) when it did not. */
  async function save(): Promise<boolean> {
    if (values === null || base === null || savingRef.current) return false
    setSaved(false)
    const parsed = {} as Record<Key, number>
    for (const { key, label } of [...RATES, ...COUNTS]) {
      const value = parse(values[key])
      if (value === null) {
        setProblem({ key, text: `${label}: enter a whole number, 0 or nothing for unlimited.` })
        return false
      }
      parsed[key] = value
    }
    setProblem(null)
    savingRef.current = true
    setBusy(true)
    let limitsApplied = false
    try {
      if (legacy === null) {
        const answer = await client.saveSpeed({
          download_kib: orNull(parsed.down),
          upload_kib: orNull(parsed.up),
          active_downloads: orNull(parsed.downloads),
          active_seeds: orNull(parsed.seeds),
          active_total: orNull(parsed.total),
        })
        setBase(fromSettings(answer.speed))
        setValues(fromSettings(answer.speed))
      } else {
        // A rate still showing the text it was filled with goes back as the exact bytes, never the
        // rounded KiB/s, so saving the queue never rewrites a limit the person did not touch.
        const rate = (kibs: number, shown: string, original: number, current: string): number | null =>
          current === shown ? (original > 0 ? original : null) : kibs ? kibs * 1024 : null
        const down = rate(parsed.down, base.down, legacy.down, values.down)
        const up = rate(parsed.up, base.up, legacy.up, values.up)
        const queue = {
          ...legacy.queue,
          active_downloads: orNull(parsed.downloads),
          active_seeds: orNull(parsed.seeds),
          active_limit: orNull(parsed.total),
        }
        await client.setSessionLimits(down, up)
        limitsApplied = true
        setLegacy({ queue: legacy.queue, down: down ?? 0, up: up ?? 0 })
        setBase({ ...base, down: values.down, up: values.up })
        await client.setQueueSettings(queue)
        setLegacy({ queue, down: down ?? 0, up: up ?? 0 })
        setBase(values)
      }
      setSaved(true)
      return true
    } catch (cause) {
      const reason = errorMessage(cause)
      setProblem({
        key: null,
        text: limitsApplied ? `The speed limits were applied; the queue counts were not: ${reason}` : reason,
      })
      return false
    } finally {
      savingRef.current = false
      setBusy(false)
    }
  }

  useSectionDirty(dirty, save)

  if (values === null) {
    return loadFailed === null ? (
      <p className="text-[13px] text-muted">Loading...</p>
    ) : (
      <Banner tone="danger">{loadFailed}</Banner>
    )
  }

  function field({ key, label }: { key: Key; label: string }) {
    return (
      <TextField
        key={key}
        label={label}
        inputMode="numeric"
        className="font-mono"
        value={values![key]}
        aria-invalid={problem?.key === key}
        onChange={(event) => {
          setValues({ ...values!, [key]: event.target.value })
          setSaved(false)
        }}
      />
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
      {legacy !== null && <Banner>{TOO_OLD}</Banner>}
      {RATES.map(field)}
      <h3 className="mt-2 text-[14px] font-medium text-strong">Queue</h3>
      {COUNTS.map(field)}
      <p className="text-[12px] text-muted">0 or nothing means unlimited</p>
      {problem !== null && (
        <p role="alert" className="text-[13px] text-error">
          {problem.text}
        </p>
      )}
      {saved && <p className="text-[13px] text-muted">Saved.</p>}
      <div>
        <Button type="submit" disabled={busy}>
          Save
        </Button>
      </div>
    </form>
  )
}
