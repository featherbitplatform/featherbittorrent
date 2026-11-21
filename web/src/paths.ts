/**
 * The one place a folder *name* becomes a *path*.
 *
 * `GET /api/v1/fs/list` answers the bare names of the folders inside a path
 * (`crates/fbtt-core/src/fs_list.rs`), never their absolute paths, and `GET /api/v1/fs/roots` names
 * roots but nothing below them - so the only way the picker can reach a child folder is by joining
 * the name onto the parent's own path. Getting that join wrong is not a display bug: asking the
 * server about `music space` instead of `/downloads/music space` is refused as
 * `path_not_allowed`, exactly like a path somebody typed by hand.
 *
 * @module paths
 */

/**
 * The absolute path of a folder `list()` only named, given the path of its parent.
 *
 * The separator is the one the parent already uses: `\` for a path written the way a Windows server
 * writes them, `/` for every other parent, and a parent that already ends in its own separator
 * (`/`, `C:\`) is not doubled.
 */
export function childPath(parent: string, name: string): string {
  const separator = parent.includes('\\') && !parent.includes('/') ? '\\' : '/'
  return parent.endsWith(separator) ? parent + name : parent + separator + name
}
