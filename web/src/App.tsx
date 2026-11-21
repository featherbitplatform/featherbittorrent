/**
 * The page: the sign-in screen, or the workspace that screen hands off to.
 *
 * Task 4's skeleton is now the signed-in half of this file rather than the whole page: the mark and
 * the word on the canvas is what a person with a working session sees, plus the button that ends it.
 * While the first answer about the cookie is still on its way it shows the same thing, so the sign
 * in screen never flashes in front of somebody who is already signed in, and `open` (logins off)
 * shows it without the sign-out button, because there is no session to end. Task 6 replaces the
 * placeholder with the torrent table.
 *
 * The two states that are not "the workspace is coming" get their own small screens between the sign
 * in card and the placeholder: `unreachable` says the server gave no readable answer and offers to
 * ask it again rather than asking for a password, and a sign-out the server refused keeps the person
 * signed in with the banner saying so, because their session is still live and telling them otherwise
 * would send them to a sign-in form they do not need.
 *
 * The page's two live regions are mounted here as the first two things it ever renders, outside the
 * per-phase tree, so they are the same nodes for the page's whole life no matter which screen comes
 * or goes: announcements need a region that was already there when the news arrives, and a region
 * inside a screen leaves with it. Every visible message stays rendered where it always was;
 * {@link live} mirrors its text into the polite or the assertive region, which is what makes the
 * announcement land.
 *
 * @module App
 */
import { useEffect, useRef, useState } from 'react'
import Banner from './components/Banner'
import Button from './components/Button'
import { createLiveAnnouncer, UNREACHABLE_COPY } from './live'
import type { LiveAnnouncer } from './live'
import { session, useSession } from './session'
import SignIn from './screens/SignIn'

export default function App() {
  const { state, signIn, signOut, refresh } = useSession()
  const politeRef = useRef<HTMLDivElement>(null)
  const assertiveRef = useRef<HTMLDivElement>(null)
  const live = useRef<LiveAnnouncer | null>(null)
  const [trying, setTrying] = useState(false)

  // The announcer writes into the two nodes above and follows the store. The regions are React's;
  // this only ever sets their text, and React never gives them children to fight over.
  useEffect(() => {
    const polite = politeRef.current
    const assertive = assertiveRef.current
    if (polite === null || assertive === null) return
    const announcer = createLiveAnnouncer(session, { polite, assertive })
    live.current = announcer
    return () => {
      live.current = null
      announcer.stop()
    }
  }, [])

  async function retry(): Promise<void> {
    setTrying(true)
    try {
      await refresh()
      // Still no answer: the same sentence has to be news twice, so blank the assertive region and
      // let the announcer say it again. A state change announces by itself, so this is only for the
      // case where nothing changed because nothing was there to change.
      if (session.get().phase === 'unreachable') live.current?.reannounce()
    } finally {
      setTrying(false)
    }
  }

  const signOutFailed = state.phase === 'signedIn' && state.error !== null
  return (
    <>
      <div ref={politeRef} role="status" aria-live="polite" className="sr-only" />
      <div ref={assertiveRef} role="status" aria-live="assertive" className="sr-only" />
      {state.phase === 'signedOut' ? (
        <SignIn state={state} onSignIn={signIn} />
      ) : state.phase === 'unreachable' ? (
        <main className="flex min-h-dvh flex-col items-center justify-center gap-4">
          <img className="h-16 w-auto" src="/favicon.png" alt="" />
          <div className="text-base font-medium text-strong">{UNREACHABLE_COPY}</div>
          <Button variant="secondary" disabled={trying} onClick={() => void retry()}>
            {trying ? 'Trying...' : 'Try again'}
          </Button>
        </main>
      ) : (
        <main className="flex min-h-dvh flex-col items-center justify-center">
          <img className="mb-4 h-16 w-auto" src="/favicon.png" alt="" />
          <h1 className="text-base font-medium tracking-wide text-strong">featherbittorrent</h1>
          <div className={`w-full max-w-[380px]${signOutFailed ? ' mt-4' : ''}`}>
            {signOutFailed && <Banner tone="danger">{state.error}</Banner>}
          </div>
          {state.phase === 'signedIn' && (
            <Button variant="secondary" className="mt-4" onClick={() => void signOut()}>
              Sign out
            </Button>
          )}
        </main>
      )}
    </>
  )
}
