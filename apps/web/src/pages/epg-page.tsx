import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { useVirtualizer } from '@tanstack/react-virtual'
import { CalendarDays, ChevronLeft, ChevronRight, Search } from 'lucide-react'
import { useRef, useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import { useDebouncedValue } from '@/lib/hooks/use-debounced-value'
import type { IptvApiClient } from '@/lib/api/types'

const PAGE_SIZE = 100

const timeFormatter = new Intl.DateTimeFormat('en-US', { hour: 'numeric', minute: '2-digit', timeZone: 'America/Denver' })

export function EpgPage({ client = apiClient }: { client?: IptvApiClient }) {
  const [searchInput, setSearchInput] = useState('')
  const [page, setPage] = useState(0)
  const debouncedSearch = useDebouncedValue(searchInput, 300)
  const programmeQuery = {
    limit: PAGE_SIZE,
    offset: page * PAGE_SIZE,
    ...(debouncedSearch ? { search: debouncedSearch } : {}),
  }
  const query = useQuery({
    ...apiQueries(client).programmes(programmeQuery),
    placeholderData: keepPreviousData,
  })
  const programmes = query.data?.items ?? []
  const total = query.data?.total ?? 0
  const pageCount = Math.ceil(total / PAGE_SIZE)
  const scrollRef = useRef<HTMLDivElement>(null)
  const virtualizer = useVirtualizer({
    count: programmes.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 76,
    overscan: 5,
    initialRect: { width: 960, height: 456 },
  })
  const measuredItems = virtualizer.getVirtualItems()
  const virtualItems = measuredItems.length > 0
    ? measuredItems
    : programmes.slice(0, 11).map((programme, index) => ({ key: programme.id, index, size: 76, start: index * 76 }))

  if (!query.data) return <LoadingPage label="programme guide" />

  return (
    <>
      <PageHeader eyebrow="Guide" title="EPG" description="Search mapped programme slots, XMLTV source confidence, and upcoming guide coverage. Server-side search queries the database." />
      <section className="grid gap-4 xl:grid-cols-[1fr_18rem]">
        <Card>
          <CardHeader className="items-center">
            <div className="flex w-full max-w-sm items-center gap-2">
              <label className="relative block flex-1">
                <span className="sr-only">Search programme guide</span>
                <Search aria-hidden="true" className="absolute left-3 top-3 size-4 text-slate-500" />
                <Input className="pl-9" value={searchInput} onChange={(event) => { setSearchInput(event.target.value); setPage(0) }} placeholder="Search channel or title" />
              </label>
            </div>
            <Badge tone="success">{total.toLocaleString()} slots</Badge>
          </CardHeader>
          <CardContent className="p-0">
            <div ref={scrollRef} className="h-[28.5rem] overflow-auto" tabIndex={0} aria-label="Scrollable programme schedule">
              {programmes.length === 0
                ? <p className="p-8 text-center text-sm text-slate-500">No programme data available.</p>
                : <ol className="relative m-0 list-none p-0" style={{ height: `${virtualizer.getTotalSize()}px` }}>
                  {virtualItems.map((virtualItem) => {
                    const programme = programmes[virtualItem.index]
                    if (!programme) return null
                    return (
                      <li
                        key={programme.id}
                        className="absolute left-0 top-0 grid w-full grid-cols-[5rem_1fr_auto] items-center gap-3 border-b border-white/7 px-4 py-3"
                        style={{ height: `${virtualItem.size}px`, transform: `translateY(${virtualItem.start}px)` }}
                      >
                        <time className="font-mono text-xs text-slate-500" dateTime={programme.start}>{timeFormatter.format(new Date(programme.start))}</time>
                        <div className="min-w-0"><p className="truncate text-sm font-medium text-white">{programme.title}</p><p className="truncate text-xs text-slate-500">{programme.channel} · {programme.source}</p></div>
                        <Badge tone={programme.confidence >= 95 ? 'success' : 'warning'}>{programme.confidence}%</Badge>
                      </li>
                    )
                  })}
                </ol>
              }
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
          </CardContent>
        </Card>
        <Card>
          <CardHeader><h2 className="font-semibold text-white">Guide health</h2><CalendarDays aria-hidden="true" className="size-5 text-ocean-400" /></CardHeader>
          <CardContent className="space-y-5">
            <div><p className="text-3xl font-semibold text-white">{total.toLocaleString()}</p><p className="mt-1 text-xs text-slate-500">Imported programme slots</p></div>
          </CardContent>
        </Card>
      </section>
    </>
  )
}
