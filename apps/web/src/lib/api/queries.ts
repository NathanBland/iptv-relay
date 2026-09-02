import { queryOptions } from '@tanstack/react-query'
import { apiClient } from './client'
import type { ChannelQuery, IptvApiClient, OperatorSettingScope, ProgrammeQuery } from './types'

export function apiQueries(client: IptvApiClient = apiClient) {
  return {
    authStatus: queryOptions({ queryKey: ['auth', 'status'], queryFn: () => client.getAuthStatus(), retry: false }),
    overview: queryOptions({ queryKey: ['overview'], queryFn: () => client.getOverview() }),
    sources: queryOptions({ queryKey: ['sources'], queryFn: () => client.getSources() }),
    sourceSyncStatus: (sourceId: string) => queryOptions({
      queryKey: ['source-sync-status', sourceId],
      queryFn: () => client.getSourceSyncStatus(sourceId),
      staleTime: Infinity,
      enabled: !!sourceId,
    }),
    groups: queryOptions({ queryKey: ['groups'], queryFn: () => client.getGroups() }),
    regionSettings: queryOptions({ queryKey: ['region-settings'], queryFn: () => client.getRegionSettings() }),
    channels: (query?: ChannelQuery) => queryOptions({ queryKey: ['channels', query ?? null], queryFn: () => client.getChannels(query) }),
    programmes: (query?: ProgrammeQuery) => queryOptions({ queryKey: ['programmes', query ?? null], queryFn: () => client.getProgrammes(query) }),
    events: queryOptions({ queryKey: ['events'], queryFn: () => client.getEvents() }),
    eventTemplates: queryOptions({ queryKey: ['event-templates'], queryFn: () => client.getEventTemplates() }),
    eventChannels: (templateId?: string) => queryOptions({ queryKey: ['event-channels', templateId ?? null], queryFn: () => client.getEventChannels(templateId) }),
    lineupTemplates: queryOptions({ queryKey: ['lineup-templates'], queryFn: () => client.getLineupTemplates() }),
    sessions: queryOptions({ queryKey: ['sessions'], queryFn: () => client.getSessions() }),
    jellyfinSetup: queryOptions({ queryKey: ['jellyfin-setup'], queryFn: () => client.getJellyfinSetup() }),
    streamHealthStats: () => queryOptions({ queryKey: ['stream-health-stats'], queryFn: () => client.getStreamHealthStats() }),
    streamHealth: (status?: string, group?: string, limit?: number) => queryOptions({ queryKey: ['stream-health', status ?? null, group ?? null, limit ?? null], queryFn: () => client.getStreamHealth(status, group, limit) }),
    users: queryOptions({ queryKey: ['users'], queryFn: () => client.getUsers() }),
    channelAliases: (country?: string) => queryOptions({ queryKey: ['channel-aliases', country ?? null], queryFn: () => client.getChannelAliases(country) }),
    recordingRules: queryOptions({ queryKey: ['recording-rules'], queryFn: () => client.getRecordingRules() }),
    recordings: (status?: string) => queryOptions({ queryKey: ['recordings', status ?? null], queryFn: () => client.getRecordings(status) }),
    recordingStats: queryOptions({ queryKey: ['recording-stats'], queryFn: () => client.getRecordingStats() }),
    streamProfiles: queryOptions({ queryKey: ['stream-profiles'], queryFn: () => client.getStreamProfiles() }),
    settingSchema: queryOptions({ queryKey: ['settings', 'schema'], queryFn: () => client.getSettingSchema() }),
    effectiveSettings: (providerId?: string, groupId?: string) => queryOptions({
      queryKey: ['settings', 'effective', providerId ?? null, groupId ?? null],
      queryFn: () => client.getEffectiveSettings(providerId, groupId),
    }),
    operatorOverrides: queryOptions({ queryKey: ['settings', 'overrides'], queryFn: () => client.getOperatorOverrides() }),
    operatorScope: (scope: OperatorSettingScope, scopeId: string) => queryOptions({
      queryKey: ['settings', 'scope', scope, scopeId],
      queryFn: () => client.getOperatorScope(scope, scopeId),
    }),
    operatorRevisions: (scope: OperatorSettingScope, scopeId: string) => queryOptions({
      queryKey: ['settings', 'revisions', scope, scopeId],
      queryFn: () => client.listOperatorRevisions(scope, scopeId),
    }),
    reconciliationRevisions: (sourceId: string) => queryOptions({
      queryKey: ['reconciliation', 'revisions', sourceId],
      queryFn: () => client.listReconciliationRevisions(sourceId),
    }),
  }
}
