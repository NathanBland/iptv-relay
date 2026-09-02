import { useQuery } from '@tanstack/react-query'
import { Download, FileText, RefreshCw, ShieldCheck } from 'lucide-react'
import { LoadingPage } from '@/components/loading-page'
import { PageHeader } from '@/components/page-header'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { apiClient, IptvApiError } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { IptvApiClient, SupportBundle, SupportLogEntry } from '@/lib/api/types'

function downloadBundle(bundle: SupportBundle) {
  const body = JSON.stringify(bundle, null, 2)
  const url = URL.createObjectURL(new Blob([body], { type: 'application/json' }))
  const link = document.createElement('a')
  link.href = url
  link.download = `iptv-support-${bundle.generatedAt.replace(/[:.]/g, '-')}.json`
  link.click()
  URL.revokeObjectURL(url)
}

function levelTone(level: SupportLogEntry['level']) {
  return level === 'error' ? 'danger' as const : 'info' as const
}

function logContext(log: SupportLogEntry) {
  try {
    return JSON.stringify(log.context)
  } catch {
    return '{}'
  }
}

export function SupportPage({ client = apiClient }: { client?: IptvApiClient }) {
  const bundleQuery = useQuery({ ...apiQueries(client).supportBundle })
  const logsQuery = useQuery({ ...apiQueries(client).supportLogs })
  const bundle = bundleQuery.data
  const logs = logsQuery.data ?? bundle?.logs ?? []

  if (!bundle) return <LoadingPage label="support diagnostics" />

  function download() {
    try {
      downloadBundle(bundle)
    } catch {
      // The API result remains available for manual copy if the browser blocks downloads.
    }
  }

  return (
    <>
      <PageHeader
        eyebrow="Operations"
        title="Support diagnostics"
        description="Review redacted diagnostics and download a support bundle for safe troubleshooting."
        actions={<Button onClick={download}><Download aria-hidden="true" className="size-4" />Download support bundle</Button>}
      />

      <div className="grid gap-4 lg:grid-cols-[1.1fr_0.9fr]">
        <Card>
          <CardHeader>
            <div className="flex items-center gap-3">
              <ShieldCheck aria-hidden="true" className="size-5 text-mint-400" />
              <div>
                <h2 className="text-lg font-semibold text-white">Redaction status</h2>
                <p className="mt-1 text-xs text-slate-500">Bundle schema {bundle.schemaVersion} · generated {new Date(bundle.generatedAt).toLocaleString()}</p>
              </div>
            </div>
          </CardHeader>
          <CardContent>
            <p className="text-sm leading-6 text-slate-300">{bundle.redaction}</p>
            <p className="mt-3 rounded-lg border border-mint-400/15 bg-mint-400/5 p-3 text-xs leading-5 text-mint-200">
              Credentials, tokens, provider URLs, and sensitive diagnostic fields do not appear in this export.
            </p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader><h2 className="text-lg font-semibold text-white">System snapshot</h2></CardHeader>
          <CardContent className="grid grid-cols-2 gap-3 text-sm">
            <Snapshot label="Channels" value={bundle.system.channels} />
            <Snapshot label="Healthy streams" value={bundle.system.healthyStreams} />
            <Snapshot label="Active sessions" value={bundle.system.activeSessions} />
            <Snapshot label="Guide coverage" value={`${bundle.system.guideCoverage.toFixed(1)}%`} />
            <Snapshot label="Failed streams" value={bundle.streamHealth.dead} />
            <Snapshot label="Uptime" value={`${Math.floor(bundle.system.uptimeSeconds / 3600)}h`} />
          </CardContent>
        </Card>
      </div>

      <Card className="mt-4">
        <CardHeader>
          <div className="flex w-full items-center justify-between gap-3">
            <div className="flex items-center gap-3"><FileText aria-hidden="true" className="size-5 text-ocean-400" /><div><h2 className="text-lg font-semibold text-white">Redacted logs</h2><p className="mt-1 text-xs text-slate-500">Recent operator activity and failed job diagnostics.</p></div></div>
            <Button variant="secondary" onClick={() => { void logsQuery.refetch() }} disabled={logsQuery.isFetching}><RefreshCw aria-hidden="true" className="size-4" />Refresh logs</Button>
          </div>
        </CardHeader>
        <CardContent>
          {logsQuery.error instanceof IptvApiError && <p role="alert" className="mb-3 text-sm text-red-400">{logsQuery.error.message}</p>}
          <div role="log" aria-label="Redacted support logs" aria-live="polite" className="max-h-96 space-y-2 overflow-auto rounded-lg border border-white/8 bg-ink-950/60 p-3">
            {logs.length === 0 ? <p className="py-8 text-center text-sm text-slate-500">No support log entries.</p> : logs.map((log, index) => (
              <article key={`${log.timestamp}-${index}`} className="rounded-md border border-white/6 bg-white/[0.02] p-3">
                <div className="flex flex-wrap items-center gap-2 text-xs"><Badge tone={levelTone(log.level)}>{log.level}</Badge><time dateTime={log.timestamp} className="font-mono text-slate-500">{new Date(log.timestamp).toLocaleString()}</time></div>
                <p className="mt-2 text-sm text-slate-200">{log.message}</p>
                <p className="mt-1 break-all font-mono text-xs text-slate-500">{logContext(log)}</p>
              </article>
            ))}
          </div>
        </CardContent>
      </Card>
    </>
  )
}

function Snapshot({ label, value }: { label: string; value: string | number }) {
  return <div className="rounded-lg border border-white/6 bg-ink-950/45 p-3"><p className="text-xs text-slate-500">{label}</p><p className="mt-1 font-mono text-lg text-white">{value}</p></div>
}
