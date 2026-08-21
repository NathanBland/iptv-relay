import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { LayoutGrid, Loader2, Power, PowerOff } from 'lucide-react'
import { useMemo, useState } from 'react'
import { LoadingPage } from '@/components/loading-page'
import { PageHeader } from '@/components/page-header'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { Group, IptvApiClient } from '@/lib/api/types'

function groupStatus(group: Group): 'all-enabled' | 'all-disabled' | 'partial' {
  if (group.enabledCount === 0) return 'all-disabled'
  if (group.enabledCount === group.channelCount) return 'all-enabled'
  return 'partial'
}

const statusTone: Record<string, 'success' | 'neutral' | 'warning'> = {
  'all-enabled': 'success',
  'all-disabled': 'neutral',
  partial: 'warning',
}

const statusLabel: Record<string, string> = {
  'all-enabled': 'all enabled',
  'all-disabled': 'all disabled',
  partial: 'partial',
}

function Spinner({ label }: { label: string }) {
  return (
    <>
      <Loader2 aria-hidden="true" className="size-4 animate-spin" />
      <span className="sr-only">{label}</span>
    </>
  )
}

function GroupCard({
  group,
  onToggle,
  pending,
  pendingDirection,
}: {
  group: Group
  onToggle: (enabled: boolean) => void
  pending: boolean
  pendingDirection: 'enable' | 'disable' | null
}) {
  const status = groupStatus(group)
  return (
    <Card>
      <CardContent className="flex flex-wrap items-center gap-4 py-4">
        <span className="grid size-10 place-items-center rounded-xl bg-ocean-400/10 text-ocean-400">
          <LayoutGrid aria-hidden="true" className="size-5" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="truncate font-semibold text-white">{group.name}</h2>
            <Badge tone={statusTone[status]}>{statusLabel[status]}</Badge>
          </div>
          <p className="mt-1 text-xs text-slate-400">
            {group.enabledCount.toLocaleString()} of {group.channelCount.toLocaleString()} channels enabled
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            variant="secondary"
            size="sm"
            disabled={pending || status === 'all-enabled'}
            onClick={() => onToggle(true)}
          >
            {pending && pendingDirection === 'enable' ? (
              <Spinner label="Enabling all channels" />
            ) : (
              <Power aria-hidden="true" className="size-4" />
            )}
            Enable all
          </Button>
          <Button
            variant="ghost"
            size="sm"
            disabled={pending || status === 'all-disabled'}
            onClick={() => onToggle(false)}
          >
            {pending && pendingDirection === 'disable' ? (
              <Spinner label="Disabling all channels" />
            ) : (
              <PowerOff aria-hidden="true" className="size-4" />
            )}
            Disable all
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}

export function GroupsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const [search, setSearch] = useState('')
  const [pendingGroup, setPendingGroup] = useState<string | null>(null)
  const [pendingDirection, setPendingDirection] = useState<'enable' | 'disable' | null>(null)
  const [bulkPending, setBulkPending] = useState<'enable' | 'disable' | null>(null)

  const groupsQuery = useQuery({ ...apiQueries(client).groups })

  const toggleMutation = useMutation({
    mutationFn: ({ groupName, enabled }: { groupName: string; enabled: boolean }) =>
      client.setGroupEnabled(groupName, enabled),
    onMutate: ({ groupName, enabled }) => {
      setPendingGroup(groupName)
      setPendingDirection(enabled ? 'enable' : 'disable')
    },
    onSettled: () => {
      setPendingGroup(null)
      setPendingDirection(null)
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['groups'] })
      void queryClient.invalidateQueries({ queryKey: ['channels'] })
      void queryClient.invalidateQueries({ queryKey: ['overview'] })
    },
  })

  const bulkMutation = useMutation({
    mutationFn: ({ enabled }: { enabled: boolean }) =>
      client.setAllGroupsEnabled(enabled),
    onMutate: ({ enabled }) => setBulkPending(enabled ? 'enable' : 'disable'),
    onSettled: () => setBulkPending(null),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['groups'] })
      void queryClient.invalidateQueries({ queryKey: ['channels'] })
      void queryClient.invalidateQueries({ queryKey: ['overview'] })
    },
  })

  const filtered = useMemo(() => {
    const term = search.trim().toLowerCase()
    if (!term) return groupsQuery.data ?? []
    return (groupsQuery.data ?? []).filter((group) => group.name.toLowerCase().includes(term))
  }, [groupsQuery.data, search])

  if (!groupsQuery.data) return <LoadingPage label="channel groups" />

  const totalChannels = groupsQuery.data.reduce((sum, group) => sum + group.channelCount, 0)
  const totalEnabled = groupsQuery.data.reduce((sum, group) => sum + group.enabledCount, 0)
  const allEnabled = totalEnabled === totalChannels
  const allDisabled = totalEnabled === 0

  return (
    <>
      <PageHeader
        eyebrow="Lineup control"
        title="Groups"
        description="Enable or disable entire channel groups. Disabled groups do not appear in M3U or XMLTV output."
      />
      <section className="space-y-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <label className="relative block max-w-sm flex-1">
            <span className="sr-only">Search groups</span>
            <Input
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder="Search by group name"
            />
          </label>
          <div className="flex items-center gap-2">
            <Button
              variant="secondary"
              size="sm"
              disabled={bulkPending !== null || allEnabled}
              onClick={() => bulkMutation.mutate({ enabled: true })}
            >
              {bulkPending === 'enable' ? (
                <Spinner label="Enabling all groups" />
              ) : (
                <Power aria-hidden="true" className="size-4" />
              )}
              Enable all groups
            </Button>
            <Button
              variant="ghost"
              size="sm"
              disabled={bulkPending !== null || allDisabled}
              onClick={() => bulkMutation.mutate({ enabled: false })}
            >
              {bulkPending === 'disable' ? (
                <Spinner label="Disabling all groups" />
              ) : (
                <PowerOff aria-hidden="true" className="size-4" />
              )}
              Disable all groups
            </Button>
          </div>
        </div>
        <p className="text-xs text-slate-400">
          {totalEnabled.toLocaleString()} of {totalChannels.toLocaleString()} channels enabled across {groupsQuery.data.length} groups
        </p>
        {filtered.length === 0 ? (
          <Card>
            <CardContent className="py-8 text-center text-sm text-slate-500">
              {search ? 'No groups match your search.' : 'No channel groups found.'}
            </CardContent>
          </Card>
        ) : (
          <div className="space-y-3">
            {filtered.map((group) => (
              <GroupCard
                key={group.name}
                group={group}
                pending={pendingGroup === group.name}
                pendingDirection={pendingGroup === group.name ? pendingDirection : null}
                onToggle={(enabled) => toggleMutation.mutate({ groupName: group.name, enabled })}
              />
            ))}
          </div>
        )}
      </section>
    </>
  )
}
