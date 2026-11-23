/**
 * The Sign in screen: the name, the password, and everything the page has to say about trying them.
 *
 * The layout is the Sign in prototype's, reduced to the password form the milestone actually has:
 * the mark glowing on the canvas, the word, the host and scheme this page was served over, and a
 * 380px card holding the two fields and the button. The prototype's device-key button and its
 * paragraph are milestone-2 furniture and stay out. The banners reuse the prototype's warning strip
 * for all three of the page's messages, and the plain-http warning shows only where a password
 * would actually travel in the clear: over `http:` to a host that is not loopback, because a LAN
 * box answering `localhost` is nobody else's wire.
 *
 * In the desktop app the host line and the plain-http check read the instance's address rather than the
 * page's own, since that is where the password goes, and the card offers `Save password` when the shell
 * has a keychain to keep it in (or says it has none): see `savePassword.ts`.
 *
 * The two messages from the session store are its strings, shown as they are; this screen decides
 * nothing about why a sign-in failed. They are visible banners only: the page's hoisted live
 * regions (`live.ts`, mounted in `App`) carry the same words to a screen reader, because a message
 * that mounts together with this whole screen has no region that was already around to announce it.
 * The card carries its own margins rather than sharing a column gap, so an empty message strip
 * costs the layout nothing.
 *
 * @module screens/SignIn
 */
import { useState } from 'react'
import type { FormEvent } from 'react'
import Banner from '../components/Banner'
import Button from '../components/Button'
import TextField from '../components/TextField'
import { useConnection } from '../connection'
import type { Endpoint } from '../endpoint'
import { NO_KEYCHAIN, useSavePassword } from '../savePassword'
import type { SignedOutState } from '../session'

export interface SignInProps {
  state: SignedOutState
  onSignIn: (user: string, password: string) => Promise<boolean>
}

/** The hosts a plain `http:` password is nobody else's to read: this machine, over loopback. */
const LOOPBACK_HOSTS = ['localhost', '127.0.0.1', '[::1]']

/** The warning the plan states, verbatim: kept as one string so what renders is exactly what was written. */
const PLAIN_HTTP_WARNING =
  'This page is not encrypted: the password travels in the clear. Use https through a reverse proxy beyond your LAN.'

/** Where the password goes: the page's own origin in a browser, the instance's address in the desktop app. */
function signInTarget(endpoint: Endpoint): { host: string; protocol: string; hostname: string } {
  if (endpoint.kind === 'bearer') {
    try {
      return new URL(endpoint.base)
    } catch {
      // Not an address a URL can be made of: the page's own is the best there is to show.
    }
  }
  return location
}

function travelsInPlainHttp(target: { protocol: string; hostname: string }): boolean {
  return (
    target.protocol === 'http:' && !LOOPBACK_HOSTS.includes(target.hostname)
  )
}

export default function SignIn({ state, onSignIn }: SignInProps) {
  const [user, setUser] = useState('')
  const [password, setPassword] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [keep, setKeep] = useState(false)
  const offer = useSavePassword()
  const target = signInTarget(useConnection().endpoint)

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setSubmitting(true)
    try {
      const signedIn = await onSignIn(user, password)
      // Saved only once the server took it: a wrong password kept would fail every launch.
      if (signedIn && keep && offer?.keychain === true) {
        void offer.save(user, password).catch(() => {})
      }
    } finally {
      // A sign-in that worked replaced this screen under React's feet; saying the attempt is over
      // is then a message to nobody, and harmless.
      setSubmitting(false)
    }
  }

  const insecure = travelsInPlainHttp(target)

  const hasMessage = state.notice !== null || state.error !== null || insecure

  return (
    <main className="flex min-h-dvh flex-col items-center justify-center px-4 py-12">
      <div className="mb-7 flex flex-col items-center gap-3">
        <img
          className="h-16 w-auto object-contain drop-shadow-[0_0_18px] drop-shadow-violet-400/45"
          src="/favicon.png"
          alt=""
        />
        <h1 className="m-0 text-xl font-semibold text-strong">Sign in to featherbittorrent</h1>
        <span className="font-mono text-xs text-muted">
          {target.host} &middot; {target.protocol.slice(0, -1)}
        </span>
      </div>

      <form
        className={`flex w-full max-w-[380px] flex-col gap-3.5 rounded-overlay border border-rule bg-surface p-6${hasMessage ? ' mb-7' : ''}`}
        onSubmit={submit}
      >
        <TextField
          label="User name"
          type="text"
          autoComplete="username"
          value={user}
          onChange={(event) => {
            setUser(event.target.value)
          }}
        />
        <TextField
          label="Password"
          type="password"
          autoComplete="current-password"
          value={password}
          onChange={(event) => {
            setPassword(event.target.value)
          }}
        />
        {offer !== null &&
          (offer.keychain ? (
            <label className="flex items-center gap-2 text-[13px] text-body">
              <input
                type="checkbox"
                className="h-4 w-4 accent-accent"
                checked={keep}
                onChange={(event) => {
                  setKeep(event.target.checked)
                }}
              />
              Save password
            </label>
          ) : (
            <p className="m-0 text-[13px] text-muted">{NO_KEYCHAIN}</p>
          ))}
        <Button type="submit" className="w-full" disabled={submitting}>
          Sign in
        </Button>
      </form>

      <div className="flex w-full max-w-[380px] flex-col gap-2.5">
        {state.notice !== null && <Banner>{state.notice}</Banner>}
        {state.error !== null && <Banner tone="danger">{state.error}</Banner>}
        {insecure && <Banner>{PLAIN_HTTP_WARNING}</Banner>}
      </div>
    </main>
  )
}
