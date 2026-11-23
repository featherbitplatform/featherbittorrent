// @vitest-environment jsdom
/**
 * The Desktop section of Settings: drawn only inside the app, the tray answer only with a tray, the
 * system title bar switch only on Linux (design spec 2.4: Windows always draws its own bar), shortcut
 * changes saved through the shell and applied at once, and Serve the web UI applied with its login.
 *
 * @module desktop/DesktopSettings.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { DesktopSettingsContext } from '../desktopSettings'
import { effectiveBindings, KEY_BINDINGS, setOverrides } from '../keys'
import type { Api } from '../api/client'

const shell = vi.hoisted(() => ({
  desktopInfo: vi.fn(),
  trayAvailable: vi.fn(),
  desktopPreferences: vi.fn(),
  systemTitleBar: vi.fn(),
  setSystemTitleBar: vi.fn(),
  setCloseAction: vi.fn(),
  setStartAtLogin: vi.fn(),
  setShortcuts: vi.fn(),
  servingStatus: vi.fn(),
  setServing: vi.fn(),
  addInstance: vi.fn(),
  removeInstance: vi.fn(),
  setDefaultInstance: vi.fn(),
  forgetPassword: vi.fn(),
}))
vi.mock('./bridge', () => ({ ...shell, LOCAL_ID: 'local' }))
vi.mock('../components/settings/ServerSettings', () => ({ default: () => <div>server settings</div> }))
vi.mock('../components/settings/Devices', () => ({ default: () => <div>devices</div> }))

import { Settings } from '../screens/Settings'
import { DesktopSettings, PLAIN_HTTP_WARNING } from './DesktopSettings'

const LIST = {
  default: 'local',
  keychain: true,
  items: [{ id: 'local', name: 'This computer', url: '', saved_password: false }],
}

const OFF = { enabled: false, bind: '0.0.0.0:8090', user: '', address: null }

function platform(name: 'macos' | 'windows' | 'linux'): void {
  shell.desktopInfo.mockResolvedValue({ version: '0.3.0', platform: name })
}

beforeEach(() => {
  platform('macos')
  shell.trayAvailable.mockResolvedValue(true)
  shell.desktopPreferences.mockResolvedValue({ close: 'ask', start_at_login: false })
  shell.systemTitleBar.mockResolvedValue(false)
  shell.servingStatus.mockResolvedValue(OFF)
  shell.setShortcuts.mockResolvedValue(undefined)
  shell.setCloseAction.mockResolvedValue(undefined)
})

afterEach(() => {
  cleanup()
  setOverrides([])
  for (const fn of Object.values(shell)) fn.mockReset()
})

function renderSection(props: Partial<Parameters<typeof DesktopSettings>[0]> = {}) {
  const onServingChanged = vi.fn().mockResolvedValue(undefined)
  render(
    <DesktopSettings
      list={LIST}
      current="local"
      onInstancesChanged={vi.fn().mockResolvedValue(undefined)}
      onServingChanged={onServingChanged}
      {...props}
    />,
  )
  return { onServingChanged }
}

it('desktop_section_only_in_desktop', async () => {
  const client = {} as Api
  const { unmount } = render(<Settings client={client} onBack={vi.fn()} />)
  expect(screen.queryByRole('heading', { name: 'Desktop' })).toBeNull()
  expect(screen.getByRole('heading', { name: 'Keyboard shortcuts' }), 'the browser keeps its read-only list').toBeTruthy()
  unmount()

  render(
    <DesktopSettingsContext.Provider
      value={{
        sections: (
          <DesktopSettings
            list={LIST}
            current="local"
            onInstancesChanged={vi.fn().mockResolvedValue(undefined)}
            onServingChanged={vi.fn().mockResolvedValue(undefined)}
          />
        ),
        request: null,
        onOpened: vi.fn(),
      }}
    >
      <Settings client={client} onBack={vi.fn()} />
    </DesktopSettingsContext.Provider>,
  )
  expect(screen.getByRole('heading', { name: 'Desktop' })).toBeTruthy()
  expect(screen.getAllByRole('heading', { name: 'Keyboard shortcuts' })).toHaveLength(1)
  expect(screen.getAllByRole('button', { name: 'Change' }).length, 'the desktop list is editable').toBeGreaterThan(0)
  expect(screen.getByRole('heading', { name: 'Instances' })).toBeTruthy()
  await waitFor(() => expect(shell.servingStatus).toHaveBeenCalled())
})

it('tray_choice_hidden_without_tray', async () => {
  shell.trayAvailable.mockResolvedValue(false)
  renderSection()
  await screen.findByText('Close completely')
  await waitFor(() => expect(shell.trayAvailable).toHaveBeenCalled())
  expect(screen.queryByText('Keep running in the tray')).toBeNull()
  cleanup()

  shell.trayAvailable.mockResolvedValue(true)
  renderSection()
  fireEvent.click(await screen.findByLabelText('Keep running in the tray'))
  await waitFor(() => expect(shell.setCloseAction).toHaveBeenCalledWith('tray'))
})

it('system_title_bar_switch_only_on_windows_and_linux', async () => {
  // Spec 2.4 binds over the brief's wording: Windows always draws its own bar, so only Linux has the switch.
  for (const name of ['macos', 'windows'] as const) {
    platform(name)
    renderSection()
    await waitFor(() => expect(shell.desktopInfo).toHaveBeenCalled())
    await screen.findByText('Start at login')
    expect(screen.queryByText('Use the system title bar'), `${name} has no switch`).toBeNull()
    cleanup()
    shell.desktopInfo.mockClear()
  }
  platform('linux')
  shell.setSystemTitleBar.mockResolvedValue(true)
  renderSection()
  fireEvent.click(await screen.findByLabelText('Use the system title bar'))
  await waitFor(() => expect(shell.setSystemTitleBar).toHaveBeenCalledWith(true))
})

it('shortcut_changes_are_saved_and_applied', async () => {
  renderSection()
  const binding = KEY_BINDINGS.find((b) => b.where === 'Torrent list')!
  const row = document.querySelector(`[data-shortcut="${binding.where}/${binding.action}"]`)!
  fireEvent.click(row.querySelector('button')!)
  fireEvent.keyDown(window, { key: 'F9' })
  await waitFor(() =>
    expect(shell.setShortcuts).toHaveBeenCalledWith([{ where: binding.where, action: binding.action, keys: ['F9'] }]),
  )
  const applied = effectiveBindings().find((b) => b.where === binding.where && b.action === binding.action)
  expect(applied?.keys).toEqual(['F9'])
  expect(row.textContent).toContain('F9')
})

it('serving_is_applied_with_its_login_and_shows_the_address', async () => {
  shell.setServing.mockResolvedValue({
    enabled: true,
    bind: '0.0.0.0:8090',
    user: 'alice',
    address: 'http://192.168.1.20:8090',
  })
  const { onServingChanged } = renderSection()
  await screen.findByText('Off')
  fireEvent.click(screen.getByLabelText('Serve the web UI'))
  expect((screen.getByLabelText('Address to listen on') as HTMLInputElement).value).toBe('0.0.0.0:8090')
  fireEvent.change(screen.getByLabelText('User name'), { target: { value: 'alice' } })
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'hunter2' } })
  fireEvent.click(screen.getByRole('button', { name: 'Apply' }))
  await screen.findByText('http://192.168.1.20:8090')
  expect(shell.setServing).toHaveBeenCalledWith({
    enabled: true,
    bind: '0.0.0.0:8090',
    user: 'alice',
    password: 'hunter2',
  })
  await waitFor(() => expect(onServingChanged).toHaveBeenCalledWith(null))
  expect((screen.getByLabelText('Password') as HTMLInputElement).value, 'the password is not kept').toBe('')
})

it('a_refused_serving_change_hands_the_line_to_the_page', async () => {
  shell.setServing.mockRejectedValue('Serving the web UI needs a password.')
  const { onServingChanged } = renderSection()
  await screen.findByText('Off')
  fireEvent.click(screen.getByLabelText('Serve the web UI'))
  fireEvent.change(screen.getByLabelText('User name'), { target: { value: 'alice' } })
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'wrong' } })
  fireEvent.click(screen.getByRole('button', { name: 'Apply' }))
  await waitFor(() => expect(onServingChanged).toHaveBeenCalledWith('Serving the web UI needs a password.'))
  if (screen.queryByLabelText('Password') === null) fireEvent.click(screen.getByLabelText('Serve the web UI'))
  expect((screen.getByLabelText('Password') as HTMLInputElement).value, 'cleared after a refusal too').toBe('')
  cleanup()
  renderSection({ servingProblem: 'Serving the web UI needs a password.' })
  await screen.findByText('Serving the web UI needs a password.')
})

it('an_apply_that_changes_nothing_asks_nothing', async () => {
  const onServingApplying = vi.fn()
  renderSection({ onServingApplying })
  await screen.findByText('Off')
  fireEvent.click(screen.getByRole('button', { name: 'Apply' }))
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(shell.setServing).not.toHaveBeenCalled()
  expect(onServingApplying).not.toHaveBeenCalled()
})

it('the_page_is_told_before_this_computer_restarts', async () => {
  const order: string[] = []
  const onServingApplying = vi.fn(() => order.push('applying'))
  shell.setServing.mockImplementation(() => {
    order.push('set_serving')
    return Promise.resolve(OFF)
  })
  renderSection({ onServingApplying })
  await screen.findByText('Off')
  fireEvent.click(screen.getByLabelText('Serve the web UI'))
  fireEvent.change(screen.getByLabelText('User name'), { target: { value: 'alice' } })
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'hunter2' } })
  fireEvent.click(screen.getByRole('button', { name: 'Apply' }))
  await waitFor(() => expect(order).toEqual(['applying', 'set_serving']))
})

it('the_plain_http_warning_shows_for_a_network_bind', async () => {
  renderSection()
  await screen.findByText('Off')
  expect(screen.queryByText(PLAIN_HTTP_WARNING)).toBeNull()
  fireEvent.click(screen.getByLabelText('Serve the web UI'))
  expect(screen.getByText(PLAIN_HTTP_WARNING)).toBeTruthy()
  fireEvent.change(screen.getByLabelText('Address to listen on'), { target: { value: '127.0.0.1:8090' } })
  expect(screen.queryByText(PLAIN_HTTP_WARNING)).toBeNull()
  fireEvent.change(screen.getByLabelText('Address to listen on'), { target: { value: '[::]:8090' } })
  expect(screen.getByText(PLAIN_HTTP_WARNING)).toBeTruthy()
})
