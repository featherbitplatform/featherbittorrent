/**
 * The button: one shape, two weights.
 *
 * 40px tall and radius 6 are the plan's control measures, spelled `min-h-10` and `rounded-control`.
 * `primary` is the violet fill the plan names for the action a person is meant to take; `secondary`
 * is the raised, ruled outline the Sign in prototype draws its lesser button with. `type` is
 * `button` unless the caller says otherwise, so a button inside a form cannot submit it by accident.
 *
 * @module components/Button
 */
import type { ButtonHTMLAttributes } from 'react'

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  /** The violet fill for the thing to do, the raised outline for everything else. */
  variant?: 'primary' | 'secondary'
}

const VARIANTS: Record<'primary' | 'secondary', string> = {
  primary: 'border border-transparent bg-accent text-strong hover:bg-accent-hover',
  secondary: 'border border-rule bg-raised text-strong hover:bg-ink-700',
}

export default function Button({
  variant = 'primary',
  className = '',
  type = 'button',
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={
        'inline-flex min-h-10 cursor-pointer items-center justify-center gap-2 rounded-control px-3 ' +
        'font-medium transition-colors disabled:cursor-default disabled:opacity-55 ' +
        `${VARIANTS[variant]} ${className}`
      }
      {...rest}
    />
  )
}
