/**
 * The page's side of the desktop shell: the commands it calls through Tauri's `invoke`.
 *
 * This is the only module that imports `@tauri-apps/api`, and it is only ever reached through the
 * dynamic `import('./desktop')` in `main.tsx`, so a browser loading the page never fetches it. The
 * shell answers with plain data; turning that into the page's own types (an `Endpoint` on a normalised
 * base) happens here, so nothing past this file knows what the shell's answers look like.
 *
 * @module desktop/bridge
 */
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import type { Window } from '@tauri-apps/api/window'
import { normaliseBase } from '../endpoint'
import type { Endpoint } from '../endpoint'

/** The shell's id for This computer, the app's own engine, and the instance the page opens on until saved instances arrive. */
export const LOCAL_ID = 'local'

/** This computer's name, wherever the app shows it. */
export const THIS_COMPUTER = 'This computer'

/** What the shell's `connect` answers: an absolute base address and the bearer token to present there, or null when the page has to sign in. */
interface EndpointAnswer {
  kind: 'bearer'
  base: string
  token: string | null
}

/** The app's version, and the system it runs on. */
export interface DesktopInfo {
  version: string
  platform: 'macos' | 'windows' | 'linux'
}

/** Whether this page runs inside the desktop app: Tauri puts its internals on `window` before any script runs. */
export function inDesktop(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

/**
 * The endpoint for instance `id`. For This computer that is its loopback address and this launch's
 * secret, asked for each time rather than kept anywhere: an engine that restarted has a new one. For a
 * saved instance it is its address, with a token when the shell signed in with a saved password and
 * null when the page has to ask for one. A refusal (no such instance, the engine is not running, the
 * server gives no answer) rejects with the shell's line.
 */
export async function connect(id: string): Promise<Endpoint> {
  const answer = await invoke<EndpointAnswer>('connect', { id })
  return { kind: 'bearer', base: normaliseBase(answer.base), token: answer.token }
}

/** The app's version and platform. */
export async function desktopInfo(): Promise<DesktopInfo> {
  return invoke<DesktopInfo>('desktop_info')
}

/** Whether the page draws a bare toolbar under the system's title bar rather than its own bar with window controls. Only on Linux. */
export async function systemTitleBar(): Promise<boolean> {
  return invoke<boolean>('system_title_bar')
}

/** Saves the "Use the system title bar" setting and puts the matching chrome on the window; answers what {@link systemTitleBar} now answers. On Windows and macOS it does nothing and answers `false`. */
export async function setSystemTitleBar(on: boolean): Promise<boolean> {
  return invoke<boolean>('set_system_title_bar', { on })
}

/** Calls `changed` whenever the shell switches the title bar; resolves to the function that stops listening. */
export async function onSystemTitleBar(changed: (system: boolean) => void): Promise<() => void> {
  return listen<boolean>('system-title-bar', (event) => changed(event.payload))
}

/** The window this page is in, for the title bar's own controls. */
export function currentWindow(): Window {
  return getCurrentWindow()
}

/** One instance the switcher lists. This computer's `url` is empty: its address changes every launch. */
export interface InstanceItem {
  id: string
  name: string
  url: string
  saved_password: boolean
}

/** The instance list: This computer first, the instance the app opens on, and whether this system can save passwords. */
export interface InstancesList {
  default: string
  items: InstanceItem[]
  keychain: boolean
}

/** What the shell found at a saved instance's address: anything answering, and a CORS answer naming this app's origin. */
export interface Probe {
  reachable: boolean
  cors_ok: boolean
}

/** Every instance the app can switch to. */
export async function listInstances(): Promise<InstancesList> {
  const list = await invoke<InstancesList>('list_instances')
  return { ...list, items: list.items.map((item) => ({ ...item, url: normaliseBase(item.url) })) }
}

/** Saves a new instance; rejects with the line saying why it was refused. */
export async function addInstance(name: string, url: string): Promise<InstanceItem> {
  return invoke<InstanceItem>('add_instance', { name, url })
}

/** Removes a saved instance and its saved password. */
export async function removeInstance(id: string): Promise<void> {
  await invoke('remove_instance', { id })
}

/** Makes an instance the one the app opens on. */
export async function setDefaultInstance(id: string): Promise<void> {
  await invoke('set_default_instance', { id })
}

/** Saves a password in the system keychain, for the shell to sign in with next time. */
export async function savePassword(id: string, user: string, password: string): Promise<void> {
  await invoke('save_password', { id, user, password })
}

/** Forgets a saved password. */
export async function forgetPassword(id: string): Promise<void> {
  await invoke('forget_password', { id })
}

/** Asks the shell whether a saved instance answers, and whether it lets this app's origin in. */
export async function probeInstance(id: string): Promise<Probe> {
  return invoke<Probe>('probe_instance', { id })
}

/** What closing the window does once the first-close question is answered. */
export type CloseAction = 'quit' | 'tray'

/** The shell's event when closing the window needs the page's answer first. */
export const CLOSE_REQUESTED_EVENT = 'close-requested'

/** Saves what closing the window does from now on. */
export async function setCloseAction(action: CloseAction): Promise<void> {
  await invoke('set_close_action', { action })
}

/** Closes the window the way the saved choice says: quit, or hide in the tray. */
export async function closeWindow(): Promise<void> {
  await invoke('close_window_now')
}

/** Whether this system gave the app a tray to keep running in. */
export async function trayAvailable(): Promise<boolean> {
  return invoke<boolean>('tray_available')
}

/** Turns Start at login on or off; rejects with the shell's line when the login item cannot change. */
export async function setStartAtLogin(on: boolean): Promise<void> {
  await invoke('set_start_at_login', { on })
}

/** Names the connected instance in the tray menu. */
export async function setTrayInstance(name: string): Promise<void> {
  await invoke('set_tray_instance', { name })
}

/** Calls `handler` each time closing the window asks the page first; resolves to the function that stops listening. */
export async function onCloseRequested(handler: () => void): Promise<() => void> {
  return listen(CLOSE_REQUESTED_EVENT, () => handler())
}
