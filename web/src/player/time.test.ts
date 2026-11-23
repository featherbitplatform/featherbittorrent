/**
 * The two clocks, against the shift the probe answers.
 *
 * @module player/time.test
 */
import { expect, it } from 'vitest'
import { toMedia, toSource } from './time'

/** The offset every remuxed track carries. */
const OFFSET = 1000

it('maps_source_to_media_and_back_across_the_offset', () => {
  // A remuxed track is shifted forward by the offset: the source's start is the offset seconds
  // ahead in the element, and the track's own zero is at source `offset`.
  expect(toMedia(0, OFFSET), 'the source start is the offset ahead in the element').toBe(1)
  expect(toMedia(2000, OFFSET), 'the start header maps source time to media time').toBe(3)
  expect(toSource(3, OFFSET), 'the offset is subtracted, not added').toBe(2000)
  expect(toSource(0, OFFSET), 'the element start is before the offset, so it clamps at the start').toBe(0)

  // A point walked from source to media and back arrives where it left, for source times at or
  // past the offset (the element's time there is at or past the offset, so the clamp never fires).
  for (const sourceMs of [1000, 2500, 97_000]) {
    const media = toMedia(sourceMs, OFFSET)
    expect(toSource(media, OFFSET), `rounding moved the point from ${sourceMs}`).toBe(sourceMs)
  }
})
