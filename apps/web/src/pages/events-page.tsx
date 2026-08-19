import { useQuery } from '@tanstack/react-query'
import { Braces, CalendarClock, ChevronRight } from 'lucide-react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { DynamicEvent, IptvApiClient } from '@/lib/api/types'

const eventTones: Record<DynamicEvent['state'], 'success' | 'info' | 'warning' | 'neutral'> = {
  live: 'success',
  scheduled: 'info',
  ambiguous: 'warning',
  unmatched: 'neutral',
}

export function EventsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const query = useQuery({ ...apiQueries(client).events })
  if (!query.data) return <LoadingPage label="dynamic events" />
  return (
    <>
      <PageHeader eyebrow="Dynamic lineup" title="Events" description="The system parses provider titles into stable event channels and EPG programme slots with per-group templates." actions={<Button variant="secondary"><Braces aria-hidden="true" className="size-4" /> Configure templates</Button>} />
      <section className="grid gap-4 xl:grid-cols-[1fr_21rem]">
        <div className="space-y-3">
          {query.data.length === 0
            ? <Card><CardContent className="py-8 text-center text-sm text-slate-500">No dynamic events configured.</CardContent></Card>
            : query.data.map((event) => (
            <Card key={event.id}>
              <CardContent className="grid gap-4 sm:grid-cols-[auto_1fr_auto] sm:items-center">
                <span className="grid size-10 place-items-center rounded-xl bg-ocean-400/10 text-ocean-400"><CalendarClock aria-hidden="true" className="size-5" /></span>
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2"><h2 className="font-semibold text-white">{event.programmeTitle}</h2><Badge tone={eventTones[event.state]}>{event.state}</Badge></div>
                  <p className="mt-1 truncate font-mono text-xs text-slate-500">{event.rawTitle}</p>
                  <p className="mt-2 text-xs text-slate-400">{event.channelSlot} · {new Date(event.start).toLocaleString('en-US', { timeZone: 'America/Denver', dateStyle: 'medium', timeStyle: 'short' })}</p>
                </div>
                <Button variant="ghost" size="icon" aria-label={`Review ${event.programmeTitle}`}><ChevronRight aria-hidden="true" className="size-4" /></Button>
              </CardContent>
            </Card>
          ))}
        </div>
        <Card className="self-start">
          <CardHeader><div><h2 className="font-semibold text-white">NFL template</h2><p className="mt-1 text-xs text-slate-500">America/Denver · 3h 30m</p></div><Badge tone="success">active</Badge></CardHeader>
          <CardContent className="space-y-4 text-xs">
            <div><p className="mb-1 font-medium text-slate-300">Match pattern</p><code className="block rounded-lg bg-ink-950 p-3 leading-5 text-mint-400">{'{date} {time} {away} vs {home}'}</code></div>
            <div><p className="mb-1 font-medium text-slate-300">Channel output</p><code className="block rounded-lg bg-ink-950 p-3 leading-5 text-cyan-300">{'{league} Event {slot:02}'}</code></div>
            <p className="leading-5 text-slate-500">The system quarantines unparseable or DST-ambiguous titles. The system does not use guesswork to schedule them.</p>
          </CardContent>
        </Card>
      </section>
    </>
  )
}
