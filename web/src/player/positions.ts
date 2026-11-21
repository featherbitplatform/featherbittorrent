/**
 * Where the person left a file, remembered between visits.
 *
 * The page stores only view preferences, and this is the only one: the position, under
 * `fbtt.position.<infohash>.<file>`. A position is a courtesy, not a promise: a `localStorage`
 * behind a private window throws on access, a browser with storage turned off answers nothing, and
 * the page may be running where there is no storage at all, so every read and write is inside a
 * `try`/`catch` and a failure is simply a file opened at the start.
 *
 * @module player/positions
 */

/** How far in a file must be before a position is worth remembering. */
const MINIMUM_SECONDS = 10
/** How close to the end a file is done, so its position is cleared. */
const FINISHED_WITHIN = 30

/**
 * The storage the position lives in, read on the call: a page that runs where there is no window or
 * no storage gets a storage that throws, and the `try`/`catch` below is what turns that into a
 * missing position rather than a page that stops loading.
 */
function storage(): Storage {
  return window.localStorage
}

/** The storage key for one file of one torrent. */
export function positionKey(id: string, file: number): string {
  return `fbtt.position.${id}.${file}`
}

/** The remembered position for a file, in source seconds, or null when there is none. */
export function loadPosition(id: string, file: number): number | null {
  try {
    const stored = storage().getItem(positionKey(id, file))
    // An empty string is not a position: `Number('')` is `0`, and a file would be reopened at the start.
    if (stored === null || stored === '') return null
    const seconds = Number(stored)
    return Number.isFinite(seconds) && seconds >= 0 ? seconds : null
  } catch {
    return null
  }
}

/**
 * Remember a position. Saves nothing below `MINIMUM_SECONDS`, and clears what is stored when the
 * file is within `FINISHED_WITHIN` of its end. A failure to store is no more than a courtesy lost.
 */
export function savePosition(id: string, file: number, seconds: number, duration: number): void {
  const key = positionKey(id, file)
  try {
    // A position that is not a number is not written: `String(NaN)` would store a word, not a time.
    if (!Number.isFinite(seconds)) return
    if (duration > 0 && seconds >= duration - FINISHED_WITHIN) {
      storage().removeItem(key)
      return
    }
    if (seconds < MINIMUM_SECONDS) return
    storage().setItem(key, String(seconds))
  } catch {
    // A storage that throws is treated as one that is not there: the player plays on.
  }
}
