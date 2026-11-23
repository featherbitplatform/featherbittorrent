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

/** Whether the page draws a bare toolbar under the system's title bar rather than its own bar with window controls. Never on macOS. */
export async function systemTitleBar(): Promise<boolean> {
  return invoke<boolean>('system_title_bar')
}

/** Saves the "Use the system title bar" setting and puts the matching chrome on the window; answers what {@link systemTitleBar} now answers. */
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
