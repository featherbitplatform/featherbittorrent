/**
 * What Settings is told about the desktop app: nothing in a browser, and inside the app the sections of
 * the `This app` group, the editable keyboard shortcuts, plus a request to open Settings at one of its sections (the
 * switcher's `Manage instances`, or the page coming back after This computer restarted).
 *
 * It lives apart from `desktop/` so Settings and `App` can read it without the browser build reaching for
 * the shell: the default is `null`, which is a browser, and only the desktop page provides a value.
 *
 * @module desktopSettings
 */
import { createContext, useContext } from 'react'
import type { ReactNode } from 'react'

/** A request to open Settings at section `section`: a section id, or one of the old element ids (`LEGACY_IDS`). `seq` tells two requests for the same section apart. */
export interface SettingsRequest {
  section: string
  seq: number
}

export interface DesktopSettingsSlot {
  /** The `This app` sections, each drawn by Settings when it is the open one. */
  sections: Array<{ id: 'window' | 'serving' | 'instances'; render(): ReactNode }>
  /** The editable shortcut list, which replaces the browser's read-only one in the Keyboard shortcuts section. */
  shortcuts: ReactNode
  /** Open Settings at this section, or null for nothing asked. */
  request: SettingsRequest | null
  /** Settings opened for `request`: the page that asked can forget it. */
  onOpened(seq: number): void
}

export const DesktopSettingsContext = createContext<DesktopSettingsSlot | null>(null)

/** The desktop part of Settings, or null in a browser. */
export function useDesktopSettings(): DesktopSettingsSlot | null {
  return useContext(DesktopSettingsContext)
}
