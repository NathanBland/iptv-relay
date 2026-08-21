import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Shield, Trash2, UserCog } from 'lucide-react'
import { useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { IptvApiClient, User } from '@/lib/api/types'

const roleTone = {
  admin: 'danger' as const,
  operator: 'info' as const,
  viewer: 'neutral' as const,
}

export function UsersPage({ client = apiClient }: { client?: IptvApiClient }) {
  const queryClient = useQueryClient()
  const usersQuery = useQuery(apiQueries(client).users)
  const [showForm, setShowForm] = useState(false)
  const [username, setUsername] = useState('')
  const [displayName, setDisplayName] = useState('')
  const [password, setPassword] = useState('')
  const [role, setRole] = useState<'admin' | 'operator' | 'viewer'>('viewer')

  const createMutation = useMutation({
    mutationFn: () => client.createUser({ username, displayName, password, role }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['users'] })
      setShowForm(false)
      setUsername('')
      setDisplayName('')
      setPassword('')
      setRole('viewer')
    },
  })

  const deleteMutation = useMutation({
    mutationFn: (id: string) => client.deleteUser(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['users'] }),
  })

  const toggleMutation = useMutation({
    mutationFn: (user: User) => client.updateUser(user.id, { enabled: !user.enabled }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['users'] }),
  })

  if (!usersQuery.data) return <LoadingPage label="users" />

  const users = usersQuery.data

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Access control"
        title="Users"
        description="Manage user accounts and access permissions."
        actions={
          <Button onClick={() => setShowForm(!showForm)}>
            <Plus className="size-4" />
            Add user
          </Button>
        }
      />

      {showForm && (
        <Card>
          <CardHeader>
            <p className="text-lg font-semibold text-white">New user</p>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid gap-4 sm:grid-cols-2">
              <div>
                <label className="mb-1 block text-sm text-slate-400">Username</label>
                <Input value={username} onChange={(e) => setUsername(e.target.value)} placeholder="username" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Display name</label>
                <Input value={displayName} onChange={(e) => setDisplayName(e.target.value)} placeholder="Display name" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Password</label>
                <Input type="password" value={password} onChange={(e) => setPassword(e.target.value)} placeholder="Password" />
              </div>
              <div>
                <label className="mb-1 block text-sm text-slate-400">Role</label>
                <select
                  value={role}
                  onChange={(e) => setRole(e.target.value as 'admin' | 'operator' | 'viewer')}
                  className="w-full rounded-md border border-white/10 bg-ink-900 px-3 py-2 text-white"
                >
                  <option value="viewer">Viewer</option>
                  <option value="operator">Operator</option>
                  <option value="admin">Admin</option>
                </select>
              </div>
            </div>
            <div className="flex gap-2">
              <Button onClick={() => createMutation.mutate()} disabled={!username || !password || !displayName}>
                Create user
              </Button>
              <Button variant="secondary" onClick={() => setShowForm(false)}>
                Cancel
              </Button>
            </div>
            {createMutation.isError && (
              <p className="text-sm text-red-400">Failed to create user. Check that the username is unique.</p>
            )}
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <p className="text-lg font-semibold text-white">Accounts ({users.length})</p>
        </CardHeader>
        <CardContent>
          <div className="space-y-2">
            {users.length === 0 && (
              <p className="py-8 text-center text-sm text-slate-500">No user accounts configured.</p>
            )}
            {users.map((user) => (
              <div
                key={user.id}
                className="flex items-center justify-between rounded-lg border border-white/5 bg-ink-900/50 px-4 py-3"
              >
                <div className="flex items-center gap-3">
                  {user.role === 'admin' ? (
                    <Shield className="size-4 text-red-400" />
                  ) : (
                    <UserCog className="size-4 text-ocean-400" />
                  )}
                  <div>
                    <p className="text-sm font-medium text-white">{user.displayName}</p>
                    <p className="text-xs text-slate-500">
                      {user.username}
                      {user.lastLoginAt ? ` · Last login: ${new Date(user.lastLoginAt).toLocaleDateString()}` : ''}
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <Badge tone={roleTone[user.role]}>{user.role}</Badge>
                  {!user.enabled && <Badge tone="warning">Disabled</Badge>}
                  <Button
                    variant="secondary"
                    onClick={() => toggleMutation.mutate(user)}
                    className="px-2"
                  >
                    {user.enabled ? 'Disable' : 'Enable'}
                  </Button>
                  <Button
                    variant="secondary"
                    onClick={() => deleteMutation.mutate(user.id)}
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
