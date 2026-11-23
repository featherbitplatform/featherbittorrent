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
