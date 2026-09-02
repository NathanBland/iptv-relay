import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { History, RotateCcw, Save, SlidersHorizontal } from 'lucide-react'
import { useMemo, useState } from 'react'
import { PageHeader } from '@/components/page-header'
import { LoadingPage } from '@/components/loading-page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
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
    </div>
  )
}
