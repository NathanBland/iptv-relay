import { createFileRoute } from '@tanstack/react-router'
import { safeNextPath } from '@/lib/api/client'
import { LoginPage } from '@/pages/login-page'

export const Route = createFileRoute('/login')({
  validateSearch: (search: Record<string, unknown>) => ({ next: safeNextPath(search.next) }),
  component: LoginRoute,
})

function LoginRoute() {
  const { next } = Route.useSearch()
  return <LoginPage next={next} />
}
