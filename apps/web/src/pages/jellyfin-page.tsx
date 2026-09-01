import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CheckCircle2, Clipboard, ExternalLink, RefreshCw } from 'lucide-react'
import { useEffect, useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { IptvApiClient, JellyfinSetup } from '@/lib/api/types'

const EMPTY_SETUP: JellyfinSetup = {
  status: 'regeneration-required',
  guideDaysMax: 30,
}

export function JellyfinPage({ client = apiClient }: { client?: IptvApiClient }) {
  const [copied, setCopied] = useState('')
  const [rotated, setRotated] = useState<JellyfinSetup | null>(null)
  const queryClient = useQueryClient()
  const query = useQuery({ ...apiQueries(client).jellyfinSetup })
  const rotation = useMutation({
    mutationFn: () => client.rotateJellyfinToken(),
    onSuccess: (result) => setRotated(result),
  })
  const setup: JellyfinSetup = rotated ?? query.data ?? EMPTY_SETUP
  const available = setup.status === 'available'

  // Remove the token-bearing setup data from the query cache when the page
  // unmounts so the URLs do not persist after navigation.
  useEffect(() => {
    return () => {
      queryClient.removeQueries({ queryKey: ['jellyfin-setup'] })
    }
  }, [queryClient])

  // Clear the one-time rotation result from local state on unmount.
  useEffect(() => {
    return () => setRotated(null)
  }, [])

  const endpoints = [
    { label: 'M3U tuner URL', value: setup.playlistUrl },
    { label: 'XMLTV guide URL', value: setup.xmltvUrl },
    { label: 'HDHomeRun device URL', value: setup.hdhrDeviceUrl },
  ]

  async function copyEndpoint(label: string, value: string | undefined) {
    if (!value) return
    await navigator.clipboard?.writeText(value)
    setCopied(label)
  }

  return (
    <>
      <PageHeader eyebrow="Integration" title="Jellyfin setup" description="Publish the managed lineup as an M3U tuner and XMLTV guide, then connect both endpoints in Jellyfin Live TV." actions={<a className="inline-flex h-10 items-center gap-2 rounded-lg border border-white/12 bg-white/6 px-3 text-sm font-semibold text-slate-100 hover:bg-white/10" href="https://jellyfin.org/docs/general/server/live-tv/setup-guide/" target="_blank" rel="noreferrer">Jellyfin docs <ExternalLink aria-hidden="true" className="size-4" /></a>} />
      <section className="grid gap-4 xl:grid-cols-[1.1fr_.9fr]">
        <Card>
          <CardHeader><div><h2 className="font-semibold text-white">Connection settings</h2><p className="mt-1 text-xs text-slate-500">The output profile supplies the token and tuner count.</p></div></CardHeader>
          <CardContent className="space-y-3">
            <p className="text-sm text-slate-400">The published URLs below come from the relay output profile. Copy each URL into the matching Jellyfin Live TV field. The guide horizon accepts up to {setup.guideDaysMax} days.</p>
            <div className="flex items-center gap-3">
              <Button type="button" variant="secondary" onClick={() => rotation.mutate()} disabled={rotation.isPending} aria-label="Rotate publish token">{rotation.isPending ? 'Wait…' : 'Rotate publish token'}<RefreshCw aria-hidden="true" className="size-4" /></Button>
              {rotation.isError ? <p role="status" className="text-xs text-rose-400">Token rotation failed.</p> : null}
              {rotated ? <p role="status" className="text-xs text-mint-400">New token published. Copy the URLs below now; they show once.</p> : null}
            </div>
          </CardContent>
        </Card>
        <div className="space-y-4">
          <Card>
            <CardHeader><h2 className="font-semibold text-white">Published endpoints</h2></CardHeader>
            <CardContent className="space-y-3">{query.isLoading ? <p role="status" className="text-sm text-slate-500">Wait while the published endpoints load.</p> : query.isError && !rotated ? <p role="status" className="text-sm text-rose-400">The published endpoints could not load.</p> : !available ? <p role="status" className="text-sm text-amber-400" data-testid="regeneration-required">A token rotation occurred. Rotate the publish token to display new URLs.</p> : endpoints.map((endpoint) => <div key={endpoint.label}><p className="mb-1 text-xs font-medium text-slate-400">{endpoint.label}</p><div className="flex items-center gap-2 rounded-lg bg-ink-950 p-2"><code className="min-w-0 flex-1 truncate text-xs text-cyan-300" data-testid={`endpoint-${endpoint.label}`}>{endpoint.value}</code><Button size="icon" variant="ghost" aria-label={`Copy ${endpoint.label}`} onClick={() => void copyEndpoint(endpoint.label, endpoint.value)}><Clipboard aria-hidden="true" className="size-4" /></Button></div>{copied === endpoint.label ? <p className="mt-1 text-xs text-mint-400">Copied.</p> : null}</div>)}</CardContent>
          </Card>
          <Card><CardContent><h2 className="font-semibold text-white">Jellyfin checklist</h2><ol className="mt-4 space-y-3 text-sm text-slate-400">{['Add the M3U URL as an M3U Tuner.', 'Add the XMLTV URL as a TV guide data provider.', 'Map guide data and refresh the Jellyfin guide.'].map((step, index) => <li key={step} className="flex gap-3"><CheckCircle2 aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-ocean-400" /><span><strong className="text-slate-200">{index + 1}.</strong> {step}</span></li>)}</ol></CardContent></Card>
        </div>
      </section>
    </>
  )
}
