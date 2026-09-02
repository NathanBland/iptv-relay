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

export const eventTemplateSchema = z.object({
  name: nonBlank('Enter a template name.'),
  displayName: nonBlank('Enter a display name.'),
  matchRegex: nonBlank('Enter a match pattern.'),
  channelNameFormat: nonBlank('Enter a channel name format.'),
  groupName: nonBlank('Enter a group name.'),
  eventDurationHours: z.number().int('Use a whole number.').min(1, 'Use at least one hour.'),
  pastDateGraceHours: z.number().int('Use a whole number.').min(0, 'Use zero or more hours.'),
  futureDateDays: z.number().int('Use a whole number.').min(0, 'Use zero or more days.'),
  timezone: nonBlank('Enter an IANA timezone.'),
  fillerTitle: nonBlank('Enter a filler title.'),
})

export const eventTemplateDefaults = {
  eventDurationHours: 3,
  pastDateGraceHours: 4,
  futureDateDays: 2,
  timezone: 'UTC',
  fillerTitle: 'No programs available',
} as const

export type LoginFormValues = z.input<typeof loginSchema>
export type SourceFormValues = z.input<typeof sourceSchema>
export type SourceUpdateFormValues = z.input<typeof sourceUpdateSchema>
export type EventTemplateFormValues = z.input<typeof eventTemplateSchema>
