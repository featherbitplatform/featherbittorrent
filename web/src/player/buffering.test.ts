/**
 * The read gate's hysteresis, and the eviction of what lies far behind the playhead.
 *
 * @module player/buffering.test
 */
import { expect, it } from 'vitest'
import { createReadGate, evictRanges } from './buffering'

it('reads_below_the_floor_stops_above_the_ceil_and_holds_between', () => {
  const shouldRead = createReadGate()
  const answers = [89, 100, 121, 100, 89].map(shouldRead)
  expect(answers, 'the hysteresis at 89, 100, 121, 100 and 89 did not hold its last answer between the two lines').toEqual([
    true,
    true,
    false,
    false,
    true,
  ])
})

it('two_gates_keep_their_own_answers', () => {
  const video = createReadGate()
  const audio = createReadGate()
  video(121)
  expect(video(100), 'the video gate did not stop at the ceiling').toBe(false)
  expect(audio(100), 'the audio gate did not keep reading in the band').toBe(true)
})

it('evicts_only_what_lies_far_behind_the_playhead', () => {
  expect(evictRanges([[0, 200]], 100), 'a range across the keep line was not cut to it').toEqual([[0, 40]])
  expect(
    evictRanges([[0, 30], [50, 60], [80, 200]], 100),
    'a range wholly behind the keep line was not evicted, or one across it was not cut to it',
  ).toEqual([[0, 30]])
  expect(evictRanges([[100, 200]], 100), 'a range that starts at the keep line was evicted').toEqual([])
  expect(evictRanges([], 100), 'nothing buffered evicts nothing').toEqual([])
})
