/**
 * The strip of words the page puts above its own content: a warning, or a failure.
 *
 * The shape is the Sign in prototype's warning block, and the colours are its colours read back
 * through the token list: the amber is `stalled` at 45% for the rule and 8% for the fill, the red
 * is `error` the same way, and the text is the plain token. The tone chooses the colours and the
 * icon and nothing else: announcing is the job of the live region the screens wrap these in, which
 * stays mounted whether or not a strip is inside it. A `role` here would announce the whole strip as
 * it mounts, and a strip that mounts together with its text is exactly how a screen reader misses
 * being told about it.
 *
 * @module components/Banner
 */
import { CircleAlert, TriangleAlert } from 'lucide-react'
import type { ReactNode } from 'react'

export interface BannerProps {
  children: ReactNode
  /** `warning` for something to know, `danger` for something that just failed. */
  tone?: 'warning' | 'danger'
  className?: string
}

const TONES: Record<'warning' | 'danger', { box: string; Icon: typeof TriangleAlert }> = {
  warning: { box: 'border-stalled/45 bg-stalled/8 text-stalled', Icon: TriangleAlert },
  danger: { box: 'border-error/45 bg-error/8 text-error', Icon: CircleAlert },
}

export default function Banner({ children, tone = 'warning', className = '' }: BannerProps) {
  const { box, Icon } = TONES[tone]
  return (
    <div
      className={`flex gap-2.5 rounded-card border px-3.5 py-3 text-[13px] leading-[1.5] ${box} ${className}`}
    >
      <Icon className="mt-0.5 h-4 w-4 flex-none" aria-hidden="true" />
      <span>{children}</span>
    </div>
  )
}
