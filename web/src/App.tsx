/**
 * The page: the sign-in screen, or the workspace that screen hands off to.
 *
 * A person with a working session (or a server with logins off) gets the Torrents screen: the
 * header with the mark and the word, the state filters, and the live table. While the first answer
 * about the cookie is still on its way the page shows only the mark and the word, so the sign-in
 * screen never flashes in front of somebody who is already signed in, and the workspace does not
 * start reading through a cookie nobody has vouched for yet. `open` shows the workspace without a
 * Sign out button, because there is no session to end.
 *
 * The two states that are not "ask what this cookie is" or "the workspace" get their own small
 * screens between the sign-in card and the workspace: `unreachable` says the server gave no
 * readable answer and offers to ask it again rather than asking for a password, and a sign-out the
 * server refused keeps the person signed in with the notice riding above the table, because their
 * session is still live and telling them otherwise would send them to a sign-in form they do not
 * need.
 *
 * The page's two live regions are mounted here as the first two things it ever renders, outside
 * the per-phase tree, so they are the same nodes for the page's whole life no matter which screen
 * comes or goes: announcements need a region that was already there when the news arrives, and a
 * region inside a screen leaves with it. Every visible message stays rendered where it always was;
 * {@link live} mirrors its text into the polite or the assertive region, which is what makes the
 * announcement land. `the_live_regions_outlive_the_screens_in_the_page` is the check that they
 * really do outlive the screens, as mounted in this page and not just in a unit harness.
 *
 * @module App
 */
import { useEffect, useRef, useState } from 'react'
import Button from './components/Button'
import { createLiveAnnouncer, UNREACHABLE_COPY } from './live'
import type { LiveAnnouncer } from './live'
import { session, useSession } from './session'
import { Settings } from './screens/Settings'
import SignIn from './screens/SignIn'
import Torrents from './screens/Torrents'

export default function App() {
  const { state, signIn, signOut, refresh } = useSession()
  const politeRef = useRef<HTMLDivElement>(null)
  const assertiveRef = useRef<HTMLDivElement>(null)
  const live = useRef<LiveAnnouncer | null>(null)
  const [trying, setTrying] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)

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

  const signOutError = state.phase === 'signedIn' && state.error !== null ? state.error : null
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
      ) : state.phase === 'loading' ? (
        <main className="flex min-h-dvh flex-col items-center justify-center">
          <img className="h-16 w-auto" src="/favicon.png" alt="" />
          <h1 className="mt-4 text-base font-medium tracking-wide text-strong">featherbittorrent</h1>
        </main>
      ) : settingsOpen ? (
        <Settings
          onBack={() => {
            setSettingsOpen(false)
          }}
        />
      ) : (
        <Torrents
          onSettings={() => {
            setSettingsOpen(true)
          }}
          signedIn={state.phase === 'signedIn'}
          signOutError={signOutError}
          onSignOut={() => void signOut()}
        />
      )}
    </>
  )
}
