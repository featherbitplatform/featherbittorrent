/**
 * The desktop app's own title bar, and the frame that puts it above the page.
 *
 * The window has no system title bar (see `window.rs` in the shell): on macOS the system's title strip
 * is transparent and untitled, and the native traffic lights sit over this bar's left end, which keeps
 * 78px free for them. On Windows and Linux this bar draws Minimize, Maximize (Restore when the window
 * is maximized) and Close on the right. Empty bar space carries `data-tauri-drag-region`: pressing
 * there moves the window, and a double click maximizes or restores it (zooms, on macOS), which Tauri's
 * own drag script does. With the system title bar switched on (Linux only: Windows always draws its
 * own) the bar is a plain toolbar: the switcher and the actions, no controls and no dragging, which
 * the system's own title bar above it does.
 *
 * {@link DesktopChrome} is the frame: the bar on top, the page below in the space that is left, and
 * the bar's slots handed to the page through {@link TitleBarSlotsContext}, so the header can put its
 * parts in the bar instead of drawing a strip of its own.
 *
 * @module desktop/TitleBar
 */
import { useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { Copy, Minus, Square, X } from 'lucide-react'
import { TitleBarSlotsContext } from '../components/titleBarSlots'
import type { TitleBarSlots } from '../components/titleBarSlots'
import { currentWindow, desktopInfo, onSystemTitleBar, systemTitleBar } from './bridge'
import type { DesktopInfo } from './bridge'

/** Room for the three traffic lights at the left of a macOS bar (`trafficLightPosition` in `tauri.conf.json` puts them at x 16). */
const TRAFFIC_LIGHTS_INSET = 78

export interface TitleBarProps {
  platform: DesktopInfo['platform']
  /** The system's title bar is shown above: draw a plain toolbar, no window controls and no drag region. */
  systemTitleBar: boolean
  instanceSwitcher: ReactNode
  actions: ReactNode
}

/** Whether the window is maximized now, following it as it is resized. */
function useMaximized(enabled: boolean): boolean {
  const [maximized, setMaximized] = useState(false)
  useEffect(() => {
    if (!enabled) return
    let live = true
    let stop: (() => void) | null = null
    const window = currentWindow()
    const probe = () => {
      void window
        .isMaximized()
        .then((now) => {
          if (live) setMaximized(now)
        })
        .catch(() => {})
    }
    probe()
    void window
      .onResized(probe)
      .then((unlisten) => {
        if (live) stop = unlisten
        else unlisten()
      })
      .catch(() => {})
    return () => {
      live = false
      stop?.()
    }
  }, [enabled])
  return maximized
}

/** One of the drawn window controls: the full bar height, 46px wide, the icon centred. */
function Control({
  label,
  danger = false,
  onClick,
  children,
}: {
  label: string
  danger?: boolean
  onClick(): void
  children: ReactNode
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className={
        'inline-flex h-full w-[46px] items-center justify-center text-muted transition-colors ' +
        (danger ? 'hover:bg-error hover:text-strong' : 'hover:bg-ink-700 hover:text-strong')
      }
    >
      {children}
    </button>
  )
}

function WindowControls() {
  const maximized = useMaximized(true)
  const window = () => currentWindow()
  return (
    <div className="flex h-full shrink-0 items-stretch">
      <Control label="Minimize" onClick={() => void window().minimize()}>
        <Minus className="h-4 w-4" aria-hidden="true" />
      </Control>
      <Control label={maximized ? 'Restore' : 'Maximize'} onClick={() => void window().toggleMaximize()}>
        {maximized ? (
          <Copy className="h-3.5 w-3.5" aria-hidden="true" />
        ) : (
          <Square className="h-3.5 w-3.5" aria-hidden="true" />
        )}
      </Control>
      <Control label="Close" danger onClick={() => void window().close()}>
        <X className="h-4 w-4" aria-hidden="true" />
      </Control>
    </div>
  )
}

export function TitleBar({ platform, systemTitleBar, instanceSwitcher, actions }: TitleBarProps) {
  // One tree for both modes: switching the system title bar changes attributes and the edge pieces,
  // never the elements around `instanceSwitcher` and `actions`, so what is portalled into them (an open
  // switcher menu, say) stays mounted.
  const drawn = !systemTitleBar
  const mac = drawn && platform === 'macos'
  const drag = drawn ? '' : undefined
  return (
    <div
      role="toolbar"
      aria-label="Window"
      data-tauri-drag-region={drag}
      className={
        'flex h-12 shrink-0 select-none items-center border-b border-rule bg-sunken ' +
        (!drawn ? 'gap-4 px-4' : mac ? 'pr-4' : 'pl-4')
      }
    >
      {mac && (
        <div
          data-traffic-lights
          data-tauri-drag-region
          className="h-full shrink-0"
          style={{ width: `${TRAFFIC_LIGHTS_INSET}px` }}
        />
      )}
      <div data-tauri-drag-region={drawn ? 'deep' : undefined} className="flex min-w-0 items-center gap-4">
        {instanceSwitcher}
      </div>
      <div data-title-bar-spacer data-tauri-drag-region={drag} className="h-full min-w-4 flex-1" />
      <div className="flex shrink-0 items-center gap-4">{actions}</div>
      {drawn && !mac && (
        <>
          <div data-tauri-drag-region className="h-full w-3 shrink-0" />
          <WindowControls />
        </>
      )}
    </div>
  )
}

/** Whether the shell shows the system's title bar above the page, following the setting when it changes. */
function useSystemTitleBar(): boolean {
  const [system, setSystem] = useState(false)
  useEffect(() => {
    let live = true
    let stop: (() => void) | null = null
    void systemTitleBar()
      .then((now) => {
        if (live) setSystem(now)
      })
      .catch(() => {})
    void onSystemTitleBar((now) => setSystem(now))
      .then((unlisten) => {
        if (live) stop = unlisten
        else unlisten()
      })
      .catch(() => {})
    return () => {
      live = false
      stop?.()
    }
  }, [])
  return system
}

/**
 * The system this page runs on, read from the user agent: what the bar is drawn for until the shell's
 * `desktop_info` answers, and for good if it never does, so an undecorated window always has its
 * controls and somewhere to drag.
 */
function guessPlatform(): DesktopInfo['platform'] {
  const agent = typeof navigator === 'undefined' ? '' : navigator.userAgent
  if (/Mac/.test(agent)) return 'macos'
  if (/Windows/.test(agent)) return 'windows'
  return 'linux'
}

/**
 * The bar above `children`, with the bar's slots provided to them. `children` gets the rest of the
 * window: a screen that sizes itself to the viewport (`h-dvh`, `min-h-dvh`) is sized to that space.
 */
export function DesktopChrome({ children }: { children: ReactNode }) {
  const [platform, setPlatform] = useState<DesktopInfo['platform']>(guessPlatform)
  const system = useSystemTitleBar()
  const [switcher, setSwitcher] = useState<HTMLElement | null>(null)
  const [lead, setLead] = useState<HTMLElement | null>(null)
  const [actions, setActions] = useState<HTMLElement | null>(null)
  const slots = useMemo<TitleBarSlots>(() => ({ switcher, lead, actions }), [switcher, lead, actions])

  useEffect(() => {
    let live = true
    void desktopInfo()
      .then((info) => {
        if (live) setPlatform(info.platform)
      })
      .catch(() => {})
    return () => {
      live = false
    }
  }, [])

  return (
    <TitleBarSlotsContext.Provider value={slots}>
      <div className="flex h-dvh flex-col overflow-hidden bg-canvas">
        <TitleBar
          platform={platform}
          systemTitleBar={system}
          instanceSwitcher={
            <>
              <div ref={setSwitcher} className="contents" />
              <div ref={setLead} className="contents" />
            </>
          }
          actions={<div ref={setActions} className="contents" />}
        />
        <div className="relative flex min-h-0 flex-1 flex-col overflow-auto [&_.h-dvh]:h-full [&_.min-h-dvh]:min-h-full">
          {children}
        </div>
      </div>
    </TitleBarSlotsContext.Provider>
  )
}
