/**
 * The bridge: the only module that speaks to the shell. In a browser it says so and stays out of the
 * way; inside the app it turns the shell's `connect` answer into the page's own `Endpoint`.
 *
 * @module desktop/bridge.test
 */
import { afterEach, expect, it, vi } from 'vitest'

const shell = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: shell.invoke }))

import { connect, desktopInfo, inDesktop, listInstances, probeInstance, savePassword } from './bridge'

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

it('connect_keeps_a_null_token_for_an_instance_that_needs_a_sign_in', async () => {
  shell.invoke.mockResolvedValueOnce({ kind: 'bearer', base: 'http://pi.local:8090', token: null })
  expect(await connect('pi')).toEqual({ kind: 'bearer', base: 'http://pi.local:8090', token: null })
})

it('instance_commands_carry_their_arguments', async () => {
  shell.invoke.mockResolvedValueOnce({
    default: 'local',
    keychain: true,
    items: [{ id: 'pi', name: 'Pi', url: 'http://pi.local:8090/', saved_password: false }],
  })
  expect((await listInstances()).items[0]!.url).toBe('http://pi.local:8090')
  expect(shell.invoke).toHaveBeenLastCalledWith('list_instances')
  shell.invoke.mockResolvedValueOnce({ reachable: true, cors_ok: false })
  expect(await probeInstance('pi')).toEqual({ reachable: true, cors_ok: false })
  expect(shell.invoke).toHaveBeenLastCalledWith('probe_instance', { id: 'pi' })
  shell.invoke.mockResolvedValueOnce(null)
  await savePassword('pi', 'alice', 'hunter2')
  expect(shell.invoke).toHaveBeenLastCalledWith('save_password', { id: 'pi', user: 'alice', password: 'hunter2' })
})
