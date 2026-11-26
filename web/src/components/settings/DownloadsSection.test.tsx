// @vitest-environment jsdom
/**
 * The Downloads section of Settings: the default and unfinished folders, start paused, a watch folder and
 * the program to run on completion, saved through `PUT /api/v1/settings/downloads`.
 *
 * The program is the one field a server never lets a browser set: the section draws it as a field only when
 * the server says `editable`, and sends it only then and only when it changed.
 *
 * @module components/settings/DownloadsSection.test
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, describe, expect, it } from 'vitest'
import DownloadsSection from './DownloadsSection'
import { TOO_OLD } from './SpeedSection'
import { SectionDirtyContext } from './useSectionDirty'
import type { DirtyEntry } from './useSectionDirty'
import { createClient } from '../../api/client'
import type { DownloadsSettings } from '../../api/client'

afterEach(cleanup)

const SPEED = { download_kib: null, upload_kib: null, active_downloads: 3, active_seeds: 5, active_total: 500 }
const BASE: DownloadsSettings = {
  default_dir: '/data',
  unfinished_dir: null,
  start_paused: false,
  watch_dir: null,
  on_complete: { program: null, editable: false },
  watch_problem: null,
  on_complete_problem: null,
}

interface Call {
  method: string
  url: string
  body: Record<string, unknown> | undefined
}

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
}

/** The real client over a fake fetch that answers the settings routes and the two folder routes. */
function server(
  downloads: Partial<DownloadsSettings> = {},
  options: { put?: (body: Record<string, unknown>) => Response; settings?: () => Response } = {},
) {
  const calls: Call[] = []
  const held = { ...BASE, ...downloads }
  const fetchFn = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const call: Call = {
      method: init?.method ?? 'GET',
      url,
      body: init?.body ? (JSON.parse(String(init.body)) as Record<string, unknown>) : undefined,
    }
    calls.push(call)
    if (url === '/api/v1/fs/roots') return json(200, ['/data'])
    if (url.startsWith('/api/v1/fs/list')) return json(200, { path: '/data', dirs: ['part', 'watch'] })
    if (call.method === 'PUT') {
      if (options.put !== undefined) return options.put(call.body!)
      const next = { ...held, ...call.body, on_complete: held.on_complete }
      const program = (call.body?.on_complete as { program?: string | null } | undefined)?.program
      if (program !== undefined) next.on_complete = { ...held.on_complete, program }
      return json(200, { speed: SPEED, downloads: next })
    }
    return options.settings?.() ?? json(200, { speed: SPEED, downloads: held })
  }) as typeof fetch
  return { calls, client: createClient(fetchFn, () => 'tok', () => 0, () => {}) }
}

const puts = (calls: Call[]) => calls.filter((c) => c.method === 'PUT')
const save = () => fireEvent.click(screen.getByRole('button', { name: 'Save' }))
const paused = () => screen.getByRole('checkbox', { name: 'Start torrents paused' }) as HTMLInputElement

describe('the Downloads section', () => {
  it('shows what the server holds', async () => {
    const { client } = server({ unfinished_dir: '/data/part', start_paused: true })
    render(<DownloadsSection client={client} />)
    await screen.findByText('/data')
    expect(paused().checked).toBe(true)
    expect(screen.getByText('/data/part')).toBeTruthy()
    expect(screen.getByText('Default folder')).toBeTruthy()
    expect(screen.getByText('Unfinished folder')).toBeTruthy()
    expect(screen.getByText('Watch folder')).toBeTruthy()
  })

  it('downloads_section_shows_set_in_the_servers_config_file_when_not_editable', async () => {
    const { client } = server({ on_complete: { program: '/usr/bin/notify', editable: false } })
    render(<DownloadsSection client={client} />)
    await screen.findByText('/data')
    expect(screen.queryByLabelText('Run a program on completion')).toBeNull()
    expect(screen.getByText("Set in the server's config file")).toBeTruthy()
    expect(screen.getByText('/usr/bin/notify')).toBeTruthy()
  })

  it('says None when no program is set and it cannot be edited', async () => {
    const { client } = server()
    render(<DownloadsSection client={client} />)
    await screen.findByText('/data')
    expect(screen.getByText('None')).toBeTruthy()
  })

  it('not_editable_save_never_sends_on_complete', async () => {
    const { client, calls } = server({ on_complete: { program: '/usr/bin/notify', editable: false } })
    render(<DownloadsSection client={client} />)
    await screen.findByText('/data')
    fireEvent.click(paused())
    save()
    await screen.findByText('Saved.')
    const put = puts(calls)[0]
    expect(put.url).toBe('/api/v1/settings/downloads')
    expect(put.body).toEqual({ default_dir: '/data', unfinished_dir: null, start_paused: true, watch_dir: null })
    expect('on_complete' in put.body!).toBe(false)
  })

  it('editable_program_is_sent', async () => {
    const { client, calls } = server({ on_complete: { program: null, editable: true } })
    render(<DownloadsSection client={client} />)
    const input = (await screen.findByLabelText('Run a program on completion')) as HTMLInputElement
    expect(screen.queryByText("Set in the server's config file")).toBeNull()
    fireEvent.change(input, { target: { value: '/usr/bin/x' } })
    save()
    await screen.findByText('Saved.')
    expect(puts(calls)[0].body).toEqual({
      default_dir: '/data',
      unfinished_dir: null,
      start_paused: false,
      watch_dir: null,
      on_complete: { program: '/usr/bin/x' },
    })
  })

  it('editable_but_unchanged_program_is_not_sent', async () => {
    const { client, calls } = server({ on_complete: { program: '/usr/bin/x', editable: true } })
    render(<DownloadsSection client={client} />)
    await screen.findByDisplayValue('/usr/bin/x')
    fireEvent.click(paused())
    save()
    await screen.findByText('Saved.')
    expect('on_complete' in puts(calls)[0].body!).toBe(false)
  })

  it('clearing_an_editable_program_sends_no_program', async () => {
    const { client, calls } = server({ on_complete: { program: '/usr/bin/x', editable: true } })
    render(<DownloadsSection client={client} />)
    const input = await screen.findByDisplayValue('/usr/bin/x')
    fireEvent.change(input, { target: { value: '  ' } })
    save()
    await screen.findByText('Saved.')
    expect(puts(calls)[0].body!.on_complete).toEqual({ program: null })
  })

  it('refusal_message_is_shown', async () => {
    const reason = 'The unfinished folder must be different from the default folder.'
    const { client } = server(
      { on_complete: { program: null, editable: true } },
      { put: () => json(400, { error: { code: 'invalid_setting', message: reason } }) },
    )
    render(<DownloadsSection client={client} />)
    const input = (await screen.findByLabelText('Run a program on completion')) as HTMLInputElement
    fireEvent.change(input, { target: { value: '/usr/bin/x' } })
    fireEvent.click(paused())
    save()
    expect((await screen.findByRole('alert')).textContent).toContain(reason)
    expect(input.value).toBe('/usr/bin/x')
    expect(paused().checked).toBe(true)
    expect(screen.queryByText('Saved.')).toBeNull()
  })

  it('watch_problem_is_shown', async () => {
    const { client } = server({
      watch_dir: '/data/watch',
      watch_problem: 'The watch folder must be different from the download folders.',
      on_complete: { program: '/bin/x', editable: false },
      on_complete_problem: 'No program at that path.',
    })
    render(<DownloadsSection client={client} />)
    await screen.findByText('/data/watch')
    expect(screen.getByText('The watch folder must be different from the download folders.')).toBeTruthy()
    expect(screen.getByText('No program at that path.')).toBeTruthy()
  })

  it('the_off_switches_send_null_folders', async () => {
    const { client, calls } = server({ unfinished_dir: '/data/part', watch_dir: '/data/watch' })
    render(<DownloadsSection client={client} />)
    await screen.findByText('/data/part')
    fireEvent.click(screen.getByRole('checkbox', { name: 'Use an unfinished folder' }))
    fireEvent.click(screen.getByRole('checkbox', { name: 'Use a watch folder' }))
    save()
    await screen.findByText('Saved.')
    expect(puts(calls)[0].body).toEqual({ default_dir: '/data', unfinished_dir: null, start_paused: false, watch_dir: null })
  })

  it('a_folder_is_chosen_with_the_picker_on_the_servers_roots', async () => {
    const { client, calls } = server()
    render(<DownloadsSection client={client} />)
    await screen.findByText('/data')
    fireEvent.click(screen.getByRole('checkbox', { name: 'Use an unfinished folder' }))
    fireEvent.click(screen.getByRole('button', { name: 'Choose Unfinished folder' }))
    const part = await waitFor(() => {
      const row = document.querySelector('[data-folder="/data/part"]')
      if (row === null) throw new Error('not listed yet')
      return row
    })
    fireEvent.click(part)
    save()
    await screen.findByText('Saved.')
    expect(puts(calls)[0].body!.unfinished_dir).toBe('/data/part')
  })

  it('an old server says it is too old and shows no form', async () => {
    const { client } = server({}, { settings: () => json(404, { error: { code: 'not_found', message: 'nothing' } }) })
    render(<DownloadsSection client={client} />)
    await screen.findByText(TOO_OLD)
    expect(screen.queryByRole('button', { name: 'Save' })).toBeNull()
  })

  it('registers_unsaved_changes_with_the_page', async () => {
    const { client } = server()
    let entry: (() => DirtyEntry) | null = null
    const registry = { register: (next: (() => DirtyEntry) | null) => void (entry = next) }
    const Wrap = ({ children }: { children: ReactNode }) => (
      <SectionDirtyContext.Provider value={registry}>{children}</SectionDirtyContext.Provider>
    )
    render(<DownloadsSection client={client} />, { wrapper: Wrap })
    await screen.findByText('/data')
    expect(entry!().dirty).toBe(false)
    fireEvent.click(paused())
    expect(entry!().dirty).toBe(true)
    await expect(entry!().save()).resolves.toBe(true)
    await waitFor(() => expect(entry!().dirty).toBe(false))
  })
})
