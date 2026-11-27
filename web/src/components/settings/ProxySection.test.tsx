// @vitest-environment jsdom
/**
 * The Proxy section of Settings: the type, host, port and login, the password rules (never prefilled,
 * `keep` when untouched, `set` when typed, `clear` on request, asked for again when the address
 * changes), and the health lines.
 *
 * @module components/settings/ProxySection.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import ProxySection from './ProxySection'
import { ApiError } from '../../api/client'
import type { Api, ProductSettings, ProxyChange, ProxyHealth, ProxySettings } from '../../api/client'

afterEach(cleanup)

const SPEED = { download_kib: null, upload_kib: null, active_downloads: null, active_seeds: null, active_total: null }
const DOWNLOADS = {
  default_dir: '/data',
  unfinished_dir: null,
  start_paused: false,
  watch_dir: null,
  on_complete: { program: null, editable: false },
  watch_problem: null,
  on_complete_problem: null,
}
const SET: ProxySettings = {
  type: 'socks5',
  host: 'proxy.example',
  port: 1080,
  username: 'ana',
  password_saved: true,
  password_storage: 'config_file',
}
const NONE: ProxySettings = {
  type: 'none',
  host: '',
  port: 0,
  username: null,
  password_saved: false,
  password_storage: 'config_file',
}

function make(proxy: ProxySettings, health: ProxyHealth = { state: 'ok' }) {
  const saveProxy = vi.fn(
    (change: ProxyChange): Promise<ProductSettings> =>
      Promise.resolve({
        speed: SPEED,
        downloads: DOWNLOADS,
        proxy: { ...proxy, type: change.type, host: change.host, port: change.port },
      }),
  )
  const client = {
    settings: () => Promise.resolve({ speed: SPEED, downloads: DOWNLOADS, proxy }),
    proxyState: () => Promise.resolve(health),
    saveProxy,
  } as unknown as Api
  return { client, saveProxy }
}

const field = (name: string) => screen.getByLabelText(name) as HTMLInputElement
const type = (name: string, value: string) => fireEvent.change(field(name), { target: { value } })
const save = () => fireEvent.click(screen.getByRole('button', { name: 'Save' }))

async function open(proxy: ProxySettings, health?: ProxyHealth) {
  const made = make(proxy, health)
  render(<ProxySection client={made.client} />)
  await screen.findByLabelText('Proxy type')
  return made
}

describe('the password', () => {
  it('password_is_never_prefilled', async () => {
    await open(SET)
    expect(field('Password').value).toBe('')
    expect(field('Password').placeholder).toBe('saved')
    expect(field('Password').type).toBe('password')
  })

  it('untouched_password_sends_keep', async () => {
    const { saveProxy } = await open(SET)
    type('User name', 'bob')
    save()
    await waitFor(() => expect(saveProxy).toHaveBeenCalled())
    expect(saveProxy).toHaveBeenCalledWith({
      type: 'socks5',
      host: 'proxy.example',
      port: 1080,
      username: 'bob',
      password: 'keep',
    })
  })

  it('typed_password_sends_set', async () => {
    const { saveProxy } = await open(SET)
    type('Password', 'hunter2')
    save()
    await waitFor(() => expect(saveProxy).toHaveBeenCalled())
    expect(saveProxy.mock.calls[0][0].password).toEqual({ set: 'hunter2' })
    // After the save the field is empty again.
    await screen.findByText('Saved.')
    expect(field('Password').value).toBe('')
  })

  it('clear_sends_clear', async () => {
    const { saveProxy } = await open(SET)
    fireEvent.click(screen.getByRole('button', { name: 'Clear password' }))
    save()
    await waitFor(() => expect(saveProxy).toHaveBeenCalled())
    expect(saveProxy.mock.calls[0][0].password).toBe('clear')
  })

  it('changing_the_address_asks_for_the_password_again', async () => {
    const { saveProxy } = await open(SET)
    type('Port', '1081')
    expect(screen.getByText("Enter the password again when the proxy's host or port changes.")).toBeTruthy()
    expect(field('Password').placeholder).toBe('')
    const button = screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement
    expect(button.disabled).toBe(true)
    save()
    expect(saveProxy).not.toHaveBeenCalled()
    type('Password', 'again')
    expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(false)
    expect(screen.queryByText("Enter the password again when the proxy's host or port changes.")).toBeNull()
  })

  it('changing_the_address_can_be_saved_with_clear', async () => {
    const { saveProxy } = await open(SET)
    type('Host', 'other.example')
    fireEvent.click(screen.getByRole('button', { name: 'Clear password' }))
    expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(false)
    save()
    await waitFor(() => expect(saveProxy).toHaveBeenCalled())
    expect(saveProxy.mock.calls[0][0]).toMatchObject({ host: 'other.example', password: 'clear' })
  })

  it('changing_only_the_type_keeps_saved', async () => {
    await open(SET)
    fireEvent.change(screen.getByLabelText('Proxy type'), { target: { value: 'http' } })
    expect(field('Password').placeholder).toBe('saved')
    expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(false)
  })

  it('no_keychain_shows_kept_in_config_line', async () => {
    await open({ ...SET, password_storage: 'unavailable' })
    expect(screen.getByText('This system has no keychain, so the proxy password is kept in config.yaml.')).toBeTruthy()
  })

  it('a_keychain_shows_no_such_line', async () => {
    await open({ ...SET, password_storage: 'keychain' })
    expect(screen.queryByText(/no keychain/)).toBeNull()
  })
})

describe('the form', () => {
  it('none_hides_the_fields_and_saves_the_removal', async () => {
    const { saveProxy } = await open(SET)
    fireEvent.change(screen.getByLabelText('Proxy type'), { target: { value: 'none' } })
    for (const name of ['Host', 'Port', 'User name', 'Password']) expect(screen.queryByLabelText(name)).toBeNull()
    expect(screen.queryByText(/While a proxy is set/)).toBeNull()
    save()
    await waitFor(() => expect(saveProxy).toHaveBeenCalled())
    expect(saveProxy.mock.calls[0][0]).toMatchObject({ type: 'none', host: '', port: 0, username: null })
  })

  it('http_shows_tcp_only_line', async () => {
    await open(NONE)
    fireEvent.change(screen.getByLabelText('Proxy type'), { target: { value: 'socks5' } })
    expect(screen.getByText(/While a proxy is set, every connection goes through it\./)).toBeTruthy()
    expect(screen.queryByText(/carries TCP only/)).toBeNull()
    fireEvent.change(screen.getByLabelText('Proxy type'), { target: { value: 'http' } })
    expect(
      screen.getByText("An HTTP proxy carries TCP only, so the DHT, uTP and UDP trackers are off too."),
    ).toBeTruthy()
  })

  it('invalid_port_shows_refusal', async () => {
    const { saveProxy } = await open(NONE)
    fireEvent.change(screen.getByLabelText('Proxy type'), { target: { value: 'http' } })
    type('Host', 'p.example')
    for (const bad of ['', '0', '65536', 'abc', '-1']) {
      type('Port', bad)
      save()
      expect((await screen.findByRole('alert')).textContent).toBe('The port must be a number from 1 to 65535.')
    }
    expect(saveProxy).not.toHaveBeenCalled()
  })

  it('an_empty_host_is_refused', async () => {
    const { saveProxy } = await open(NONE)
    fireEvent.change(screen.getByLabelText('Proxy type'), { target: { value: 'http' } })
    type('Port', '8080')
    save()
    expect((await screen.findByRole('alert')).textContent).toBe("Enter the proxy's host name or address.")
    expect(saveProxy).not.toHaveBeenCalled()
  })

  it('a_server_refusal_is_shown_and_fields_are_kept', async () => {
    const { client, saveProxy } = await open(SET)
    saveProxy.mockRejectedValueOnce(new ApiError(400, 'invalid_setting', 'A proxy login needs a user name.\u0007'))
    type('User name', '')
    save()
    expect((await screen.findByRole('alert')).textContent).toBe('A proxy login needs a user name.')
    expect(field('User name').value).toBe('')
    expect(client).toBeTruthy()
  })
})

describe('health', () => {
  it('udp_not_relayed_line_shown', async () => {
    await open(SET, { state: 'udp_not_relayed' })
    expect(await screen.findByText('This proxy does not relay UDP, so the DHT and UDP trackers are off')).toBeTruthy()
  })

  it('unreachable_headline_has_the_reason', async () => {
    await open(SET, { state: 'unreachable', reason: 'connection refused' })
    expect(await screen.findByText("Can't reach the proxy: connection refused")).toBeTruthy()
  })

  it('no_proxy_shows_no_health_line', async () => {
    await open(NONE, { state: 'unreachable', reason: 'x' })
    expect(screen.queryByText(/Can't reach/)).toBeNull()
  })
})

it('an_older_server_says_so', async () => {
  const client = { settings: () => Promise.resolve({ speed: SPEED, downloads: DOWNLOADS }), proxyState: () => Promise.reject(new Error('x')) } as unknown as Api
  render(<ProxySection client={client} />)
  expect(await screen.findByText(/too old to change its settings/)).toBeTruthy()
})
