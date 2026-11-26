/**
 * The list of Settings sections: a `nav` of buttons under group headings, and below the `sm`
 * breakpoint a `Jump to` menu instead.
 *
 * The open section carries `aria-current="page"`. The list is one tab stop (roving tabindex): the
 * arrow keys move to the previous or next section and open it, Home and End go to the first and last,
 * and the focus stays on the list's button (`onSelect(id, 'arrow')`); a click, Enter or Space is
 * `onSelect(id, 'click')`, and the page then moves the focus to the section's heading.
 *
 * @module components/settings/SectionList
 */
import { useEffect, useRef } from 'react'
import type { KeyboardEvent } from 'react'
import type { Group, SectionDef, SectionId } from './sections'

export interface SectionListProps {
  sections: SectionDef[]
  current: SectionId
  onSelect(id: SectionId, how: 'arrow' | 'click'): void
}

function groups(sections: SectionDef[]): Array<{ group: Group; items: SectionDef[] }> {
  const out: Array<{ group: Group; items: SectionDef[] }> = []
  for (const s of sections) {
    const last = out[out.length - 1]
    if (last !== undefined && last.group === s.group) last.items.push(s)
    else out.push({ group: s.group, items: [s] })
  }
  return out
}

export default function SectionList({ sections, current, onSelect }: SectionListProps) {
  const nav = useRef<HTMLElement>(null)
  // An arrow key asked for the section: once it is the open one, the focus follows it (it stays on the list).
  const followFocus = useRef(false)
  useEffect(() => {
    if (!followFocus.current) return
    followFocus.current = false
    nav.current?.querySelector<HTMLElement>('[aria-current="page"]')?.focus()
  }, [current])

  function onKeyDown(event: KeyboardEvent<HTMLElement>): void {
    const from = sections.findIndex((s) => s.id === current)
    let to: number
    switch (event.key) {
      case 'ArrowDown':
        to = Math.min(sections.length - 1, from + 1)
        break
      case 'ArrowUp':
        to = Math.max(0, from - 1)
        break
      case 'Home':
        to = 0
        break
      case 'End':
        to = sections.length - 1
        break
      default:
        return
    }
    event.preventDefault()
    if (to !== from) {
      followFocus.current = true
      onSelect(sections[to].id, 'arrow')
      // The section changes in this same event or not at all (the unsaved-changes question holds it):
      // a flag still set after that would pull the focus on some later, unrelated change.
      setTimeout(() => {
        followFocus.current = false
      }, 0)
    }
  }

  return (
    <>
      <nav ref={nav} aria-label="Settings sections" onKeyDown={onKeyDown} className="hidden w-52 flex-none flex-col gap-4 sm:flex">
        {groups(sections).map(({ group, items }) => (
          <div key={group} className="flex flex-col gap-1">
            <p className="px-3 text-[12px] uppercase tracking-wide text-muted">{group}</p>
            {items.map((s) => (
              <button
                key={s.id}
                type="button"
                tabIndex={s.id === current ? 0 : -1}
                aria-current={s.id === current ? 'page' : undefined}
                onClick={() => onSelect(s.id, 'click')}
                className={
                  'min-h-9 cursor-pointer rounded-control px-3 text-left text-[13px] outline-none ' +
                  'focus-visible:ring-2 focus-visible:ring-accent-soft ' +
                  (s.id === current ? 'bg-raised text-strong' : 'text-body hover:bg-ink-700')
                }
              >
                {s.title}
              </button>
            ))}
          </div>
        ))}
      </nav>
      <select
        aria-label="Jump to"
        value={current}
        onChange={(event) => onSelect(event.target.value as SectionId, 'click')}
        className="min-h-10 rounded-control border border-rule bg-sunken px-3 text-strong sm:hidden"
      >
        {groups(sections).map(({ group, items }) => (
          <optgroup key={group} label={group}>
            {items.map((s) => (
              <option key={s.id} value={s.id}>
                {s.title}
              </option>
            ))}
          </optgroup>
        ))}
      </select>
    </>
  )
}
