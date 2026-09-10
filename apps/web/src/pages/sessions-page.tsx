import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Activity, Network, Power, Users } from 'lucide-react'
import { HealthBadge } from '@/components/health-badge'
import { LoadingPage } from '@/components/loading-page'
import { PageHeader } from '@/components/page-header'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { HealthState, IptvApiClient, Session, SessionFailure } from '@/lib/api/types'

const failureLabels: Record<SessionFailure, string> = {
  'upstream-ended': 'Upstream ended',
  http: 'HTTP',
  packetization: 'Packetization',
  priming: 'Priming',
  'recovery-expired': 'Recovery expired',
}

export function sessionHealth(state: Session['state']): HealthState {
  if (state === 'streaming') return 'healthy'
  if (state === 'recovering' || state === 'failing-over') return 'degraded'
  if (state === 'failed') return 'offline'
  return 'syncing'
}

export function ringPercent(session: Session) {
  if (session.capacityPackets <= 0) return 0
  return Math.min(100, Math.round((session.retainedPackets / session.capacityPackets) * 100))
}

function sessionKey(session: Session) {
  return `${session.providerPoolId}\u001f${session.sourceId}\u001f${session.configuredGeneration}`
}

export function SessionsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const query = useQuery({ ...apiQueries(client).sessions })
  const queryClient = useQueryClient()
  const terminate = useMutation({
    mutationFn: (session: Session) => client.terminateSession(session),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['sessions'] }),
  })
  const sessions = query.data ?? []
  if (!query.data) return <LoadingPage label="sessions" />

  const viewers = sessions.reduce((total, session) => total + session.viewerCount, 0)
  const pools = new Map(sessions.map((session) => [session.providerPoolId, session]))
  const activeSlots = [...pools.values()].reduce((total, session) => total + session.providerActiveSessions, 0)
  const capacity = [...pools.values()].reduce((total, session) => total + session.providerCapacity, 0)
  const fanOut = sessions.length === 0 ? '0.0' : (viewers / sessions.length).toFixed(1)

  return (
    <>
      <PageHeader eyebrow="Relay" title="Sessions" description="Observe each shared upstream, its downstream viewer fan-out, ring pressure, and bounded recovery activity." />
      <section aria-label="Session summary" className="mb-4 grid gap-3 sm:grid-cols-3">
        <Card><CardContent className="flex items-center gap-3"><Users aria-hidden="true" className="size-5 text-ocean-400" /><div><p className="text-xl font-semibold text-white">{viewers}</p><p className="text-xs text-slate-500">Downstream viewers</p></div></CardContent></Card>
        <Card><CardContent className="flex items-center gap-3"><Network aria-hidden="true" className="size-5 text-ocean-400" /><div><p className="text-xl font-semibold text-white">{sessions.length}</p><p className="text-xs text-slate-500">Shared upstreams · {fanOut}:1 fan-out</p></div></CardContent></Card>
        <Card><CardContent className="flex items-center gap-3"><Activity aria-hidden="true" className="size-5 text-ocean-400" /><div><p className="text-xl font-semibold text-white">{activeSlots}/{capacity}</p><p className="text-xs text-slate-500">Provider slots in use</p></div></CardContent></Card>
      </section>
      <Card>
        <div className="overflow-x-auto">
          <Table>
            <caption className="sr-only">Live shared IPTV upstream sessions</caption>
            <TableHeader><TableRow><TableHead>Source</TableHead><TableHead>Provider pool</TableHead><TableHead>Viewers</TableHead><TableHead>Generation</TableHead><TableHead>Ring</TableHead><TableHead>Recovery</TableHead><TableHead>Status</TableHead><TableHead>Action</TableHead></TableRow></TableHeader>
            <TableBody>
              {sessions.length === 0
                ? <TableRow><TableCell colSpan={8} className="py-8 text-center text-slate-500">No live shared sessions.</TableCell></TableRow>
                : sessions.map((session) => {
                    const retained = ringPercent(session)
                    return (
                      <TableRow key={sessionKey(session)}>
                        <TableCell className="font-mono text-xs font-medium text-white"><span className="block font-sans text-sm">{session.channelName || 'Unknown channel'}</span><span>{session.sourceId}</span></TableCell>
                        <TableCell><Badge>{session.providerPoolId}</Badge><p className="mt-1 text-xs text-slate-500">{session.baseServer || 'server unknown'}</p><p className="mt-1 text-xs text-slate-500">{session.providerAvailableSlots} slots available · peak {session.providerHighWatermark}</p></TableCell>
                        <TableCell className="font-mono">{session.viewerCount}</TableCell>
                        <TableCell className="font-mono text-xs">configured {session.configuredGeneration}<br />upstream {session.upstreamGeneration}<br /><span className="text-slate-500">adapter {session.adapter}</span></TableCell>
                        <TableCell className="min-w-44"><div className="flex justify-between font-mono text-xs"><span>{session.retainedPackets.toLocaleString()} / {session.capacityPackets.toLocaleString()} packets</span><span>{retained}%</span></div><progress className="mt-1 h-1.5 w-full accent-sky-400" aria-label={`${session.sourceId} ring utilization`} max={100} value={retained} /><p className="mt-1 text-xs text-slate-500">lag {session.lagEvents} · wraps {session.wrapEvents} · overwritten {session.overwrittenPackets.toLocaleString()}</p></TableCell>
                        <TableCell className="text-xs">reconnects {session.reconnectAttempts} · failovers {session.failoverAttempts}<br /><span className="text-slate-500">failures {session.failureCount}{session.lastFailure ? ` · ${failureLabels[session.lastFailure]}` : ''}</span></TableCell>
                        <TableCell><HealthBadge state={sessionHealth(session.state)} /><p className="mt-1 text-xs capitalize text-slate-500">{session.state.replace('-', ' ')}</p></TableCell>
                        <TableCell><Button variant="ghost" size="sm" className="text-slate-400 hover:text-red-300" disabled={terminate.isPending} onClick={() => { if (window.confirm(`Terminate the stream for ${session.channelName || session.sourceId}?`)) terminate.mutate(session) }}><Power aria-hidden="true" className="mr-1 size-3" />Terminate</Button></TableCell>
                      </TableRow>
                    )
                  })}
            </TableBody>
          </Table>
        </div>
        {terminate.isError ? <p role="alert" className="border-t border-white/5 px-4 py-3 text-sm text-rose-300">The session could not be terminated. Refresh and try again.</p> : null}
      </Card>
    </>
  )
}
