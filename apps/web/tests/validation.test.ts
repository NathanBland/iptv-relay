import { describe, expect, it } from 'vitest'
import { eventTemplateDefaults, eventTemplateSchema, loginSchema, sourceSchema, sourceUpdateSchema } from '@/lib/validation'

describe('management form schemas', () => {
  it('accepts the login values used by the authentication API', () => {
    expect(loginSchema.safeParse({ username: 'operator', password: 'secret' }).success).toBe(true)
  })

  it('reports a useful error for each missing login value', () => {
    const result = loginSchema.safeParse({ username: ' ', password: '' })
    expect(result.success).toBe(false)
    if (result.success) return
    expect(result.error.issues.map((issue) => issue.message)).toEqual([
      'Enter your username.',
      'Enter your password.',
    ])
  })

  it('accepts standard Xtream credentials', () => {
    expect(sourceSchema.safeParse({
      name: 'Prime IPTV',
      kind: 'Xtream',
      endpoint: '',
      serverUrl: 'https://provider.example:8080',
      username: 'operator',
      password: 'secret',
      timezone: 'UTC',
    }).success).toBe(true)
  })

  it('accepts a complete advanced Xtream endpoint', () => {
    expect(sourceSchema.safeParse({
      name: 'Prime IPTV',
      kind: 'Xtream',
      endpoint: 'https://provider.example/player_api.php?username=operator&password=secret',
      serverUrl: '',
      username: '',
      password: '',
      timezone: 'UTC',
    }).success).toBe(true)
  })

  it('requires all standard Xtream credential fields', () => {
    const result = sourceSchema.safeParse({
      name: 'Prime IPTV',
      kind: 'Xtream',
      endpoint: '',
      serverUrl: 'https://provider.example',
      username: 'operator',
      timezone: 'UTC',
    })
    expect(result.success).toBe(false)
    if (result.success) return
    expect(result.error.issues.some((issue) => issue.path[0] === 'password')).toBe(true)
  })

  it('rejects source values that could not reach a provider', () => {
    const result = sourceSchema.safeParse({ name: 'X', kind: 'M3U', endpoint: 'file:///playlist.m3u', serverUrl: '', username: '', password: '', timezone: 'UTC' })
    expect(result.success).toBe(false)
    if (result.success) return
    expect(result.error.issues.map((issue) => issue.message)).toEqual([
      'Enter at least two characters.',
      'Use an absolute http(s) URL.',
    ])
  })

  it('rejects whitespace in provider URLs', () => {
    expect(sourceSchema.safeParse({ name: 'Prime IPTV', kind: 'M3U', endpoint: 'https://provider.example/list name.m3u', serverUrl: '', username: '', password: '', timezone: 'UTC' }).success).toBe(false)
  })

  it('requires safe provider settings when a source is edited', () => {
    expect(sourceUpdateSchema.safeParse({ maxConnections: 3, timezone: 'America/Denver', enabled: true }).success).toBe(true)
    expect(sourceUpdateSchema.safeParse({ maxConnections: 0, timezone: ' ', enabled: true }).success).toBe(false)
  })

  it('validates event template guide settings and exposes API defaults', () => {
    expect(eventTemplateDefaults).toEqual({
      eventDurationHours: 3,
      pastDateGraceHours: 4,
      futureDateDays: 2,
      timezone: 'UTC',
      fillerTitle: 'No programs available',
    })
    expect(eventTemplateSchema.safeParse({
      name: 'nfl', displayName: 'NFL', matchRegex: 'NFL.*', channelNameFormat: '{event}', groupName: 'Sports',
      eventDurationHours: 3, pastDateGraceHours: 4, futureDateDays: 2, timezone: 'America/Denver', fillerTitle: 'Off air',
    }).success).toBe(true)
    expect(eventTemplateSchema.safeParse({
      name: 'nfl', displayName: 'NFL', matchRegex: 'NFL.*', channelNameFormat: '{event}', groupName: 'Sports',
      eventDurationHours: 0, pastDateGraceHours: 4, futureDateDays: 2, timezone: ' ', fillerTitle: '',
    }).success).toBe(false)
  })
})
