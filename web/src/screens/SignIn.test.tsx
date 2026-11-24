// @vitest-environment jsdom
/**
 * The sign-in screen's desktop extras: `Save password` when the shell has a keychain, the line saying
 * there is none when it does not, nothing at all in a browser, and the instance's own address in the
 * host line.
 *
 * @module screens/SignIn.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { ConnectionContext, createConnection } from '../connection'
import { NO_KEYCHAIN, SavePasswordContext } from '../savePassword'
import type { SavePasswordOffer } from '../savePassword'
import SignIn from './SignIn'

afterEach(cleanup)

const SIGNED_OUT = { phase: 'signedOut' as const, notice: null, error: null }
const REMOTE = createConnection({ kind: 'bearer', base: 'http://pi.local:8090', token: null })

function renderWith(offer: SavePasswordOffer | null, onSignIn = vi.fn().mockResolvedValue(true)) {
  render(
    <ConnectionContext.Provider value={REMOTE}>
      <SavePasswordContext.Provider value={offer}>
        <SignIn state={SIGNED_OUT} onSignIn={onSignIn} />
      </SavePasswordContext.Provider>
    </ConnectionContext.Provider>,
  )
  return onSignIn
}

function signIn(user: string, password: string): void {
  fireEvent.change(screen.getByLabelText('User name'), { target: { value: user } })
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: password } })
  fireEvent.click(screen.getByRole('button', { name: 'Sign in' }))
}

it('save_password_hidden_without_keychain', () => {
  renderWith({ keychain: false, save: vi.fn() })
  expect(screen.queryByLabelText('Save password')).toBeNull()
  expect(screen.getByText(NO_KEYCHAIN).textContent).toBe(
    'This system has no keychain, so passwords are not saved.',
  )
})

it('nothing_about_saving_in_a_browser', () => {
  renderWith(null)
  expect(screen.queryByLabelText('Save password')).toBeNull()
  expect(screen.queryByText(NO_KEYCHAIN)).toBeNull()
})

it('a_checked_save_password_is_saved_after_the_sign_in_worked', async () => {
  const save = vi.fn().mockResolvedValue(undefined)
  renderWith({ keychain: true, save })
  fireEvent.click(screen.getByLabelText('Save password'))
  signIn('alice', 'hunter2')
  await waitFor(() => expect(save).toHaveBeenCalledWith('alice', 'hunter2'))
})

it('a_refused_password_is_not_saved', async () => {
  const save = vi.fn().mockResolvedValue(undefined)
  const onSignIn = renderWith({ keychain: true, save }, vi.fn().mockResolvedValue(false))
  fireEvent.click(screen.getByLabelText('Save password'))
  signIn('alice', 'wrong')
  await waitFor(() => expect(onSignIn).toHaveBeenCalled())
  expect(save).not.toHaveBeenCalled()
})

it('the_host_line_names_the_instance_and_warns_about_plain_http', () => {
  renderWith(null)
  expect(screen.getByText(/pi\.local:8090/)).toBeTruthy()
  expect(screen.getByText(/password travels in the clear/)).toBeTruthy()
})
