import { describe, expect, it } from 'vitest'
import { KEY_BINDINGS, matches } from './keys'

describe('the key list', () => {
  it('matches only its own group and action', () => {
    expect(matches('Player', 'Play or pause', { key: ' ' }), 'Space did not match play').toBe(true)
    expect(matches('Player', 'Play or pause', { key: 'k' }), 'k did not match play').toBe(true)
    expect(matches('Torrent list', 'Play or pause', { key: 'k' }), 'a group leaked into another').toBe(false)
    expect(matches('Player', 'Play or pause', { key: 'x' }), 'an unlisted key matched').toBe(false)
    expect(matches('Dialogs', 'Close the dialog', { key: 'Escape' }), 'Escape did not match').toBe(true)
  })

  it('has no action twice in one group and no empty copy', () => {
    const seen = new Set<string>()
    for (const binding of KEY_BINDINGS) {
      const id = `${binding.where}/${binding.action}`
      expect(seen.has(id), `${id} is listed twice`).toBe(false)
      seen.add(id)
      expect(binding.keys.length > 0 && binding.action.length > 0, `${id} is empty`).toBe(true)
    }
  })
})
