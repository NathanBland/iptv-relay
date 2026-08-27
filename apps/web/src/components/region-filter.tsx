import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import { cn } from '@/lib/utils'
import type { IptvApiClient } from '@/lib/api/types'

export function RegionFilter({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(true)
  const [initialized, setInitialized] = useState(false)
  const [selected, setSelected] = useState<string[]>([])
  const [result, setResult] = useState<{ type: 'success' | 'error'; message: string } | null>(null)

  const query = useQuery({ ...apiQueries(client).regionSettings })

  useEffect(() => {
    if (query.data && !initialized) {
      setSelected(query.data.settings.enabledPrefixes)
      setInitialized(true)
    }
  }, [query.data, initialized])

  const applyMutation = useMutation({
    mutationFn: () => client.applyRegionFilter({ enabledPrefixes: selected }),
    onSuccess: (res) => {
      setResult({
        type: 'success',
        message: `Region filter applied. ${res.enabled.toLocaleString()} groups enabled, ${res.disabled.toLocaleString()} groups disabled.`,
      })
      void queryClient.invalidateQueries({ queryKey: ['groups'] })
      void queryClient.invalidateQueries({ queryKey: ['channels'] })
      void queryClient.invalidateQueries({ queryKey: ['overview'] })
      void queryClient.invalidateQueries({ queryKey: ['region-settings'] })
    },
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Apply failed.'
      setResult({ type: 'error', message })
    },
  })

  const togglePrefix = (prefix: string) => {
    setSelected((prev) => (prev.includes(prefix) ? prev.filter((p) => p !== prefix) : [...prev, prefix]))
  }

  const selectSuggested = () => {
    if (!query.data) return
    setSelected(query.data.settings.suggestedPrefixes)
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between gap-4">
          <div className="min-w-0">
            <h2 className="text-base font-semibold text-white">Region Filter</h2>
            {query.data ? (
              <p className="text-xs text-slate-400">
                Detected timezone: {query.data.settings.timezone}
                {query.data.settings.autoDetected ? (
                  <span className="ml-2 rounded-sm bg-emerald-400/15 px-1.5 py-0.5 text-[0.65rem] text-emerald-300">Auto-detected</span>
                ) : (
                  <span className="ml-2 rounded-sm bg-slate-500/15 px-1.5 py-0.5 text-[0.65rem] text-slate-300">Manual</span>
                )}
              </p>
            ) : (
              <p className="text-xs text-slate-500">Loading region filter...</p>
            )}
          </div>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            aria-expanded={open}
            aria-label={open ? 'Collapse region filter' : 'Expand region filter'}
            onClick={() => setOpen((prev) => !prev)}
          >
            <ChevronDown
              aria-hidden="true"
              className={cn('size-4 text-slate-300 transition-transform', open && 'rotate-180')}
            />
          </Button>
        </div>
      </CardHeader>
      {open && (
        <CardContent className="space-y-4">
          {query.isLoading ? (
            <p className="text-sm text-slate-500">Loading region filter...</p>
          ) : query.error ? (
            <p className="text-sm text-red-300" role="alert">
              {query.error instanceof Error ? query.error.message : 'Failed to load region settings.'}
            </p>
          ) : query.data ? (
            <>
              <p className="text-sm text-slate-400">
                Toggle prefixes to enable matching groups. Click “Select suggested” to use the prefixes for{' '}
                <span className="font-medium text-slate-200">{query.data.settings.timezone}</span>.
              </p>
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">
                {query.data.prefixes.map((prefix) => {
                  const isSelected = selected.includes(prefix.prefix)
                  return (
                    <Badge
                      key={prefix.prefix}
                      tone={isSelected ? 'success' : 'neutral'}
                      className={cn(
                        'cursor-pointer justify-between gap-2 border text-xs uppercase transition-opacity',
                        isSelected ? 'opacity-100' : 'opacity-70',
                      )}
                      role="button"
                      tabIndex={0}
                      aria-pressed={isSelected}
                      onClick={() => togglePrefix(prefix.prefix)}
                      onKeyDown={(event) => {
                        if (event.key === 'Enter' || event.key === ' ') {
                          event.preventDefault()
                          togglePrefix(prefix.prefix)
                        }
                      }}
                    >
                      <span>{prefix.prefix}</span>
                      <span className="opacity-80">{prefix.channelCount.toLocaleString()}</span>
                    </Badge>
                  )
                })}
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  type="button"
                  size="sm"
                  disabled={applyMutation.isPending}
                  onClick={() => applyMutation.mutate()}
                >
                  {applyMutation.isPending ? (
                    <span className="inline-flex items-center gap-2">
                      <span className="sr-only">Applying</span>
                      <span className="size-4 animate-spin rounded-full border-2 border-current border-t-transparent" aria-hidden="true" />
                      Apply...
                    </span>
                  ) : (
                    'Apply region filter'
                  )}
                </Button>
                <Button type="button" variant="secondary" size="sm" onClick={selectSuggested}>
                  Select suggested
                </Button>
              </div>
              {result && (
                <p
                  className={cn('text-sm', result.type === 'success' ? 'text-emerald-300' : 'text-red-300')}
                  role={result.type === 'error' ? 'alert' : 'status'}
                >
                  {result.message}
                </p>
              )}
            </>
          ) : null}
        </CardContent>
      )}
    </Card>
  )
}
