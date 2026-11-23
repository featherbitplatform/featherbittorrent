/**
 * The Desktop section of Settings, drawn only inside the app: what closing the window does, Start at
 * login, the system title bar (Linux only, where the drawn bar has a fallback), Serve the web UI, the
 * editable keyboard shortcuts and the instances.
 *
 * Every switch goes to the shell, which owns `desktop.yaml`; this section keeps only what it shows. Closing
 * offers `Keep running in the tray` only when the system gave the app a tray. Serve the web UI restarts This
 * computer, so after it is applied the page around this section reconnects to `local`
 * (`onServingChanged`), and the section shows where the web UI is reachable, or `Off`.
 *
 * @module desktop/DesktopSettings
 */
import { useEffect, useState } from 'react'
import type { FormEvent, ReactNode } from 'react'
import Banner from '../components/Banner'
import Button from '../components/Button'
import TextField from '../components/TextField'
import { Shortcuts } from '../components/settings/Shortcuts'
import type { ShortcutOverride } from '../keys'
import {
  desktopInfo,
  desktopPreferences,
  servingStatus,
  setCloseAction,
  setServing,
  setShortcuts,
  setStartAtLogin,
  setSystemTitleBar,
  systemTitleBar,
  trayAvailable,
} from './bridge'
import type {
  CloseAction,
  DesktopInfo,
  DesktopPreferences,
  InstancesList,
  ServingRequest,
  ServingState,
} from './bridge'
import { Instances } from './Instances'

/** The address serving listens on until the person picks another: every interface, port 8090. */
export const DEFAULT_BIND = '0.0.0.0:8090'

/** The section ids Settings can be opened at. */
export const DESKTOP_SECTION = 'settings-desktop'
export const SERVING_SECTION = 'settings-desktop-serving'
export const INSTANCES_SECTION = 'settings-desktop-instances'

export interface DesktopSettingsProps {
  /** The instance list, as the switcher has it. */
  list: InstancesList
  /** The instance the window is connected to. */
  current: string
  /** Read the instance list again: something changed. */
  onInstancesChanged(): Promise<void>
  /**
   * Serving was applied, or refused with `problem`: connect to This computer again if it restarted. The
   * page may remount Settings on that reconnect, so the problem lives with the caller and comes back as
   * `servingProblem`.
   */
  onServingChanged(problem: string | null): Promise<void>
  /** An apply is about to restart This computer: requests to it may fail until `onServingChanged`. */
  onServingApplying?(): void
  /** The line the last apply was refused with, kept across a reconnect. */
  servingProblem?: string | null
}

/** The shell's refusal as a line: Tauri rejects a command with its error string. */
function line(cause: unknown, fallback: string): string {
  if (typeof cause === 'string' && cause !== '') return cause
  if (cause instanceof Error && cause.message !== '') return cause.message
  return fallback
}

/** Said under the switch when serving is on and other machines can reach it. */
export const PLAIN_HTTP_WARNING =
  'Passwords travel unencrypted over http. For access from outside your home network, put the web UI behind https.'

/** Whether `bind` (an address and port as typed) listens on this machine only. */
function loopback(bind: string): boolean {
  const text = bind.trim()
  const host = text.startsWith('[') ? text.slice(1, text.indexOf(']')) : text.slice(0, text.lastIndexOf(':'))
  return host === 'localhost' || host === '::1' || /^127\./.test(host)
}

/** Whether applying `asked` over `now` would change nothing, so the shell is not asked to restart. */
function unchanged(now: ServingState, asked: ServingRequest): boolean {
  if (!asked.enabled) return !now.enabled
  return now.enabled && asked.password === '' && asked.bind.trim() === now.bind && asked.user.trim() === now.user
}

const CHECKBOX = 'h-4 w-4 accent-accent'
const ROW = 'flex items-center gap-2 text-[13px] text-body'

function Part({ id, title, children }: { id?: string; title: string; children: ReactNode }) {
  return (
    <div id={id} className="flex flex-col gap-3">
      <h3 className="text-[14px] font-medium text-strong">{title}</h3>
      {children}
    </div>
  )
}

/** Serve the web UI: the switch, where to listen and the login, applied together. */
function Serving({
  onServingApplying,
  onServingChanged,
  problem,
}: {
  onServingApplying?(): void
  onServingChanged(problem: string | null): Promise<void>
  problem: string | null
}) {
  const [state, setState] = useState<ServingState | null>(null)
  const [enabled, setEnabled] = useState(false)
  const [bind, setBind] = useState(DEFAULT_BIND)
  const [user, setUser] = useState('')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)

  function show(now: ServingState): void {
    setState(now)
    setEnabled(now.enabled)
    setBind(now.bind === '' ? DEFAULT_BIND : now.bind)
    setUser(now.user)
  }

  useEffect(() => {
    let live = true
    servingStatus()
      .then((now) => {
        if (live) show(now)
      })
      .catch(() => {})
    return () => {
      live = false
    }
  }, [])

  async function apply(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault()
    // Nothing would change: off while off, or the same address and login with no new password.
    if (state !== null && unchanged(state, { enabled, bind, user, password })) return
    setBusy(true)
    let refused: string | null = null
    onServingApplying?.()
    try {
      const now = await setServing({ enabled, bind, user, password })
      show(now)
    } catch (cause) {
      refused = line(cause, 'Serving could not be changed.')
      servingStatus()
        .then(show)
        .catch(() => {})
    } finally {
      // Never kept in the page longer than the apply, answered or refused.
      setPassword('')
      setBusy(false)
    }
    // A refused start still restarted This computer (back on the window's own engine), so the page asks
    // either way; the caller reconnects only when the launch secret changed.
    await onServingChanged(refused)
  }

  return (
    <form className="flex flex-col gap-3" onSubmit={(event) => void apply(event)}>
      <p className="m-0 text-[13px] text-body" data-serving-status>
        {state?.address != null ? (
          <>
            Reachable at <code className="font-mono text-strong">{state.address}</code>
          </>
        ) : (
          'Off'
        )}
      </p>
      <label className={ROW}>
        <input
          type="checkbox"
          className={CHECKBOX}
          checked={enabled}
          onChange={(event) => setEnabled(event.target.checked)}
        />
        Serve the web UI
      </label>
      {enabled && !loopback(bind) && (
        <Banner tone="warning">{PLAIN_HTTP_WARNING}</Banner>
      )}
      {enabled && (
        <>
          <TextField
            label="Address to listen on"
            spellCheck={false}
            className="font-mono"
            value={bind}
            onChange={(event) => setBind(event.target.value)}
          />
          <TextField
            label="User name"
            autoComplete="off"
            value={user}
            onChange={(event) => setUser(event.target.value)}
          />
          <TextField
            label="Password"
            type="password"
            autoComplete="new-password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </>
      )}
      <div>
        <Button type="submit" disabled={busy}>
          {busy ? 'Applying...' : 'Apply'}
        </Button>
      </div>
      {problem !== null && <Banner tone="danger">{problem}</Banner>}
    </form>
  )
}

export function DesktopSettings({
  list,
  current,
  onInstancesChanged,
  onServingApplying,
  onServingChanged,
  servingProblem = null,
}: DesktopSettingsProps) {
  const [platform, setPlatform] = useState<DesktopInfo['platform'] | null>(null)
  const [tray, setTray] = useState(false)
  const [prefs, setPrefs] = useState<DesktopPreferences | null>(null)
  const [systemBar, setSystemBar] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let live = true
    const keep =
      <T,>(set: (value: T) => void) =>
      (value: T): void => {
        if (live) set(value)
      }
    desktopInfo()
      .then((info) => keep(setPlatform)(info.platform))
      .catch(() => {})
    trayAvailable()
      .then(keep(setTray))
      .catch(() => {})
    desktopPreferences()
      .then(keep(setPrefs))
      .catch(() => {})
    systemTitleBar()
      .then(keep(setSystemBar))
      .catch(() => {})
    return () => {
      live = false
    }
  }, [])

  async function change(act: () => Promise<void>, fallback: string): Promise<void> {
    setError(null)
    try {
      await act()
    } catch (cause) {
      setError(line(cause, fallback))
    }
  }

  function close(action: CloseAction): void {
    void change(async () => {
      await setCloseAction(action)
      setPrefs((was) => (was === null ? was : { ...was, close: action }))
    }, 'Closing the window could not be changed.')
  }

  function startAtLogin(on: boolean): void {
    void change(async () => {
      await setStartAtLogin(on)
      setPrefs((was) => (was === null ? was : { ...was, start_at_login: on }))
    }, 'Start at login could not be changed.')
  }

  function chooseSystemBar(on: boolean): void {
    void change(async () => {
      setSystemBar(await setSystemTitleBar(on))
    }, 'The title bar could not be changed.')
  }

  function saveShortcuts(overrides: ShortcutOverride[]): void {
    void change(() => setShortcuts(overrides), 'The shortcuts could not be saved.')
  }

  return (
    <section id={DESKTOP_SECTION} aria-labelledby="settings-desktop-title" className="flex flex-col gap-6">
      <h2 id="settings-desktop-title" className="text-base font-medium text-strong">
        Desktop
      </h2>
      {error !== null && <Banner tone="danger">{error}</Banner>}

      <Part title="Close the window">
        <div role="radiogroup" aria-label="Close the window" className="flex flex-col gap-2">
          <label className={ROW}>
            <input
              type="radio"
              name="close-the-window"
              className={CHECKBOX}
              checked={prefs?.close === 'quit'}
              onChange={() => close('quit')}
            />
            Close completely
          </label>
          {tray && (
            <label className={ROW}>
              <input
                type="radio"
                name="close-the-window"
                className={CHECKBOX}
                checked={prefs?.close === 'tray'}
                onChange={() => close('tray')}
              />
              Keep running in the tray
            </label>
          )}
        </div>
      </Part>

      <div className="flex flex-col gap-3">
        <label className={ROW}>
          <input
            type="checkbox"
            className={CHECKBOX}
            checked={prefs?.start_at_login ?? false}
            onChange={(event) => startAtLogin(event.target.checked)}
          />
          Start at login
        </label>
        {platform === 'linux' && (
          <label className={ROW}>
            <input
              type="checkbox"
              className={CHECKBOX}
              checked={systemBar}
              onChange={(event) => chooseSystemBar(event.target.checked)}
            />
            Use the system title bar
          </label>
        )}
      </div>

      <Part id={SERVING_SECTION} title="Serve the web UI">
        <Serving
          onServingApplying={onServingApplying}
          onServingChanged={onServingChanged}
          problem={servingProblem}
        />
      </Part>

      <Part title="Keyboard shortcuts">
        <Shortcuts editable onChange={saveShortcuts} />
      </Part>

      <Part id={INSTANCES_SECTION} title="Instances">
        <Instances embedded list={list} current={current} onChanged={onInstancesChanged} />
      </Part>
    </section>
  )
}

export default DesktopSettings
