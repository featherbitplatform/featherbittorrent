/**
 * The Settings screen: what the server and this page can be told, in three sections.
 *
 * Server (the speed limits and queue counts) is built here. Devices and Keyboard shortcuts are
 * named slots with a heading and no content yet: the device logins and the shortcut list are
 * mounted into them by their own pieces of work. The screen replaces the Torrents screen while it
 * is open, and `Back to torrents` returns.
 *
 * @module screens/Settings
 */
import { ArrowLeft } from 'lucide-react'
import { api } from '../api/client'
import type { Api } from '../api/client'
import Button from '../components/Button'
import Devices from '../components/settings/Devices'
import ServerSettings from '../components/settings/ServerSettings'

export interface SettingsProps {
  client?: Api
  onBack(): void
}

export function Settings({ client = api, onBack }: SettingsProps) {
  return (
    <main className="mx-auto flex min-h-dvh w-full max-w-3xl flex-col gap-8 px-4 py-6">
      <div className="flex flex-wrap items-center gap-4">
        <Button variant="secondary" onClick={onBack}>
          <ArrowLeft className="h-4 w-4" aria-hidden="true" />
          Back to torrents
        </Button>
        <h1 className="text-lg font-semibold text-strong">Settings</h1>
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
      <section aria-labelledby="settings-shortcuts" className="flex flex-col gap-3">
        <h2 id="settings-shortcuts" className="text-base font-medium text-strong">
          Keyboard shortcuts
        </h2>
        {/* SHORTCUTS SLOT: the read-only key list is mounted here. */}
      </section>
    </main>
  )
}

export default Settings
