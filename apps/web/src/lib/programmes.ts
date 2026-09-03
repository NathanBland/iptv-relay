import type { Programme } from '@/lib/api/types'

export function formatProgrammeTime(iso: string, timezone: string, locale = 'en-US') {
  const instant = new Date(iso)
  if (Number.isNaN(instant.getTime())) return 'Invalid time'
  try {
    return new Intl.DateTimeFormat(locale, {
      hour: 'numeric',
      minute: '2-digit',
      timeZone: timezone,
    }).format(instant)
  } catch {
    return new Intl.DateTimeFormat(locale, { hour: 'numeric', minute: '2-digit' }).format(instant)
  }
}

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
