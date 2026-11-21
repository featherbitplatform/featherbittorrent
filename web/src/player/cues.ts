/**
 * Subtitles arrive in windows the page asks for as playback moves, and they may overlap: a window
 * fetched for one second of the file and the one fetched for the next share their seam, and a seek
 * back into already-fetched time re-fetches it. The two rules that keep the WebVTT engine fed
 * without doubling are choosing the next window to ask for, and keeping only the cues it has not
 * seen before.
 *
 * The window itself is the server's shape (`GET .../cues?from_ms=..&to_ms=..` answers
 * `{"cues":[{"start_ms","end_ms","text"}]}`), and the seam rule is the page's own: read ahead a
 * little more than the window, so the window that ends now is not the one that starts now, and do
 * not fetch further than that, because the file may not have those pieces yet.
 *
 * @module player/cues
 */
import type { Cue } from '../api/client'

/** The size of a cue window, in source milliseconds: the server's `CUE_WINDOW_MAX_MS` at its minimum the page needs. */
const WINDOW_MS = 60_000
/** How far ahead of the playhead the next window may start before the page waits for the current one to fill. */
const AHEAD_MS = 20_000

/**
 * The next window to fetch, or null when there is no point to fetch from yet: the first source
 * millisecond inside `AHEAD_MS` of `currentMs` that no window in `fetched` covers. The window it
 * returns covers that point for `WINDOW_MS` and nothing before it is asked for twice.
 */
export function nextWindow(currentMs: number, fetched: [number, number][]): [number, number] | null {
  const horizon = currentMs + AHEAD_MS
  const start =
    fetched
      .filter(([, end]) => end > currentMs)
      .map(([, end]) => end)
      .reduce((latest, end) => Math.max(latest, end), 0)
  const from = Math.max(currentMs, start)
  if (from > horizon) return null
  return [from, from + WINDOW_MS]
}

/** The key of a cue: what makes two fetched cues the same cue. */
export function cueKey(cue: Cue): string {
  return `${cue.start_ms}|${cue.end_ms}|${cue.text}`
}

/** The cues that are new: those whose key is not in `existing`. The input set is not touched. */
export function mergeCues(existing: Set<string>, cues: Cue[]): Cue[] {
  const fresh: Cue[] = []
  for (const cue of cues) {
    const key = cueKey(cue)
    if (!existing.has(key)) {
      existing.add(key)
      fresh.push(cue)
    }
  }
  return fresh
}
