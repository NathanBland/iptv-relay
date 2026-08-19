import { useForm } from '@tanstack/react-form'
import { useMutation } from '@tanstack/react-query'
import { CheckCircle2, Clipboard, ExternalLink } from 'lucide-react'
import { useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { FieldMessage, Input, Select } from '@/components/ui/input'
import { apiClient } from '@/lib/api/client'
import type { IptvApiClient, JellyfinConfig } from '@/lib/api/types'

const isAbsoluteUrl = (value: string) => /^https?:\/\/[^\s]+$/.test(value)

export function JellyfinPage({ client = apiClient }: { client?: IptvApiClient }) {
  const [notice, setNotice] = useState('')
  const mutation = useMutation({ mutationFn: (value: JellyfinConfig) => client.saveJellyfin(value), onSuccess: (result) => setNotice(result.message) })
  const form = useForm({
    defaultValues: { baseUrl: 'http://jellyfin:8096', tunerName: 'Relay Control', publicBaseUrl: 'http://iptv-web:3000', guideDays: 7 },
    onSubmit: async ({ value }) => { await mutation.mutateAsync(value) },
  })
  const endpoints = [
    { label: 'M3U tuner URL', value: 'http://iptv-web:3000/api/jellyfin/playlist.m3u' },
    { label: 'XMLTV guide URL', value: 'http://iptv-web:3000/api/jellyfin/guide.xml' },
  ]

  return (
    <>
      <PageHeader eyebrow="Integration" title="Jellyfin setup" description="Publish the managed lineup as an M3U tuner and XMLTV guide, then connect both endpoints in Jellyfin Live TV." actions={<a className="inline-flex h-10 items-center gap-2 rounded-lg border border-white/12 bg-white/6 px-3 text-sm font-semibold text-slate-100 hover:bg-white/10" href="https://jellyfin.org/docs/general/server/live-tv/setup-guide/" target="_blank" rel="noreferrer">Jellyfin docs <ExternalLink aria-hidden="true" className="size-4" /></a>} />
      {notice ? <p role="status" className="mb-4 rounded-lg border border-ocean-400/20 bg-ocean-400/8 p-3 text-sm text-mint-400">{notice}</p> : null}
      <section className="grid gap-4 xl:grid-cols-[1.1fr_.9fr]">
        <Card>
          <CardHeader><div><h2 className="font-semibold text-white">Connection settings</h2><p className="mt-1 text-xs text-slate-500">Saved through the typed server API boundary</p></div></CardHeader>
          <CardContent>
            <form className="space-y-4" onSubmit={(event) => { event.preventDefault(); event.stopPropagation(); void form.handleSubmit() }}>
              <form.Field name="baseUrl" validators={{ onChange: ({ value }) => isAbsoluteUrl(value) ? undefined : 'Enter an absolute Jellyfin URL.' }}>
                {(field) => <label className="block text-xs font-medium text-slate-300">Jellyfin server URL<Input className="mt-1" value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} aria-invalid={field.state.meta.errors.length > 0} /><FieldMessage>{field.state.meta.errors[0]}</FieldMessage></label>}
              </form.Field>
              <div className="grid gap-4 sm:grid-cols-2">
                <form.Field name="tunerName" validators={{ onChange: ({ value }) => value.trim() ? undefined : 'A tuner name is required.' }}>
                  {(field) => <label className="block text-xs font-medium text-slate-300">Tuner name<Input className="mt-1" value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} aria-invalid={field.state.meta.errors.length > 0} /><FieldMessage>{field.state.meta.errors[0]}</FieldMessage></label>}
                </form.Field>
                <form.Field name="guideDays">
                  {(field) => <label className="block text-xs font-medium text-slate-300">Guide horizon<Select className="mt-1" value={field.state.value} onChange={(event) => field.handleChange(Number(event.target.value))}>{[3, 7, 14].map((days) => <option key={days} value={days}>{days} days</option>)}</Select></label>}
                </form.Field>
              </div>
              <form.Field name="publicBaseUrl" validators={{ onChange: ({ value }) => isAbsoluteUrl(value) ? undefined : 'Enter an absolute URL reachable by Jellyfin.' }}>
                {(field) => <label className="block text-xs font-medium text-slate-300">Relay URL visible to Jellyfin<Input className="mt-1" value={field.state.value} onBlur={field.handleBlur} onChange={(event) => field.handleChange(event.target.value)} aria-invalid={field.state.meta.errors.length > 0} /><FieldMessage>{field.state.meta.errors[0]}</FieldMessage></label>}
              </form.Field>
              <form.Subscribe selector={(state) => [state.canSubmit, state.isSubmitting]}>
                {([canSubmit, isSubmitting]) => <Button type="submit" disabled={!canSubmit || isSubmitting}>{isSubmitting ? 'Wait…' : 'Save Jellyfin setup'}</Button>}
              </form.Subscribe>
            </form>
          </CardContent>
        </Card>
        <div className="space-y-4">
          <Card>
            <CardHeader><h2 className="font-semibold text-white">Published endpoints</h2></CardHeader>
            <CardContent className="space-y-3">{endpoints.map((endpoint) => <div key={endpoint.label}><p className="mb-1 text-xs font-medium text-slate-400">{endpoint.label}</p><div className="flex items-center gap-2 rounded-lg bg-ink-950 p-2"><code className="min-w-0 flex-1 truncate text-xs text-cyan-300">{endpoint.value}</code><Button size="icon" variant="ghost" aria-label={`Copy ${endpoint.label}`} onClick={() => void navigator.clipboard?.writeText(endpoint.value)}><Clipboard aria-hidden="true" className="size-4" /></Button></div></div>)}</CardContent>
          </Card>
          <Card><CardContent><h2 className="font-semibold text-white">Jellyfin checklist</h2><ol className="mt-4 space-y-3 text-sm text-slate-400">{['Add the M3U URL as an M3U Tuner.', 'Add the XMLTV URL as a TV guide data provider.', 'Map guide data and refresh the Jellyfin guide.'].map((step, index) => <li key={step} className="flex gap-3"><CheckCircle2 aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-ocean-400" /><span><strong className="text-slate-200">{index + 1}.</strong> {step}</span></li>)}</ol></CardContent></Card>
        </div>
      </section>
    </>
  )
}
