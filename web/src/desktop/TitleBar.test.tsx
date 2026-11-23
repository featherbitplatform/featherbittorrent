// @vitest-environment jsdom
/**
 * The desktop title bar: room for the traffic lights on macOS, drawn window controls on Windows and
 * Linux, a bare toolbar under the system's title bar, and empty space that drags the window.
 *
 * @module desktop/TitleBar.test
 */
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

const win = vi.hoisted(() => ({
  minimize: vi.fn(() => Promise.resolve()),
  toggleMaximize: vi.fn(() => Promise.resolve()),
  close: vi.fn(() => Promise.resolve()),
  isMaximized: vi.fn(() => Promise.resolve(false)),
  onResized: vi.fn(() => Promise.resolve(() => {})),
}))
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => win }))

import { TitleBar } from './TitleBar'

afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})

const CONTROLS = ['Minimize', 'Maximize', 'Close']

async function renderBar(platform: 'macos' | 'windows' | 'linux', systemTitleBar = false) {
  const view = render(
    <TitleBar
      platform={platform}
      systemTitleBar={systemTitleBar}
      instanceSwitcher={<button type="button">This computer</button>}
      actions={<button type="button">Add torrent</button>}
    />,
  )
  // Let the maximized-state probe settle so its update lands inside act.
  await act(async () => {})
  return view
}

it('mac_bar_leaves_room_for_the_traffic_lights_and_draws_no_controls', async () => {
  const { container } = await renderBar('macos')
  const inset = container.querySelector('[data-traffic-lights]')
  expect(inset, 'no inset for the traffic lights').not.toBeNull()
  expect((inset as HTMLElement).style.width).toBe('78px')
  for (const name of CONTROLS) expect(screen.queryByRole('button', { name })).toBeNull()
  screen.getByRole('button', { name: 'This computer' })
  screen.getByRole('button', { name: 'Add torrent' })
})

it('windows_and_linux_bars_draw_three_controls', async () => {
  for (const platform of ['windows', 'linux'] as const) {
    const { container } = await renderBar(platform)
    for (const name of CONTROLS) screen.getByRole('button', { name })
    expect(container.querySelector('[data-traffic-lights]')).toBeNull()
    cleanup()
  }
})

it('system_title_bar_draws_no_controls', async () => {
  for (const platform of ['windows', 'linux'] as const) {
    const { container } = await renderBar(platform, true)
    for (const name of CONTROLS) expect(screen.queryByRole('button', { name })).toBeNull()
    expect(container.querySelector('[data-traffic-lights]')).toBeNull()
    screen.getByRole('button', { name: 'This computer' })
    screen.getByRole('button', { name: 'Add torrent' })
    cleanup()
  }
})

it('empty_space_is_a_drag_region', async () => {
  const { container } = await renderBar('windows')
  const spacer = container.querySelector('[data-title-bar-spacer]')
  expect(spacer, 'no spacer').not.toBeNull()
  expect(spacer!.hasAttribute('data-tauri-drag-region')).toBe(true)
  for (const button of screen.getAllByRole('button')) {
    expect(button.hasAttribute('data-tauri-drag-region'), `${button.textContent} drags`).toBe(false)
  }
})

it('control_buttons_call_the_window_api', async () => {
  await renderBar('linux')
  fireEvent.click(screen.getByRole('button', { name: 'Minimize' }))
  expect(win.minimize).toHaveBeenCalledTimes(1)
  fireEvent.click(screen.getByRole('button', { name: 'Maximize' }))
  expect(win.toggleMaximize).toHaveBeenCalledTimes(1)
  fireEvent.click(screen.getByRole('button', { name: 'Close' }))
  expect(win.close).toHaveBeenCalledTimes(1)
})

it('a_maximized_window_offers_restore', async () => {
  win.isMaximized.mockResolvedValueOnce(true)
  await renderBar('windows')
  screen.getByRole('button', { name: 'Restore' })
  expect(screen.queryByRole('button', { name: 'Maximize' })).toBeNull()
})
