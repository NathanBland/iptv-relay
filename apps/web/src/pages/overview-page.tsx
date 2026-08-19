import { useQuery } from '@tanstack/react-query'
import { Activity, CalendarCheck2, RadioTower, Tv2, Users } from 'lucide-react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { apiQueries } from '@/lib/api/queries'
import type { IptvApiClient } from '@/lib/api/types'
import { apiClient } from '@/lib/api/client'

export function OverviewPage({ client = apiClient }: { client?: IptvApiClient }) {
  const query = useQuery({ ...apiQueries(client).overview })
  if (query.isError) return <p role="alert" className="rounded-lg border border-red-400/20 bg-red-400/8 p-4 text-sm text-red-200">The system overview is not available. Try again.</p>
  if (!query.data) return <LoadingPage label="system overview" />
  const overview = query.data
  const connectionPercent = overview.providerLimit > 0
    ? Math.min(100, Math.round((overview.providerConnections / overview.providerLimit) * 100))
    : 0

  const stats = [
    { label: 'Published channels', value: overview.channels.toLocaleString(), icon: Tv2 },
    { label: 'Healthy streams', value: overview.healthyStreams.toLocaleString(), icon: Activity },
    { label: 'Active viewers', value: overview.activeSessions.toString(), icon: Users },
    { label: 'Guide coverage', value: `${overview.guideCoverage.toFixed(1)}%`, icon: CalendarCheck2 },
  ]

  return (
    <>
      <PageHeader
        eyebrow="Operations"
        title="Overview"
        description="A live view of lineup health, guide coverage, and shared upstream capacity."
      />

      <section aria-label="System summary" className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        {stats.map(({ label, value, icon: Icon }) => (
          <Card key={label}>
            <CardContent className="flex items-start justify-between">
              <div>
                <p className="text-xs font-medium text-slate-500">{label}</p>
                <p className="mt-2 text-2xl font-semibold tracking-tight text-white">{value}</p>
              </div>
              <span className="grid size-9 place-items-center rounded-lg bg-ocean-400/10 text-ocean-400">
                <Icon aria-hidden="true" className="size-4" />
              </span>
            </CardContent>
          </Card>
        ))}
      </section>

      <section className="mt-4 grid gap-4 xl:grid-cols-[1fr_1fr]">
        <Card>
          <CardHeader>
            <div>
              <h2 className="font-semibold text-white">Provider connection budget</h2>
              <p className="mt-1 text-xs text-slate-500">Live media connections only</p>
            </div>
            <RadioTower aria-hidden="true" className="size-5 text-ocean-400" />
          </CardHeader>
          <CardContent>
            <div className="flex items-end justify-between">
              <p className="text-4xl font-semibold tracking-tight text-white">
                {overview.providerConnections}<span className="text-lg text-slate-500"> / {overview.providerLimit}</span>
              </p>
              <p className="text-xs text-slate-400">{overview.activeSessions} downstream sessions</p>
            </div>
            <div className="mt-6 h-2 overflow-hidden rounded-full bg-white/8">
              <div className="h-full rounded-full bg-ocean-400" style={{ width: `${connectionPercent}%` }} />
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div>
              <h2 className="font-semibold text-white">Guide coverage</h2>
              <p className="mt-1 text-xs text-slate-500">Channels with imported programme data</p>
            </div>
            <CalendarCheck2 aria-hidden="true" className="size-5 text-ocean-400" />
          </CardHeader>
          <CardContent>
            <p className="text-4xl font-semibold tracking-tight text-white">{overview.guideCoverage.toFixed(1)}%</p>
            <div className="mt-6 h-2 overflow-hidden rounded-full bg-white/8">
              <div className="h-full rounded-full bg-ocean-400" style={{ width: `${Math.min(100, overview.guideCoverage)}%` }} />
            </div>
          </CardContent>
        </Card>
      </section>
    </>
  )
}
