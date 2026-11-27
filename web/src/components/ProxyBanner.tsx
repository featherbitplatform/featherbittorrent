/**
 * The header's proxy warning: a button that says the proxy cannot be reached and opens Settings at
 * the Proxy section.
 *
 * It asks `GET /api/v1/settings/proxy/state` every five seconds, and only while it has something to
 * ask about: the page is visible, Torrents is the screen in front, and the settings say a proxy is set.
 * The settings are read when the banner mounts and each time the Torrents screen comes back to the
 * front (which is when a proxy saved in Settings shows up), never on a timer, so a page with no proxy
 * makes no repeating request at all. A server that answers `404` is older than the proxy setting: there
 * is nothing to show and nothing more to ask. A network error keeps what the banner last said and says
 * nothing itself; the header's own pill is where a lost link shows.
 *
 * Only `unreachable` and `login_refused` show anything here. `udp_not_relayed` has its own explanation
 * beside the proxy fields, and `ok` and `off` have nothing to say. The words are the product's: the
 * reason is the server's, already display-safe.
 *
 * @module components/ProxyBanner
 */
import { useEffect, useState } from 'react'
import { CircleAlert } from 'lucide-react'
import type { Api, ProxyHealth } from '../api/client'

/** How often the proxy state is asked for while the banner has reason to. */
const POLL_MS = 5_000

export interface ProxyBannerProps {
  api: Pick<Api, 'settings' | 'proxyState'>
  /** Opens Settings at the named section. */
  onOpenSettings(section: string): void
  /** Whether the Torrents screen is the one in front: false while Settings covers it. */
  active?: boolean
  /** The poll period, for tests. */
  intervalMs?: number
}

/** The one line for a health, or null when it has nothing to say here. */
function proxyHeadline(health: ProxyHealth | null): string | null {
  if (health === null) return null
  if (health.state === 'login_refused') return "Can't reach the proxy: it refused the login"
  if (health.state === 'unreachable') {
    return health.reason ? `Can't reach the proxy: ${health.reason}` : "Can't reach the proxy"
  }
  return null
}

export default function ProxyBanner({ api, onOpenSettings, active = true, intervalMs = POLL_MS }: ProxyBannerProps) {
  const [proxySet, setProxySet] = useState(false)
  const [health, setHealth] = useState<ProxyHealth | null>(null)

  // Whether a proxy is set: read on mount and whenever this screen comes back to the front.
  useEffect(() => {
    if (!active) return
    let live = true
    Promise.resolve()
      .then(() => api.settings())
      .then(
        (settings) => {
          if (!live) return
          const set = settings.proxy !== undefined && settings.proxy.type !== 'none'
          setProxySet(set)
          if (!set) setHealth(null)
        },
        () => {
          // A server that will not say keeps what the banner knew.
        },
      )
    return () => {
      live = false
    }
  }, [api, active])

  // The state, while there is a proxy to ask about and somebody to see the answer.
  useEffect(() => {
    if (!proxySet || !active) return
    let live = true
    let stop = false
    const ask = (): void => {
      if (stop || document.visibilityState === 'hidden') return
      Promise.resolve()
        .then(() => api.proxyState())
        .then(
          (next) => {
            if (live) setHealth(next)
          },
          (problem: unknown) => {
            // A 404 is an older server: nothing to show, nothing more to ask.
            if ((problem as { status?: number } | null)?.status === 404) {
              stop = true
              clearInterval(timer)
              if (live) setHealth(null)
            }
          },
        )
    }
    const timer = setInterval(ask, intervalMs)
    ask()
    const onVisible = (): void => {
      if (document.visibilityState === 'visible') ask()
    }
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      live = false
      clearInterval(timer)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [api, proxySet, active, intervalMs])

  const headline = proxySet ? proxyHeadline(health) : null
  if (headline === null) return null
  return (
    <button
      type="button"
      onClick={() => {
        onOpenSettings('proxy')
      }}
      className="flex min-w-0 items-center gap-2 rounded-control border border-error/45 bg-error/8 px-2.5 py-1 text-left text-[13px] font-medium text-error"
    >
      <CircleAlert className="h-4 w-4 flex-none" aria-hidden="true" />
      <span className="truncate">{headline}</span>
    </button>
  )
}
