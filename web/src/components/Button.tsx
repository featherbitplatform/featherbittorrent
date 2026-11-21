/**
 * The button: one shape, three weights.
 *
 * 40px tall and radius 6 are the plan's control measures, spelled `min-h-10` and `rounded-control`.
 * `primary` is the violet fill the plan names for the action a person is meant to take; `secondary`
 * is the raised, ruled outline the Sign in prototype draws its lesser button with; `danger` is that
 * same outline wearing `error`, for the action that takes something away - the outline of the lesser
 * button with the colour of the thing it does, so a dialog can put its destructive choice last,
 * where the eye goes, without turning it into the violet button. `type` is
 * `button` unless the caller says otherwise, so a button inside a form cannot submit it by accident.
 *
 * @module components/Button
 */
import type { ButtonHTMLAttributes } from 'react'

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  /** The violet fill for the thing to do, the raised outline for everything else, the same outline wearing `error` for the one that cannot be undone. */
  variant?: 'primary' | 'secondary' | 'danger'
  /** `md` is the plan's 40px control. `sm` is the 36px the Torrents prototype draws its toolbar buttons at, beside a filter field of the same height. */
  size?: 'md' | 'sm'
}

const VARIANTS: Record<'primary' | 'secondary' | 'danger', string> = {
  primary: 'border border-transparent bg-accent text-strong hover:bg-accent-hover',
  secondary: 'border border-rule bg-raised text-strong hover:bg-ink-700',
  danger: 'border border-rule bg-raised text-error hover:border-error/60 hover:bg-error/8',
}

const SIZES: Record<'md' | 'sm', string> = {
  md: 'min-h-10 px-3 font-medium',
  sm: 'min-h-9 px-3 text-[13px]',
}

export default function Button({
  variant = 'primary',
  size = 'md',
  className = '',
  type = 'button',
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={
        'inline-flex cursor-pointer items-center justify-center gap-2 rounded-control ' +
        'transition-colors disabled:cursor-default disabled:opacity-55 ' +
        `${SIZES[size]} ${VARIANTS[variant]} ${className}`
      }
      {...rest}
    />
  )
}
