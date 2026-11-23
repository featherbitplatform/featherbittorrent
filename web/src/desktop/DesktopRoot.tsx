/**
 * The desktop page: {@link App} on a connection the shell hands out, instead of the browser's own.
 *
 * At startup it asks the shell to connect to the default instance (This computer, `local`, until saved
 * instances arrive) and renders `App` under that connection. Nothing about the connection is kept
 * outside memory: the token is the launch secret, which the shell hands over again on request.
 *
 * A launch token can stop working while the window is open: This computer restarted (with Serve the web
 * UI switched, say) and made a new secret. The client sees a `401` and ends the session, which in a
 * browser means the sign-in screen. This computer has no sign-in routes, so that screen would be a dead
 * end: instead the page asks the shell to connect again and rebuilds its connection on the answer. If
 * that fails, or hands back the same token that was just refused, the page says it cannot reach the
 * server and offers to try again, rather than asking again in a loop.
 *
 * @module desktop/DesktopRoot
 */
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react'
import App from '../App'
import Button from '../components/Button'
import { ConnectionContext, createConnection } from '../connection'
import type { Connection } from '../connection'
import { UNREACHABLE_COPY } from '../live'
import { connect, LOCAL_ID, setTrayInstance, THIS_COMPUTER } from './bridge'
import { CloseChoiceHost } from './CloseChoice'

type Phase =
  | { phase: 'connecting' }
  | { phase: 'ready'; connection: Connection; id: string }
  | { phase: 'unreachable'; id: string }

/** The token a connection started with, when it is a launch token for This computer. */
function launchToken(connection: Connection, id: string): string | null {
  if (id !== LOCAL_ID || connection.endpoint.kind !== 'bearer') return null
  return connection.endpoint.token
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
function Unreachable({ onRetry }: { onRetry: () => Promise<void> }) {
  const [trying, setTrying] = useState(false)
  return (
    <main className="flex min-h-dvh flex-col items-center justify-center gap-4">
      <img className="h-16 w-auto" src="/favicon.png" alt="" />
      <div role="alert" className="text-base font-medium text-strong">
        {UNREACHABLE_COPY}
      </div>
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

/** The screens below, with the first-close question ready whatever phase they are in. */
export function DesktopRoot() {
  return (
    <>
      <DesktopScreens />
      <CloseChoiceHost />
    </>
  )
}

function DesktopScreens() {
  const [phase, setPhase] = useState<Phase>({ phase: 'connecting' })
  // One connect at a time: a second request while one is out would race it for the last word.
  const pending = useRef<Promise<void> | null>(null)

  const open = useCallback((id: string, refused: string | null): Promise<void> => {
    pending.current ??= (async () => {
      try {
        const endpoint = await connect(id)
        // The shell handed back the token the engine just refused: asking again would only loop.
        if (refused !== null && endpoint.kind === 'bearer' && endpoint.token === refused) {
          setPhase({ phase: 'unreachable', id })
          return
        }
        setPhase({ phase: 'ready', connection: createConnection(endpoint), id })
      } catch {
        setPhase({ phase: 'unreachable', id })
      }
    })().finally(() => {
      pending.current = null
    })
    return pending.current
  }, [])

  useEffect(() => {
    void open(LOCAL_ID, null)
  }, [open])

  const current = phase.phase === 'connecting' ? LOCAL_ID : phase.id
  const onTokenRefused = useCallback((token: string) => void open(current, token), [open, current])

  // The tray names the instance the page is connected to.
  const connectedId = phase.phase === 'ready' ? phase.id : null
  useEffect(() => {
    if (connectedId === null) return
    setTrayInstance(connectedId === LOCAL_ID ? THIS_COMPUTER : connectedId).catch(() => undefined)
  }, [connectedId])

  if (phase.phase === 'connecting') return <Connecting />
  if (phase.phase === 'unreachable') return <Unreachable onRetry={() => open(phase.id, null)} />
  return (
    <Connected
      key={launchToken(phase.connection, phase.id) ?? phase.id}
      connection={phase.connection}
      id={phase.id}
      onTokenRefused={onTokenRefused}
    />
  )
}
