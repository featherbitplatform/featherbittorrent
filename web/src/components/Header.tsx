/**
 * The header: who this page is, whether its server is answering, and what is moving.
 *
 * The shape is the Torrents prototype's header strip: the mark and the word with the library's
 * version in mono beside them, a bordered pill that says whether the link is up (the connection
 * cyan, or the error red with the pause the refresh loop is waiting out), the total rates summed
 * over every row with the arrows muted and the numbers strong the way the terminal states them,
 * the DHT size where the server gives one, and the actions at the end. The Add torrent button is
 * the violet fill the plan names for the primary action, and it answers nothing yet: it is present
 * so the header is the header the finished screen will have. Sign out appears only when there is
 * a session to end, because on a server with logins off there is nobody signed in.
 *
 * Nothing here is remote text except the version, and that came from this server's own `/session`
 * route; the pill's words are this file's, built around a number.
 *
 * @module components/Header
 */
import { LogOut, Plus } from 'lucide-react'
import { formatRate } from '../format'
import type { LinkState } from '../live'
import Button from './Button'

export interface HeaderProps {
  /** The library version from `GET /api/v1/session`, or null until that answer has come. */
  version: string | null
  /** How the link to the server stands, as the refresh loop last reported it. */
  link: LinkState
  /** Bytes per second summed over every row, whatever the filters show. */
  totalDown: number
  totalUp: number
  /** Nodes in the server's DHT routing tables, or null when it is not running one: a host with no DHT gets no node count rather than a count of nothing. */
  dhtNodes: number | null
  /** Whether there is a session to end. */
  signedIn: boolean
  onSignOut(): void
}

export default function Header({
  version,
  link,
  totalDown,
  totalUp,
  dhtNodes,
  signedIn,
  onSignOut,
}: HeaderProps) {
  const lost = link.phase === 'lost'
  return (
    <header className="flex flex-wrap items-center gap-4 border-b border-rule bg-sunken px-5 py-3">
      <div className="flex items-center gap-2.5">
        <img className="h-6.5 w-auto object-contain" src="/favicon.png" alt="" />
        <span className="text-[15px] font-semibold tracking-[-0.01em] text-strong">
          featherbittorrent
        </span>
        {version !== null && <span className="font-mono text-xs text-muted">{version}</span>}
      </div>

      <div className="flex items-center gap-2 rounded-control border border-rule bg-surface px-2.5 py-1">
        <span aria-hidden="true" className={`h-2 w-2 rounded-full ${lost ? 'bg-error' : 'bg-connection'}`} />
        <span className={`text-[13px] font-medium ${lost ? 'text-error' : 'text-strong'}`}>
          {lost ? `Reconnecting in ${link.retryIn} s` : 'Connected'}
        </span>
      </div>

      <div className="ml-auto flex flex-wrap items-center gap-4 font-mono text-[13px]">
        <span className="text-muted">
          ↓ <span className="text-strong">{formatRate(totalDown)}</span>
        </span>
        <span className="text-muted">
          ↑ <span className="text-strong">{formatRate(totalUp)}</span>
        </span>
        {dhtNodes !== null && <span className="text-muted">DHT {dhtNodes} nodes</span>}
      </div>

      <div className="flex items-center gap-2">
        <Button disabled>
          <Plus className="h-4 w-4" aria-hidden="true" />
          Add torrent
        </Button>
        {signedIn && (
          <Button variant="secondary" onClick={onSignOut}>
            <LogOut className="h-4 w-4" aria-hidden="true" />
            Sign out
          </Button>
        )}
      </div>
    </header>
  )
}
