import { useForm } from '@tanstack/react-form'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus } from 'lucide-react'
import { useState } from 'react'
import { HealthBadge } from '@/components/health-badge'
import { LoadingPage } from '@/components/loading-page'
import { PageHeader } from '@/components/page-header'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { FieldMessage, Input, Select } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { IptvApiClient, Source, SourceInput, SourceKind } from '@/lib/api/types'
import { formatRelativeTime } from '@/lib/utils'

const sourceKinds: SourceKind[] = ['M3U', 'Xtream', 'XMLTV', 'Network tuner']

export function SourcesPage({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const sourcesQuery = useQuery({ ...apiQueries(client).sources })
  const [showForm, setShowForm] = useState(false)
  const [notice, setNotice] = useState('')
  const mutation = useMutation({
    mutationFn: (input: SourceInput) => client.createSource(input),
    onSuccess: (source) => {
      queryClient.setQueryData<Source[]>(['sources'], (current = []) => [...current, source])
      setNotice(`${source.name} was added and its first sync has started.`)
      setShowForm(false)
    },
  })

  const form = useForm({
    defaultValues: { name: '', kind: 'M3U' as SourceKind, endpoint: '' },
    onSubmit: async ({ value }) => {
      await mutation.mutateAsync(value)
    },
  })

  if (!sourcesQuery.data) return <LoadingPage label="sources" />

  return (
    <>
      <PageHeader
        eyebrow="Ingest"
        title="Sources"
        description="Manage provider playlists, guide feeds, Xtream accounts, and network tuners. Credentials remain server-side."
        actions={
          <Button onClick={() => setShowForm((value) => !value)} aria-expanded={showForm}>
            <Plus aria-hidden="true" className="size-4" /> Add source
          </Button>
        }
      />

      {notice ? <p role="status" className="mb-4 rounded-lg border border-emerald-400/20 bg-emerald-400/8 p-3 text-sm text-emerald-200">{notice}</p> : null}

      {showForm ? (
        <Card className="mb-4">
          <CardHeader><h2 className="font-semibold text-white">Connect a source</h2></CardHeader>
          <CardContent>
            <form
              className="grid gap-4 lg:grid-cols-[1fr_13rem_1.5fr_auto] lg:items-start"
              onSubmit={(event) => { event.preventDefault(); event.stopPropagation(); void form.handleSubmit() }}
            >
              <form.Field
                name="name"
                validators={{ onChange: ({ value }) => value.trim().length < 2 ? 'Enter at least two characters.' : undefined }}
              >
                {(field) => (
                  <label className="text-xs font-medium text-slate-300">
                    Display name
                    <Input
                      className="mt-1"
                      name={field.name}
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) => field.handleChange(event.target.value)}
                      aria-invalid={field.state.meta.errors.length > 0}
                    />
                    <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                  </label>
                )}
              </form.Field>
              <form.Field name="kind">
                {(field) => (
                  <label className="text-xs font-medium text-slate-300">
                    Source type
                    <Select className="mt-1" value={field.state.value} onChange={(event) => field.handleChange(event.target.value as SourceKind)}>
                      {sourceKinds.map((kind) => <option key={kind}>{kind}</option>)}
                    </Select>
                  </label>
                )}
              </form.Field>
              <form.Field
                name="endpoint"
                validators={{ onChange: ({ value }) => /^https?:\/\//.test(value) ? undefined : 'Use an absolute http(s) URL.' }}
              >
                {(field) => (
                  <label className="text-xs font-medium text-slate-300">
                    Endpoint
                    <Input
                      className="mt-1"
                      type="url"
                      placeholder="https://provider.example/playlist.m3u"
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) => field.handleChange(event.target.value)}
                      aria-invalid={field.state.meta.errors.length > 0}
                    />
                    <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                  </label>
                )}
              </form.Field>
              <form.Subscribe selector={(state) => [state.canSubmit, state.isSubmitting]}>
                {([canSubmit, isSubmitting]) => (
                  <Button className="mt-5" type="submit" disabled={!canSubmit || isSubmitting}>
                    {isSubmitting ? 'Wait…' : 'Add source'}
                  </Button>
                )}
              </form.Subscribe>
            </form>
          </CardContent>
        </Card>
      ) : null}

      <Card>
        <div className="overflow-x-auto">
          <Table>
            <caption className="sr-only">Configured IPTV and guide sources</caption>
            <TableHeader><TableRow><TableHead>Name</TableHead><TableHead>Type</TableHead><TableHead>Status</TableHead><TableHead>Channels</TableHead><TableHead>Last sync</TableHead></TableRow></TableHeader>
            <TableBody>
              {sourcesQuery.data.map((source) => (
                <TableRow key={source.id}>
                  <TableCell><p className="font-medium text-white">{source.name}</p><p className="mt-1 max-w-md truncate font-mono text-[0.68rem] text-slate-600">{source.endpoint}</p></TableCell>
                  <TableCell>{source.kind}</TableCell>
                  <TableCell><HealthBadge state={source.state} /></TableCell>
                  <TableCell className="font-mono text-slate-200">{source.channels.toLocaleString()}</TableCell>
                  <TableCell>{formatRelativeTime(source.lastSync)}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </Card>
    </>
  )
}
