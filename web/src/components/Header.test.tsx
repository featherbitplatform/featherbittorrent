// @vitest-environment jsdom
import { act, cleanup, render } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import Header from './Header'
import type { HeaderProps } from './Header'
import { TitleBarSlotsContext } from './titleBarSlots'

afterEach(cleanup)

/** A version number that belongs to the engine library and to nothing else in this test: if it reaches the visible text the header is quoting the server instead of itself. */
const ENGINE_VERSION = '0.11.1'

const headerText = (container: HTMLElement): string =>
  container.querySelector('header')?.textContent ?? ''

const headerProps = (over: Partial<HeaderProps>): HeaderProps => ({
  engineVersion: ENGINE_VERSION,
  link: { phase: 'live' },
  totalDown: 1_234,
  totalUp: 5_678,
  dhtNodes: 123,
  signedIn: true,
  onSignOut: () => {},
  onSettings: () => {},
  onAdd: () => {},
  ...over,
})

function renderHeader(dhtNodes: number | null) {
  return render(<Header {...headerProps({ dhtNodes })} />)
}

it('the_header_version_is_the_apps_own', () => {
  const { container } = renderHeader(123)
  const text = headerText(container)

  // `__APP_VERSION__` is this bundle's own version, injected at build time from the root
  // Cargo.toml: what the reader must see is the app they are looking at, not the hash of some
  // library the server happens to link. The pattern guard is here so a missing `define` - which
  // Vite would leave as a bare identifier the test could trip over - is a loud failure too.
  expect(/^\d+\.\d+\.\d+$/.test(__APP_VERSION__), '__APP_VERSION__ is not a version string').toBe(
    true,
  )
  expect(text.includes(`v${__APP_VERSION__}`), 'the header did not show the built-in app version').toBe(
    true,
  )
  expect(text.includes(ENGINE_VERSION), 'the engine version leaked into the header text').toBe(false)

  // The engine version is not lost, only demoted: it rides the version text's tooltip, where it
  // is useful to the person filing a bug and harmless to everyone else.
  const versionSpan = container.querySelector('span[title]') as HTMLElement | null
  expect(
    versionSpan !== null && versionSpan.getAttribute('title') === `engine ${ENGINE_VERSION}`,
    'the engine version is not kept as the version text tooltip',
  ).toBe(true)

  // The DHT count is always beside the rates - a number, muted, plain - and gives way to the
  // rates below 768 px rather than pushing the actions off the strip.
  expect(text.includes('DHT 123'), 'the DHT count is missing from the header').toBe(true)
  const dht = Array.from(container.querySelectorAll('span')).find((span) =>
    (span.textContent ?? '').startsWith('DHT '),
  ) as HTMLElement | undefined
  expect(
    dht !== undefined && dht.className.includes('max-md:hidden'),
    'the DHT count is not hidden at phone width',
  ).toBe(true)

  // A server with nothing to say says zero: the slot stays, it does not vanish.
  const { container: nullDht } = renderHeader(null)
  expect(
    headerText(nullDht).includes('DHT 0'),
    'no DHT count when the server gives null',
  ).toBe(true)
})

it('the_countdown_restarts_on_equal_pauses', () => {
  // The ladder ends 30, 30, 30: two consecutive failures can carry the same pause. What must
  // restart the count is the failure - the loop's per-round count, which `live.ts` puts on every
  // lost report as `attempt` - not the pause's number, or the second 30 s pause keeps the first
  // one's spent zero and the pill sticks at "Reconnecting..." for the whole wait.
  vi.useFakeTimers()
  try {
    const { container, rerender } = render(
      <Header {...headerProps({ link: { phase: 'lost', retryIn: 30, attempt: 1 } })} />,
    )
    expect(
      headerText(container).includes('Reconnecting in 30 s'),
      'the first 30 s failure did not start its countdown at 30 s',
    ).toBe(true)

    act(() => {
      vi.advanceTimersByTime(30_000)
    })
    expect(
      headerText(container).includes('Reconnecting...'),
      'the first pause did not end in the retry message',
    ).toBe(true)

    // The next failure, same pause and a new count: the pill starts over, not from the zero the
    // first pause left behind.
    rerender(<Header {...headerProps({ link: { phase: 'lost', retryIn: 30, attempt: 2 } })} />)
    expect(
      headerText(container).includes('Reconnecting in 30 s'),
      "the second 30 s pause kept the first pause's spent count instead of restarting the countdown",
    ).toBe(true)
  } finally {
    vi.useRealTimers()
  }
})

it('inside_the_desktop_the_header_moves_into_the_title_bar', () => {
  const lead = document.createElement('div')
  const actions = document.createElement('div')
  document.body.append(lead, actions)
  const { container } = render(
    <TitleBarSlotsContext.Provider value={{ switcher: null, lead, actions }}>
      <Header {...headerProps({})} />
    </TitleBarSlotsContext.Provider>,
  )
  expect(container.querySelector('header'), 'the header drew its own strip as well').toBeNull()
  expect(lead.textContent).toContain('featherbittorrent')
  expect(lead.textContent).toContain('Connected')
  expect(actions.textContent).toContain('Add torrent')
  expect(actions.textContent).toContain('Settings')
  lead.remove()
  actions.remove()
})
