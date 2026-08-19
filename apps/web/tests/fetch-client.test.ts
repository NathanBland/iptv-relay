import { describe, expect, it, vi } from 'vitest'
import {
  FetchIptvApiClient,
  IptvApiError,
  MockIptvApiClient,
  apiClient,
  createIptvApiClient,
  mockInitialData,
  readCsrfCookie,
  safeNextPath,
} from '@/lib/api/client'
import {
  mockChannels,
  mockEvents,
  mockOverview,
  mockProgrammes,
  mockSessions,
  mockSources,
} from '@/lib/api/mock-data'

function jsonResponse(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json' } })
}

describe('FetchIptvApiClient', () => {
  it('uses the exact same-origin routes and command methods', async () => {
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input)
      if (path === '/api/v1/auth/status') return jsonResponse({ authenticated: false })
      if (path === '/api/v1/auth/login') return jsonResponse({ authenticated: true, user: { id: 'operator-1', username: 'operator', displayName: 'Relay operator' } })
      if (path === '/api/v1/auth/logout') return jsonResponse({ ok: true, message: 'Signed out.' })
      if (path === '/api/v1/system') return jsonResponse(mockOverview)
      if (path.startsWith('/api/v1/channels')) return jsonResponse({ total: mockChannels.length, limit: 100, offset: 0, items: mockChannels })
      if (path.startsWith('/api/v1/programmes')) return jsonResponse({ total: mockProgrammes.length, limit: 100, offset: 0, items: mockProgrammes })
      if (path === '/api/v1/events') return jsonResponse(mockEvents)
      if (path === '/api/v1/sessions') return jsonResponse(mockSessions)
      if (path === '/api/v1/sources' && init?.method === 'POST') return jsonResponse(mockSources[0], 201)
      if (path === '/api/v1/sources') return jsonResponse(mockSources)
      if (path === '/api/v1/jellyfin' && init?.method === 'PUT') return jsonResponse({ ok: true, message: 'Saved.' })
      return jsonResponse({ title: 'Not found' }, 404)
    })
    const client = new FetchIptvApiClient(fetcher, { readCsrfToken: () => 'csrf-test-token' })

    expect(await client.getAuthStatus()).toEqual({ authenticated: false })
    expect(await client.login({ username: 'operator', password: 'correct horse battery staple' })).toMatchObject({ authenticated: true })
    expect(await client.logout()).toEqual({ ok: true, message: 'Signed out.' })

    const [overview, sources, channels, programmes, events, sessions, created, jellyfin] = await Promise.all([
      client.getOverview(),
      client.getSources(),
      client.getChannels(),
      client.getProgrammes(),
      client.getEvents(),
      client.getSessions(),
      client.createSource({ name: 'Prime IPTV', kind: 'Xtream', endpoint: 'https://provider.invalid/player_api.php' }),
      client.saveJellyfin({ baseUrl: 'http://jellyfin:8096', tunerName: 'Relay', publicBaseUrl: 'http://relay:3000', guideDays: 7 }),
    ])

    expect(overview.channels).toBe(486)
    expect(sources).toHaveLength(3)
    expect(channels.items[0]?.tvgId).toBe('KWGN-DT.us')
    expect(programmes.items[0]?.confidence).toBe(99)
    expect(events[0]?.state).toBe('scheduled')
    expect(sessions).toHaveLength(3)
    expect(sessions[2]).toMatchObject({ state: 'recovering', viewerCount: 2, lastFailure: 'http' })
    expect(created?.id).toBe('source-prime')
    expect(jellyfin).toEqual({ ok: true, message: 'Saved.' })

    const sourceCommand = fetcher.mock.calls.find(([path, init]) => path === '/api/v1/sources' && init?.method === 'POST')
    expect(sourceCommand?.[1]).toEqual(expect.objectContaining({ credentials: 'same-origin', body: expect.stringContaining('Prime IPTV') }))
    expect(new Headers(sourceCommand?.[1]?.headers).get('content-type')).toBe('application/json')
    expect(new Headers(sourceCommand?.[1]?.headers).get('accept')).toContain('application/problem+json')
    expect(new Headers(sourceCommand?.[1]?.headers).get('x-csrf-token')).toBe('csrf-test-token')
    const mutations = fetcher.mock.calls.filter(([, init]) => ['POST', 'PUT'].includes(init?.method ?? ''))
    expect(mutations.every(([, init]) => new Headers(init?.headers).get('x-csrf-token') === 'csrf-test-token')).toBe(true)
    expect(fetcher.mock.calls.every(([path]) => String(path).startsWith('/api/v1/'))).toBe(true)
  })

  it('reads a bounded CSRF cookie and rejects unsafe return paths', () => {
    expect(readCsrfCookie('theme=dark; iptv_csrf=token%20value; session=secret')).toBe('token value')
    expect(readCsrfCookie('iptv_csrf=%E0%A4%A')).toBeUndefined()
    expect(readCsrfCookie(`iptv_csrf=${'x'.repeat(1_025)}`)).toBeUndefined()
    expect(readCsrfCookie('iptv_csrf=line%0Abreak')).toBeUndefined()
    expect(readCsrfCookie('unrelated=value')).toBeUndefined()
    expect(safeNextPath('/sources?filter=local#main')).toBe('/sources?filter=local#main')
    expect(safeNextPath('/login?next=/sources')).toBe('/')
    expect(safeNextPath('//attacker.example')).toBe('/')
    expect(safeNextPath('/safe\\redirect')).toBe('/')
    expect(safeNextPath('/safe\nredirect')).toBe('/')
    expect(safeNextPath('https://attacker.example')).toBe('/')
    expect(safeNextPath(undefined)).toBe('/')
  })

  it('requires CSRF on mutations and redirects only once for protected 401 responses', async () => {
    const noCsrfClient = new FetchIptvApiClient(vi.fn())
    await expect(noCsrfClient.login({ username: 'operator', password: 'secret' })).rejects.toMatchObject({
      problem: { status: 403, type: 'urn:iptv:error:missing-csrf-token' },
    })

    const unauthorized = () => jsonResponse({ type: 'urn:iptv:error:unauthorized', title: 'Sign-in required' }, 401)
    const onUnauthorized = vi.fn()
    const client = new FetchIptvApiClient(async () => unauthorized(), { onUnauthorized })
    await expect(client.getOverview()).rejects.toMatchObject({ problem: { status: 401 } })
    await expect(client.getSources()).rejects.toMatchObject({ problem: { status: 401 } })
    expect(onUnauthorized).toHaveBeenCalledOnce()
    expect(onUnauthorized).toHaveBeenCalledWith('/api/v1/system')

    const authHandler = vi.fn()
    const authClient = new FetchIptvApiClient(async () => unauthorized(), { onUnauthorized: authHandler })
    await expect(authClient.getAuthStatus()).rejects.toMatchObject({ problem: { status: 401 } })
    expect(authHandler).not.toHaveBeenCalled()
  })

  it('normalizes and bounds RFC 9457 problem details', async () => {
    const detail = 'x'.repeat(700)
    const client = new FetchIptvApiClient(async () => new Response(JSON.stringify({
      type: 'urn:iptv:error:conflict',
      title: 'Source conflict',
      status: 499,
      detail,
      instance: '/api/v1/sources',
    }), { status: 409, headers: { 'content-type': 'application/problem+json' } }))

    const error = await client.getSources().catch((caught: unknown) => caught)
    expect(error).toBeInstanceOf(IptvApiError)
    expect((error as IptvApiError).problem).toMatchObject({ status: 409, title: 'Source conflict', type: 'urn:iptv:error:conflict' })
    expect((error as IptvApiError).problem.detail).toHaveLength(500)
  })

  it('never includes an HTML error body in the surfaced message', async () => {
    const client = new FetchIptvApiClient(async () => new Response('<h1>internal secret</h1>', {
      status: 500,
      statusText: 'Server failure',
      headers: { 'content-type': 'text/html' },
    }))
    const error = await client.getOverview().catch((caught: unknown) => caught) as IptvApiError
    expect(error.message).toBe('Server failure')
    expect(error.message).not.toContain('internal secret')
    expect(error.problem.type).toBe('about:blank')
  })

  it('turns malformed problems, network failures, and invalid success payloads into safe typed errors', async () => {
    const malformedProblem = new FetchIptvApiClient(async () => new Response('{', { status: 400, headers: { 'content-type': 'application/problem+json' } }))
    await expect(malformedProblem.getOverview()).rejects.toMatchObject({ problem: { status: 400, title: 'Request failed' } })

    const networkFailure = new FetchIptvApiClient(async () => { throw new Error('secret network detail') })
    await expect(networkFailure.getSources()).rejects.toMatchObject({ problem: { status: 0, type: 'urn:iptv:error:network' } })

    const invalidJson = new FetchIptvApiClient(async () => new Response('not-json', { status: 200, headers: { 'content-type': 'application/json' } }))
    await expect(invalidJson.getOverview()).rejects.toMatchObject({ problem: { status: 502, title: 'Invalid API response' } })

    const invalidCollection = new FetchIptvApiClient(async () => jsonResponse('not-an-object'))
    await expect(invalidCollection.getChannels()).rejects.toThrow('JSON object')
  })

  it('defaults to fetch and enables fixtures only when explicitly selected', () => {
    expect(apiClient).toBeInstanceOf(FetchIptvApiClient)
    expect(createIptvApiClient({ useMock: true })).toBeInstanceOf(MockIptvApiClient)
    expect(createIptvApiClient({ useMock: false, fetcher: vi.fn() })).toBeInstanceOf(FetchIptvApiClient)
    expect(createIptvApiClient({ useMock: false, onUnauthorized: vi.fn(), readCsrfToken: () => 'token' })).toBeInstanceOf(FetchIptvApiClient)
    expect(mockInitialData(new MockIptvApiClient(), mockSources)).toEqual({ initialData: mockSources })
    expect(mockInitialData(new FetchIptvApiClient(vi.fn()), mockSources)).toEqual({})
  })
})
