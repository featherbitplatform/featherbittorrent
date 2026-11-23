/**
 * The Shortcuts section of Settings: a read-only list of every keyboard shortcut, drawn from the
 * one list the key handlers read (`keys.ts`).
 *
 * @module components/settings/Shortcuts
 */
import type { JSX } from 'react'
import { KEY_BINDINGS } from '../../keys'
import type { KeyBinding } from '../../keys'

const GROUPS: readonly KeyBinding['where'][] = ['Player', 'Torrent list', 'Detail tabs', 'Menus', 'Dialogs']

const PHONE_QUERY = '(max-width: 640px)'

function keyLabel(key: string): string {
  if (key === ' ') return 'Space'
  const glyphs: Record<string, string> = { ArrowLeft: '←', ArrowRight: '→', ArrowUp: '↑', ArrowDown: '↓' }
  if (key in glyphs) return glyphs[key] as string
  return key.length === 1 ? key.toUpperCase() : key
}

function onPhone(): boolean {
  return typeof window.matchMedia === 'function' && window.matchMedia(PHONE_QUERY).matches
}

export function Shortcuts(): JSX.Element {
  return (
    <div className="flex flex-col gap-4">
      {onPhone() && <p className="text-[13px] text-muted">For a keyboard.</p>}
      {GROUPS.map((where) => (
        <div key={where} className="flex flex-col gap-1">
          <h3 className="text-[12px] uppercase tracking-wide text-muted">{where}</h3>
          <ul className="flex flex-col divide-y divide-ink-700 border-y border-ink-700">
            {KEY_BINDINGS.filter((binding) => binding.where === where).map((binding) => (
              <li
                key={binding.action}
                data-shortcut={`${binding.where}/${binding.action}`}
                className="flex items-center justify-between gap-3 py-2 text-[13px] text-body"
              >
                <span>{binding.action}</span>
                <span className="flex gap-1">
                  {binding.keys.map((key) => (
                    <kbd
                      key={key}
                      className="rounded-control border border-ink-700 bg-sunken px-1.5 py-0.5 font-mono text-[12px] text-strong"
                    >
                      {keyLabel(key)}
                    </kbd>
                  ))}
                </span>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </div>
  )
}
