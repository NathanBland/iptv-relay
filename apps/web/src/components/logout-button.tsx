import { LogOut } from 'lucide-react'
import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { apiClient, IptvApiError } from '@/lib/api/client'
import type { IptvApiClient } from '@/lib/api/types'

function defaultLoggedOut() {
  if (typeof window !== 'undefined') window.location.assign('/login')
}

export function LogoutButton({
  client = apiClient,
  onLoggedOut = defaultLoggedOut,
}: {
  client?: IptvApiClient
  onLoggedOut?: () => void
}) {
  const [pending, setPending] = useState(false)
  const [error, setError] = useState('')

  const logout = async () => {
    setPending(true)
    setError('')
    try {
      const result = await client.logout()
      if (!result.ok) {
        setError(result.message || 'Unable to sign out.')
        return
      }
      onLoggedOut()
    } catch (caught) {
      setError(caught instanceof IptvApiError ? caught.problem.detail ?? caught.problem.title : 'Unable to sign out.')
    } finally {
      setPending(false)
    }
  }

  return (
    <div className="flex items-center gap-2">
      {error ? <span role="alert" className="sr-only">{error}</span> : null}
      <Button variant="ghost" size="sm" disabled={pending} onClick={() => void logout()}>
        <LogOut aria-hidden="true" className="size-4" />
        <span>{pending ? 'Wait…' : 'Sign out'}</span>
      </Button>
    </div>
  )
}
