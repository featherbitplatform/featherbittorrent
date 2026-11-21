/**
 * The Files tab: what the selected torrent is made of and how much of it is on disk.
 *
 * The columns are the prototype's - path, size, done and priority - with no "Play in browser" in
 * this milestone: priorities are edited in milestone 2, so this list is read-only and carries no
 * buttons at all. BEP 47 pad files (`pad === true`, length is padding, never written to disk) are
 * hidden the way the TUI hides them, so a row count here counts real files. `downloaded` counts
 * bytes that live inside verified pieces, which is why the TUI treats a file at or past its length
 * as finished and draws it in the seeding colour even while the torrent still wants the rest; a
 * zero-length file counts as whole, as `share` does in `fbtt-tui/src/view/detail.rs`. Paths are
 * remote text and go through `displaySafe` before they are drawn.
 *
 * Rows are the pane's 28 px height with the table's own windowing shrunk to the pane (see
 * `window.ts`), because a torrent can hold thousands of files and the pane must not render them all.
 *
 * @module components/detail/Files
 */
import { useMemo } from 'react'
import type { FileInfo } from '../../api/client'
import { displaySafe, formatBytes, formatPercent } from '../../format'
import { DETAIL_ROW_HEIGHT, DETAIL_ROW_RULE, useRowWindow } from './window'

export interface FilesProps {
  files: FileInfo[]
  /** The scroll box's height in pixels. Given by tests because jsdom lays nothing out. */
  viewportHeight?: number
}

export default function Files({ files, viewportHeight }: FilesProps) {
  const shown = useMemo(() => files.filter((file) => !file.pad), [files])
  const { boxRef, onScroll, start, end, padTop, padBottom } = useRowWindow(
    shown.length,
    viewportHeight,
  )

  return (
    <div ref={boxRef} onScroll={onScroll} className="max-h-56 overflow-auto">
      <table className="w-full min-w-[560px] border-separate border-spacing-0">
        <caption className="sr-only">Files of the selected torrent</caption>
        <thead>
          <tr className="text-[12px] text-muted">
            <th scope="col" className="px-5 py-2 text-left font-medium">
              File
            </th>
            <th scope="col" className="px-3 py-2 text-right font-medium">
              Size
            </th>
            <th scope="col" className="px-3 py-2 text-right font-medium">
              Done
            </th>
            <th scope="col" className="px-3 py-2 text-left font-medium">
              Priority
            </th>
          </tr>
        </thead>
        <tbody>
          {padTop > 0 && (
            <tr data-spacer="top" aria-hidden="true" style={{ height: padTop }}>
              <td colSpan={4} />
            </tr>
          )}
          {shown.slice(start, end).map((file) => {
            // The TUI's `share`: an empty file counts as whole, so it reads 100 % and not NaN.
            const fraction = file.len === 0 ? 1 : file.downloaded / file.len
            const complete = file.downloaded >= file.len
            const path = displaySafe(file.path)
            return (
              <tr key={file.index} data-row={file.index} style={{ height: DETAIL_ROW_HEIGHT }}>
                <td
                  className={`box-border h-7 max-w-[420px] truncate px-5 font-mono text-[12px] leading-4 text-ink-200 ${DETAIL_ROW_RULE}`}
                  title={path}
                >
                  {path}
                </td>
                <td
                  className={`box-border h-7 truncate px-3 text-right font-mono text-[12px] leading-4 text-body ${DETAIL_ROW_RULE}`}
                >
                  {formatBytes(file.len)}
                </td>
                <td
                  className={`box-border h-7 truncate px-3 text-right font-mono text-[12px] leading-4 ${complete ? 'text-seeding' : 'text-body'} ${DETAIL_ROW_RULE}`}
                >
                  {formatPercent(fraction)}
                </td>
                <td
                  className={`box-border h-7 truncate px-3 text-[12px] leading-4 text-body ${DETAIL_ROW_RULE}`}
                >
                  {file.priority}
                </td>
              </tr>
            )
          })}
          {shown.length === 0 && (
            <tr>
              <td colSpan={4} className="px-5 py-3 text-[12px] text-muted">
                No files.
              </td>
            </tr>
          )}
          {padBottom > 0 && (
            <tr data-spacer="bottom" aria-hidden="true" style={{ height: padBottom }}>
              <td colSpan={4} />
            </tr>
          )}
        </tbody>
      </table>
    </div>
  )
}
