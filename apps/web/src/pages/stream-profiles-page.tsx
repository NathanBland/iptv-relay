import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Settings, Trash2, Zap } from 'lucide-react'
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

export function StreamProfilesPage({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const profilesQuery = useQuery(apiQueries(client).streamProfiles)
  const [showForm, setShowForm] = useState(false)
  const [name, setName] = useState('')
  const [profileType, setProfileType] = useState<'direct' | 'ffmpeg' | 'vlc' | 'streamlink' | 'custom'>('direct')
  const [command, setCommand] = useState('')
  const [bufferSeconds, setBufferSeconds] = useState('0')
  const [userAgent, setUserAgent] = useState('')

  const createMutation = useMutation({
    mutationFn: () => client.createStreamProfile({
      name,
      profileType,
      ...(command ? { command } : {}),
      bufferSeconds: parseFloat(bufferSeconds) || 0,
      ...(userAgent ? { userAgent } : {}),
    }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['stream-profiles'] })
      setShowForm(false)
      setName('')
      setProfileType('direct')
      setCommand('')
      setBufferSeconds('0')
      setUserAgent('')
    },
  })

  const deleteMutation = useMutation({
    mutationFn: (id: string) => client.deleteStreamProfile(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['stream-profiles'] }),
  })

  if (!profilesQuery.data) return <LoadingPage label="stream profiles" />

  const profiles = profilesQuery.data

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Streaming"
        title="Stream profiles"
        description="Configure how the backend connects to upstream provider streams."
        actions={
          <Button onClick={() => setShowForm(!showForm)}>
            <Plus className="size-4" />
            Add profile
          </Button>
        }
      />

      {showForm && (
        <Card>
          <CardHeader>
            <p className="text-lg font-semibold text-white">New stream profile</p>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid gap-4 sm:grid-cols-2">
              <div>
                <label className="mb-1 block text-sm text-slate-400">Profile name</label>
                <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="My FFmpeg profile" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Profile type</label>
                <select
                  value={profileType}
                  onChange={(e) => setProfileType(e.target.value as 'direct' | 'ffmpeg' | 'vlc' | 'streamlink' | 'custom')}
                  className="w-full rounded-md border border-white/10 bg-ink-900 px-3 py-2 text-white"
                >
                  <option value="direct">Direct</option>
                  <option value="ffmpeg">FFmpeg</option>
                  <option value="vlc">VLC</option>
                  <option value="streamlink">Streamlink</option>
                  <option value="custom">Custom</option>
                </select>
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Command (optional)</label>
                <Input value={command} onChange={(e) => setCommand(e.target.value)} placeholder="/usr/bin/ffmpeg" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Buffer seconds</label>
                <Input type="number" value={bufferSeconds} onChange={(e) => setBufferSeconds(e.target.value)} placeholder="0" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">User agent (optional)</label>
                <Input value={userAgent} onChange={(e) => setUserAgent(e.target.value)} placeholder="VLC/3.0" />
              </div>
            </div>
            <div className="flex gap-2">
              <Button onClick={() => createMutation.mutate()} disabled={!name}>
                Create profile
              </Button>
              <Button variant="secondary" onClick={() => setShowForm(false)}>
                Cancel
              </Button>
            </div>
            {createMutation.isError && (
              <p className="text-sm text-red-400">Failed to create the profile. The name may already exist.</p>
            )}
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <p className="text-lg font-semibold text-white">Profiles ({profiles.length})</p>
        </CardHeader>
        <CardContent>
          <div className="space-y-2">
            {profiles.length === 0 && (
              <p className="py-8 text-center text-sm text-slate-500">No stream profiles configured.</p>
            )}
            {profiles.map((profile) => (
              <div
                key={profile.id}
                className="flex items-center justify-between rounded-lg border border-white/5 bg-ink-900/50 px-4 py-3"
              >
                <div className="flex items-center gap-3">
                  {profile.profileType === 'direct' ? (
                    <Zap className="size-4 text-mint-400" />
                  ) : (
                    <Settings className="size-4 text-ocean-400" />
                  )}
                  <div>
                    <p className="text-sm font-medium text-white">{profile.name}</p>
                    <p className="text-xs text-slate-500">
                      {profile.profileType}
                      {profile.bufferSeconds > 0 ? ` · ${profile.bufferSeconds}s buffer` : ''}
                      {profile.command ? ` · ${profile.command}` : ''}
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  {!profile.enabled && <Badge tone="warning">Disabled</Badge>}
                  <Button
                    variant="secondary"
                    onClick={() => deleteMutation.mutate(profile.id)}
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
