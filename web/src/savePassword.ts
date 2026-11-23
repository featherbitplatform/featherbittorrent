/**
 * What the sign-in screen is told about saving a password: nothing in a browser, and in the desktop app
 * whether the shell can keep one in the system keychain and how to hand it over.
 *
 * It lives apart from `desktop/` so the sign-in screen can read it without the browser build reaching for
 * the shell: the default is `null`, which is a browser, and only the desktop page provides a value.
 *
 * @module savePassword
 */
import { createContext, useContext } from 'react'

/** The line shown instead of the checkbox on a system with no keychain. */
export const NO_KEYCHAIN = 'This system has no keychain, so passwords are not saved.'

export interface SavePasswordOffer {
  /** Whether this system has a keychain to save in. */
  keychain: boolean
  /** Keep this password for the instance being signed in to. Called only after the sign-in worked. */
  save(user: string, password: string): Promise<void>
}

export const SavePasswordContext = createContext<SavePasswordOffer | null>(null)

/** The offer for the screen this is rendered in, or null in a browser. */
export function useSavePassword(): SavePasswordOffer | null {
  return useContext(SavePasswordContext)
}
