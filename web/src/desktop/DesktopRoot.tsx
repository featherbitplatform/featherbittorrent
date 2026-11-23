/**
 * The desktop page: {@link App} on a connection the shell hands out, instead of the browser's own, with
 * the instance switcher in the title bar's switcher slot and the first-close question ready in every
 * phase.
 *
 * At startup it asks the shell for the instance list and connects to the default instance (This
 * computer, `local`, unless another one was made the default), then renders `App` under that
 * connection. Switching builds a new connection and remounts `App` under a key of the instance id, so
 * nothing of one instance's screens carries over to the next. Nothing about a connection is kept outside
 * memory: This computer's token is the launch secret, which the shell hands over again on request, and a
 * remote instance's token comes from a sign-in (the page's, or the shell's with a saved password).
 *
 * A launch token can stop working while the window is open: This computer restarted (with Serve the web
 * UI switched, say) and made a new secret. The client sees a `401` and ends the session, which in a
 * browser means the sign-in screen. This computer has no sign-in routes, so that screen would be a dead
 * end: instead the page asks the shell to connect again and rebuilds its connection on the answer. If
 * that fails, or hands back the same token that was just refused, the page says it cannot reach the
 * server and offers to try again, rather than asking again in a loop. A remote instance's `401` is the
 * sign-in screen, as in a browser.
 *
 * A remote instance that does not answer gets the same screen, never an empty table: before connecting
 * the page asks the shell to probe it, and once connected any request that fails before an HTTP status
 * (which is also what a CORS refusal looks like from inside the webview) has the shell probe it again.
 * Down is "Cannot reach the server."; up but not listing this app's origin adds the line naming the
 * origin to put in the server's `cors_origins`. The switcher stays on screen through all of it, so a
 * default instance that is down at startup is one click away from This computer.
 *
 * The first-close question ({@link CloseChoiceHost}) sits beside the screens, outside anything keyed by
 * instance, so it answers in every phase and is never remounted by a switch. The tray names the
 * instance on screen by its name in the list, never by its id, in every phase: an instance switched to
 * and found down is still the one the window shows. The links and files the app was opened with
 * ({@link OpenedHost}) sit beside the screens too, so a switch never loses one. A failed start of This
 * computer ({@link StartupProblemHost}) replaces all of it.
 *
 * Settings gets the Desktop section through {@link DesktopSettingsContext}. The switcher's `Manage
 * instances` opens Settings at its Instances part (the standalone manager is kept for when no workspace
 * is on screen: connecting, unreachable, or a remote's sign-in). Applying Serve the web UI restarts This
 * computer with a new launch secret: the page then asks the shell for `local` again at once, rather than
 * waiting for a `401`, and reopens Settings at the serving part. The saved keyboard shortcuts are applied
 * when the page starts.
 *
 * @module desktop/DesktopRoot
 */
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react'
import { createPortal } from 'react-dom'
import App from '../App'
import Banner from '../components/Banner'
import Button from '../components/Button'
import { ConnectionContext, createConnection } from '../connection'
import type { Connection } from '../connection'
import { useTitleBarSlots } from '../components/titleBarSlots'
import { DesktopSettingsContext } from '../desktopSettings'
import type { DesktopSettingsSlot, SettingsRequest } from '../desktopSettings'
import { setOverrides } from '../keys'
import { UNREACHABLE_COPY } from '../live'
import { SavePasswordContext } from '../savePassword'
import type { SavePasswordOffer } from '../savePassword'
import {
  connect,
  getShortcuts,
  listInstances,
  LOCAL_ID,
  probeInstance,
  savePassword,
  setTrayInstance,
  THIS_COMPUTER,
} from './bridge'
import type { InstancesList, Probe } from './bridge'
import { CloseChoiceHost } from './CloseChoice'
import { DesktopSettings, INSTANCES_SECTION, SERVING_SECTION } from './DesktopSettings'
import { InstanceSwitcher } from './InstanceSwitcher'
import { OpenedHost } from './OpenedAdd'
import { StartupProblemHost } from './StartupProblem'
import { Instances } from './Instances'

type Phase =
  | { phase: 'connecting'; id: string }
  | { phase: 'ready'; connection: Connection; id: string }
  /** `corsOrigin` is this app's origin when the server answered but does not let it in. */
  | { phase: 'unreachable'; id: string; corsOrigin: string | null }

/** The list before the shell has answered, and if it never does: This computer alone. */
const LOCAL_ONLY: InstancesList = {
  default: LOCAL_ID,
  items: [{ id: LOCAL_ID, name: THIS_COMPUTER, url: '', saved_password: false }],
  keychain: false,
}

/** The line naming the origin a server has to list, verbatim from the plan with the origin after it. */
export const CORS_HINT = "Add this to the server's cors_origins: "

/** What a refused save says when the shell gave no line of its own. */
const PASSWORD_NOT_SAVED = 'The password was not saved.'

/** The token a connection started with, when it is a launch token for This computer. */
function launchToken(connection: Connection, id: string): string | null {
  if (id !== LOCAL_ID || connection.endpoint.kind !== 'bearer') return null
  return connection.endpoint.token
}

/** The unreachable phase for what a probe found (null: the probe itself failed). */
function unreachable(id: string, probe: Probe | null): Phase {
  const corsOrigin = probe !== null && probe.reachable && !probe.cors_ok ? window.location.origin : null
  return { phase: 'unreachable', id, corsOrigin }
}

/** The mark and the word, while a connection is being made. */
function Connecting() {
  return (
    <main className="flex min-h-dvh flex-col items-center justify-center">
      <img className="h-16 w-auto" src="/favicon.png" alt="" />
      <h1 className="mt-4 text-base font-medium tracking-wide text-strong">featherbittorrent</h1>
    </main>
  )
}

/** The same screen `App` shows when the server gives no answer, with the shell asked again on Try again. */
function Unreachable({ corsOrigin, onRetry }: { corsOrigin: string | null; onRetry: () => Promise<void> }) {
  const [trying, setTrying] = useState(false)
  return (
    <main className="flex min-h-dvh flex-col items-center justify-center gap-4 px-4">
      <img className="h-16 w-auto" src="/favicon.png" alt="" />
      <div role="alert" className="text-base font-medium text-strong">
        {UNREACHABLE_COPY}
      </div>
      {corsOrigin !== null && (
        <p className="m-0 text-center text-[13px] text-body">
          {CORS_HINT}
          <code className="font-mono text-strong">{corsOrigin}</code>
        </p>
      )}
      <Button
        variant="secondary"
        disabled={trying}
        onClick={() => {
          setTrying(true)
          void onRetry().finally(() => setTrying(false))
        }}
      >
        {trying ? 'Trying...' : 'Try again'}
      </Button>
    </main>
  )
}

/** `App` under `connection`, unless its launch token was just refused: then the reconnect is under way, and nothing of the sign-in screen is drawn. */
function Connected({
  connection,
  id,
  onTokenRefused,
}: {
  connection: Connection
  id: string
  onTokenRefused: (token: string) => void
}) {
  const state = useSyncExternalStore(connection.session.subscribe, connection.session.get)
  const token = launchToken(connection, id)
  const refused = token !== null && state.phase === 'signedOut'
  useEffect(() => {
    if (refused) onTokenRefused(token)
  }, [refused, token, onTokenRefused])
  if (refused) return <Connecting />
  return (
    <ConnectionContext.Provider value={connection}>
      <App />
    </ConnectionContext.Provider>
  )
}

/** The screens below, with the first-close question ready whatever phase they are in; a failed start of This computer instead of all of it. */
export function DesktopRoot() {
  return (
    <StartupProblemHost>
      <DesktopScreens />
      <CloseChoiceHost />
    </StartupProblemHost>
  )
}

/** The name the tray shows for instance `id`: its name in the list, This computer's own for `local`. */
function trayName(list: InstancesList, id: string): string | null {
  const name = list.items.find((item) => item.id === id)?.name
  if (name !== undefined) return name
  return id === LOCAL_ID ? THIS_COMPUTER : null
}

function DesktopScreens() {
  const slots = useTitleBarSlots()
  const [phase, setPhase] = useState<Phase>({ phase: 'connecting', id: LOCAL_ID })
  const [list, setList] = useState<InstancesList>(LOCAL_ONLY)
  const [managing, setManaging] = useState(false)
  // A saved password the shell could not keep: said above the page, which by then is the workspace.
  const [saveFailed, setSaveFailed] = useState<string | null>(null)
  // One connect per instance at a time: a second request for the same one while it is out would race
  // it for the last word. A connect to another instance supersedes it.
  const pending = useRef<{ id: string; promise: Promise<void> } | null>(null)
  // The newest connect; an answer for an older one arrives too late to matter.
  const latest = useRef(0)
  const probing = useRef(false)
  // Serve the web UI is being applied: This computer is down or has a new secret until the shell
  // answers, so a failed request or a refused token for `local` in that window is the restart, not a
  // fault, and {@link onServingChanged} reconnects once it is over.
  const applying = useRef(false)
  // Settings opened at a section by the shell's side of the page, and the last one asked.
  const [settingsRequest, setSettingsRequest] = useState<SettingsRequest | null>(null)
  const requests = useRef(0)
  const [servingProblem, setServingProblem] = useState<string | null>(null)
  // The phase as of the last render, for callbacks that outlive it.
  const phaseNow = useRef(phase)
  useEffect(() => {
    phaseNow.current = phase
  }, [phase])

  const askSettings = useCallback((section: string) => {
    requests.current += 1
    setSettingsRequest({ section, seq: requests.current })
  }, [])

  // The saved shortcuts, applied once at start; the list in Settings follows them.
  useEffect(() => {
    getShortcuts()
      .then(setOverrides)
      .catch((cause: unknown) => {
        console.warn('The saved shortcuts could not be read:', cause)
      })
  }, [])

  const reload = useCallback(async (): Promise<InstancesList | null> => {
    try {
      const fresh = await listInstances()
      setList(fresh)
      return fresh
    } catch {
      return null
    }
  }, [])

  /** A request to remote instance `id` failed before any HTTP status: down, or refusing this origin? */
  const lost = useCallback(async (id: string, seq: number): Promise<void> => {
    if (probing.current || seq !== latest.current) return
    if (applying.current && id === LOCAL_ID) return
    probing.current = true
    let probe: Probe | null = null
    try {
      probe = await probeInstance(id)
    } catch {
      // The shell could not ask: nothing is known to be reachable.
    } finally {
      probing.current = false
    }
    if (seq !== latest.current) return
    // A blip that is already over is no reason to take the screen away.
    if (probe !== null && probe.reachable && probe.cors_ok) return
    latest.current += 1
    setPhase(unreachable(id, probe))
  }, [])

  /** `fetch`, with a failure that got no answer at all reported to {@link lost}. */
  const watched = useCallback(
    (id: string, seq: number): typeof fetch =>
      async (input, init) => {
        try {
          return await globalThis.fetch(input, init)
        } catch (cause) {
          const aborted = cause instanceof DOMException && cause.name === 'AbortError'
          if (!aborted) void lost(id, seq)
          throw cause
        }
      },
    [lost],
  )

  const open = useCallback(
    (id: string, refused: string | null): Promise<void> => {
      if (pending.current?.id === id) return pending.current.promise
      latest.current += 1
      const seq = latest.current
      setPhase({ phase: 'connecting', id })
      const promise: Promise<void> = (async () => {
        try {
          if (id !== LOCAL_ID) {
            const probe = await probeInstance(id)
            if (seq !== latest.current) return
            if (!probe.reachable || !probe.cors_ok) {
              setPhase(unreachable(id, probe))
              return
            }
          }
          const endpoint = await connect(id)
          if (seq !== latest.current) return
          // The shell handed back the token the engine just refused: asking again would only loop.
          if (refused !== null && endpoint.kind === 'bearer' && endpoint.token === refused) {
            setPhase(unreachable(id, null))
            return
          }
          const connection = id === LOCAL_ID ? createConnection(endpoint) : createConnection(endpoint, watched(id, seq))
          setPhase({ phase: 'ready', connection, id })
        } catch {
          if (seq === latest.current) setPhase(unreachable(id, null))
        }
      })().finally(() => {
        if (pending.current?.promise === promise) pending.current = null
      })
      pending.current = { id, promise }
      return promise
    },
    [watched],
  )

  useEffect(() => {
    void (async () => {
      const fresh = await reload()
      await open(fresh?.default ?? LOCAL_ID, null)
    })()
  }, [reload, open])

  const current = phase.id
  const onTokenRefused = useCallback(
    (token: string) => {
      if (applying.current && current === LOCAL_ID) return
      void open(current, token)
    },
    [open, current],
  )

  const onSwitch = useCallback(
    (id: string) => {
      setManaging(false)
      setSaveFailed(null)
      setSettingsRequest(null)
      if (id === current && phase.phase === 'ready') return
      void open(id, null)
    },
    [open, current, phase.phase],
  )

  /** Manage instances: Settings at its Instances part when a workspace is on screen, the standalone manager otherwise. */
  const onManage = useCallback(() => {
    const now = phaseNow.current
    const session = now.phase === 'ready' ? now.connection.session.get().phase : null
    if (session === 'signedIn' || session === 'open') askSettings(INSTANCES_SECTION)
    else setManaging(true)
  }, [askSettings])

  /** Serving was applied (or refused): if This computer is on screen with an old secret, connect again now. */
  const onServingApplying = useCallback(() => {
    applying.current = true
    setServingProblem(null)
  }, [])

  const onServingChanged = useCallback(
    async (problem: string | null): Promise<void> => {
      applying.current = false
      setServingProblem(problem)
      const now = phaseNow.current
      if (now.id !== LOCAL_ID) return
      if (now.phase === 'ready') {
        let endpoint
        try {
          endpoint = await connect(LOCAL_ID)
        } catch {
          await open(LOCAL_ID, null)
          askSettings(SERVING_SECTION)
          return
        }
        if (endpoint.kind === 'bearer' && endpoint.token === launchToken(now.connection, LOCAL_ID)) return
        latest.current += 1
        setPhase({ phase: 'ready', connection: createConnection(endpoint), id: LOCAL_ID })
      } else {
        // Unreachable or still connecting: connect again from the start.
        await open(LOCAL_ID, null)
      }
      // Back on Settings at the serving part, where the answer (or the refusal) is shown.
      askSettings(SERVING_SECTION)
    },
    [open, askSettings],
  )

  const onChanged = useCallback(async () => {
    const fresh = await reload()
    // The instance on screen was removed: back to the one that is never gone.
    if (fresh !== null && !fresh.items.some((item) => item.id === current)) void open(LOCAL_ID, null)
  }, [reload, open, current])

  // The tray names the instance on screen, by the name the switcher shows, whatever phase it is in.
  const shownName = trayName(list, phase.id)
  useEffect(() => {
    if (shownName === null) return
    setTrayInstance(shownName).catch((cause: unknown) => {
      console.warn('The tray could not name the connected instance:', cause)
    })
  }, [shownName])

  const offer: SavePasswordOffer | null =
    current === LOCAL_ID
      ? null
      : {
          keychain: list.keychain,
          save: async (user, password) => {
            try {
              await savePassword(current, user, password)
              setSaveFailed(null)
            } catch (cause) {
              setSaveFailed(typeof cause === 'string' && cause !== '' ? cause : PASSWORD_NOT_SAVED)
            }
            await reload()
          },
        }

  const desktopSettings = useMemo<DesktopSettingsSlot>(
    () => ({
      sections: (
        <DesktopSettings
          list={list}
          current={current}
          onInstancesChanged={onChanged}
          onServingApplying={onServingApplying}
          onServingChanged={onServingChanged}
          servingProblem={servingProblem}
        />
      ),
      request: settingsRequest,
      onOpened: (seq) => setSettingsRequest((was) => (was?.seq === seq ? null : was)),
    }),
    [list, current, onChanged, onServingApplying, onServingChanged, servingProblem, settingsRequest],
  )

  let body
  if (managing) {
    body = <Instances list={list} current={current} onChanged={onChanged} onClose={() => setManaging(false)} />
  } else if (phase.phase === 'connecting') {
    body = <Connecting />
  } else if (phase.phase === 'unreachable') {
    body = <Unreachable corsOrigin={phase.corsOrigin} onRetry={() => open(phase.id, null)} />
  } else {
    body = (
      <Connected
        key={`${phase.id}:${launchToken(phase.connection, phase.id) ?? ''}`}
        connection={phase.connection}
        id={phase.id}
        onTokenRefused={onTokenRefused}
      />
    )
  }

  // The switcher goes into the title bar's slot; until the bar has mounted it there is nowhere to draw
  // it. The save-failure banner sits under the bar, above the page, which fills the rest of the window
  // (the frame sizes `h-dvh` screens to the space it leaves them, as `height: 100%`, which holds only
  // while every box between this flex column and a screen's root is `contents`; see `App`).
  return (
    <>
      {slots?.switcher != null &&
        createPortal(
          <InstanceSwitcher
            instances={list.items}
            current={current}
            onSwitch={onSwitch}
            onManage={onManage}
          />,
          slots.switcher,
        )}
      {saveFailed !== null && (
        <div className="flex shrink-0 items-start gap-2 px-3 pt-3">
          <Banner tone="danger" className="flex-1">
            {saveFailed}
          </Banner>
          <Button variant="secondary" size="sm" onClick={() => setSaveFailed(null)}>
            Dismiss
          </Button>
        </div>
      )}
      <OpenedHost
        instances={list.items}
        shown={{ id: phase.id, connection: phase.phase === 'ready' ? phase.connection : null }}
      />
      <div className="relative flex min-h-0 flex-1 flex-col overflow-auto">
        <DesktopSettingsContext.Provider value={desktopSettings}>
          <SavePasswordContext.Provider value={offer}>{body}</SavePasswordContext.Provider>
        </DesktopSettingsContext.Provider>
      </div>
    </>
  )
}
