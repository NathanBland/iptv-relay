import { keepPreviousData, useQuery, useQueryClient } from '@tanstack/react-query'
import { Activity, AlertCircle, CheckCircle2, HelpCircle, Loader2, Radio, Zap } from 'lucide-react'
import { useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { IptvApiClient } from '@/lib/api/types'

const statusConfig = {
  alive: { icon: CheckCircle2, color: 'text-mint-400', tone: 'success' as const, label: 'Alive' },
  dead: { icon: AlertCircle, color: 'text-red-400', tone: 'danger' as const, label: 'Dead' },
  unknown: { icon: HelpCircle, color: 'text-slate-400', tone: 'neutral' as const, label: 'Unknown' },
  checking: { icon: Loader2, color: 'text-ocean-400', tone: 'info' as const, label: 'Checking' },
} as const

export function StreamHealthPage({ client = apiClient }: { client?: IptvApiClient }) {
  const [statusFilter, setStatusFilter] = useState<string>('')
  const [groupFilter, setGroupFilter] = useState<string>('')
  const queryClient = useQueryClient()

  const statsQuery = useQuery(apiQueries(client).streamHealthStats())
  const healthQuery = useQuery({
    ...apiQueries(client).streamHealth(statusFilter || undefined, groupFilter || undefined, 100),
    placeholderData: keepPreviousData,
  })

  const stats = statsQuery.data
  const streams = healthQuery.data?.items ?? []
  const total = healthQuery.data?.total ?? 0

  if (!healthQuery.data) return <LoadingPage label="stream health" />

  async function triggerCheck() {
    await client.triggerHealthCheck(50)
    await queryClient.invalidateQueries({ queryKey: ['stream-health'] })
    await queryClient.invalidateQueries({ queryKey: ['stream-health-stats'] })
  }

  async function rankStreams() {
    await client.rankAllStreams()
    await queryClient.invalidateQueries({ queryKey: ['stream-health'] })
  }

  return (
    <div className="space-y-6">
      <PageHeader eyebrow="Streams" title="Stream Health" description="Monitor provider stream status and quality rankings." />
      <div className="flex flex-wrap items-center gap-3">
        <Button onClick={triggerCheck} disabled={statsQuery.isFetching}>
          <Radio className="size-4" />
          Check streams
        </Button>
        <Button onClick={rankStreams} variant="secondary">
          <Zap className="size-4" />
          Rank by quality
        </Button>
      </div>

      {stats && (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <Card>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <p className="text-sm font-medium text-slate-400">Alive</p>
              <CheckCircle2 className="size-4 text-mint-400" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-white">{stats.alive}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <p className="text-sm font-medium text-slate-400">Dead</p>
              <AlertCircle className="size-4 text-red-400" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-white">{stats.dead}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <p className="text-sm font-medium text-slate-400">Unknown</p>
              <HelpCircle className="size-4 text-slate-400" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-white">{stats.unknown}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <p className="text-sm font-medium text-slate-400">Checking</p>
              <Activity className="size-4 text-ocean-400" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-white">{stats.checking}</div>
            </CardContent>
          </Card>
        </div>
      )}

      <Card>
        <CardHeader>
          <p className="text-lg font-semibold text-white">Provider streams ({total})</p>
        </CardHeader>
        <CardContent>
          <div className="mb-4 flex flex-wrap gap-3">
            <Input
              placeholder="Filter by status (alive, dead, unknown)"
              value={statusFilter}
              onChange={(e) => setStatusFilter(e.target.value)}
              className="max-w-xs"
            />
            <Input
              placeholder="Filter by group"
              value={groupFilter}
              onChange={(e) => setGroupFilter(e.target.value)}
              className="max-w-xs"
            />
          </div>
          <div className="space-y-2">
            {streams.length === 0 && (
              <p className="py-8 text-center text-sm text-slate-500">No streams match the current filters.</p>
            )}
            {streams.map((stream) => {
              const config = statusConfig[stream.healthStatus] ?? statusConfig.unknown
              const StatusIcon = config.icon
              return (
                <div
                  key={stream.providerStreamId}
                  className="flex items-center justify-between rounded-lg border border-white/5 bg-ink-900/50 px-4 py-3"
                >
                  <div className="flex items-center gap-3">
                    <StatusIcon className={`size-4 ${config.color}`} />
                    <div>
                      <p className="text-sm font-medium text-white">{stream.streamName}</p>
                      <p className="text-xs text-slate-500">
                        {stream.groupName ?? 'No group'}
                        {stream.videoWidth && stream.videoHeight
                          ? ` · ${stream.videoWidth}x${stream.videoHeight}`
                          : ''}
                        {stream.videoFps ? ` @ ${stream.videoFps}fps` : ''}
                        {stream.videoCodec ? ` · ${stream.videoCodec}` : ''}
                      </p>
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    <Badge tone={config.tone}>
                      {config.label}
                    </Badge>
                    {stream.bitrateKbps && (
                      <span className="text-xs text-slate-500">{stream.bitrateKbps} kbps</span>
                    )}
                  </div>
                </div>
              )
            })}
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
