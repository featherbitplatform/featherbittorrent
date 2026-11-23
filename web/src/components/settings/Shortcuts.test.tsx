// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { KEY_BINDINGS, matches, setOverrides } from '../../keys'
import { Shortcuts } from './Shortcuts'

afterEach(() => {
  cleanup()
  setOverrides([])
  vi.unstubAllGlobals()
})

function stubMedia(matchesQuery: (query: string) => boolean): void {
  vi.stubGlobal('matchMedia', (query: string) => ({
    matches: matchesQuery(query),
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }))
}

describe('the Shortcuts section', () => {
  it('the screen lists every binding', () => {
    stubMedia(() => false)
    const { container } = render(<Shortcuts />)
    const rows = container.querySelectorAll('[data-shortcut]')
    expect(rows.length === KEY_BINDINGS.length, `${rows.length} rows for ${KEY_BINDINGS.length} bindings`).toBe(true)
    for (const binding of KEY_BINDINGS) {
      expect(
        container.querySelector(`[data-shortcut="${binding.where}/${binding.action}"]`) !== null,
        `${binding.action} is not listed`,
      ).toBe(true)
    }
    for (const where of ['Player', 'Torrent list', 'Detail tabs', 'Menus', 'Dialogs']) {
      expect(
        Array.from(container.querySelectorAll('h3')).some((heading) => heading.textContent === where),
        `no group heading for ${where}`,
      ).toBe(true)
    }
    expect(container.textContent?.includes('For a keyboard.'), 'the note showed on a wide screen').toBe(false)
  })

  it('the keys read as glyphs and capitals', () => {
    stubMedia(() => false)
    const { container } = render(<Shortcuts />)
    const kbds = Array.from(container.querySelectorAll('kbd')).map((node) => node.textContent)
    for (const label of ['←', '→', '↑', '↓', 'Space', 'K', 'F']) {
      expect(kbds.includes(label), `no key is drawn as ${label}`).toBe(true)
    }
    expect(kbds.includes('ArrowLeft') || kbds.includes('k'), 'a key is drawn raw').toBe(false)
  })

  it('on a phone the section says it is for a keyboard', () => {
    stubMedia((query) => query === '(max-width: 640px)')
    const { container } = render(<Shortcuts />)
    expect(container.textContent?.includes('For a keyboard.'), 'the phone note is missing').toBe(true)
  })
})

describe('the editable Shortcuts section', () => {
  it('read_only_list_has_no_change_buttons', () => {
    stubMedia(() => false)
    render(<Shortcuts />)
    expect(screen.queryAllByRole('button', { name: 'Change' }).length).toBe(0)
    expect(screen.queryByRole('button', { name: 'Reset to defaults' })).toBeNull()
  })

  it('editable_list_captures_a_key_and_reports_overrides', () => {
    stubMedia(() => false)
    const onChange = vi.fn()
    const { container } = render(<Shortcuts editable onChange={onChange} />)
    const row = container.querySelector('[data-shortcut="Player/Full screen"]') as HTMLElement
    fireEvent.click(row.querySelector('button') as HTMLElement)
    fireEvent.keyDown(window, { key: 'Escape' })
    expect(onChange).not.toHaveBeenCalled()
    fireEvent.click(row.querySelector('button') as HTMLElement)
    fireEvent.keyDown(window, { key: 'g' })
    expect(onChange).toHaveBeenCalledWith([{ where: 'Player', action: 'Full screen', keys: ['g'] }])
    expect(matches('Player', 'Full screen', { key: 'g' })).toBe(true)
    expect(row.textContent).toContain('G')
    fireEvent.click(row.querySelector('button') as HTMLElement)
    fireEvent.keyDown(window, { key: 'm' })
    expect(container.textContent).toContain('Already used by Mute or unmute')
    expect(onChange).toHaveBeenCalledTimes(1)
  })

  it('reset_restores_defaults_and_reports_empty', () => {
    stubMedia(() => false)
    setOverrides([{ where: 'Player', action: 'Full screen', keys: ['g'] }])
    const onChange = vi.fn()
    render(<Shortcuts editable onChange={onChange} />)
    fireEvent.click(screen.getByRole('button', { name: 'Reset to defaults' }))
    expect(onChange).toHaveBeenCalledWith([])
    expect(matches('Player', 'Full screen', { key: 'f' })).toBe(true)
  })
})
