// @vitest-environment jsdom
/**
 * The Speed section of Settings: the five numbers the terminal's settings screen has, saved through
 * `PUT /api/v1/settings/speed`, and on a server that has no settings routes the same form saved through
 * the engine routes with a line saying why.
 *
 * @module components/settings/SpeedSection.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import SpeedSection, { TOO_OLD } from './SpeedSection'
import { ApiError, createClient } from '../../api/client'
import type { Api, SessionInfo } from '../../api/client'

afterEach(cleanup)

const SPEED = { download_kib: 1024, upload_kib: null, active_downloads: 3, active_seeds: 5, active_total: 500 }
const DOWNLOADS = {
  default_dir: '/data',
  unfinished_dir: null,
  start_paused: false,
  watch_dir: null,
  on_complete: { program: null, editable: false },
  watch_problem: null,
  on_complete_problem: null,
}

const field = (name: string) => screen.getByLabelText(name) as HTMLInputElement
const type = (name: string, value: string) => fireEvent.change(field(name), { target: { value } })
const save = () => fireEvent.click(screen.getByRole('button', { name: 'Save' }))

interface Call {
  method: string
  url: string
  body: unknown
}

/** The real client over a fake fetch that answers the settings routes the way A3 does. */
function serverClient(speed = SPEED) {
  const calls: Call[] = []
  const fetchFn = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const call = { method: init?.method ?? 'GET', url: String(input), body: init?.body ? JSON.parse(String(init.body)) : undefined }
    calls.push(call)
    const answered = call.method === 'PUT' ? (call.body as typeof SPEED) : speed
    return new Response(JSON.stringify({ speed: answered, downloads: DOWNLOADS }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    })
  }) as typeof fetch
  return { calls, client: createClient(fetchFn, () => 'tok', () => 0, () => {}) }
}

describe('on a server with the settings routes', () => {
  it('shows the server values', async () => {
    const { client } = serverClient()
    render(<SpeedSection client={client} />)
    await screen.findByDisplayValue('3')
    expect(field('Download limit KiB/s').value).toBe('1024')
    expect(field('Upload limit KiB/s').value).toBe('0')
    expect(field('Active seeds').value).toBe('5')
    expect(field('Active total').value).toBe('500')
    expect(screen.getByRole('heading', { name: 'Queue' })).toBeTruthy()
    expect(screen.queryByText(TOO_OLD)).toBeNull()
  })

  it('save_puts_speed_route', async () => {
    const { client, calls } = serverClient()
    render(<SpeedSection client={client} />)
    await screen.findByDisplayValue('3')
    type('Download limit KiB/s', '64')
    type('Active seeds', '')
    save()
    await screen.findByText('Saved.')
    const put = calls.find((c) => c.method === 'PUT')!
    expect(put.url).toBe('/api/v1/settings/speed')
    expect(put.body).toEqual({ download_kib: 64, upload_kib: null, active_downloads: 3, active_seeds: null, active_total: 500 })
  })

  it('a bad value is refused in the form and nothing is sent', async () => {
    const { client, calls } = serverClient()
    render(<SpeedSection client={client} />)
    await screen.findByDisplayValue('3')
    for (const bad of ['-1', 'abc', '9007199254740993']) {
      type('Active seeds', bad)
      save()
      expect(screen.getByRole('alert').textContent, `no error for ${bad}`).not.toBe('')
    }
    expect(calls.some((c) => c.method === 'PUT')).toBe(false)
  })

  it('shows the line the server refused with', async () => {
    const client = {
      settings: () => Promise.resolve({ speed: SPEED, downloads: DOWNLOADS }),
      saveSpeed: () => Promise.reject(new ApiError(400, 'invalid_setting', 'Active total must be at least Active downloads.')),
    } as unknown as Api
    render(<SpeedSection client={client} />)
    await screen.findByDisplayValue('3')
    type('Active seeds', '9')
    save()
    expect((await screen.findByRole('alert')).textContent).toBe('Active total must be at least Active downloads.')
  })

  it('saving twice quickly sends once', async () => {
    const { client, calls } = serverClient()
    render(<SpeedSection client={client} />)
    await screen.findByDisplayValue('3')
    const form = field('Active seeds').closest('form') as HTMLFormElement
    fireEvent.submit(form)
    fireEvent.submit(form)
    await screen.findByText('Saved.')
    expect(calls.filter((c) => c.method === 'PUT')).toHaveLength(1)
  })
})

describe('on a server that is too old', () => {
  const SESSION: SessionInfo = {
    version: '0.13.5',
    download_limit: 1_048_576,
    upload_limit: 0,
    download_rate: 0,
    upload_rate: 0,
    listen_addrs: [],
    torrents: 0,
    dht_nodes: 0,
  }
  const QUEUE = {
    active_downloads: 3,
    active_seeds: 5,
    active_limit: 500,
    dont_count_slow_torrents: true,
    inactive_down_rate: 2048,
    inactive_up_rate: 4096,
    startup_grace: 7,
    interval: 11,
  }

  function oldServer(session: SessionInfo = SESSION, queueFails = false) {
    const setSessionLimits = vi.fn(() => Promise.resolve())
    const setQueueSettings = vi.fn(() => (queueFails ? Promise.reject(new Error('queue refused')) : Promise.resolve()))
    const saveSpeed = vi.fn()
    const client = {
      settings: () => Promise.reject(new ApiError(404, null, 'the server answered 404')),
      session: () => Promise.resolve(session),
      queueSettings: () => Promise.resolve({ ...QUEUE }),
      setSessionLimits,
      setQueueSettings,
      saveSpeed,
    } as unknown as Api
    return { client, setSessionLimits, setQueueSettings, saveSpeed }
  }

  it('old_server_shows_too_old_and_speed_still_saves', async () => {
    // Review Focus 5: the line, and the engine routes, never the settings route.
    const fetchCalls: Call[] = []
    const fetchFn = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      const method = init?.method ?? 'GET'
      fetchCalls.push({ method, url, body: init?.body ? JSON.parse(String(init.body)) : undefined })
      const answers: Record<string, [number, string]> = {
        'GET /api/v1/settings': [404, '{"error":{"code":"not_found","message":"not found"}}'],
        'GET /api/v1/session': [200, JSON.stringify(SESSION)],
        'GET /api/v1/session/queue': [200, JSON.stringify(QUEUE)],
      }
      const [status, body] = answers[`${method} ${url}`] ?? [204, '']
      return new Response(body === '' ? null : body, { status, headers: { 'content-type': 'application/json' } })
    }) as typeof fetch
    const client = createClient(fetchFn, () => 'tok', () => 0, () => {})
    render(<SpeedSection client={client} />)
    await screen.findByText(TOO_OLD)
    type('Download limit KiB/s', '2048')
    save()
    await screen.findByText('Saved.')
    const limits = fetchCalls.find((c) => c.method === 'PUT' && c.url === '/api/v1/session/limits')
    expect(limits?.body).toEqual({ download: 2_097_152, upload: null })
    expect(fetchCalls.some((c) => c.url === '/api/v1/settings/speed'), 'the settings route was used').toBe(false)
  })

  it('shows the values the engine routes hold', async () => {
    const { client } = oldServer()
    render(<SpeedSection client={client} />)
    await screen.findByText(TOO_OLD)
    expect(field('Download limit KiB/s').value).toBe('1024')
    expect(field('Active seeds').value).toBe('5')
  })

  it('save sends rates in bytes and keeps unknown queue fields', async () => {
    const { client, setSessionLimits, setQueueSettings, saveSpeed } = oldServer()
    render(<SpeedSection client={client} />)
    await screen.findByText(TOO_OLD)
    type('Download limit KiB/s', '2048')
    type('Active seeds', '8')
    save()
    await waitFor(() => expect(setQueueSettings).toHaveBeenCalledTimes(1))
    expect(setSessionLimits).toHaveBeenCalledWith(2_097_152, null)
    expect(setQueueSettings).toHaveBeenCalledWith({ ...QUEUE, active_seeds: 8 })
    expect(saveSpeed).not.toHaveBeenCalled()
  })

  it('an untouched rate is sent back exactly', async () => {
    const { client, setSessionLimits, setQueueSettings } = oldServer({ ...SESSION, download_limit: 1_500_000, upload_limit: 100 })
    render(<SpeedSection client={client} />)
    await screen.findByText(TOO_OLD)
    expect(field('Upload limit KiB/s').value, 'a nonzero limit read as 0').toBe('1')
    type('Active seeds', '8')
    save()
    await waitFor(() => expect(setQueueSettings).toHaveBeenCalledTimes(1))
    expect(setSessionLimits).toHaveBeenCalledWith(1_500_000, 100)
  })

  it('a failed queue save says the limits were applied', async () => {
    const { client, setSessionLimits } = oldServer(SESSION, true)
    render(<SpeedSection client={client} />)
    await screen.findByText(TOO_OLD)
    save()
    const alert = await screen.findByRole('alert')
    expect(alert.textContent).toContain('The speed limits were applied; the queue counts were not')
    expect(setSessionLimits).toHaveBeenCalledTimes(1)
  })
})
