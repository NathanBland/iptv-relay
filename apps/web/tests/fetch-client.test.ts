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
import type { IptvApiClient } from '@/lib/api/types'

function jsonResponse(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json' } })
}

describe('FetchIptvApiClient', () => {
  it('covers every management endpoint with encoded identifiers and optional filters', async () => {
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input)
      if ((init?.method ?? 'GET') === 'GET' && (
        path === '/api/v1/groups'
        || path === '/api/v1/events'
        || path === '/api/v1/event-templates'
        || path.startsWith('/api/v1/event-channels')
        || path === '/api/v1/lineup-templates'
        || path === '/api/v1/sessions'
        || path === '/api/v1/users'
        || path === '/api/v1/recordings/rules'
        || path === '/api/v1/stream-profiles'
        || path.includes('/candidates')
        || path.startsWith('/api/v1/epg/channels/search')
      )) return jsonResponse([])
      if (
        path.startsWith('/api/v1/channels?')
        || path.startsWith('/api/v1/programmes?')
        || path.startsWith('/api/v1/epg/mappings')
        || path.startsWith('/api/v1/epg/unmapped')
        || path.startsWith('/api/v1/streams/health?')
        || path.startsWith('/api/v1/channel-aliases?')
        || path.startsWith('/api/v1/recordings?')
      ) return jsonResponse({ total: 0, limit: 25, offset: 5, items: [] })
      return jsonResponse({ ok: true, message: 'Done.', queued: 2, ranked: 3 })
    })
    const client = new FetchIptvApiClient(fetcher, { readCsrfToken: () => 'csrf-token' })

    await Promise.all([
      client.deleteSource('source / one'),
      client.setSourceRefreshInterval('source / one', 900),
      client.triggerSourceSync('source / one'),
      client.cancelSourceSync('source / one'),
      client.getSourceSyncStatus('source / one'),
      client.updateSource('source / one', { timezone: 'UTC' }),
      client.getGroups(),
      client.getChannels({ search: 'local news', group: 'US / News', enabled: false, limit: 25, offset: 5 }),
      client.getChannelPreview('channel / one'),
      client.setChannelEnabled('channel / one', false),
      client.setGroupEnabled('US / News', true),
      client.setAllGroupsEnabled(false),
      client.getProgrammes({ channelId: 'channel / one', search: 'game', limit: 25, offset: 5 }),
      client.reconcileEpg(),
      client.getEpgMappings('review', 25, 5),
      client.getUnmappedChannels('local news', 25, 5),
      client.getReviewCandidates('channel / one'),
      client.searchEpgChannels('KUSA / Denver', 20),
      client.setChannelEpgMapping('channel / one', 'epg / one'),
      client.removeChannelEpgMapping('channel / one'),
      client.resolveReview('channel / one', true, 'epg / one'),
      client.getEvents(),
      client.getEventTemplates(),
      client.createEventTemplate({ name: 'sports', displayName: 'Sports', matchRegex: '(.+)', channelNameFormat: '$1', groupName: 'Sports' }),
      client.deleteEventTemplate('template / one'),
      client.getEventChannels('template / one'),
      client.scanEventTemplate('template / one'),
      client.getLineupTemplates(),
      client.createLineupTemplate({ name: 'local', packageName: 'Denver', country: 'US' }),
      client.deleteLineupTemplate('lineup / one'),
      client.applyLineupTemplate('lineup / one'),
      client.getStreamHealth('alive', 'US / News', 25, 5),
      client.getStreamHealthStats(),
      client.triggerHealthCheck(40),
      client.triggerHealthCheck(),
      client.rankAllStreams(),
      client.getBestStream('channel / one'),
      client.getUsers(),
      client.createUser({ username: 'operator', displayName: 'Operator', password: 'password', role: 'operator' }),
      client.updateUser('user / one', { enabled: false }),
      client.deleteUser('user / one'),
      client.getChannelAliases('US / CA', 25, 5),
      client.createChannelAlias({ canonicalName: 'ESPN', alias: 'ESPN HD', country: 'US', category: 'sports' }),
      client.deleteChannelAlias('alias / one'),
      client.resolveChannelAlias('ESPN / HD'),
      client.getRecordingRules(),
      client.createRecordingRule({ name: 'News', channelId: 'channel / one', ruleType: 'recurring' }),
      client.deleteRecordingRule('rule / one'),
      client.getRecordings('completed', 25, 5),
      client.createRecording({ channelId: 'channel / one', title: 'News', startsAt: '2026-01-01T00:00:00Z', endsAt: '2026-01-01T01:00:00Z' }),
      client.deleteRecording('recording / one'),
      client.getRecordingStats(),
      client.getStreamProfiles(),
      client.createStreamProfile({ name: 'VLC', profileType: 'vlc', bufferSeconds: 8 }),
      client.deleteStreamProfile('profile / one'),
      client.assignStreamProfile('channel / one', 'profile / one'),
      client.removeStreamProfile('channel / one'),
    ])

    const paths = fetcher.mock.calls.map(([path]) => String(path))
    expect(paths).toContain('/api/v1/channels?search=local+news&group=US+%2F+News&enabled=false&limit=25&offset=5')
    expect(paths).toContain('/api/v1/channel-aliases/resolve?name=ESPN%20%2F%20HD')
    expect(paths).toContain('/api/v1/channels/channel%20%2F%20one/stream-profile')
  })

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
      if (path === '/api/v1/jellyfin/setup') return jsonResponse({ status: 'available', playlistUrl: 'http://relay:3000/out/token/playlist.m3u', xmltvUrl: 'http://relay:3000/out/token/xmltv.xml', hdhrDeviceUrl: 'http://relay:3000/out/token/hdhr/device.xml', guideDaysMax: 30 })
      if (path === '/api/v1/jellyfin/setup/rotate' && init?.method === 'POST') return jsonResponse({ status: 'available', playlistUrl: 'http://relay:3000/out/new-token/playlist.m3u', xmltvUrl: 'http://relay:3000/out/new-token/xmltv.xml', hdhrDeviceUrl: 'http://relay:3000/out/new-token/hdhr/device.xml', guideDaysMax: 30 })
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
      client.getJellyfinSetup(),
    ])

    expect(overview.channels).toBe(486)
    expect(sources).toHaveLength(3)
    expect(channels.items[0]?.tvgId).toBe('KWGN-DT.us')
    expect(programmes.items[0]?.confidence).toBe(99)
    expect(events).toEqual([])
    expect(sessions).toHaveLength(3)
    expect(sessions[2]).toMatchObject({ state: 'recovering', viewerCount: 2, lastFailure: 'http' })
    expect(created?.id).toBe('source-prime')
    expect(jellyfin).toEqual({ status: 'available', playlistUrl: 'http://relay:3000/out/token/playlist.m3u', xmltvUrl: 'http://relay:3000/out/token/xmltv.xml', hdhrDeviceUrl: 'http://relay:3000/out/token/hdhr/device.xml', guideDaysMax: 30 })

    const rotated = await client.rotateJellyfinToken({ overlapSeconds: 60 })
    expect(rotated.status).toBe('available')
    expect(rotated.playlistUrl).toBe('http://relay:3000/out/new-token/playlist.m3u')
    const rotationCommand = fetcher.mock.calls.find(([path, init]) => path === '/api/v1/jellyfin/setup/rotate' && init?.method === 'POST')
    expect(rotationCommand?.[1]).toEqual(expect.objectContaining({ credentials: 'same-origin', body: expect.stringContaining('overlapSeconds') }))
    expect(new Headers(rotationCommand?.[1]?.headers).get('x-csrf-token')).toBe('csrf-test-token')

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
    expect(safeNextPath(null)).toBe('/')
    expect(safeNextPath(42)).toBe('/')
    expect(safeNextPath('')).toBe('/')
    expect(safeNextPath('/')).toBe('/')
    expect(safeNextPath('/channels/')).toBe('/channels/')
    expect(safeNextPath('/login/')).toBe('/')
    expect(readCsrfCookie('iptv_csrf=')).toBeUndefined()
    expect(readCsrfCookie('iptv_csrf=%20%20')).toBe('  ')
  })

  it('rejects every special command when CSRF is missing', async () => {
    const client = new FetchIptvApiClient(vi.fn())
    await expect(client.setSourceRefreshInterval('id', 60)).rejects.toMatchObject({ problem: { status: 400 } })
    await expect(client.triggerSourceSync('id')).rejects.toMatchObject({ problem: { status: 400 } })
    await expect(client.cancelSourceSync('id')).rejects.toMatchObject({ problem: { status: 400 } })
    await expect(client.updateSource('id', {})).rejects.toMatchObject({ problem: { status: 400 } })
    await expect(client.setChannelEpgMapping('id', 'epg')).rejects.toMatchObject({ problem: { status: 400 } })
    await expect(client.resolveReview('id', false)).rejects.toMatchObject({ problem: { status: 400 } })
    await expect(client.deleteUser('id')).rejects.toMatchObject({ problem: { status: 403 } })
  })

  it('normalizes transport and HTTP failures for special commands', async () => {
    const network = new FetchIptvApiClient(async () => { throw new Error('network') }, { readCsrfToken: () => 'token' })
    await expect(network.setSourceRefreshInterval('id', 60)).rejects.toMatchObject({ problem: { status: 0 } })
    await expect(network.triggerSourceSync('id')).rejects.toMatchObject({ problem: { status: 0 } })
    await expect(network.cancelSourceSync('id')).rejects.toMatchObject({ problem: { status: 0 } })
    await expect(network.updateSource('id', {})).rejects.toMatchObject({ problem: { status: 0 } })
    await expect(network.setChannelEpgMapping('id', 'epg')).rejects.toMatchObject({ problem: { status: 0 } })
    await expect(network.resolveReview('id', false)).rejects.toMatchObject({ problem: { status: 0 } })
    await expect(network.deleteUser('id')).rejects.toMatchObject({ problem: { status: 0 } })

    const refused = new FetchIptvApiClient(async () => jsonResponse({ title: 'Refused' }, 503), { readCsrfToken: () => 'token' })
    await expect(refused.setSourceRefreshInterval('id', 60)).rejects.toMatchObject({ problem: { status: 503 } })
    await expect(refused.triggerSourceSync('id')).rejects.toMatchObject({ problem: { status: 503 } })
    await expect(refused.cancelSourceSync('id')).rejects.toMatchObject({ problem: { status: 503 } })
    await expect(refused.updateSource('id', {})).rejects.toMatchObject({ problem: { status: 503 } })
    await expect(refused.setChannelEpgMapping('id', 'epg')).rejects.toMatchObject({ problem: { status: 503 } })
    await expect(refused.resolveReview('id', false)).rejects.toMatchObject({ problem: { status: 503 } })
    await expect(refused.deleteUser('id')).rejects.toMatchObject({ problem: { status: 503 } })
  })

  it('uses page defaults and rejects malformed collection members', async () => {
    const defaults = new FetchIptvApiClient(async () => jsonResponse({ items: [] }))
    await expect(defaults.getChannels()).resolves.toEqual({ total: 0, limit: 0, offset: 0, items: [] })
    await expect(defaults.getProgrammes()).resolves.toEqual({ total: 0, limit: 0, offset: 0, items: [] })

    const nullPage = new FetchIptvApiClient(async () => jsonResponse(null))
    await expect(nullPage.getChannels()).rejects.toThrow('JSON object')
    const member = new FetchIptvApiClient(async () => jsonResponse([{}, null]))
    await expect(member.getSources()).rejects.toThrow('array of objects')
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

  it('covers the complete mock management surface and expected unsupported creates', async () => {
    const client = new MockIptvApiClient()
    const managementClient: IptvApiClient = client
    await expect(client.deleteSource('missing')).resolves.toBeUndefined()
    await expect(client.setSourceRefreshInterval('source-prime', 60)).resolves.toBeUndefined()
    await expect(client.triggerSourceSync('source-prime')).resolves.toMatchObject({ jobId: expect.any(String) })
    await expect(client.getSourceSyncStatus('source-prime')).resolves.toMatchObject({ jobId: 'mock-source-prime' })
    await expect(client.cancelSourceSync('source-prime')).resolves.toMatchObject({ ok: true })
    await expect(client.updateSource('source-prime', { enabled: false })).resolves.toBeUndefined()
    await expect(client.getGroups()).resolves.toEqual(expect.any(Array))
    await expect(client.getChannelPreview('id / one')).resolves.toMatchObject({ contentType: 'video/mp2t' })
    await expect(client.setChannelEnabled('id', true)).resolves.toMatchObject({ ok: true })
    await expect(client.setGroupEnabled('group', true)).resolves.toMatchObject({ ok: true })
    await expect(client.setAllGroupsEnabled(false)).resolves.toMatchObject({ ok: true })
    await expect(client.reconcileEpg()).resolves.toMatchObject({ reviewQueued: 0 })
    await expect(client.getEpgMappings('review', 1, 0)).resolves.toMatchObject({ total: 0 })
    await expect(client.getUnmappedChannels('x', 1, 0)).resolves.toMatchObject({ total: 0 })
    await expect(client.getReviewCandidates('id')).resolves.toEqual([])
    await expect(client.searchEpgChannels('x', 1)).resolves.toEqual([])
    await expect(client.setChannelEpgMapping('id', 'epg')).resolves.toBeUndefined()
    await expect(client.removeChannelEpgMapping('id')).resolves.toBeUndefined()
    await expect(client.resolveReview('id', false)).resolves.toBeUndefined()
    await expect(client.createEventTemplate({ name: 'n', displayName: 'N', matchRegex: 'x', channelNameFormat: 'x', groupName: 'g' })).resolves.toMatchObject({ eventDurationHours: 3 })
    await expect(client.deleteEventTemplate('id')).resolves.toBeUndefined()
    await expect(client.getEventChannels('id')).resolves.toEqual([])
    await expect(client.scanEventTemplate('id')).resolves.toMatchObject({ ok: true })
    await expect(client.createLineupTemplate({ name: 'n', packageName: 'p', country: 'US' })).resolves.toMatchObject({ description: null })
    await expect(client.deleteLineupTemplate('id')).resolves.toBeUndefined()
    await expect(client.applyLineupTemplate('id')).resolves.toMatchObject({ ok: true })
    await expect(client.getStreamHealth()).resolves.toMatchObject({ total: 0 })
    await expect(client.getStreamHealthStats()).resolves.toMatchObject({ alive: 0 })
    await expect(client.triggerHealthCheck()).resolves.toMatchObject({ queued: 0 })
    await expect(client.rankAllStreams()).resolves.toMatchObject({ ranked: 0 })
    await expect(managementClient.getBestStream('id')).rejects.toThrow('Not implemented')
    await expect(client.getUsers()).resolves.toEqual([])
    await expect(managementClient.createUser({ username: 'a', displayName: 'A', password: 'b' })).rejects.toThrow('Not implemented')
    await expect(managementClient.updateUser('id', {})).rejects.toThrow('Not implemented')
    await expect(managementClient.deleteUser('id')).resolves.toBeUndefined()
    await expect(client.getChannelAliases()).resolves.toMatchObject({ total: 0 })
    await expect(managementClient.createChannelAlias({ canonicalName: 'A', alias: 'B' })).rejects.toThrow('Not implemented')
    await expect(managementClient.deleteChannelAlias('id')).resolves.toBeUndefined()
    await expect(client.resolveChannelAlias('name')).resolves.toMatchObject({ canonicalName: null })
    await expect(client.getRecordingRules()).resolves.toEqual([])
    await expect(managementClient.createRecordingRule({ name: 'n', channelId: 'id' })).rejects.toThrow('Not implemented')
    await expect(managementClient.deleteRecordingRule('id')).resolves.toBeUndefined()
    await expect(client.getRecordings()).resolves.toMatchObject({ total: 0 })
    await expect(managementClient.createRecording({ channelId: 'id', title: 't', startsAt: 'a', endsAt: 'b' })).rejects.toThrow('Not implemented')
    await expect(managementClient.deleteRecording('id')).resolves.toBeUndefined()
    await expect(client.getRecordingStats()).resolves.toMatchObject({ totalBytes: 0 })
    await expect(client.getStreamProfiles()).resolves.toEqual([])
    await expect(managementClient.createStreamProfile({ name: 'n' })).rejects.toThrow('Not implemented')
    await expect(managementClient.deleteStreamProfile('id')).resolves.toBeUndefined()
    await expect(managementClient.assignStreamProfile('channel', 'profile')).resolves.toBeUndefined()
    await expect(managementClient.removeStreamProfile('channel')).resolves.toBeUndefined()
  })

  it('advances mock sync stages and applies optional mock filters', async () => {
    const client = new MockIptvApiClient()
    await expect(client.getSourceSyncStatus('missing')).rejects.toMatchObject({ problem: { status: 404 } })
    await expect(client.cancelSourceSync('missing')).rejects.toMatchObject({ problem: { status: 404 } })
    await client.triggerSourceSync('source-prime')
    const statuses = []
    for (let index = 0; index < 4; index++) statuses.push(await client.getSourceSyncStatus('source-prime'))
    expect(statuses.map((status) => status.stage)).toEqual(['parsing', 'activating', 'reconciling', 'completed'])
    expect(statuses.at(-1)).toMatchObject({ status: 'succeeded', percent: 100 })
    await client.updateSource('source-prime', { maxConnections: 8, timezone: 'America/Denver', enabled: false })
    await client.updateSource('missing', { enabled: true })
    await client.setSourceRefreshInterval('missing', 10)
    await client.deleteSource('missing')
    await expect(client.getChannels({ search: 'espn', group: 'Sports', enabled: true, limit: 1, offset: 1 })).resolves.toMatchObject({ limit: 1, offset: 1 })
    await expect(client.getProgrammes({ channelId: 'program-', search: 'news', limit: 2, offset: 1 })).resolves.toMatchObject({ limit: 2, offset: 1 })
    await expect(client.getEpgMappings(undefined, undefined, undefined)).resolves.toMatchObject({ limit: 100, offset: 0 })
    await expect(client.getUnmappedChannels(undefined, undefined, undefined)).resolves.toMatchObject({ limit: 100, offset: 0 })
  })
})
