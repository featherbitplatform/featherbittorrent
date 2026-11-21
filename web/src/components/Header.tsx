/**
 * The header: who this page is, whether its server is answering, and what is moving.
 *
 * The shape is the Torrents prototype's header strip: the mark and the word with featherbittorrent's
 * own version in mono beside them - the version baked into this bundle at build time from the
 * workspace `Cargo.toml`, with the engine library's version the server reports tucked into that
 * text's tooltip - a bordered pill that says whether the link is up (the connection cyan) or counts
 * down the pause the refresh loop is waiting out in error red, and once the pause is spent says
 * `Reconnecting...` while the retry itself runs, so the pill is never a number frozen mid-second,
 * the total rates summed over every row with the arrows muted and the numbers strong the way the
 * terminal states them, the DHT node count beside them, muted and giving way to the rates below
 * 768 px, and the actions at the end. The Add torrent button is the violet fill the plan names for
 * the primary action, and it answers nothing yet: it is present so the header is the header the
 * finished screen will have. Sign out appears only when there is a session to end, because on a
 * server with logins off there is nobody signed in.
 *
 * The visible version is this bundle's own build constant. The remote text here is the tooltip's
 * engine version and the DHT count, both from this server's own `/session` route; the pill's words
 * are this file's, built around a number.
 *
 * @module components/Header
 */
import { useEffect, useState } from 'react'
import { LogOut, Plus } from 'lucide-react'
import { formatRate } from '../format'
import type { LinkState } from '../live'
import Button from './Button'

export interface HeaderProps {
  /** The engine library's version from `GET /api/v1/session`, or null until that answer has come. It is tooltip decoration only: the version the header displays is `__APP_VERSION__`, featherbittorrent's own, injected at build time. */
  engineVersion: string | null
  /** How the link to the server stands, as the refresh loop last reported it. */
  link: LinkState
  /** Bytes per second summed over every row, whatever the filters show. */
  totalDown: number
  totalUp: number
  /** Nodes in the server's DHT routing tables. The count shows whatever the server says: null, like zero, reads as `DHT 0`. */
  dhtNodes: number | null
  /** Whether there is a session to end. */
  signedIn: boolean
  onSignOut(): void
}

/** The pill's words while a retry pause is running: it counts down once per second and, when the pause is spent, says the retry is happening rather than showing a frozen number. The header keys it on the loop's failure count rather than on the pause, because the top of the ladder repeats: two 30-second pauses in a row are two failures, and the second one starts its count over instead of inheriting the first one's spent zero. */
function PauseCountdown({ retryIn }: { retryIn: number }) {
  const [secondsLeft, setSecondsLeft] = useState(retryIn)
  useEffect(() => {
    const timer = setInterval(() => {
      setSecondsLeft((left) => (left > 0 ? left - 1 : 0))
    }, 1_000)
    return () => {
      clearInterval(timer)
    }
  }, [])
  if (secondsLeft <= 0) return <>Reconnecting...</>
  return <>Reconnecting in {secondsLeft} s</>
}

export default function Header({
  engineVersion,
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
        <span
          className="font-mono text-xs text-muted"
          title={engineVersion === null ? undefined : `engine ${engineVersion}`}
        >
          v{__APP_VERSION__}
        </span>
      </div>

      <div className="flex items-center gap-2 rounded-control border border-rule bg-surface px-2.5 py-1">
        <span aria-hidden="true" className={`h-2 w-2 rounded-full ${lost ? 'bg-error' : 'bg-connection'}`} />
        <span className={`text-[13px] font-medium ${lost ? 'text-error' : 'text-strong'}`}>
          {lost ? <PauseCountdown key={link.attempt} retryIn={link.retryIn} /> : 'Connected'}
        </span>
      </div>

      <div className="ml-auto flex flex-wrap items-center gap-4 font-mono text-[13px]">
        <span className="text-muted">
          ↓ <span className="text-strong">{formatRate(totalDown)}</span>
        </span>
        <span className="text-muted">
          ↑ <span className="text-strong">{formatRate(totalUp)}</span>
        </span>
        <span className="text-muted max-md:hidden">DHT {dhtNodes ?? 0}</span>
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
