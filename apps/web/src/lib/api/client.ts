import {
  mockChannels,
  mockOverview,
  mockProgrammes,
  mockRegionPrefixes,
  mockRegionSettings,
  mockSessions,
  mockSources,
} from './mock-data'
import type {
  Channel,
  ChannelPage,
  ChannelPreview,
  ChannelQuery,
  AuthStatus,
  CreateEventTemplateInput,
  CreateLineupTemplateInput,
  EventChannel,
  EventTemplate,
  EventTemplateSuggestion,
  Group,
  IptvApiClient,
  JellyfinConfig,
  LineupTemplate,
  Overview,
  LoginInput,
  Page,
  Programme,
  ProgrammePage,
  ProgrammeQuery,
  EpgMapping,
  EpgMappingPage,
  EpgReconcileResult,
  EpgChannelSearchResult,
  ReviewCandidate,
  UnmappedChannel,
  UnmappedChannelPage,
  SaveResult,
  Session,
  Source,
  SourceInput,
  SourceSyncStatus,
  SourceUpdateInput,
  ProblemDetails,
  StreamHealthItem,
  StreamHealthPage,
  StreamHealthStats,
  BestStream,
  User,
  CreateUserInput,
  UpdateUserInput,
  ChannelAlias,
  ChannelAliasPage,
  CreateChannelAliasInput,
  ResolveAliasResult,
  RecordingRule,
  CreateRecordingRuleInput,
  Recording,
  RecordingPage,
  CreateRecordingInput,
  RecordingStats,
  RegionPrefixInfo,
  RegionSettings,
  RegionSettingsResponse,
  StreamProfile,
  CreateStreamProfileInput,
} from './types'

type Fetcher = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>
type UnauthorizedHandler = (apiPath: string) => void
type CsrfTokenReader = () => string | undefined
type JsonRecord = Record<string, unknown>

const API_PATHS = {
  authStatus: '/api/v1/auth/status',
  login: '/api/v1/auth/login',
  logout: '/api/v1/auth/logout',
  system: '/api/v1/system',
  sources: '/api/v1/sources',
  channels: '/api/v1/channels',
  programmes: '/api/v1/programmes',
  events: '/api/v1/events',
  eventTemplates: '/api/v1/event-templates',
  eventChannels: '/api/v1/event-channels',
  lineupTemplates: '/api/v1/lineup-templates',
  sessions: '/api/v1/sessions',
  jellyfin: '/api/v1/jellyfin',
  groups: '/api/v1/groups',
  regionSettings: '/api/v1/region-settings',
  streamsHealth: '/api/v1/streams/health',
  streamsHealthStats: '/api/v1/streams/health/stats',
  streamsHealthCheck: '/api/v1/streams/health/check',
  streamsRank: '/api/v1/streams/rank',
  users: '/api/v1/users',
  channelAliases: '/api/v1/channel-aliases',
  recordings: '/api/v1/recordings',
  recordingRules: '/api/v1/recordings/rules',
  recordingStats: '/api/v1/recordings/stats',
  streamProfiles: '/api/v1/stream-profiles',
} as const

const MUTATION_METHODS = new Set(['POST', 'PUT', 'PATCH', 'DELETE'])

function hasControlCharacter(value: string) {
  return [...value].some((character) => {
    const codePoint = character.codePointAt(0) ?? 0
    return codePoint <= 0x1f || codePoint === 0x7f
  })
}

export function safeNextPath(value: unknown) {
  if (
    typeof value !== 'string'
    || !value.startsWith('/')
    || value.startsWith('//')
    || value.includes('\\')
    || hasControlCharacter(value)
  ) return '/'

  const pathname = value.split(/[?#]/, 1)[0]?.replace(/\/+$/, '') || '/'
  return pathname === '/login' ? '/' : value
}

export function readCsrfCookie(cookie = typeof document === 'undefined' ? '' : document.cookie) {
  const encoded = cookie
    .split(';')
    .map((part) => part.trim())
    .find((part) => part.startsWith('iptv_csrf='))
    ?.slice('iptv_csrf='.length)
  if (!encoded) return undefined
  try {
    const token = decodeURIComponent(encoded)
    return token && token.length <= 1_024 && !/[\r\n]/.test(token) ? token : undefined
  } catch {
    return undefined
  }
}

function redirectToLogin(apiPath: string) {
  if (typeof window === 'undefined' || apiPath.startsWith('/api/v1/auth/') || window.location.pathname === '/login') return
  const next = safeNextPath(`${window.location.pathname}${window.location.search}${window.location.hash}`)
  window.location.assign(`/login?next=${encodeURIComponent(next)}`)
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function boundedString(value: unknown, fallback: string, maxLength: number) {
  return typeof value === 'string' && value.trim() ? value.trim().slice(0, maxLength) : fallback
}

function optionalBoundedString(value: unknown, maxLength: number) {
  return typeof value === 'string' && value.trim() ? value.trim().slice(0, maxLength) : undefined
}

export class IptvApiError extends Error {
  readonly problem: ProblemDetails

  constructor(problem: ProblemDetails) {
    super(problem.detail ?? problem.title)
    this.name = 'IptvApiError'
    this.problem = problem
  }
}

function invalidResponse(detail: string) {
  return new IptvApiError({
    type: 'urn:iptv:error:invalid-response',
    title: 'Invalid API response',
    status: 502,
    detail,
  })
}

function asObject<T>(value: unknown, resource: string): T {
  if (!isRecord(value)) throw invalidResponse(`${resource} must be a JSON object.`)
  return value as T
}

function asObjectArray<T>(value: unknown, resource: string): T[] {
  if (!Array.isArray(value) || !value.every(isRecord)) {
    throw invalidResponse(`${resource} must be a JSON array of objects.`)
  }
  return value as T[]
}

function asPage<T>(value: unknown, resource: string): Page<T> {
  if (!isRecord(value)) throw invalidResponse(`${resource} must be a JSON object.`)
  const total = typeof value.total === 'number' ? value.total : 0
  const limit = typeof value.limit === 'number' ? value.limit : 0
  const offset = typeof value.offset === 'number' ? value.offset : 0
  const items = asObjectArray<T>(value.items, `${resource} items`)
  return { total, limit, offset, items }
}

function buildQueryString(params: Record<string, unknown>): string {
  const entries = Object.entries(params).filter(([, value]) => value !== undefined && value !== null && value !== '')
  if (entries.length === 0) return ''
  const search = new URLSearchParams()
  for (const [key, value] of entries) search.set(key, String(value))
  return `?${search.toString()}`
}

async function problemFromResponse(response: Response): Promise<IptvApiError> {
  const fallbackTitle = boundedString(response.statusText, 'Request failed', 160)
  const contentType = response.headers.get('content-type')?.toLowerCase() ?? ''
  let payload: unknown

  if (contentType.includes('json')) {
    try {
      payload = await response.json()
    } catch {
      payload = undefined
    }
  }

  const problem = isRecord(payload) ? payload : {}
  const detail = optionalBoundedString(problem.detail, 500)
  const instance = optionalBoundedString(problem.instance, 500)
  return new IptvApiError({
    type: boundedString(problem.type, 'about:blank', 500),
    title: boundedString(problem.title, fallbackTitle, 160),
    status: response.status,
    ...(detail ? { detail } : {}),
    ...(instance ? { instance } : {}),
  })
}

export class FetchIptvApiClient implements IptvApiClient {
  private readonly fetcher: Fetcher
  private readonly onUnauthorized: UnauthorizedHandler
  private readonly readCsrfToken: CsrfTokenReader
  private redirectingToLogin = false

  constructor(fetcher: Fetcher = globalThis.fetch.bind(globalThis), options: {
    onUnauthorized?: UnauthorizedHandler
    readCsrfToken?: CsrfTokenReader
  } = {}) {
    this.fetcher = fetcher
    this.onUnauthorized = options.onUnauthorized ?? redirectToLogin
    this.readCsrfToken = options.readCsrfToken ?? readCsrfCookie
  }

  private async request<T>(path: string, decode: (value: unknown) => T, init: RequestInit = {}): Promise<T> {
    const headers = new Headers(init.headers)
    headers.set('Accept', 'application/json, application/problem+json')
    const method = init.method?.toUpperCase() ?? 'GET'
    if (init.body !== undefined) headers.set('Content-Type', 'application/json')
    if (MUTATION_METHODS.has(method)) {
      const csrfToken = this.readCsrfToken()
      if (!csrfToken) {
        throw new IptvApiError({
          type: 'urn:iptv:error:missing-csrf-token',
          title: 'CSRF token unavailable',
          status: 403,
          detail: 'Refresh the page before retrying this request.',
        })
      }
      headers.set('X-CSRF-Token', csrfToken)
    }

    let response: Response
    try {
      response = await this.fetcher(path, {
        ...init,
        headers,
        credentials: 'same-origin',
      })
    } catch {
      throw new IptvApiError({
        type: 'urn:iptv:error:network',
        title: 'Unable to reach the IPTV API',
        status: 0,
        detail: 'The same-origin API request failed before a response was received.',
      })
    }

    if (!response.ok) {
      if (response.status === 401 && !path.startsWith('/api/v1/auth/') && !this.redirectingToLogin) {
        this.redirectingToLogin = true
        this.onUnauthorized(path)
      }
      throw await problemFromResponse(response)
    }

    let payload: unknown
    try {
      payload = await response.json()
    } catch {
      throw invalidResponse('The server did not return valid JSON.')
    }
    return decode(payload)
  }

  async getAuthStatus(): Promise<AuthStatus> {
    return this.request(API_PATHS.authStatus, (value) => asObject<AuthStatus>(value, 'Auth status response'))
  }

  async login(input: LoginInput): Promise<AuthStatus> {
    return this.request(API_PATHS.login, (value) => asObject<AuthStatus>(value, 'Login response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async logout(): Promise<SaveResult> {
    return this.request(API_PATHS.logout, (value) => asObject<SaveResult>(value, 'Logout response'), {
      method: 'POST',
      body: JSON.stringify({}),
    })
  }

  async getOverview(): Promise<Overview> {
    return this.request(API_PATHS.system, (value) => asObject<Overview>(value, 'System response'))
  }

  async getSources(): Promise<Source[]> {
    return this.request(API_PATHS.sources, (value) => asObjectArray<Source>(value, 'Sources response'))
  }

  async createSource(input: SourceInput): Promise<Source> {
    return this.request(API_PATHS.sources, (value) => asObject<Source>(value, 'Source response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async deleteSource(id: string): Promise<void> {
    await this.deleteResource(`${API_PATHS.sources}/${encodeURIComponent(id)}`, 'Source')
  }

  async setSourceRefreshInterval(id: string, refreshIntervalSeconds: number): Promise<void> {
    const csrfToken = this.readCsrfToken()
    if (!csrfToken) {
      throw new IptvApiError({
        type: 'urn:iptv:error:missing-csrf-token',
        title: 'CSRF token unavailable',
        status: 400,
        detail: 'Sign in before you change source settings.',
      })
    }
    const headers = new Headers()
    headers.set('Content-Type', 'application/json')
    headers.set('Accept', 'application/json, application/problem+json')
    headers.set('X-CSRF-Token', csrfToken)
    let response: Response
    try {
      response = await this.fetcher(
        `${API_PATHS.sources}/${encodeURIComponent(id)}/refresh-interval`,
        {
          method: 'PATCH',
          headers,
          body: JSON.stringify({ refreshIntervalSeconds }),
          credentials: 'same-origin',
        },
      )
    } catch {
      throw new IptvApiError({
        type: 'urn:iptv:error:network',
        title: 'Unable to reach the IPTV API',
        status: 0,
        detail: 'The same-origin API request failed before a response was received.',
      })
    }
    if (!response.ok) {
      throw await problemFromResponse(response)
    }
  }

  async triggerSourceSync(id: string): Promise<{ jobId: string; message: string }> {
    const csrfToken = this.readCsrfToken()
    if (!csrfToken) {
      throw new IptvApiError({
        type: 'urn:iptv:error:missing-csrf-token',
        title: 'CSRF token unavailable',
        status: 400,
        detail: 'Sign in before you sync a source.',
      })
    }
    const headers = new Headers()
    headers.set('Accept', 'application/json, application/problem+json')
    headers.set('X-CSRF-Token', csrfToken)
    let response: Response
    try {
      response = await this.fetcher(
        `${API_PATHS.sources}/${encodeURIComponent(id)}/sync`,
        {
          method: 'POST',
          headers,
          credentials: 'same-origin',
        },
      )
    } catch {
      throw new IptvApiError({
        type: 'urn:iptv:error:network',
        title: 'Unable to reach the IPTV API',
        status: 0,
        detail: 'The same-origin API request failed before a response was received.',
      })
    }
    if (!response.ok) {
      throw await problemFromResponse(response)
    }
    return await response.json() as { jobId: string; message: string }
  }

  async cancelSourceSync(id: string): Promise<{ ok: boolean; message: string }> {
    const csrfToken = this.readCsrfToken()
    if (!csrfToken) {
      throw new IptvApiError({
        type: 'urn:iptv:error:missing-csrf-token',
        title: 'CSRF token unavailable',
        status: 400,
        detail: 'Sign in before you cancel a sync.',
      })
    }
    const headers = new Headers()
    headers.set('Accept', 'application/json, application/problem+json')
    headers.set('X-CSRF-Token', csrfToken)
    let response: Response
    try {
      response = await this.fetcher(
        `${API_PATHS.sources}/${encodeURIComponent(id)}/sync/cancel`,
        {
          method: 'POST',
          headers,
          credentials: 'same-origin',
        },
      )
    } catch {
      throw new IptvApiError({
        type: 'urn:iptv:error:network',
        title: 'Unable to reach the IPTV API',
        status: 0,
        detail: 'The same-origin API request failed before a response was received.',
      })
    }
    if (!response.ok) {
      throw await problemFromResponse(response)
    }
    return await response.json() as { ok: boolean; message: string }
  }

  async getSourceSyncStatus(sourceId: string): Promise<SourceSyncStatus> {
    return this.request(`${API_PATHS.sources}/${encodeURIComponent(sourceId)}/sync-status`, (value) => asObject<SourceSyncStatus>(value, 'Source sync status response'))
  }

  async updateSource(id: string, input: SourceUpdateInput): Promise<void> {
    const csrfToken = this.readCsrfToken()
    if (!csrfToken) {
      throw new IptvApiError({
        type: 'urn:iptv:error:missing-csrf-token',
        title: 'CSRF token unavailable',
        status: 400,
        detail: 'Sign in before you change source settings.',
      })
    }
    const headers = new Headers()
    headers.set('Content-Type', 'application/json')
    headers.set('Accept', 'application/json, application/problem+json')
    headers.set('X-CSRF-Token', csrfToken)
    let response: Response
    try {
      response = await this.fetcher(
        `${API_PATHS.sources}/${encodeURIComponent(id)}`,
        {
          method: 'PATCH',
          headers,
          body: JSON.stringify(input),
          credentials: 'same-origin',
        },
      )
    } catch {
      throw new IptvApiError({
        type: 'urn:iptv:error:network',
        title: 'Unable to reach the IPTV API',
        status: 0,
        detail: 'The same-origin API request failed before a response was received.',
      })
    }
    if (!response.ok) {
      throw await problemFromResponse(response)
    }
  }

  private async deleteResource(path: string, _resource: string): Promise<void> {
    const csrfToken = this.readCsrfToken()
    if (!csrfToken) {
      throw new IptvApiError({
        type: 'urn:iptv:error:missing-csrf-token',
        title: 'CSRF token unavailable',
        status: 403,
        detail: 'Refresh the page before retrying this request.',
      })
    }
    const headers = new Headers()
    headers.set('Accept', 'application/json, application/problem+json')
    headers.set('X-CSRF-Token', csrfToken)
    let response: Response
    try {
      response = await this.fetcher(path, {
        method: 'DELETE',
        headers,
        credentials: 'same-origin',
      })
    } catch {
      throw new IptvApiError({
        type: 'urn:iptv:error:network',
        title: 'Unable to reach the IPTV API',
        status: 0,
        detail: 'The same-origin API request failed before a response was received.',
      })
    }
    if (!response.ok) {
      if (response.status === 401 && !path.startsWith('/api/v1/auth/') && !this.redirectingToLogin) {
        this.redirectingToLogin = true
        this.onUnauthorized(path)
      }
      throw await problemFromResponse(response)
    }
  }

  async getGroups(): Promise<Group[]> {
    return this.request(API_PATHS.groups, (value) => asObjectArray<Group>(value, 'Groups response'))
  }

  async getRegionSettings(): Promise<RegionSettingsResponse> {
    return this.request(API_PATHS.regionSettings, (value) => asObject<RegionSettingsResponse>(value, 'Region settings response'))
  }

  async updateRegionSettings(input: { timezone: string; enabledPrefixes: string[] }): Promise<RegionSettings> {
    return this.request(API_PATHS.regionSettings, (value) => asObject<RegionSettings>(value, 'Region settings update response'), {
      method: 'PUT',
      body: JSON.stringify(input),
    })
  }

  async applyRegionFilter(input: { enabledPrefixes: string[] }): Promise<{ enabled: number; disabled: number }> {
    return this.request(`${API_PATHS.regionSettings}/apply`, (value) => asObject<{ enabled: number; disabled: number }>(value, 'Region filter apply response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async getChannels(query?: ChannelQuery): Promise<ChannelPage> {
    const qs = buildQueryString({ search: query?.search, group: query?.group, enabled: query?.enabled, limit: query?.limit, offset: query?.offset })
    return this.request(`${API_PATHS.channels}${qs}`, (value) => asPage<Channel>(value, 'Channels response'))
  }

  async getChannelPreview(channelId: string): Promise<ChannelPreview> {
    return this.request(`${API_PATHS.channels}/${encodeURIComponent(channelId)}/preview`, (value) => asObject<ChannelPreview>(value, 'Channel preview response'))
  }

  async setChannelEnabled(channelId: string, enabled: boolean): Promise<SaveResult> {
    return this.request(`${API_PATHS.channels}/${encodeURIComponent(channelId)}/enabled`, (value) => asObject<SaveResult>(value, 'Channel enabled response'), {
      method: 'PATCH',
      body: JSON.stringify({ enabled }),
    })
  }

  async setGroupEnabled(groupName: string, enabled: boolean): Promise<SaveResult> {
    return this.request(`${API_PATHS.groups}/${encodeURIComponent(groupName)}/enabled`, (value) => asObject<SaveResult>(value, 'Group enabled response'), {
      method: 'PATCH',
      body: JSON.stringify({ enabled }),
    })
  }

  async setAllGroupsEnabled(enabled: boolean): Promise<SaveResult> {
    return this.request(`${API_PATHS.groups}/enabled`, (value) => asObject<SaveResult>(value, 'Bulk group enabled response'), {
      method: 'PATCH',
      body: JSON.stringify({ enabled }),
    })
  }

  async getProgrammes(query?: ProgrammeQuery): Promise<ProgrammePage> {
    const qs = buildQueryString({ channelId: query?.channelId, search: query?.search, limit: query?.limit, offset: query?.offset })
    return this.request(`${API_PATHS.programmes}${qs}`, (value) => asPage<Programme>(value, 'Programmes response'))
  }

  async reconcileEpg(): Promise<EpgReconcileResult> {
    return this.request('/api/v1/epg/reconcile', (value) => asObject<EpgReconcileResult>(value, 'EPG reconcile response'), {
      method: 'POST',
    })
  }

  async getEpgMappings(reviewStatus?: string, limit?: number, offset?: number): Promise<EpgMappingPage> {
    const qs = buildQueryString({ reviewStatus, limit, offset })
    return this.request(`/api/v1/epg/mappings${qs}`, (value) => asPage<EpgMapping>(value, 'EPG mappings response'))
  }

  async getUnmappedChannels(search?: string, limit?: number, offset?: number): Promise<UnmappedChannelPage> {
    const qs = buildQueryString({ search, limit, offset })
    return this.request(`/api/v1/epg/unmapped${qs}`, (value) => asPage<UnmappedChannel>(value, 'Unmapped channels response'))
  }

  async getReviewCandidates(channelId: string): Promise<ReviewCandidate[]> {
    return this.request(`/api/v1/epg/review/${encodeURIComponent(channelId)}/candidates`, (value) => asObjectArray<ReviewCandidate>(value, 'Review candidates response'))
  }

  async searchEpgChannels(query: string, limit?: number): Promise<EpgChannelSearchResult[]> {
    const qs = buildQueryString({ q: query, limit })
    return this.request(`/api/v1/epg/channels/search${qs}`, (value) => asObjectArray<EpgChannelSearchResult>(value, 'EPG channel search response'))
  }

  async setChannelEpgMapping(channelId: string, epgChannelId: string): Promise<void> {
    const csrfToken = this.readCsrfToken()
    if (!csrfToken) {
      throw new IptvApiError({
        type: 'urn:iptv:error:missing-csrf-token',
        title: 'CSRF token unavailable',
        status: 400,
        detail: 'Sign in before you change EPG mappings.',
      })
    }
    const headers = new Headers()
    headers.set('Content-Type', 'application/json')
    headers.set('Accept', 'application/json, application/problem+json')
    headers.set('X-CSRF-Token', csrfToken)
    let response: Response
    try {
      response = await this.fetcher(
        `/api/v1/channels/${encodeURIComponent(channelId)}/epg-mapping`,
        {
          method: 'PATCH',
          headers,
          body: JSON.stringify({ epgChannelId }),
          credentials: 'same-origin',
        },
      )
    } catch {
      throw new IptvApiError({
        type: 'urn:iptv:error:network',
        title: 'Unable to reach the IPTV API',
        status: 0,
        detail: 'The same-origin API request failed before a response was received.',
      })
    }
    if (!response.ok) {
      throw await problemFromResponse(response)
    }
  }

  async removeChannelEpgMapping(channelId: string): Promise<void> {
    await this.deleteResource(`/api/v1/channels/${encodeURIComponent(channelId)}/epg-mapping`, 'EPG mapping')
  }

  async resolveReview(channelId: string, accept: boolean, epgChannelId?: string): Promise<void> {
    const csrfToken = this.readCsrfToken()
    if (!csrfToken) {
      throw new IptvApiError({
        type: 'urn:iptv:error:missing-csrf-token',
        title: 'CSRF token unavailable',
        status: 400,
        detail: 'Sign in before you resolve reviews.',
      })
    }
    const headers = new Headers()
    headers.set('Content-Type', 'application/json')
    headers.set('Accept', 'application/json, application/problem+json')
    headers.set('X-CSRF-Token', csrfToken)
    let response: Response
    try {
      response = await this.fetcher(
        `/api/v1/epg/review/${encodeURIComponent(channelId)}/resolve`,
        {
          method: 'POST',
          headers,
          body: JSON.stringify({ accept, epgChannelId }),
          credentials: 'same-origin',
        },
      )
    } catch {
      throw new IptvApiError({
        type: 'urn:iptv:error:network',
        title: 'Unable to reach the IPTV API',
        status: 0,
        detail: 'The same-origin API request failed before a response was received.',
      })
    }
    if (!response.ok) {
      throw await problemFromResponse(response)
    }
  }

  async getEvents(): Promise<EventChannel[]> {
    return this.request(API_PATHS.events, (value) => asObjectArray<EventChannel>(value, 'Events response'))
  }

  async getEventTemplates(): Promise<EventTemplate[]> {
    return this.request(API_PATHS.eventTemplates, (value) => asObjectArray<EventTemplate>(value, 'Event templates response'))
  }

  async createEventTemplate(input: CreateEventTemplateInput): Promise<EventTemplate> {
    return this.request(API_PATHS.eventTemplates, (value) => asObject<EventTemplate>(value, 'Event template response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async deleteEventTemplate(id: string): Promise<void> {
    await this.deleteResource(`${API_PATHS.eventTemplates}/${encodeURIComponent(id)}`, 'Event template')
  }

  async getEventChannels(templateId?: string): Promise<EventChannel[]> {
    const qs = buildQueryString({ templateId })
    return this.request(`${API_PATHS.eventChannels}${qs}`, (value) => asObjectArray<EventChannel>(value, 'Event channels response'))
  }

  async scanEventTemplate(id: string): Promise<SaveResult> {
    return this.request(`${API_PATHS.eventTemplates}/${encodeURIComponent(id)}/scan`, (value) => asObject<SaveResult>(value, 'Scan response'), {
      method: 'POST',
      body: JSON.stringify({}),
    })
  }

  async suggestEventTemplates(): Promise<EventTemplateSuggestion[]> {
    return this.request(`${API_PATHS.eventTemplates}/suggestions`, (value) => asObjectArray<EventTemplateSuggestion>(value, 'Event template suggestions'))
  }

  async getLineupTemplates(): Promise<LineupTemplate[]> {
    return this.request(API_PATHS.lineupTemplates, (value) => asObjectArray<LineupTemplate>(value, 'Lineup templates response'))
  }

  async createLineupTemplate(input: CreateLineupTemplateInput): Promise<LineupTemplate> {
    return this.request(API_PATHS.lineupTemplates, (value) => asObject<LineupTemplate>(value, 'Lineup template response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async deleteLineupTemplate(id: string): Promise<void> {
    await this.deleteResource(`${API_PATHS.lineupTemplates}/${encodeURIComponent(id)}`, 'Lineup template')
  }

  async applyLineupTemplate(id: string): Promise<SaveResult> {
    return this.request(`${API_PATHS.lineupTemplates}/${encodeURIComponent(id)}/apply`, (value) => asObject<SaveResult>(value, 'Apply lineup response'), {
      method: 'POST',
      body: JSON.stringify({}),
    })
  }

  async getSessions(): Promise<Session[]> {
    return this.request(API_PATHS.sessions, (value) => asObjectArray<Session>(value, 'Sessions response'))
  }

  async saveJellyfin(config: JellyfinConfig): Promise<SaveResult> {
    return this.request(API_PATHS.jellyfin, (value) => asObject<SaveResult>(value, 'Jellyfin response'), {
      method: 'PUT',
      body: JSON.stringify(config),
    })
  }

  async getStreamHealth(status?: string, group?: string, limit?: number, offset?: number): Promise<StreamHealthPage> {
    const params = new URLSearchParams()
    if (status) params.set('status', status)
    if (group) params.set('group', group)
    if (limit !== undefined) params.set('limit', String(limit))
    if (offset !== undefined) params.set('offset', String(offset))
    const qs = params.toString() ? `?${params}` : ''
    return this.request(`${API_PATHS.streamsHealth}${qs}`, (value) => asPage<StreamHealthItem>(value, 'Stream health response'))
  }

  async getStreamHealthStats(): Promise<StreamHealthStats> {
    return this.request(API_PATHS.streamsHealthStats, (value) => asObject<StreamHealthStats>(value, 'Stream health stats response'))
  }

  async triggerHealthCheck(limit?: number): Promise<{ queued: number }> {
    const body = limit !== undefined ? JSON.stringify({ limit }) : '{}'
    return this.request(API_PATHS.streamsHealthCheck, (value) => asObject<{ queued: number }>(value, 'Health check trigger response'), {
      method: 'POST',
      body,
    })
  }

  async rankAllStreams(): Promise<{ ranked: number }> {
    return this.request(API_PATHS.streamsRank, (value) => asObject<{ ranked: number }>(value, 'Stream rank response'), {
      method: 'POST',
    })
  }

  async getBestStream(channelId: string): Promise<BestStream> {
    return this.request(`${API_PATHS.channels}/${encodeURIComponent(channelId)}/best-stream`, (value) => asObject<BestStream>(value, 'Best stream response'))
  }

  async getUsers(): Promise<User[]> {
    return this.request(API_PATHS.users, (value) => asObjectArray<User>(value, 'Users response'))
  }

  async createUser(input: CreateUserInput): Promise<User> {
    return this.request(API_PATHS.users, (value) => asObject<User>(value, 'Create user response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async updateUser(id: string, input: UpdateUserInput): Promise<User> {
    return this.request(`${API_PATHS.users}/${encodeURIComponent(id)}`, (value) => asObject<User>(value, 'Update user response'), {
      method: 'PATCH',
      body: JSON.stringify(input),
    })
  }

  async deleteUser(id: string): Promise<void> {
    await this.deleteResource(`${API_PATHS.users}/${encodeURIComponent(id)}`, 'User')
  }

  async getChannelAliases(country?: string, limit?: number, offset?: number): Promise<ChannelAliasPage> {
    const params = new URLSearchParams()
    if (country) params.set('country', country)
    if (limit !== undefined) params.set('limit', String(limit))
    if (offset !== undefined) params.set('offset', String(offset))
    const qs = params.toString() ? `?${params}` : ''
    return this.request(`${API_PATHS.channelAliases}${qs}`, (value) => asPage<ChannelAlias>(value, 'Channel aliases response'))
  }

  async createChannelAlias(input: CreateChannelAliasInput): Promise<ChannelAlias> {
    return this.request(API_PATHS.channelAliases, (value) => asObject<ChannelAlias>(value, 'Create alias response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async deleteChannelAlias(id: string): Promise<void> {
    await this.deleteResource(`${API_PATHS.channelAliases}/${encodeURIComponent(id)}`, 'Channel alias')
  }

  async resolveChannelAlias(name: string): Promise<ResolveAliasResult> {
    const qs = `?name=${encodeURIComponent(name)}`
    return this.request(`${API_PATHS.channelAliases}/resolve${qs}`, (value) => asObject<ResolveAliasResult>(value, 'Resolve alias response'))
  }

  async getRecordingRules(): Promise<RecordingRule[]> {
    return this.request(API_PATHS.recordingRules, (value) => asObjectArray<RecordingRule>(value, 'Recording rules response'))
  }

  async createRecordingRule(input: CreateRecordingRuleInput): Promise<RecordingRule> {
    return this.request(API_PATHS.recordingRules, (value) => asObject<RecordingRule>(value, 'Create recording rule response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async deleteRecordingRule(id: string): Promise<void> {
    await this.deleteResource(`${API_PATHS.recordingRules}/${encodeURIComponent(id)}`, 'Recording rule')
  }

  async getRecordings(status?: string, limit?: number, offset?: number): Promise<RecordingPage> {
    const params = new URLSearchParams()
    if (status) params.set('status', status)
    if (limit !== undefined) params.set('limit', String(limit))
    if (offset !== undefined) params.set('offset', String(offset))
    const qs = params.toString() ? `?${params}` : ''
    return this.request(`${API_PATHS.recordings}${qs}`, (value) => asPage<Recording>(value, 'Recordings response'))
  }

  async createRecording(input: CreateRecordingInput): Promise<Recording> {
    return this.request(API_PATHS.recordings, (value) => asObject<Recording>(value, 'Create recording response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async deleteRecording(id: string): Promise<void> {
    await this.deleteResource(`${API_PATHS.recordings}/${encodeURIComponent(id)}`, 'Recording')
  }

  async getRecordingStats(): Promise<RecordingStats> {
    return this.request(API_PATHS.recordingStats, (value) => asObject<RecordingStats>(value, 'Recording stats response'))
  }

  async getStreamProfiles(): Promise<StreamProfile[]> {
    return this.request(API_PATHS.streamProfiles, (value) => asObjectArray<StreamProfile>(value, 'Stream profiles response'))
  }

  async createStreamProfile(input: CreateStreamProfileInput): Promise<StreamProfile> {
    return this.request(API_PATHS.streamProfiles, (value) => asObject<StreamProfile>(value, 'Create stream profile response'), {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async deleteStreamProfile(id: string): Promise<void> {
    await this.deleteResource(`${API_PATHS.streamProfiles}/${encodeURIComponent(id)}`, 'Stream profile')
  }

  async assignStreamProfile(channelId: string, profileId: string): Promise<void> {
    return this.request(`${API_PATHS.channels}/${encodeURIComponent(channelId)}/stream-profile`, () => undefined, {
      method: 'POST',
      body: JSON.stringify({ stream_profile_id: profileId }),
    })
  }

  async removeStreamProfile(channelId: string): Promise<void> {
    await this.deleteResource(`${API_PATHS.channels}/${encodeURIComponent(channelId)}/stream-profile`, 'Stream profile assignment')
  }
}

export class MockIptvApiClient implements IptvApiClient {
  private readonly sources: Source[] = mockSources.map((source) => ({ ...source }))
  private readonly syncJobs = new Map<string, SourceSyncStatus>()
  private authenticated = false

  async getAuthStatus(): Promise<AuthStatus> {
    return this.authenticated
      ? { authenticated: true, user: { id: 'user-demo', username: 'demo', displayName: 'Demo operator' } }
      : { authenticated: false }
  }

  async login(input: LoginInput): Promise<AuthStatus> {
    if (!input.username.trim() || !input.password) throw new Error('Username and password are required.')
    this.authenticated = true
    return { authenticated: true, user: { id: 'user-demo', username: input.username.trim(), displayName: input.username.trim() } }
  }

  async logout(): Promise<SaveResult> {
    this.authenticated = false
    return { ok: true, message: 'Signed out.' }
  }

  async getOverview(): Promise<Overview> {
    return { ...mockOverview }
  }

  async getSources(): Promise<Source[]> {
    return this.sources.map((source) => ({ ...source }))
  }

  async createSource(input: SourceInput): Promise<Source> {
    if (!input.name.trim() || !input.endpoint.trim()) {
      throw new Error('Name and endpoint are required.')
    }

    const source: Source = {
      id: `source-${this.sources.length + 1}`,
      name: input.name.trim(),
      kind: input.kind,
      endpoint: input.endpoint.trim(),
      state: 'syncing',
      channels: 0,
      lastSync: '2026-08-19T18:00:00Z',
      refreshIntervalSeconds: 0,
      lastRefreshedAt: null,
      maxConnections: 1,
      timezone: input.timezone?.trim() || 'UTC',
      enabled: true,
    }
    this.sources.push(source)
    return { ...source }
  }

  async deleteSource(id: string): Promise<void> {
    const index = this.sources.findIndex((source) => source.id === id)
    if (index >= 0) this.sources.splice(index, 1)
  }

  async setSourceRefreshInterval(id: string, refreshIntervalSeconds: number): Promise<void> {
    const source = this.sources.find((source) => source.id === id)
    if (source) {
      source.refreshIntervalSeconds = refreshIntervalSeconds
    }
  }

  async triggerSourceSync(id: string): Promise<{ jobId: string; message: string }> {
    const now = new Date().toISOString()
    const jobId = `mock-${id}`
    this.syncJobs.set(id, {
      jobId,
      status: 'running',
      stage: 'downloading',
      percent: 0,
      message: 'Download source data',
      bytesDownloaded: 0,
      recordsProcessed: 0,
      startedAt: now,
      updatedAt: now,
    })
    return { jobId, message: 'Source sync started.' }
  }

  async getSourceSyncStatus(sourceId: string): Promise<SourceSyncStatus> {
    const job = this.syncJobs.get(sourceId)
    if (!job) {
      throw new IptvApiError({
        type: 'urn:iptv:error:not-found',
        title: 'Sync status not found',
        status: 404,
        detail: 'No sync is currently running for this source.',
      })
    }
    const nextPercent = Math.min(100, job.percent + 25)
    const stage: SourceSyncStatus['stage'] = nextPercent === 100 ? 'completed' : nextPercent < 25 ? 'downloading' : nextPercent < 50 ? 'parsing' : nextPercent < 75 ? 'activating' : 'reconciling'
    const status: SourceSyncStatus['status'] = nextPercent === 100 ? 'succeeded' : 'running'
    const message = nextPercent === 100 ? 'Source sync complete' : nextPercent < 25 ? 'Download source data' : nextPercent < 50 ? 'Parse source data' : nextPercent < 75 ? 'Activate source data' : 'Reconcile source data'
    const recordsProcessed = (job.recordsProcessed ?? 0) + (nextPercent === 100 ? 0 : 12_000)
    const bytesDownloaded = (job.bytesDownloaded ?? 0) + (nextPercent === 100 ? 0 : 500_000)
    const updated: SourceSyncStatus = {
      ...job,
      percent: nextPercent,
      stage,
      status,
      message,
      recordsProcessed,
      bytesDownloaded,
      updatedAt: new Date().toISOString(),
    }
    this.syncJobs.set(sourceId, updated)
    return { ...updated }
  }

  async cancelSourceSync(id: string): Promise<{ ok: boolean; message: string }> {
    const job = this.syncJobs.get(id)
    if (!job) {
      throw new IptvApiError({
        type: 'urn:iptv:error:not-found',
        title: 'Sync not found',
        status: 404,
        detail: 'No active sync is running for this source.',
      })
    }
    this.syncJobs.delete(id)
    return { ok: true, message: 'Sync cancelled.' }
  }

  async updateSource(id: string, input: SourceUpdateInput): Promise<void> {
    const source = this.sources.find((source) => source.id === id)
    if (source) {
      if (input.maxConnections !== undefined) source.maxConnections = input.maxConnections
      if (input.timezone !== undefined) source.timezone = input.timezone
      if (input.enabled !== undefined) source.enabled = input.enabled
    }
  }

  async getGroups(): Promise<Group[]> {
    const groups = new Map<string, { channelCount: number; enabledCount: number }>()
    for (const channel of mockChannels) {
      const entry = groups.get(channel.group) ?? { channelCount: 0, enabledCount: 0 }
      entry.channelCount += 1
      if (channel.enabled) entry.enabledCount += 1
      groups.set(channel.group, entry)
    }
    return Array.from(groups.entries())
      .map(([name, { channelCount, enabledCount }]) => ({ name, channelCount, enabledCount }))
      .sort((a, b) => b.channelCount - a.channelCount)
  }

  async getRegionSettings(): Promise<RegionSettingsResponse> {
    return {
      settings: { ...mockRegionSettings },
      prefixes: mockRegionPrefixes.map((prefix) => ({ ...prefix })),
    }
  }

  async updateRegionSettings(input: { timezone: string; enabledPrefixes: string[] }): Promise<RegionSettings> {
    return { ...mockRegionSettings, ...input, autoDetected: false }
  }

  async applyRegionFilter(input: { enabledPrefixes: string[] }): Promise<{ enabled: number; disabled: number }> {
    const enabledSet = new Set(input.enabledPrefixes)
    let enabled = 0
    let disabled = 0
    for (const channel of mockChannels) {
      const prefix = channel.name.split(' ')[0] ?? ''
      if (enabledSet.has(prefix) || enabledSet.has(channel.group)) {
        channel.enabled = true
        enabled += 1
      } else {
        channel.enabled = false
        disabled += 1
      }
    }
    return { enabled, disabled }
  }

  async getChannels(query?: ChannelQuery): Promise<ChannelPage> {
    const limit = query?.limit ?? 100
    const offset = query?.offset ?? 0
    let items = mockChannels.map((channel) => ({ ...channel }))
    if (query?.search) {
      const term = query.search.toLowerCase()
      items = items.filter((channel) => `${channel.name} ${channel.group} ${channel.tvgId}`.toLowerCase().includes(term))
    }
    if (query?.group) {
      items = items.filter((channel) => channel.group === query.group)
    }
    if (query?.enabled !== undefined) {
      items = items.filter((channel) => channel.enabled === query.enabled)
    }
    const total = items.length
    items = items.slice(offset, offset + limit)
    return { total, limit, offset, items }
  }

  async getChannelPreview(channelId: string): Promise<ChannelPreview> {
    return { streamUrl: `/api/v1/channels/${encodeURIComponent(channelId)}/stream`, contentType: 'video/mp2t' }
  }

  async setChannelEnabled(_channelId: string, _enabled: boolean): Promise<SaveResult> {
    return { ok: true, message: 'Channel updated.' }
  }

  async setGroupEnabled(_groupName: string, _enabled: boolean): Promise<SaveResult> {
    return { ok: true, message: 'Group updated.' }
  }

  async setAllGroupsEnabled(_enabled: boolean): Promise<SaveResult> {
    return { ok: true, message: 'All groups updated.' }
  }

  async getProgrammes(query?: ProgrammeQuery): Promise<ProgrammePage> {
    const limit = query?.limit ?? 100
    const offset = query?.offset ?? 0
    let items = mockProgrammes.map((programme) => ({ ...programme }))
    if (query?.channelId) {
      items = items.filter((programme) => programme.id.startsWith(query.channelId!))
    }
    if (query?.search) {
      const term = query.search.toLowerCase()
      items = items.filter((programme) => `${programme.channel} ${programme.title}`.toLowerCase().includes(term))
    }
    const total = items.length
    items = items.slice(offset, offset + limit)
    return { total, limit, offset, items }
  }

  async reconcileEpg(): Promise<EpgReconcileResult> {
    return { mappingsApplied: 0, mappingsRemoved: 0, reviewQueued: 0 }
  }

  async getEpgMappings(reviewStatus?: string, limit?: number, offset?: number): Promise<EpgMappingPage> {
    return { total: 0, limit: limit ?? 100, offset: offset ?? 0, items: [] }
  }

  async getUnmappedChannels(search?: string, limit?: number, offset?: number): Promise<UnmappedChannelPage> {
    return { total: 0, limit: limit ?? 100, offset: offset ?? 0, items: [] }
  }

  async getReviewCandidates(_channelId: string): Promise<ReviewCandidate[]> {
    return []
  }

  async searchEpgChannels(_query: string, _limit?: number): Promise<EpgChannelSearchResult[]> {
    return []
  }

  async setChannelEpgMapping(_channelId: string, _epgChannelId: string): Promise<void> {}

  async removeChannelEpgMapping(_channelId: string): Promise<void> {}

  async resolveReview(_channelId: string, _accept: boolean, _epgChannelId?: string): Promise<void> {}

  async getEvents(): Promise<EventChannel[]> {
    return []
  }

  async getEventTemplates(): Promise<EventTemplate[]> {
    return []
  }

  async createEventTemplate(input: CreateEventTemplateInput): Promise<EventTemplate> {
    return {
      id: `event-template-${Date.now()}`,
      name: input.name,
      displayName: input.displayName,
      matchRegex: input.matchRegex,
      channelNameFormat: input.channelNameFormat,
      groupName: input.groupName,
      eventDurationHours: input.eventDurationHours ?? 3,
      pastDateGraceHours: input.pastDateGraceHours ?? 6,
      futureDateDays: input.futureDateDays ?? 7,
      enabled: true,
    }
  }

  async deleteEventTemplate(_id: string): Promise<void> {}

  async getEventChannels(_templateId?: string): Promise<EventChannel[]> {
    return []
  }

  async scanEventTemplate(_id: string): Promise<SaveResult> {
    return { ok: true, message: 'Scan complete.' }
  }

  async suggestEventTemplates(): Promise<EventTemplateSuggestion[]> {
    return []
  }

  async getLineupTemplates(): Promise<LineupTemplate[]> {
    return []
  }

  async createLineupTemplate(input: CreateLineupTemplateInput): Promise<LineupTemplate> {
    return {
      id: `lineup-template-${Date.now()}`,
      name: input.name,
      packageName: input.packageName,
      country: input.country,
      description: input.description ?? null,
      enabled: true,
    }
  }

  async deleteLineupTemplate(_id: string): Promise<void> {}

  async applyLineupTemplate(_id: string): Promise<SaveResult> {
    return { ok: true, message: 'Lineup applied.' }
  }

  async getSessions(): Promise<Session[]> {
    return mockSessions.map((session) => ({ ...session }))
  }

  async saveJellyfin(config: JellyfinConfig): Promise<SaveResult> {
    try {
      new URL(config.baseUrl)
      new URL(config.publicBaseUrl)
    } catch {
      return { ok: false, message: 'Both Jellyfin and public URLs must be valid absolute URLs.' }
    }
    return { ok: true, message: `Saved tuner “${config.tunerName}” with ${config.guideDays} guide days.` }
  }

  async getStreamHealth(): Promise<StreamHealthPage> {
    return { total: 0, items: [] }
  }

  async getStreamHealthStats(): Promise<StreamHealthStats> {
    return { alive: 0, dead: 0, unknown: 0, checking: 0 }
  }

  async triggerHealthCheck(): Promise<{ queued: number }> {
    return { queued: 0 }
  }

  async rankAllStreams(): Promise<{ ranked: number }> {
    return { ranked: 0 }
  }

  async getBestStream(): Promise<BestStream> {
    throw new Error('Not implemented in mock client')
  }

  async getUsers(): Promise<User[]> {
    return []
  }

  async createUser(): Promise<User> {
    throw new Error('Not implemented in mock client')
  }

  async updateUser(): Promise<User> {
    throw new Error('Not implemented in mock client')
  }

  async deleteUser(): Promise<void> {
    return
  }

  async getChannelAliases(): Promise<ChannelAliasPage> {
    return { total: 0, items: [] }
  }

  async createChannelAlias(): Promise<ChannelAlias> {
    throw new Error('Not implemented in mock client')
  }

  async deleteChannelAlias(): Promise<void> {
    return
  }

  async resolveChannelAlias(name: string): Promise<ResolveAliasResult> {
    return { canonicalName: null, input: name }
  }

  async getRecordingRules(): Promise<RecordingRule[]> {
    return []
  }

  async createRecordingRule(): Promise<RecordingRule> {
    throw new Error('Not implemented in mock client')
  }

  async deleteRecordingRule(): Promise<void> {
    return
  }

  async getRecordings(): Promise<RecordingPage> {
    return { total: 0, items: [] }
  }

  async createRecording(): Promise<Recording> {
    throw new Error('Not implemented in mock client')
  }

  async deleteRecording(): Promise<void> {
    return
  }

  async getRecordingStats(): Promise<RecordingStats> {
    return { scheduled: 0, recording: 0, completed: 0, failed: 0, totalBytes: 0 }
  }

  async getStreamProfiles(): Promise<StreamProfile[]> {
    return []
  }

  async createStreamProfile(): Promise<StreamProfile> {
    throw new Error('Not implemented in mock client')
  }

  async deleteStreamProfile(): Promise<void> {
    return
  }

  async assignStreamProfile(): Promise<void> {
    return
  }

  async removeStreamProfile(): Promise<void> {
    return
  }
}

export function createIptvApiClient({ useMock = import.meta.env.VITE_USE_MOCK_API === 'true', fetcher, onUnauthorized, readCsrfToken }: {
  useMock?: boolean
  fetcher?: Fetcher
  onUnauthorized?: UnauthorizedHandler
  readCsrfToken?: CsrfTokenReader
} = {}): IptvApiClient {
  if (useMock) return new MockIptvApiClient()
  const options = {
    ...(onUnauthorized ? { onUnauthorized } : {}),
    ...(readCsrfToken ? { readCsrfToken } : {}),
  }
  return fetcher ? new FetchIptvApiClient(fetcher, options) : new FetchIptvApiClient(undefined, options)
}

export function mockInitialData<T>(client: IptvApiClient, data: T) {
  return client instanceof MockIptvApiClient ? { initialData: data } : {}
}

export const apiClient: IptvApiClient = createIptvApiClient()
