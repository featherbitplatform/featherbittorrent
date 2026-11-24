/**
 * Managing instances: every saved instance with its address, which one the app opens on, its saved
 * password, and a form to add another.
 *
 * Every change goes to the shell, which owns the settings file and the keychain; this screen then asks
 * the page around it to read the list again (`onChanged`), so what it shows is always the shell's list
 * and never a copy that drifted from it.
 *
 * @module desktop/Instances
 */
import { useState } from 'react'
import type { FormEvent } from 'react'
import { ArrowLeft } from 'lucide-react'
import Banner from '../components/Banner'
import Button from '../components/Button'
import TextField from '../components/TextField'
import { NO_KEYCHAIN } from '../savePassword'
import { addInstance, forgetPassword, LOCAL_ID, removeInstance, setDefaultInstance } from './bridge'
import type { InstancesList } from './bridge'
import { inSwitcherOrder } from './instanceOrder'

export interface InstancesProps {
  list: InstancesList
  /** The instance the window is connected to. */
  current: string
  /** Read the list again: something changed. */
  onChanged(): Promise<void>
  /** Back to the page; only the standalone screen has a Back button. */
  onClose?(): void
  /** Drawn inside Settings' Desktop section: no screen of its own, no Back, no page title. */
  embedded?: boolean
}

/** The shell's refusal as a line to show: it answers with a string, anything else gets a general line. */
function line(cause: unknown): string {
  return typeof cause === 'string' && cause !== '' ? cause : 'That did not work. Try again.'
}

export function Instances({ list, current, onChanged, onClose, embedded = false }: InstancesProps) {
  const [name, setName] = useState('')
  const [url, setUrl] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  async function run(change: () => Promise<unknown>): Promise<boolean> {
    setBusy(true)
    setError(null)
    try {
      await change()
      return true
    } catch (cause) {
      setError(line(cause))
      return false
    } finally {
      await onChanged()
      setBusy(false)
    }
  }

  async function add(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault()
    if (await run(() => addInstance(name, url))) {
      setName('')
      setUrl('')
    }
  }

  const Frame = embedded ? 'div' : 'main'
  const AddHeading = embedded ? 'h4' : 'h2'
  return (
    <Frame className={embedded ? 'flex flex-col gap-4' : 'mx-auto flex w-full max-w-3xl flex-col gap-6 px-4 py-6'}>
      {!embedded && (
        <div className="flex flex-wrap items-center gap-4">
          <Button variant="secondary" onClick={onClose}>
            <ArrowLeft className="h-4 w-4" aria-hidden="true" />
            Back
          </Button>
          <h1 className="text-lg font-semibold text-strong">Instances</h1>
        </div>
      )}

      <ul className="flex flex-col divide-y divide-rule rounded-card border border-rule bg-surface">
        {inSwitcherOrder(list.items).map((item) => (
          <li key={item.id} className="flex flex-wrap items-center gap-3 px-4 py-3">
            <div className="flex min-w-0 flex-1 flex-col">
              <span className="truncate font-medium text-strong">
                {item.name}
                {item.id === current && <span className="ml-2 text-xs font-normal text-accent-light">Connected</span>}
                {item.id === list.default && <span className="ml-2 text-xs font-normal text-muted">Default</span>}
              </span>
              {item.url !== '' && <span className="truncate font-mono text-xs text-muted">{item.url}</span>}
            </div>
            <div className="flex flex-wrap gap-2">
              {item.id !== list.default && (
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={busy}
                  onClick={() => void run(() => setDefaultInstance(item.id))}
                >
                  Make default
                </Button>
              )}
              {item.saved_password && (
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={busy}
                  onClick={() => void run(() => forgetPassword(item.id))}
                >
                  Forget password
                </Button>
              )}
              {item.id !== LOCAL_ID && (
                <Button
                  variant="danger"
                  size="sm"
                  disabled={busy}
                  onClick={() => void run(() => removeInstance(item.id))}
                >
                  Remove
                </Button>
              )}
            </div>
          </li>
        ))}
      </ul>

      <form
        className="flex flex-col gap-3.5 rounded-card border border-rule bg-surface p-4"
        onSubmit={(event) => void add(event)}
      >
        <AddHeading className="text-base font-medium text-strong">Add an instance</AddHeading>
        <TextField label="Name" value={name} onChange={(event) => setName(event.target.value)} />
        <TextField
          label="Address"
          placeholder="http://192.168.1.20:8090"
          inputMode="url"
          spellCheck={false}
          value={url}
          onChange={(event) => setUrl(event.target.value)}
        />
        <div>
          <Button type="submit" disabled={busy}>
            Add instance
          </Button>
        </div>
      </form>

      {error !== null && <Banner tone="danger">{error}</Banner>}
      {!list.keychain && <p className="text-[13px] text-muted">{NO_KEYCHAIN}</p>}
    </Frame>
  )
}

export default Instances
