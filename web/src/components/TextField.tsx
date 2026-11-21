/**
 * A label and its field, wired together.
 *
 * The pair is 6px apart and the field is a control measure (40px, radius 6) sunk into `ink-900`
 * with a `ink-600` rule, the Sign in prototype's own numbers. The label gets a generated id and the
 * input gets its `htmlFor` because a field a screen reader cannot name is a field half missing.
 * The focus edge lifts to `violet-400` plus a soft ring rather than leaving the browser default,
 * so the keyboard path is visible on the canvas without spending a colour the token list does not
 * have.
 *
 * @module components/TextField
 */
import { useId } from 'react'
import type { InputHTMLAttributes } from 'react'

export interface TextFieldProps extends InputHTMLAttributes<HTMLInputElement> {
  label: string
}

export default function TextField({ label, className = '', ...rest }: TextFieldProps) {
  const id = useId()
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="text-[13px] text-body">
        {label}
      </label>
      <input
        id={id}
        className={
          'min-h-10 box-border rounded-control border border-rule bg-sunken px-3 text-strong ' +
          'outline-none focus-visible:border-accent-light focus-visible:ring-2 focus-visible:ring-accent-soft ' +
          className
        }
        {...rest}
      />
    </div>
  )
}
