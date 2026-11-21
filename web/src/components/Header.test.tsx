// @vitest-environment jsdom
import { cleanup, render } from '@testing-library/react'
import { afterEach, expect, it } from 'vitest'
import Header from './Header'

afterEach(cleanup)

/** A version number that belongs to the engine library and to nothing else in this test: if it reaches the visible text the header is quoting the server instead of itself. */
const ENGINE_VERSION = '0.11.1'

const headerText = (container: HTMLElement): string =>
  container.querySelector('header')?.textContent ?? ''

function renderHeader(dhtNodes: number | null) {
  return render(
    <Header
      engineVersion={ENGINE_VERSION}
      link={{ phase: 'live' }}
      totalDown={1_234}
      totalUp={5_678}
      dhtNodes={dhtNodes}
      signedIn
      onSignOut={() => {}}
    />,
  )
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
