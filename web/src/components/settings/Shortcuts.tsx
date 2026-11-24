/**
 * The Shortcuts section of Settings: every keyboard shortcut, drawn from the one list the key
 * handlers read (`keys.ts`). Read-only unless `editable`, where each row can be given another key.
 *
 * @module components/settings/Shortcuts
 */
import { useEffect, useState, useSyncExternalStore } from 'react'
import type { JSX } from 'react'
import { conflictFor, currentOverrides, effectiveBindings, setOverrides, subscribeOverrides } from '../../keys'
import type { KeyBinding, ShortcutOverride } from '../../keys'

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

const MODIFIERS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'AltGraph', 'CapsLock', 'Dead'])

interface Props {
  editable?: boolean
  onChange?: (overrides: ShortcutOverride[]) => void
}

export function Shortcuts({ editable = false, onChange }: Props): JSX.Element {
  const [capturing, setCapturing] = useState<string | null>(null)
  const [refusal, setRefusal] = useState<{ id: string; text: string } | null>(null)

  useEffect(() => {
    if (capturing === null) return
    const slash = capturing.indexOf('/')
    const where = capturing.slice(0, slash) as KeyBinding['where']
    const action = capturing.slice(slash + 1)
    // Capture phase on the window, so the key reaches nothing else: Escape here must cancel the
    // capture only, and preventing its default stops the browser closing a surrounding dialog.
    const onKey = (event: KeyboardEvent): void => {
      event.preventDefault()
      event.stopImmediatePropagation()
      if (event.repeat || MODIFIERS.has(event.key)) return
      if (event.key === 'Escape') {
        setCapturing(null)
        return
      }
      setCapturing(null)
      // The handlers ignore modifiers, so a stored Cmd+R would fire on a plain R.
      if (event.ctrlKey || event.metaKey || event.altKey) {
        setRefusal({ id: capturing, text: 'Use a key without Ctrl, Alt or Cmd' })
        return
      }
      const holder = conflictFor(where, action, event.key)
      if (holder !== null) {
        setRefusal({ id: capturing, text: `Already used by ${holder}` })
        return
      }
      const next = currentOverrides().filter((o) => !(o.where === where && o.action === action))
      next.push({ where, action, keys: [event.key] })
      setOverrides(next)
      setRefusal(null)
      onChange?.(currentOverrides())
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [capturing, onChange])

  // Follows every change to the overrides, including ones applied from outside this list.
  const bindings = useSyncExternalStore(subscribeOverrides, effectiveBindings)
  return (
    <div className="flex flex-col gap-4">
      {onPhone() && <p className="text-[13px] text-muted">For a keyboard.</p>}
      {GROUPS.map((where) => (
        <div key={where} className="flex flex-col gap-1">
          <h3 className="text-[12px] uppercase tracking-wide text-muted">{where}</h3>
          <ul className="flex flex-col divide-y divide-ink-700 border-y border-ink-700">
            {bindings
              .filter((binding) => binding.where === where)
              .map((binding) => {
                const id = `${binding.where}/${binding.action}`
                return (
                  <li
                    key={binding.action}
                    data-shortcut={id}
                    className="flex flex-wrap items-center justify-between gap-3 py-2 text-[13px] text-body"
                  >
                    <span>{binding.action}</span>
                    <span className="flex items-center gap-1">
                      {capturing === id ? (
                        <span className="text-muted">Press a key</span>
                      ) : (
                        binding.keys.map((key) => (
                          <kbd
                            key={key}
                            className="rounded-control border border-ink-700 bg-sunken px-1.5 py-0.5 font-mono text-[12px] text-strong"
                          >
                            {keyLabel(key)}
                          </kbd>
                        ))
                      )}
                      {editable && (
                        <button
                          type="button"
                          className="ml-2 rounded-control border border-ink-700 px-2 py-0.5 text-[12px] text-strong"
                          onClick={() => {
                            setRefusal(null)
                            setCapturing(id)
                          }}
                        >
                          Change
                        </button>
                      )}
                    </span>
                    {refusal?.id === id && (
                      <p role="alert" className="basis-full text-[12px] text-error">
                        {refusal.text}
                      </p>
                    )}
                  </li>
                )
              })}
          </ul>
        </div>
      ))}
      {editable && (
        <div>
          <button
            type="button"
            className="rounded-control border border-ink-700 px-3 py-1 text-[13px] text-strong"
            onClick={() => {
              setCapturing(null)
              setRefusal(null)
              setOverrides([])
              onChange?.([])
            }}
          >
            Reset to defaults
          </button>
        </div>
      )}
    </div>
  )
}
