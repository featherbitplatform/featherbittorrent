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
import { normaliseBase } from '../endpoint'
import type { Endpoint } from '../endpoint'

/** The shell's id for This computer, the app's own engine, and the instance the page opens on until saved instances arrive. */
export const LOCAL_ID = 'local'

/** This computer's name, wherever the app shows it. */
export const THIS_COMPUTER = 'This computer'

/** What the shell's `connect` answers: an absolute base address and the bearer token to present there. */
interface EndpointAnswer {
  kind: 'bearer'
  base: string
  token: string
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
 * secret, asked for each time rather than kept anywhere: an engine that restarted has a new one. A
 * refusal (no such instance, the engine is not running) rejects with the shell's line.
 */
export async function connect(id: string): Promise<Endpoint> {
  const answer = await invoke<EndpointAnswer>('connect', { id })
  return { kind: 'bearer', base: normaliseBase(answer.base), token: answer.token }
}

/** The app's version and platform. */
export async function desktopInfo(): Promise<DesktopInfo> {
  return invoke<DesktopInfo>('desktop_info')
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
