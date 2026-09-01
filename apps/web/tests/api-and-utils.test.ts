import { describe, expect, it } from 'vitest'
import { MockIptvApiClient, apiClient } from '@/lib/api/client'
import { apiQueries } from '@/lib/api/queries'
import { partitionProgrammesAt } from '@/lib/programmes'
import { cn, formatBitrate, formatRelativeTime } from '@/lib/utils'

describe('typed API boundary', () => {
  it('returns isolated snapshots for every dashboard resource', async () => {
    const client = new MockIptvApiClient()
    const [overview, sources, channels, programmes, events, sessions] = await Promise.all([
      client.getOverview(), client.getSources(), client.getChannels(), client.getProgrammes(), client.getEvents(), client.getSessions(),
    ])
    expect(overview.providerConnections).toBe(3)
    expect(sources).toHaveLength(3)
    expect(channels.items.length).toBeGreaterThan(10)
    expect(programmes.items.length).toBeGreaterThan(10)
    expect(events).toEqual([])
    expect(sessions).toHaveLength(3)
    expect(sessions.reduce((total, session) => total + session.viewerCount, 0)).toBe(6)

    sources[0]!.name = 'mutated'
    expect((await client.getSources())[0]?.name).toBe('Prime IPTV')
  })

  it('validates and creates sources', async () => {
    const client = new MockIptvApiClient()
    await expect(client.createSource({ name: ' ', kind: 'M3U', endpoint: '' })).rejects.toThrow('required')
    const source = await client.createSource({ name: '  Backup  ', kind: 'M3U', endpoint: ' https://backup.invalid/list.m3u ', timezone: 'America/Denver' })
    expect(source).toMatchObject({ name: 'Backup', state: 'syncing', endpoint: 'https://backup.invalid/list.m3u', timezone: 'America/Denver' })
    expect(await client.getSources()).toHaveLength(4)
  })

  it('models the complete sign-in and sign-out lifecycle', async () => {
    const client = new MockIptvApiClient()
    expect(await client.getAuthStatus()).toEqual({ authenticated: false })
    await expect(client.login({ username: ' ', password: '' })).rejects.toThrow('required')
    expect(await client.login({ username: '  operator  ', password: 'secret' })).toEqual({
      authenticated: true,
      user: { id: 'user-demo', username: 'operator', displayName: 'operator' },
    })
    expect(await client.getAuthStatus()).toMatchObject({ authenticated: true })
    expect(await client.logout()).toEqual({ ok: true, message: 'Signed out.' })
    expect(await client.getAuthStatus()).toEqual({ authenticated: false })
  })

  it('returns published Jellyfin setup URLs from the mock boundary', async () => {
    const client = new MockIptvApiClient()
    const setup = await client.getJellyfinSetup()
    expect(setup.status).toBe('available')
    expect(setup.playlistUrl).toContain('/out/')
    expect(setup.playlistUrl).toContain('/playlist.m3u')
    expect(setup.xmltvUrl).toContain('/out/')
    expect(setup.xmltvUrl).toContain('/xmltv.xml')
    expect(setup.hdhrDeviceUrl).toContain('/out/')
    expect(setup.hdhrDeviceUrl).toContain('/hdhr/device.xml')
    expect(setup.guideDaysMax).toBe(30)
  })

  it('rotates the Jellyfin publish token and returns new URLs once', async () => {
    const client = new MockIptvApiClient()
    const rotated = await client.rotateJellyfinToken({ overlapSeconds: 120 })
    expect(rotated.status).toBe('available')
    expect(rotated.playlistUrl).toContain('/out/rotated-token/playlist.m3u')
    expect(rotated.xmltvUrl).toContain('/out/rotated-token/xmltv.xml')
    expect(rotated.hdhrDeviceUrl).toContain('/out/rotated-token/hdhr/device.xml')
  })

  it('builds query definitions for default and injected clients', () => {
    expect(apiQueries().overview.queryKey).toEqual(['overview'])
    expect(apiQueries().authStatus.queryKey).toEqual(['auth', 'status'])
    expect(apiQueries().authStatus.retry).toBe(false)
    expect(apiQueries(apiClient).sessions.queryKey).toEqual(['sessions'])
  })
})

describe('display utilities', () => {
  it('selects current and upcoming programme instants at a fixed clock', () => {
    const winterNow = Date.parse('2026-01-01T19:00:00Z')
    const summerNow = Date.parse('2026-07-01T18:00:00Z')
    const programmes = [
      { id: 'utc', channel: 'UTC', title: 'UTC current', start: '2026-01-01T18:30:00Z', end: '2026-01-01T19:30:00Z', source: 'XMLTV', confidence: 100 },
      { id: 'winter', channel: 'Denver', title: 'Winter local noon', start: '2026-01-01T19:00:00Z', end: '2026-01-01T20:00:00Z', source: 'XMLTV', confidence: 100 },
      { id: 'summer', channel: 'Denver', title: 'Summer local noon', start: '2026-07-01T18:00:00Z', end: '2026-07-01T19:00:00Z', source: 'XMLTV', confidence: 100 },
      { id: 'next', channel: 'Denver', title: 'Next', start: '2026-07-01T19:00:00Z', end: '2026-07-01T20:00:00Z', source: 'XMLTV', confidence: 100 },
    ]
    expect(partitionProgrammesAt(programmes, winterNow).current.map((item) => item.id)).toEqual(['utc', 'winter'])
    const summer = partitionProgrammesAt(programmes, summerNow)
    expect(summer.current.map((item) => item.id)).toEqual(['summer'])
    expect(summer.upcoming.map((item) => item.id)).toEqual(['next'])
  })

  it('merges classes and formats bitrate', () => {
    expect(cn('px-2', false, 'px-4')).toBe('px-4')
    expect(formatBitrate(800)).toBe('800 Kbps')
    expect(formatBitrate(6_500)).toBe('6.5 Mbps')
  })

  it('formats relative times across each display band', () => {
    const now = new Date('2026-08-19T18:00:00Z')
    expect(formatRelativeTime('2026-08-19T18:00:00Z', now)).toBe('just now')
    expect(formatRelativeTime('2026-08-19T17:42:00Z', now)).toBe('18m ago')
    expect(formatRelativeTime('2026-08-19T15:00:00Z', now)).toBe('3h ago')
    expect(formatRelativeTime('2026-08-17T18:00:00Z', now)).toBe('2d ago')
  })
})
