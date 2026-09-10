import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  createColumnHelper,
  createCoreRowModel,
  createSortedRowModel,
  flexRender,
  rowSortingFeature,
  sortFn_alphanumeric,
  sortFn_basic,
  tableFeatures,
  useTable,
  type SortingState,
} from '@tanstack/react-table'
import { ArrowUpDown, ChevronLeft, ChevronRight, Eye, Power, Search } from 'lucide-react'
import { useMemo, useState } from 'react'
import { HealthBadge } from '@/components/health-badge'
import { LoadingPage } from '@/components/loading-page'
import { PageHeader } from '@/components/page-header'
import { StreamPreview } from '@/components/stream-preview'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Combobox } from '@/components/ui/combobox'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import { useDebouncedValue } from '@/lib/hooks/use-debounced-value'
import type { Channel, ChannelQuery, IptvApiClient } from '@/lib/api/types'
import { formatBitrate } from '@/lib/utils'

const channelTableFeatures = tableFeatures({
  rowSortingFeature,
  coreRowModel: createCoreRowModel(),
  sortedRowModel: createSortedRowModel(),
  sortFns: { alphanumeric: sortFn_alphanumeric, basic: sortFn_basic },
})

const columnHelper = createColumnHelper<typeof channelTableFeatures, Channel>()

const PAGE_SIZE = 50

export function ChannelsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const [searchInput, setSearchInput] = useState('')
  const [groupFilter, setGroupFilter] = useState('')
  const [enabledFilter, setEnabledFilter] = useState<string>('')
  const [page, setPage] = useState(0)
  const debouncedSearch = useDebouncedValue(searchInput, 300)
  const queryClient = useQueryClient()
  const channelQuery: ChannelQuery = {
    limit: PAGE_SIZE,
    offset: page * PAGE_SIZE,
    ...(debouncedSearch ? { search: debouncedSearch } : {}),
    ...(groupFilter ? { group: groupFilter } : {}),
    ...(enabledFilter === 'enabled' ? { enabled: true } : {}),
    ...(enabledFilter === 'disabled' ? { enabled: false } : {}),
  }
  const query = useQuery({
    ...apiQueries(client).channels(channelQuery),
    placeholderData: keepPreviousData,
  })
  const groupsQuery = useQuery({ ...apiQueries(client).groups })
  const channels = query.data?.items ?? []
  const total = query.data?.total ?? 0
  const pageCount = Math.ceil(total / PAGE_SIZE)
  const [sorting, setSorting] = useState<SortingState>([{ id: 'number', desc: false }])
  const [previewChannels, setPreviewChannels] = useState<{ id: string; name: string }[]>([])

  const channelToggle = useMutation({
    mutationFn: ({ channelId, enabled }: { channelId: string; enabled: boolean }) =>
      client.setChannelEnabled(channelId, enabled),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['channels'] })
      queryClient.invalidateQueries({ queryKey: ['groups'] })
      queryClient.invalidateQueries({ queryKey: ['overview'] })
    },
  })

  const groupToggle = useMutation({
    mutationFn: ({ groupName, enabled }: { groupName: string; enabled: boolean }) =>
      client.setGroupEnabled(groupName, enabled),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['channels'] })
      queryClient.invalidateQueries({ queryKey: ['groups'] })
      queryClient.invalidateQueries({ queryKey: ['overview'] })
    },
  })

  const columns = useMemo(() => columnHelper.columns([
    columnHelper.accessor('number', { header: 'No.', cell: (info) => <span className="font-mono text-slate-400">{info.getValue()}</span> }),
    columnHelper.accessor('name', { header: 'Channel', cell: (info) => <div><p className="font-medium text-white">{info.getValue()}</p><p className="mt-1 font-mono text-[0.68rem] text-slate-600">{info.row.original.tvgId}</p></div> }),
    columnHelper.accessor('group', { header: 'Group', cell: (info) => <Badge>{info.getValue()}</Badge> }),
    columnHelper.accessor('streams', { header: 'Streams', cell: (info) => `${info.getValue()} alternates` }),
    columnHelper.accessor('primaryCodec', { header: 'Primary', cell: (info) => <div><p>{info.getValue()}</p><p className="text-xs text-slate-500">{formatBitrate(info.row.original.bitrateKbps)}</p></div> }),
    columnHelper.accessor('state', { header: 'Status', cell: (info) => <HealthBadge state={info.getValue()} /> }),
    columnHelper.accessor('enabled', { header: 'Enabled', cell: (info) => (
      <Button
        variant={info.getValue() ? 'ghost' : 'secondary'}
        size="sm"
        disabled={channelToggle.isPending}
        aria-label={info.getValue() ? `Disable ${info.row.original.name}` : `Enable ${info.row.original.name}`}
        onClick={() => channelToggle.mutate({ channelId: info.row.original.id, enabled: !info.getValue() })}
      >
        <Power aria-hidden="true" className={`size-4 ${info.getValue() ? 'text-emerald-400' : 'text-slate-500'}`} />
        {info.getValue() ? 'On' : 'Off'}
      </Button>
    ) }),
    columnHelper.display({ id: 'preview', header: 'Preview', cell: (info) => (
      <Button
        variant="ghost"
        size="sm"
        disabled={info.row.original.state === 'offline' || !info.row.original.enabled}
        aria-label={`Preview ${info.row.original.name}`}
        onClick={() => setPreviewChannels((current) => current.some((channel) => channel.id === info.row.original.id)
          ? current
          : [...current, { id: info.row.original.id, name: info.row.original.name }])}
      >
        <Eye aria-hidden="true" className="size-4" />
      </Button>
    ) }),
  ]), [channelToggle])
  const table = useTable({
    features: channelTableFeatures,
    data: channels,
    columns,
    state: { sorting },
    onSortingChange: setSorting,
  })

  if (!query.data) return <LoadingPage label="channels" />

  return (
    <>
      <PageHeader eyebrow="Lineup" title="Channels" description="Inspect normalized channel identity, alternate streams, groups, codecs, and health-derived primary ordering. Toggle channels and groups to control Jellyfin output." />
      {previewChannels.map((channel) => (
        <StreamPreview
          key={channel.id}
          channelId={channel.id}
          channelName={channel.name}
          client={client}
          onClose={() => setPreviewChannels((current) => current.filter((item) => item.id !== channel.id))}
        />
      ))}
      <Card>
        <div className="flex flex-col justify-between gap-3 border-b border-white/8 p-4 sm:flex-row sm:items-center">
          <div className="flex flex-1 flex-col gap-2 sm:flex-row sm:items-center">
            <label className="relative block max-w-sm flex-1">
              <span className="sr-only">Search channels</span>
              <Search aria-hidden="true" className="pointer-events-none absolute left-3 top-3 size-4 text-slate-500" />
              <Input className="pl-9" value={searchInput} onChange={(event) => { setSearchInput(event.target.value); setPage(0) }} placeholder="Search by channel or group" />
            </label>
            <div className="sm:w-64">
              <Combobox
                ariaLabel="Filter by group"
                placeholder="All groups"
                searchPlaceholder="Search groups…"
                emptyText="No groups found."
                value={groupFilter}
                onChange={(value) => { setGroupFilter(value); setPage(0) }}
                items={[
                  { value: '', label: 'All groups' },
                  ...(groupsQuery.data?.map((group) => ({
                    value: group.name,
                    label: group.name,
                    hint: `${group.channelCount.toLocaleString()} channels`,
                  })) ?? []),
                ]}
              />
            </div>
            <div className="sm:w-40">
              <Combobox
                ariaLabel="Filter by enabled state"
                placeholder="All states"
                searchPlaceholder="Search states…"
                emptyText="No states found."
                value={enabledFilter}
                onChange={(value) => { setEnabledFilter(value); setPage(0) }}
                items={[
                  { value: '', label: 'All states' },
                  { value: 'enabled', label: 'Enabled' },
                  { value: 'disabled', label: 'Disabled' },
                ]}
              />
            </div>
            {groupFilter && (
              <div className="flex gap-2">
                <Button variant="ghost" size="sm" disabled={groupToggle.isPending} onClick={() => groupToggle.mutate({ groupName: groupFilter, enabled: true })}>
                  Enable all
                </Button>
                <Button variant="secondary" size="sm" disabled={groupToggle.isPending} onClick={() => groupToggle.mutate({ groupName: groupFilter, enabled: false })}>
                  Disable all
                </Button>
              </div>
            )}
          </div>
          <p role="status" className="text-xs text-slate-500">
            {query.isFetching ? 'Searching…' : `${total.toLocaleString()} channels`}
          </p>
        </div>
        <div className="overflow-x-auto">
          <Table>
            <caption className="sr-only">Published channel lineup</caption>
            <TableHeader>
              {table.getHeaderGroups().map((headerGroup) => (
                <TableRow key={headerGroup.id}>
                  {headerGroup.headers.map((header) => (
                    <TableHead key={header.id}>
                      {header.isPlaceholder ? null : (
                        <button className="inline-flex items-center gap-1.5" onClick={header.column.getToggleSortingHandler()} disabled={!header.column.getCanSort()}>
                          {flexRender(header.column.columnDef.header, header.getContext())}
                          {header.column.getCanSort() ? <ArrowUpDown aria-hidden="true" className="size-3" /> : null}
                        </button>
                      )}
                    </TableHead>
                  ))}
                </TableRow>
              ))}
            </TableHeader>
            <TableBody>
              {table.getRowModel().rows.length === 0
                ? <TableRow><TableCell colSpan={8} className="py-8 text-center text-slate-500">No channels found.</TableCell></TableRow>
                : table.getRowModel().rows.map((row) => (
                  <TableRow key={row.id}>{row.getAllCells().map((cell) => <TableCell key={cell.id}>{flexRender(cell.column.columnDef.cell, cell.getContext())}</TableCell>)}</TableRow>
                ))
              }
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
