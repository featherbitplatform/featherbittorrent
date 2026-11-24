/**
 * The instance switcher: a button naming the instance the window is connected to, and a menu of every
 * instance the app can switch to, This computer first, with a mark on the connected one and a
 * `Manage instances` entry at the end.
 *
 * The open menu opts out of the title bar's window dragging (`data-tauri-drag-region="false"`): it
 * hangs below the bar inside the bar's "deep" drag group, and its padding and separator are not bar.
 *
 * It is self-contained: it takes the list and says what was picked, and owns nothing but whether its
 * menu is open, so the title bar can place it wherever it has room.
 *
 * @module desktop/InstanceSwitcher
 */
import { useEffect, useId, useRef, useState } from 'react'
import { Check, ChevronDown, Settings2 } from 'lucide-react'
import type { InstanceItem } from './bridge'
import { inSwitcherOrder } from './instanceOrder'

export interface InstanceSwitcherProps {
  /** Every instance, in the shell's order; This computer is put first whatever that order is. */
  instances: InstanceItem[]
  /** The id of the instance the window is on now. */
  current: string
  onSwitch(id: string): void
  onManage(): void
}

const ITEM =
  'flex min-h-9 w-full items-center gap-2 rounded-control px-2.5 text-left text-[13px] text-body ' +
  'outline-none hover:bg-ink-700 focus-visible:bg-ink-700'

export function InstanceSwitcher({ instances, current, onSwitch, onManage }: InstanceSwitcherProps) {
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  const menuId = useId()
  const ordered = inSwitcherOrder(instances)
  const connected = ordered.find((item) => item.id === current)

  // A click anywhere else, or Escape, closes the menu.
  useEffect(() => {
    if (!open) return
    const onPointer = (event: PointerEvent): void => {
      if (!root.current?.contains(event.target as Node)) setOpen(false)
    }
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') setOpen(false)
    }
    document.addEventListener('pointerdown', onPointer)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('pointerdown', onPointer)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])

  function pick(act: () => void): void {
    setOpen(false)
    act()
  }

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        className="flex min-h-8 max-w-64 items-center gap-1.5 rounded-control px-2.5 text-[13px] font-medium text-strong hover:bg-ink-700"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label={`Instance: ${connected?.name ?? 'This computer'}`}
        onClick={() => setOpen((was) => !was)}
      >
        <span className="truncate">{connected?.name ?? 'This computer'}</span>
        <ChevronDown className="h-3.5 w-3.5 shrink-0 text-muted" aria-hidden="true" />
      </button>
      {open && (
        <div
          id={menuId}
          role="menu"
          aria-label="Instances"
          // The title bar's group around the switcher drags the window from anywhere inside it ("deep");
          // the open menu is not part of the bar, so pressing its padding or separator must not drag.
          data-tauri-drag-region="false"
          className="absolute left-0 top-full z-50 mt-1 flex w-72 flex-col gap-0.5 rounded-overlay border border-rule bg-raised p-1 shadow-lg"
        >
          {ordered.map((item) => (
            <button
              key={item.id}
              type="button"
              role="menuitemradio"
              aria-checked={item.id === current}
              className={ITEM}
              onClick={() => pick(() => onSwitch(item.id))}
            >
              <Check
                className={`h-3.5 w-3.5 shrink-0 ${item.id === current ? 'text-accent-light' : 'invisible'}`}
                aria-hidden="true"
              />
              <span className="flex min-w-0 flex-col">
                <span className="truncate text-strong">{item.name}</span>
                {item.url !== '' && <span className="truncate font-mono text-xs text-muted">{item.url}</span>}
              </span>
            </button>
          ))}
          <div role="separator" className="my-1 border-t border-rule" />
          <button type="button" role="menuitem" className={ITEM} onClick={() => pick(onManage)}>
            <Settings2 className="h-3.5 w-3.5 shrink-0 text-muted" aria-hidden="true" />
            Manage instances
          </button>
        </div>
      )}
    </div>
  )
}

export default InstanceSwitcher
