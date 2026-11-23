// @vitest-environment jsdom
/**
 * The desktop chunk arriving in the page: it renders when it loads, and when it cannot load the
 * window gets one line and the log gets the reason, rather than the window staying blank. The loader
 * is handed in, the way `createSessionStore` is handed a `fetch`, so this test can give it one that
 * fails.
 *
 * @module main.test
 */
import { act } from '@testing-library/react'
import { expect, it, vi } from 'vitest'
import { createRoot } from 'react-dom/client'

// `main.tsx` mounts the page the moment it is imported, and the session store takes hold of
// `globalThis.fetch` while its own module loads: both run before this file's body, so the `#root`
// the page mounts into and a fetch that never answers are set up above every import. Nothing ever
// resolves to move the mounted browser page out of `loading`, so it stays quiet outside act.
vi.hoisted((): void => {
  globalThis.fetch = (): Promise<Response> => new Promise<Response>(() => {})
  document.body.innerHTML = '<div id="root"></div>'
})

import { renderDesktop } from './main'

function blankWindow(): { container: HTMLDivElement; root: ReturnType<typeof createRoot> } {
  const container = document.createElement('div')
  document.body.append(container)
  return { container, root: createRoot(container) }
}

it('renders_the_desktop_page_when_the_chunk_loads', async () => {
  const { container, root } = blankWindow()

  await act(async () => {
    await renderDesktop(root, async () => ({ DesktopRoot: () => <div>the desktop page</div> }))
  })

  expect(container.textContent, 'a desktop chunk that loaded was not rendered').toBe('the desktop page')
  act(() => {
    root.unmount()
  })
})

it('says_so_in_one_line_when_the_chunk_fails_to_load', async () => {
  const { container, root } = blankWindow()
  const problem = new Error('chunk load failed')
  const logged = vi.spyOn(console, 'error').mockImplementation(() => {})

  await act(async () => {
    await renderDesktop(root, async () => {
      throw problem
    })
  })

  expect(
    container.textContent,
    'a desktop chunk that failed to load did not leave its one line in the window',
  ).toBe('The desktop part of the page did not load. Restart featherbittorrent.')
  expect(
    logged.mock.calls.some((args) => args.includes(problem)),
    'the reason the chunk failed was not logged',
  ).toBe(true)
  logged.mockRestore()
  act(() => {
    root.unmount()
  })
})
