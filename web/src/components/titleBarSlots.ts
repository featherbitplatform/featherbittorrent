/**
 * Where parts of the page go when the desktop app draws its own title bar.
 *
 * In a browser this context is `null` and the header draws itself as it always has. Inside the desktop
 * app, `DesktopChrome` (in `desktop/TitleBar.tsx`) provides the bar's three slots as DOM nodes, and the
 * header portals its own parts into them: the mark and the link pill into `lead`, the rates and the
 * buttons into `actions`. The instance switcher goes into `switcher`. The context lives here, outside
 * `desktop/`, because the header is part of the browser bundle and must not import anything that
 * reaches the Tauri API.
 *
 * A slot is `null` until the bar has mounted it; a part with nowhere to go yet draws nothing.
 *
 * @module components/titleBarSlots
 */
import { createContext, useContext } from 'react'

export interface TitleBarSlots {
  /** Beside the window controls on the left: the instance switcher. */
  switcher: HTMLElement | null
  /** After the switcher: what this page is and whether its server answers. */
  lead: HTMLElement | null
  /** On the right, before the window controls: the header's actions. */
  actions: HTMLElement | null
}

export const TitleBarSlotsContext = createContext<TitleBarSlots | null>(null)

/** The desktop bar's slots, or `null` in a browser. */
export function useTitleBarSlots(): TitleBarSlots | null {
  return useContext(TitleBarSlotsContext)
}
