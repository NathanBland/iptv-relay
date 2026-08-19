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

export function FieldMessage({ children }: { children: string | undefined }) {
  return children ? <p className="mt-1 text-xs text-red-300">{children}</p> : null
}
