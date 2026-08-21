import type { Programme } from '@/lib/api/types'

export function partitionProgrammesAt(programmes: Programme[], now: number) {
  const current: Programme[] = []
  const upcoming: Programme[] = []

  for (const programme of programmes) {
    const start = Date.parse(programme.start)
    const end = Date.parse(programme.end)
    if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start) continue
    if (start <= now && end > now) current.push(programme)
    if (start > now) upcoming.push(programme)
  }

  return { current, upcoming }
}
