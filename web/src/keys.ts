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

/** Whether `event` is one of the keys the list gives to `action` in the `where` group. */
export function matches(
  where: KeyBinding['where'],
  action: string,
  event: { key: string },
): boolean {
  return KEY_BINDINGS.some(
    (binding) => binding.where === where && binding.action === action && binding.keys.includes(event.key),
  )
}
