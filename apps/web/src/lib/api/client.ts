import {
  mockChannels,
  mockEvents,
  mockOverview,
  mockProgrammes,
  mockSessions,
  mockSources,
} from './mock-data'
import type {
  Channel,
  ChannelPage,
  ChannelQuery,
  AuthStatus,
  DynamicEvent,
  IptvApiClient,
  JellyfinConfig,
  Overview,
  LoginInput,
  Page,
  Programme,
  ProgrammePage,
  ProgrammeQuery,
  SaveResult,
  Session,
  Source,
  SourceInput,
  ProblemDetails,
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
  sessions: '/api/v1/sessions',
  jellyfin: '/api/v1/jellyfin',
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

  async getChannels(query?: ChannelQuery): Promise<ChannelPage> {
    const qs = buildQueryString({ search: query?.search, group: query?.group, limit: query?.limit, offset: query?.offset })
    return this.request(`${API_PATHS.channels}${qs}`, (value) => asPage<Channel>(value, 'Channels response'))
  }

  async getProgrammes(query?: ProgrammeQuery): Promise<ProgrammePage> {
    const qs = buildQueryString({ channelId: query?.channelId, limit: query?.limit, offset: query?.offset })
    return this.request(`${API_PATHS.programmes}${qs}`, (value) => asPage<Programme>(value, 'Programmes response'))
  }

  async getEvents(): Promise<DynamicEvent[]> {
    return this.request(API_PATHS.events, (value) => asObjectArray<DynamicEvent>(value, 'Events response'))
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
}

export class MockIptvApiClient implements IptvApiClient {
  private readonly sources: Source[] = mockSources.map((source) => ({ ...source }))
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
    }
    this.sources.push(source)
    return { ...source }
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
    const total = items.length
    items = items.slice(offset, offset + limit)
    return { total, limit, offset, items }
  }

  async getProgrammes(query?: ProgrammeQuery): Promise<ProgrammePage> {
    const limit = query?.limit ?? 100
    const offset = query?.offset ?? 0
    let items = mockProgrammes.map((programme) => ({ ...programme }))
    if (query?.channelId) {
      items = items.filter((programme) => programme.id.startsWith(query.channelId!))
    }
    const total = items.length
    items = items.slice(offset, offset + limit)
    return { total, limit, offset, items }
  }

  async getEvents(): Promise<DynamicEvent[]> {
    return mockEvents.map((event) => ({ ...event }))
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
