import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, ChevronLeft, ChevronRight, History, Link2, RefreshCw, RotateCcw, Search, Unlink, X } from 'lucide-react'
import { useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { apiClient, IptvApiError } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { EpgMapping, IptvApiClient, ReconciliationRevision } from '@/lib/api/types'

const PAGE_SIZE = 50

type Tab = 'mapped' | 'review' | 'unmapped'
type BulkReviewAction = 'accept' | 'reject'

interface BulkReviewResult {
  action: BulkReviewAction
  resolved: number
}

export function EpgMappingsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const [tab, setTab] = useState<Tab>('mapped')
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(0)
  const [selectedReviewIds, setSelectedReviewIds] = useState<Set<string>>(new Set())
  const [bulkReviewAction, setBulkReviewAction] = useState<BulkReviewAction | null>(null)
  const [bulkReviewError, setBulkReviewError] = useState<string | null>(null)
  const [bulkReviewResult, setBulkReviewResult] = useState<BulkReviewResult | null>(null)
  const queryClient = useQueryClient()

  const reconcileMutation = useMutation({
    mutationFn: () => client.reconcileEpg(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['epg-mappings'] })
      queryClient.invalidateQueries({ queryKey: ['epg-unmapped'] })
      queryClient.invalidateQueries({ queryKey: ['epg-review'] })
      queryClient.invalidateQueries({ queryKey: ['overview'] })
    },
  })

  const reviewStatus = tab === 'review' ? 'review' : tab === 'mapped' ? 'applied' : undefined

  const mappingsQuery = useQuery({
    queryKey: ['epg-mappings', reviewStatus, page],
    queryFn: () => client.getEpgMappings(reviewStatus, PAGE_SIZE, page * PAGE_SIZE),
    enabled: tab === 'mapped' || tab === 'review',
  })

  const unmappedQuery = useQuery({
    queryKey: ['epg-unmapped', search, page],
    queryFn: () => client.getUnmappedChannels(search || undefined, PAGE_SIZE, page * PAGE_SIZE),
    enabled: tab === 'unmapped',
  })

  const total = mappingsQuery.data?.total ?? unmappedQuery.data?.total ?? 0
  const pageCount = Math.ceil(total / PAGE_SIZE)
  const activeQueryLoading = tab === 'unmapped' ? unmappedQuery.isLoading : mappingsQuery.isLoading

  const removeMapping = useMutation({
    mutationFn: (channelId: string) => client.removeChannelEpgMapping(channelId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['epg-mappings'] })
      queryClient.invalidateQueries({ queryKey: ['epg-unmapped'] })
    },
  })

  const reviewMappings = tab === 'review' ? mappingsQuery.data?.items ?? [] : []
  const selectedReviewMappings = reviewMappings.filter((mapping) => selectedReviewIds.has(mapping.channelId))
  const allReviewMappingsSelected = reviewMappings.length > 0 && reviewMappings.every((mapping) => selectedReviewIds.has(mapping.channelId))

  const bulkReviewMutation = useMutation<BulkReviewResult, unknown, { action: BulkReviewAction; mappings: EpgMapping[] }>({
    mutationFn: async ({ action, mappings }) => {
      // Resolve one mapping at a time in channel ID order. This keeps audit events
      // reproducible and avoids sending an empty bulk request.
      const orderedMappings = [...mappings].sort((left, right) => left.channelId.localeCompare(right.channelId))
      for (const mapping of orderedMappings) {
        await client.resolveReview(mapping.channelId, action === 'accept', action === 'accept' ? mapping.epgChannelId : undefined)
      }
      return { action, resolved: orderedMappings.length }
    },
    onSuccess: (result) => {
      setSelectedReviewIds(new Set())
      setBulkReviewAction(null)
      setBulkReviewError(null)
      setBulkReviewResult(result)
      queryClient.invalidateQueries({ queryKey: ['epg-mappings'] })
      queryClient.invalidateQueries({ queryKey: ['epg-review'] })
      queryClient.invalidateQueries({ queryKey: ['epg-unmapped'] })
    },
    onError: (failure: unknown) => {
      setBulkReviewError(failure instanceof IptvApiError ? failure.problem.detail ?? failure.problem.title : 'The bulk review failed. Try again.')
      setBulkReviewAction(null)
    },
  })

  return (
    <>
      <PageHeader
        eyebrow="Guide"
        title="EPG Mappings"
        description="Review and override how channels connect to XMLTV programme data. Confidence scores show the match quality."
        actions={
          <Button
            variant="secondary"
            size="sm"
            disabled={reconcileMutation.isPending}
            onClick={() => reconcileMutation.mutate()}
          >
            <RefreshCw aria-hidden="true" className={`size-4 ${reconcileMutation.isPending ? 'animate-spin' : ''}`} />
            {reconcileMutation.isPending ? 'Reconciling…' : 'Reconcile now'}
          </Button>
        }
      />

      {reconcileMutation.data ? (
        <div className="mb-4 flex items-center gap-4 rounded-lg border border-ocean-400/30 bg-ocean-400/5 p-3 text-xs text-slate-300">
          <span>Applied: <strong className="text-white">{reconcileMutation.data.mappingsApplied.toLocaleString()}</strong></span>
          <span>Removed: <strong className="text-white">{reconcileMutation.data.mappingsRemoved.toLocaleString()}</strong></span>
          <span>Review queued: <strong className="text-white">{reconcileMutation.data.reviewQueued.toLocaleString()}</strong></span>
        </div>
      ) : null}

      <ReconciliationRollbackCard client={client} />

      <Card>
        <div className="flex flex-col gap-3 border-b border-white/8 p-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex gap-2">
            {(['mapped', 'review', 'unmapped'] as const).map((t) => (
              <Button
                key={t}
                variant={tab === t ? 'primary' : 'ghost'}
                size="sm"
                onClick={() => { setTab(t); setPage(0); setSelectedReviewIds(new Set()); setBulkReviewAction(null); setBulkReviewError(null); setBulkReviewResult(null) }}
              >
                {t === 'mapped' ? 'Mapped' : t === 'review' ? 'Needs review' : 'Unmapped'}
              </Button>
            ))}
          </div>
          {tab === 'unmapped' ? (
            <label className="relative block max-w-sm flex-1">
              <span className="sr-only">Search unmapped channels</span>
              <Search aria-hidden="true" className="pointer-events-none absolute left-3 top-3 size-4 text-slate-500" />
              <Input className="pl-9" value={search} onChange={(event) => { setSearch(event.target.value); setPage(0) }} placeholder="Search channel name" />
            </label>
          ) : null}
          <p role="status" className="text-xs text-slate-500">
            {mappingsQuery.isFetching || unmappedQuery.isFetching ? 'Loading…' : `${total.toLocaleString()} channels`}
          </p>
        </div>

        {tab === 'review' ? (
          <div className="flex flex-wrap items-center gap-2 border-b border-white/8 px-4 py-3">
            <p className="mr-auto text-xs text-slate-500">
              {selectedReviewMappings.length > 0 ? `${selectedReviewMappings.length} selected` : 'Select mappings to review together.'}
            </p>
            <Button
              variant="secondary"
              size="sm"
              disabled={selectedReviewMappings.length === 0 || bulkReviewMutation.isPending}
              onClick={() => { setBulkReviewError(null); setBulkReviewAction('accept') }}
            >
              <Check aria-hidden="true" className="size-4" /> Accept selected
            </Button>
            <Button
              variant="danger"
              size="sm"
              disabled={selectedReviewMappings.length === 0 || bulkReviewMutation.isPending}
              onClick={() => { setBulkReviewError(null); setBulkReviewAction('reject') }}
            >
              <X aria-hidden="true" className="size-4" /> Reject selected
            </Button>
          </div>
        ) : null}

        {tab === 'review' && bulkReviewAction ? (
          <div className="flex flex-wrap items-center gap-3 border-b border-amber-400/20 bg-amber-400/5 px-4 py-3 text-xs text-amber-100" role="alertdialog" aria-label="Confirm bulk review">
            <p className="mr-auto">
              {bulkReviewAction === 'accept'
                ? `Apply the current best candidate to ${selectedReviewMappings.length} selected mapping(s)?`
                : `Reject ${selectedReviewMappings.length} selected mapping(s)?`}
            </p>
            <Button
              variant={bulkReviewAction === 'accept' ? 'primary' : 'danger'}
              size="sm"
              disabled={bulkReviewMutation.isPending || selectedReviewMappings.length === 0}
              onClick={() => bulkReviewMutation.mutate({ action: bulkReviewAction, mappings: selectedReviewMappings })}
            >
              {bulkReviewMutation.isPending ? 'Applying…' : bulkReviewAction === 'accept' ? 'Confirm accept' : 'Confirm reject'}
            </Button>
            <Button variant="ghost" size="sm" disabled={bulkReviewMutation.isPending} onClick={() => setBulkReviewAction(null)}>Cancel</Button>
          </div>
        ) : null}

        {bulkReviewResult ? (
          <p role="status" className="border-b border-white/8 px-4 py-2 text-xs text-mint-400">
            {bulkReviewResult.action === 'accept' ? 'Accepted' : 'Rejected'} {bulkReviewResult.resolved} mapping(s).
          </p>
        ) : null}
        {bulkReviewError ? <p role="alert" className="border-b border-white/8 px-4 py-2 text-xs text-red-400">{bulkReviewError}</p> : null}

        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                {tab === 'review' ? (
                  <TableHead>
                    <input
                      type="checkbox"
                      aria-label="Select all review mappings"
                      checked={allReviewMappingsSelected}
                      disabled={reviewMappings.length === 0 || bulkReviewMutation.isPending}
                      onChange={(event) => {
                        setSelectedReviewIds(event.target.checked
                          ? new Set(reviewMappings.map((mapping) => mapping.channelId))
                          : new Set())
                      }}
                    />
                  </TableHead>
                ) : null}
                <TableHead>Channel</TableHead>
                <TableHead>EPG match</TableHead>
                <TableHead>Method</TableHead>
                <TableHead>Confidence</TableHead>
                {tab === 'review' ? <TableHead>Actions</TableHead> : <TableHead className="text-right">Actions</TableHead>}
              </TableRow>
            </TableHeader>
            <TableBody>
              {activeQueryLoading ? (
                <TableRow><TableCell colSpan={tab === 'review' ? 6 : 5} className="py-8 text-center text-slate-500">Loading mappings…</TableCell></TableRow>
              ) : tab === 'unmapped' ? (
                unmappedQuery.data?.items.length === 0 ? (
                  <TableRow><TableCell colSpan={5} className="py-8 text-center text-slate-500">All channels have EPG mappings.</TableCell></TableRow>
                ) : (
                  unmappedQuery.data?.items.map((channel) => (
                    <UnmappedChannelRow
                      key={channel.id}
                      channel={channel}
                      client={client}
                      onMapped={() => {
                        queryClient.invalidateQueries({ queryKey: ['epg-mappings'] })
                        queryClient.invalidateQueries({ queryKey: ['epg-unmapped'] })
                      }}
                    />
                  ))
                )
              ) : (
                mappingsQuery.data?.items.length === 0 ? (
                  <TableRow><TableCell colSpan={tab === 'review' ? 6 : 5} className="py-8 text-center text-slate-500">
                    {tab === 'review' ? 'No channels need review.' : 'No mappings found.'}
                  </TableCell></TableRow>
                ) : (
                  mappingsQuery.data?.items.map((mapping) => (
                    <MappingRow
                      key={mapping.channelId}
                      mapping={mapping}
                      showReviewActions={tab === 'review'}
                      selected={selectedReviewIds.has(mapping.channelId)}
                      onSelect={(selected) => {
                        setSelectedReviewIds((current) => {
                          const next = new Set(current)
                          if (selected) next.add(mapping.channelId)
                          else next.delete(mapping.channelId)
                          return next
                        })
                      }}
                      client={client}
                      onRemove={() => removeMapping.mutate(mapping.channelId)}
                      onResolved={() => {
                        setSelectedReviewIds((current) => {
                          const next = new Set(current)
                          next.delete(mapping.channelId)
                          return next
                        })
                        queryClient.invalidateQueries({ queryKey: ['epg-mappings'] })
                        queryClient.invalidateQueries({ queryKey: ['epg-review'] })
                        queryClient.invalidateQueries({ queryKey: ['epg-unmapped'] })
                      }}
                    />
                  ))
                )
              )}
            </TableBody>
          </Table>
        </div>

        {pageCount > 1 ? (
          <div className="flex items-center justify-between border-t border-white/8 p-3">
            <p className="text-xs text-slate-500">Page {page + 1} of {pageCount.toLocaleString()}</p>
            <div className="flex gap-2">
              <Button variant="ghost" size="sm" disabled={page === 0} onClick={() => { setPage((p) => Math.max(0, p - 1)); setSelectedReviewIds(new Set()) }}>
                <ChevronLeft aria-hidden="true" className="size-4" /> Prev
              </Button>
              <Button variant="ghost" size="sm" disabled={page >= pageCount - 1} onClick={() => { setPage((p) => p + 1); setSelectedReviewIds(new Set()) }}>
                Next <ChevronRight aria-hidden="true" className="size-4" />
              </Button>
            </div>
          </div>
        ) : null}
      </Card>
    </>
  )
}

function confidenceTone(confidence: number): 'success' | 'warning' | 'danger' {
  if (confidence >= 0.95) return 'success'
  if (confidence >= 0.85) return 'warning'
  return 'danger'
}

function methodTone(method: string): 'info' | 'neutral' {
  if (method === 'alias') return 'info'
  return 'neutral'
}

function methodLabel(method: string): string {
  if (method === 'alias') return 'Alias'
  return method
}

function MappingRow({
  mapping,
  showReviewActions,
  selected,
  onSelect,
  client,
  onRemove,
  onResolved,
}: {
  mapping: EpgMapping
  showReviewActions: boolean
  selected: boolean
  onSelect: (selected: boolean) => void
  client: IptvApiClient
  onRemove: () => void
  onResolved: () => void
}) {
  const [showCandidates, setShowCandidates] = useState(false)

  if (showCandidates) {
    return (
      <CandidateReviewRow
        channelId={mapping.channelId}
        channelName={mapping.channelName}
        client={client}
        onClose={() => setShowCandidates(false)}
        onResolved={onResolved}
      />
    )
  }

  return (
    <TableRow>
      {showReviewActions ? (
        <TableCell>
          <input
            type="checkbox"
            aria-label={`Select review mapping for ${mapping.channelName}`}
            checked={selected}
            onChange={(event) => onSelect(event.target.checked)}
          />
        </TableCell>
      ) : null}
      <TableCell>
        <p className="font-medium text-white">{mapping.channelName}</p>
        {mapping.canonicalKey ? <p className="mt-0.5 font-mono text-[0.68rem] text-slate-600">{mapping.canonicalKey}</p> : null}
      </TableCell>
      <TableCell>
        {mapping.epgDisplayName ? (
          <div>
            <p className="text-sm text-slate-200">{mapping.epgDisplayName}</p>
            {mapping.epgXmltvId ? <p className="mt-0.5 font-mono text-[0.68rem] text-slate-600">{mapping.epgXmltvId}</p> : null}
          </div>
        ) : <span className="text-slate-500">—</span>}
      </TableCell>
      <TableCell><Badge tone={methodTone(mapping.method)}>{methodLabel(mapping.method)}</Badge></TableCell>
      <TableCell>
        <Badge tone={confidenceTone(mapping.confidence)}>
          {(mapping.confidence * 100).toFixed(0)}%
        </Badge>
      </TableCell>
      <TableCell className="text-right">
        {showReviewActions ? (
          <span className="inline-flex gap-1">
            <Button variant="ghost" size="sm" className="text-emerald-300 hover:text-emerald-200" onClick={() => setShowCandidates(true)}>
              <Check aria-hidden="true" className="size-4" /> Review
            </Button>
          </span>
        ) : (
          <span className="inline-flex gap-1">
            <Button variant="ghost" size="sm" className="text-slate-400 hover:text-red-300" aria-label={`Remove mapping for ${mapping.channelName}`} onClick={onRemove}>
              <Unlink aria-hidden="true" className="size-4" />
            </Button>
          </span>
        )}
      </TableCell>
    </TableRow>
  )
}

function UnmappedChannelRow({
  channel,
  client,
  onMapped,
}: {
  channel: { id: string; name: string; canonicalKey: string | null; groupName: string | null }
  client: IptvApiClient
  onMapped: () => void
}) {
  const [searching, setSearching] = useState(false)
  const [query, setQuery] = useState('')
  const queryClient = useQueryClient()

  const searchQuery = useQuery({
    queryKey: ['epg-channel-search', query],
    queryFn: () => client.searchEpgChannels(query, 20),
    enabled: searching && query.length >= 2,
  })

  const setMapping = useMutation({
    mutationFn: (epgChannelId: string) => client.setChannelEpgMapping(channel.id, epgChannelId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['epg-channel-search'] })
      onMapped()
      setSearching(false)
    },
  })

  if (searching) {
    return (
      <TableRow>
        <TableCell colSpan={5}>
          <div className="space-y-3">
            <p className="text-sm font-medium text-white">Link "{channel.name}" to an EPG channel</p>
            <label className="relative block max-w-md">
              <span className="sr-only">Search EPG channels</span>
              <Search aria-hidden="true" className="pointer-events-none absolute left-3 top-3 size-4 text-slate-500" />
              <Input className="pl-9" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search EPG channel name or XMLTV ID" autoFocus />
            </label>
            {searchQuery.data && searchQuery.data.length > 0 ? (
              <ul className="max-h-48 overflow-y-auto rounded-lg border border-white/10">
                {searchQuery.data.map((result) => (
                  <li key={result.id}>
                    <button
                      className="flex w-full items-center justify-between gap-2 px-3 py-2 text-left text-sm text-slate-200 hover:bg-white/5"
                      onClick={() => setMapping.mutate(result.id)}
                      disabled={setMapping.isPending}
                    >
                      <span>
                        <span className="text-white">{result.displayName ?? result.xmltvId}</span>
                        {result.displayName ? <span className="ml-2 font-mono text-[0.68rem] text-slate-500">{result.xmltvId}</span> : null}
                      </span>
                      <Link2 aria-hidden="true" className="size-4 text-ocean-400" />
                    </button>
                  </li>
                ))}
              </ul>
            ) : searchQuery.data && searchQuery.data.length === 0 ? (
              <p className="text-xs text-slate-500">No EPG channels found. Try a different search term.</p>
            ) : null}
            <div className="flex gap-2">
              <Button variant="ghost" size="sm" onClick={() => setSearching(false)}>Cancel</Button>
            </div>
          </div>
        </TableCell>
      </TableRow>
    )
  }

  return (
    <TableRow>
      <TableCell>
        <p className="font-medium text-white">{channel.name}</p>
        {channel.canonicalKey ? <p className="mt-0.5 font-mono text-[0.68rem] text-slate-600">{channel.canonicalKey}</p> : null}
      </TableCell>
      <TableCell><span className="text-slate-500">No EPG match</span></TableCell>
      <TableCell><Badge tone="danger">unmapped</Badge></TableCell>
      <TableCell>—</TableCell>
      <TableCell className="text-right">
        <Button variant="ghost" size="sm" className="text-ocean-400 hover:text-ocean-300" onClick={() => setSearching(true)}>
          <Link2 aria-hidden="true" className="size-4" /> Link
        </Button>
      </TableCell>
    </TableRow>
  )
}

function CandidateReviewRow({
  channelId,
  channelName,
  client,
  onClose,
  onResolved,
}: {
  channelId: string
  channelName: string
  client: IptvApiClient
  onClose: () => void
  onResolved: () => void
}) {
  const candidatesQuery = useQuery({
    queryKey: ['review-candidates', channelId],
    queryFn: () => client.getReviewCandidates(channelId),
  })

  const resolveMutation = useMutation({
    mutationFn: ({ accept, epgChannelId }: { accept: boolean; epgChannelId?: string }) =>
      client.resolveReview(channelId, accept, epgChannelId),
    onSuccess: () => {
      onResolved()
      onClose()
    },
  })

  if (!candidatesQuery.data) return <TableRow><TableCell colSpan={6} className="text-center text-slate-500">Loading candidates…</TableCell></TableRow>

  return (
    <TableRow>
      <TableCell colSpan={6}>
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <p className="text-sm font-medium text-white">Review candidates for "{channelName}"</p>
            <Button variant="ghost" size="sm" onClick={onClose}><X aria-hidden="true" className="size-4" /></Button>
          </div>
          {candidatesQuery.data.length === 0 ? (
            <p className="text-xs text-slate-500">No candidates available.</p>
          ) : (
            <ul className="space-y-1">
              {candidatesQuery.data.map((candidate) => (
                <li key={candidate.id} className="flex items-center justify-between gap-2 rounded-lg border border-white/8 px-3 py-2">
                  <div className="flex items-center gap-3">
                    <Badge tone={confidenceTone(candidate.confidence)}>
                      {(candidate.confidence * 100).toFixed(0)}%
                    </Badge>
                    <div>
                      <p className="text-sm text-slate-200">{candidate.epgDisplayName ?? candidate.epgXmltvId ?? 'Unknown'}</p>
                      {candidate.epgXmltvId ? <p className="font-mono text-[0.68rem] text-slate-600">{candidate.epgXmltvId}</p> : null}
                    </div>
                  </div>
                  <div className="flex gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      className="text-emerald-300 hover:text-emerald-200"
                      disabled={resolveMutation.isPending}
                      onClick={() => resolveMutation.mutate({ accept: true, epgChannelId: candidate.epgChannelId })}
                    >
                      <Check aria-hidden="true" className="size-4" /> Accept
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          )}
          <div className="flex gap-2">
            <Button
              variant="ghost"
              size="sm"
              className="text-red-300 hover:text-red-200"
              disabled={resolveMutation.isPending}
              onClick={() => resolveMutation.mutate({ accept: false })}
            >
              <X aria-hidden="true" className="size-4" /> Reject all
            </Button>
          </div>
        </div>
      </TableCell>
    </TableRow>
  )
}

function ReconciliationRollbackCard({ client }: { client: IptvApiClient }) {
  const queryClient = useQueryClient()
  const [sourceId, setSourceId] = useState('')
  const [rollbackTarget, setRollbackTarget] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)

  const revisionsQuery = useQuery({
    ...apiQueries(client).reconciliationRevisions(sourceId),
    enabled: sourceId.trim().length > 0,
  })

  const rollbackMutation = useMutation({
    mutationFn: (revision: number) => client.rollbackReconciliation(sourceId, { revision }),
    onSuccess: () => {
      setError(null)
      setRollbackTarget(null)
      queryClient.invalidateQueries({ queryKey: ['reconciliation', 'revisions', sourceId] })
      queryClient.invalidateQueries({ queryKey: ['epg-mappings'] })
      queryClient.invalidateQueries({ queryKey: ['epg-unmapped'] })
      queryClient.invalidateQueries({ queryKey: ['epg-review'] })
      queryClient.invalidateQueries({ queryKey: ['overview'] })
    },
    onError: (failure: unknown) => {
      if (failure instanceof IptvApiError) {
        setError(failure.problem.detail ?? failure.problem.title)
      } else {
        setError('The rollback failed. Try again.')
      }
    },
  })

  const revisions: ReconciliationRevision[] = revisionsQuery.data ?? []

  return (
    <Card>
      <CardHeader>
        <div>
          <p className="text-lg font-semibold text-white">Reconciliation rollback</p>
          <p className="mt-1 text-xs text-slate-500">
            Restore channels, streams, and EPG mappings for one source to a prior reconciliation revision.
          </p>
        </div>
        <History aria-hidden="true" className="size-5 text-slate-500" />
      </CardHeader>
      <CardContent className="space-y-4">
        <label className="block max-w-md">
          <span className="sr-only">Source id</span>
          <Input
            aria-label="Source id"
            value={sourceId}
            onChange={(event) => { setSourceId(event.target.value); setError(null) }}
            placeholder="Enter a source id to load reconciliation history"
          />
        </label>
        {sourceId.trim().length === 0 ? (
          <p className="text-sm text-slate-500">Enter a source id to load its reconciliation revision history.</p>
        ) : revisionsQuery.isLoading ? (
          <p role="status" className="text-sm text-slate-500">Loading revisions…</p>
        ) : revisions.length === 0 ? (
          <p className="text-sm text-slate-500">No reconciliation revisions recorded for this source.</p>
        ) : (
          <div className="space-y-2">
            {revisions.map((revision) => (
              <div
                key={revision.revision}
                className="flex items-center justify-between rounded-lg border border-white/5 bg-ink-900/50 px-4 py-3"
              >
                <div>
                  <p className="text-sm font-medium text-white">Revision {revision.revision}</p>
                  <p className="text-xs text-slate-500">
                    {revision.actor} · {new Date(revision.createdAt).toLocaleString()}
                  </p>
                </div>
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={rollbackMutation.isPending && rollbackTarget === revision.revision}
                  onClick={() => {
                    setRollbackTarget(revision.revision)
                    rollbackMutation.mutate(revision.revision)
                  }}
                >
                  <RotateCcw aria-hidden="true" className="size-4" />
                  Roll back
                </Button>
              </div>
            ))}
          </div>
        )}
        {rollbackMutation.data ? (
          <p role="status" className="text-xs text-mint-400">
            Restored {rollbackMutation.data.channelsRestored} channel(s), {rollbackMutation.data.streamLinksRestored} stream link(s), and {rollbackMutation.data.epgMappingsRestored} EPG mapping(s) to revision {rollbackMutation.data.targetRevision}.
          </p>
        ) : null}
        {error ? <p role="alert" className="text-sm text-red-400">{error}</p> : null}
      </CardContent>
    </Card>
  )
}
