import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Braces, CalendarClock, Radio, ScanLine } from 'lucide-react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { EventChannel, EventTemplate, IptvApiClient } from '@/lib/api/types'

const channelTones: Record<EventChannel['state'], 'success' | 'info' | 'warning' | 'neutral'> = {
  live: 'success',
  scheduled: 'info',
  ended: 'warning',
  hidden: 'neutral',
}

function formatTimestamp(value: string | null): string {
  if (!value) return 'Not scheduled'
  return new Date(value).toLocaleString('en-US', {
    timeZone: 'America/Denver',
    dateStyle: 'medium',
    timeStyle: 'short',
  })
}

function TemplateCard({
  template,
  channels,
  onScan,
  scanning,
}: {
  template: EventTemplate
  channels: EventChannel[]
  onScan: () => void
  scanning: boolean
}) {
  return (
    <Card>
      <CardHeader>
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="font-semibold text-white">{template.displayName}</h2>
            <Badge tone={template.enabled ? 'success' : 'neutral'}>
              {template.enabled ? 'enabled' : 'disabled'}
            </Badge>
          </div>
          <p className="mt-1 text-xs text-slate-500">
            {template.groupName} · {template.eventDurationHours}h duration
          </p>
        </div>
        <Button variant="secondary" size="sm" onClick={onScan} disabled={scanning}>
          <ScanLine aria-hidden="true" className="size-4" />
          {scanning ? 'Scanning' : 'Scan'}
        </Button>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid gap-3 sm:grid-cols-2">
          <div>
            <p className="mb-1 text-xs font-medium text-slate-300">Match pattern</p>
            <code className="block break-all rounded-lg bg-ink-950 p-3 text-xs leading-5 text-mint-400">
              {template.matchRegex}
            </code>
          </div>
          <div>
            <p className="mb-1 text-xs font-medium text-slate-300">Channel name format</p>
            <code className="block break-all rounded-lg bg-ink-950 p-3 text-xs leading-5 text-cyan-300">
              {template.channelNameFormat}
            </code>
          </div>
        </div>
        <div className="grid gap-2 text-xs text-slate-400 sm:grid-cols-3">
          <span>Duration: {template.eventDurationHours}h</span>
          <span>Past grace: {template.pastDateGraceHours}h</span>
          <span>Future window: {template.futureDateDays}d</span>
        </div>
        {channels.length === 0 ? (
          <p className="rounded-lg border border-white/8 bg-white/3 px-4 py-3 text-center text-xs text-slate-500">
            No event channels found. Scan provider streams to detect events.
          </p>
        ) : (
          <ul className="space-y-2">
            {channels.map((channel) => (
              <li
                key={channel.id}
                className="flex flex-wrap items-center gap-3 rounded-lg border border-white/8 bg-white/3 px-4 py-3"
              >
                <span className="grid size-8 place-items-center rounded-lg bg-ocean-400/10 text-ocean-400">
                  {channel.state === 'live' ? (
                    <Radio aria-hidden="true" className="size-4" />
                  ) : (
                    <CalendarClock aria-hidden="true" className="size-4" />
                  )}
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="truncate font-medium text-white">
                      {channel.eventTitle ?? `Slot ${channel.slotNumber}`}
                    </span>
                    <Badge tone={channelTones[channel.state]}>{channel.state}</Badge>
                  </div>
                  <p className="mt-1 truncate text-xs text-slate-500">
                    {channel.rawStreamName ?? 'No source stream'}
                  </p>
                  <p className="mt-1 text-xs text-slate-400">
                    Slot {channel.slotNumber} · {formatTimestamp(channel.eventStart)}
                  </p>
                </div>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  )
}

export function EventsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const [scanningId, setScanningId] = useState<string | null>(null)

  const templatesQuery = useQuery({ ...apiQueries(client).eventTemplates })
  const channelsQuery = useQuery({ ...apiQueries(client).eventChannels() })

  const scanMutation = useMutation({
    mutationFn: (templateId: string) => client.scanEventTemplate(templateId),
    onMutate: (templateId) => setScanningId(templateId),
    onSettled: () => setScanningId(null),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['event-channels'] })
      void queryClient.invalidateQueries({ queryKey: ['events'] })
    },
  })

  if (!templatesQuery.data || !channelsQuery.data) {
    return <LoadingPage label="event templates" />
  }

  const templates = templatesQuery.data
  const channels = channelsQuery.data

  return (
    <>
      <PageHeader
        eyebrow="Dynamic lineup"
        title="Events"
        description="The system parses provider titles into stable event channels and EPG programme slots with per-group templates."
        actions={
          <Button variant="secondary">
            <Braces aria-hidden="true" className="size-4" /> Configure templates
          </Button>
        }
      />
      <section className="space-y-4">
        {templates.length === 0 ? (
          <Card>
            <CardContent className="py-8 text-center text-sm text-slate-500">
              No event templates configured. Create a template to start detecting events from provider streams.
            </CardContent>
          </Card>
        ) : (
          templates.map((template) => (
            <TemplateCard
              key={template.id}
              template={template}
              channels={channels.filter((channel) => channel.templateId === template.id)}
              onScan={() => scanMutation.mutate(template.id)}
              scanning={scanningId === template.id || scanMutation.isPending}
            />
          ))
        )}
      </section>
    </>
  )
}
