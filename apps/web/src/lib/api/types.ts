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
}

export interface SourceInput {
  name: string
  kind: SourceKind
  endpoint: string
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
  limit?: number
  offset?: number
}

export interface ProgrammeQuery {
  channelId?: string
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

export interface DynamicEvent {
  id: string
  group: string
  rawTitle: string
  programmeTitle: string
  channelSlot: string
  start: string
  state: 'scheduled' | 'live' | 'ambiguous' | 'unmatched'
  template: string
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

export interface JellyfinConfig {
  baseUrl: string
  tunerName: string
  publicBaseUrl: string
  guideDays: number
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

export interface IptvApiClient {
  getAuthStatus(): Promise<AuthStatus>
  login(input: LoginInput): Promise<AuthStatus>
  logout(): Promise<SaveResult>
  getOverview(): Promise<Overview>
  getSources(): Promise<Source[]>
  createSource(input: SourceInput): Promise<Source>
  getChannels(query?: ChannelQuery): Promise<ChannelPage>
  getProgrammes(query?: ProgrammeQuery): Promise<ProgrammePage>
  getEvents(): Promise<DynamicEvent[]>
  getSessions(): Promise<Session[]>
  saveJellyfin(config: JellyfinConfig): Promise<SaveResult>
}
