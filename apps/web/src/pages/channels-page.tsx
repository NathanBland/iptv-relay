import { useQuery } from '@tanstack/react-query'
import {
  columnFilteringFeature,
  createColumnHelper,
  createCoreRowModel,
  createFilteredRowModel,
  createSortedRowModel,
  filterFn_includesString,
  flexRender,
  globalFilteringFeature,
  rowSortingFeature,
  sortFn_alphanumeric,
  sortFn_basic,
  tableFeatures,
  useTable,
  type SortingState,
} from '@tanstack/react-table'
import { ArrowUpDown, Search } from 'lucide-react'
import { useMemo, useState } from 'react'
import { HealthBadge } from '@/components/health-badge'
import { LoadingPage } from '@/components/loading-page'
import { PageHeader } from '@/components/page-header'
import { Badge } from '@/components/ui/badge'
import { Card } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { Channel, IptvApiClient } from '@/lib/api/types'
import { formatBitrate } from '@/lib/utils'

const channelTableFeatures = tableFeatures({
  columnFilteringFeature,
  globalFilteringFeature,
  rowSortingFeature,
  coreRowModel: createCoreRowModel(),
  filteredRowModel: createFilteredRowModel(),
  sortedRowModel: createSortedRowModel(),
  filterFns: { includesString: filterFn_includesString },
  sortFns: { alphanumeric: sortFn_alphanumeric, basic: sortFn_basic },
})

const columnHelper = createColumnHelper<typeof channelTableFeatures, Channel>()

export function ChannelsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const query = useQuery({ ...apiQueries(client).channels({ limit: 500 }) })
  const channels = query.data?.items ?? []
  const total = query.data?.total ?? 0
  const [sorting, setSorting] = useState<SortingState>([{ id: 'number', desc: false }])
  const [filter, setFilter] = useState('')
  const columns = useMemo(() => columnHelper.columns([
    columnHelper.accessor('number', { header: 'No.', cell: (info) => <span className="font-mono text-slate-400">{info.getValue()}</span> }),
    columnHelper.accessor('name', { header: 'Channel', cell: (info) => <div><p className="font-medium text-white">{info.getValue()}</p><p className="mt-1 font-mono text-[0.68rem] text-slate-600">{info.row.original.tvgId}</p></div> }),
    columnHelper.accessor('group', { header: 'Group', cell: (info) => <Badge>{info.getValue()}</Badge> }),
    columnHelper.accessor('streams', { header: 'Streams', cell: (info) => `${info.getValue()} alternates` }),
    columnHelper.accessor('primaryCodec', { header: 'Primary', cell: (info) => <div><p>{info.getValue()}</p><p className="text-xs text-slate-500">{formatBitrate(info.row.original.bitrateKbps)}</p></div> }),
    columnHelper.accessor('state', { header: 'Status', cell: (info) => <HealthBadge state={info.getValue()} /> }),
  ]), [])
  const table = useTable({
    features: channelTableFeatures,
    data: channels,
    columns,
    state: { sorting, globalFilter: filter },
    onSortingChange: setSorting,
    onGlobalFilterChange: setFilter,
    globalFilterFn: 'includesString',
  })

  if (!query.data) return <LoadingPage label="channels" />

  return (
    <>
      <PageHeader eyebrow="Lineup" title="Channels" description="Inspect normalized channel identity, alternate streams, groups, codecs, and health-derived primary ordering." />
      <Card>
        <div className="flex flex-col justify-between gap-3 border-b border-white/8 p-4 sm:flex-row sm:items-center">
          <label className="relative block max-w-sm flex-1">
            <span className="sr-only">Search channels</span>
            <Search aria-hidden="true" className="pointer-events-none absolute left-3 top-3 size-4 text-slate-500" />
            <Input className="pl-9" value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="Search by channel or group" />
          </label>
          <p role="status" className="text-xs text-slate-500">{table.getRowModel().rows.length} of {total.toLocaleString()} channels</p>
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
                ? <TableRow><TableCell colSpan={6} className="py-8 text-center text-slate-500">No channels found.</TableCell></TableRow>
                : table.getRowModel().rows.map((row) => (
                  <TableRow key={row.id}>{row.getAllCells().map((cell) => <TableCell key={cell.id}>{flexRender(cell.column.columnDef.cell, cell.getContext())}</TableCell>)}</TableRow>
                ))
              }
            </TableBody>
          </Table>
        </div>
      </Card>
    </>
  )
}
