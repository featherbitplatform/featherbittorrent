/**
 * The cue windows and the de-duplication, on windows that overlap the way fetched ones do.
 *
 * @module player/cues.test
 */
import { expect, it } from 'vitest'
import { cueKey, mergeCues, nextWindow } from './cues'
import type { Cue } from '../api/client'

it('asks_for_a_window_from_the_first_unfetched_point_inside_its_horizon', () => {
  expect(nextWindow(1000, []), 'the first window did not start at the playhead').toEqual([1000, 61000])
  expect(
    nextWindow(2000, [[1000, 61000]]),
    'a fully-fetched horizon did not give a window',
  ).toBeNull()
  expect(
    nextWindow(50000, [[0, 61000]]),
    'the window did not start where the fetched ones ended',
  ).toEqual([61000, 121000])
})

it('refuses_to_fetch_beyond_its_horizon', () => {
  expect(
    nextWindow(1000, [[0, 61000], [61000, 121000], [121000, 181000]]),
    'a horizon with no unfetched point inside it fetched further ahead',
  ).toBeNull()
  expect(
    nextWindow(1000, [[0, 21000]]),
    'an unfetched point at the horizon edge was not reached for',
  ).toEqual([21000, 81000])
})

it('handles_overlapping_windows_without_asking_twice', () => {
  // The second window starts where the first ends: the seam is fetched once, not twice.
  const first = nextWindow(1000, [])
  if (first === null) throw new Error('the first window was not fetched')
  expect(first, 'the first window was not a 60 000 ms window from the playhead').toEqual([1000, 61000])
  const fetched: [number, number][] = [first]
  expect(
    nextWindow(61000, fetched),
    'the seam of the next window asked again for what the first window covers',
  ).toEqual([61000, 121000])
  fetched.push([61000, 121000])
  expect(
    nextWindow(60000, fetched),
    'a fully-fetched horizon fetched the seam again',
  ).toBeNull()
  // The next window starts at the last end, even when the playhead sits inside it: the overlap is
  // already fetched, so it is not asked for again.
  expect(nextWindow(70000, [[1000, 61000], [50000, 121000]]), 'a playhead inside a fetched window asked for it again').toBeNull()
  expect(nextWindow(130000, [[1000, 61000], [50000, 121000]]), 'the next window started inside a fetched window').toEqual([
    130000,
    190000,
  ])
})

const CUE_A: Cue = { start_ms: 1000, end_ms: 2000, text: 'hello' }
const CUE_B: Cue = { start_ms: 2000, end_ms: 3000, text: 'world' }
const CUE_C: Cue = { start_ms: 1000, end_ms: 2000, text: 'goodbye' }

it('keeps_only_the_cues_it_has_not_seen', () => {
  const seen = new Set([cueKey(CUE_A)])
  const fresh = mergeCues(seen, [CUE_A, CUE_B, CUE_C])
  expect(fresh, 'an already-seen cue was returned again').toEqual([CUE_B, CUE_C])
  expect([...seen], 'the seen set was not extended with the new cues').toEqual([
    cueKey(CUE_A),
    cueKey(CUE_B),
    cueKey(CUE_C),
  ])
})

it('sees_a_cue_by_its_start_end_and_text', () => {
  expect(cueKey(CUE_A), 'the key was not the cue spelled out').toBe('1000|2000|hello')
  expect(cueKey(CUE_A) === cueKey(CUE_C), 'a cue with different text was keyed the same').toBe(false)
  expect(cueKey(CUE_A) === cueKey(CUE_B), 'a cue with different times was keyed the same').toBe(false)

  // The same cue arriving in two fetched windows is one cue.
  const seen = new Set<string>()
  const first = mergeCues(seen, [CUE_A])
  const second = mergeCues(seen, [CUE_A])
  expect(first).toEqual([CUE_A])
  expect(second, 'a refetched cue was not returned a second time').toEqual([])
})
