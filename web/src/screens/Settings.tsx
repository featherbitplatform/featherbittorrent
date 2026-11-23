/**
 * The Settings screen: what the server and this page can be told, in three sections.
 *
 * Server (the speed limits and queue counts), Devices (the device logins) and Keyboard shortcuts
 * (the read-only key list). The screen replaces the Torrents screen while it is open, and
 * `Back to torrents` returns. Inside the desktop app a Desktop section follows (`desktopSections`, from
 * {@link useDesktopSettings} unless given), and the editable shortcut list lives there instead of the
 * read-only one. `section` opens the screen scrolled to that element, its heading focused.
 *
 * @module screens/Settings
 */
import { useEffect, useRef } from 'react'
import type { ReactNode } from 'react'
import { ArrowLeft } from 'lucide-react'
import { useConnection } from '../connection'
import type { Api } from '../api/client'
import Button from '../components/Button'
import Devices from '../components/settings/Devices'
import ServerSettings from '../components/settings/ServerSettings'
import { Shortcuts } from '../components/settings/Shortcuts'
import { useDesktopSettings } from '../desktopSettings'

export interface SettingsProps {
  client?: Api
  onBack(): void
  /** The desktop app's own section; in a browser there is none. */
  desktopSections?: ReactNode
  /** The id of the section to open at; the top when not given. */
  section?: string | null
}

export function Settings({ client: given, onBack, desktopSections, section = null }: SettingsProps) {
  const connection = useConnection()
  const desktop = useDesktopSettings()
  const extra = desktopSections ?? desktop?.sections ?? null
  const client = given ?? connection.api
  const heading = useRef<HTMLHeadingElement>(null)
  // Opening replaces the screen the focus was on, so it starts at the new screen's title, or at the
  // heading of the section it was opened at.
  useEffect(() => {
    const target = section === null ? null : document.getElementById(section)
    if (target === null) {
      heading.current?.focus()
      return
    }
    target.scrollIntoView?.({ block: 'start' })
    const title = target.querySelector<HTMLElement>('h2, h3')
    if (title !== null) {
      title.tabIndex = -1
      title.focus()
    }
  }, [section])
  return (
    <main className="mx-auto flex min-h-dvh w-full max-w-3xl flex-col gap-8 px-4 py-6">
      <div className="flex flex-wrap items-center gap-4">
        <Button variant="secondary" onClick={onBack}>
          <ArrowLeft className="h-4 w-4" aria-hidden="true" />
          Back to torrents
        </Button>
        <h1 ref={heading} tabIndex={-1} className="text-lg font-semibold text-strong outline-none">Settings</h1>
      </div>
      <section aria-labelledby="settings-server" className="flex flex-col gap-3">
        <h2 id="settings-server" className="text-base font-medium text-strong">
          Server
        </h2>
        <ServerSettings client={client} />
      </section>
      <section aria-labelledby="settings-devices" className="flex flex-col gap-3">
        <h2 id="settings-devices" className="text-base font-medium text-strong">
          Devices
        </h2>
        <Devices client={client} />
      </section>
      {extra === null && (
        <section aria-labelledby="settings-shortcuts" className="flex flex-col gap-3">
          <h2 id="settings-shortcuts" className="text-base font-medium text-strong">
            Keyboard shortcuts
          </h2>
          <Shortcuts />
        </section>
      )}
      {extra}
    </main>
  )
}

export default Settings
