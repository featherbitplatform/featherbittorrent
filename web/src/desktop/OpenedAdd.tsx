/**
 * The Add page for the links and files the app was opened with.
 *
 * The oldest item in the {@link OpenedQueue} opens the Add torrent dialog with the link in the field or
 * the file picked, and an instance picker above the folder tree, preselected on the instance the window
 * is connected to. Picking another instance builds that instance's own connection (the shell's
 * `connect`), and the folder tree reloads from it; an instance that needs a sign-in shows the sign-in
 * form where the tree was, and a failed sign-in says why and keeps everything as it was. The item leaves
 * the queue only when the add went through or somebody pressed Cancel (or closed the dialog, which is
 * the same answer); then the next one opens.
 *
 * A file the shell would not read (too large, or gone) is said in a banner with a Dismiss, never
 * dropped silently.
 *
 * @module desktop/OpenedAdd
 */
import { useEffect, useId, useState, useSyncExternalStore } from 'react'
import type { KeyboardEvent } from 'react'
import AddTorrent from '../components/AddTorrent'
import type { InitialSource } from '../components/AddTorrent'
import Banner from '../components/Banner'
import Button from '../components/Button'
import TextField from '../components/TextField'
import { createConnection } from '../connection'
import type { Connection } from '../connection'
import type { SessionState } from '../session'
import { displaySafe } from '../format'
import { UNREACHABLE_COPY } from '../live'
import { connect, onOpened, takeOpened } from './bridge'
import type { InstanceItem, OpenedItem } from './bridge'
import { OpenedQueue, startOpenedFeed, useCurrentOpened } from './OpenedQueue'
import type { QueuedItem } from './OpenedQueue'

/** The picker's label. */
export const INSTANCE_LABEL = 'Add to'

/** What the in-place sign-in says above its fields. */
export const SIGN_IN_FIRST = 'Sign in to this instance to add the torrent there.'

/** The instance the window shows, with its connection once it has one. */
export interface Shown {
  id: string
  connection: Connection | null
}

/** A `.torrent` the shell read, as the file the dialog sends. */
function fileOf(name: string, bytesB64: string): File {
  const binary = atob(bytesB64)
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index)
  return new File([bytes], name, { type: 'application/x-bittorrent' })
}

function initialOf(item: OpenedItem): InitialSource | null {
  if (item.kind === 'magnet') return { kind: 'magnet', uri: item.uri }
  if (item.kind === 'file') return { kind: 'file', file: fileOf(item.name, item.bytes_b64) }
  return null
}

/** The sign-in form in place of the folder tree. Not a `<form>`: it sits inside the Add dialog's. */
function SignInHere({ connection }: { connection: Connection }) {
  const state = useSyncExternalStore(connection.session.subscribe, connection.session.get)
  const [user, setUser] = useState('')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const error = state.phase === 'signedOut' ? state.error : null

  async function signIn(): Promise<void> {
    if (busy || user === '' || password === '') return
    setBusy(true)
    try {
      await connection.session.signIn(user, password)
    } finally {
      setBusy(false)
    }
  }

  function onEnter(event: KeyboardEvent<HTMLInputElement>): void {
    if (event.key !== 'Enter') return
    // Enter here signs in, rather than submitting the Add form around it.
    event.preventDefault()
    void signIn()
  }

  return (
    <div className="flex flex-col gap-3 rounded-card border border-rule bg-surface p-3">
      <p className="m-0 text-[13px] text-body">{SIGN_IN_FIRST}</p>
      <TextField
        label="User name"
        autoComplete="username"
        value={user}
        onChange={(event) => setUser(event.target.value)}
        onKeyDown={onEnter}
      />
      <TextField
        label="Password"
        type="password"
        autoComplete="current-password"
        value={password}
        onChange={(event) => setPassword(event.target.value)}
        onKeyDown={onEnter}
      />
      {error !== null && <Banner tone="danger">{error}</Banner>}
      <Button className="self-start" disabled={busy || user === '' || password === ''} onClick={() => void signIn()}>
        {busy ? 'Signing in...' : 'Sign in'}
      </Button>
    </div>
  )
}

/** The connection for `id`: the window's own when it is that instance's, otherwise one built for it. */
function useConnectionFor(id: string, shown: Shown): { connection: Connection | null; error: string | null } {
  const own = shown.id === id && shown.connection !== null ? shown.connection : null
  const [built, setBuilt] = useState<{ id: string; connection: Connection | null; error: string | null } | null>(
    null,
  )
  useEffect(() => {
    if (own !== null) return
    let cancelled = false
    connect(id)
      .then((endpoint) => {
        if (!cancelled) setBuilt({ id, connection: createConnection(endpoint), error: null })
      })
      .catch((cause: unknown) => {
        if (cancelled) return
        const message = typeof cause === 'string' && cause !== '' ? cause : UNREACHABLE_COPY
        setBuilt({ id, connection: null, error: message })
      })
    return () => {
      cancelled = true
    }
  }, [id, own])
  if (own !== null) return { connection: own, error: null }
  if (built === null || built.id !== id) return { connection: null, error: null }
  return { connection: built.connection, error: built.error }
}

/** The Add dialog for one item, with the picker and whatever the picked instance needs first. */
function OpenedDialog({
  queued,
  instances,
  shown,
  onDone,
}: {
  queued: QueuedItem
  instances: InstanceItem[]
  shown: Shown
  onDone: () => void
}) {
  const [initial] = useState(() => initialOf(queued.item))
  const [chosen, setChosen] = useState(shown.id)
  const pickerId = useId()
  const { connection, error } = useConnectionFor(chosen, shown)
  const state = useSyncExternalStore(
    connection?.session.subscribe ?? noSubscribe,
    connection?.session.get ?? noState,
  )
  if (initial === null) return null

  let inPlace = undefined
  if (error !== null) inPlace = <Banner tone="danger">{error}</Banner>
  else if (connection === null)
    inPlace = <p className="px-3 py-2 font-mono text-[12px] text-muted">Connecting...</p>
  else if (state.phase === 'signedOut') inPlace = <SignInHere connection={connection} />
  else if (state.phase === 'unreachable') inPlace = <Banner tone="danger">{UNREACHABLE_COPY}</Banner>

  const picker = (
    <div className="flex flex-col gap-2">
      <label htmlFor={pickerId} className="text-[13px] text-body">
        {INSTANCE_LABEL}
      </label>
      <select
        id={pickerId}
        value={chosen}
        onChange={(event) => setChosen(event.target.value)}
        className="min-h-10 box-border rounded-control border border-rule bg-sunken px-3 text-[13px] text-strong outline-none focus-visible:border-accent-light focus-visible:ring-2 focus-visible:ring-accent-soft"
      >
        {instances.map((instance) => (
          <option key={instance.id} value={instance.id}>
            {displaySafe(instance.name)}
          </option>
        ))}
      </select>
    </div>
  )

  return (
    <AddTorrent
      client={connection?.api ?? null}
      initial={initial}
      instancePicker={picker}
      signIn={inPlace}
      onAdded={() => {}}
      onClose={onDone}
    />
  )
}

const noSubscribe = (): (() => void) => () => {}
const LOADING: SessionState = { phase: 'loading' }
const noState = (): SessionState => LOADING

/** A file the shell would not hand over, said until somebody dismisses it. */
function RefusedBanner({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  return (
    <div className="flex shrink-0 items-start gap-2 px-3 pt-3">
      <Banner tone="danger" className="flex-1">
        {displaySafe(message)}
      </Banner>
      <Button variant="secondary" size="sm" onClick={onDismiss}>
        Dismiss
      </Button>
    </div>
  )
}

/**
 * The opened links and files, one at a time, for as long as the page runs. Mount it outside anything
 * keyed by instance: the queue it holds has to outlive a switch.
 */
export function OpenedHost({ instances, shown }: { instances: InstanceItem[]; shown: Shown }) {
  const [queue] = useState(() => new OpenedQueue())
  useEffect(() => startOpenedFeed(queue, { onOpened, takeOpened }), [queue])
  const current = useCurrentOpened(queue)
  if (current === null) return null
  const done = () => queue.drop(current.id)
  if (current.item.kind === 'refused') return <RefusedBanner message={current.item.message} onDismiss={done} />
  return <OpenedDialog key={current.id} queued={current} instances={instances} shown={shown} onDone={done} />
}
