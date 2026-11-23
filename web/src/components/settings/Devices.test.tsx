// @vitest-environment jsdom
/**
 * The Devices section of Settings: the Ed25519 device logins, listed, added and removed.
 *
 * The fingerprint is always the server's: the page shows what the 201 answer carries and computes
 * nothing. With logins off the route answers 403 `forbidden`, which is a line of explanation here and
 * not a failure.
 *
 * @module components/settings/Devices.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import Devices from './Devices'
import { ApiError } from '../../api/client'
import type { Api, DeviceRow } from '../../api/client'
import { installDialogShim } from '../../dialog-shim'

afterEach(cleanup)
installDialogShim()

const LINE = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample laptop'

function fakeClient(over: Partial<Record<'devices' | 'addDevice' | 'removeDevice', unknown>> = {}) {
  const devices = vi.fn((): Promise<DeviceRow[]> => Promise.resolve([{ name: 'phone', fingerprint: 'SHA256:aaa' }]))
  const addDevice = vi.fn((name: string): Promise<DeviceRow> => Promise.resolve({ name, fingerprint: 'SHA256:fromserver' }))
  const removeDevice = vi.fn((): Promise<void> => Promise.resolve())
  const client = { devices, addDevice, removeDevice, ...over } as unknown as Api
  return { client, devices, addDevice, removeDevice }
}

async function open(fake = fakeClient()) {
  render(<Devices client={fake.client} />)
  return fake
}

describe('the Devices section', () => {
  it('devices are listed with their fingerprints', async () => {
    await open()
    expect(await screen.findByText('phone')).not.toBeNull()
    expect(screen.getByText('SHA256:aaa')).not.toBeNull()
  })

  it('adding a device shows the server\'s fingerprint', async () => {
    const { addDevice } = await open()
    await screen.findByText('phone')
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'laptop' } })
    fireEvent.change(screen.getByLabelText('Public key'), { target: { value: LINE } })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    expect(await screen.findByText('SHA256:fromserver')).not.toBeNull()
    expect(addDevice).toHaveBeenCalledWith('laptop', LINE)
    expect((screen.getByLabelText('Name') as HTMLInputElement).value, 'the form kept the saved device').toBe('')
  })

  it('removing asks first', async () => {
    const { removeDevice } = await open()
    await screen.findByText('phone')
    fireEvent.click(screen.getByRole('button', { name: 'Remove phone' }))
    expect(removeDevice, 'removed without asking').not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: 'Remove device' }))
    await waitFor(() => {
      expect(removeDevice).toHaveBeenCalledWith('phone')
    })
    await waitFor(() => {
      expect(screen.queryByText('SHA256:aaa')).toBeNull()
    })
  })

  it('logins off shows a line, not an error', async () => {
    const fake = fakeClient({
      devices: () => Promise.reject(new ApiError(403, 'forbidden', 'forbidden')),
    })
    const { container } = render(<Devices client={fake.client} />)
    expect(await screen.findByText("Device logins need the server's logins on.")).not.toBeNull()
    expect(container.querySelector('[role="alert"]'), 'logins off was shown as a failure').toBeNull()
    expect(screen.queryByRole('button', { name: 'Add' }), 'the add form showed with logins off').toBeNull()
  })

  it('changes are saved on the server', async () => {
    await open()
    expect(await screen.findByText("Changes are saved in the server's config.yaml.")).not.toBeNull()
  })

  it('a refused key shows the server\'s reason', async () => {
    const fake = fakeClient({
      addDevice: () => Promise.reject(new ApiError(400, 'invalid_key', 'not an ed25519 key')),
    })
    await open(fake)
    await screen.findByText('phone')
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'laptop' } })
    fireEvent.change(screen.getByLabelText('Public key'), { target: { value: 'junk' } })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    expect((await screen.findByRole('alert')).textContent).toContain('not an ed25519 key')
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('laptop')
    expect((screen.getByLabelText('Public key') as HTMLInputElement).value).toBe('junk')
  })

  it('a private key is never sent', async () => {
    const { addDevice } = await open()
    await screen.findByText('phone')
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'laptop' } })
    fireEvent.change(screen.getByLabelText('Public key'), {
      target: { value: '-----BEGIN OPENSSH PRIVATE KEY-----' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    expect((await screen.findByRole('alert')).textContent).toBe(
      'This is a private key. Paste the public key line instead.',
    )
    expect(addDevice, 'a private key was sent to the server').not.toHaveBeenCalled()
    expect((screen.getByLabelText('Public key') as HTMLInputElement).value).toBe('')
  })

  it('the key field refuses autofill and autocorrect', async () => {
    await open()
    await screen.findByText('phone')
    for (const label of ['Name', 'Public key']) {
      const input = screen.getByLabelText(label)
      expect(input.getAttribute('autocomplete')).toBe('off')
      expect(input.getAttribute('autocapitalize')).toBe('off')
      expect(input.getAttribute('autocorrect')).toBe('off')
      expect(input.getAttribute('spellcheck')).toBe('false')
    }
  })

  it('the name and key are sent as typed', async () => {
    const { addDevice } = await open()
    await screen.findByText('phone')
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: ' laptop ' } })
    fireEvent.change(screen.getByLabelText('Public key'), { target: { value: ` ${LINE} ` } })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    await waitFor(() => {
      expect(addDevice).toHaveBeenCalledWith(' laptop ', ` ${LINE} `)
    })
  })

  it('removing a device already gone drops the row', async () => {
    const fake = fakeClient({
      removeDevice: () => Promise.reject(new ApiError(404, 'not_found', 'no such device')),
    })
    await open(fake)
    await screen.findByText('phone')
    fireEvent.click(screen.getByRole('button', { name: 'Remove phone' }))
    fireEvent.click(screen.getByRole('button', { name: 'Remove device' }))
    await waitFor(() => {
      expect(screen.queryByText('SHA256:aaa')).toBeNull()
    })
  })

  it('a refused add is tied to the key field', async () => {
    const fake = fakeClient({
      addDevice: () => Promise.reject(new ApiError(400, 'invalid_key', 'bad key')),
    })
    await open(fake)
    await screen.findByText('phone')
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'laptop' } })
    fireEvent.change(screen.getByLabelText('Public key'), { target: { value: 'junk' } })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    const alert = await screen.findByRole('alert')
    const key = screen.getByLabelText('Public key')
    expect(key.getAttribute('aria-invalid')).toBe('true')
    expect(key.getAttribute('aria-describedby')).toBe(alert.id)
  })
})
