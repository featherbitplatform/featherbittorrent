// @vitest-environment jsdom
/**
 * The first-close question: each answer is saved through the shell and then applied by closing the
 * window again, and the shell's `close-requested` event is what opens it.
 *
 * @module desktop/CloseChoice.test
 */
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

const shell = vi.hoisted(() => ({
  calls: [] as string[],
  setCloseAction: vi.fn(),
  closeWindow: vi.fn(),
  onCloseRequested: vi.fn(),
}))
vi.mock('./bridge', () => ({
  setCloseAction: shell.setCloseAction,
  closeWindow: shell.closeWindow,
  onCloseRequested: shell.onCloseRequested,
}))

import { installDialogShim } from '../dialog-shim'
import { MODAL_DIALOG } from '../modal'
import { CloseChoice, CloseChoiceHost } from './CloseChoice'

installDialogShim()

afterEach(() => {
  cleanup()
  shell.calls.length = 0
  shell.setCloseAction.mockReset()
  shell.closeWindow.mockReset()
  shell.onCloseRequested.mockReset()
})

function recordCalls(): void {
  shell.setCloseAction.mockImplementation((action: string) => {
    shell.calls.push(`save ${action}`)
    return Promise.resolve()
  })
  shell.closeWindow.mockImplementation(() => {
    shell.calls.push('close')
    return Promise.resolve()
  })
}

it('close_choice_saves_and_applies_each_answer', async () => {
  recordCalls()
  for (const [label, action] of [
    ['Close completely', 'quit'],
    ['Keep running in the tray', 'tray'],
  ] as const) {
    const onClose = vi.fn()
    render(<CloseChoice onClose={onClose} />)
    expect(screen.getByText('What should closing the window do?')).toBeTruthy()
    expect(screen.getByText('You can change this later in Settings.')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: label }))
    await waitFor(() => expect(onClose).toHaveBeenCalled())
    expect(shell.calls, `the ${action} answer was not saved and then applied`).toEqual([
      `save ${action}`,
      'close',
    ])
    shell.calls.length = 0
    cleanup()
  }
})

it('a_refused_save_keeps_the_window_and_says_why', async () => {
  shell.setCloseAction.mockRejectedValueOnce('cannot write desktop.yaml: denied')
  const onClose = vi.fn()
  render(<CloseChoice onClose={onClose} />)
  fireEvent.click(screen.getByRole('button', { name: 'Close completely' }))
  await screen.findByText('cannot write desktop.yaml: denied')
  expect(shell.closeWindow).not.toHaveBeenCalled()
  expect(onClose).not.toHaveBeenCalled()
})

it('the_shell_event_opens_the_question', async () => {
  let ask: (() => void) | null = null
  const unlisten = vi.fn()
  shell.onCloseRequested.mockImplementation((handler: () => void) => {
    ask = handler
    return Promise.resolve(unlisten)
  })
  const { unmount } = render(<CloseChoiceHost />)
  expect(screen.queryByText('What should closing the window do?')).toBeNull()
  await waitFor(() => expect(ask).not.toBeNull())
  act(() => ask!())
  await screen.findByText('What should closing the window do?')
  unmount()
  await waitFor(() => expect(unlisten).toHaveBeenCalled())
})

it('the_question_is_a_centred_shared_modal', () => {
  // Preflight's `margin: 0` pinned this dialog under the traffic lights; the shared classes put the
  // `margin: auto` back that a browser centres a modal with.
  render(<CloseChoice onClose={vi.fn()} />)
  const dialog = document.querySelector('dialog')
  expect(dialog, 'the question is not a dialog').not.toBeNull()
  expect(dialog!.className).toContain(MODAL_DIALOG)
  expect(dialog!.className.split(' ')).toContain('m-auto')
})
