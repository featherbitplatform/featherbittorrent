import { afterEach, describe, expect, it } from 'vitest'
import { conflictFor, effectiveBindings, KEY_BINDINGS, matches, setOverrides } from './keys'

afterEach(() => setOverrides([]))

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

describe('overrides', () => {
  it('override_changes_what_matches_answers', () => {
    setOverrides([{ where: 'Player', action: 'Full screen', keys: ['g'] }])
    expect(matches('Player', 'Full screen', { key: 'g' })).toBe(true)
    expect(matches('Player', 'Full screen', { key: 'f' })).toBe(false)
  })

  it('effective_bindings_show_the_override', () => {
    setOverrides([{ where: 'Player', action: 'Full screen', keys: ['g'] }])
    const row = effectiveBindings().find((b) => b.where === 'Player' && b.action === 'Full screen')
    expect(row?.keys).toEqual(['g'])
    expect(KEY_BINDINGS.find((b) => b.action === 'Full screen')?.keys).toEqual(['f'])
  })

  it('conflict_names_the_action_in_the_same_group', () => {
    expect(conflictFor('Player', 'Full screen', 'm')).toBe('Mute or unmute')
    expect(conflictFor('Player', 'Full screen', 'f')).toBe(null)
  })

  it('same_key_in_another_group_is_allowed', () => {
    expect(conflictFor('Player', 'Full screen', 'Home')).toBe(null)
    setOverrides([{ where: 'Player', action: 'Full screen', keys: ['Home'] }])
    expect(matches('Player', 'Full screen', { key: 'Home' })).toBe(true)
  })

  it('stale_and_invalid_overrides_fall_back_to_defaults', () => {
    setOverrides([
      { where: 'Player', action: 'No such action', keys: ['g'] },
      { where: 'Player', action: 'Full screen', keys: [] },
      { where: 'Player', action: 'Mute or unmute', keys: ['f', 'f'] },
      { where: 'Player', action: 'Next audio track', keys: ['m'] },
      { where: 'Nowhere', action: 'Full screen', keys: ['g'] },
      { where: 'Player', action: 'Next subtitle track', keys: [3] },
      null,
    ] as never)
    expect(effectiveBindings()).toEqual(KEY_BINDINGS)
  })
})
