/**
 * How much of the stream the page keeps in its buffer, and when it stops asking.
 *
 * Reading a body that nobody has consumed yet is a cost, so the page reads while it is short and
 * stops when it is not: `shouldRead` answers for one gate with hysteresis, because a buffer that
 * hovers around one threshold would otherwise toggle on every `timeupdate` and the fetch would
 * stutter. The state lives in the gate's closure, one per stream the engine runs, so the two tracks
 * of one file each keep their own answer.
 *
 * Eviction is the other edge: a `SourceBuffer` with no cap is still limited by the browser's quota,
 * so what lies far behind the playhead goes first. It is only *behind*, and only *far*: evicting
 * something the decoder may still reach would be the bug the eviction exists to prevent.
 *
 * @module player/buffering
 */

/** Below here the gate always reads, whatever it answered before. */
const READ_BELOW = 90
/** At or above here the gate always stops, whatever it answered before. */
const READ_ABOVE = 120
/** How far behind the playhead a buffered second is before it is evicted. */
const EVICT_BEHIND = 60

/** One stream's read gate, with the hysteresis between `READ_BELOW` and `READ_ABOVE` kept in its closure. */
export function createReadGate(): (bufferedAheadSeconds: number) => boolean {
  let reading = true
  return function shouldRead(bufferedAheadSeconds: number): boolean {
    if (bufferedAheadSeconds < READ_BELOW) {
      reading = true
    } else if (bufferedAheadSeconds >= READ_ABOVE) {
      reading = false
    }
    return reading
  }
}

/**
 * What lies more than `EVICT_BEHIND` seconds behind `currentTime`, ready to be removed. A range
 * wholly behind the keep line is evicted whole; one that crosses it is shortened to it; one that
 * starts at or after it is kept.
 */
export function evictRanges(
  buffered: [number, number][],
  currentTime: number,
): [number, number][] {
  const keepFrom = currentTime - EVICT_BEHIND
  const dropped: [number, number][] = []
  for (const [start, end] of buffered) {
    if (start < keepFrom) {
      dropped.push([start, Math.min(end, keepFrom)])
    }
  }
  return dropped
}
