// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App'
import { UNREACHABLE_COPY } from './live'
import { SESSION_ENDED, session } from './session'

// The `api` and `session` singletons read `globalThis.fetch` the moment their modules load,
// so the controllable fetch must exist before this file's imports run.
const world = vi.hoisted((): { behavior: null | ((url: string) => Promise<unknown>) } => {
  const state: { behavior: null | ((url: string) => Promise<unknown>) } = { behavior: null }
  globalThis.fetch = ((input: unknown) => {
    const behavior = state.behavior
    if (behavior === null) throw new Error(`nothing was set up to answer ${String(input)}`)
    return behavior(String(input))
  }) as typeof globalThis.fetch
  return state
})

const answer = (body: unknown): Promise<unknown> =>
  Promise.resolve({ ok: true, status: 200, statusText: 'OK', json: async () => body })

const SESSION_BODY = {
  version: '0.13.5',
  download_limit: 0,
  upload_limit: 0,
  download_rate: 1_000_000,
  upload_rate: 250_000,
  listen_addrs: ['0.0.0.0:51497'],
  torrents: 0,
  dht_nodes: 120,
}

/** Every read fails the way a dead server makes fetch fail: the promise itself. */
const dead = (): Promise<unknown> => Promise.reject(new TypeError('fetch failed'))

/** The same server, answering: signed in, an empty table, a session row. */
function alive(url: string): Promise<unknown> {
  if (url.includes('/auth/')) return answer({ auth: true, subject: 'alice', csrf: 'csrf-value' })
  if (url.includes('/torrents')) return answer([])
  if (url.includes('/api/v1/session')) return answer(SESSION_BODY)
  return answer({})
}

afterEach(cleanup)

describe('the page as a whole', () => {
  it('the_live_regions_outlive_the_screens_in_the_page', async () => {
    world.behavior = dead
    const { container } = render(<App />)

    const polite = container.querySelector('[aria-live="polite"]')
    const assertive = container.querySelector('[aria-live="assertive"]')
    expect(
      polite !== null && assertive !== null,
      'the page mounted without its two live regions',
    ).toBe(true)

    // loading to unreachable: the words must land in the region that was already mounted,
    // not in a fresh node built together with the new screen.
    await waitFor(() => screen.getByRole('button', { name: 'Try again' }))
    expect(
      assertive !== null && assertive.textContent === UNREACHABLE_COPY,
      'the unreachable copy arrived in a fresh assertive region instead of the one already mounted',
    ).toBe(true)
    expect(
      polite !== null && polite.textContent === '',
      'the unreachable copy also went to the polite region',
    ).toBe(true)

    // unreachable to signedIn: the whole screen is replaced, the regions are not.
    world.behavior = alive
    await act(async () => {
      await session.refresh()
    })
    expect(
      polite !== null && assertive !== null && document.contains(polite) && document.contains(assertive),
      'a screen change replaced one of the live regions',
    ).toBe(true)
    expect(screen.getByRole('button', { name: 'Sign out' }) !== null, 'the signed-in screen never arrived').toBe(true)
    expect(
      polite !== null && polite.textContent === '',
      'a live session was announced as ended',
    ).toBe(true)
    expect(
      assertive !== null && assertive.textContent === '',
      'a signed-in server kept shouting in the assertive region',
    ).toBe(true)

    // signedIn to signedOut: the notice rides in the same polite node.
    act(() => {
      session.endSession(session.generation())
    })
    expect(
      polite !== null && assertive !== null && document.contains(polite) && document.contains(assertive),
      'a screen change replaced one of the live regions',
    ).toBe(true)
    expect(
      polite !== null && polite.textContent === SESSION_ENDED,
      'the ended session never reached the polite region that was already mounted',
    ).toBe(true)
    expect(
      assertive !== null && assertive.textContent === '',
      'the ended session interrupted the person in the assertive region',
    ).toBe(true)

    world.behavior = null
  })

  it('try_again_says_it_is_trying', async () => {
    world.behavior = dead
    const { unmount } = render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Try again' }))

    // The next attempt hangs, so the button must show it is working and refuse to start another.
    let rejectAttempt: (err: Error) => void = () => {}
    world.behavior = () =>
      new Promise((_resolve, reject) => {
        rejectAttempt = reject
      })

    const button = screen.getByRole<HTMLButtonElement>('button', { name: 'Try again' })
    fireEvent.click(button)
    expect(button.disabled, 'Try again kept working while the retry was in flight').toBe(true)
    expect(button.textContent, 'the button did not say what it was doing').toBe('Trying...')

    await act(async () => {
      rejectAttempt(new TypeError('fetch failed'))
      await Promise.resolve()
    })
    expect(button.disabled, 'the failed retry left the button dead').toBe(false)
    expect(button.textContent, 'the failed retry did not offer itself again').toBe('Try again')

    unmount()
    world.behavior = null
  })
})

describe('the Settings screen in the page', () => {
  it('settings_replaces_the_torrents_and_back_returns', async () => {
    world.behavior = alive
    render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Settings' }))
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    expect(screen.getByRole('heading', { name: 'Server' }) !== null, 'Settings did not open').toBe(true)
    expect(screen.queryByRole('button', { name: 'Add torrent' }), 'the table stayed under Settings').toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'Back to torrents' }))
    await waitFor(() => screen.getByRole('button', { name: 'Add torrent' }))
    expect(screen.queryByRole('heading', { name: 'Server' }), 'Settings stayed after Back').toBeNull()
    world.behavior = null
  })

  it('the filter survives a trip to Settings', async () => {
    world.behavior = alive
    render(<App />)
    await waitFor(() => screen.getByRole('button', { name: /^Paused/ }))
    fireEvent.click(screen.getByRole('button', { name: /^Paused/ }))
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    fireEvent.click(screen.getByRole('button', { name: 'Back to torrents' }))
    await waitFor(() => screen.getByRole('button', { name: /^Paused/ }))
    expect(
      screen.getByRole('button', { name: /^Paused/ }).getAttribute('aria-pressed'),
      'the filter was reset by the trip',
    ).toBe('true')
    world.behavior = null
  })

  it('focus moves into Settings and back', async () => {
    world.behavior = alive
    render(<App />)
    await waitFor(() => screen.getByRole('button', { name: 'Settings' }))
    const opener = screen.getByRole('button', { name: 'Settings' })
    opener.focus()
    fireEvent.click(opener)
    expect(document.activeElement, 'focus did not move to the Settings title').toBe(
      screen.getByRole('heading', { name: 'Settings' }),
    )
    fireEvent.click(screen.getByRole('button', { name: 'Back to torrents' }))
    await waitFor(() => {
      expect(document.activeElement, 'focus did not return to the Settings button').toBe(
        screen.getByRole('button', { name: 'Settings' }),
      )
    })
    world.behavior = null
  })
})
