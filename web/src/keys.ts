/**
 * Every keyboard shortcut the web UI answers, in one list. The key handlers decide from this list
 * (through `matches`) and the Shortcuts section of Settings draws it, so the two cannot drift apart.
 *
 * `keys` are `KeyboardEvent.key` values. Escape closing a modal dialog is the browser's own close
 * request (see `modal.ts`), so it is listed here for the Settings screen and answered by the dialog's
 * `cancel` event rather than by a key handler.
 *
 * @module keys
 */
export interface KeyBinding {
  keys: string[]
  action: string
  where: 'Player' | 'Torrent list' | 'Detail tabs' | 'Menus' | 'Dialogs'
}

export const KEY_BINDINGS: readonly KeyBinding[] = [
  { where: 'Player', action: 'Play or pause', keys: [' ', 'k'] },
  { where: 'Player', action: 'Seek back', keys: ['ArrowLeft'] },
  { where: 'Player', action: 'Seek forward', keys: ['ArrowRight'] },
  { where: 'Player', action: 'Volume up', keys: ['ArrowUp'] },
  { where: 'Player', action: 'Volume down', keys: ['ArrowDown'] },
  { where: 'Player', action: 'Full screen', keys: ['f'] },
  { where: 'Player', action: 'Mute or unmute', keys: ['m'] },
  { where: 'Player', action: 'Next audio track', keys: ['a'] },
  { where: 'Player', action: 'Next subtitle track', keys: ['c'] },
  { where: 'Torrent list', action: 'Select the focused torrent', keys: ['Enter', ' '] },
  { where: 'Torrent list', action: 'Move to the next torrent', keys: ['ArrowDown'] },
  { where: 'Torrent list', action: 'Move to the previous torrent', keys: ['ArrowUp'] },
  { where: 'Detail tabs', action: 'Next tab', keys: ['ArrowRight'] },
  { where: 'Detail tabs', action: 'Previous tab', keys: ['ArrowLeft'] },
  { where: 'Detail tabs', action: 'First tab', keys: ['Home'] },
  { where: 'Detail tabs', action: 'Last tab', keys: ['End'] },
  { where: 'Menus', action: 'Open the menu', keys: ['ArrowDown', 'Enter', ' '] },
  { where: 'Menus', action: 'Choose the focused option', keys: ['Enter', ' '] },
  { where: 'Menus', action: 'Next option', keys: ['ArrowDown'] },
  { where: 'Menus', action: 'Previous option', keys: ['ArrowUp'] },
  { where: 'Menus', action: 'First option', keys: ['Home'] },
  { where: 'Menus', action: 'Last option', keys: ['End'] },
  { where: 'Menus', action: 'Close the menu', keys: ['Escape'] },
  { where: 'Dialogs', action: 'Close the dialog', keys: ['Escape'] },
]

/** A replacement key list for one default binding, as the desktop shell's settings file carries it. */
export interface ShortcutOverride {
  where: KeyBinding['where']
  action: string
  keys: string[]
}

let effective: readonly KeyBinding[] = KEY_BINDINGS

function sameKeys(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((key, index) => key === b[index])
}

/** The bindings the handlers answer to now: the defaults with the valid overrides applied. */
export function effectiveBindings(): readonly KeyBinding[] {
  return effective
}

/** The action in the same group, other than `action`, that already holds `key`; otherwise null. */
export function conflictFor(where: KeyBinding['where'], action: string, key: string): string | null {
  const other = effective.find(
    (binding) => binding.where === where && binding.action !== action && binding.keys.includes(key),
  )
  return other === undefined ? null : other.action
}

/**
 * Apply overrides over the defaults. A file can be edited by hand, so anything that does not fit is
 * ignored and that binding keeps its default: an unknown group or action, keys that are not a
 * non-empty list of non-empty strings, a repeated key, or a key another action in the group holds.
 */
export function setOverrides(overrides: ShortcutOverride[]): void {
  effective = KEY_BINDINGS.map((binding) => ({ ...binding, keys: [...binding.keys] }))
  if (!Array.isArray(overrides)) return
  for (const entry of overrides as unknown[]) {
    if (typeof entry !== 'object' || entry === null) continue
    const { where, action, keys } = entry as Record<string, unknown>
    if (typeof where !== 'string' || typeof action !== 'string' || !Array.isArray(keys)) continue
    if (keys.length === 0 || !keys.every((key) => typeof key === 'string' && key.length > 0)) continue
    const list = keys as string[]
    if (new Set(list).size !== list.length) continue
    const index = effective.findIndex((binding) => binding.where === where && binding.action === action)
    if (index < 0) continue
    const target = effective[index] as KeyBinding
    if (list.some((key) => conflictFor(target.where, target.action, key) !== null)) continue
    effective = effective.map((binding, at) => (at === index ? { ...binding, keys: list } : binding))
  }
}

/** The bindings whose keys differ from the defaults, in the shape `setOverrides` takes. */
export function currentOverrides(): ShortcutOverride[] {
  const out: ShortcutOverride[] = []
  for (const binding of effective) {
    const base = KEY_BINDINGS.find((b) => b.where === binding.where && b.action === binding.action)
    if (base !== undefined && !sameKeys(base.keys, binding.keys)) {
      out.push({ where: binding.where, action: binding.action, keys: [...binding.keys] })
    }
  }
  return out
}

/** Whether `event` is one of the keys the list gives to `action` in the `where` group. */
export function matches(
  where: KeyBinding['where'],
  action: string,
  event: { key: string },
): boolean {
  return effective.some(
    (binding) => binding.where === where && binding.action === action && binding.keys.includes(event.key),
  )
}
