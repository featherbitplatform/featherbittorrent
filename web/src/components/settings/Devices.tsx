/**
 * The Devices section of Settings: device logins, the Ed25519 keys that sign in without a password.
 *
 * The list is name and key fingerprint, and the fingerprint is always the server's: after an add it
 * comes from the 201 answer, and this page computes none (a phone on plain HTTP has no Web Crypto to
 * do it with). Adding takes a name and a pasted `ssh-ed25519` line; a refusal is shown in the
 * server's own words with the form still filled. Removing asks first. A server with logins off
 * answers the devices route 403 `forbidden`, which is explained in one line instead of shown as an
 * error, and the add form stays away because there is nothing it could do.
 *
 * @module components/settings/Devices
 */
import { useEffect, useId, useRef, useState } from 'react'
import { ApiError, errorMessage } from '../../api/client'
import { useConnection } from '../../connection'
import type { Api, DeviceRow } from '../../api/client'
import { displaySafe } from '../../format'
import { MODAL_DIALOG, useModalDialog } from '../../modal'
import Banner from '../Banner'
import Button from '../Button'
import TextField from '../TextField'

export const LOGINS_OFF_COPY = "Device logins need the server's logins on."
export const PRIVATE_KEY_COPY = 'This is a private key. Paste the public key line instead.'
/** Anything that looks like private key material: PEM headers, PuTTY files, or an OpenSSH body pasted without its header. */
const PRIVATE_KEY_PATTERN = /-----BEGIN|PRIVATE[ -]?KEY|PuTTY-User-Key-File|Private-Lines|b3BlbnNzaC1rZXktdjE/i

export const SAVED_ON_SERVER_COPY = "Changes are saved in the server's config.yaml."

type Load = { phase: 'loading' } | { phase: 'off' } | { phase: 'failed'; text: string } | { phase: 'ready' }

function ConfirmRemove({
  name,
  client,
  onClose,
  onRemoved,
}: {
  name: string
  client: Api
  onClose(): void
  onRemoved(): void
}) {
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const busyRef = useRef(false)
  const { ref, onKeyDown } = useModalDialog(() => {
    if (!busyRef.current) onClose()
  }, '[data-cancel]')

  async function remove(): Promise<void> {
    if (busyRef.current) return
    busyRef.current = true
    setBusy(true)
    setError(null)
    try {
      await client.removeDevice(name)
    } catch (cause) {
      // Already removed elsewhere: the row is gone either way, so drop it instead of leaving it.
      if (cause instanceof ApiError && cause.status === 404) {
        onRemoved()
        onClose()
        return
      }
      setError(errorMessage(cause))
      busyRef.current = false
      setBusy(false)
      return
    }
    onRemoved()
    onClose()
  }

  return (
    <dialog
      ref={ref}
      onKeyDown={onKeyDown}
      aria-labelledby="remove-device-title"
      className={`w-[min(460px,calc(100vw-32px))] rounded-overlay border border-rule bg-raised text-body shadow-[0_24px_64px_rgba(0,0,0,0.5)] backdrop:bg-canvas/72 ${MODAL_DIALOG}`}
    >
      <div className="flex flex-col gap-2 px-5.5 py-5">
        <h2 id="remove-device-title" className="text-[16px] font-semibold text-strong">
          Remove device
        </h2>
        <p className="break-all font-mono text-[13px] text-strong">{displaySafe(name)}</p>
        <p className="text-[13px]">This device can no longer sign in with its key.</p>
        {error !== null && <Banner tone="danger">{error}</Banner>}
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2.5 border-t border-ink-700 px-5.5 py-4">
        <Button data-cancel variant="secondary" disabled={busy} onClick={onClose}>
          Cancel
        </Button>
        <Button
          variant="danger"
          disabled={busy}
          onClick={() => {
            void remove()
          }}
        >
          Remove device
        </Button>
      </div>
    </dialog>
  )
}

export default function Devices({ client: given }: { client?: Api }) {
  const connection = useConnection()
  const client = given ?? connection.api
  const [load, setLoad] = useState<Load>({ phase: 'loading' })
  const [rows, setRows] = useState<DeviceRow[]>([])
  const [name, setName] = useState('')
  const [key, setKey] = useState('')
  const [keyEpoch, setKeyEpoch] = useState(0)
  const [addError, setAddError] = useState<string | null>(null)
  const [adding, setAdding] = useState(false)
  const addingRef = useRef(false)
  const [removing, setRemoving] = useState<string | null>(null)
  const errorId = useId()

  useEffect(() => {
    let cancelled = false
    client.devices().then(
      (list) => {
        if (cancelled) return
        setRows(list)
        setLoad({ phase: 'ready' })
      },
      (cause: unknown) => {
        if (cancelled) return
        if (cause instanceof ApiError && cause.status === 403 && cause.code === 'forbidden') {
          setLoad({ phase: 'off' })
        } else {
          setLoad({ phase: 'failed', text: errorMessage(cause) })
        }
      },
    )
    return () => {
      cancelled = true
    }
  }, [client])

  async function add(): Promise<void> {
    if (addingRef.current) return
    // A private key must never leave the page, least of all over plain HTTP.
    if (PRIVATE_KEY_PATTERN.test(key)) {
      setKey('')
      // A fresh input, so the browser's undo history cannot bring the secret back.
      setKeyEpoch((n) => n + 1)
      setAddError(PRIVATE_KEY_COPY)
      return
    }
    addingRef.current = true
    setAdding(true)
    setAddError(null)
    try {
      const row = await client.addDevice(name, key)
      setRows((current) => [...current.filter((r) => r.name !== row.name), row])
      setName('')
      setKey('')
    } catch (cause) {
      setAddError(errorMessage(cause))
    } finally {
      addingRef.current = false
      setAdding(false)
    }
  }

  if (load.phase === 'loading') return <p className="text-[13px] text-muted">Loading...</p>
  if (load.phase === 'off') return <p className="text-[13px] text-muted">{LOGINS_OFF_COPY}</p>
  if (load.phase === 'failed') return <Banner tone="danger">{load.text}</Banner>

  return (
    <div className="flex max-w-xl flex-col gap-4">
      <p className="text-[13px] text-muted">{SAVED_ON_SERVER_COPY}</p>
      {rows.length === 0 ? (
        <p className="text-[13px] text-muted">No devices yet.</p>
      ) : (
        <ul className="flex flex-col divide-y divide-rule rounded-control border border-rule">
          {rows.map((row) => (
            <li key={row.name} className="flex flex-wrap items-center justify-between gap-2 px-3 py-2">
              <div className="flex min-w-0 flex-col">
                <span className="break-all text-[13px] text-strong">
                  {displaySafe(row.name)}
                </span>
                <span className="break-all font-mono text-[12px] text-muted">
                  {displaySafe(row.fingerprint)}
                </span>
              </div>
              <Button
                variant="secondary"
                size="sm"
                aria-label={`Remove ${displaySafe(row.name)}`}
                onClick={() => {
                  setRemoving(row.name)
                }}
              >
                Remove
              </Button>
            </li>
          ))}
        </ul>
      )}
      <form
        className="flex flex-col gap-3"
        onSubmit={(event) => {
          event.preventDefault()
          void add()
        }}
      >
        <TextField
          label="Name"
          autoComplete="off"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          value={name}
          onChange={(event) => {
            setName(event.target.value)
          }}
        />
        <TextField
          key={keyEpoch}
          label="Public key"
          autoComplete="off"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          aria-invalid={addError !== null}
          aria-describedby={addError !== null ? errorId : undefined}
          className="font-mono"
          placeholder="ssh-ed25519 AAAA... comment"
          value={key}
          onChange={(event) => {
            setKey(event.target.value)
            setAddError(null)
          }}
        />
        {addError !== null && (
          <p id={errorId} role="alert" className="text-[13px] text-error">
            {addError}
          </p>
        )}
        <div>
          <Button type="submit" disabled={adding || name.trim() === '' || key.trim() === ''}>
            Add
          </Button>
        </div>
      </form>
      {removing !== null && (
        <ConfirmRemove
          name={removing}
          client={client}
          onClose={() => {
            setRemoving(null)
          }}
          onRemoved={() => {
            setRows((current) => current.filter((r) => r.name !== removing))
          }}
        />
      )}
    </div>
  )
}
