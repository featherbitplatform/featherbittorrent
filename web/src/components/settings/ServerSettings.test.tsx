// @vitest-environment jsdom
/**
 * The Server section of Settings: the five numbers the terminal's settings screen has.
 *
 * Rates are shown in KiB/s and travel in bytes per second; the queue settings are read, changed and
 * written back as a whole object so the fields the screen never shows are not lost.
 *
 * @module components/settings/ServerSettings.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import ServerSettings from './ServerSettings'
import type { Api, SessionInfo } from '../../api/client'

afterEach(cleanup)

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

function fakeClient(session: SessionInfo = SESSION, queueFails = false) {
  const setSessionLimits = vi.fn(() => Promise.resolve())
  const setQueueSettings = vi.fn(() => (queueFails ? Promise.reject(new Error('queue refused')) : Promise.resolve()))
  const client = {
    session: () => Promise.resolve(session),
    queueSettings: () => Promise.resolve({ ...QUEUE }),
    setSessionLimits,
    setQueueSettings,
  } as unknown as Api
  return { client, setSessionLimits, setQueueSettings }
}

async function open(session: SessionInfo = SESSION, queueFails = false) {
  const fake = fakeClient(session, queueFails)
  render(<ServerSettings client={fake.client} />)
  await screen.findByDisplayValue('3')
  return fake
}

const field = (name: string) => screen.getByLabelText(name)
const type = (name: string, value: string) => {
  fireEvent.change(field(name), { target: { value } })
}
const save = () => {
  fireEvent.click(screen.getByRole('button', { name: 'Save' }))
}

describe('the Server section', () => {
  it('settings open with the server\'s values', async () => {
    await open()
    expect((field('Download limit KiB/s') as HTMLInputElement).value).toBe('1024')
    expect((field('Upload limit KiB/s') as HTMLInputElement).value).toBe('0')
    expect((field('Active downloads') as HTMLInputElement).value).toBe('3')
    expect((field('Active seeds') as HTMLInputElement).value).toBe('5')
    expect((field('Active total') as HTMLInputElement).value).toBe('500')
  })

  it('save sends rates in bytes and keeps unknown queue fields', async () => {
    const { setSessionLimits, setQueueSettings } = await open()
    type('Download limit KiB/s', '2048')
    type('Active seeds', '8')
    save()
    await waitFor(() => {
      expect(setQueueSettings).toHaveBeenCalledTimes(1)
    })
    expect(setSessionLimits).toHaveBeenCalledWith(2_097_152, null)
    expect(setQueueSettings).toHaveBeenCalledWith({ ...QUEUE, active_seeds: 8 })
  })

  it('a bad value is refused in the form', async () => {
    const { setSessionLimits, setQueueSettings } = await open()
    for (const bad of ['-1', 'abc', '9007199254740993']) {
      type('Active seeds', bad)
      save()
      expect(screen.getByRole('alert').textContent, `no error for ${bad}`).not.toBe('')
    }
    expect(setSessionLimits).not.toHaveBeenCalled()
    expect(setQueueSettings).not.toHaveBeenCalled()
  })

  it('empty means unlimited', async () => {
    const { setSessionLimits, setQueueSettings } = await open()
    type('Download limit KiB/s', '')
    type('Active downloads', '')
    save()
    await waitFor(() => {
      expect(setQueueSettings).toHaveBeenCalledTimes(1)
    })
    expect(setSessionLimits).toHaveBeenCalledWith(null, null)
    expect(setQueueSettings).toHaveBeenCalledWith({ ...QUEUE, active_downloads: null })
  })

  it('an untouched rate is sent back exactly', async () => {
    const { setSessionLimits, setQueueSettings } = await open({
      ...SESSION,
      download_limit: 1_500_000,
      upload_limit: 100,
    })
    expect((field('Upload limit KiB/s') as HTMLInputElement).value, 'a nonzero limit read as 0').toBe('1')
    type('Active seeds', '8')
    save()
    await waitFor(() => {
      expect(setQueueSettings).toHaveBeenCalledTimes(1)
    })
    expect(setSessionLimits).toHaveBeenCalledWith(1_500_000, 100)
  })

  it('saving twice quickly sends once', async () => {
    const { setSessionLimits, setQueueSettings } = await open()
    // Enter in a field submits the form even though the button is already disabled.
    const form = field('Active seeds').closest('form') as HTMLFormElement
    fireEvent.submit(form)
    fireEvent.submit(form)
    await waitFor(() => {
      expect(setQueueSettings).toHaveBeenCalledTimes(1)
    })
    expect(setSessionLimits).toHaveBeenCalledTimes(1)
  })

  it('a failed queue save says the limits were applied', async () => {
    const { setSessionLimits } = await open(SESSION, true)
    save()
    const alert = await screen.findByRole('alert')
    expect(alert.textContent).toContain('The speed limits were applied; the queue counts were not')
    expect(setSessionLimits).toHaveBeenCalledTimes(1)
  })
})
