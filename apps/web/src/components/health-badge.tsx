import { Circle } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import type { HealthState } from '@/lib/api/types'

const tones = {
  healthy: 'success',
  degraded: 'warning',
  offline: 'danger',
  syncing: 'info',
} as const

export function HealthBadge({ state }: { state: HealthState }) {
  return (
    <Badge tone={tones[state]}>
      <Circle aria-hidden="true" className="size-1.5 fill-current" />
      {state}
    </Badge>
  )
}
