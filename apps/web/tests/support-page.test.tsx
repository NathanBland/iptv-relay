import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { SupportPage } from '@/pages/support-page'
import { IptvApiError, MockIptvApiClient } from '@/lib/api/client'
import type { SupportLogEntry } from '@/lib/api/types'
import { renderWithQuery } from './test-utils'

describe('support diagnostics page', () => {
  it('shows its loading state while the bundle loads', () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getSupportBundle').mockReturnValue(new Promise(() => {}))

    renderWithQuery(<SupportPage client={client} />)

    expect(screen.getByRole('status')).toHaveTextContent('Wait while the system loads support diagnostics')
  })

  it('downloads a redacted bundle and keeps the page usable when the browser blocks downloads', async () => {
    const originalCreateObjectUrl = Object.getOwnPropertyDescriptor(URL, 'createObjectURL')
    const originalRevokeObjectUrl = Object.getOwnPropertyDescriptor(URL, 'revokeObjectURL')
    const createObjectUrl = vi.fn(() => 'blob:iptv-support')
    const revokeObjectUrl = vi.fn()
    Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: createObjectUrl })
    Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: revokeObjectUrl })
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {})

    try {
      const client = new MockIptvApiClient()
      renderWithQuery(<SupportPage client={client} />)
      expect(await screen.findByRole('heading', { name: 'Support diagnostics' })).toBeInTheDocument()

      await userEvent.click(screen.getByRole('button', { name: 'Download support bundle' }))
      expect(createObjectUrl).toHaveBeenCalledOnce()
      expect(click).toHaveBeenCalledOnce()
      expect(revokeObjectUrl).toHaveBeenCalledWith('blob:iptv-support')

      createObjectUrl.mockImplementation(() => {
        throw new Error('Downloads are disabled')
      })
      await userEvent.click(screen.getByRole('button', { name: 'Download support bundle' }))
      expect(screen.getByRole('heading', { name: 'Redaction status' })).toBeInTheDocument()
    } finally {
      click.mockRestore()
      if (originalCreateObjectUrl) Object.defineProperty(URL, 'createObjectURL', originalCreateObjectUrl)
      else Reflect.deleteProperty(URL, 'createObjectURL')
      if (originalRevokeObjectUrl) Object.defineProperty(URL, 'revokeObjectURL', originalRevokeObjectUrl)
      else Reflect.deleteProperty(URL, 'revokeObjectURL')
    }
  })

  it('renders an empty log state and a redacted error context', async () => {
    const client = new MockIptvApiClient()
    const bundle = await client.getSupportBundle()
    vi.spyOn(client, 'getSupportBundle').mockResolvedValue({ ...bundle, logs: [] })
    const circularContext: Record<string, unknown> = {}
    circularContext.self = circularContext
    const log: SupportLogEntry = {
      timestamp: '2026-08-20T11:59:00Z',
      level: 'error',
      message: 'Provider probe failed.',
      context: circularContext,
    }
    vi.spyOn(client, 'getSupportLogs').mockResolvedValue([log])

    renderWithQuery(<SupportPage client={client} />)

    expect(await screen.findByText('Provider probe failed.')).toBeInTheDocument()
    expect(screen.getByText('{}')).toBeInTheDocument()
    expect(screen.getByText('error')).toBeInTheDocument()
  })

  it('shows an empty log state and a typed log request error', async () => {
    const client = new MockIptvApiClient()
    const bundle = await client.getSupportBundle()
    vi.spyOn(client, 'getSupportBundle').mockResolvedValue({ ...bundle, logs: [] })
    vi.spyOn(client, 'getSupportLogs').mockRejectedValue(new IptvApiError({
      type: 'urn:iptv:error:support-logs',
      title: 'Support logs unavailable',
      status: 503,
      detail: 'Try again shortly.',
    }))

    renderWithQuery(<SupportPage client={client} />)

    expect(await screen.findByRole('alert')).toHaveTextContent('Try again shortly.')
    expect(screen.getByText('No support log entries.')).toBeInTheDocument()
  })
})
