import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, CheckCircle2, Clock, Circle, Plus, Trash2, Video } from 'lucide-react'
import { useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { IptvApiClient, Recording } from '@/lib/api/types'

const statusConfig = {
  scheduled: { icon: Clock, tone: 'neutral' as const, label: 'Scheduled' },
  recording: { icon: Video, tone: 'info' as const, label: 'Recording' },
  completed: { icon: CheckCircle2, tone: 'success' as const, label: 'Completed' },
  failed: { icon: AlertCircle, tone: 'danger' as const, label: 'Failed' },
  cancelled: { icon: Circle, tone: 'neutral' as const, label: 'Cancelled' },
} as const

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[i]}`
}

export function RecordingsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const statsQuery = useQuery(apiQueries(client).recordingStats)
  const recordingsQuery = useQuery(apiQueries(client).recordings())
  const rulesQuery = useQuery(apiQueries(client).recordingRules)
  const [showRuleForm, setShowRuleForm] = useState(false)
  const [ruleName, setRuleName] = useState('')
  const [channelId, setChannelId] = useState('')
  const [ruleType, setRuleType] = useState<'one-time' | 'recurring' | 'series'>('one-time')

  const createRuleMutation = useMutation({
    mutationFn: () => client.createRecordingRule({ name: ruleName, channelId, ruleType }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['recording-rules'] })
      setShowRuleForm(false)
      setRuleName('')
      setChannelId('')
      setRuleType('one-time')
    },
  })

  const deleteRuleMutation = useMutation({
    mutationFn: (id: string) => client.deleteRecordingRule(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['recording-rules'] }),
  })

  const deleteRecordingMutation = useMutation({
    mutationFn: (id: string) => client.deleteRecording(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['recordings'] })
      queryClient.invalidateQueries({ queryKey: ['recording-stats'] })
    },
  })

  if (!recordingsQuery.data || !rulesQuery.data) return <LoadingPage label="recordings" />

  const stats = statsQuery.data
  const recordings = recordingsQuery.data.items
  const rules = rulesQuery.data

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="DVR"
        title="Recordings"
        description="Schedule and manage DVR recordings."
        actions={
          <Button onClick={() => setShowRuleForm(!showRuleForm)}>
            <Plus className="size-4" />
            Add rule
          </Button>
        }
      />

      {stats && (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-5">
          <Card>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <p className="text-sm font-medium text-slate-400">Scheduled</p>
              <Clock className="size-4 text-slate-400" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-white">{stats.scheduled}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <p className="text-sm font-medium text-slate-400">Recording</p>
              <Video className="size-4 text-ocean-400" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-white">{stats.recording}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <p className="text-sm font-medium text-slate-400">Completed</p>
              <CheckCircle2 className="size-4 text-mint-400" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-white">{stats.completed}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <p className="text-sm font-medium text-slate-400">Failed</p>
              <AlertCircle className="size-4 text-red-400" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-white">{stats.failed}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <p className="text-sm font-medium text-slate-400">Storage</p>
              <Video className="size-4 text-slate-400" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-white">{formatBytes(stats.totalBytes)}</div>
            </CardContent>
          </Card>
        </div>
      )}

      {showRuleForm && (
        <Card>
          <CardHeader>
            <p className="text-lg font-semibold text-white">New recording rule</p>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid gap-4 sm:grid-cols-3">
              <div>
                <label className="mb-1 block text-sm text-slate-400">Rule name</label>
                <Input value={ruleName} onChange={(e) => setRuleName(e.target.value)} placeholder="Daily news" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Channel ID</label>
                <Input value={channelId} onChange={(e) => setChannelId(e.target.value)} placeholder="UUID" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Rule type</label>
                <select
                  value={ruleType}
                  onChange={(e) => setRuleType(e.target.value as 'one-time' | 'recurring' | 'series')}
                  className="w-full rounded-md border border-white/10 bg-ink-900 px-3 py-2 text-white"
                >
                  <option value="one-time">One-time</option>
                  <option value="recurring">Recurring</option>
                  <option value="series">Series</option>
                </select>
              </div>
            </div>
            <div className="flex gap-2">
              <Button onClick={() => createRuleMutation.mutate()} disabled={!ruleName || !channelId}>
                Create rule
              </Button>
              <Button variant="secondary" onClick={() => setShowRuleForm(false)}>
                Cancel
              </Button>
            </div>
            {createRuleMutation.isError && (
              <p className="text-sm text-red-400">Failed to create the recording rule.</p>
            )}
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <p className="text-lg font-semibold text-white">Recording rules ({rules.length})</p>
        </CardHeader>
        <CardContent>
          <div className="space-y-2">
            {rules.length === 0 && (
              <p className="py-8 text-center text-sm text-slate-500">No recording rules configured.</p>
            )}
            {rules.map((rule) => (
              <div
                key={rule.id}
                className="flex items-center justify-between rounded-lg border border-white/5 bg-ink-900/50 px-4 py-3"
              >
                <div className="flex items-center gap-3">
                  <Video className="size-4 text-ocean-400" />
                  <div>
                    <p className="text-sm font-medium text-white">{rule.name}</p>
                    <p className="text-xs text-slate-500">
                      {rule.ruleType} · {rule.keepUntil}
                      {rule.titleFilter ? ` · Filter: ${rule.titleFilter}` : ''}
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  {!rule.enabled && <Badge tone="warning">Disabled</Badge>}
                  <Button
                    variant="secondary"
                    onClick={() => deleteRuleMutation.mutate(rule.id)}
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

      <Card>
        <CardHeader>
          <p className="text-lg font-semibold text-white">Recordings ({recordings.length})</p>
        </CardHeader>
        <CardContent>
          <div className="space-y-2">
            {recordings.length === 0 && (
              <p className="py-8 text-center text-sm text-slate-500">No recordings found.</p>
            )}
            {recordings.map((recording: Recording) => {
              const config = statusConfig[recording.status] ?? statusConfig.scheduled
              const StatusIcon = config.icon
              return (
                <div
                  key={recording.id}
                  className="flex items-center justify-between rounded-lg border border-white/5 bg-ink-900/50 px-4 py-3"
                >
                  <div className="flex items-center gap-3">
                    <StatusIcon className="size-4 text-slate-400" />
                    <div>
                      <p className="text-sm font-medium text-white">{recording.title}</p>
                      <p className="text-xs text-slate-500">
                        {new Date(recording.startsAt).toLocaleString()}
                        {recording.durationSeconds ? ` · ${Math.round(recording.durationSeconds / 60)} min` : ''}
                        {recording.fileSizeBytes ? ` · ${formatBytes(recording.fileSizeBytes)}` : ''}
                      </p>
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    <Badge tone={config.tone}>{config.label}</Badge>
                    <Button
                      variant="secondary"
                      onClick={() => deleteRecordingMutation.mutate(recording.id)}
                      className="px-2"
                    >
                      <Trash2 className="size-4" />
                    </Button>
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
