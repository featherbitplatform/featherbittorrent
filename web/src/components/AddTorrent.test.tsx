// @vitest-environment jsdom
/**
 * The Add torrent dialog, against a fake server reached through a fake `fetch`.
 *
 * Every rule here is about what leaves the page or what stays when nothing leaves. The folder sent is
 * a folder the server named, never one somebody typed, and the dialog starts on the first root rather
 * than on the filesystem's roof. A `.torrent` leaves as the bytes it is, under the torrent content
 * type, with its two options in the query. A refusal is the server's own words inside a dialog that
 * kept everything that was typed into it. And a size or a shape this page can rule out by itself is
 * refused here, without a request at all.
 *
 * The client is the page's own `createClient` over a recording `fetch` rather than a hand-written
 * fake of the `Api` methods, because "as `application/x-bittorrent` with its exact bytes" is a claim
 * about the request that left the browser, not about which method the component happened to call.
 *
 * @module components/AddTorrent.test
 */
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import { useLayoutEffect, useRef, useState } from 'react'
import AddTorrent from './AddTorrent'
import { createClient } from '../api/client'
import type { Api } from '../api/client'
import { installDialogShim } from '../dialog-shim'

afterEach(cleanup)
installDialogShim()

const BASE = 'http://localhost'
const MAGNET = 'magnet:?xt=urn:btih:5a8062c076fa85e8056451c0d9aa04349ae27909&dn=Sintel'

interface Seen {
  url: string
  method: string
  headers: Headers
  body: BodyInit | null
}

function json(status: number, body: string): Response {
  return new Response(body, { status, headers: { 'content-type': 'application/json' } })
}

/** The server the dialog talks to: the two fs routes, and the add route, answering whatever the test asks it to. `refuse` names paths that answer `path_not_allowed` even though they are inside a root, the way a server would for a folder that went off limits; `rootsAnswer` replaces the roots answer with a failure of the test's choosing. */
function server(
  options: {
    roots?: string[]
    dirs?: Record<string, string[]>
    refuse?: string[]
    add?: () => Response
    rootsAnswer?: () => Response
  } = {},
): { client: Api; seen: Seen[] } {
  const roots = options.roots ?? ['/downloads', '/usb/media']
  const dirs = options.dirs ?? {}
  const refuse = options.refuse ?? []
  const seen: Seen[] = []
  const fetchFn = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input)
    const call: Seen = {
      url,
      method: init?.method ?? 'GET',
      headers: new Headers(init?.headers),
      body: init?.body ?? null,
    }
    seen.push(call)
    if (url === '/api/v1/fs/roots') {
      return options.rootsAnswer?.() ?? json(200, JSON.stringify(roots))
    }
    if (url.startsWith('/api/v1/fs/list')) {
      const path = new URL(url, BASE).searchParams.get('path') ?? ''
      const inside = (root: string): boolean => path === root || path.startsWith(`${root}/`)
      if (refuse.includes(path) || !roots.some(inside)) {
        return json(
          403,
          '{"error":{"code":"path_not_allowed","message":"that path is not one of the folders this server allows"}}',
        )
      }
      return json(200, JSON.stringify({ path, dirs: dirs[path] ?? [] }))
    }
    if (url.startsWith('/api/v1/torrents') && call.method === 'POST') {
      return options.add?.() ?? json(201, '{"id":"a1b2c3d4","name":"Sintel"}')
    }
    return json(404, '{"error":{"code":"not_found","message":"nothing at that path"}}')
  }
  return {
    client: createClient(fetchFn, () => 'csrf-value', () => 0, () => {}),
    seen,
  }
}

/** The dialog inside the shape the screen puts it in: mounted only while it is open, with the page's polite region already on the page and the button that opened it before it. The layout effect hands that button the focus before the dialog's own effect runs, which is what a click does in a browser and what the dialog has to see in order to hand the focus back. */
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
        Add torrent
      </button>
      {open && (
        <AddTorrent
          client={client}
          onClose={() => {
            events.push('closed')
            setOpen(false)
          }}
          onAdded={() => {
            events.push('added')
          }}
        />
      )}
    </>
  )
}

function openDialog(client: Api): { container: HTMLElement; polite: HTMLElement; events: string[] } {
  const events: string[] = []
  const { container } = render(<Harness client={client} events={events} />)
  return {
    container,
    polite: container.querySelector('[aria-live="polite"]') as HTMLElement,
    events,
  }
}

/** Let everything the dialog just fired land before looking at the DOM. */
async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0))
    await new Promise((resolve) => setTimeout(resolve, 0))
  })
}

const listPaths = (seen: Seen[]): string[] =>
  seen
    .filter((call) => call.url.startsWith('/api/v1/fs/list'))
    .map((call) => new URL(call.url, BASE).searchParams.get('path') ?? '')

const rowPaths = (container: HTMLElement): string[] =>
  Array.from(container.querySelectorAll('[data-folder]')).map((row) => row.getAttribute('data-folder') ?? '')

const chosenPath = (container: HTMLElement): string | null =>
  container.querySelector('[data-folder][aria-pressed="true"]')?.getAttribute('data-folder') ?? null

function pressFolder(container: HTMLElement, path: string): void {
  fireEvent.click(container.querySelector(`[data-folder="${path}"]`) as Element)
}

/** The body of the last JSON add, parsed, or null when no add left the page. */
function addedJson(seen: Seen[]): { magnet?: string; save_path?: string; paused?: boolean } | null {
  const post = seen.filter((call) => call.method === 'POST').at(-1)
  if (post === undefined || typeof post.body !== 'string') return null
  return JSON.parse(post.body) as { magnet?: string; save_path?: string; paused?: boolean }
}

const addRequest = (seen: Seen[]): Seen | null => seen.find((call) => call.method === 'POST') ?? null
const postCount = (seen: Seen[]): number => seen.filter((call) => call.method === 'POST').length
const magnetField = (): HTMLInputElement => screen.getByLabelText('Magnet link') as HTMLInputElement

/** The area a `.torrent` is dropped onto. A dialog with no drop area is the failure this says out loud. */
function dropArea(container: HTMLElement): Element {
  const area = container.querySelector('[data-drop-zone]')
  expect(area !== null, 'the file source has no drop area to drop a torrent onto').toBe(true)
  return area as Element
}

describe('the folder the torrent goes into', () => {
  it('add_uses_the_default_folder_and_only_allowed_ones', async () => {
    const { client, seen } = server({
      roots: ['/downloads', '/usb/media'],
      dirs: {
        '/downloads': ['/downloads/films', '/downloads/linux'],
        '/downloads/films': ['/downloads/films/subtitles'],
        '/downloads/linux': [],
        '/usb/media': [],
      },
    })
    const { container } = openDialog(client)
    await flush()

    // It opens on the first root `roots()` named, not on `/` and not on anything else, and the only
    // folder it asked the server about is that one root.
    expect(
      chosenPath(container) === '/downloads',
      'the folder picker did not start at the first root roots() answered',
    ).toBe(true)
    expect(
      listPaths(seen).join(','),
      'the folder picker asked the server about a folder outside the roots it was given',
    ).toBe('/downloads')
    expect(
      rowPaths(container).sort().join(','),
      'the folder picker listed a folder list() never answered',
    ).toBe('/downloads,/downloads/films,/downloads/linux,/usb/media')

    // Opening another root asks only that root, and lists only what came back for it.
    pressFolder(container, '/usb/media')
    await flush()
    expect(
      listPaths(seen).join(','),
      'opening another root asked the server about a folder nobody named',
    ).toBe('/downloads,/usb/media')

    // A folder inside the default root: the picker lists it, and the add sends exactly that path.
    pressFolder(container, '/downloads/films')
    await flush()
    expect(
      rowPaths(container).includes('/downloads/films/subtitles') &&
        !rowPaths(container).includes('/etc'),
      'the folder picker did not list what list() answered for the folder that was opened',
    ).toBe(true)

    fireEvent.change(magnetField(), { target: { value: MAGNET } })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    await flush()

    const body = addedJson(seen)
    expect(
      body?.save_path === '/downloads/films' &&
        body?.magnet === MAGNET &&
        body?.paused === false,
      'the add did not save to the folder that was picked',
    ).toBe(true)
  })

  it('the_add_waits_for_the_server_to_name_a_folder', async () => {
    const { client, seen } = server({
      roots: [],
      rootsAnswer: () =>
        json(500, '{"error":{"code":"internal","message":"the library is not ready"}}'),
    })
    const { container } = openDialog(client)
    await flush()

    // Nothing came back for the roots, so there is no folder to save into: the dialog says what the
    // server said instead of inventing a path of its own, and it will not add anything.
    expect(
      chosenPath(container) === null &&
        (container.textContent ?? '').includes('the library is not ready') &&
        (screen.getByRole('button', { name: 'Add' }) as HTMLButtonElement).disabled &&
        postCount(seen) === 0,
      'the add went ahead with no folder from the server',
    ).toBe(true)
  })

  it('a_refused_folder_shows_the_servers_reason', async () => {
    const { client } = server({ roots: ['/downloads'], refuse: ['/downloads'] })
    const { container } = openDialog(client)
    await flush()
    expect(
      (container.textContent ?? '').includes(
        'that path is not one of the folders this server allows',
      ),
      'the picker hid the reason the server gave for refusing a folder',
    ).toBe(true)
  })
})

describe('a .torrent file', () => {
  const torrentBytes = (): Uint8Array<ArrayBuffer> =>
    new Uint8Array([0x64, 0x38, 0x3a, 0x69, 0x6e, 0x66, 0x6f, 0x65, 0x31, 0x33, 0x3a, 0x6c, 0x65, 0x6e, 0x67, 0x74, 0x68, 0x69, 0x35, 0x3a, 0x31, 0x32, 0x33, 0x34, 0x35])

  const torrentFile = (): File =>
    new File([torrentBytes()], 'sintel.torrent', { type: 'application/x-bittorrent' })

  function openFileTab(container: HTMLElement): void {
    fireEvent.click(screen.getByRole('tab', { name: 'Torrent file' }))
    dropArea(container)
  }

  it('a_torrent_file_is_uploaded_as_is', async () => {
    const { client, seen } = server()
    const { container } = openDialog(client)
    await flush()
    openFileTab(container)

    fireEvent.drop(dropArea(container), { dataTransfer: { files: [torrentFile()] } })
    await flush()
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    await flush()

    const post = addRequest(seen)
    const headers = post?.headers.get('content-type') ?? ''
    const sent =
      post?.body instanceof ArrayBuffer
        ? Array.from(new Uint8Array(post.body)).join(',')
        : post === null || post.body === null
          ? 'no body'
          : `body is ${typeof post.body}`
    const url = post === null ? null : new URL(post.url, BASE)
    expect(
      headers === 'application/x-bittorrent' &&
        sent === Array.from(torrentBytes()).join(','),
      'the dialog did not send the .torrent as application/x-bittorrent with its exact bytes',
    ).toBe(true)
    expect(
      url !== null &&
        url.pathname === '/api/v1/torrents' &&
        url.searchParams.get('save_path') === '/downloads' &&
        url.searchParams.get('paused') === 'false',
      'the file add did not put its folder and its paused flag in the query',
    ).toBe(true)
  })

  it('the_file_input_is_a_second_way_in', async () => {
    const { client, seen } = server()
    const { container } = openDialog(client)
    await flush()
    openFileTab(container)

    const input = screen.getByLabelText('Choose file') as HTMLInputElement
    expect(
      input.getAttribute('accept') === '.torrent,application/x-bittorrent',
      'the file chooser accepts something other than a torrent file',
    ).toBe(true)

    fireEvent.change(input, { target: { files: [torrentFile()] } })
    await flush()
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    await flush()

    const post = addRequest(seen)
    const sent =
      post?.body instanceof ArrayBuffer ? Array.from(new Uint8Array(post.body)).join(',') : 'no bytes'
    expect(
      post?.headers.get('content-type') === 'application/x-bittorrent' &&
        sent === Array.from(torrentBytes()).join(','),
      'a .torrent chosen with the file chooser was not uploaded as it is',
    ).toBe(true)
  })

  it('an_oversize_file_is_refused_without_a_request', async () => {
    const { client, seen } = server()
    const { container } = openDialog(client)
    await flush()
    openFileTab(container)

    const huge = new File([new Uint8Array(10 * 1024 * 1024 + 1)], 'huge.torrent')
    fireEvent.drop(dropArea(container), { dataTransfer: { files: [huge] } })
    await flush()
    expect(
      (container.textContent ?? '').includes('That file is too large to be a torrent.') &&
        postCount(seen) === 0 &&
        (screen.getByRole('button', { name: 'Add' }) as HTMLButtonElement).disabled,
      'a file over ten MiB was accepted or sent to the server',
    ).toBe(true)
  })

  it('an_empty_file_is_refused_without_a_request', async () => {
    const { client, seen } = server()
    const { container } = openDialog(client)
    await flush()
    openFileTab(container)

    fireEvent.drop(dropArea(container), { dataTransfer: { files: [new File([], 'empty.torrent')] } })
    await flush()
    expect(
      (container.textContent ?? '').includes('That file is empty.') &&
        postCount(seen) === 0 &&
        (screen.getByRole('button', { name: 'Add' }) as HTMLButtonElement).disabled,
      'an empty file was accepted or sent to the server',
    ).toBe(true)
  })
})

describe('what the server answers', () => {
  it('an_add_error_keeps_the_dialog_open', async () => {
    const { client, seen } = server({
      dirs: { '/downloads': ['/downloads/films'], '/downloads/films': [] },
      add: () =>
        json(409, '{"error":{"code":"duplicate_torrent","message":"that torrent is already in the list"}}'),
    })
    const { container, events } = openDialog(client)
    await flush()
    pressFolder(container, '/downloads/films')
    await flush()

    const field = magnetField()
    fireEvent.change(field, { target: { value: MAGNET } })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    await flush()

    // The refusal is words in a dialog that kept everything typed into it: same fields, same folder,
    // and exactly the one request it made.
    const open = screen.queryByRole('dialog')
    expect(
      open !== null &&
        document.contains(field) &&
        field.value === MAGNET &&
        (open?.textContent ?? '').includes('that torrent is already in the list') &&
        chosenPath(container) === '/downloads/films' &&
        postCount(seen) === 1 &&
        events.length === 0,
      'a refused add closed the dialog or lost what was typed into it',
    ).toBe(true)
  })

  it('a_successful_add_closes_the_dialog_and_announces', async () => {
    const { client, seen } = server()
    const { container, polite, events } = openDialog(client)
    await flush()

    fireEvent.change(magnetField(), { target: { value: MAGNET } })
    fireEvent.click(screen.getByRole('checkbox', { name: 'Start paused' }))
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    await flush()

    expect(
      screen.queryByRole('dialog') === null &&
        polite.textContent === 'Torrent added.' &&
        events.join(',') === 'added,closed',
      'a taken add did not close the dialog and say so through the polite region',
    ).toBe(true)
    expect(
      (container.textContent ?? '').includes('That file is empty.') === false,
      'a successful add left a stray message behind',
    ).toBe(true)
    const body = addedJson(seen)
    expect(
      body?.paused === true && body?.magnet === MAGNET,
      'the add did not carry the Start paused answer to the server',
    ).toBe(true)
  })

  it('a_non_magnet_is_refused_without_a_request', async () => {
    const { client, seen } = server()
    const { container } = openDialog(client)
    await flush()
    fireEvent.change(magnetField(), { target: { value: 'https://example.org/torrents/7.torrent' } })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    await flush()
    expect(
      (container.textContent ?? '').includes('That is not a magnet link.') &&
        postCount(seen) === 0 &&
        screen.queryByRole('dialog') !== null,
      'something that is not a magnet link was sent to the server',
    ).toBe(true)

    // A magnet spelled with capital letters is still a magnet.
    fireEvent.change(magnetField(), { target: { value: 'MAGNET:?xt=urn:btih:0000000000000000000000000000000000000000' } })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
    await flush()
    expect(postCount(seen), 'a magnet link with capital letters was refused').toBe(1)
  })
})

describe('the keyboard', () => {
  it('the_add_dialog_traps_the_focus_it_took', async () => {
    const { client, seen } = server()
    const { container } = openDialog(client)
    await flush()
    const opener = screen.getByRole('button', { name: 'Add torrent' })

    const dialog = screen.getByRole('dialog')
    expect(
      document.activeElement !== null && dialog.contains(document.activeElement),
      'opening the add dialog left the focus outside it',
    ).toBe(true)

    // Tab at the last control wraps to the first, and Shift Tab at the first wraps to the last: the
    // keyboard cannot walk out of the dialog into the table behind it.
    const close = screen.getByRole('button', { name: 'Close' })
    const add = screen.getByRole('button', { name: 'Add' })
    add.focus()
    const taken = fireEvent.keyDown(dialog, { key: 'Tab' })
    expect(
      taken === false && document.activeElement === close,
      'Tab at the last control left the dialog instead of wrapping to the first',
    ).toBe(true)
    close.focus()
    fireEvent.keyDown(dialog, { key: 'Tab', shiftKey: true })
    expect(document.activeElement, 'Shift Tab at the first control did not wrap to the last control').toBe(add)

    // Escape closes it, asks the server nothing, and gives the focus back where it came from.
    fireEvent.keyDown(dialog, { key: 'Escape' })
    await flush()
    expect(
      screen.queryByRole('dialog') === null &&
        postCount(seen) === 0 &&
        document.activeElement === opener,
      'Escape left the add dialog open, sent something, or kept the focus',
    ).toBe(true)
    expect(container.querySelector('dialog'), 'the add dialog element stayed in the page').toBeNull()
  })
})
