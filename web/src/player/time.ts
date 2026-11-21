/**
 * The two clocks the player speaks.
 *
 * Everything on the wire, and everything remembered or asked for, is a count of *source*
 * milliseconds: the time the file itself claims. The browser, though, plays *media* seconds, and a
 * remuxed track is shifted forward by a fixed `time_offset_ms` so its synthesized decode times are
 * never negative (plan ruling 5: `TIME_OFFSET_MS = 1000`). The offset the probe answers is the
 * whole of the translation; the player holds it and maps at its two edges, the fetch and the
 * element, rather than trusting a component to remember which clock it is reading.
 *
 * @module player/time
 */

/** Media seconds for a point of source time: the point minus the shift, in seconds. */
export function toMedia(sourceMs: number, offsetMs: number): number {
  return (sourceMs - offsetMs) / 1000
}

/** Source milliseconds for a point of media time: the point plus the shift. */
export function toSource(mediaSeconds: number, offsetMs: number): number {
  return Math.round(mediaSeconds * 1000 + offsetMs)
}
