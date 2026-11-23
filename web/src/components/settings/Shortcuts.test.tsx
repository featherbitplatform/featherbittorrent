// @vitest-environment jsdom
import { cleanup, render } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { KEY_BINDINGS } from '../../keys'
import { Shortcuts } from './Shortcuts'

afterEach(() => {
  cleanup()
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
    for (const where of ['Player', 'Torrent list', 'Detail tabs', 'Dialogs']) {
      expect(
        Array.from(container.querySelectorAll('h3')).some((heading) => heading.textContent === where),
        `no group heading for ${where}`,
      ).toBe(true)
    }
    expect(container.textContent?.includes('For a keyboard.'), 'the note showed on a wide screen').toBe(false)
  })

  it('on a phone the section says it is for a keyboard', () => {
    stubMedia((query) => query === '(max-width: 640px)')
    const { container } = render(<Shortcuts />)
    expect(container.textContent?.includes('For a keyboard.'), 'the phone note is missing').toBe(true)
  })
})
