import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Search, Tag, Trash2 } from 'lucide-react'
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

export function ChannelAliasesPage({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const aliasesQuery = useQuery(apiQueries(client).channelAliases())
  const [showForm, setShowForm] = useState(false)
  const [canonicalName, setCanonicalName] = useState('')
  const [alias, setAlias] = useState('')
  const [country, setCountry] = useState('')
  const [category, setCategory] = useState('')
  const [resolveInput, setResolveInput] = useState('')
  const [resolveResult, setResolveResult] = useState<string | null | undefined>(undefined)

  const createMutation = useMutation({
    mutationFn: () => client.createChannelAlias({
      canonicalName,
      alias,
      ...(country ? { country } : {}),
      ...(category ? { category } : {}),
    }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['channel-aliases'] })
      setShowForm(false)
      setCanonicalName('')
      setAlias('')
      setCountry('')
      setCategory('')
    },
  })

  const deleteMutation = useMutation({
    mutationFn: (id: string) => client.deleteChannelAlias(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['channel-aliases'] }),
  })

  async function handleResolve() {
    if (!resolveInput) return
    const result = await client.resolveChannelAlias(resolveInput)
    setResolveResult(result.canonicalName)
  }

  if (!aliasesQuery.data) return <LoadingPage label="channel aliases" />

  const aliases = aliasesQuery.data.items
  const total = aliasesQuery.data.total

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Standardization"
        title="Channel aliases"
        description="Manage the alias database for channel name standardization."
        actions={
          <Button onClick={() => setShowForm(!showForm)}>
            <Plus className="size-4" />
            Add alias
          </Button>
        }
      />

      <Card>
        <CardHeader>
          <p className="text-lg font-semibold text-white">Resolve a channel name</p>
        </CardHeader>
        <CardContent>
          <div className="flex gap-2">
            <Input
              placeholder="Enter a channel name to resolve"
              value={resolveInput}
              onChange={(e) => setResolveInput(e.target.value)}
              className="max-w-md"
            />
            <Button onClick={handleResolve} disabled={!resolveInput}>
              <Search className="size-4" />
              Resolve
            </Button>
          </div>
          {resolveResult !== undefined && (
            <p className="mt-3 text-sm text-slate-400">
              {resolveResult !== null ? (
                <>Canonical name: <span className="font-medium text-white">{resolveResult}</span></>
              ) : (
                <span className="text-slate-500">No alias found for this name.</span>
              )}
            </p>
          )}
        </CardContent>
      </Card>

      {showForm && (
        <Card>
          <CardHeader>
            <p className="text-lg font-semibold text-white">New alias</p>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid gap-4 sm:grid-cols-2">
              <div>
                <label className="mb-1 block text-sm text-slate-400">Canonical name</label>
                <Input value={canonicalName} onChange={(e) => setCanonicalName(e.target.value)} placeholder="ESPN" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Alias</label>
                <Input value={alias} onChange={(e) => setAlias(e.target.value)} placeholder="ESPN HD" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Country (optional)</label>
                <Input value={country} onChange={(e) => setCountry(e.target.value)} placeholder="US" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Category (optional)</label>
                <Input value={category} onChange={(e) => setCategory(e.target.value)} placeholder="sports" />
              </div>
            </div>
            <div className="flex gap-2">
              <Button onClick={() => createMutation.mutate()} disabled={!canonicalName || !alias}>
                Create alias
              </Button>
              <Button variant="secondary" onClick={() => setShowForm(false)}>
                Cancel
              </Button>
            </div>
            {createMutation.isError && (
              <p className="text-sm text-red-400">Failed to create alias. The alias may already exist.</p>
            )}
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <p className="text-lg font-semibold text-white">Aliases ({total})</p>
        </CardHeader>
        <CardContent>
          <div className="space-y-2">
            {aliases.length === 0 && (
              <p className="py-8 text-center text-sm text-slate-500">No aliases configured.</p>
            )}
            {aliases.map((item) => (
              <div
                key={item.id}
                className="flex items-center justify-between rounded-lg border border-white/5 bg-ink-900/50 px-4 py-3"
              >
                <div className="flex items-center gap-3">
                  <Tag className="size-4 text-ocean-400" />
                  <div>
                    <p className="text-sm font-medium text-white">
                      {item.alias} <span className="text-slate-500">→</span> {item.canonicalName}
                    </p>
                    <p className="text-xs text-slate-500">
                      {item.country ?? 'No country'}
                      {item.category ? ` · ${item.category}` : ''}
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  {item.country && <Badge tone="neutral">{item.country}</Badge>}
                  {item.category && <Badge tone="info">{item.category}</Badge>}
                  <Button
                    variant="secondary"
                    onClick={() => deleteMutation.mutate(item.id)}
                    className="px-2"
                  >
                    <Trash2 className="size-4" />
                  </Button>
                </div>
              </div>
            ))}
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
