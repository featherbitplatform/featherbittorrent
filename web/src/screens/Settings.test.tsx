// @vitest-environment jsdom
/**
 * The Settings page: one section at a time, a list on the left, and the question asked before
 * leaving a section that holds changes.
 *
 * @module screens/Settings.test
 */
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { installDialogShim } from '../dialog-shim'
import { LAST_SECTION_KEY } from '../components/settings/sections'
import type { Api } from '../api/client'

vi.mock('../components/settings/Devices', () => ({ default: () => <div>devices body</div> }))

import { Settings } from './Settings'

installDialogShim()

const SPEED = { download_kib: 100, upload_kib: null, active_downloads: 3, active_seeds: 5, active_total: 500 }

function fakeClient() {
  const saveSpeed = vi.fn((speed: typeof SPEED) =>
    Promise.resolve({ speed, downloads: {} as never }),
  )
  const client = {
    settings: () => Promise.resolve({ speed: { ...SPEED }, downloads: {} }),
    saveSpeed,
  } as unknown as Api
  return { client, saveSpeed }
}

beforeEach(() => {
  window.localStorage.clear()
})
afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

async function open(props: { section?: string | null } = {}) {
  const fake = fakeClient()
  const onBack = vi.fn()
  render(<Settings client={fake.client} onBack={onBack} {...props} />)
  await screen.findByDisplayValue('100')
  return { ...fake, onBack }
}

const list = () => screen.getByRole('navigation', { name: 'Settings sections' })
const pick = (name: string) => fireEvent.click(within(list()).getByRole('button', { name }))
const downloadField = () => screen.getByLabelText('Download limit KiB/s') as HTMLInputElement

it('shows_one_section_at_a_time', async () => {
  await open()
  expect(screen.getByLabelText('Download limit KiB/s')).toBeTruthy()
  expect(screen.getByLabelText('Active total')).toBeTruthy()
  expect(screen.queryByText('devices body')).toBeNull()
  expect(screen.queryByRole('heading', { name: 'Keyboard shortcuts' })).toBeNull()
  expect(screen.getByRole('heading', { name: 'Speed', level: 2 })).toBeTruthy()
  expect(within(list()).getByRole('button', { name: 'Speed' }).getAttribute('aria-current')).toBe('page')
  pick('Devices')
  expect(screen.getByText('devices body')).toBeTruthy()
  expect(screen.queryByLabelText('Download limit KiB/s')).toBeNull()
})

it('arrow_keys_move_and_focus_the_heading', async () => {
  await open()
  const speed = within(list()).getByRole('button', { name: 'Speed' })
  speed.focus()
  expect(speed.tabIndex).toBe(0)
  expect(within(list()).getByRole('button', { name: 'Devices' }).tabIndex).toBe(-1)
  fireEvent.keyDown(speed, { key: 'ArrowDown' })
  expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'Devices', level: 2 }))
  expect(within(list()).getByRole('button', { name: 'Devices' }).getAttribute('aria-current')).toBe('page')
  fireEvent.keyDown(list(), { key: 'End' })
  expect(screen.getByRole('heading', { name: 'Keyboard shortcuts', level: 2 })).toBeTruthy()
  fireEvent.keyDown(list(), { key: 'Home' })
  expect(screen.getByRole('heading', { name: 'Speed', level: 2 })).toBeTruthy()
  fireEvent.keyDown(list(), { key: 'ArrowUp' })
  expect(screen.getByRole('heading', { name: 'Speed', level: 2 }), 'moved past the first section').toBeTruthy()
})

it('jump_to_select_on_phone_width', async () => {
  await open()
  const jump = screen.getByLabelText('Jump to') as HTMLSelectElement
  expect(jump.tagName).toBe('SELECT')
  // The list is hidden under the sm breakpoint and the menu shown there, by class.
  expect(list().className).toContain('sm:flex')
  expect(jump.className).toContain('sm:hidden')
  fireEvent.change(jump, { target: { value: 'devices' } })
  expect(screen.getByText('devices body')).toBeTruthy()
  expect(jump.value).toBe('devices')
})

it('back_to_torrents_is_always_present', async () => {
  const { onBack } = await open()
  fireEvent.click(screen.getByRole('button', { name: 'Back to torrents' }))
  expect(onBack).toHaveBeenCalledTimes(1)
  pick('Devices')
  expect(screen.getByRole('button', { name: 'Back to torrents' })).toBeTruthy()
})

it('leaving_with_unsaved_changes_asks', async () => {
  const { saveSpeed } = await open()
  fireEvent.change(downloadField(), { target: { value: '999' } })
  pick('Devices')
  expect(screen.getByText('You have unsaved changes in Speed.')).toBeTruthy()
  expect(screen.queryByText('devices body'), 'left before the answer').toBeNull()
  fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }))
  expect(screen.getByText('devices body')).toBeTruthy()
  expect(saveSpeed, 'Discard must not save').not.toHaveBeenCalled()
  // Coming back shows what the server holds, not the draft.
  pick('Speed')
  await waitFor(() => expect(downloadField().value).toBe('100'))
  expect(screen.queryByText(/You have unsaved changes/)).toBeNull()
})

it('back_to_torrents_with_unsaved_changes_asks', async () => {
  const { onBack } = await open()
  fireEvent.change(downloadField(), { target: { value: '999' } })
  fireEvent.click(screen.getByRole('button', { name: 'Back to torrents' }))
  expect(screen.getByText('You have unsaved changes in Speed.')).toBeTruthy()
  expect(onBack).not.toHaveBeenCalled()
  fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }))
  expect(onBack).toHaveBeenCalledTimes(1)
})

it('escape_closes_the_question_and_stays', async () => {
  await open()
  fireEvent.change(downloadField(), { target: { value: '999' } })
  pick('Devices')
  const dialog = document.querySelector('dialog')!
  fireEvent(dialog, new Event('cancel', { cancelable: true }))
  await waitFor(() => expect(screen.queryByText(/You have unsaved changes/)).toBeNull())
  expect(downloadField().value, 'the draft was lost').toBe('999')
  expect(screen.queryByText('devices body')).toBeNull()
})

it('save_changes_from_the_prompt_saves_then_moves', async () => {
  const { saveSpeed } = await open()
  fireEvent.change(downloadField(), { target: { value: '999' } })
  pick('Devices')
  fireEvent.click(screen.getByRole('button', { name: 'Save changes' }))
  await screen.findByText('devices body')
  expect(saveSpeed).toHaveBeenCalledWith({ ...SPEED, download_kib: 999 })
})

it('a_refused_save_from_the_prompt_stays_on_the_section', async () => {
  const { client, saveSpeed } = await open()
  void client
  saveSpeed.mockRejectedValueOnce(new Error('no'))
  fireEvent.change(screen.getByLabelText('Active seeds'), { target: { value: '99' } })
  pick('Devices')
  fireEvent.click(screen.getByRole('button', { name: 'Save changes' }))
  await waitFor(() => expect(screen.queryByText(/You have unsaved changes/)).toBeNull())
  expect(screen.queryByText('devices body')).toBeNull()
  expect(await screen.findByRole('alert')).toBeTruthy()
})

it('opens_on_last_used_section', async () => {
  window.localStorage.setItem(LAST_SECTION_KEY, 'devices')
  render(<Settings client={fakeClient().client} onBack={vi.fn()} />)
  expect(screen.getByText('devices body')).toBeTruthy()
  cleanup()
  // A link wins over the memory.
  render(<Settings client={fakeClient().client} onBack={vi.fn()} section="settings-server" />)
  await screen.findByLabelText('Download limit KiB/s')
})

it('choosing_a_section_remembers_it_and_storage_failing_changes_nothing', async () => {
  await open()
  pick('Devices')
  expect(window.localStorage.getItem(LAST_SECTION_KEY)).toBe('devices')
  cleanup()
  vi.stubGlobal('localStorage', {
    getItem: () => {
      throw new Error('blocked')
    },
    setItem: () => {
      throw new Error('blocked')
    },
  })
  render(<Settings client={fakeClient().client} onBack={vi.fn()} />)
  await screen.findByLabelText('Download limit KiB/s')
  pick('Devices')
  expect(screen.getByText('devices body')).toBeTruthy()
})
