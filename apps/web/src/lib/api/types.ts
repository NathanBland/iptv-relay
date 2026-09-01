export type HealthState = 'healthy' | 'degraded' | 'offline' | 'syncing'
export type SourceKind = 'M3U' | 'Xtream' | 'XMLTV' | 'Network tuner'

export interface Overview {
  channels: number
  healthyStreams: number
  activeSessions: number
  guideCoverage: number
  providerConnections: number
  providerLimit: number
}

export interface Source {
  id: string
  name: string
  kind: SourceKind
  state: HealthState
  channels: number
  lastSync: string
  endpoint: string
  refreshIntervalSeconds: number
  lastRefreshedAt: string | null
  maxConnections: number
  timezone: string
  enabled: boolean
}

export interface SourceUpdateInput {
  maxConnections?: number
  timezone?: string
  enabled?: boolean
}

export interface SourceInput {
  name: string
  kind: SourceKind
  endpoint: string
  timezone?: string
}

export interface SourceSyncStatus {
  jobId: string
  status: 'running' | 'queued' | 'succeeded' | 'failed' | 'cancelled'
  stage: 'downloading' | 'parsing' | 'activating' | 'reconciling' | 'completed'
  percent: number
  message: string
  bytesDownloaded?: number
  recordsProcessed?: number
  startedAt?: string
  updatedAt?: string
}

export interface Channel {
  id: string
  number: string
  name: string
  group: string
  tvgId: string
  streams: number
  primaryCodec: string
  bitrateKbps: number
  state: HealthState
  enabled: boolean
}

export interface Page<T> {
  total: number
  limit: number
  offset: number
  items: T[]
}

export type ChannelPage = Page<Channel>
export type ProgrammePage = Page<Programme>

export interface ChannelQuery {
  search?: string
  group?: string
  enabled?: boolean
  limit?: number
  offset?: number
}

export interface ChannelPreview {
  streamUrl: string
  contentType: string
}

export interface Group {
  name: string
  channelCount: number
  enabledCount: number
}

export interface ProgrammeQuery {
  channelId?: string
  search?: string
  limit?: number
  offset?: number
}

export interface Programme {
  id: string
  channel: string
  title: string
  start: string
  end: string
  source: string
  confidence: number
}

export interface EpgMapping {
  channelId: string
  epgChannelId: string
  method: 'tvg-id' | 'exact-name' | 'alias' | string
  confidence: number
  evidence: Record<string, unknown>
  reviewStatus: 'applied' | 'review' | 'rejected' | 'manual'
  reviewedBy: string | null
  reviewedAt: string | null
  revision: number
  updatedAt: string
  channelName: string
  canonicalKey: string | null
  epgXmltvId: string | null
  epgDisplayName: string | null
}

export interface EpgMappingPage {
  total: number
  limit: number
  offset: number
  items: EpgMapping[]
}

export interface UnmappedChannel {
  id: string
  name: string
  canonicalKey: string | null
  groupName: string | null
}

export interface UnmappedChannelPage {
  total: number
  limit: number
  offset: number
  items: UnmappedChannel[]
}

export interface ReviewCandidate {
  id: string
  channelId: string
  epgChannelId: string
  method: string
  confidence: number
  evidence: Record<string, unknown>
  createdAt: string
  epgXmltvId: string | null
  epgDisplayName: string | null
}

export interface EpgChannelSearchResult {
  id: string
  xmltvId: string
  displayName: string | null
}

export interface EpgReconcileResult {
  mappingsApplied: number
  mappingsRemoved: number
  reviewQueued: number
}

export interface StreamHealthItem {
  providerStreamId: string
  streamName: string
  groupName: string | null
  healthStatus: 'alive' | 'dead' | 'unknown' | 'checking'
  healthCheckedAt: string | null
  healthError: string | null
  videoCodec: string | null
  videoResolution: string | null
  videoWidth: number | null
  videoHeight: number | null
  videoFps: number | null
  audioCodec: string | null
  audioChannels: number | null
  audioSampleRate: number | null
  bitrateKbps: number | null
  providerAccountId: string
}

export interface StreamHealthPage {
  total: number
  items: StreamHealthItem[]
}

export interface StreamHealthStats {
  alive: number
  dead: number
  unknown: number
  checking: number
}

export interface BestStream {
  channelId: string
  channelName: string
  providerStreamId: string
  streamName: string
  priority: number
  qualityRank: number
  healthStatus: 'alive' | 'dead' | 'unknown' | 'checking'
  videoWidth: number | null
  videoHeight: number | null
  videoFps: number | null
  videoCodec: string | null
  failoverCount: number
  lastFailoverAt: string | null
  urlTemplate: string
  providerAccountId: string
}

export interface User {
  id: string
  username: string
  displayName: string
  role: 'admin' | 'operator' | 'viewer'
  enabled: boolean
  lastLoginAt: string | null
  createdAt: string
  updatedAt: string
}

export interface CreateUserInput {
  username: string
  displayName: string
  password: string
  role?: 'admin' | 'operator' | 'viewer'
}

export interface UpdateUserInput {
  displayName?: string
  password?: string
  role?: 'admin' | 'operator' | 'viewer'
  enabled?: boolean
}

export interface ChannelAlias {
  id: string
  canonicalName: string
  alias: string
  country: string | null
  category: string | null
  createdAt: string
}

export interface ChannelAliasPage {
  total: number
  items: ChannelAlias[]
}

export interface CreateChannelAliasInput {
  canonicalName: string
  alias: string
  country?: string
  category?: string
}

export interface ResolveAliasResult {
  canonicalName: string | null
  input: string
}

export interface RecordingRule {
  id: string
  name: string
  channelId: string
  ruleType: 'one-time' | 'recurring' | 'series'
  titleFilter: string | null
  categoryFilter: string | null
  startPaddingMinutes: number
  endPaddingMinutes: number
  maxRecordings: number | null
  keepUntil: 'space-needed' | 'one-day' | 'one-week' | 'until-watched' | 'forever'
  enabled: boolean
  createdAt: string
  updatedAt: string
}

export interface CreateRecordingRuleInput {
  name: string
  channelId: string
  ruleType?: 'one-time' | 'recurring' | 'series'
  titleFilter?: string
  categoryFilter?: string
  startPaddingMinutes?: number
  endPaddingMinutes?: number
  maxRecordings?: number
  keepUntil?: 'space-needed' | 'one-day' | 'one-week' | 'until-watched' | 'forever'
}

export interface Recording {
  id: string
  ruleId: string | null
  channelId: string
  programmeId: string | null
  title: string
  description: string | null
  startsAt: string
  endsAt: string
  status: 'scheduled' | 'recording' | 'completed' | 'failed' | 'cancelled'
  filePath: string | null
  fileSizeBytes: number | null
  durationSeconds: number | null
  errorMessage: string | null
  createdAt: string
  updatedAt: string
}

export interface RecordingPage {
  total: number
  items: Recording[]
}

export interface CreateRecordingInput {
  ruleId?: string
  channelId: string
  programmeId?: string
  title: string
  description?: string
  startsAt: string
  endsAt: string
}

export interface RecordingStats {
  scheduled: number
  recording: number
  completed: number
  failed: number
  totalBytes: number
}

export interface StreamProfile {
  id: string
  name: string
  profileType: 'direct' | 'ffmpeg' | 'vlc' | 'streamlink' | 'custom'
  command: string | null
  arguments: unknown[]
  bufferSeconds: number
  userAgent: string | null
  referer: string | null
  enabled: boolean
  createdAt: string
  updatedAt: string
}

export interface CreateStreamProfileInput {
  name: string
  profileType?: 'direct' | 'ffmpeg' | 'vlc' | 'streamlink' | 'custom'
  command?: string
  arguments?: unknown[]
  bufferSeconds?: number
  userAgent?: string
  referer?: string
}

export interface EventTemplate {
  id: string
  name: string
  displayName: string
  matchRegex: string
  channelNameFormat: string
  groupName: string
  eventDurationHours: number
  pastDateGraceHours: number
  futureDateDays: number
  enabled: boolean
}

export interface EventTemplateSuggestion {
  name: string
  displayName: string
  matchRegex: string
  channelNameFormat: string
  groupName: string
  eventDurationHours: number
  pastDateGraceHours: number
  futureDateDays: number
  sampleStreams: string[]
  streamCount: number
}

export interface EventChannel {
  id: string
  templateId: string
  channelId: string | null
  slotNumber: number
  eventTitle: string | null
  eventStart: string | null
  eventEnd: string | null
  rawStreamName: string | null
  state: 'scheduled' | 'live' | 'ended' | 'hidden'
}

export interface LineupTemplate {
  id: string
  name: string
  packageName: string
  country: string
  description: string | null
  enabled: boolean
}

export interface CreateEventTemplateInput {
  name: string
  displayName: string
  matchRegex: string
  channelNameFormat: string
  groupName: string
  eventDurationHours?: number
  pastDateGraceHours?: number
  futureDateDays?: number
}

export interface CreateLineupTemplateInput {
  name: string
  packageName: string
  country: string
  description?: string
}

export type SessionState =
  | 'idle'
  | 'reserving'
  | 'starting'
  | 'priming'
  | 'streaming'
  | 'recovering'
  | 'failing-over'
  | 'stopping'
  | 'failed'

export type SessionFailure =
  | 'upstream-ended'
  | 'http'
  | 'packetization'
  | 'priming'
  | 'recovery-expired'

export interface Session {
  providerPoolId: string
  sourceId: string
  configuredGeneration: number
  upstreamGeneration: number
  state: SessionState
  viewerCount: number
  retainedPackets: number
  capacityPackets: number
  lagEvents: number
  wrapEvents: number
  overwrittenPackets: number
  reconnectAttempts: number
  failoverAttempts: number
  failureCount: number
  lastFailure: SessionFailure | null
  providerCapacity: number
  providerActiveSessions: number
  providerHighWatermark: number
  providerAvailableSlots: number
}

export type JellyfinSetupStatus = 'available' | 'regeneration-required'

export interface JellyfinSetup {
  status: JellyfinSetupStatus
  playlistUrl?: string | undefined
  xmltvUrl?: string | undefined
  hdhrDeviceUrl?: string | undefined
  guideDaysMax: number
}

export interface RotateJellyfinTokenInput {
  overlapSeconds?: number
}

export interface SaveResult {
  ok: boolean
  message: string
}

export interface AuthUser {
  id: string
  username: string
  displayName: string
}

export type AuthStatus =
  | { authenticated: false }
  | { authenticated: true; user: AuthUser }

export interface LoginInput {
  username: string
  password: string
}

export interface ProblemDetails {
  type: string
  title: string
  status: number
  detail?: string
  instance?: string
}

export interface RegionSettings {
  timezone: string
  enabledPrefixes: string[]
  suggestedPrefixes: string[]
  autoDetected: boolean
}

export interface RegionPrefixInfo {
  prefix: string
  groupCount: number
  channelCount: number
  suggested: boolean
}

export interface RegionSettingsResponse {
  settings: RegionSettings
  prefixes: RegionPrefixInfo[]
}

export interface IptvApiClient {
  getAuthStatus(): Promise<AuthStatus>
  login(input: LoginInput): Promise<AuthStatus>
  logout(): Promise<SaveResult>
  getOverview(): Promise<Overview>
  getSources(): Promise<Source[]>
  createSource(input: SourceInput): Promise<Source>
  deleteSource(id: string): Promise<void>
  setSourceRefreshInterval(id: string, refreshIntervalSeconds: number): Promise<void>
  triggerSourceSync(id: string): Promise<{ jobId: string; message: string }>
  cancelSourceSync(sourceId: string): Promise<{ ok: boolean; message: string }>
  getSourceSyncStatus(sourceId: string): Promise<SourceSyncStatus>
  updateSource(id: string, input: SourceUpdateInput): Promise<void>
  getGroups(): Promise<Group[]>
  getRegionSettings(): Promise<RegionSettingsResponse>
  updateRegionSettings(input: { timezone: string; enabledPrefixes: string[] }): Promise<RegionSettings>
  applyRegionFilter(input: { enabledPrefixes: string[] }): Promise<{ enabled: number; disabled: number }>
  getChannels(query?: ChannelQuery): Promise<ChannelPage>
  getChannelPreview(channelId: string): Promise<ChannelPreview>
  setChannelEnabled(channelId: string, enabled: boolean): Promise<SaveResult>
  setGroupEnabled(groupName: string, enabled: boolean): Promise<SaveResult>
  setAllGroupsEnabled(enabled: boolean): Promise<SaveResult>
  getProgrammes(query?: ProgrammeQuery): Promise<ProgrammePage>
  reconcileEpg(): Promise<EpgReconcileResult>
  getEpgMappings(reviewStatus?: string, limit?: number, offset?: number): Promise<EpgMappingPage>
  getUnmappedChannels(search?: string, limit?: number, offset?: number): Promise<UnmappedChannelPage>
  getReviewCandidates(channelId: string): Promise<ReviewCandidate[]>
  searchEpgChannels(query: string, limit?: number): Promise<EpgChannelSearchResult[]>
  setChannelEpgMapping(channelId: string, epgChannelId: string): Promise<void>
  removeChannelEpgMapping(channelId: string): Promise<void>
  resolveReview(channelId: string, accept: boolean, epgChannelId?: string): Promise<void>
  getEvents(): Promise<EventChannel[]>
  getEventTemplates(): Promise<EventTemplate[]>
  createEventTemplate(input: CreateEventTemplateInput): Promise<EventTemplate>
  deleteEventTemplate(id: string): Promise<void>
  getEventChannels(templateId?: string): Promise<EventChannel[]>
  scanEventTemplate(id: string): Promise<SaveResult>
  suggestEventTemplates(): Promise<EventTemplateSuggestion[]>
  getLineupTemplates(): Promise<LineupTemplate[]>
  createLineupTemplate(input: CreateLineupTemplateInput): Promise<LineupTemplate>
  deleteLineupTemplate(id: string): Promise<void>
  applyLineupTemplate(id: string): Promise<SaveResult>
  getSessions(): Promise<Session[]>
  getJellyfinSetup(): Promise<JellyfinSetup>
  rotateJellyfinToken(input?: RotateJellyfinTokenInput): Promise<JellyfinSetup>
  getStreamHealth(status?: string, group?: string, limit?: number, offset?: number): Promise<StreamHealthPage>
  getStreamHealthStats(): Promise<StreamHealthStats>
  triggerHealthCheck(limit?: number): Promise<{ queued: number }>
  rankAllStreams(): Promise<{ ranked: number }>
  getBestStream(channelId: string): Promise<BestStream>
  getUsers(): Promise<User[]>
  createUser(input: CreateUserInput): Promise<User>
  updateUser(id: string, input: UpdateUserInput): Promise<User>
  deleteUser(id: string): Promise<void>
  getChannelAliases(country?: string, limit?: number, offset?: number): Promise<ChannelAliasPage>
  createChannelAlias(input: CreateChannelAliasInput): Promise<ChannelAlias>
  deleteChannelAlias(id: string): Promise<void>
  resolveChannelAlias(name: string): Promise<ResolveAliasResult>
  getRecordingRules(): Promise<RecordingRule[]>
  createRecordingRule(input: CreateRecordingRuleInput): Promise<RecordingRule>
  deleteRecordingRule(id: string): Promise<void>
  getRecordings(status?: string, limit?: number, offset?: number): Promise<RecordingPage>
  createRecording(input: CreateRecordingInput): Promise<Recording>
  deleteRecording(id: string): Promise<void>
  getRecordingStats(): Promise<RecordingStats>
  getStreamProfiles(): Promise<StreamProfile[]>
  createStreamProfile(input: CreateStreamProfileInput): Promise<StreamProfile>
  deleteStreamProfile(id: string): Promise<void>
  assignStreamProfile(channelId: string, profileId: string): Promise<void>
  removeStreamProfile(channelId: string): Promise<void>
}
