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
 * Settings owns one history entry while it is open, the way the phone's full torrent view does:
 * opening pushes it, `Back to torrents` consumes it with `history.back()`, and the browser's back (a
 * `popstate`) closes Settings instead of leaving the page. Each push carries a number, and a
 * popstate that lands on one of them is a late answer to an earlier `history.back()`, not the
 * person going back. The Torrents screen under Settings is told it is hidden, so its live loop
 * reads at the hidden-page cadence until it is shown again.
 *
 * The session and the client come from {@link useConnection}: the browser's own unless the page is
 * rendered under a provider (the desktop app's bearer connection). A desktop session that started
 * from its launch token has nothing to sign out of, so it gets no Sign out button, as `open` does not.
 *
 * @module App
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import Button from './components/Button'
import { createLiveAnnouncer, UNREACHABLE_COPY } from './live'
import type { LiveAnnouncer } from './live'
import { useConnection } from './connection'
import { useDesktopSettings } from './desktopSettings'
import { useSession } from './session'
import { Settings } from './screens/Settings'
import type { ExitGuard } from './screens/Settings'
import SignIn from './screens/SignIn'
import Torrents from './screens/Torrents'

export default function App() {
  const connection = useConnection()
  const { session } = connection
  const { state, signIn, signOut, refresh } = useSession(session)
  const politeRef = useRef<HTMLDivElement>(null)
  const assertiveRef = useRef<HTMLDivElement>(null)
  const live = useRef<LiveAnnouncer | null>(null)
  const [trying, setTrying] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  // Whether Settings owns a history entry, and the number of the last one it pushed.
  const settingsPushed = useRef(false)
  const settingsToken = useRef(0)
  // The open Settings page's unsaved-changes question, for the exits that are App's own.
  const exitGuard = useRef<ExitGuard | null>(null)
  const setExitGuard = useCallback((guard: ExitGuard | null): void => {
    exitGuard.current = guard
  }, [])
  // The desktop app asks for Settings at a section (Manage instances, or after This computer restarted).
  const desktop = useDesktopSettings()
  const [settingsSection, setSettingsSection] = useState<string | null>(null)

  /** Back to torrents: through the history when Settings owns an entry, so the entry is consumed. */
  const closeSettings = useCallback((): void => {
    const owned = settingsPushed.current
    settingsPushed.current = false
    setSettingsOpen(false)
    if (owned) window.history.back()
  }, [])

  useEffect(() => {
    const onPop = (event: PopStateEvent): void => {
      if (!settingsPushed.current) return
      // A stale answer to either screen's earlier `history.back()` (Settings' or the full view's).
      const landed = event.state as { fbttSettings?: number; fbttFull?: number } | null
      if (landed?.fbttSettings !== undefined || landed?.fbttFull !== undefined) return
      const guarded = exitGuard.current
      if (guarded?.dirty() === true) {
        // Settings has changes that were not saved: put the entry the browser just left back, so the
        // page and the history still agree, and ask. Settings closes (and consumes that entry) only on
        // an answer that lets it.
        settingsToken.current += 1
        window.history.pushState({ fbttSettings: settingsToken.current }, '')
        guarded.guard(closeSettings)
        return
      }
      settingsPushed.current = false
      setSettingsOpen(false)
    }
    window.addEventListener('popstate', onPop)
    return () => {
      window.removeEventListener('popstate', onPop)
    }
  }, [closeSettings])

  const openSettings = useCallback((section: string | null = null): void => {
    setSettingsSection(section)
    if (!settingsPushed.current) {
      settingsToken.current += 1
      window.history.pushState({ fbttSettings: settingsToken.current }, '')
      settingsPushed.current = true
    }
    setSettingsOpen(true)
  }, [])

  /** The header's own way back to Torrents: asks first when Settings holds changes. False when it is waiting for the answer. */
  function showTorrents(): boolean {
    const guarded = exitGuard.current
    if (guarded?.dirty() === true) {
      guarded.guard(closeSettings)
      return false
    }
    closeSettings()
    return true
  }

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
  }, [session])

  // Going back hands the focus to the button that opened Settings, which is on screen again by
  // then. Settings closing because Add torrent asked for the screen back is the exception: that
  // click opens the dialog in the same render, its layout effect has already put the focus inside
  // it by the time this runs, and pulling focus to the opener would strand it behind a modal.
  const wasOpen = useRef(false)
  useEffect(() => {
    if (wasOpen.current && !settingsOpen) {
      if (document.querySelector('dialog[open]') === null) {
        document.querySelector<HTMLElement>('[data-settings-opener]')?.focus()
      }
    }
    wasOpen.current = settingsOpen
  }, [settingsOpen])

  // Settings belongs to a live workspace: when the session ends, the next sign-in starts on Torrents.
  const workspace = state.phase === 'signedIn' || state.phase === 'open'

  // A desktop request is answered once the workspace is on screen, and then handed back as done.
  const request = desktop?.request ?? null
  const onOpened = desktop?.onOpened
  const answered = useRef<number | null>(null)
  useEffect(() => {
    if (!workspace || request === null || answered.current === request.seq) return
    answered.current = request.seq
    openSettings(request.section)
    onOpened?.(request.seq)
  }, [workspace, request, onOpened, openSettings])
  if (!workspace && settingsOpen) setSettingsOpen(false)
  // ...and the entry Settings pushed goes back with it.
  useEffect(() => {
    if (!workspace && settingsPushed.current) {
      settingsPushed.current = false
      window.history.back()
    }
  }, [workspace])

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
  // The launch token is the desktop's own engine letting the page in; there is no sign-in to undo.
  const launched =
    connection.endpoint.kind === 'bearer' &&
    connection.endpoint.token !== null &&
    session.token() === connection.endpoint.token
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
      ) : (
        <>
          {settingsOpen && (
            <Settings onExitGuard={setExitGuard} onBack={closeSettings} section={settingsSection} />
          )}
          {/* Hidden, not unmounted: the filter and the selection stay in state, and the table puts its scroll position back when it is measured again.
              `contents` while shown, so the wrapper draws no box and the screen's own root is the one
              its parent lays out: the desktop frame sizes `h-dvh` as `height: 100%` of the space under
              the title bar, and a percentage through a block of auto height is no height at all, which
              cut the Torrents screen (sidebar and all) to its content. Preflight's `display: none
              !important` on `[hidden]` still hides it while Settings is up. */}
          <div hidden={settingsOpen} className="contents">
            <Torrents
              hidden={settingsOpen}
              onSettings={(section) => {
                openSettings(section ?? null)
              }}
              onShowTorrents={showTorrents}
              signedIn={state.phase === 'signedIn' && !launched}
              signOutError={signOutError}
              onSignOut={() => void signOut()}
            />
          </div>
        </>
      )}
    </>
  )
}
