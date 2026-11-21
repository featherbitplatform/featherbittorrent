/**
 * Numbers and remote text in the form a person reads them, tested against the TUI's own table.
 *
 * The reference for every case here is the terminal: `crates/fbtt-tui/tests/format.rs` and the
 * `display_safe` table in fbt-lib (`crates/fbt-core/src/text.rs`). The same byte count must read
 * the same in both screens, so the cases are ported rather than invented, including the awkward
 * ones (1023 B, the value one byte short of the next unit, the EiB ceiling) that a formatter gets
 * wrong by being clever about floats. The cap is this front end's own 255 characters.
 *
 * @module format.test
 */
import { expect, it } from 'vitest'
import { displaySafe, formatBytes, formatPercent, formatRate } from './format'

it('format_units', () => {
  // The two figures the brief names, plus the unit the brief's own example was written in decimal:
  // the TUI counts binary, and the TUI is the reference.
  expect(formatBytes(5_800_000_000), '5.8e9 bytes should read as 5.4 GiB').toBe('5.4 GiB')
  expect(formatRate(850_000), '850000 B/s should read as 830.1 KiB/s').toBe('830.1 KiB/s')
  expect(formatBytes(512), 'whole bytes below one KiB stay whole bytes').toBe('512 B')
  // A value that would be 1024.0 of its unit is one of the next unit instead.
  expect(formatBytes(1_073_741_823), 'one byte short of a GiB reads as 1.0 GiB, not 1024.0 MiB').toBe(
    '1.0 GiB',
  )

  // The TUI's own rate table, ported case for case from `crates/fbtt-tui/tests/format.rs`.
  const rates: [number, string][] = [
    [0, '0 B/s'],
    [512, '512 B/s'],
    [1_023, '1023 B/s'],
    [1_024, '1.0 KiB/s'],
    [1_536, '1.5 KiB/s'],
    [317_440, '310.0 KiB/s'],
    [1_153_434, '1.1 MiB/s'],
    [4_404_019, '4.2 MiB/s'],
    [1_073_741_823, '1.0 GiB/s'],
    [1_099_511_627_776, '1.0 TiB/s'],
    [2 ** 64, '16.0 EiB/s'],
  ]
  for (const [bytesPerSec, want] of rates) {
    expect(formatRate(bytesPerSec), `${bytesPerSec} B/s should read as ${want}`).toBe(want)
  }

  // The TUI's own size table, likewise.
  const sizes: [number, string][] = [
    [0, '0 B'],
    [512, '512 B'],
    [1_024, '1.0 KiB'],
    [88_741, '86.7 KiB'],
    [3_298_534_880, '3.1 GiB'],
    [2 ** 40, '1.0 TiB'],
  ]
  for (const [bytes, want] of sizes) {
    expect(formatBytes(bytes), `${bytes} bytes should read as ${want}`).toBe(want)
  }

  // Input a confused engine can hand over: fractional bytes do not exist and are floored, and a
  // negative, NaN, or infinite count reads as nothing rather than as odd text.
  const guarded: [number, string][] = [
    [1.4, '1 B'],
    [1_023.9, '1023 B'],
    [-255, '0 B'],
    [Number.NaN, '0 B'],
    [Number.POSITIVE_INFINITY, '0 B'],
    [Number.NEGATIVE_INFINITY, '0 B'],
  ]
  for (const [bytes, want] of guarded) {
    expect(formatBytes(bytes), `${bytes} bytes should read as ${want}`).toBe(want)
  }
  expect(formatRate(Number.NaN), 'a NaN rate should read as 0 B/s').toBe('0 B/s')

  // The percent: whole, clamped, and NaN-proof, as the TUI states it.
  const percents: [number, string][] = [
    [0.62, '62%'],
    [1.0, '100%'],
    [0.0, '0%'],
    [-0.5, '0%'],
    [2.0, '100%'],
    [Number.NaN, '0%'],
  ]
  for (const [progress, want] of percents) {
    expect(formatPercent(progress), `progress ${progress} should read as ${want}`).toBe(want)
  }
})

it('display_safe_cuts_hostile_text', () => {
  // A bidi override, a control character and a run of spaces are what remote text uses to spoof
  // a display; none of them survive. Ported from the TUI's `safe_cuts_hostile_text_to_what_fits`
  // and the `display_safe_table` it leans on.
  expect(displaySafe('evil\u202egpj.exe'), 'a bidi override reverses nothing').toBe('evilgpj.exe')
  expect(displaySafe('a\u001b[31mb\tc'), 'an escape keeps the characters it is spelled with').toBe(
    'a[31mb c',
  )
  expect(displaySafe('  padded   name  '), 'whitespace collapses and the ends are trimmed').toBe(
    'padded name',
  )
  expect(displaySafe('a\u200b\ufeffb'), 'zero-width characters hide nothing').toBe('ab')
  expect(displaySafe('a\nb\u0085c'), 'control characters that are whitespace separate words').toBe(
    'a b c',
  )
  expect(displaySafe('\u0301x'), 'a mark with nothing to sit on goes').toBe('x')
  expect(displaySafe('e\u0301x'), 'a mark with a base stays').toBe('e\u0301x')
  expect(displaySafe('qB\u202e4.6\u200b\nx'), 'a spoofed client name is flattened').toBe('qB4.6 x')
  expect(displaySafe(''), 'nothing usable left is the empty string').toBe('')

  // The cap is 255 characters, on a character boundary, and it never glues words: a cut stops
  // before a separator with no character to follow it.
  expect(displaySafe('é'.repeat(10_000)), 'the cap is 255 characters').toHaveLength(255)
  expect(displaySafe('a'.repeat(254) + ' b'), 'a space needs a word after it, so the cut ends at 254').toBe(
    'a'.repeat(254),
  )
  expect(displaySafe('a'.repeat(255) + ' b'), 'the result never runs past the cap').toBe('a'.repeat(255))
})
