import { cva, type VariantProps } from 'class-variance-authority'
import type { HTMLAttributes } from 'react'
import { cn } from '@/lib/utils'

const badgeVariants = cva(
  'inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-[0.7rem] font-semibold tracking-wide',
  {
    variants: {
      tone: {
        neutral: 'border-slate-500/25 bg-slate-500/10 text-slate-300',
        success: 'border-emerald-400/25 bg-emerald-400/10 text-emerald-300',
        warning: 'border-amber-400/25 bg-amber-400/10 text-amber-200',
        danger: 'border-red-400/25 bg-red-400/10 text-red-200',
        info: 'border-cyan-400/25 bg-cyan-400/10 text-cyan-200',
      },
    },
    defaultVariants: { tone: 'neutral' },
  },
)

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement>, VariantProps<typeof badgeVariants> {}

export function Badge({ className, tone, ...props }: BadgeProps) {
  return <span className={cn(badgeVariants({ tone }), className)} {...props} />
}
