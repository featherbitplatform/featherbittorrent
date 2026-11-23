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
  shell.startupProblem.mockResolvedValue({ message: 'featherbittorrent is already running.' })
  render(<StartupProblemHost />)
  await screen.findByText('featherbittorrent is already running.')
  expect(screen.getByRole('alertdialog', { name: STARTUP_PROBLEM_TITLE })).toBeTruthy()
  fireEvent.click(screen.getByRole('button', { name: 'Quit' }))
  expect(shell.quitApp).toHaveBeenCalledTimes(1)
})

it('a_start_that_went_fine_draws_nothing', async () => {
  shell.startupProblem.mockResolvedValue(null)
  const { container } = render(<StartupProblemHost />)
  await waitFor(() => expect(shell.startupProblem).toHaveBeenCalled())
  expect(container.innerHTML).toBe('')
})
