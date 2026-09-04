import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Ban, Clipboard, History, KeyRound, Plus, RefreshCw, RotateCcw, Save, SlidersHorizontal } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input, Select } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { apiClient, IptvApiError } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import type {
  ApplyRequirement,
  EffectiveSetting,
  IptvApiClient,
  InheritanceSource,
  OperatorRevisionResponse,
  OperatorScopeResponse,
  OperatorSettingScope,
  SettingDefinition,
} from '@/lib/api/types'

const operatorTokenScopes = ['read', 'control', 'output', 'admin'] as const

const scopeLabels: Record<OperatorSettingScope, string> = {
  global: 'Global',
  provider: 'Provider',
  group: 'Channel group',
}

const riskTone = {
  low: 'success' as const,
  capacity: 'warning' as const,
  compatibility: 'info' as const,
  service_disruption: 'danger' as const,
  security: 'danger' as const,
}

const applyRequirementLabel: Record<ApplyRequirement, string> = {
  immediate: 'Immediate',
  restart: 'Restart',
  reimport: 'Reimport',
}

function inheritanceLabel(source: InheritanceSource): string {
  switch (source.scope) {
    case 'system_default':
      return 'System default'
    case 'global':
      return 'Global override'
    case 'provider':
      return `Provider: ${source.id}`
    case 'channel_group':
      return `Group: ${source.id}`
  }
}

function describeValue(value: unknown): string {
  if (value === null || value === undefined) return ''
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  return JSON.stringify(value)
}

function parseOverrideInput(definition: SettingDefinition, raw: string): unknown {
  switch (definition.valueKind) {
    case 'integer': {
      const parsed = Number.parseInt(raw, 10)
      return Number.isNaN(parsed) ? raw : parsed
    }
    case 'boolean':
      return raw === 'true'
    case 'choice':
    case 'string':
      return raw
  }
}

interface ScopeEditorProps {
  client: IptvApiClient
  scope: OperatorSettingScope
  scopeId: string
  schema: SettingDefinition[]
}

function ScopeEditor({ client, scope, scopeId, schema }: ScopeEditorProps) {
  const queryClient = useQueryClient()
  const scopeQuery = useQuery(apiQueries(client).operatorScope(scope, scopeId))
  const revisionsQuery = useQuery(apiQueries(client).operatorRevisions(scope, scopeId))
  const [draft, setDraft] = useState<Record<string, unknown>>({})
  const [draftLoadedRevision, setDraftLoadedRevision] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [rollbackTarget, setRollbackTarget] = useState<number | null>(null)

  const current = scopeQuery.data
  const currentRevision = current?.revision ?? 0

  // Seed the draft from the loaded scope the first time the data arrives.
  if (current && draftLoadedRevision !== current.revision && Object.keys(draft).length === 0) {
    setDraft({ ...current.overrides })
    setDraftLoadedRevision(current.revision)
  }

  const editableKeys = useMemo(
    () =>
      schema.filter((definition) => {
        if (scope === 'provider') return definition.providerOverridable
        if (scope === 'group') return definition.groupOverridable
        return true
      }),
    [schema, scope],
  )

  const replaceMutation = useMutation({
    mutationFn: (input: { overrides: Record<string, unknown>; ifMatch?: string }) =>
      client.replaceOperatorScope(scope, scopeId, input),
    onSuccess: (updated: OperatorScopeResponse) => {
      setError(null)
      setDraft({ ...updated.overrides })
      setDraftLoadedRevision(updated.revision)
      queryClient.invalidateQueries({ queryKey: ['settings'] })
    },
    onError: (failure: unknown) => {
      if (failure instanceof IptvApiError && failure.problem.status === 412) {
        setError('The stored revision changed. Refresh the scope and retry your edit.')
      } else if (failure instanceof IptvApiError) {
        setError(failure.problem.detail ?? failure.problem.title)
      } else {
        setError('The save failed. Try again.')
      }
    },
  })

  const rollbackMutation = useMutation({
    mutationFn: (revision: number) => client.rollbackOperatorScope(scope, scopeId, { revision }),
    onSuccess: (updated: OperatorScopeResponse) => {
      setError(null)
      setRollbackTarget(null)
      setDraft({ ...updated.overrides })
      setDraftLoadedRevision(updated.revision)
      queryClient.invalidateQueries({ queryKey: ['settings'] })
    },
    onError: (failure: unknown) => {
      if (failure instanceof IptvApiError) {
        setError(failure.problem.detail ?? failure.problem.title)
      } else {
        setError('The rollback failed. Try again.')
      }
    },
  })

  function updateField(key: string, raw: string) {
    const definition = schema.find((item) => item.key === key)
    if (!definition) return
    setDraft((previous) => ({ ...previous, [key]: parseOverrideInput(definition, raw) }))
  }

  function clearField(key: string) {
    setDraft((previous) => {
      const next = { ...previous }
      delete next[key]
      return next
    })
  }

  function save() {
    const overrides: Record<string, unknown> = {}
    for (const definition of editableKeys) {
      const value = draft[definition.key]
      if (value !== undefined) overrides[definition.key] = value
    }
    const input: { overrides: Record<string, unknown>; ifMatch?: string } = { overrides }
    if (currentRevision > 0) input.ifMatch = `"${currentRevision}"`
    replaceMutation.mutate(input)
  }

  function refreshScope() {
    setError(null)
    void scopeQuery.refetch()
  }

  if (!current) return <LoadingPage label="operator scope" />

  const revisions = revisionsQuery.data ?? []

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <div>
            <p className="text-lg font-semibold text-white">
              {scopeLabels[scope]} overrides
              {scopeId ? ` · ${scopeId}` : ''}
            </p>
            <p className="mt-1 text-xs text-slate-500">
              Revision {current.revision}. Edits use If-Match optimistic concurrency.
            </p>
          </div>
          <Badge tone="info">ETag &quot;{current.revision}&quot;</Badge>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid gap-4 sm:grid-cols-2">
            {editableKeys.map((definition) => {
              const value = draft[definition.key]
              const raw = describeValue(value)
              return (
                <div key={definition.key} className="rounded-lg border border-white/5 bg-ink-900/50 p-4">
                  <div className="flex items-center justify-between gap-2">
                    <label htmlFor={`override-${definition.key}`} className="text-sm font-medium text-white">
                      {definition.label}
                    </label>
                    <Badge tone={riskTone[definition.risk]}>{definition.risk}</Badge>
                  </div>
                  <p className="mt-1 text-xs leading-5 text-slate-500">{definition.description}</p>
                  {definition.valueKind === 'boolean' ? (
                    <select
                      id={`override-${definition.key}`}
                      value={raw === 'true' ? 'true' : 'false'}
                      onChange={(event) => updateField(definition.key, event.target.value)}
                      className="mt-3 w-full rounded-md border border-white/10 bg-ink-900 px-3 py-2 text-white"
                    >
                      <option value="false">false</option>
                      <option value="true">true</option>
                    </select>
                  ) : definition.valueKind === 'choice' ? (
                    <select
                      id={`override-${definition.key}`}
                      value={typeof value === 'string' ? value : ''}
                      onChange={(event) => updateField(definition.key, event.target.value)}
                      className="mt-3 w-full rounded-md border border-white/10 bg-ink-900 px-3 py-2 text-white"
                    >
                      <option value="">Select…</option>
                      {definition.choices.map((choice) => (
                        <option key={choice} value={choice}>{choice}</option>
                      ))}
                    </select>
                  ) : (
                    <Input
                      id={`override-${definition.key}`}
                      value={raw}
                      onChange={(event) => updateField(definition.key, event.target.value)}
                      className="mt-3"
                    />
                  )}
                  <div className="mt-2 flex items-center justify-between">
                    <span className="text-xs text-slate-500">
                      Apply: {applyRequirementLabel[definition.applyRequirement]}
                      {definition.unit ? ` · ${definition.unit}` : ''}
                    </span>
                    <button
                      type="button"
                      onClick={() => clearField(definition.key)}
                      className="text-xs text-slate-400 hover:text-white"
                    >
                      Clear
                    </button>
                  </div>
                </div>
              )
            })}
            {editableKeys.length === 0 && (
              <p className="col-span-full py-8 text-center text-sm text-slate-500">
                No settings can be overridden at this scope.
              </p>
            )}
          </div>
          <div className="flex flex-wrap gap-2">
            <Button onClick={save} disabled={replaceMutation.isPending}>
              <Save className="size-4" />
              Save overrides
            </Button>
            <Button variant="secondary" onClick={refreshScope}>
              <RotateCcw className="size-4" />
              Refresh
            </Button>
            {error && <p role="alert" className="self-center text-sm text-red-400">{error}</p>}
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div>
            <p className="text-lg font-semibold text-white">Revision history</p>
            <p className="mt-1 text-xs text-slate-500">Restore a prior revision to roll back this scope.</p>
          </div>
          <History className="size-5 text-slate-500" />
        </CardHeader>
        <CardContent>
          {revisions.length === 0 ? (
            <p className="py-8 text-center text-sm text-slate-500">No revisions recorded yet.</p>
          ) : (
            <div className="space-y-2">
              {revisions.map((revision: OperatorRevisionResponse) => (
                <div
                  key={revision.revision}
                  className="flex items-center justify-between rounded-lg border border-white/5 bg-ink-900/50 px-4 py-3"
                >
                  <div>
                    <p className="text-sm font-medium text-white">Revision {revision.revision}</p>
                    <p className="text-xs text-slate-500">
                      {revision.actor} · {new Date(revision.createdAt).toLocaleString()}
                    </p>
                  </div>
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={rollbackMutation.isPending && rollbackTarget === revision.revision}
                    onClick={() => {
                      setRollbackTarget(revision.revision)
                      rollbackMutation.mutate(revision.revision)
                    }}
                  >
                    <RotateCcw className="size-4" />
                    Roll back
                  </Button>
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

function ScopeEditorPanel({
  client,
  scope,
  schema,
  label,
  placeholder,
}: {
  client: IptvApiClient
  scope: OperatorSettingScope
  schema: SettingDefinition[]
  label: string
  placeholder: string
}) {
  const [scopeId, setScopeId] = useState('')
  if (!scopeId) {
    return (
      <Card>
        <CardContent className="space-y-3">
          <label htmlFor={`scope-id-${scope}`} className="block text-sm text-slate-400">{label}</label>
          <Input
            id={`scope-id-${scope}`}
            value={scopeId}
            onChange={(event) => setScopeId(event.target.value)}
            placeholder={placeholder}
          />
          <p className="text-sm text-slate-500">Enter an identifier to load and edit overrides for this scope.</p>
        </CardContent>
      </Card>
    )
  }
  return <ScopeEditor client={client} scope={scope} scopeId={scopeId} schema={schema} />
}

function apiErrorText(failure: unknown, fallback: string): string {
  if (failure instanceof IptvApiError) return failure.problem.detail ?? failure.problem.title
  return fallback
}

function ApiTokensCard({ client }: { client: IptvApiClient }) {
  const queryClient = useQueryClient()
  const tokensQuery = useQuery(apiQueries(client).operatorApiTokens)
  const [showCreate, setShowCreate] = useState(false)
  const [name, setName] = useState('')
  const [scope, setScope] = useState<string>('read')
  const [expiresAt, setExpiresAt] = useState('')
  const [issued, setIssued] = useState<{ name: string; token: string } | null>(null)
  const [copied, setCopied] = useState(false)
  const [notice, setNotice] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [confirmRevoke, setConfirmRevoke] = useState<string | null>(null)
  const [confirmRotate, setConfirmRotate] = useState<string | null>(null)

  // Clear the one-time token value from local state when the card unmounts.
  useEffect(() => {
    return () => setIssued(null)
  }, [])

  function invalidateTokens() {
    void queryClient.invalidateQueries({ queryKey: ['operator-api-tokens'] })
  }

  const createMutation = useMutation({
    mutationFn: () =>
      client.createOperatorApiToken({
        name: name.trim(),
        scopes: [scope],
        ...(expiresAt ? { expiresAt: new Date(expiresAt).toISOString() } : {}),
      }),
    onSuccess: (result) => {
      setIssued({ name: result.name, token: result.token })
      setCopied(false)
      setError(null)
      setNotice('')
      setShowCreate(false)
      setName('')
      setScope('read')
      setExpiresAt('')
      invalidateTokens()
    },
    onError: (failure) => setError(apiErrorText(failure, 'The API did not create the token. Try again.')),
  })

  const rotateMutation = useMutation({
    mutationFn: (id: string) => client.rotateOperatorApiToken(id),
    onSuccess: (result) => {
      setIssued({ name: result.name, token: result.token })
      setCopied(false)
      setError(null)
      setNotice('')
      setConfirmRotate(null)
      invalidateTokens()
    },
    onError: (failure) => {
      setError(apiErrorText(failure, 'The API did not rotate the token. Try again.'))
      setConfirmRotate(null)
    },
  })

  const revokeMutation = useMutation({
    mutationFn: (id: string) => client.revokeOperatorApiToken(id),
    onSuccess: () => {
      setNotice('The token is revoked.')
      setError(null)
      setConfirmRevoke(null)
      invalidateTokens()
    },
    onError: (failure) => {
      setError(apiErrorText(failure, 'The API did not revoke the token. Try again.'))
      setConfirmRevoke(null)
    },
  })

  async function copyIssuedToken() {
    if (!issued) return
    await navigator.clipboard?.writeText(issued.token)
    setCopied(true)
  }

  const tokens = tokensQuery.data ?? []

  return (
    <Card>
      <CardHeader>
        <div>
          <p className="text-lg font-semibold text-white">API tokens</p>
          <p className="mt-1 text-xs text-slate-500">
            Operator API tokens give automation access to the API. The token value shows once at creation or rotation.
          </p>
        </div>
        <div className="flex items-center gap-3">
          <KeyRound className="size-5 text-slate-500" />
          <Button size="sm" onClick={() => setShowCreate((value) => !value)} aria-expanded={showCreate}>
            <Plus aria-hidden="true" className="size-4" /> Create token
          </Button>
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        {notice ? (
          <p role="status" className="rounded-lg border border-emerald-400/20 bg-emerald-400/8 p-3 text-sm text-emerald-200">{notice}</p>
        ) : null}
        {error ? (
          <p role="alert" className="rounded-lg border border-red-400/20 bg-red-400/8 p-3 text-sm text-red-200">{error}</p>
        ) : null}

        {issued ? (
          <div className="rounded-lg border border-amber-400/25 bg-amber-400/10 p-4" role="alert">
            <p className="text-sm font-medium text-amber-200">
              The API issued the token &quot;{issued.name}&quot;. The value shows once. Copy it now and store it in a safe place.
            </p>
            <div className="mt-2 flex items-center gap-2 rounded-lg bg-ink-950 p-2">
              <code className="min-w-0 flex-1 truncate font-mono text-xs text-cyan-300" data-testid="issued-token-value">{issued.token}</code>
              <Button size="icon" variant="ghost" aria-label="Copy token value" onClick={() => void copyIssuedToken()}>
                <Clipboard aria-hidden="true" className="size-4" />
              </Button>
            </div>
            <div className="mt-2 flex items-center justify-between">
              {copied ? <p role="status" className="text-xs text-mint-400">Copied.</p> : <span />}
              <Button variant="ghost" size="sm" onClick={() => setIssued(null)}>Dismiss</Button>
            </div>
          </div>
        ) : null}

        {showCreate ? (
          <form
            className="grid gap-3 rounded-lg border border-white/5 bg-ink-900/50 p-4 sm:grid-cols-[1fr_10rem_14rem_auto] sm:items-end"
            onSubmit={(event) => {
              event.preventDefault()
              if (name.trim()) createMutation.mutate()
            }}
          >
            <label className="text-xs font-medium text-slate-300">
              Token name
              <Input
                className="mt-1"
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder="Backup automation"
              />
            </label>
            <label className="text-xs font-medium text-slate-300">
              Scope
              <Select className="mt-1" value={scope} onChange={(event) => setScope(event.target.value)}>
                {operatorTokenScopes.map((item) => <option key={item} value={item}>{item}</option>)}
              </Select>
            </label>
            <label className="text-xs font-medium text-slate-300">
              Expires at (optional)
              <Input
                className="mt-1"
                type="datetime-local"
                value={expiresAt}
                onChange={(event) => setExpiresAt(event.target.value)}
              />
            </label>
            <span className="inline-flex gap-2">
              <Button type="submit" disabled={!name.trim() || createMutation.isPending}>
                {createMutation.isPending ? 'Wait…' : 'Create token'}
              </Button>
              <Button variant="ghost" onClick={() => setShowCreate(false)}>Cancel</Button>
            </span>
          </form>
        ) : null}

        {tokensQuery.isLoading ? (
          <p role="status" className="text-sm text-slate-500">Wait while the token list loads.</p>
        ) : tokensQuery.isError ? (
          <p role="alert" className="text-sm text-red-400">{apiErrorText(tokensQuery.error, 'The token list did not load.')}</p>
        ) : (
          <div className="overflow-x-auto">
            <Table>
              <caption className="sr-only">Operator API tokens</caption>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Scopes</TableHead>
                  <TableHead>Created</TableHead>
                  <TableHead>Expires</TableHead>
                  <TableHead>Last used</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {tokens.map((token) => (
                  <TableRow key={token.id} className={token.revokedAt ? 'opacity-60' : undefined}>
                    <TableCell>
                      <p className="font-medium text-white">{token.name}</p>
                      <p className="mt-1 font-mono text-[0.68rem] text-slate-600">{token.id}</p>
                    </TableCell>
                    <TableCell>
                      <span className="flex flex-wrap gap-1">
                        {token.scopes.map((item) => <Badge key={item} tone="neutral">{item}</Badge>)}
                      </span>
                    </TableCell>
                    <TableCell>{new Date(token.createdAt).toLocaleString()}</TableCell>
                    <TableCell>{token.expiresAt ? new Date(token.expiresAt).toLocaleString() : 'Never'}</TableCell>
                    <TableCell>{token.lastUsedAt ? new Date(token.lastUsedAt).toLocaleString() : 'Never'}</TableCell>
                    <TableCell>
                      {token.revokedAt ? <Badge tone="danger">Revoked</Badge> : <Badge tone="success">Active</Badge>}
                    </TableCell>
                    <TableCell className="text-right">
                      {token.revokedAt ? null : confirmRevoke === token.id ? (
                        <span className="inline-flex items-center gap-2">
                          <span className="text-xs text-slate-300">Revoke?</span>
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-red-300 hover:text-red-200"
                            disabled={revokeMutation.isPending}
                            onClick={() => revokeMutation.mutate(token.id)}
                          >
                            {revokeMutation.isPending ? 'Wait…' : 'Yes'}
                          </Button>
                          <Button variant="ghost" size="sm" onClick={() => setConfirmRevoke(null)}>No</Button>
                        </span>
                      ) : confirmRotate === token.id ? (
                        <span className="inline-flex items-center gap-2">
                          <span className="text-xs text-slate-300">Rotate?</span>
                          <Button
                            variant="ghost"
                            size="sm"
                            disabled={rotateMutation.isPending}
                            onClick={() => rotateMutation.mutate(token.id)}
                          >
                            {rotateMutation.isPending ? 'Wait…' : 'Yes'}
                          </Button>
                          <Button variant="ghost" size="sm" onClick={() => setConfirmRotate(null)}>No</Button>
                        </span>
                      ) : (
                        <span className="inline-flex items-center gap-1">
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-slate-400 hover:text-ocean-400"
                            aria-label={`Rotate ${token.name}`}
                            onClick={() => setConfirmRotate(token.id)}
                          >
                            <RefreshCw aria-hidden="true" className="size-4" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-slate-400 hover:text-red-300"
                            aria-label={`Revoke ${token.name}`}
                            onClick={() => setConfirmRevoke(token.id)}
                          >
                            <Ban aria-hidden="true" className="size-4" />
                          </Button>
                        </span>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
                {tokens.length === 0 ? (
                  <TableRow>
                    <TableCell colSpan={7} className="py-8 text-center text-slate-500">
                      No API tokens exist. Create a token to give automation access.
                    </TableCell>
                  </TableRow>
                ) : null}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

export function OperatorSettingsPage({ client = apiClient }: { client?: IptvApiClient }) {
  const schemaQuery = useQuery(apiQueries(client).settingSchema)
  const [providerId, setProviderId] = useState('')
  const [groupId, setGroupId] = useState('')
  const [activeScope, setActiveScope] = useState<OperatorSettingScope>('global')

  const effectiveQuery = useQuery(apiQueries(client).effectiveSettings(providerId || undefined, groupId || undefined))

  const schema = schemaQuery.data ?? []
  const effective = effectiveQuery.data

  if (!schema.length) return <LoadingPage label="operator settings" />

  const settings = effective?.settings ?? []
  const applyRequirements = effective?.applyRequirements ?? []

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Configuration"
        title="Operator settings"
        description="Inspect effective settings and override global, provider, and channel group behavior."
      />

      <Card>
        <CardHeader>
          <div>
            <p className="text-lg font-semibold text-white">Effective settings</p>
            <p className="mt-1 text-xs text-slate-500">
              Group overrides win over provider, provider over global, and global over system defaults.
            </p>
          </div>
          <SlidersHorizontal className="size-5 text-slate-500" />
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid gap-4 sm:grid-cols-2">
            <div>
              <label htmlFor="effective-provider" className="mb-1 block text-sm text-slate-400">Provider id</label>
              <Input
                id="effective-provider"
                value={providerId}
                onChange={(event) => setProviderId(event.target.value)}
                placeholder="provider-a"
              />
            </div>
            <div>
              <label htmlFor="effective-group" className="mb-1 block text-sm text-slate-400">Channel group id</label>
              <Input
                id="effective-group"
                value={groupId}
                onChange={(event) => setGroupId(event.target.value)}
                placeholder="sports"
              />
            </div>
          </div>
          {applyRequirements.length > 0 && (
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-xs text-slate-500">Apply requirements:</span>
              {applyRequirements.map((requirement) => (
                <Badge key={requirement} tone="info">{applyRequirementLabel[requirement]}</Badge>
              ))}
            </div>
          )}
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-white/8 text-left text-xs uppercase tracking-wide text-slate-500">
                  <th className="py-2 pr-4 font-semibold">Setting</th>
                  <th className="py-2 pr-4 font-semibold">Effective value</th>
                  <th className="py-2 pr-4 font-semibold">Inherited from</th>
                  <th className="py-2 font-semibold">Risk</th>
                </tr>
              </thead>
              <tbody>
                {settings.map((setting: EffectiveSetting) => (
                  <tr key={setting.definition.key} className="border-b border-white/5">
                    <td className="py-2 pr-4">
                      <p className="font-medium text-white">{setting.definition.label}</p>
                      <p className="text-xs text-slate-500">{setting.definition.key}</p>
                    </td>
                    <td className="py-2 pr-4 font-mono text-mint-400">{describeValue(setting.value)}</td>
                    <td className="py-2 pr-4 text-slate-300">{inheritanceLabel(setting.inheritedFrom)}</td>
                    <td className="py-2"><Badge tone={riskTone[setting.definition.risk]}>{setting.definition.risk}</Badge></td>
                  </tr>
                ))}
                {settings.length === 0 && (
                  <tr><td colSpan={4} className="py-8 text-center text-slate-500">No effective settings available.</td></tr>
                )}
              </tbody>
            </table>
          </div>
        </CardContent>
      </Card>

      <div className="flex flex-wrap gap-2">
        {(['global', 'provider', 'group'] as const).map((scope) => (
          <Button
            key={scope}
            variant={activeScope === scope ? 'primary' : 'secondary'}
            onClick={() => setActiveScope(scope)}
          >
            {scopeLabels[scope]}
          </Button>
        ))}
      </div>

      {activeScope === 'global' && (
        <ScopeEditor client={client} scope="global" scopeId="" schema={schema} />
      )}
      {activeScope === 'provider' && (
        <ScopeEditorPanel
          client={client}
          scope="provider"
          schema={schema}
          label="Provider id"
          placeholder="provider-a"
        />
      )}
      {activeScope === 'group' && (
        <ScopeEditorPanel
          client={client}
          scope="group"
          schema={schema}
          label="Channel group id"
          placeholder="sports"
        />
      )}

      <ApiTokensCard client={client} />
    </div>
  )
}
