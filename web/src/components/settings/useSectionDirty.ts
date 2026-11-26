/**
 * How the open Settings section tells the page it holds unsaved changes.
 *
 * The page provides a {@link SectionDirtyContext}; a section calls `useSectionDirty(dirty, save)` and
 * the page asks before switching section, pressing `Back to torrents` or unloading the tab. `save`
 * answers whether the changes were kept (false means a refusal the section already shows).
 *
 * @module components/settings/useSectionDirty
 */
import { createContext, useContext, useEffect, useRef } from 'react'

/** What the open section registers: its dirty flag and a way to save. */
export interface DirtyEntry {
  dirty: boolean
  save(): Promise<boolean>
}

export interface SectionDirtyRegistry {
  /** Register the open section's current state, or null when it unmounts. */
  register(entry: (() => DirtyEntry) | null): void
}

export const SectionDirtyContext = createContext<SectionDirtyRegistry | null>(null)

/** Register this section's unsaved state with the page. A no-op outside the Settings page. */
export function useSectionDirty(dirty: boolean, save: () => Promise<boolean>): void {
  const registry = useContext(SectionDirtyContext)
  // Read at asking time, so the page always sees the latest values without re-registering each render.
  const latest = useRef<DirtyEntry>({ dirty, save })
  useEffect(() => {
    latest.current = { dirty, save }
  })
  useEffect(() => {
    registry?.register(() => latest.current)
    return () => registry?.register(null)
  }, [registry])
}
