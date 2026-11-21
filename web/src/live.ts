/**
 * The page's two live regions: the mouths it speaks through.
 *
 * An announcement lands when text appears inside a live region that was already on the page. A
 * message that mounts together with its region - or with a whole screen that arrives replacing
 * another - is news nobody hears, because the element carrying the news is itself brand new and
 * the screen reader had no reason to be watching it yet. So the page has exactly two regions,
 * mounted once in `App` outside every screen, and this module decides which words belong in which:
 * `polite` for what a person can read when it suits them (the "session ended" notice, a sign-out
 * that failed while the session was still live), `assertive` for what just happened to them (a
 * refused sign-in, a server that stopped answering).
 *
 * The screens keep their visible banners exactly where they are; what moved here is the
 * announcing. The mapping from a session state to a message lives in this file and is the only
 * writer of the two regions' text, and the announcer never replaces the nodes it was handed -
 * `the_live_regions_outlive_the_screens` is what keeps that promise.
 *
 * @module live
 */
import type { SessionState, SessionStore } from './session'

/** The one line the unreachable screen says. Shared between the visible text and the announcer so the two can never drift apart. */
export const UNREACHABLE_COPY = 'Cannot reach the server.'

/** The two nodes the page announces through. App mounts them once; this module only ever writes text into them. */
export interface LiveRegions {
  polite: HTMLElement
  assertive: HTMLElement
}

/** The running announcer: the regions it writes, how to stop it, and how to make it repeat itself. */
export interface LiveAnnouncer {
  /** The polite region as it stands now. Exposed through getters so a test can hold the mount to its nodes. */
  polite: HTMLElement
  assertive: HTMLElement
  /** Stop following the store. The regions stay mounted; they just stop being written to. */
  stop(): void
  /** Say the assertive words again: blank them now, restore them a moment later, so the same sentence registers as news twice. */
  reannounce(): void
}

/** What each state has to say, and through which region it says it. */
export function messageFor(state: SessionState): { polite: string | null; assertive: string | null } {
  switch (state.phase) {
    case 'signedOut':
      // The notice is context a person reads when they get to it; the reason this attempt failed is about the attempt they just made.
      return { polite: state.notice, assertive: state.error }
    case 'signedIn':
      // The session is live; the only news here is a sign-out that failed inside it.
      return { polite: state.error, assertive: null }
    case 'unreachable':
      return { polite: null, assertive: UNREACHABLE_COPY }
    default:
      // Loading and open have nothing to announce.
      return { polite: null, assertive: null }
  }
}

/**
 * Mount the announcer: write the current words into the given regions, then follow the store and
 * write every later change into those same nodes. `injected` are App's own persistent nodes; the
 * announcer keeps writing into them for its whole life, which is the whole reason the regions are
 * hoisted out of the screens - a message that outlives a screen needs a region that outlives it too.
 */
export function createLiveAnnouncer(store: SessionStore, regions: LiveRegions): LiveAnnouncer {
  let timer: ReturnType<typeof setTimeout> | undefined

  function say(): void {
    const next = messageFor(store.get())
    regions.polite.textContent = next.polite ?? ''
    regions.assertive.textContent = next.assertive ?? ''
  }

  const unsubscribe = store.subscribe(() => {
    clearTimeout(timer)
    say()
  })
  say()

  return {
    get polite(): HTMLElement {
      return regions.polite
    },
    get assertive(): HTMLElement {
      return regions.assertive
    },
    stop: () => {
      clearTimeout(timer)
      unsubscribe()
    },
    reannounce: () => {
      // Blank now, restore on the next tick: screen readers compare against what the region held
      // a moment ago, and text that never left is not news.
      regions.assertive.textContent = ''
      clearTimeout(timer)
      timer = setTimeout(say, 60)
    },
  }
}
