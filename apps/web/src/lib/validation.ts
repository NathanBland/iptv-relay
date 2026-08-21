import { z } from 'zod'

function isAbsoluteHttpUrl(value: string) {
  if (/\s/.test(value)) return false
  try {
    const url = new URL(value)
    return (url.protocol === 'http:' || url.protocol === 'https:') && url.hostname.length > 0
  } catch {
    return false
  }
}

const nonBlank = (message: string) => z.string().refine((value) => value.trim().length > 0, message)

export const loginSchema = z.object({
  username: nonBlank('Enter your username.'),
  password: nonBlank('Enter your password.'),
})

export const sourceSchema = z.object({
  name: z.string().refine((value) => value.trim().length >= 2, 'Enter at least two characters.'),
  kind: z.enum(['M3U', 'Xtream', 'XMLTV', 'Network tuner']),
  endpoint: z.string().refine(isAbsoluteHttpUrl, 'Use an absolute http(s) URL.'),
  timezone: nonBlank('Enter an IANA timezone.'),
})

export const sourceUpdateSchema = z.object({
  maxConnections: z.number().int('Use a whole number.').min(1, 'Use at least one connection.'),
  timezone: nonBlank('Enter a timezone.'),
  enabled: z.boolean(),
})

export const jellyfinSchema = z.object({
  baseUrl: z.string().refine(isAbsoluteHttpUrl, 'Enter an absolute Jellyfin URL.'),
  tunerName: nonBlank('A tuner name is required.'),
  publicBaseUrl: z.string().refine(isAbsoluteHttpUrl, 'Enter an absolute URL reachable by Jellyfin.'),
  guideDays: z.number().int('Use a whole number.').min(1, 'Choose at least one guide day.').max(31, 'Choose no more than 31 guide days.'),
})

export type LoginFormValues = z.input<typeof loginSchema>
export type SourceFormValues = z.input<typeof sourceSchema>
export type SourceUpdateFormValues = z.input<typeof sourceUpdateSchema>
export type JellyfinFormValues = z.input<typeof jellyfinSchema>
