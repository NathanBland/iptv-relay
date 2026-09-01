import { describe, expect, it } from 'vitest'
import { loginSchema, sourceSchema, sourceUpdateSchema } from '@/lib/validation'

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

  it('accepts supported source kinds and absolute HTTP URLs', () => {
    expect(sourceSchema.safeParse({ name: 'Prime IPTV', kind: 'Xtream', endpoint: 'https://provider.example/live', timezone: 'UTC' }).success).toBe(true)
  })

  it('rejects source values that could not reach a provider', () => {
    const result = sourceSchema.safeParse({ name: 'X', kind: 'M3U', endpoint: 'file:///playlist.m3u', timezone: 'UTC' })
    expect(result.success).toBe(false)
    if (result.success) return
    expect(result.error.issues.map((issue) => issue.message)).toEqual([
      'Enter at least two characters.',
      'Use an absolute http(s) URL.',
    ])
  })

  it('rejects whitespace in provider URLs', () => {
    expect(sourceSchema.safeParse({ name: 'Prime IPTV', kind: 'M3U', endpoint: 'https://provider.example/list name.m3u', timezone: 'UTC' }).success).toBe(false)
  })

  it('requires safe provider settings when a source is edited', () => {
    expect(sourceUpdateSchema.safeParse({ maxConnections: 3, timezone: 'America/Denver', enabled: true }).success).toBe(true)
    expect(sourceUpdateSchema.safeParse({ maxConnections: 0, timezone: ' ', enabled: true }).success).toBe(false)
  })
})
