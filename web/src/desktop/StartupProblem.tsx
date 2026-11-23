/**
 * What the window says when This computer did not start: the shell's line, over the whole window, and a
 * Quit button. The shell keeps the problem instead of showing a native message from inside its event
 * loop, and shows the window so this is seen; Quit exits with the code the problem carries (3 when
 * another featherbittorrent holds the library, 1 otherwise).
 *
 * It wraps the whole page, outside anything keyed by instance: with a problem it is the only thing drawn
 * (no workspace, no opened-link dialog, no close question; a close after a failed start is a quit), so it
 * wins over everything else.
 *
 * @module desktop/StartupProblem
 */
import { useEffect, useState } from 'react'
import type { ReactNode } from 'react'
import Button from '../components/Button'
import { displaySafe } from '../format'
import { quitApp, startupProblem } from './bridge'

/** The heading over the shell's line. */
export const STARTUP_PROBLEM_TITLE = 'featherbittorrent could not start'

/** `children`, unless This computer could not start: then the problem screen alone. */
export function StartupProblemHost({ children }: { children?: ReactNode }) {
  const [problem, setProblem] = useState<string | null>(null)
  const [quitting, setQuitting] = useState(false)

  useEffect(() => {
    let cancelled = false
    // The shell waits for the start a while and answers `starting` if it is still under way: ask again
    // until it says how it went, so a slow start is never taken for a fine one.
    const ask = () => {
      startupProblem()
        .then((answer) => {
          if (cancelled) return
          if (answer.state === 'starting') ask()
          else if (answer.state === 'problem') setProblem(answer.message)
        })
        .catch((cause: unknown) => {
          console.warn('Whether This computer started could not be asked:', cause)
        })
    }
    ask()
    return () => {
      cancelled = true
    }
  }, [])

  if (problem === null) return <>{children}</>
  return (
    <div
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="startup-problem-title"
      aria-describedby="startup-problem-line"
      className="fixed inset-0 z-50 flex flex-col items-center justify-center gap-4 bg-canvas px-4"
    >
      <img className="h-16 w-auto" src="/favicon.png" alt="" />
      <h1 id="startup-problem-title" className="m-0 text-base font-medium text-strong">
        {STARTUP_PROBLEM_TITLE}
      </h1>
      <p id="startup-problem-line" className="m-0 max-w-[480px] text-center text-[13px] text-body">
        {displaySafe(problem)}
      </p>
      <Button
        disabled={quitting}
        onClick={() => {
          setQuitting(true)
          quitApp().catch(() => setQuitting(false))
        }}
      >
        Quit
      </Button>
    </div>
  )
}
