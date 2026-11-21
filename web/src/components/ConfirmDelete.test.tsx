// @vitest-environment jsdom
/**
 * The dialog that asks *how* to remove a torrent, against a fake server on a fake `fetch`.
 *
 * Keeping the files and deleting them are different requests, and the only thing that decides which
 * one leaves the page is which button was pressed, so each branch gets its own server and its own
 * count of calls: a dialog that always sent `delete_files=true` would still show a DELETE with the
 * right id and the right method. Escape is the third branch, the one that sends nothing at all. The
 * client is the page's own over a recording `fetch` because `delete_files` is a query parameter the
 * server reads, not an argument the component hands to a friendly fake.
 *
 * @module components/ConfirmDelete.test
 */
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import { useLayoutEffect, useRef, useState } from 'react'
import ConfirmDelete from './ConfirmDelete'
import { createClient } from '../api/client'
import type { Api } from '../api/client'
import { installDialogShim } from '../dialog-shim'

afterEach(cleanup)
installDialogShim()

const BASE = 'http://localhost'
const ID = 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678'
const NAME = 'Sintel (2010) 4K'

interface Seen {
  url: string
  method: string
}

function json(status: number, body: string): Response {
  return new Response(body, { status, headers: { 'content-type': 'application/json' } })
}

/** A server that answers the one DELETE this dialog makes with whatever the test asks for. */
function server(answer: () => Response = () => new Response(null, { status: 204 })): {
  client: Api
  seen: Seen[]
} {
  const seen: Seen[] = []
  const fetchFn = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input)
    const method = init?.method ?? 'GET'
    seen.push({ url, method })
    if (url.startsWith(`/api/v1/torrents/${encodeURIComponent(ID)}`) && method === 'DELETE') {
      return answer()
    }
    return json(404, '{"error":{"code":"not_found","message":"nothing at that path"}}')
  }
  return {
    client: createClient(fetchFn, () => 'csrf-value', () => 0, () => {}),
    seen,
  }
}

/** The dialog in the shape the screen puts it in: mounted only while open, with the polite region already on the page and the Remove button that opened it holding the focus, the way a click leaves it. */
function Harness({ client, events }: { client: Api; events: string[] }) {
  const [open, setOpen] = useState(true)
  const openerRef = useRef<HTMLButtonElement>(null)
  useLayoutEffect(() => {
    openerRef.current?.focus()
  }, [])
  return (
    <>
      <div role="status" aria-live="polite" />
      <button ref={openerRef} type="button">
        Remove
      </button>
      {open && (
        <ConfirmDelete
          id={ID}
          name={NAME}
          client={client}
          onClose={() => {
            events.push('closed')
            setOpen(false)
          }}
          onRemoved={(goneId) => {
            events.push(`removed:${goneId}`)
          }}
        />
      )}
    </>
  )
}

function openConfirm(client: Api): {
  container: HTMLElement
  polite: HTMLElement
  opener: HTMLElement
  events: string[]
} {
  const events: string[] = []
  const { container } = render(<Harness client={client} events={events} />)
  return {
    container,
    polite: container.querySelector('[aria-live="polite"]') as HTMLElement,
    opener: container.querySelector('button') as HTMLElement,
    events,
  }
}

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0))
    await new Promise((resolve) => setTimeout(resolve, 0))
  })
}

const removeRequests = (seen: Seen[]): Seen[] => seen.filter((call) => call.method === 'DELETE')

/** The `delete_files` the one removal that left sent, or null when nothing left. */
function deleteFilesSent(seen: Seen[]): string | null {
  const call = removeRequests(seen)[0]
  if (call === undefined) return null
  return new URL(call.url, BASE).searchParams.get('delete_files')
}

describe('what the remove dialog sends', () => {
  it('delete_asks_whether_to_keep_the_files', async () => {
    // "Remove and keep files" asks for the files to stay.
    const keep = server()
    const keepDialog = openConfirm(keep.client)
    await flush()
    fireEvent.click(screen.getByRole('button', { name: 'Remove and keep files' }))
    await flush()
    expect(
      deleteFilesSent(keep.seen) === 'false' && removeRequests(keep.seen).length === 1,
      'the keep files button did not send one removal with delete_files=false',
    ).toBe(true)
    expect(
      screen.queryByRole('dialog') === null &&
        keepDialog.polite.textContent === 'Torrent removed.' &&
        keepDialog.events.join(',') === `removed:${ID},closed`,
      'the keep files removal did not close the dialog and announce through the polite region',
    ).toBe(true)
    cleanup()

    // "Remove and delete files" asks for them to go, and that is the only difference.
    const gone = server()
    openConfirm(gone.client)
    await flush()
    fireEvent.click(screen.getByRole('button', { name: 'Remove and delete files' }))
    await flush()
    expect(
      deleteFilesSent(gone.seen) === 'true' && removeRequests(gone.seen).length === 1,
      'the delete files button did not send one removal with delete_files=true',
    ).toBe(true)
    cleanup()

    // Escape answers nothing: the torrent stays, and no request leaves.
    const escaped = server()
    const escapedDialog = openConfirm(escaped.client)
    await flush()
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' })
    await flush()
    expect(
      removeRequests(escaped.seen).length === 0 &&
        screen.queryByRole('dialog') === null &&
        escapedDialog.events.join(',') === 'closed' &&
        escapedDialog.polite.textContent === '',
      'Escape removed the torrent or asked the server about it',
    ).toBe(true)
  })

  it('a_refused_removal_says_why_and_keeps_the_dialog_open', async () => {
    const { client, seen } = server(() =>
      json(404, '{"error":{"code":"torrent_not_found","message":"no torrent with that id"}}'),
    )
    const { polite, events } = openConfirm(client)
    await flush()
    fireEvent.click(screen.getByRole('button', { name: 'Remove and delete files' }))
    await flush()

    const open = screen.queryByRole('dialog')
    expect(
      open !== null &&
        (open.textContent ?? '').includes('no torrent with that id') &&
        polite.textContent === '' &&
        events.length === 0 &&
        removeRequests(seen).length === 1,
      'a refused removal closed the dialog, announced a removal, or tried again',
    ).toBe(true)
  })
})

describe('the remove dialog in words', () => {
  it('the_remove_dialog_names_the_torrent_and_the_choice', async () => {
    const { client } = server()
    const { container } = openConfirm(client)
    await flush()
    const dialog = screen.getByRole('dialog')
    expect(
      dialog.getAttribute('aria-labelledby') !== null &&
        (container.textContent ?? '').includes('Remove torrent') &&
        (container.textContent ?? '').includes(NAME),
      'the remove dialog did not say what it removes or name the torrent it is about',
    ).toBe(true)
    for (const label of ['Cancel', 'Remove and keep files', 'Remove and delete files', 'Close']) {
      expect(
        screen.getByRole('button', { name: label }) !== null,
        `the remove dialog has no ${label} button`,
      ).toBe(true)
    }
  })
})

describe('the keyboard', () => {
  it('the_remove_dialog_returns_the_focus_it_took', async () => {
    const { client, seen } = server()
    const { container, opener } = openConfirm(client)
    await flush()

    const dialog = screen.getByRole('dialog')
    expect(
      document.activeElement !== null && dialog.contains(document.activeElement),
      'opening the remove dialog left the focus outside it',
    ).toBe(true)

    const close = screen.getByRole('button', { name: 'Close' })
    const del = screen.getByRole('button', { name: 'Remove and delete files' })
    del.focus()
    const taken = fireEvent.keyDown(dialog, { key: 'Tab' })
    expect(
      taken === false && document.activeElement === close,
      'Tab at the last control left the remove dialog instead of wrapping to the first',
    ).toBe(true)
    close.focus()
    fireEvent.keyDown(dialog, { key: 'Tab', shiftKey: true })
    expect(document.activeElement, 'Shift Tab at the first control did not wrap to the last control').toBe(del)

    // Escape, and the focus goes back to the Remove button that opened the dialog, with nothing sent.
    fireEvent.keyDown(dialog, { key: 'Escape' })
    await flush()
    expect(
      screen.queryByRole('dialog') === null &&
        removeRequests(seen).length === 0 &&
        document.activeElement === opener,
      'Escape left the remove dialog holding the focus or asking the server',
    ).toBe(true)
    expect(container.querySelector('dialog'), 'the remove dialog element stayed in the page').toBeNull()
  })
})
