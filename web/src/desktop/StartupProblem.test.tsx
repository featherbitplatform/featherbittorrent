// @vitest-environment jsdom
/**
 * A failed start of This computer: the shell's line over the whole window, and Quit, which asks the shell
 * to quit (the exit code is the shell's to choose). A start that went fine draws nothing.
 *
 * @module desktop/StartupProblem.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

const shell = vi.hoisted(() => ({
  startupProblem: vi.fn(),
  quitApp: vi.fn(() => Promise.resolve()),
}))
vi.mock('./bridge', () => ({ startupProblem: shell.startupProblem, quitApp: shell.quitApp }))

import { STARTUP_PROBLEM_TITLE, StartupProblemHost } from './StartupProblem'

afterEach(() => {
  cleanup()
  shell.startupProblem.mockReset()
  shell.quitApp.mockClear()
})

it('a_startup_problem_shows_the_line_and_quit', async () => {
  shell.startupProblem.mockResolvedValue({ state: 'problem', message: 'featherbittorrent is already running.' })
  render(<StartupProblemHost />)
  await screen.findByText('featherbittorrent is already running.')
  expect(screen.getByRole('alertdialog', { name: STARTUP_PROBLEM_TITLE })).toBeTruthy()
  fireEvent.click(screen.getByRole('button', { name: 'Quit' }))
  expect(shell.quitApp).toHaveBeenCalledTimes(1)
})

it('a_start_that_went_fine_draws_nothing', async () => {
  shell.startupProblem.mockResolvedValue({ state: 'fine' })
  const { container } = render(<StartupProblemHost />)
  await waitFor(() => expect(shell.startupProblem).toHaveBeenCalled())
  expect(container.innerHTML).toBe('')
})

it('a_slow_start_is_asked_about_again_until_it_settles', async () => {
  shell.startupProblem
    .mockResolvedValueOnce({ state: 'starting' })
    .mockResolvedValueOnce({ state: 'starting' })
    .mockResolvedValueOnce({ state: 'problem', message: 'This computer could not start: no port' })
  render(<StartupProblemHost />)
  await screen.findByText('This computer could not start: no port')
  expect(shell.startupProblem).toHaveBeenCalledTimes(3)
})

it('the_problem_screen_replaces_what_it_wraps', async () => {
  shell.startupProblem.mockResolvedValue({ state: 'problem', message: 'featherbittorrent is already running.' })
  render(
    <StartupProblemHost>
      <p>the workspace</p>
    </StartupProblemHost>,
  )
  expect(screen.getByText('the workspace')).toBeTruthy()
  await screen.findByText('featherbittorrent is already running.')
  expect(screen.queryByText('the workspace')).toBeNull()
})
