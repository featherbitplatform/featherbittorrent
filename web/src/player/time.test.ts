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
  expect(toMedia(4000, OFFSET), 'the offset was not subtracted').toBe(3)
  expect(toMedia(1000, OFFSET), 'the start of the file is at media zero').toBe(0)
  expect(toMedia(1500, OFFSET), 'a half second is read in whole seconds').toBe(0.5)
  expect(toSource(3, OFFSET), 'the offset was not added back').toBe(4000)
  expect(toSource(0, OFFSET), 'media zero is the start of the file, not zero source').toBe(1000)

  // A point walked from source to media and back arrives where it left.
  for (const sourceMs of [1000, 2500, 97_000]) {
    const media = toMedia(sourceMs, OFFSET)
    expect(toSource(media, OFFSET), `rounding moved the point from ${sourceMs}`).toBe(sourceMs)
  }
})
