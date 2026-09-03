import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { CalendarClock, Search, Tv } from 'lucide-react'
import { useMemo, useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import { useDebouncedValue } from '@/lib/hooks/use-debounced-value'
import { formatProgrammeTime, partitionProgrammesAt } from '@/lib/programmes'
import type { IptvApiClient } from '@/lib/api/types'

export function TvGuidePage({ client = apiClient }: { client?: IptvApiClient }) {
  const [searchInput, setSearchInput] = useState('')
  const debouncedSearch = useDebouncedValue(searchInput, 300)
  const programmeQuery = {
    limit: 200,
    ...(debouncedSearch ? { search: debouncedSearch } : {}),
  }
  const query = useQuery({
    ...apiQueries(client).programmes(programmeQuery),
    placeholderData: keepPreviousData,
  })
  const regionQuery = useQuery(apiQueries(client).regionSettings)
  const enabledChannelsQuery = useQuery({
    ...apiQueries(client).channels({ enabled: true, limit: 500 }),
  })
  const programmes = query.data?.items ?? []
  const total = query.data?.total ?? 0
  const enabledChannels = enabledChannelsQuery.data?.items ?? []
  const timezone = regionQuery.data?.settings.timezone ?? Intl.DateTimeFormat().resolvedOptions().timeZone

  const { current, upcoming } = useMemo(() => {
    return partitionProgrammesAt(programmes, Date.now())
  }, [programmes])

  const channelsWithoutEpg = useMemo(() => {
    const names = new Set(programmes.map((p) => p.channel))
    const filtered = enabledChannels.filter((c) => !names.has(c.name))
    if (!debouncedSearch) return filtered
    const lower = debouncedSearch.toLowerCase()
    return filtered.filter((c) => c.name.toLowerCase().includes(lower))
  }, [enabledChannels, programmes, debouncedSearch])

  if (!query.data) return <LoadingPage label="tv guide" />

  return (
    <>
      <PageHeader eyebrow="Live" title="TV Guide" description="See what is currently playing across enabled channels with EPG data. Search filters channel names and programme titles server-side." />
      <section className="space-y-4">
        <Card>
          <CardHeader className="items-center">
            <label className="relative block w-full max-w-sm">
              <span className="sr-only">Search TV guide</span>
              <Search aria-hidden="true" className="absolute left-3 top-3 size-4 text-slate-500" />
              <Input className="pl-9" value={searchInput} onChange={(event) => setSearchInput(event.target.value)} placeholder="Search channel or programme" />
            </label>
            <Badge tone="success">{total.toLocaleString()} programmes</Badge>
          </CardHeader>
          <CardContent className="space-y-6 p-0">
            <div className="border-b border-white/8 p-4">
              <div className="mb-3 flex items-center gap-2">
                <Tv aria-hidden="true" className="size-4 text-ocean-400" />
                <h2 className="text-sm font-semibold text-white">On now</h2>
                <Badge tone="success">{current.length}</Badge>
              </div>
              {current.length === 0
                ? <p className="text-sm text-slate-500">No programmes are currently airing.</p>
                : <ul className="space-y-2">
                  {current.slice(0, 50).map((programme) => (
                    <li key={programme.id} className="flex items-center gap-3 rounded-lg border border-white/6 bg-white/[0.02] px-3 py-2">
                      <span aria-hidden="true" className="size-2 shrink-0 rounded-full bg-rose-400 shadow-[0_0_8px_rgba(251,113,133,.6)]" />
                      <div className="min-w-0 flex-1">
                        <p className="truncate text-sm font-medium text-white">{programme.title}</p>
                        <p className="truncate text-xs text-slate-500">{programme.channel}</p>
                      </div>
                      <time className="shrink-0 font-mono text-xs text-slate-500" dateTime={programme.start}>
                        {formatProgrammeTime(programme.start, timezone)}
                      </time>
                    </li>
                  ))}
                </ul>
              }
            </div>
            <div className="p-4">
              <div className="mb-3 flex items-center gap-2">
                <CalendarClock aria-hidden="true" className="size-4 text-ocean-400" />
                <h2 className="text-sm font-semibold text-white">Up next</h2>
                <Badge>{upcoming.length}</Badge>
              </div>
              {upcoming.length === 0
                ? <p className="text-sm text-slate-500">No upcoming programmes found.</p>
                : <ul className="space-y-2">
                  {upcoming.slice(0, 50).map((programme) => (
                    <li key={programme.id} className="flex items-center gap-3 rounded-lg border border-white/6 bg-white/[0.02] px-3 py-2">
                      <div className="min-w-0 flex-1">
                        <p className="truncate text-sm font-medium text-white">{programme.title}</p>
                        <p className="truncate text-xs text-slate-500">{programme.channel}</p>
                      </div>
                      <time className="shrink-0 font-mono text-xs text-slate-500" dateTime={programme.start}>
                        {formatProgrammeTime(programme.start, timezone)}
                      </time>
                    </li>
                  ))}
                </ul>
              }
            </div>
            {channelsWithoutEpg.length > 0 && (
              <div className="border-t border-white/8 p-4">
                <div className="mb-3 flex items-center gap-2">
                  <Tv aria-hidden="true" className="size-4 text-slate-500" />
                  <h2 className="text-sm font-semibold text-white">Enabled channels without guide data</h2>
                  <Badge tone="neutral">{channelsWithoutEpg.length}</Badge>
                </div>
                <ul className="flex flex-wrap gap-2">
                  {channelsWithoutEpg.slice(0, 200).map((channel) => (
                    <li key={channel.id} className="rounded-lg border border-white/6 bg-white/[0.02] px-2.5 py-1.5">
                      <p className="truncate text-xs font-medium text-slate-300">{channel.name}</p>
                      <p className="truncate text-[0.68rem] text-slate-600">{channel.group}</p>
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </CardContent>
        </Card>
      </section>
    </>
  )
}
