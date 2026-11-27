// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { ApiError } from '../api/client'
import type { ProductSettings, ProxyHealth } from '../api/client'
import ProxyBanner from './ProxyBanner'

beforeEach(() => {
  vi.useFakeTimers()
})
afterEach(() => {
  cleanup()
  vi.useRealTimers()
})

const withProxy = { proxy: { type: 'socks5' } } as unknown as ProductSettings
const withoutProxy = { proxy: { type: 'none' } } as unknown as ProductSettings

/** Lets the promise chains the banner starts run, without moving the clock. */
const settle = () => act(() => vi.advanceTimersByTimeAsync(0))

function setup(settings: ProductSettings, state: () => Promise<ProxyHealth>) {
  const api = { settings: vi.fn(() => Promise.resolve(settings)), proxyState: vi.fn(state) }
  const onOpenSettings = vi.fn()
  const view = render(<ProxyBanner api={api} onOpenSettings={onOpenSettings} />)
  return { api, onOpenSettings, view }
}

it('banner_shows_headline_and_opens_proxy_section', async () => {
  const { onOpenSettings } = setup(withProxy, () =>
    Promise.resolve({ state: 'unreachable', reason: 'Connection refused' }),
  )
  await settle()
  const button = screen.getByRole('button', { name: "Can't reach the proxy: Connection refused" })
  fireEvent.click(button)
  expect(onOpenSettings).toHaveBeenCalledWith('proxy')
})

it('banner_says_the_login_was_refused', async () => {
  setup(withProxy, () => Promise.resolve({ state: 'login_refused' }))
  await settle()
  expect(screen.getByRole('button').textContent).toBe("Can't reach the proxy: it refused the login")
})

it('banner_hidden_when_ok', async () => {
  for (const state of ['ok', 'off', 'udp_not_relayed'] as const) {
    cleanup()
    setup(withProxy, () => Promise.resolve({ state }))
    await settle()
    expect(screen.queryByRole('button'), state).toBeNull()
  }
})

it('banner_polls_every_five_seconds', async () => {
  const { api } = setup(withProxy, () => Promise.resolve({ state: 'ok' }))
  await settle()
  expect(api.proxyState).toHaveBeenCalledTimes(1)
  await act(() => vi.advanceTimersByTimeAsync(4_900))
  expect(api.proxyState).toHaveBeenCalledTimes(1)
  await act(() => vi.advanceTimersByTimeAsync(200))
  expect(api.proxyState).toHaveBeenCalledTimes(2)
})

it('banner_stops_polling_when_no_proxy', async () => {
  const { api, view } = setup(withoutProxy, () => Promise.resolve({ state: 'ok' }))
  await settle()
  await act(() => vi.advanceTimersByTimeAsync(20_000))
  expect(api.proxyState).not.toHaveBeenCalled()
  // And it stops on unmount.
  cleanup()
  const second = setup(withProxy, () => Promise.resolve({ state: 'ok' }))
  await settle()
  const before = second.api.proxyState.mock.calls.length
  second.view.unmount()
  await act(() => vi.advanceTimersByTimeAsync(20_000))
  expect(second.api.proxyState.mock.calls.length).toBe(before)
  void view
})

it('banner_does_not_poll_while_the_page_is_hidden', async () => {
  const visibility = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden')
  const { api } = setup(withProxy, () => Promise.resolve({ state: 'ok' }))
  await settle()
  await act(() => vi.advanceTimersByTimeAsync(20_000))
  expect(api.proxyState).not.toHaveBeenCalled()
  visibility.mockReturnValue('visible')
  await act(async () => {
    document.dispatchEvent(new Event('visibilitychange'))
    await vi.advanceTimersByTimeAsync(0)
  })
  expect(api.proxyState).toHaveBeenCalledTimes(1)
  visibility.mockRestore()
})

it('banner_stops_asking_an_older_server', async () => {
  const { api } = setup(withProxy, () => Promise.reject(new ApiError(404, null, 'the server answered 404')))
  await settle()
  await act(() => vi.advanceTimersByTimeAsync(30_000))
  expect(api.proxyState).toHaveBeenCalledTimes(1)
  expect(screen.queryByRole('button')).toBeNull()
})

it('banner_keeps_the_last_state_on_a_network_error', async () => {
  let first = true
  setup(withProxy, () => {
    if (first) {
      first = false
      return Promise.resolve({ state: 'unreachable', reason: 'down' })
    }
    return Promise.reject(new TypeError('network'))
  })
  await settle()
  await act(() => vi.advanceTimersByTimeAsync(6_000))
  expect(screen.getByRole('button').textContent).toBe("Can't reach the proxy: down")
})
