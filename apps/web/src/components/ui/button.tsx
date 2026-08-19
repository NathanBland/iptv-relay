import { cva, type VariantProps } from 'class-variance-authority'
import type { ButtonHTMLAttributes } from 'react'
import { cn } from '@/lib/utils'

export const buttonVariants = cva(
  'inline-flex min-h-9 items-center justify-center gap-2 rounded-lg px-3 text-sm font-semibold transition-colors disabled:pointer-events-none disabled:opacity-45',
  {
    variants: {
      variant: {
        primary: 'bg-ocean-400 text-ink-950 hover:bg-mint-400',
        secondary: 'border border-white/12 bg-white/6 text-slate-100 hover:bg-white/10',
        ghost: 'text-slate-300 hover:bg-white/7 hover:text-white',
        danger: 'bg-red-500/15 text-red-200 hover:bg-red-500/25',
      },
      size: {
        default: 'h-10',
        sm: 'h-8 min-h-8 px-2.5 text-xs',
        icon: 'size-9 min-h-9 px-0',
      },
    },
    defaultVariants: { variant: 'primary', size: 'default' },
  },
)

export interface ButtonProps
  extends ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {}

export function Button({ className, variant, size, type = 'button', ...props }: ButtonProps) {
  return <button type={type} className={cn(buttonVariants({ variant, size }), className)} {...props} />
}
