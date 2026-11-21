/**
 * Numbers and remote text in the form a person reads them, matching the terminal exactly.
 *
 * This is `crates/fbtt-tui/src/format.rs` carried to the browser, not a fresh guess at the same
 * idea: rates and sizes are stated the way a person states them out loud (whole bytes below one
 * KiB, one decimal place above it, and the next unit up as soon as the number would read as
 * 1024.0 of this one), and every value is computed in whole tenths so the same byte count always
 * produces the same string. A table that redraws once a second needs that: a percent that jitters
 * between two values looks like a bug even when it is arithmetic.
 *
 * `displaySafe` is the other half, the rule `fbt::display_safe` enforces in the terminal: remote
 * strings cannot reverse the row they sit in or run past it. React escapes text nodes, but nothing
 * but this filter stops a name from wearing an invisible bidi override that makes `.png` display
 * as `.jpg`, or from being a megabyte of text somebody paid for by the byte.
 */

/** The units `formatBytes` and `formatRate` count in: binary, because that is what a person comparing a file size against a disk expects. */
const UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB', 'EiB'] as const
/** How many of the previous unit make the next one. */
const UNIT_STEP = 1024
/** A value this many tenths of a unit (1024.0) belongs in the unit above it. */
const ROUNDED_UP = UNIT_STEP * 10

/**
 * Longest string `displaySafe` keeps, in characters: the cap on remote names and messages that
 * reach a user. The terminal's own constant (`fbt::MAX_DISPLAY_CHARS`) is 256; this page keeps one
 * fewer so a name and the state beside it still share a row on the narrowest phone worth laying out.
 */
const MAX_DISPLAY_CHARS = 255

/** Unicode whitespace, exactly the set Rust's `char::is_whitespace` tests. */
const IS_WHITESPACE = /\p{White_Space}/u
/** Unicode control (Cc) and format (Cf) characters: invisible, and the tools remote text uses to spoof a display. */
const IS_INVISIBLE = /[\p{Cc}\p{Cf}]/u
/** Unicode marks (category M): a combining character with nothing to sit on lands on whatever came before it. */
const IS_MARK = /\p{M}/u

/**
 * A byte count, as `3.1 GiB`. Whole bytes below one KiB, one decimal from there up, computed in
 * whole tenths rounded half up - a tenth is a digit here, not a float - and the largest unit the
 * value reaches whole, so 999 KiB never reads as 0.9 MiB and a value that rounds up to a full
 * 1024.0 of its unit is one of the next unit instead.
 *
 * Sizes beyond 2^53 lose exact tenths in plain JS numbers; the API answers integers that fit in a
 * number in practice, and every string stays a well-formed reading of whatever number came in.
 */
export function formatBytes(bytes: number): string {
  let unit = 0
  let divisor = 1
  while (unit + 1 < UNITS.length && bytes >= divisor * UNIT_STEP) {
    unit += 1
    divisor *= UNIT_STEP
  }

  // The value in tenths of that unit, rounded half up: the divisor is a power of two, so half of
  // it is a whole number and this is the same arithmetic as the Rust original's integer math.
  let tenths = Math.floor((bytes * 10 + Math.floor(divisor / 2)) / divisor)
  if (unit + 1 < UNITS.length && tenths >= ROUNDED_UP) {
    unit += 1
    divisor *= UNIT_STEP
    tenths = Math.floor((bytes * 10 + Math.floor(divisor / 2)) / divisor)
  }

  const whole = Math.floor(tenths / 10)
  const tenth = tenths % 10
  return unit === 0 ? `${whole} ${UNITS[unit]}` : `${whole}.${tenth} ${UNITS[unit]}`
}

/** A transfer rate, as `4.2 MiB/s`. */
export function formatRate(bytesPerSec: number): string {
  return `${formatBytes(bytesPerSec)}/s`
}

/** `x` inside 0..1; NaN and the infinities no progress number should be become 0. */
function clampUnit(x: number): number {
  if (Number.isFinite(x)) return Math.min(1, Math.max(0, x))
  return 0
}

/** The percent to show for a progress number, clamped to 0..100 and rounded to a whole percent. The bar takes its width from the same number the label shows, so the two can never disagree. */
export function filledPercent(progress: number): number {
  const scaled = Math.round(clampUnit(progress) * 100)
  if (scaled <= 0) return 0
  if (scaled >= 100) return 100
  return scaled
}

/** A fraction as a whole percent: `62%`. Values outside 0..1 and NaN are clamped, because a progress number comes from the engine and is not worth a broken row. */
export function formatPercent(progress: number): string {
  return `${filledPercent(progress)}%`
}

/**
 * `s` as a person should see it: Unicode control and format characters dropped (the bidi overrides
 * that spoof a name's ending live there), every run of whitespace replaced by one space, the ends
 * trimmed, a combining mark with nothing to sit on dropped, and the result cut to 255 characters
 * on a character boundary. The one deviation from `fbt::display_safe`'s own cap, and the reason is
 * at `MAX_DISPLAY_CHARS`.
 *
 * The result never starts or ends with a space, and cutting it short never glues two words
 * together: a separator only goes in when at least one character can follow it, so a cut lands
 * inside a word, not on the space between two.
 */
export function displaySafe(s: string): string {
  let out = ''
  // Kept characters, spaces included: the cap counts characters.
  let kept = 0
  // A separator seen since the last kept character. It becomes one space, or nothing at all while
  // nothing is kept yet, which is what trims the front.
  let space = false
  // Iterating by code point so the cut never splits a surrogate pair, which is what Rust's
  // `chars()` iteration cannot do by construction.
  for (const c of s) {
    if (IS_WHITESPACE.test(c)) {
      space = true
      continue
    }
    // Cc (the whitespace ones went above) and Cf are invisible: gone. A mark has no base when
    // nothing was kept or the only thing since then is the separator pending here, so it goes too.
    if (IS_INVISIBLE.test(c) || ((kept === 0 || space) && IS_MARK.test(c))) continue
    if (space && kept > 0) {
      // Room for this space and one more character: without that the result would end on a space,
      // or the next word would be glued to this one, so the string ends here instead.
      if (kept + 2 > MAX_DISPLAY_CHARS) break
      out += ' '
      kept += 1
    }
    space = false
    if (kept === MAX_DISPLAY_CHARS) break
    out += c
    kept += 1
  }
  return out
}
