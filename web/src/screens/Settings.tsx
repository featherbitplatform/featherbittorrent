/**
 * The Settings screen: a list of sections on the left and one section at a time on the right.
 *
 * `Back to torrents` stays fixed at the top. Groups: Torrents (Downloads, Speed), Access (Devices, Keyboard
 * shortcuts) and, inside the desktop app only, This app (Window, Serve the web UI, Instances, supplied
 * by {@link useDesktopSettings}); a section that is not built yet is not listed. Each section has its
 * own Save and tells the page when it holds changes (`useSectionDirty`): switching section, pressing
 * `Back to torrents` or unloading the tab then asks to keep or discard them. The page opens on the
 * section a link asked for (`section`: a section id or an old element id), else the last one used. Below
 * the `sm` breakpoint the list is a `Jump to` menu. The section's heading takes focus when it opens by a
 * click, Enter or Space or a request; the arrow keys keep the focus on the list.
 *
 * The page does not own every way out: the header's `show torrents` action and the browser's Back button
 * live in `App`. `onExitGuard` is how App reaches the same question: the page hands it `guard` (ask
 * first, then go) and `dirty` (is there anything to ask about).
 *
 * @module screens/Settings
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { ArrowLeft } from 'lucide-react'
import { useConnection } from '../connection'
import type { Api } from '../api/client'
import Button from '../components/Button'
import DownloadsSection from '../components/settings/DownloadsSection'
import Devices from '../components/settings/Devices'
import SectionList from '../components/settings/SectionList'
import { BUILT_SECTIONS, resolveSection, visibleSections, writeLastSection } from '../components/settings/sections'
import type { SectionId } from '../components/settings/sections'
import { Shortcuts } from '../components/settings/Shortcuts'
import SpeedSection from '../components/settings/SpeedSection'
import UnsavedPrompt from '../components/settings/UnsavedPrompt'
import { SectionDirtyContext } from '../components/settings/useSectionDirty'
import type { DirtyEntry, SectionDirtyRegistry } from '../components/settings/useSectionDirty'
import { useDesktopSettings } from '../desktopSettings'

/** What App holds to ask the open Settings page's unsaved-changes question on the exits the page does not own. */
export interface ExitGuard {
  /** Run `go` now, or once the person has answered the question about the open section's changes. */
  guard(go: () => void): void
  /** Whether the open section holds changes that were not saved. */
  dirty(): boolean
}

export interface SettingsProps {
  client?: Api
  /** Told by the page while it is open (and null when it closes), so App's own exits (the header, the browser's Back) ask too. */
  onExitGuard?(guard: ExitGuard | null): void
  onBack(): void
  /** The section to open at: a section id or an old element id; the last used when not given. */
  section?: string | null
}

export function Settings({ client: given, onExitGuard, onBack, section = null }: SettingsProps) {
  const connection = useConnection()
  const desktop = useDesktopSettings()
  const client = given ?? connection.api
  const isDesktop = desktop !== null
  const sections = useMemo(() => visibleSections({ desktop: isDesktop, built: BUILT_SECTIONS }), [isDesktop])
  const [current, setCurrent] = useState<SectionId>(() => resolveSection(section, sections))
  // What the page does once the person has answered the unsaved-changes question.
  const [pending, setPending] = useState<(() => void) | null>(null)
  const [saving, setSaving] = useState(false)
  const entry = useRef<(() => DirtyEntry) | null>(null)
  const title = useRef<HTMLHeadingElement>(null)
  const heading = useRef<HTMLHeadingElement>(null)
  const opened = useRef(false)

  const registry = useMemo<SectionDirtyRegistry>(
    () => ({
      register(next) {
        entry.current = next
      },
    }),
    [],
  )

  /** Run `go` now, or after the person says what becomes of the open section's changes. */
  const guard = useCallback((go: () => void): void => {
    if (entry.current?.().dirty) setPending(() => go)
    else go()
  }, [])

  useEffect(() => {
    onExitGuard?.({ guard, dirty: () => entry.current?.().dirty === true })
    return () => onExitGuard?.(null)
  }, [onExitGuard, guard])

  // Set when an arrow key opened the section: the focus stays on the list, not the heading.
  const quiet = useRef(false)
  const choose = useCallback(
    (id: SectionId, how: 'arrow' | 'click'): void => {
      if (id === current) {
        // Pressing the open section's own entry takes the person to its heading.
        if (how === 'click') title.current?.focus()
        return
      }
      guard(() => {
        quiet.current = how === 'arrow'
        setCurrent(id)
        writeLastSection(id)
      })
    },
    [current, guard],
  )

  // A link asks for a section while the page is open.
  const asked = useRef(section)
  useEffect(() => {
    if (section === asked.current) return
    asked.current = section
    if (section === null) return
    const id = resolveSection(section, sections)
    if (id !== current) guard(() => setCurrent(id))
  }, [section, sections, current, guard])

  // Opening with no request starts at the page's title; every later change, and a request, at the
  // heading of the section it shows.
  useEffect(() => {
    if (!opened.current && section === null) {
      opened.current = true
      heading.current?.focus()
      return
    }
    opened.current = true
    if (quiet.current) {
      quiet.current = false
      return
    }
    title.current?.focus()
  }, [current, section])

  // Closing the tab with changes asks the browser's own question.
  useEffect(() => {
    function onUnload(event: BeforeUnloadEvent): void {
      if (entry.current?.().dirty) event.preventDefault()
    }
    window.addEventListener('beforeunload', onUnload)
    return () => window.removeEventListener('beforeunload', onUnload)
  }, [])

  async function saveAndGo(): Promise<void> {
    const go = pending
    const open = entry.current?.()
    if (go === null || open === undefined) return
    setSaving(true)
    const kept = await open.save()
    setSaving(false)
    // A refusal stays on the section, which shows the reason.
    if (!kept) {
      setPending(null)
      return
    }
    setPending(null)
    go()
  }

  function discardAndGo(): void {
    const go = pending
    setPending(null)
    // The section unmounts with its draft: coming back shows what the server holds.
    go?.()
  }

  const def = sections.find((s) => s.id === current) ?? sections[0]
  function body(): ReactNode {
    switch (def.id) {
      case 'downloads':
        return <DownloadsSection client={client} />
      case 'speed':
        return <SpeedSection client={client} />
      case 'devices':
        return <Devices client={client} />
      case 'shortcuts':
        return desktop !== null ? desktop.shortcuts : <Shortcuts />
      default:
        return desktop?.sections.find((s) => s.id === def.id)?.render() ?? null
    }
  }

  return (
    <main className="mx-auto flex min-h-dvh w-full max-w-5xl flex-col gap-6 px-4 py-6">
      <div className="flex flex-wrap items-center gap-4">
        <Button variant="secondary" onClick={() => guard(onBack)}>
          <ArrowLeft className="h-4 w-4" aria-hidden="true" />
          Back to torrents
        </Button>
        <h1 ref={heading} tabIndex={-1} className="text-lg font-semibold text-strong outline-none">Settings</h1>
      </div>
      <div className="flex flex-col gap-6 sm:flex-row sm:gap-10">
        <SectionList sections={sections} current={def.id} onSelect={choose} />
        <SectionDirtyContext.Provider value={registry}>
          <section aria-labelledby="settings-section-title" className="flex min-w-0 flex-1 flex-col gap-4">
            <h2
              id="settings-section-title"
              ref={title}
              tabIndex={-1}
              className="text-base font-medium text-strong outline-none"
            >
              {def.title}
            </h2>
            {/* Keyed so a section starts fresh on every visit. */}
            <div key={def.id}>{body()}</div>
          </section>
        </SectionDirtyContext.Provider>
      </div>
      {pending !== null && (
        <UnsavedPrompt
          section={def.title}
          busy={saving}
          onStay={() => setPending(null)}
          onSave={() => void saveAndGo()}
          onDiscard={discardAndGo}
        />
      )}
    </main>
  )
}

export default Settings
