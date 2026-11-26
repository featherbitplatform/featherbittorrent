/**
 * The sections of the Settings page: which exist, in what order, and which one opens.
 *
 * Settings shows one section at a time with a list on the left (design spec 6). A section appears only
 * once it is built (`built`), so there are no greyed-out placeholders, and the desktop app's own group
 * appears only inside the app. The page opens on a section a link asked for (the old element ids of the
 * single long page still resolve, `LEGACY_IDS`), else the last one used, else Downloads.
 *
 * @module components/settings/sections
 */

export type SectionId =
  | 'downloads'
  | 'speed'
  | 'proxy'
  | 'devices'
  | 'shortcuts'
  | 'window'
  | 'serving'
  | 'instances'

export type Group = 'Torrents' | 'Access' | 'This app'

export interface SectionDef {
  id: SectionId
  title: string
  group: Group
}

/** Every section there will be, in the order of the design spec's section 6. */
const ALL: SectionDef[] = [
  { id: 'downloads', title: 'Downloads', group: 'Torrents' },
  { id: 'speed', title: 'Speed', group: 'Torrents' },
  { id: 'proxy', title: 'Proxy', group: 'Torrents' },
  { id: 'devices', title: 'Devices', group: 'Access' },
  { id: 'shortcuts', title: 'Keyboard shortcuts', group: 'Access' },
  { id: 'window', title: 'Window', group: 'This app' },
  { id: 'serving', title: 'Serve the web UI', group: 'This app' },
  { id: 'instances', title: 'Instances', group: 'This app' },
]

/** The sections this build has: later rounds add `downloads` and `proxy`. */
export const BUILT_SECTIONS: ReadonlySet<SectionId> = new Set<SectionId>([
  'speed',
  'devices',
  'shortcuts',
  'window',
  'serving',
  'instances',
])

/** The sections to list: built ones, and the `This app` group only inside the desktop app. */
export function visibleSections(opts: { desktop: boolean; built: ReadonlySet<SectionId> }): SectionDef[] {
  return ALL.filter((s) => opts.built.has(s.id) && (opts.desktop || s.group !== 'This app'))
}

/** The element ids the single long page used, which links and the desktop shell still ask for. */
export const LEGACY_IDS: Record<string, SectionId> = {
  'settings-server': 'speed',
  'settings-devices': 'devices',
  'settings-shortcuts': 'shortcuts',
  'settings-desktop': 'window',
  'settings-desktop-serving': 'serving',
  'settings-desktop-instances': 'instances',
}

/** localStorage key of the last section used. Every read and write is in a try/catch. */
export const LAST_SECTION_KEY = 'fbtt.settings.section'

/** The last section used, or null when there is none or storage is unavailable. */
export function readLastSection(): string | null {
  try {
    return window.localStorage.getItem(LAST_SECTION_KEY)
  } catch {
    return null
  }
}

/** Remember the section; storage that is missing or refuses is not a problem. */
export function writeLastSection(id: SectionId): void {
  try {
    window.localStorage.setItem(LAST_SECTION_KEY, id)
  } catch {
    // Private windows and blocked site data: the page works without the memory.
  }
}

/**
 * The section to open: the one asked for (a section id or a legacy element id), else the last used,
 * else Downloads, else the first listed. Only a listed section is ever answered.
 */
export function resolveSection(id: string | null, visible: SectionDef[]): SectionId {
  const listed = new Set(visible.map((s) => s.id as string))
  const asked = id === null ? null : (LEGACY_IDS[id] ?? id)
  for (const candidate of [asked, readLastSection(), 'downloads']) {
    if (candidate !== null && listed.has(candidate)) return candidate as SectionId
  }
  return visible[0]?.id ?? 'downloads'
}
