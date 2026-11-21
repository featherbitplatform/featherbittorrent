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
 * The mark is `/favicon.png`, the one copy the server has: `web/public/` is copied into the built
 * page as it is, so this file, the sign-in screen's mark and the tab icon are the same bytes and
 * the same request. Its `alt` is empty because the word beside it is the name, and a screen reader
 * would otherwise hear it twice.
 *
 * @module App
 */
import Button from './components/Button'
import SignIn from './screens/SignIn'
import { useSession } from './session'

export default function App() {
  const { state, signIn, signOut } = useSession()

  if (state.phase === 'signedOut') {
    return <SignIn state={state} onSignIn={signIn} />
  }

  return (
    <main className="flex min-h-dvh flex-col items-center justify-center gap-4">
      <img className="h-16 w-auto" src="/favicon.png" alt="" />
      <h1 className="text-base font-medium tracking-wide text-strong">featherbittorrent</h1>
      {state.phase === 'signedIn' && (
        <Button variant="secondary" onClick={() => void signOut()}>
          Sign out
        </Button>
      )}
    </main>
  )
}
