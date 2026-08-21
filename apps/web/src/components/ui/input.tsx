import { forwardRef, type InputHTMLAttributes, type SelectHTMLAttributes } from 'react'
import { cn } from '@/lib/utils'

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function Input(
  { className, ...props },
  ref,
) {
  return (
    <input
      ref={ref}
      className={cn(
        'h-10 w-full rounded-lg border border-white/12 bg-ink-950/70 px-3 text-sm text-white placeholder:text-slate-500 disabled:opacity-50',
        className,
      )}
      {...props}
    />
  )
})

export const Select = forwardRef<HTMLSelectElement, SelectHTMLAttributes<HTMLSelectElement>>(function Select(
  { className, ...props },
  ref,
) {
  return (
    <select
      ref={ref}
      className={cn('h-10 w-full rounded-lg border border-white/12 bg-ink-950/70 px-3 text-sm text-white', className)}
      {...props}
    />
  )
})

function errorMessage(error: unknown): string | undefined {
  if (typeof error === 'string') return error
  if (Array.isArray(error)) return error.map(errorMessage).find((message) => message !== undefined)
  if (error && typeof error === 'object' && 'message' in error && typeof error.message === 'string') {
    return error.message
  }
  return undefined
}

export function FieldMessage({ children }: { children: unknown }) {
  const message = errorMessage(children)
  return message ? <p className="mt-1 text-xs text-red-300">{message}</p> : null
}
