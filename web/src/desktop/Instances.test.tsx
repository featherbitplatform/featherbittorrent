// @vitest-environment jsdom
/**
 * Managing instances: adding goes to the shell and the list is read again; a refusal shows the shell's
 * line; Remove, Make default and Forget password are offered only where they mean something.
 *
 * @module desktop/Instances.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

const shell = vi.hoisted(() => ({
  addInstance: vi.fn(),
  removeInstance: vi.fn(),
  setDefaultInstance: vi.fn(),
  forgetPassword: vi.fn(),
}))
vi.mock('./bridge', () => ({ ...shell, LOCAL_ID: 'local' }))

import { Instances } from './Instances'

afterEach(() => {
  cleanup()
  for (const fn of Object.values(shell)) fn.mockReset()
})

const LIST = {
  default: 'local',
  keychain: true,
  items: [
    { id: 'local', name: 'This computer', url: '', saved_password: false },
    { id: 'pi', name: 'Pi at home', url: 'http://pi.local:8090', saved_password: true },
  ],
}

it('adds_and_reads_the_list_again', async () => {
  shell.addInstance.mockResolvedValueOnce({ id: 'nas', name: 'NAS', url: 'https://nas.example', saved_password: false })
  const onChanged = vi.fn().mockResolvedValue(undefined)
  render(<Instances list={LIST} current="local" onChanged={onChanged} onClose={vi.fn()} />)
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'NAS' } })
  fireEvent.change(screen.getByLabelText('Address'), { target: { value: 'https://nas.example' } })
  fireEvent.click(screen.getByRole('button', { name: 'Add instance' }))
  await waitFor(() => expect(onChanged).toHaveBeenCalled())
  expect(shell.addInstance).toHaveBeenCalledWith('NAS', 'https://nas.example')
  expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('')
})

it('a_refused_add_shows_the_shells_line', async () => {
  shell.addInstance.mockRejectedValueOnce('An instance named Pi at home is already saved.')
  render(<Instances list={LIST} current="local" onChanged={vi.fn().mockResolvedValue(undefined)} onClose={vi.fn()} />)
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'pi at home' } })
  fireEvent.click(screen.getByRole('button', { name: 'Add instance' }))
  await screen.findByText('An instance named Pi at home is already saved.')
})

it('offers_only_the_actions_that_mean_something', async () => {
  shell.removeInstance.mockResolvedValue(undefined)
  shell.setDefaultInstance.mockResolvedValue(undefined)
  const onChanged = vi.fn().mockResolvedValue(undefined)
  render(<Instances list={LIST} current="pi" onChanged={onChanged} onClose={vi.fn()} />)
  expect(screen.getAllByRole('button', { name: 'Remove' })).toHaveLength(1)
  expect(screen.getAllByRole('button', { name: 'Make default' })).toHaveLength(1)
  expect(screen.getAllByRole('button', { name: 'Forget password' })).toHaveLength(1)
  fireEvent.click(screen.getByRole('button', { name: 'Make default' }))
  await waitFor(() => expect(shell.setDefaultInstance).toHaveBeenCalledWith('pi'))
  fireEvent.click(screen.getByRole('button', { name: 'Remove' }))
  await waitFor(() => expect(shell.removeInstance).toHaveBeenCalledWith('pi'))
})
