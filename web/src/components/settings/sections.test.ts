// @vitest-environment jsdom
/**
 * Which Settings sections are listed and which one opens.
 *
 * @module components/settings/sections.test
 */
import { afterEach, expect, it, vi } from 'vitest'
import { BUILT_SECTIONS, LAST_SECTION_KEY, resolveSection, visibleSections } from './sections'
import type { SectionId } from './sections'

afterEach(() => {
  vi.unstubAllGlobals()
  window.localStorage.clear()
})

const ids = (desktop: boolean, built: ReadonlySet<SectionId> = BUILT_SECTIONS) =>
  visibleSections({ desktop, built }).map((s) => s.id)

it('visible_sections_browser_has_no_this_app_group', () => {
  const sections = visibleSections({ desktop: false, built: BUILT_SECTIONS })
  expect(sections.some((s) => s.group === 'This app')).toBe(false)
  expect(sections.map((s) => s.id)).toEqual(['downloads', 'speed', 'devices', 'shortcuts'])
})

it('visible_sections_desktop_order', () => {
  expect(ids(true)).toEqual(['downloads', 'speed', 'devices', 'shortcuts', 'window', 'serving', 'instances'])
  const all = new Set<SectionId>([...BUILT_SECTIONS, 'proxy'])
  expect(ids(true, all)).toEqual([
    'downloads', 'speed', 'proxy', 'devices', 'shortcuts', 'window', 'serving', 'instances',
  ])
})

it('legacy_ids_resolve', () => {
  const v = visibleSections({ desktop: true, built: BUILT_SECTIONS })
  expect(resolveSection('settings-desktop-serving', v)).toBe('serving')
  expect(resolveSection('settings-desktop-instances', v)).toBe('instances')
  expect(resolveSection('settings-desktop', v)).toBe('window')
  expect(resolveSection('settings-server', v)).toBe('speed')
  expect(resolveSection('devices', v)).toBe('devices')
})

it('unbuilt_sections_are_not_listed', () => {
  expect(ids(true)).not.toContain('proxy')
  // Nor can a link open one.
  const v = visibleSections({ desktop: true, built: BUILT_SECTIONS })
  expect(resolveSection('proxy', v)).toBe('downloads')
})

it('a_desktop_id_in_a_browser_falls_back', () => {
  const v = visibleSections({ desktop: false, built: BUILT_SECTIONS })
  expect(resolveSection('settings-desktop-serving', v)).toBe('downloads')
})

it('last_section_is_used_when_nothing_is_asked', () => {
  const v = visibleSections({ desktop: false, built: BUILT_SECTIONS })
  window.localStorage.setItem(LAST_SECTION_KEY, 'devices')
  expect(resolveSection(null, v)).toBe('devices')
})

it('last_section_survives_storage_throwing', () => {
  const throwing = {
    getItem: () => {
      throw new Error('blocked')
    },
    setItem: () => {
      throw new Error('blocked')
    },
  }
  vi.stubGlobal('localStorage', throwing)
  expect(resolveSection(null, visibleSections({ desktop: false, built: BUILT_SECTIONS }))).toBe('downloads')
  // Without Downloads built, the first listed section opens.
  const without = new Set<SectionId>([...BUILT_SECTIONS].filter((id) => id !== 'downloads'))
  expect(resolveSection(null, visibleSections({ desktop: false, built: without }))).toBe('speed')
})
