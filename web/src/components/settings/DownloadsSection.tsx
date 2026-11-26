/**
 * The Downloads section of Settings: where finished torrents go, an optional folder for the unfinished
 * ones, whether new torrents start paused, an optional watch folder, and the program to run when a
 * torrent finishes.
 *
 * Every folder is chosen with the same {@link FolderPicker} the Add dialog uses, on the server's roots, so
 * there is no box to type a path into. One Save sends the whole section to `PUT /api/v1/settings/downloads`
 * and the server's refusal, in its own words, is shown with every field kept.
 *
 * The program is the one setting a server never lets a browser change: `downloads.on_complete.editable`
 * is the server's own answer about this request (true only for the desktop app's This computer view, which
 * carries the launch secret), and the section never works it out for itself. When it is not editable the
 * program is shown as text with `Set in the server's config file`, and a save never sends the key. When it
 * is editable the program is a field, and a save sends it only when it changed.
 *
 * A server older than the settings routes answers `404`: the section says so and has no form.
 *
 * @module components/settings/DownloadsSection
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import { ApiError, errorMessage } from '../../api/client'
import type { Api, DownloadsSettings } from '../../api/client'
import { useConnection } from '../../connection'
import Banner from '../Banner'
import Button from '../Button'
import FolderPicker from '../FolderPicker'
import TextField from '../TextField'
import { TOO_OLD } from './SpeedSection'
import { useSectionDirty } from './useSectionDirty'

/** What the form holds: the three folders, the switch and the program as typed. */
interface Values {
  defaultDir: string
  /** Null is off. */
  unfinishedDir: string | null
  startPaused: boolean
  /** Null is off. */
  watchDir: string | null
  program: string
}

const fromSettings = (d: DownloadsSettings): Values => ({
  defaultDir: d.default_dir,
  unfinishedDir: d.unfinished_dir,
  startPaused: d.start_paused,
  watchDir: d.watch_dir,
  program: d.on_complete?.program ?? '',
})

const same = (a: Values, b: Values): boolean =>
  a.defaultDir === b.defaultDir &&
  a.unfinishedDir === b.unfinishedDir &&
  a.startPaused === b.startPaused &&
  a.watchDir === b.watchDir &&
  a.program === b.program

const CHECKBOX = 'h-4 w-4 accent-accent'
const ROW = 'flex items-center gap-2 text-[13px] text-body'

/** A problem the server filed about a field, drawn under it. */
function Problem({ text }: { text: string | null }) {
  return text === null ? null : <p className="text-[12px] text-stalled">{text}</p>
}

interface FolderFieldProps {
  client: Api
  label: string
  value: string
  onChange(path: string): void
}

/** One folder: its path, and a button that opens the picker on the server's roots. */
function FolderField({ client, label, value, onChange }: FolderFieldProps) {
  const [open, setOpen] = useState(false)
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <code className="min-w-0 break-all font-mono text-[12px] text-strong">{value}</code>
        <Button
          variant="secondary"
          size="sm"
          aria-label={`${open ? 'Close the picker for' : 'Choose'} ${label}`}
          aria-expanded={open}
          onClick={() => setOpen(!open)}
        >
          {open ? 'Done' : 'Choose...'}
        </Button>
      </div>
      {open && <FolderPicker client={client} value={value} keep label={label} onPathChange={onChange} />}
    </div>
  )
}

export default function DownloadsSection({ client: given }: { client?: Api }) {
  const connection = useConnection()
  const client = given ?? connection.api
  const [settings, setSettings] = useState<DownloadsSettings | null>(null)
  const [values, setValues] = useState<Values | null>(null)
  const [base, setBase] = useState<Values | null>(null)
  const [tooOld, setTooOld] = useState(false)
  const [loadFailed, setLoadFailed] = useState<string | null>(null)
  const [problem, setProblem] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)
  const [busy, setBusy] = useState(false)
  const savingRef = useRef(false)

  useEffect(() => {
    let cancelled = false
    client.settings().then(
      (all) => {
        if (cancelled) return
        const downloads = all?.downloads
        if (typeof downloads !== 'object' || downloads === null || typeof downloads.default_dir !== 'string') {
          setLoadFailed('The server sent no downloads settings.')
          return
        }
        setSettings(downloads)
        setValues(fromSettings(downloads))
        setBase(fromSettings(downloads))
      },
      (cause: unknown) => {
        if (cancelled) return
        if (cause instanceof ApiError && cause.status === 404) setTooOld(true)
        else setLoadFailed(errorMessage(cause))
      },
    )
    return () => {
      cancelled = true
    }
  }, [client])

  const dirty = values !== null && base !== null && !same(values, base)
  // The server's answer, never a guess: only it says whether this request may name the program.
  const editable = settings?.on_complete?.editable === true

  const change = useCallback((next: Partial<Values>): void => {
    setValues((current) => (current === null ? current : { ...current, ...next }))
    setSaved(false)
  }, [])
  const setDefaultDir = useCallback((path: string) => change({ defaultDir: path }), [change])
  const setUnfinishedDir = useCallback((path: string) => change({ unfinishedDir: path }), [change])
  const setWatchDir = useCallback((path: string) => change({ watchDir: path }), [change])

  /** Send the section. True when the server kept it; false (with its reason shown) when it did not. */
  async function save(): Promise<boolean> {
    if (values === null || base === null || savingRef.current) return false
    savingRef.current = true
    setBusy(true)
    setSaved(false)
    setProblem(null)
    try {
      const program = values.program.trim()
      const sendProgram = editable && values.program !== base.program
      const answer = await client.saveDownloads(
        {
          default_dir: values.defaultDir,
          unfinished_dir: values.unfinishedDir,
          start_paused: values.startPaused,
          watch_dir: values.watchDir,
        },
        sendProgram ? { onComplete: { program: program === '' ? null : program } } : undefined,
      )
      setSettings(answer.downloads)
      setValues(fromSettings(answer.downloads))
      setBase(fromSettings(answer.downloads))
      setSaved(true)
      return true
    } catch (cause) {
      // The server's own words; every field stays as typed.
      setProblem(errorMessage(cause))
      return false
    } finally {
      savingRef.current = false
      setBusy(false)
    }
  }

  useSectionDirty(dirty, save)

  if (tooOld) return <Banner>{TOO_OLD}</Banner>
  if (values === null || settings === null) {
    return loadFailed === null ? (
      <p className="text-[13px] text-muted">Loading...</p>
    ) : (
      <Banner tone="danger">{loadFailed}</Banner>
    )
  }

  return (
    <form
      className="flex max-w-xl flex-col gap-5"
      onSubmit={(event) => {
        event.preventDefault()
        void save()
      }}
    >
      <fieldset className="flex flex-col gap-2 border-0 p-0">
        <legend className="mb-2 text-[13px] text-body">Default folder</legend>
        <FolderField client={client} label="Default folder" value={values.defaultDir} onChange={setDefaultDir} />
      </fieldset>

      <fieldset className="flex flex-col gap-2 border-0 p-0">
        <legend className="mb-2 text-[13px] text-body">Unfinished folder</legend>
        <label className={ROW}>
          <input
            type="checkbox"
            className={CHECKBOX}
            checked={values.unfinishedDir !== null}
            onChange={(event) => change({ unfinishedDir: event.target.checked ? values.defaultDir : null })}
          />
          Use an unfinished folder
        </label>
        {values.unfinishedDir !== null && (
          <FolderField
            client={client}
            label="Unfinished folder"
            value={values.unfinishedDir}
            onChange={setUnfinishedDir}
          />
        )}
      </fieldset>

      <label className={ROW}>
        <input
          type="checkbox"
          className={CHECKBOX}
          checked={values.startPaused}
          onChange={(event) => change({ startPaused: event.target.checked })}
        />
        Start torrents paused
      </label>

      <fieldset className="flex flex-col gap-2 border-0 p-0">
        <legend className="mb-2 text-[13px] text-body">Watch folder</legend>
        <label className={ROW}>
          <input
            type="checkbox"
            className={CHECKBOX}
            checked={values.watchDir !== null}
            onChange={(event) => change({ watchDir: event.target.checked ? values.defaultDir : null })}
          />
          Use a watch folder
        </label>
        {values.watchDir !== null && (
          <FolderField client={client} label="Watch folder" value={values.watchDir} onChange={setWatchDir} />
        )}
        <Problem text={settings.watch_problem} />
      </fieldset>

      {editable ? (
        <div className="flex flex-col gap-1.5">
          <TextField
            label="Run a program on completion"
            spellCheck={false}
            autoComplete="off"
            className="font-mono"
            placeholder="/usr/local/bin/notify"
            value={values.program}
            onChange={(event) => change({ program: event.target.value })}
          />
          <Problem text={settings.on_complete_problem} />
        </div>
      ) : (
        <div className="flex flex-col gap-1.5">
          <span className="text-[13px] text-body">Run a program on completion</span>
          <code className="font-mono text-[12px] text-strong">{settings.on_complete?.program ?? 'None'}</code>
          <span className="text-[12px] text-muted">Set in the server's config file</span>
          <Problem text={settings.on_complete_problem} />
        </div>
      )}

      {problem !== null && (
        <p role="alert" className="text-[13px] text-error">
          {problem}
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
