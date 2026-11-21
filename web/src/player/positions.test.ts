/**
 * The remembered position, against a `localStorage` the test stands in for: the round-trip, the
 * two refusals (a file barely started, a file reached its end), and the storage that throws.
 *
 * The module reads `window.localStorage` on the call, and this suite runs where there is no window,
 * so the test stands a `globalThis` in for it and takes it off again when the test is done. The
 * throwing test does the same with a storage that throws on every access.
 *
 * @module player/positions.test
 */
import { afterEach, beforeEach, expect, it } from 'vitest'
import { loadPosition, positionKey, savePosition } from './positions'

const ID = 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678'
const FILE = 0
const DURATION = 4000

/** A `localStorage` the tests stand in for, and the shape of the one the browser gives. */
function fakeStorage(): Storage {
  const store = new Map<string, string>()
  const length = { value: 0 }
  return {
    getItem: (key: string) => (store.has(key) ? store.get(key)! : null),
    setItem: (key: string, value: string) => {
      store.set(key, value)
      length.value = store.size
    },
    removeItem: (key: string) => {
      store.delete(key)
      length.value = store.size
    },
    clear: () => {
      store.clear()
      length.value = 0
    },
    key: (index: number) => [...store.keys()][index] ?? null,
    get length() {
      return length.value
    },
  }
}

let fake: Storage | null = null
let previousStorage: PropertyDescriptor | undefined
let previousWindow: PropertyDescriptor | undefined

function standStorage(storage: Storage): void {
  previousStorage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage')
  previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  Object.defineProperty(globalThis, 'localStorage', { value: storage, configurable: true })
  Object.defineProperty(globalThis, 'window', { value: globalThis, configurable: true })
  fake = storage
}

function takeStorageOff(): void {
  if (previousStorage === undefined) delete (globalThis as Record<string, unknown>).localStorage
  else Object.defineProperty(globalThis, 'localStorage', previousStorage)
  if (previousWindow === undefined) delete (globalThis as Record<string, unknown>).window
  else Object.defineProperty(globalThis, 'window', previousWindow)
  fake = null
}

beforeEach(() => {
  standStorage(fakeStorage())
})

afterEach(takeStorageOff)

it('round_trips_a_position_under_its_own_key', () => {
  savePosition(ID, FILE, 120, DURATION)
  expect(loadPosition(ID, FILE), 'the saved position did not come back').toBe(120)
  expect(
    fake?.getItem(positionKey(ID, FILE)),
    'the position was not stored under the key the module names',
  ).toBe('120')
  expect(
    loadPosition('ffffffffffffffffffffffffffffffffffffffff', FILE),
    'another torrent read this file position',
  ).toBeNull()
  expect(loadPosition(ID, 1), 'another file read this position').toBeNull()
})

it('saves_nothing_below_the_minimum', () => {
  savePosition(ID, FILE, 5, DURATION)
  expect(loadPosition(ID, FILE), 'a file abandoned in its first seconds was remembered').toBeNull()
  expect(
    fake?.getItem(positionKey(ID, FILE)),
    'the minimum position was written where it should not be',
  ).toBeNull()
})

it('refuses_a_position_that_is_not_a_number', () => {
  savePosition(ID, FILE, Number.NaN, DURATION)
  expect(
    fake?.getItem(positionKey(ID, FILE)),
    'a NaN position was written to storage',
  ).toBeNull()

  fake?.setItem(positionKey(ID, FILE), '')
  expect(loadPosition(ID, FILE), 'an empty stored position was read as the start of the file').toBeNull()
})

it('clears_a_position_when_the_file_is_done', () => {
  savePosition(ID, FILE, 120, DURATION)
  savePosition(ID, FILE, DURATION - 10, DURATION)
  expect(loadPosition(ID, FILE), 'a file played to its last seconds was still remembered').toBeNull()

  // A position saved early survives a save that is still short of the end.
  savePosition(ID, FILE, DURATION - 60, DURATION)
  expect(loadPosition(ID, FILE), 'a position near, not within, the end was cleared').toBe(3940)
})

it('never_propagates_a_storage_that_throws', () => {
  const boom = () => {
    throw new Error('storage is not available')
  }
  const throwing = {
    getItem: boom,
    setItem: boom,
    removeItem: boom,
    clear: boom,
    key: boom,
    get length() {
      return boom()
    },
  } as unknown as Storage
  standStorage(throwing)
  expect(loadPosition(ID, FILE), 'a throwing storage was not answered with null').toBeNull()
  expect(() => savePosition(ID, FILE, 120, DURATION), 'a throwing storage threw out of the save').not.toThrow()
})
