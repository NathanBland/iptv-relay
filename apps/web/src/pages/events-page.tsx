import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Braces, CalendarClock, Eye, Power, Radio, ScanLine, Sparkles, Trash2, X } from 'lucide-react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { CreateEventTemplateInput, EventChannel, EventTemplate, EventTemplateSuggestion, IptvApiClient, UpdateEventTemplateInput } from '@/lib/api/types'

const channelTones: Record<EventChannel['state'], 'success' | 'info' | 'warning' | 'neutral'> = {
  live: 'success',
  scheduled: 'info',
  ended: 'warning',
  hidden: 'neutral',
}

function formatTimestamp(value: string | null): string {
  if (!value) return 'Not scheduled'
  // Render in the browser timezone. Omitting timeZone lets the runtime use
  // the viewer's local timezone instead of a hardcoded region.
  return new Date(value).toLocaleString('en-US', {
    dateStyle: 'medium',
    timeStyle: 'short',
  })
}

export interface RulePreviewMatch {
  sample: string
  matched: boolean
  channelName: string | null
  groups: Record<string, string>
  error: string | null
}

/**
 * Apply an event template regex and channel name format to sample stream
 * names. The evaluation runs client-side against real stream names supplied
 * by the caller (existing event channel raw stream names or operator-entered
 * samples). No data is fabricated.
 */
export function previewEventRule(
  regexSource: string,
  channelNameFormat: string,
  samples: string[],
): RulePreviewMatch[] {
  if (!regexSource.trim()) return []
  let pattern: RegExp
  try {
    pattern = new RegExp(regexSource)
  } catch {
    return samples.map((sample) => ({
      sample,
      matched: false,
      channelName: null,
      groups: {},
      error: 'The match pattern is not valid regex.',
    }))
  }
  return samples.map((sample) => {
    if (!sample) return { sample, matched: false, channelName: null, groups: {}, error: null }
    const match = pattern.exec(sample)
    if (!match) return { sample, matched: false, channelName: null, groups: {}, error: null }
    const groups = match.groups ?? {}
    const channelName = channelNameFormat.replace(/\{(\w+)\}/g, (_whole, key: string) => groups[key] ?? '')
    return { sample, matched: true, channelName, groups, error: null }
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
  onToggleEnabled,
  togglingEnabled,
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
  onToggleEnabled: () => void
  togglingEnabled: boolean
}) {
  const [showPreview, setShowPreview] = useState(false)
  const previewSamples = useMemo(
    () => channels.map((channel) => channel.rawStreamName).filter((name): name is string => Boolean(name)),
    [channels],
  )
  const previewMatches = useMemo(
    () => (showPreview ? previewEventRule(template.matchRegex, template.channelNameFormat, previewSamples) : []),
    [showPreview, template.matchRegex, template.channelNameFormat, previewSamples],
  )
  const matchedCount = previewMatches.filter((match) => match.matched).length

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
          <Button
            variant="secondary"
            size="sm"
            onClick={onToggleEnabled}
            disabled={togglingEnabled}
            aria-label={`${template.enabled ? 'Disable' : 'Enable'} ${template.displayName}`}
            aria-pressed={template.enabled}
          >
            <Power aria-hidden="true" className="size-4" />
            {togglingEnabled ? 'Wait…' : template.enabled ? 'Disable' : 'Enable'}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => setShowPreview((value) => !value)}
            aria-expanded={showPreview}
            aria-controls={`event-rule-preview-${template.id}`}
            aria-label={`Preview rule for ${template.displayName}`}
          >
            <Eye aria-hidden="true" className="size-4" />
            {showPreview ? 'Hide preview' : 'Preview'}
          </Button>
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
        {showPreview ? (
          <EventRulePreviewPanel
            id={`event-rule-preview-${template.id}`}
            matches={previewMatches}
            emptyMessage={previewSamples.length === 0
              ? 'No scanned stream names are available for this template. Scan provider streams to populate the preview.'
              : 'The match pattern did not match any scanned stream names.'}
            summary={previewSamples.length > 0
              ? `${matchedCount} of ${previewSamples.length} scanned stream names match.`
              : null}
          />
        ) : null}
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

function EventRulePreviewPanel({
  id,
  matches,
  emptyMessage,
  summary,
}: {
  id: string
  matches: RulePreviewMatch[]
  emptyMessage: string
  summary: string | null
}) {
  return (
    <section id={id} aria-label="Event rule preview" className="rounded-lg border border-white/8 bg-ink-950/50 p-4">
      <p className="text-xs font-medium text-slate-300">Rule preview</p>
      <p className="mt-1 text-[0.68rem] text-slate-500">
        The preview applies the match pattern and channel name format to real scanned stream names. No data is fabricated.
      </p>
      {summary ? <p role="status" className="mt-2 text-xs text-slate-300">{summary}</p> : null}
      {matches.every((match) => !match.matched && !match.error) ? (
        <p className="mt-2 text-xs text-slate-500">{emptyMessage}</p>
      ) : (
        <ul className="mt-3 space-y-1.5">
          {matches.map((match, index) => (
            <li
              key={`${match.sample}-${index}`}
              className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-white/5 bg-white/3 px-3 py-2 text-xs"
            >
              <div className="min-w-0">
                <p className="truncate font-mono text-slate-300">{match.sample || '(empty)'}</p>
                {match.error ? (
                  <p role="alert" className="mt-0.5 text-red-300">{match.error}</p>
                ) : match.matched ? (
                  <p className="mt-0.5 text-cyan-300">
                    <span className="text-slate-500">channel: </span>
                    {match.channelName || '(empty format)'}
                  </p>
                ) : (
                  <p className="mt-0.5 text-slate-500">No match</p>
                )}
              </div>
              <Badge tone={match.matched ? 'success' : match.error ? 'danger' : 'neutral'}>
                {match.matched ? 'Match' : match.error ? 'Error' : 'Skip'}
              </Badge>
            </li>
          ))}
        </ul>
      )}
    </section>
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
  const [togglingId, setTogglingId] = useState<string | null>(null)

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

  const toggleEnabledMutation = useMutation({
    mutationFn: ({ template, enabled }: { template: EventTemplate; enabled: boolean }) =>
      client.updateEventTemplate(template.id, { enabled } satisfies UpdateEventTemplateInput),
    onMutate: ({ template }) => setTogglingId(template.id),
    onSettled: () => setTogglingId(null),
    onSuccess: (_data, { template }) => {
      void queryClient.invalidateQueries({ queryKey: ['event-templates'] })
      void queryClient.invalidateQueries({ queryKey: ['event-channels'] })
      void queryClient.invalidateQueries({ queryKey: ['events'] })
      void queryClient.invalidateQueries({ queryKey: ['event-templates', template.id] })
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
              onToggleEnabled={() =>
                toggleEnabledMutation.mutate({ template, enabled: !template.enabled })
              }
              togglingEnabled={togglingId === template.id || toggleEnabledMutation.isPending}
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
  const [eventDurationHours, setEventDurationHours] = useState('3')
  const [pastDateGraceHours, setPastDateGraceHours] = useState('4')
  const [futureDateDays, setFutureDateDays] = useState('2')
  const [previewSample, setPreviewSample] = useState('')

  const previewMatches = useMemo(
    () => (previewSample.trim() ? previewEventRule(matchRegex, channelNameFormat, [previewSample]) : []),
    [matchRegex, channelNameFormat, previewSample],
  )
  const previewResult = previewMatches[0]

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    onSubmit({
      name,
      displayName,
      matchRegex,
      channelNameFormat,
      groupName,
      eventDurationHours: Number(eventDurationHours),
      pastDateGraceHours: Number(pastDateGraceHours),
      futureDateDays: Number(futureDateDays),
    })
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
          <div className="grid gap-3 sm:grid-cols-3">
            <label className="block text-xs font-medium text-slate-300">
              Duration (hours)
              <Input
                className="mt-1"
                type="number"
                min={1}
                step={1}
                value={eventDurationHours}
                onChange={(e) => setEventDurationHours(e.target.value)}
                placeholder="3"
                required
              />
            </label>
            <label className="block text-xs font-medium text-slate-300">
              Past grace (hours)
              <Input
                className="mt-1"
                type="number"
                min={0}
                step={1}
                value={pastDateGraceHours}
                onChange={(e) => setPastDateGraceHours(e.target.value)}
                placeholder="4"
                required
              />
            </label>
            <label className="block text-xs font-medium text-slate-300">
              Future window (days)
              <Input
                className="mt-1"
                type="number"
                min={0}
                step={1}
                value={futureDateDays}
                onChange={(e) => setFutureDateDays(e.target.value)}
                placeholder="2"
                required
              />
            </label>
          </div>
          <div className="rounded-lg border border-white/8 bg-ink-950/50 p-4">
            <p className="text-xs font-medium text-slate-300">Rule preview</p>
            <p className="mt-1 text-[0.68rem] text-slate-500">
              Enter a real provider stream name to test the match pattern and channel name format before you save. No data is fabricated.
            </p>
            <label className="mt-3 block text-xs font-medium text-slate-300">
              <span className="sr-only">Sample stream name for preview</span>
              <Input
                className="mt-1 font-mono text-xs"
                value={previewSample}
                onChange={(e) => setPreviewSample(e.target.value)}
                placeholder="NBA 08/19 1:00 PM Broncos vs Chiefs"
                aria-label="Sample stream name for preview"
              />
            </label>
            {previewResult ? (
              <div className="mt-3 space-y-1 text-xs" role="status" aria-live="polite">
                {previewResult.error ? (
                  <p role="alert" className="text-red-300">{previewResult.error}</p>
                ) : previewResult.matched ? (
                  <p className="text-cyan-300">
                    <span className="text-slate-500">Match. Channel name: </span>
                    {previewResult.channelName || '(empty format)'}
                  </p>
                ) : (
                  <p className="text-slate-500">No match for this stream name.</p>
                )}
              </div>
            ) : null}
          </div>
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
