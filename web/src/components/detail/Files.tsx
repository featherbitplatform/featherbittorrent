/**
 * The Files tab: what the selected torrent is made of and how much of it is on disk.
 *
 * The columns are the prototype's - path, size, done and priority - with no "Play in browser" in
 * this milestone: a row's priority is a native select that rewrites the whole priority list (see
 * `change`), and the only button a row may carry is Play. BEP 47 pad files (`pad === true`, length is padding, never written to disk) are
 * hidden the way the TUI hides them, so a row count here counts real files. `downloaded` counts
 * bytes that live inside verified pieces, which is why the TUI treats a file at or past its length
 * as finished and draws it in the seeding colour even while the torrent still wants the rest; a
 * zero-length file counts as whole, as `share` does in `fbtt-tui/src/view/detail.rs`. Paths are
 * remote text and go through `displaySafe` before they are drawn.
 *
 * A row whose path ends in one of the playable video extensions gets a `Play <path>` button (the
 * path the row shows, so each row's button has a name of its own), the way
 * milestone 2 can act on it: the button opens the player, and the player decides what the browser
 * will do with the file. The extensions are matched case-insensitively, and a pad file never gets
 * one, because a pad file is padding and not a file.
 *
 * Rows are the pane's 28 px height with the table's own windowing shrunk to the pane (see
 * `window.ts`), because a torrent can hold thousands of files and the pane must not render them all.
 *
 * @module components/detail/Files
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { Play } from "lucide-react";
import { errorMessage } from "../../api/client";
import type { Api, FileInfo } from "../../api/client";
import Banner from "../Banner";
import { displaySafe, formatBytes, formatPercent } from "../../format";
import { DETAIL_ROW_HEIGHT, DETAIL_ROW_RULE, useRowWindow } from "./window";

export interface FilesProps {
  files: FileInfo[];
  /** The scroll box's height in pixels. Given by tests because jsdom lays nothing out. */
  viewportHeight?: number;
  /** The torrent the files belong to, addressed the way every other call addresses it. */
  id?: string;
  /** Open the player on a file. Only called for a file the player can open. */
  onPlay?: (index: number) => void;
  /** The client a priority change goes through. Without it the priority is only a word. */
  client?: Api;
}

/** The priorities a file can be given. `unknown` is only ever reported, never requested. */
const CHOICES = ["skip", "low", "normal", "high"] as const;
type Choice = (typeof CHOICES)[number];

/** The path endings the player offers to play, matched case-insensitively. */
const PLAYABLE_SUFFIXES = [".mkv", ".mp4", ".m4v", ".webm", ".mov"];

/** Whether a path is one the player opens, the way the Files tab offers to play it. */
function isPlayablePath(path: string): boolean {
  const lower = path.toLowerCase();
  return PLAYABLE_SUFFIXES.some((suffix) => lower.endsWith(suffix));
}

export default function Files({
  files,
  viewportHeight,
  id,
  onPlay,
  client,
}: FilesProps) {
  const [error, setError] = useState<string | null>(null);
  // One change at a time: the ref guards synchronously (two events can land before a render),
  // the state disables the selects while a change is in flight.
  const busyRef = useRef(false);
  const [busy, setBusy] = useState(false);
  // What the user just chose, by file index. A choice is held from the moment its change completes
  // until the first files list that arrives after that: that list is the server's own answer, even
  // when someone else changed the file again. A list that landed while the change was in flight is
  // the one current at completion (`base`), so it does not count.
  const [held, setHeld] = useState<Record<number, { choice: Choice; base: FileInfo[] }>>({});
  const latestFiles = useRef(files);
  useEffect(() => {
    latestFiles.current = files;
  }, [files]);
  const spent = Object.entries(held).filter(([, entry]) => entry.base !== files);
  if (spent.length > 0) {
    // Dropping during render is React's own pattern for state derived from props.
    const next = { ...held };
    for (const [index] of spent) delete next[Number(index)];
    setHeld(next);
  }
  const chosen = held;

  /**
   * The server wants one entry per file, pads included, so the list is read back first, the one
   * entry changed and the whole list sent (the TUI's rule), and a stale pane cannot undo another
   * client's change to a file it did not touch.
   */
  async function change(index: number, priority: Choice): Promise<void> {
    if (client === undefined || id === undefined || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const fresh = [...(await client.files(id))].sort(
        (a, b) => a.index - b.index,
      );
      if (!fresh.some((entry) => entry.index === index)) {
        setError(`no file with index ${index}`);
        return;
      }
      // The request is positional, so the list must run 0..n-1 with no gap.
      if (fresh.some((entry, position) => entry.index !== position)) {
        setError("the file list has gaps, so priorities cannot be changed");
        return;
      }
      const priorities = fresh.map((entry): Choice =>
        entry.index === index
          ? priority
          : entry.priority === "unknown"
            ? "normal"
            : entry.priority,
      );
      await client.setFilePriorities(id, priorities);
      setHeld((now) => ({
        ...now,
        [index]: { choice: priority, base: latestFiles.current },
      }))
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }

  const shown = useMemo(() => files.filter((file) => !file.pad), [files]);
  const { boxRef, onScroll, start, end, padTop, padBottom } = useRowWindow(
    shown.length,
    viewportHeight,
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {error !== null && <Banner tone="danger">{error}</Banner>}
      <div
        ref={boxRef}
        onScroll={onScroll}
        className="min-h-0 flex-1 overflow-auto"
      >
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
              <th scope="col" className="px-3 py-2 text-right font-medium">
                <span className="sr-only">Play</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {padTop > 0 && (
              <tr
                data-spacer="top"
                aria-hidden="true"
                style={{ height: padTop }}
              >
                <td colSpan={5} />
              </tr>
            )}
            {shown.slice(start, end).map((file) => {
              // The TUI's `share`: an empty file counts as whole, so it reads 100 % and not NaN.
              const fraction = file.len === 0 ? 1 : file.downloaded / file.len;
              const complete = file.downloaded >= file.len;
              const path = displaySafe(file.path);
              return (
                <tr
                  key={file.index}
                  data-row={file.index}
                  style={{ height: DETAIL_ROW_HEIGHT }}
                >
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
                    className={`box-border h-7 truncate px-3 text-right font-mono text-[12px] leading-4 ${complete ? "text-seeding" : "text-body"} ${DETAIL_ROW_RULE}`}
                  >
                    {formatPercent(fraction)}
                  </td>
                  <td
                    className={`box-border h-7 truncate px-3 text-[12px] leading-4 text-body max-[640px]:overflow-visible ${DETAIL_ROW_RULE}`}
                  >
                    {client !== undefined && id !== undefined ? (
                      <select
                        aria-label={`Priority of ${path}`}
                        value={
                          chosen[file.index]?.choice ??
                          (file.priority === "unknown"
                            ? "normal"
                            : file.priority)
                        }
                        disabled={busy}
                        onChange={(event) => {
                          void change(file.index, event.target.value as Choice);
                        }}
                        className="h-6 max-[640px]:-my-2.5 max-[640px]:h-11 cursor-pointer rounded-control border border-rule bg-transparent px-1 text-[12px] text-body max-[640px]:text-base"
                      >
                        {CHOICES.map((choice) => (
                          <option key={choice} value={choice}>
                            {choice}
                          </option>
                        ))}
                      </select>
                    ) : (
                      file.priority
                    )}
                  </td>
                  <td
                    className={`box-border h-7 px-3 text-right ${DETAIL_ROW_RULE}`}
                  >
                    {id !== undefined &&
                      onPlay !== undefined &&
                      !file.pad &&
                      isPlayablePath(file.path) && (
                        <button
                          type="button"
                          aria-label={`Play ${path}`}
                          onClick={() => {
                            onPlay(file.index);
                          }}
                          className="inline-flex h-6 w-6 cursor-pointer items-center justify-center rounded-control text-muted hover:bg-ink-700 hover:text-strong"
                        >
                          <Play className="h-3.5 w-3.5" aria-hidden="true" />
                        </button>
                      )}
                  </td>
                </tr>
              );
            })}
            {shown.length === 0 && (
              <tr>
                <td colSpan={5} className="px-5 py-3 text-[12px] text-muted">
                  No files.
                </td>
              </tr>
            )}
            {padBottom > 0 && (
              <tr
                data-spacer="bottom"
                aria-hidden="true"
                style={{ height: padBottom }}
              >
                <td colSpan={5} />
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
