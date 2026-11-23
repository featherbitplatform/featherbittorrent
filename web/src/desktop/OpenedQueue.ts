/**
 * The links and files the app was opened with, waiting for the Add page one at a time.
 *
 * A FIFO: the page shows the oldest item on the Add page, and the item leaves only when it was added or
 * somebody pressed Cancel on it ({@link OpenedQueue.drop} with that item's id). A failed sign-in, a
 * refused add, an instance switched in the picker: none of them take an item out, so a link is never
 * dropped without anybody having seen it. The queue lives outside the screens keyed by instance, so a
 * switch in the title bar does not lose it either.
 *
 * {@link startOpenedFeed} fills it from the shell: it listens for new items first and asks for the ones
 * that came before second, so nothing falls between the two.
 *
 * @module desktop/OpenedQueue
 */
import { useSyncExternalStore } from 'react'
import type { OpenedItem } from './bridge'

/** One item in the queue, with the id that names it for {@link OpenedQueue.drop}. */
export interface QueuedItem {
  id: number
  item: OpenedItem
}

/** The most items kept: the shell's own bound on what it keeps for the page. More is a flood, not a person. */
export const MAX_QUEUED = 256

export class OpenedQueue {
  private items: readonly QueuedItem[] = []
  private nextId = 1
  private readonly listeners = new Set<() => void>()

  /** Adds an item at the back. Past {@link MAX_QUEUED}, the newest is left out. */
  push(item: OpenedItem): void {
    if (this.items.length >= MAX_QUEUED) return
    this.items = [...this.items, { id: this.nextId, item }]
    this.nextId += 1
    this.changed()
  }

  /** The item the Add page shows, or null when nothing is waiting. */
  current(): QueuedItem | null {
    return this.items[0] ?? null
  }

  /** How many items are waiting, the current one included. */
  size(): number {
    return this.items.length
  }

  /** Takes out the item called `id`, and only that one: a second Cancel for an item already gone takes out nothing. */
  drop(id: number): void {
    const next = this.items.filter((queued) => queued.id !== id)
    if (next.length === this.items.length) return
    this.items = next
    this.changed()
  }

  /** Follows every change; returns the way to stop. */
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  private changed(): void {
    for (const listener of this.listeners) listener()
  }
}

/** The item at the front of `queue`, re-rendering when it changes. */
export function useCurrentOpened(queue: OpenedQueue): QueuedItem | null {
  return useSyncExternalStore(queue.subscribe, () => queue.current())
}

/** What {@link startOpenedFeed} needs from the shell: the bridge's two calls. */
export interface OpenedSource {
  onOpened(handler: (item: OpenedItem) => void): Promise<() => void>
  takeOpened(): Promise<OpenedItem[]>
}

/** Fills `queue` from the shell: listening first, then the items that came before. Returns the way to stop listening. */
export function startOpenedFeed(queue: OpenedQueue, source: OpenedSource): () => void {
  let stopped = false
  let unlisten: (() => void) | null = null
  void (async () => {
    try {
      const stop = await source.onOpened((item) => {
        if (!stopped) queue.push(item)
      })
      if (stopped) {
        stop()
        return
      }
      unlisten = stop
      const earlier = await source.takeOpened()
      if (stopped) return
      for (const item of earlier) queue.push(item)
    } catch (cause) {
      console.warn('The links and files the app was opened with could not be read:', cause)
    }
  })()
  return () => {
    stopped = true
    unlisten?.()
  }
}
