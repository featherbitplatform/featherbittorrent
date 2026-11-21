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
 * The mark is `/favicon.png`, the one copy the server has: `web/public/` is copied into the built
 * page as it is, so this file, the sign-in screen's mark and the tab icon are the same bytes and
 * the same request. Its `alt` is empty because the word beside it is the name, and a screen reader
 * would otherwise hear it twice.
 *
 * @module App
 */
import Banner from './components/Banner'
import Button from './components/Button'
import SignIn from './screens/SignIn'
import { useSession } from './session'

export default function App() {
  const { state, signIn, signOut, refresh } = useSession()

  if (state.phase === 'signedOut') {
    return <SignIn state={state} onSignIn={signIn} />
  }

  // The server gave no readable answer to the status question. No password can fix that, so this
  // screen offers the one thing that can: asking again. The message sits in a live region marked
  // assertive, because not reaching the server is what just happened and what the person needs to
  // know before anything else on the page.
  if (state.phase === 'unreachable') {
    return (
      <main className="flex min-h-dvh flex-col items-center justify-center gap-4">
        <img className="h-16 w-auto" src="/favicon.png" alt="" />
        <div role="status" aria-live="assertive" className="text-base font-medium text-strong">
          Cannot reach the server.
        </div>
        <Button variant="secondary" onClick={() => void refresh()}>
          Try again
        </Button>
      </main>
    )
  }

  const signOutFailed = state.phase === 'signedIn' && state.error !== null
  return (
    <main className="flex min-h-dvh flex-col items-center justify-center">
      <img className="mb-4 h-16 w-auto" src="/favicon.png" alt="" />
      <h1 className="text-base font-medium tracking-wide text-strong">featherbittorrent</h1>
      {/* Mounted whether or not there is a banner inside, so a sign-out that fails gets announced
          rather than blinking on as a finished element nobody was told about. The margins are
          conditional so the empty region takes no space. */}
      <div
        role="status"
        aria-live="polite"
        className={`w-full max-w-[380px]${signOutFailed ? ' mt-4' : ''}`}
      >
        {signOutFailed && <Banner tone="danger">{state.error}</Banner>}
      </div>
      {state.phase === 'signedIn' && (
        <Button variant="secondary" className="mt-4" onClick={() => void signOut()}>
          Sign out
        </Button>
      )}
    </main>
  )
}
