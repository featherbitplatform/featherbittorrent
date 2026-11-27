/**
 * The Proxy section of Settings: the proxy type, its host and port, an optional login, and how the
 * proxy is doing.
 *
 * One Save sends everything to `PUT /api/v1/settings/proxy`. The password is never prefilled and never
 * shown: the field starts empty with the placeholder `saved` when one is stored, an untouched field
 * saves as `keep`, a typed one as `{ set }`, and `Clear password` as `clear`. The server refuses to keep
 * a stored password for a new host or port, so the form never offers it: once the host or port is edited
 * the `saved` placeholder goes, the form says to enter the password again, and Save waits for a typed
 * password or `Clear password`. Changing only the type or the user name keeps `saved`.
 *
 * `None` hides every other field and removes the proxy. A server's refusal (or a keychain answer) is shown
 * under the form with the server's own words and every field stays as typed.
 *
 * @module components/settings/ProxySection
 */
import { useEffect, useRef, useState } from 'react'
import { ApiError, errorMessage } from '../../api/client'
import type { Api, ProxyChange, ProxyHealth, ProxySettings } from '../../api/client'
import { useConnection } from '../../connection'
import { displaySafe } from '../../format'
import Banner from '../Banner'
import Button from '../Button'
import TextField from '../TextField'
import { TOO_OLD } from './SpeedSection'
import { useSectionDirty } from './useSectionDirty'

type Kind = 'none' | 'http' | 'socks5'

const KINDS: Array<{ value: Kind; label: string }> = [
  { value: 'none', label: 'None' },
  { value: 'http', label: 'HTTP' },
  { value: 'socks5', label: 'SOCKS5' },
]

const HELP =
  'While a proxy is set, every connection goes through it. Incoming connections, port forwarding and local peer discovery are off, so you may see fewer peers.'
const HTTP_HELP = 'An HTTP proxy carries TCP only, so the DHT, uTP and UDP trackers are off too.'
const UDP_LINE = 'This proxy does not relay UDP, so the DHT and UDP trackers are off'
const NO_KEYCHAIN = 'This system has no keychain, so the proxy password is kept in config.yaml.'
const NEW_ADDRESS = "Enter the password again when the proxy's host or port changes."
const NO_HOST = "Enter the proxy's host name or address."
const BAD_PORT = 'The port must be a number from 1 to 65535.'

interface Values {
  kind: Kind
  host: string
  port: string
  username: string
}

const FIELD =
  'box-border min-h-10 rounded-control border border-rule bg-sunken px-3 text-strong outline-none focus-visible:border-accent-light focus-visible:ring-2 focus-visible:ring-accent-soft'

function kindOf(type: string): Kind {
  return type === 'http' || type === 'socks5' ? type : 'none'
}

function fromSettings(proxy: ProxySettings): Values {
  const kind = kindOf(proxy.type)
  return {
    kind,
    host: kind === 'none' ? '' : (proxy.host ?? ''),
    port: kind === 'none' || !proxy.port ? '' : String(proxy.port),
    username: kind === 'none' ? '' : (proxy.username ?? ''),
  }
}

/** The headline for a health that needs the person's attention, or null. */
function headline(health: ProxyHealth | null): string | null {
  if (health === null) return null
  if (health.state === 'login_refused') return "Can't reach the proxy: it refused the login"
  if (health.state === 'unreachable') {
    return health.reason ? `Can't reach the proxy: ${displaySafe(health.reason)}` : "Can't reach the proxy"
  }
  return null
}

export default function ProxySection({ client: given }: { client?: Api }) {
  const connection = useConnection()
  const client = given ?? connection.api
  const [proxy, setProxy] = useState<ProxySettings | null>(null)
  const [values, setValues] = useState<Values | null>(null)
  const [base, setBase] = useState<Values | null>(null)
  // What was typed in the password field, and whether the stored one is to be removed. Neither is
  // ever filled from the server.
  const [password, setPassword] = useState('')
  const [clear, setClear] = useState(false)
  const [health, setHealth] = useState<ProxyHealth | null>(null)
  const [tooOld, setTooOld] = useState(false)
  const [loadFailed, setLoadFailed] = useState<string | null>(null)
  const [problem, setProblem] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)
  const [busy, setBusy] = useState(false)
  const savingRef = useRef(false)

  function show(next: ProxySettings): void {
    setProxy(next)
    setValues(fromSettings(next))
    setBase(fromSettings(next))
    setPassword('')
    setClear(false)
  }

  function askHealth(): void {
    Promise.resolve()
      .then(() => client.proxyState())
      .then(setHealth, () => {
        // A server that will not say has nothing to show.
      })
  }

  useEffect(() => {
    let cancelled = false
    client.settings().then(
      (all) => {
        if (cancelled) return
        // A server from before the proxy setting answers settings without it.
        if (typeof all?.proxy !== 'object' || all.proxy === null) setTooOld(true)
        else show(all.proxy)
      },
      (cause: unknown) => {
        if (cancelled) return
        if (cause instanceof ApiError && cause.status === 404) setTooOld(true)
        else setLoadFailed(errorMessage(cause))
      },
    )
    let live = true
    Promise.resolve()
      .then(() => client.proxyState())
      .then(
        (next) => {
          if (live) setHealth(next)
        },
        () => {},
      )
    return () => {
      cancelled = true
      live = false
    }
  }, [client])

  const stored = proxy?.password_saved === true && base !== null && base.kind !== 'none'
  const addressChanged =
    values !== null && base !== null && (values.host.trim() !== base.host || values.port.trim() !== base.port)
  // The stored password cannot be kept for a new address: ask for it again.
  const needsPassword = stored && addressChanged && values?.kind !== 'none' && password === '' && !clear
  const dirty =
    values !== null &&
    base !== null &&
    (values.kind !== base.kind ||
      values.host !== base.host ||
      values.port !== base.port ||
      values.username !== base.username ||
      password !== '' ||
      clear)

  async function save(): Promise<boolean> {
    if (values === null || savingRef.current || needsPassword) return false
    setSaved(false)
    let change: ProxyChange
    if (values.kind === 'none') {
      change = { type: 'none', host: '', port: 0, username: null, password: 'clear' }
    } else {
      const host = values.host.trim()
      if (host === '') {
        setProblem(NO_HOST)
        return false
      }
      const port = values.port.trim()
      const number = /^\d+$/.test(port) ? Number(port) : NaN
      if (!(number >= 1 && number <= 65535)) {
        setProblem(BAD_PORT)
        return false
      }
      const user = values.username.trim()
      change = {
        type: values.kind,
        host,
        port: number,
        username: user === '' ? null : user,
        password: clear ? 'clear' : password !== '' ? { set: password } : 'keep',
      }
    }
    setProblem(null)
    savingRef.current = true
    setBusy(true)
    try {
      const answer = await client.saveProxy(change)
      if (typeof answer?.proxy === 'object' && answer.proxy !== null) show(answer.proxy)
      setSaved(true)
      askHealth()
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
  if (values === null || proxy === null) {
    return loadFailed === null ? (
      <p className="text-[13px] text-muted">Loading...</p>
    ) : (
      <Banner tone="danger">{loadFailed}</Banner>
    )
  }

  function change(next: Partial<Values>): void {
    setValues({ ...values!, ...next })
    setSaved(false)
  }

  const active = values.kind !== 'none'
  const line = base !== null && base.kind !== 'none' ? headline(health) : null
  const udp = base !== null && base.kind !== 'none' && health?.state === 'udp_not_relayed'
  const showSaved = stored && !clear && !addressChanged

  return (
    <form
      className="flex max-w-md flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault()
        void save()
      }}
    >
      {line !== null && <Banner tone="danger">{line}</Banner>}
      {udp && <Banner>{UDP_LINE}</Banner>}
      <div className="flex flex-col gap-1.5">
        <label htmlFor="proxy-type" className="text-[13px] text-body">
          Proxy type
        </label>
        <select
          id="proxy-type"
          value={values.kind}
          onChange={(event) => change({ kind: event.target.value as Kind })}
          className={`${FIELD} cursor-pointer`}
        >
          {KINDS.map((k) => (
            <option key={k.value} value={k.value}>
              {k.label}
            </option>
          ))}
        </select>
      </div>
      {active && (
        <>
          <TextField
            label="Host"
            className="font-mono"
            autoComplete="off"
            spellCheck={false}
            value={values.host}
            onChange={(event) => change({ host: event.target.value })}
          />
          <TextField
            label="Port"
            inputMode="numeric"
            className="font-mono"
            value={values.port}
            onChange={(event) => change({ port: event.target.value })}
          />
          <TextField
            label="User name"
            autoComplete="off"
            spellCheck={false}
            value={values.username}
            onChange={(event) => change({ username: event.target.value })}
          />
          <TextField
            label="Password"
            type="password"
            autoComplete="new-password"
            placeholder={showSaved ? 'saved' : ''}
            value={password}
            onChange={(event) => {
              setPassword(event.target.value)
              setClear(false)
              setSaved(false)
            }}
          />
          {needsPassword && <p className="text-[12px] text-muted">{NEW_ADDRESS}</p>}
          {clear && <p className="text-[12px] text-muted">The saved password is removed when you save.</p>}
          {proxy.password_storage === 'unavailable' && <p className="text-[12px] text-muted">{NO_KEYCHAIN}</p>}
          {stored && !clear && (
            <div>
              <Button
                type="button"
                variant="secondary"
                onClick={() => {
                  setClear(true)
                  setPassword('')
                  setSaved(false)
                }}
              >
                Clear password
              </Button>
            </div>
          )}
          <p className="text-[12px] text-muted">{HELP}</p>
          {values.kind === 'http' && <p className="text-[12px] text-muted">{HTTP_HELP}</p>}
        </>
      )}
      {problem !== null && (
        <p role="alert" className="text-[13px] text-error">
          {displaySafe(problem)}
        </p>
      )}
      {saved && <p className="text-[13px] text-muted">Saved.</p>}
      <div>
        <Button type="submit" disabled={busy || needsPassword}>
          Save
        </Button>
      </div>
    </form>
  )
}
