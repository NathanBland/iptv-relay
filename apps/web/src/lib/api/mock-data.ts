import type {
  Channel,
  EffectiveSetting,
  EventChannel,
  Job,
  OperatorApiToken,
  OperatorOverridesResponse,
  OperatorRevisionResponse,
  OperatorScopeResponse,
  OperatorSettingScope,
  Overview,
  Programme,
  RegionPrefixInfo,
  RegionSettings,
  Session,
  SettingDefinition,
  Source,
} from './types'

export const mockOverview: Overview = {
  channels: 486,
  healthyStreams: 932,
  activeSessions: 6,
  guideCoverage: 96.8,
  providerConnections: 3,
  providerLimit: 3,
}

export const mockSources: Source[] = [
  {
    id: 'source-prime',
    name: 'Prime IPTV',
    kind: 'Xtream',
    state: 'healthy',
    channels: 682,
    lastSync: '2026-08-19T17:57:00Z',
    endpoint: 'https://provider.invalid/player_api.php',
    refreshIntervalSeconds: 3600,
    lastRefreshedAt: '2026-08-19T17:57:00Z',
    maxConnections: 4,
    alternativeBaseUrls: [],
    timezone: 'UTC',
    enabled: true,
  },
  {
    id: 'source-local',
    name: 'Front Range tuner',
    kind: 'Network tuner',
    state: 'healthy',
    channels: 54,
    lastSync: '2026-08-19T17:49:00Z',
    endpoint: 'http://tuner.local/discover.json',
    refreshIntervalSeconds: 0,
    lastRefreshedAt: null,
    maxConnections: 1,
    alternativeBaseUrls: [],
    timezone: 'America/Denver',
    enabled: true,
  },
  {
    id: 'source-guide',
    name: 'North America guide',
    kind: 'XMLTV',
    state: 'syncing',
    channels: 731,
    lastSync: '2026-08-19T16:45:00Z',
    endpoint: 'https://guide.invalid/xmltv.xml.gz',
    refreshIntervalSeconds: 7200,
    lastRefreshedAt: '2026-08-19T16:45:00Z',
    maxConnections: 1,
    alternativeBaseUrls: [],
    timezone: 'UTC',
    enabled: true,
  },
]

const channelSeeds = [
  ['2.1', 'KWGN Denver', 'Denver locals', 'KWGN-DT.us', 'H.264', 7_800, 'healthy'],
  ['4.1', 'KCNC CBS Denver', 'Denver locals', 'KCNC-DT.us', 'H.264', 8_200, 'healthy'],
  ['7.1', 'KMGH ABC Denver', 'Denver locals', 'KMGH-DT.us', 'H.265', 6_400, 'healthy'],
  ['9.1', 'KUSA NBC Denver', 'Denver locals', 'KUSA-DT.us', 'H.264', 7_600, 'degraded'],
  ['20', 'ESPN', 'Sports', 'ESPN.us', 'H.265', 5_900, 'healthy'],
  ['21', 'ESPN2', 'Sports', 'ESPN2.us', 'H.264', 6_100, 'healthy'],
  ['22', 'NFL Network', 'Sports', 'NFLNetwork.us', 'H.265', 5_400, 'healthy'],
  ['23', 'MLB Network', 'Sports', 'MLBNetwork.us', 'H.264', 5_700, 'healthy'],
  ['24', 'NBA TV', 'Sports', 'NBATV.us', 'H.264', 5_500, 'offline'],
  ['40', 'HBO East', 'Premium', 'HBO.us', 'H.265', 4_900, 'healthy'],
  ['41', 'HBO West', 'Premium', 'HBOW.us', 'H.265', 4_800, 'healthy'],
  ['42', 'Showtime East', 'Premium', 'SHO.us', 'H.264', 5_100, 'healthy'],
  ['60', 'CNN', 'News', 'CNN.us', 'H.264', 4_200, 'healthy'],
  ['61', 'BBC News', 'News', 'BBCNews.uk', 'H.264', 3_900, 'healthy'],
  ['90', 'NFL Event 01', 'Dynamic events', 'event-nfl-01', 'H.265', 6_800, 'syncing'],
  ['91', 'NFL Event 02', 'Dynamic events', 'event-nfl-02', 'H.265', 6_700, 'healthy'],
] as const

export const mockChannels: Channel[] = channelSeeds.map(
  ([number, name, group, tvgId, primaryCodec, bitrateKbps, state], index) => ({
    id: `channel-${index + 1}`,
    number,
    name,
    group,
    tvgId,
    streams: index % 3 === 0 ? 3 : 2,
    primaryCodec,
    bitrateKbps,
    state,
    enabled: true,
  }),
)

export const mockProgrammes: Programme[] = [
  {
    id: 'program-1',
    channel: 'KWGN Denver',
    title: 'Colorado’s Own News at 6',
    start: '2026-08-19T18:00:00Z',
    end: '2026-08-19T19:00:00Z',
    source: 'North America guide',
    confidence: 99,
  },
  {
    id: 'program-2',
    channel: 'KCNC CBS Denver',
    title: 'CBS News Colorado',
    start: '2026-08-19T18:00:00Z',
    end: '2026-08-19T18:30:00Z',
    source: 'North America guide',
    confidence: 100,
  },
  {
    id: 'program-3',
    channel: 'KMGH ABC Denver',
    title: 'Denver7 News',
    start: '2026-08-19T18:00:00Z',
    end: '2026-08-19T19:00:00Z',
    source: 'North America guide',
    confidence: 98,
  },
  {
    id: 'program-4',
    channel: 'ESPN',
    title: 'SportsCenter',
    start: '2026-08-19T17:30:00Z',
    end: '2026-08-19T19:00:00Z',
    source: 'Prime IPTV EPG',
    confidence: 97,
  },
  {
    id: 'program-5',
    channel: 'NFL Event 01',
    title: 'Denver Broncos vs Kansas City Chiefs',
    start: '2026-08-19T19:00:00Z',
    end: '2026-08-19T22:30:00Z',
    source: 'Dynamic event scheduler',
    confidence: 96,
  },
  ...Array.from({ length: 16 }, (_, index): Programme => ({
    id: `program-extra-${index + 1}`,
    channel: mockChannels[index % mockChannels.length]?.name ?? 'Unknown channel',
    title: `Upcoming programme ${index + 1}`,
    start: new Date(Date.UTC(2026, 7, 19, 19 + Math.floor(index / 4), (index % 4) * 15)).toISOString(),
    end: new Date(Date.UTC(2026, 7, 19, 20 + Math.floor(index / 4), (index % 4) * 15)).toISOString(),
    source: index % 2 === 0 ? 'North America guide' : 'Prime IPTV EPG',
    confidence: 90 + (index % 10),
  })),
]

export const mockJobs: Job[] = [
  {
    id: 'job-source-guide-failed',
    kind: 'refresh-source',
    status: 'failed',
    progress: { stage: 'downloading', percent: 38, message: 'Download source data' },
    attempts: 3,
    maxAttempts: 3,
    lastError: 'the provider closed the connection before the download completed',
    createdAt: '2026-08-19T16:45:00Z',
    updatedAt: '2026-08-19T16:47:12Z',
    completedAt: '2026-08-19T16:47:12Z',
  },
]

export const mockOperatorApiTokens: OperatorApiToken[] = [
  {
    id: 'token-backup-automation',
    name: 'Backup automation',
    scopes: ['read'],
    expiresAt: null,
    revokedAt: null,
    createdBy: 'admin',
    createdAt: '2026-08-10T09:00:00Z',
    lastUsedAt: '2026-08-20T11:55:00Z',
  },
  {
    id: 'token-legacy-export',
    name: 'Legacy export',
    scopes: ['read', 'output'],
    expiresAt: '2026-09-01T00:00:00Z',
    revokedAt: '2026-08-15T14:30:00Z',
    createdBy: 'admin',
    createdAt: '2026-07-20T09:00:00Z',
    lastUsedAt: '2026-08-14T18:20:00Z',
  },
]

export const mockEvents: EventChannel[] = []

export const mockSessions: Session[] = [
  {
    providerPoolId: 'provider-prime',
    sourceId: 'source-kwgn-hd',
    adapter: 'vlc',
    configuredGeneration: 4,
    upstreamGeneration: 4,
    state: 'streaming',
    viewerCount: 2,
    retainedPackets: 21_276,
    capacityPackets: 42_553,
    lagEvents: 0,
    wrapEvents: 4,
    overwrittenPackets: 18_800,
    reconnectAttempts: 0,
    failoverAttempts: 0,
    failureCount: 0,
    lastFailure: null,
    providerCapacity: 3,
    providerActiveSessions: 3,
    providerHighWatermark: 3,
    providerAvailableSlots: 0,
  },
  {
    providerPoolId: 'provider-prime',
    sourceId: 'source-espn-hevc',
    adapter: 'vlc',
    configuredGeneration: 9,
    upstreamGeneration: 9,
    state: 'streaming',
    viewerCount: 2,
    retainedPackets: 18_400,
    capacityPackets: 36_800,
    lagEvents: 1,
    wrapEvents: 3,
    overwrittenPackets: 12_220,
    reconnectAttempts: 1,
    failoverAttempts: 0,
    failureCount: 1,
    lastFailure: 'upstream-ended',
    providerCapacity: 3,
    providerActiveSessions: 3,
    providerHighWatermark: 3,
    providerAvailableSlots: 0,
  },
  {
    providerPoolId: 'provider-prime',
    sourceId: 'source-nfl-event-01',
    adapter: 'ffmpeg',
    configuredGeneration: 12,
    upstreamGeneration: 13,
    state: 'recovering',
    viewerCount: 2,
    retainedPackets: 15_000,
    capacityPackets: 40_000,
    lagEvents: 2,
    wrapEvents: 7,
    overwrittenPackets: 24_440,
    reconnectAttempts: 2,
    failoverAttempts: 1,
    failureCount: 2,
    lastFailure: 'http',
    providerCapacity: 3,
    providerActiveSessions: 3,
    providerHighWatermark: 3,
    providerAvailableSlots: 0,
  },
]

export const mockRegionSettings: RegionSettings = {
  timezone: 'America/New_York',
  enabledPrefixes: ['US', 'USA', 'EN'],
  suggestedPrefixes: ['US', 'USA', 'EN'],
  autoDetected: true,
}

export const mockRegionPrefixes: RegionPrefixInfo[] = [
  { prefix: 'US', groupCount: 8, channelCount: 120, suggested: true },
  { prefix: 'USA', groupCount: 4, channelCount: 56, suggested: true },
  { prefix: 'EN', groupCount: 3, channelCount: 42, suggested: true },
  { prefix: 'UK', groupCount: 2, channelCount: 34, suggested: false },
  { prefix: 'FR', groupCount: 2, channelCount: 28, suggested: false },
  { prefix: 'DE', groupCount: 2, channelCount: 24, suggested: false },
  { prefix: 'AF', groupCount: 1, channelCount: 12, suggested: false },
  { prefix: 'ARA', groupCount: 1, channelCount: 16, suggested: false },
]

export const mockSettingSchema: SettingDefinition[] = [
  {
    key: 'media.ring.duration_seconds',
    label: 'Live ring duration',
    description: 'Keeps this many recent seconds for new or temporarily slow viewers.',
    valueKind: 'integer',
    defaultValue: 8,
    minimum: 1,
    maximum: 120,
    choices: [],
    unit: 'seconds',
    operationalEffect: 'Higher values improve jitter tolerance and increase memory and tune latency.',
    risk: 'capacity',
    applyRequirement: 'immediate',
    providerOverridable: true,
    groupOverridable: false,
  },
  {
    key: 'media.adapter',
    label: 'Input adapter',
    description: 'Selects native MPEG-TS ingestion or a fixed FFmpeg/VLC stream-copy adapter.',
    valueKind: 'choice',
    defaultValue: 'auto',
    choices: ['auto', 'native-ts', 'ffmpeg', 'vlc'],
    operationalEffect: 'Changing adapters can alter compatibility, startup time, and reconnect behavior.',
    risk: 'compatibility',
    applyRequirement: 'immediate',
    providerOverridable: true,
    groupOverridable: false,
  },
  {
    key: 'events.inferred_duration_seconds',
    label: 'Inferred event duration',
    description: 'Sets the programme length when a dynamic channel title contains a start time but no end time.',
    valueKind: 'integer',
    defaultValue: 10800,
    minimum: 300,
    maximum: 86400,
    choices: [],
    unit: 'seconds',
    operationalEffect: 'Generated filler is recalculated around the inferred programme interval.',
    risk: 'low',
    applyRequirement: 'reimport',
    providerOverridable: true,
    groupOverridable: true,
  },
]

export const mockEffectiveSettings: EffectiveSetting[] = mockSettingSchema.map((definition) => ({
  definition,
  value: definition.defaultValue,
  inheritedFrom: { scope: 'system_default' },
}))

export const mockOperatorOverrides: OperatorOverridesResponse = {
  global: { 'media.ring.duration_seconds': 12 },
  providers: { 'provider-a': { 'media.ring.duration_seconds': 16 } },
  groups: { sports: { 'events.inferred_duration_seconds': 9000 } },
}

export function mockOperatorScope(scope: OperatorSettingScope, scopeId: string): OperatorScopeResponse {
  if (scope === 'global') {
    return { scope, scopeId: '', overrides: { ...mockOperatorOverrides.global }, revision: 1 }
  }
  const bucket = scope === 'provider' ? mockOperatorOverrides.providers : mockOperatorOverrides.groups
  return {
    scope,
    scopeId,
    overrides: { ...bucket[scopeId] },
    revision: 1,
  }
}

export const mockOperatorRevisions: OperatorRevisionResponse[] = [
  {
    revision: 2,
    actor: 'operator',
    createdAt: '2026-08-20T12:01:00Z',
    beforeValue: { 'media.ring.duration_seconds': 12 },
    afterValue: { 'media.ring.duration_seconds': 20 },
  },
  {
    revision: 1,
    actor: 'operator',
    createdAt: '2026-08-20T12:00:00Z',
    beforeValue: null,
    afterValue: { 'media.ring.duration_seconds': 12 },
  },
]
