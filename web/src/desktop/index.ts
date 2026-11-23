/**
 * The desktop app's part of the page, loaded only inside the shell (`main.tsx` imports it dynamically
 * when Tauri's internals are on `window`), so the browser build never fetches it or `@tauri-apps/api`.
 *
 * @module desktop
 */
export { DesktopRoot } from './DesktopRoot'
export { DesktopChrome, TitleBar } from './TitleBar'
export {
  connect,
  desktopInfo,
  inDesktop,
  LOCAL_ID,
  onSystemTitleBar,
  setSystemTitleBar,
  systemTitleBar,
} from './bridge'
export type { DesktopInfo } from './bridge'
