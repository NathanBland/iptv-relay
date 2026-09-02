import { useForm } from '@tanstack/react-form'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Cable, LockKeyhole } from 'lucide-react'
import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { FieldMessage, Input } from '@/components/ui/input'
import { apiClient, IptvApiError, mockInitialData, safeNextPath } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type { AuthStatus, IptvApiClient, LoginInput } from '@/lib/api/types'
import { loginSchema } from '@/lib/validation'

function defaultAuthenticated(next: string) {
  if (typeof window !== 'undefined') window.location.assign(next)
}

function errorMessage(error: unknown, fallback: string) {
  if (!(error instanceof IptvApiError)) return fallback
  return error.problem.detail ?? error.problem.title
}

export function LoginPage({
  client = apiClient,
  next = '/',
  onAuthenticated = defaultAuthenticated,
}: {
  client?: IptvApiClient
  next?: string
  onAuthenticated?: (next: string) => void
}) {
  const destination = safeNextPath(next)
  const queryClient = useQueryClient()
  const [formError, setFormError] = useState('')
  const authQuery = useQuery({
    ...apiQueries(client).authStatus,
    ...mockInitialData<AuthStatus>(client, { authenticated: false }),
  })
  const mutation = useMutation({
    mutationFn: (input: LoginInput) => client.login(input),
    onSuccess: (status) => {
      queryClient.setQueryData(['auth', 'status'], status)
      if (status.authenticated) onAuthenticated(destination)
      else setFormError('The server did not establish a signed-in session.')
    },
    onError: (error) => setFormError(errorMessage(error, 'Unable to sign in. Check your credentials and try again.')),
  })
  const form = useForm({
    defaultValues: { username: '', password: '' },
    validators: { onSubmit: loginSchema },
    onSubmit: async ({ value }) => {
      setFormError('')
      await mutation.mutateAsync(value)
    },
  })

  const authenticatedUser = authQuery.data?.authenticated ? authQuery.data.user : undefined
  const statusError = authQuery.isError
    ? errorMessage(authQuery.error, 'Unable to check sign-in status. Refresh the page and try again.')
    : ''

  return (
    <main className="grid min-h-screen place-items-center px-4 py-10">
      <section className="w-full max-w-md" aria-labelledby="login-heading">
        <div className="mb-6 flex items-center justify-center gap-3">
          <span className="grid size-11 place-items-center rounded-xl bg-ocean-400 text-ink-950 shadow-[0_0_30px_rgba(34,199,189,.22)]">
            <Cable aria-hidden="true" className="size-6" />
          </span>
          <div>
            <p className="font-bold tracking-wide text-white">Relay Control</p>
            <p className="text-[0.68rem] uppercase tracking-[0.16em] text-slate-500">IPTV orchestration</p>
          </div>
        </div>

        <Card>
          <CardContent className="p-6 sm:p-8">
            <LockKeyhole aria-hidden="true" className="mb-4 size-6 text-mint-400" />
            <h1 id="login-heading" className="text-2xl font-semibold text-white">Sign in</h1>
            <p className="mt-2 text-sm leading-6 text-slate-400">Use your Relay Control operator account to manage sources, streams, and guide data.</p>

            {authenticatedUser ? (
              <div className="mt-6">
                <p role="status" className="rounded-lg border border-emerald-400/20 bg-emerald-400/8 p-3 text-sm text-emerald-200">
                  Signed in as {authenticatedUser.displayName}.
                </p>
                <a className="mt-4 inline-flex min-h-10 w-full items-center justify-center rounded-lg bg-ocean-400 px-3 text-sm font-semibold text-ink-950 hover:bg-mint-400" href={destination}>
                  Continue to Relay Control
                </a>
              </div>
            ) : (
              <form
                className="mt-6 space-y-4"
                onSubmit={(event) => {
                  event.preventDefault()
                  event.stopPropagation()
                  void form.handleSubmit().catch(() => undefined)
                }}
              >
                <form.Field
                  name="username"
                  validators={{ onChange: loginSchema.shape.username }}
                >
                  {(field) => (
                    <label className="block text-xs font-medium text-slate-300">
                      Username
                      <Input
                        className="mt-1"
                        name={field.name}
                        autoComplete="username"
                        autoCapitalize="none"
                        spellCheck={false}
                        value={field.state.value}
                        onBlur={field.handleBlur}
                        onChange={(event) => field.handleChange(event.target.value)}
                        aria-invalid={field.state.meta.errors.length > 0}
                      />
                      <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                    </label>
                  )}
                </form.Field>
                <form.Field
                  name="password"
                  validators={{ onChange: loginSchema.shape.password }}
                >
                  {(field) => (
                    <label className="block text-xs font-medium text-slate-300">
                      Password
                      <Input
                        className="mt-1"
                        name={field.name}
                        type="password"
                        autoComplete="current-password"
                        value={field.state.value}
                        onBlur={field.handleBlur}
                        onChange={(event) => field.handleChange(event.target.value)}
                        aria-invalid={field.state.meta.errors.length > 0}
                      />
                      <FieldMessage>{field.state.meta.errors[0]}</FieldMessage>
                    </label>
                  )}
                </form.Field>

                {formError || statusError ? (
                  <p role="alert" className="rounded-lg border border-red-400/20 bg-red-400/8 p-3 text-sm text-red-200">
                    {formError || statusError}
                  </p>
                ) : null}

                <form.Subscribe selector={(state) => [state.canSubmit, state.isSubmitting]}>
                  {([canSubmit, isSubmitting]) => (
                    <Button className="w-full" type="submit" disabled={!canSubmit || isSubmitting || authQuery.isPending || statusError !== ''}>
                      {isSubmitting ? 'Wait…' : authQuery.isPending ? 'Wait…' : 'Sign in'}
                    </Button>
                  )}
                </form.Subscribe>
              </form>
            )}

            {!authenticatedUser && authQuery.data?.oidcEnabled ? (
              <div className="mt-5 border-t border-white/10 pt-5">
                <p className="mb-3 text-center text-xs text-slate-500">Or use your approved identity provider.</p>
                <a
                  className="inline-flex min-h-10 w-full items-center justify-center rounded-lg border border-ocean-400/40 px-3 text-sm font-semibold text-ocean-200 hover:border-mint-400 hover:text-mint-200"
                  href="/api/v1/auth/oidc/start"
                >
                  Continue with SSO
                </a>
              </div>
            ) : null}
          </CardContent>
        </Card>
        <p className="mt-4 text-center text-xs leading-5 text-slate-600">The server stores your session in a secure, HTTP-only cookie. The page does not receive the cookie value.</p>
      </section>
    </main>
  )
}
