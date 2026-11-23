/**
 * What the window says when This computer did not start: the shell's line, over the whole window, and a
 * Quit button. The shell keeps the problem instead of showing a native message from inside its event
 * loop, and shows the window so this is seen; Quit exits with the code the problem carries (3 when
 * another featherbittorrent holds the library, 1 otherwise).
 *
 * Mounted beside the screens, outside anything keyed by instance, so it covers whatever they show.
 *
 * @module desktop/StartupProblem
 */
import { useEffect, useState } from 'react'
import Button from '../components/Button'
import { displaySafe } from '../format'
import { quitApp, startupProblem } from './bridge'

/** The heading over the shell's line. */
export const STARTUP_PROBLEM_TITLE = 'featherbittorrent could not start'

export function StartupProblemHost() {
  const [problem, setProblem] = useState<string | null>(null)
  const [quitting, setQuitting] = useState(false)

  useEffect(() => {
    let cancelled = false
    startupProblem()
      .then((answer) => {
        if (!cancelled && answer !== null) setProblem(answer.message)
      })
      .catch((cause: unknown) => {
        console.warn('Whether This computer started could not be asked:', cause)
      })
    return () => {
      cancelled = true
    }
  }, [])

  if (problem === null) return null
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
