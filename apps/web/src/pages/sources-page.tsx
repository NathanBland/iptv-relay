import { useForm } from '@tanstack/react-form'
import { useMutation, useQueries, useQuery, useQueryClient } from '@tanstack/react-query'
import { Clock, Pencil, Plus, RefreshCw, Trash2, X } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { HealthBadge } from '@/components/health-badge'
import { LoadingPage } from '@/components/loading-page'
import { PageHeader } from '@/components/page-header'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { FieldMessage, Input, Select } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { IptvApiClient, Job, Source, SourceInput, SourceKind, SourceSyncStatus, SourceUpdateInput } from '@/lib/api/types'
import { formatRelativeTime } from '@/lib/utils'
import { sourceSchema, sourceUpdateSchema } from '@/lib/validation'

const sourceKinds: SourceKind[] = ['M3U', 'Xtream', 'XMLTV', 'Network tuner']

function formatInterval(seconds: number): string {
  if (seconds === 0) return 'Manual'
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`
  return `${Math.floor(seconds / 86400)}d`
}

function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`
  if (value < 1024 * 1024 * 1024) return `${(value / 1024 / 1024).toFixed(2)} MB`
  return `${(value / 1024 / 1024 / 1024).toFixed(2)} GB`
}

function formatElapsed(startedAt: string): string {
  const seconds = Math.floor((Date.now() - new Date(startedAt).getTime()) / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  const remainingSeconds = seconds % 60
  if (minutes < 60) return `${minutes}m ${remainingSeconds}s`
  const hours = Math.floor(minutes / 60)
  const remainingMinutes = minutes % 60
  return `${hours}h ${remainingMinutes}m`
}

const stageLabel: Record<SourceSyncStatus['stage'], string> = {
  downloading: 'Download',
  parsing: 'Parse',
  activating: 'Activate',
  reconciling: 'Reconcile',
  completed: 'Complete',
}

export function SourcesPage({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const sourcesQuery = useQuery({ ...apiQueries(client).sources })
  const [showForm, setShowForm] = useState(false)
  const [notice, setNotice] = useState('')
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null)

  const sourceIds = sourcesQuery.data?.map((s) => s.id) ?? []
  const syncStatusQueries = useQueries({
    queries: sourceIds.map((id) => ({
      ...apiQueries(client).sourceSyncStatus(id),
      retry: false,
    })),
    combine: (results) => {
      const activeIds = new Set<string>()
      const statusById = new Map<string, SourceSyncStatus>()
      for (let i = 0; i < results.length; i++) {
        const data = results[i]?.data
        const id = sourceIds[i]
        if (data && id) {
          statusById.set(id, data)
          if (data.status === 'running' || data.status === 'queued') {
            activeIds.add(id)
          }
        }
      }
      return { activeSyncIds: activeIds, statusById }
    },
  })
  const syncingSourceIds = syncStatusQueries.activeSyncIds
  const syncStatusById = syncStatusQueries.statusById
  const hasDegradedSource = sourcesQuery.data?.some((source) => source.state === 'degraded') ?? false
  const jobsQuery = useQuery({ ...apiQueries(client).jobs, enabled: hasDegradedSource })

  const removeSyncing = useCallback((id: string) => {
    queryClient.removeQueries({ queryKey: ['source-sync-status', id] })
  }, [queryClient])
  const mutation = useMutation({
    mutationFn: (input: SourceInput) => client.createSource(input),
    onSuccess: (source) => {
      queryClient.setQueryData<Source[]>(['sources'], (current = []) => [...current, source])
      queryClient.invalidateQueries({ queryKey: ['source-sync-status', source.id] })
      setNotice(`${source.name} was added and its first sync has started.`)
      setShowForm(false)
    },
  })

  const deleteMutation = useMutation({
    mutationFn: (id: string) => client.deleteSource(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sources'] })
      queryClient.invalidateQueries({ queryKey: ['overview'] })
      queryClient.invalidateQueries({ queryKey: ['channels'] })
      queryClient.invalidateQueries({ queryKey: ['groups'] })
      setNotice('Source removed and related data cleaned up.')
      setConfirmDelete(null)
    },
    onError: (error: unknown) => {
      const message = error instanceof Error ? error.message : 'Failed to remove source.'
      setNotice(message)
      setConfirmDelete(null)
    },
  })

  const refreshIntervalMutation = useMutation({
    mutationFn: ({ id, seconds }: { id: string; seconds: number }) =>
      client.setSourceRefreshInterval(id, seconds),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sources'] })
    },
  })

  const [editingInterval, setEditingInterval] = useState<string | null>(null)
  const [intervalValue, setIntervalValue] = useState('')
  const [editingSource, setEditingSource] = useState<Source | null>(null)

  const updateSourceMutation = useMutation({
    mutationFn: ({ id, input }: { id: string; input: SourceUpdateInput }) =>
      client.updateSource(id, input),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sources'] })
      setEditingSource(null)
    },
  })

  const syncMutation = useMutation({
    mutationFn: (id: string) => client.triggerSourceSync(id),
    onSuccess: (data, id) => {
      queryClient.invalidateQueries({ queryKey: ['sources'] })
      queryClient.invalidateQueries({ queryKey: ['overview'] })
      queryClient.invalidateQueries({ queryKey: ['jobs'] })
      queryClient.refetchQueries({ queryKey: ['source-sync-status', id] })
      setNotice(data.message)
    },
    onError: (error: unknown) => {
      const message = error instanceof Error ? error.message : 'Failed to sync source.'
      setNotice(message)
    },
  })

  const form = useForm({
    defaultValues: {
      name: '',
      kind: 'M3U' as SourceKind,
      endpoint: '',
      serverUrl: '',
      username: '',
      password: '',
      timezone: 'UTC',
    },
    validators: { onSubmit: sourceSchema },
    onSubmit: async ({ value }) => {
      await mutation.mutateAsync(value)
      form.reset()
    },
  })

  if (!sourcesQuery.data) return <LoadingPage label="sources" />

  return (
    <>
      <PageHeader
        eyebrow="Ingest"
        title="Sources"
        description="Manage provider playlists, guide feeds, Xtream accounts, and network tuners. Credentials remain server-side."
        actions={
          <Button onClick={() => setShowForm((value) => !value)} aria-expanded={showForm}>
            <Plus aria-hidden="true" className="size-4" /> Add source
          </Button>
        }
      />

      {notice ? <p role="status" className="mb-4 rounded-lg border border-emerald-400/20 bg-emerald-400/8 p-3 text-sm text-emerald-200">{notice}</p> : null}

      {showForm ? (
        <Card className="mb-4">
          <CardHeader><h2 className="font-semibold text-white">Connect a source</h2></CardHeader>
          <CardContent>
            <form
              className="grid gap-4 lg:grid-cols-4 lg:items-start"
              onSubmit={(event) => { event.preventDefault(); event.stopPropagation(); void form.handleSubmit() }}
            >
              <form.Field
                name="name"
                validators={{ onChange: sourceSchema.shape.name }}
              >
                {(field) => (
                  <label className="text-xs font-medium text-slate-300">
                    Display name
                    <Input
                      className="mt-1"
                      name={field.name}
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) => field.handleChange(event.target.value)}
                      aria-invalid={field.state.meta.errors.length > 0}
                    />
                    <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                  </label>
                )}
              </form.Field>
              <form.Field name="kind">
                {(field) => (
                  <label className="text-xs font-medium text-slate-300">
                    Source type
                    <Select className="mt-1" value={field.state.value} onChange={(event) => field.handleChange(event.target.value as SourceKind)}>
                      {sourceKinds.map((kind) => <option key={kind}>{kind}</option>)}
                    </Select>
                  </label>
                )}
              </form.Field>
              <form.Subscribe selector={(state) => state.values.kind}>
                {(kind) => kind === 'Xtream' ? (
                  <>
                    <form.Field name="serverUrl" validators={{ onChange: sourceSchema.shape.serverUrl }}>
                      {(field) => (
                        <label className="text-xs font-medium text-slate-300">
                          Server URL
                          <Input
                            className="mt-1"
                            type="url"
                            placeholder="https://provider.example:8080"
                            value={field.state.value}
                            onBlur={field.handleBlur}
                            onChange={(event) => field.handleChange(event.target.value)}
                            aria-invalid={field.state.meta.errors.length > 0}
                          />
                          <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                          <span className="mt-1 block text-[0.68rem] text-slate-500">
                            The API adds player_api.php and your credentials.
                          </span>
                        </label>
                      )}
                    </form.Field>
                    <form.Field name="username" validators={{ onChange: sourceSchema.shape.username }}>
                      {(field) => (
                        <label className="text-xs font-medium text-slate-300">
                          Username
                          <Input
                            className="mt-1"
                            autoComplete="off"
                            value={field.state.value}
                            onBlur={field.handleBlur}
                            onChange={(event) => field.handleChange(event.target.value)}
                            aria-invalid={field.state.meta.errors.length > 0}
                          />
                          <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                        </label>
                      )}
                    </form.Field>
                    <form.Field name="password" validators={{ onChange: sourceSchema.shape.password }}>
                      {(field) => (
                        <label className="text-xs font-medium text-slate-300">
                          Password
                          <Input
                            className="mt-1"
                            type="password"
                            autoComplete="new-password"
                            value={field.state.value}
                            onBlur={field.handleBlur}
                            onChange={(event) => field.handleChange(event.target.value)}
                            aria-invalid={field.state.meta.errors.length > 0}
                          />
                          <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                        </label>
                      )}
                    </form.Field>
                    <details className="lg:col-span-4">
                      <summary className="cursor-pointer text-xs font-medium text-slate-300">
                        Advanced: use a complete player_api.php endpoint
                      </summary>
                      <form.Field name="endpoint" validators={{ onChange: sourceSchema.shape.endpoint }}>
                        {(field) => (
                          <label className="mt-3 block text-xs font-medium text-slate-300">
                            Advanced endpoint
                            <Input
                              className="mt-1"
                              type="url"
                              placeholder="https://provider.example/player_api.php?username=…&password=…"
                              value={field.state.value}
                              onBlur={field.handleBlur}
                              onChange={(event) => field.handleChange(event.target.value)}
                              aria-invalid={field.state.meta.errors.length > 0}
                            />
                            <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                            <span className="mt-1 block text-[0.68rem] text-slate-500">
                              Use this field only when the provider needs custom endpoint parameters.
                            </span>
                          </label>
                        )}
                      </form.Field>
                    </details>
                  </>
                ) : (
                  <form.Field name="endpoint" validators={{ onChange: sourceSchema.shape.endpoint }}>
                    {(field) => (
                      <label className="text-xs font-medium text-slate-300 lg:col-span-2">
                        Endpoint
                        <Input
                          className="mt-1"
                          type="url"
                          placeholder="https://provider.example/playlist.m3u"
                          value={field.state.value}
                          onBlur={field.handleBlur}
                          onChange={(event) => field.handleChange(event.target.value)}
                          aria-invalid={field.state.meta.errors.length > 0}
                        />
                        <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                      </label>
                    )}
                  </form.Field>
                )}
              </form.Subscribe>
              <form.Field
                name="timezone"
                validators={{ onChange: sourceSchema.shape.timezone }}
              >
                {(field) => (
                  <label className="text-xs font-medium text-slate-300">
                    Timezone
                    <Input
                      className="mt-1"
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) => field.handleChange(event.target.value)}
                      placeholder="UTC"
                      aria-invalid={field.state.meta.errors.length > 0}
                    />
                    <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                    <span className="mt-1 block text-[0.68rem] text-slate-500">
                      Use an IANA timezone for XMLTV values without an offset.
                    </span>
                  </label>
                )}
              </form.Field>
              <form.Subscribe selector={(state) => [state.canSubmit, state.isSubmitting]}>
                {([canSubmit, isSubmitting]) => (
                  <Button className="mt-5" type="submit" disabled={!canSubmit || isSubmitting}>
                    {isSubmitting ? 'Wait…' : 'Add source'}
                  </Button>
                )}
              </form.Subscribe>
            </form>
          </CardContent>
        </Card>
      ) : null}

      <Card>
        <div className="overflow-x-auto">
          <Table>
            <caption className="sr-only">Configured IPTV and guide sources</caption>
            <TableHeader><TableRow><TableHead>Name</TableHead><TableHead>Type</TableHead><TableHead>Status</TableHead><TableHead>Channels</TableHead><TableHead>Last sync</TableHead><TableHead>Refresh</TableHead><TableHead className="text-right">Actions</TableHead></TableRow></TableHeader>
            <TableBody>
              {sourcesQuery.data.map((source) => (
                <TableRow key={source.id}>
                  <TableCell><p className="font-medium text-white">{source.name}</p><p className="mt-1 max-w-md truncate font-mono text-[0.68rem] text-slate-600">{source.endpoint}</p></TableCell>
                  <TableCell>{source.kind}</TableCell>
                  <TableCell>
                    {syncingSourceIds.has(source.id) ? (
                      <SourceSyncProgress client={client} sourceId={source.id} enabled={syncingSourceIds.has(source.id)} onComplete={removeSyncing} />
                    ) : source.state === 'syncing' ? (
                      <div className="inline-flex items-center gap-1.5" aria-live="polite">
                        <RefreshCw aria-hidden="true" className="size-3.5 animate-spin text-cyan-400" />
                        <span className="text-xs font-medium text-cyan-200">Starting…</span>
                      </div>
                    ) : (
                      <div className="flex flex-col items-start gap-1">
                        <HealthBadge state={source.state} />
                        {source.state === 'degraded' ? (
                          <SourceSyncFailure
                            sourceName={source.name}
                            status={syncStatusById.get(source.id)}
                            jobs={jobsQuery.data}
                            onRetry={() => syncMutation.mutate(source.id)}
                            retryPending={syncMutation.isPending && syncMutation.variables === source.id}
                          />
                        ) : null}
                      </div>
                    )}
                  </TableCell>
                  <TableCell className="font-mono text-slate-200">{source.channels.toLocaleString()}</TableCell>
                  <TableCell>{formatRelativeTime(source.lastSync)}</TableCell>
                  <TableCell>
                    {editingInterval === source.id ? (
                      <span className="inline-flex items-center gap-1">
                        <Input
                          className="h-7 w-20 text-xs"
                          type="number"
                          min={0}
                          value={intervalValue}
                          onChange={(event) => setIntervalValue(event.target.value)}
                          aria-label="Refresh interval in seconds"
                        />
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={refreshIntervalMutation.isPending}
                          onClick={() => {
                            const seconds = Math.max(0, Math.floor(Number(intervalValue) || 0))
                            refreshIntervalMutation.mutate(
                              { id: source.id, seconds },
                              { onSuccess: () => setEditingInterval(null) },
                            )
                          }}
                        >
                          {refreshIntervalMutation.isPending ? 'Wait…' : 'Save'}
                        </Button>
                        <Button variant="ghost" size="sm" onClick={() => setEditingInterval(null)}>
                          Cancel
                        </Button>
                      </span>
                    ) : (
                      <button
                        className="inline-flex items-center gap-1 text-xs text-slate-300 hover:text-white"
                        onClick={() => {
                          setIntervalValue(String(source.refreshIntervalSeconds))
                          setEditingInterval(source.id)
                        }}
                      >
                        <Clock aria-hidden="true" className="size-3.5 text-slate-500" />
                        {source.refreshIntervalSeconds > 0 ? (
                          <span>
                            {formatInterval(source.refreshIntervalSeconds)}
                            {source.lastRefreshedAt ? (
                              <span className="block text-[0.68rem] text-slate-500">
                                last: {formatRelativeTime(source.lastRefreshedAt)}
                              </span>
                            ) : null}
                          </span>
                        ) : (
                          <Badge tone="neutral">Manual</Badge>
                        )}
                      </button>
                    )}
                  </TableCell>
                  <TableCell className="text-right">
                    {confirmDelete === source.id ? (
                      <span className="inline-flex items-center gap-2">
                        <span className="text-xs text-slate-300">Remove?</span>
                        <Button variant="ghost" size="sm" className="text-red-300 hover:text-red-200" disabled={deleteMutation.isPending} onClick={() => deleteMutation.mutate(source.id)}>
                          {deleteMutation.isPending ? 'Wait…' : 'Yes'}
                        </Button>
                        <Button variant="ghost" size="sm" onClick={() => setConfirmDelete(null)}>No</Button>
                      </span>
                    ) : (
                      <span className="inline-flex items-center gap-1">
                        <Button
                          variant="ghost"
                          size="sm"
                          className="text-slate-400 hover:text-ocean-400"
                          aria-label={`Sync ${source.name}`}
                          disabled={syncMutation.isPending || syncingSourceIds.has(source.id)}
                          onClick={() => syncMutation.mutate(source.id)}
                        >
                          {syncingSourceIds.has(source.id) || (syncMutation.isPending && syncMutation.variables === source.id) ? (
                            <RefreshCw aria-hidden="true" className="size-4 animate-spin" />
                          ) : (
                            <RefreshCw aria-hidden="true" className="size-4" />
                          )}
                        </Button>
                        <Button variant="ghost" size="sm" className="text-slate-400 hover:text-ocean-400" aria-label={`Edit ${source.name}`} onClick={() => setEditingSource(source)}>
                          <Pencil aria-hidden="true" className="size-4" />
                        </Button>
                        <Button variant="ghost" size="sm" className="text-slate-400 hover:text-red-300" aria-label={`Remove ${source.name}`} onClick={() => setConfirmDelete(source.id)}>
                          <Trash2 aria-hidden="true" className="size-4" />
                        </Button>
                      </span>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </Card>

      {editingSource ? (
        <SourceEditDialog
          source={editingSource}
          pending={updateSourceMutation.isPending}
          onSave={(input) =>
            updateSourceMutation.mutate({ id: editingSource.id, input })
          }
          onClose={() => setEditingSource(null)}
        />
      ) : null}
    </>
  )
}

function SourceSyncFailure({
  sourceName,
  status,
  jobs,
  onRetry,
  retryPending,
}: {
  sourceName: string
  status: SourceSyncStatus | undefined
  jobs: Job[] | undefined
  onRetry: () => void
  retryPending: boolean
}) {
  if (status?.status !== 'failed') return null
  const lastError = jobs?.find((job) => job.id === status.jobId)?.lastError
  const reason = lastError ?? (status.message || 'The error detail is not available.')
  return (
    <div className="mt-1 flex max-w-xs flex-col items-start gap-1" role="alert">
      <p className="text-[0.68rem] leading-4 text-red-200">
        <span className="font-semibold">Last sync failed.</span> {reason}
      </p>
      <Button
        variant="ghost"
        size="sm"
        className="h-7 min-h-7 px-2 text-red-200 hover:text-red-100"
        aria-label={`Retry sync ${sourceName}`}
        disabled={retryPending}
        onClick={onRetry}
      >
        <RefreshCw aria-hidden="true" className="size-3.5" /> Retry sync
      </Button>
    </div>
  )
}

function SourceSyncProgress({
  client,
  sourceId,
  enabled,
  onComplete,
}: {
  client: IptvApiClient
  sourceId: string
  enabled: boolean
  onComplete: (id: string) => void
}) {
  const queryClient = useQueryClient()
  const didInvalidate = useRef(false)
  const syncStatus = useQuery({ ...apiQueries(client).sourceSyncStatus(sourceId), enabled })
  const cancelMutation = useMutation({
    mutationFn: () => client.cancelSourceSync(sourceId),
    onSuccess: (result) => {
      queryClient.setQueryData<SourceSyncStatus | undefined>(
        ['source-sync-status', sourceId],
        (old) => {
          const base = old ?? {
            jobId: '',
            stage: 'completed' as SourceSyncStatus['stage'],
            percent: 0,
            message: '',
            bytesDownloaded: 0,
            recordsProcessed: 0,
            startedAt: new Date().toISOString(),
            updatedAt: new Date().toISOString(),
          }
          return { ...base, status: 'cancelled' as SourceSyncStatus['status'], message: result.message }
        },
      )
    },
  })

  useEffect(() => {
    if ((syncStatus.data?.status === 'succeeded' || syncStatus.data?.status === 'failed' || syncStatus.data?.status === 'cancelled') && !didInvalidate.current) {
      didInvalidate.current = true
      queryClient.invalidateQueries({ queryKey: ['sources'] })
      queryClient.invalidateQueries({ queryKey: ['overview'] })
      queryClient.invalidateQueries({ queryKey: ['jobs'] })
    }
  }, [syncStatus.data, queryClient])

  useEffect(() => {
    if (syncStatus.data?.status === 'succeeded' || syncStatus.data?.status === 'failed' || syncStatus.data?.status === 'cancelled') {
      const timeout = setTimeout(() => onComplete(sourceId), 2000)
      return () => clearTimeout(timeout)
    }
  }, [syncStatus.data?.status, sourceId, onComplete])

  useEffect(() => {
    if (cancelMutation.isError) {
      const timeout = setTimeout(() => onComplete(sourceId), 2000)
      return () => clearTimeout(timeout)
    }
  }, [cancelMutation.isError, sourceId, onComplete])

  if (syncStatus.isLoading) {
    return <span className="text-[0.7rem] text-slate-400">Sync starts…</span>
  }

  if (syncStatus.isError) {
    return (
      <div className="inline-flex flex-col gap-1" role="alert">
        <Badge tone="danger">Sync failed</Badge>
        <span className="text-[0.68rem] text-red-200">{syncStatus.error instanceof Error ? syncStatus.error.message : 'Sync status unavailable'}</span>
      </div>
    )
  }

  const data = syncStatus.data
  if (!data) return null

  if (data.status === 'succeeded') {
    return (
      <div role="status">
        <Badge tone="success">Synced</Badge>
      </div>
    )
  }

  if (data.status === 'failed') {
    return (
      <div className="inline-flex flex-col gap-1" role="alert">
        <Badge tone="danger">Sync failed</Badge>
        {data.message ? <span className="text-[0.68rem] text-red-200">{data.message}</span> : null}
      </div>
    )
  }

  if (data.status === 'cancelled') {
    return (
      <div role="status">
        <Badge tone="neutral">Cancelled</Badge>
      </div>
    )
  }

  if (cancelMutation.isError) {
    return (
      <div className="inline-flex flex-col gap-1" role="alert">
        <Badge tone="danger">Cancel failed</Badge>
        <span className="text-[0.68rem] text-red-200">{cancelMutation.error instanceof Error ? cancelMutation.error.message : 'Cancel sync failed.'}</span>
      </div>
    )
  }

  if (data.status === 'queued' || (!data.stage && data.percent === 0)) {
    return (
      <div className="flex w-full items-center gap-2" role="status" aria-live="polite">
        <div className="flex min-w-[8rem] flex-1 items-center gap-2">
          <RefreshCw aria-hidden="true" className="size-3.5 animate-spin text-cyan-400" />
          <span className="text-xs font-medium text-cyan-200">Queued</span>
          {data.startedAt ? <span className="text-[0.68rem] text-slate-500">{formatElapsed(data.startedAt)}</span> : null}
        </div>
        <Button
          variant="ghost"
          size="sm"
          aria-label="Cancel sync"
          title="Cancel sync"
          disabled={cancelMutation.isPending}
          onClick={() => cancelMutation.mutate()}
          className="text-slate-400 hover:text-red-300"
        >
          <X aria-hidden="true" className="size-4" />
        </Button>
      </div>
    )
  }

  const stage = stageLabel[data.stage] || 'Processing'
  const hasBytes = data.bytesDownloaded !== undefined && data.bytesDownloaded > 0
  const hasRecords = data.recordsProcessed !== undefined && data.recordsProcessed > 0
  const elapsed = data.startedAt ? formatElapsed(data.startedAt) : null

  return (
    <div className="flex w-full items-center gap-2" role="status" aria-live="polite">
      <div className="min-w-[8rem] flex-1">
        <div className="mb-1 flex items-center justify-between text-xs">
          <span className="font-medium text-cyan-200">{stage}</span>
          <span className="text-cyan-200">{data.percent}%{elapsed ? <span className="text-slate-500"> · {elapsed}</span> : null}</span>
        </div>
        <div
          className="h-1.5 w-full overflow-hidden rounded-full bg-slate-700"
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={data.percent}
          aria-label="Sync progress"
        >
          <div
            className="h-full rounded-full bg-cyan-400 transition-all duration-500"
            style={{ width: `${Math.min(100, Math.max(0, data.percent))}%` }}
          />
        </div>
        {data.message ? <p className="mt-1 text-[0.68rem] text-slate-400">{data.message}</p> : null}
        {hasBytes || hasRecords ? (
          <p className="text-[0.68rem] text-slate-500">
            {hasBytes ? formatBytes(data.bytesDownloaded!) : null}
            {hasBytes && hasRecords ? ' · ' : null}
            {hasRecords ? `${data.recordsProcessed!.toLocaleString()} records` : null}
          </p>
        ) : null}
      </div>
      <Button
        variant="ghost"
        size="sm"
        aria-label="Cancel sync"
        title="Cancel sync"
        disabled={cancelMutation.isPending}
        onClick={() => cancelMutation.mutate()}
        className="text-slate-400 hover:text-red-300"
      >
        <X aria-hidden="true" className="size-4" />
      </Button>
    </div>
  )
}

function SourceEditDialog({
  source,
  pending,
  onSave,
  onClose,
}: {
  source: Source
  pending: boolean
  onSave: (input: SourceUpdateInput) => void
  onClose: () => void
}) {
  const form = useForm({
    defaultValues: {
      maxConnections: source.maxConnections,
      timezone: source.timezone,
      enabled: source.enabled,
    },
    validators: { onSubmit: sourceUpdateSchema },
    onSubmit: async ({ value }) => {
      onSave({ maxConnections: value.maxConnections, timezone: value.timezone.trim() || 'UTC', enabled: value.enabled })
    },
  })

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4" role="dialog" aria-modal="true" aria-label={`Edit ${source.name}`}>
      <Card className="w-full max-w-md">
        <CardHeader>
          <h2 className="font-semibold text-white">Edit {source.name}</h2>
        </CardHeader>
        <CardContent>
          <form
            className="space-y-4"
            onSubmit={(event) => {
              event.preventDefault()
              event.stopPropagation()
              void form.handleSubmit().catch(() => undefined)
            }}
          >
            <form.Field name="maxConnections" validators={{ onChange: sourceUpdateSchema.shape.maxConnections }}>
              {(field) => <label className="block text-xs font-medium text-slate-300">
                Max connections
                <Input
                  className="mt-1"
                  type="number"
                  min={1}
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => field.handleChange(Number(event.target.value))}
                  aria-label="Max connections"
                  aria-invalid={field.state.meta.errors.length > 0}
                />
                <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                <span className="mt-1 block text-[0.68rem] text-slate-500">
                  Maximum simultaneous streams from this provider.
                </span>
              </label>}
            </form.Field>
            <form.Field name="timezone" validators={{ onChange: sourceUpdateSchema.shape.timezone }}>
              {(field) => <label className="block text-xs font-medium text-slate-300">
                Timezone
                <Input
                  className="mt-1"
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => field.handleChange(event.target.value)}
                  aria-label="Timezone"
                  placeholder="UTC"
                  aria-invalid={field.state.meta.errors.length > 0}
                />
                <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                <span className="mt-1 block text-[0.68rem] text-slate-500">
                  IANA timezone for guide data interpretation.
                </span>
              </label>}
            </form.Field>
            <form.Field name="enabled">
              {(field) => <label className="flex items-center gap-2 text-xs font-medium text-slate-300">
                <input
                  type="checkbox"
                  checked={field.state.value}
                  onChange={(event) => field.handleChange(event.target.checked)}
                  className="size-4 rounded border-white/20 bg-white/5"
                />
                Enabled
              </label>}
            </form.Field>
            <div className="flex justify-end gap-2 pt-2">
              <Button variant="ghost" size="sm" onClick={onClose} disabled={pending}>
                Cancel
              </Button>
              <form.Subscribe selector={(state) => [state.canSubmit, state.isSubmitting]}>
                {([canSubmit, isSubmitting]) => <Button size="sm" type="submit" disabled={pending || !canSubmit || isSubmitting}>
                  {pending || isSubmitting ? 'Wait…' : 'Save changes'}
                </Button>}
              </form.Subscribe>
            </div>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
