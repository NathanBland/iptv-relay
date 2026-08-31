import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Braces, CalendarClock, Radio, ScanLine, Sparkles, Trash2, X } from 'lucide-react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { CreateEventTemplateInput, EventChannel, EventTemplate, EventTemplateSuggestion, IptvApiClient } from '@/lib/api/types'

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
  onDelete,
  confirmDelete,
  deletePending,
  onConfirmDelete,
  onCancelDelete,
}: {
  template: EventTemplate
  channels: EventChannel[]
  onScan: () => void
  scanning: boolean
  onDelete: () => void
  confirmDelete: boolean
  deletePending: boolean
  onConfirmDelete: () => void
  onCancelDelete: () => void
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
        <div className="flex items-center gap-1">
          <Button variant="secondary" size="sm" onClick={onScan} disabled={scanning}>
            <ScanLine aria-hidden="true" className="size-4" />
            {scanning ? 'Scanning' : 'Scan'}
          </Button>
          {confirmDelete ? (
            <span className="inline-flex items-center gap-1">
              <Button variant="ghost" size="sm" className="text-red-300 hover:text-red-200" disabled={deletePending} onClick={onConfirmDelete}>
                {deletePending ? 'Wait…' : 'Yes'}
              </Button>
              <Button variant="ghost" size="sm" onClick={onCancelDelete}>No</Button>
            </span>
          ) : (
            <Button variant="ghost" size="sm" className="text-slate-400 hover:text-red-300" aria-label={`Delete ${template.displayName}`} onClick={onDelete}>
              <Trash2 aria-hidden="true" className="size-4" />
            </Button>
          )}
        </div>
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

function SuggestionCard({
  suggestion,
  pending,
  onCreate,
}: {
  suggestion: EventTemplateSuggestion
  pending: boolean
  onCreate: () => void
}) {
  return (
    <Card>
      <CardHeader>
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="font-semibold text-white">{suggestion.displayName}</h2>
            <Badge tone="info">{suggestion.streamCount} streams</Badge>
          </div>
          <p className="mt-1 text-xs text-slate-500">
            {suggestion.groupName} · {suggestion.eventDurationHours}h duration
          </p>
        </div>
        <Button variant="secondary" size="sm" disabled={pending} onClick={onCreate}>
          {pending ? 'Creating…' : 'Create template'}
        </Button>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid gap-3 sm:grid-cols-2">
          <div>
            <p className="mb-1 text-xs font-medium text-slate-300">Match pattern</p>
            <code className="block break-all rounded-lg bg-ink-950 p-3 text-xs leading-5 text-mint-400">
              {suggestion.matchRegex}
            </code>
          </div>
          <div>
            <p className="mb-1 text-xs font-medium text-slate-300">Channel name format</p>
            <code className="block break-all rounded-lg bg-ink-950 p-3 text-xs leading-5 text-cyan-300">
              {suggestion.channelNameFormat}
            </code>
          </div>
        </div>
        <div className="grid gap-2 text-xs text-slate-400 sm:grid-cols-2">
          <span>Group: {suggestion.groupName}</span>
          <span>Duration: {suggestion.eventDurationHours}h</span>
          <span>Past grace: {suggestion.pastDateGraceHours}h</span>
          <span>Future window: {suggestion.futureDateDays}d</span>
        </div>
        <div>
          <p className="mb-1 text-xs font-medium text-slate-300">Sample streams</p>
          {suggestion.sampleStreams.length === 0 ? (
            <p className="text-xs text-slate-500">No sample streams.</p>
          ) : (
            <ul className="space-y-2">
              {suggestion.sampleStreams.slice(0, 5).map((stream, index) => (
                <li
                  key={index}
                  className="rounded-lg border border-white/8 bg-white/3 px-4 py-3 text-xs text-slate-300"
                >
                  {stream}
                </li>
              ))}
            </ul>
          )}
        </div>
      </CardContent>
    </Card>
  )
}

export function EventsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const [scanningId, setScanningId] = useState<string | null>(null)
  const [showTemplateForm, setShowTemplateForm] = useState(false)
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null)
  const [showSuggestions, setShowSuggestions] = useState(false)
  const [suggestions, setSuggestions] = useState<EventTemplateSuggestion[] | null>(null)
  const [creatingName, setCreatingName] = useState<string | null>(null)

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

  const createMutation = useMutation({
    mutationFn: (input: CreateEventTemplateInput) => client.createEventTemplate(input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['event-templates'] })
      setShowTemplateForm(false)
    },
  })

  const deleteMutation = useMutation({
    mutationFn: (id: string) => client.deleteEventTemplate(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['event-templates'] })
      void queryClient.invalidateQueries({ queryKey: ['event-channels'] })
      setConfirmDelete(null)
    },
  })

  const suggestMutation = useMutation({
    mutationFn: () => client.suggestEventTemplates(),
    onSuccess: (data) => {
      setSuggestions(data)
    },
    onError: () => {
      setSuggestions(null)
    },
  })

  const handleCreateSuggestion = (suggestion: EventTemplateSuggestion) => {
    setCreatingName(suggestion.name)
    const input: CreateEventTemplateInput = {
      name: suggestion.name,
      displayName: suggestion.displayName,
      matchRegex: suggestion.matchRegex,
      channelNameFormat: suggestion.channelNameFormat,
      groupName: suggestion.groupName,
      eventDurationHours: suggestion.eventDurationHours,
      pastDateGraceHours: suggestion.pastDateGraceHours,
      futureDateDays: suggestion.futureDateDays,
    }
    createMutation.mutate(input, {
      onSuccess: () => {
        setSuggestions((prev) => prev?.filter((s) => s.name !== suggestion.name) ?? null)
      },
      onSettled: () => {
        setCreatingName(null)
      },
    })
  }

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
          <>
            <Button
              variant="secondary"
              onClick={() => {
                setShowSuggestions(true)
                suggestMutation.mutate()
              }}
              disabled={suggestMutation.isPending}
            >
              <Sparkles aria-hidden="true" className="size-4" />
              {suggestMutation.isPending ? 'Analyzing…' : 'Analyze streams'}
            </Button>
            <Button variant="secondary" onClick={() => setShowTemplateForm(true)}>
              <Braces aria-hidden="true" className="size-4" /> Configure templates
            </Button>
          </>
        }
      />
      {showTemplateForm ? (
        <EventTemplateForm
          pending={createMutation.isPending}
          error={createMutation.error instanceof Error ? createMutation.error.message : null}
          onSubmit={(input) => createMutation.mutate(input)}
          onCancel={() => setShowTemplateForm(false)}
        />
      ) : null}
      {showSuggestions ? (
        <section className="mb-6 space-y-4">
          <div className="flex items-center justify-between">
            <h2 className="font-semibold text-white">Suggested templates</h2>
            <Button variant="ghost" size="sm" onClick={() => setShowSuggestions(false)}>
              Close
            </Button>
          </div>
          {suggestMutation.isPending ? (
            <Card>
              <CardContent className="py-8 text-center text-sm text-slate-500">
                <span role="status">Analyzing streams…</span>
              </CardContent>
            </Card>
          ) : suggestMutation.isError ? (
            <p role="alert" className="text-sm text-red-300">
              {suggestMutation.error instanceof Error ? suggestMutation.error.message : 'Stream analysis failed.'}
            </p>
          ) : !suggestions || suggestions.length === 0 ? (
            <p className="text-sm text-slate-500">No template suggestions found.</p>
          ) : (
            <div className="space-y-4">
              {suggestions.map((suggestion) => (
                <SuggestionCard
                  key={suggestion.name}
                  suggestion={suggestion}
                  pending={creatingName === suggestion.name}
                  onCreate={() => handleCreateSuggestion(suggestion)}
                />
              ))}
            </div>
          )}
        </section>
      ) : null}
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
              onDelete={() => setConfirmDelete(template.id)}
              confirmDelete={confirmDelete === template.id}
              deletePending={deleteMutation.isPending}
              onConfirmDelete={() => deleteMutation.mutate(template.id)}
              onCancelDelete={() => setConfirmDelete(null)}
            />
          ))
        )}
      </section>
    </>
  )
}

function EventTemplateForm({
  pending,
  error,
  onSubmit,
  onCancel,
}: {
  pending: boolean
  error: string | null
  onSubmit: (input: CreateEventTemplateInput) => void
  onCancel: () => void
}) {
  const [name, setName] = useState('')
  const [displayName, setDisplayName] = useState('')
  const [matchRegex, setMatchRegex] = useState('')
  const [channelNameFormat, setChannelNameFormat] = useState('{event}')
  const [groupName, setGroupName] = useState('Sports')

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    onSubmit({ name, displayName, matchRegex, channelNameFormat, groupName })
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <h2 className="font-semibold text-white">New event template</h2>
          <Button variant="ghost" size="sm" onClick={onCancel}><X aria-hidden="true" className="size-4" /></Button>
        </div>
      </CardHeader>
      <CardContent>
        <form onSubmit={handleSubmit} className="space-y-4">
          <label className="block text-xs font-medium text-slate-300">
            Name
            <Input className="mt-1" value={name} onChange={(e) => setName(e.target.value)} placeholder="nba" required />
          </label>
          <label className="block text-xs font-medium text-slate-300">
            Display name
            <Input className="mt-1" value={displayName} onChange={(e) => setDisplayName(e.target.value)} placeholder="NBA Games" required />
          </label>
          <label className="block text-xs font-medium text-slate-300">
            Match regex
            <Input className="mt-1 font-mono text-xs" value={matchRegex} onChange={(e) => setMatchRegex(e.target.value)} placeholder="NBA.*vs.*" required />
          </label>
          <label className="block text-xs font-medium text-slate-300">
            Channel name format
            <Input className="mt-1 font-mono text-xs" value={channelNameFormat} onChange={(e) => setChannelNameFormat(e.target.value)} placeholder="{event}" required />
          </label>
          <label className="block text-xs font-medium text-slate-300">
            Group name
            <Input className="mt-1" value={groupName} onChange={(e) => setGroupName(e.target.value)} placeholder="Sports" required />
          </label>
          {error ? <p role="alert" className="text-sm text-red-300">{error}</p> : null}
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={onCancel}>Cancel</Button>
            <Button type="submit" size="sm" disabled={pending}>{pending ? 'Saving…' : 'Create template'}</Button>
          </div>
        </form>
      </CardContent>
    </Card>
  )
}
