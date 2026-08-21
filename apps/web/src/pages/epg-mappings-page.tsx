import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, ChevronLeft, ChevronRight, Link2, RefreshCw, Search, Unlink, X } from 'lucide-react'
import { useState } from 'react'
import { LoadingPage } from '@/components/loading-page'
import { PageHeader } from '@/components/page-header'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { apiClient } from '@/lib/api/client'
import type { EpgMapping, IptvApiClient } from '@/lib/api/types'

const PAGE_SIZE = 50

type Tab = 'mapped' | 'review' | 'unmapped'

export function EpgMappingsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const [tab, setTab] = useState<Tab>('mapped')
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(0)
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

  const removeMapping = useMutation({
    mutationFn: (channelId: string) => client.removeChannelEpgMapping(channelId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['epg-mappings'] })
      queryClient.invalidateQueries({ queryKey: ['epg-unmapped'] })
    },
  })

  if (tab === 'mapped' && mappingsQuery.isLoading) return <LoadingPage label="EPG mappings" />
  if (tab === 'review' && mappingsQuery.isLoading) return <LoadingPage label="review queue" />
  if (tab === 'unmapped' && unmappedQuery.isLoading) return <LoadingPage label="unmapped channels" />

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

      <Card>
        <div className="flex flex-col gap-3 border-b border-white/8 p-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex gap-2">
            {(['mapped', 'review', 'unmapped'] as const).map((t) => (
              <Button
                key={t}
                variant={tab === t ? 'primary' : 'ghost'}
                size="sm"
                onClick={() => { setTab(t); setPage(0) }}
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

        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Channel</TableHead>
                <TableHead>EPG match</TableHead>
                <TableHead>Method</TableHead>
                <TableHead>Confidence</TableHead>
                {tab === 'review' ? <TableHead>Actions</TableHead> : <TableHead className="text-right">Actions</TableHead>}
              </TableRow>
            </TableHeader>
            <TableBody>
              {tab === 'unmapped' ? (
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
                  <TableRow><TableCell colSpan={5} className="py-8 text-center text-slate-500">
                    {tab === 'review' ? 'No channels need review.' : 'No mappings found.'}
                  </TableCell></TableRow>
                ) : (
                  mappingsQuery.data?.items.map((mapping) => (
                    <MappingRow
                      key={mapping.channelId}
                      mapping={mapping}
                      showReviewActions={tab === 'review'}
                      client={client}
                      onRemove={() => removeMapping.mutate(mapping.channelId)}
                      onResolved={() => {
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
              <Button variant="ghost" size="sm" disabled={page === 0} onClick={() => setPage((p) => Math.max(0, p - 1))}>
                <ChevronLeft aria-hidden="true" className="size-4" /> Prev
              </Button>
              <Button variant="ghost" size="sm" disabled={page >= pageCount - 1} onClick={() => setPage((p) => p + 1)}>
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

function MappingRow({
  mapping,
  showReviewActions,
  client,
  onRemove,
  onResolved,
}: {
  mapping: EpgMapping
  showReviewActions: boolean
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
      <TableCell><Badge tone="neutral">{mapping.method}</Badge></TableCell>
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

  if (!candidatesQuery.data) return <TableRow><TableCell colSpan={5} className="text-center text-slate-500">Loading candidates…</TableCell></TableRow>

  return (
    <TableRow>
      <TableCell colSpan={5}>
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
