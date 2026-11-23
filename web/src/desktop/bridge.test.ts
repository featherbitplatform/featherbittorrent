/**
 * The bridge: the only module that speaks to the shell. In a browser it says so and stays out of the
 * way; inside the app it turns the shell's `connect` answer into the page's own `Endpoint`.
 *
 * @module desktop/bridge.test
 */
import { afterEach, expect, it, vi } from 'vitest'

const shell = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: shell.invoke }))

import {
  closeWindow,
  connect,
  desktopInfo,
  inDesktop,
  setCloseAction,
  setStartAtLogin,
  setTrayInstance,
} from './bridge'

afterEach(() => {
  shell.invoke.mockReset()
})

it('in_desktop_is_false_in_a_browser', () => {
  expect(inDesktop(), 'a plain page claimed to be inside the app').toBe(false)
})

it('connect_maps_the_command_answer_to_a_bearer_endpoint', async () => {
  shell.invoke.mockResolvedValueOnce({ kind: 'bearer', base: 'http://127.0.0.1:53211/', token: 'secret' })
  const endpoint = await connect('local')
  expect(shell.invoke).toHaveBeenCalledWith('connect', { id: 'local' })
  expect(endpoint, 'the answer did not become a bearer endpoint on a normalised base').toEqual({
    kind: 'bearer',
    base: 'http://127.0.0.1:53211',
    token: 'secret',
  })
})

it('connect_passes_the_shell_refusal_on', async () => {
  shell.invoke.mockRejectedValueOnce('This computer is not running.')
  await expect(connect('local')).rejects.toBe('This computer is not running.')
})

it('desktop_info_asks_the_shell', async () => {
  shell.invoke.mockResolvedValueOnce({ version: '0.3.0', platform: 'macos' })
  expect(await desktopInfo()).toEqual({ version: '0.3.0', platform: 'macos' })
  expect(shell.invoke).toHaveBeenCalledWith('desktop_info')
})

it('close_and_tray_commands_reach_the_shell', async () => {
  shell.invoke.mockResolvedValue(undefined)
  await setCloseAction('tray')
  expect(shell.invoke).toHaveBeenLastCalledWith('set_close_action', { action: 'tray' })
  await closeWindow()
  expect(shell.invoke).toHaveBeenLastCalledWith('close_window_now')
  await setTrayInstance('studio')
  expect(shell.invoke).toHaveBeenLastCalledWith('set_tray_instance', { name: 'studio' })
  await setStartAtLogin(true)
  expect(shell.invoke).toHaveBeenLastCalledWith('set_start_at_login', { on: true })
})
