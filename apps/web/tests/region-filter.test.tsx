import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { RegionFilter } from '@/components/region-filter'
import { IptvApiError, MockIptvApiClient } from '@/lib/api/client'
import type { RegionSettingsResponse } from '@/lib/api/types'
import { renderWithQuery } from './test-utils'

const fullResponse: RegionSettingsResponse = {
  settings: {
    timezone: 'America/New_York',
    enabledPrefixes: ['US', 'EN'],
    suggestedPrefixes: ['US', 'EN'],
    autoDetected: true,
  },
  prefixes: [
    { prefix: 'US', groupCount: 8, channelCount: 120, suggested: true },
    { prefix: 'EN', groupCount: 3, channelCount: 42, suggested: true },
    { prefix: 'UK', groupCount: 2, channelCount: 34, suggested: false },
  ],
}

describe('region filter management surface', () => {
  it('shows the loading state while region settings load', () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getRegionSettings').mockReturnValue(new Promise(() => {}))
    renderWithQuery(<RegionFilter client={client} />)
    expect(screen.getAllByText('Loading region filter...').length).toBeGreaterThan(0)
  })

  it('surfaces a load error to assistive technology', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getRegionSettings').mockRejectedValue(new Error('Region service offline.'))
    renderWithQuery(<RegionFilter client={client} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('Region service offline.')
  })

  it('falls back to a generic message for non-Error load failures', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getRegionSettings').mockRejectedValue('boom')
    renderWithQuery(<RegionFilter client={client} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('Failed to load region settings.')
  })

  it('renders the auto-detected badge and toggles prefixes by click and keyboard', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getRegionSettings').mockResolvedValue(fullResponse)
    renderWithQuery(<RegionFilter client={client} />)
    expect(await screen.findByText('Auto-detected')).toBeInTheDocument()
    const usBadge = screen.getByText('US').closest('[role="button"]') as HTMLElement
    expect(usBadge).toHaveAttribute('aria-pressed', 'true')
    await userEvent.click(usBadge)
    expect(usBadge).toHaveAttribute('aria-pressed', 'false')
    await userEvent.click(usBadge)
    expect(usBadge).toHaveAttribute('aria-pressed', 'true')

    const ukBadge = screen.getByText('UK').closest('[role="button"]') as HTMLElement
    expect(ukBadge).toHaveAttribute('aria-pressed', 'false')
    ukBadge.focus()
    await userEvent.keyboard('{Enter}')
    expect(ukBadge).toHaveAttribute('aria-pressed', 'true')
    await userEvent.keyboard(' ')
    expect(ukBadge).toHaveAttribute('aria-pressed', 'false')
  })

  it('selects the suggested prefixes and applies the filter', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getRegionSettings').mockResolvedValue(fullResponse)
    const apply = vi.spyOn(client, 'applyRegionFilter').mockResolvedValue({ enabled: 11, disabled: 5 })
    renderWithQuery(<RegionFilter client={client} />)
    await screen.findByText('Auto-detected')
    await userEvent.click(screen.getByRole('button', { name: 'Select suggested' }))
    expect(screen.getByText('US').closest('[role="button"]')).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByText('UK').closest('[role="button"]')).toHaveAttribute('aria-pressed', 'false')
    await userEvent.click(screen.getByRole('button', { name: 'Apply region filter' }))
    expect(await screen.findByRole('status')).toHaveTextContent('11 groups enabled, 5 groups disabled.')
    await waitFor(() => expect(apply).toHaveBeenCalledWith({ enabledPrefixes: ['US', 'EN'] }))
  })

  it('reports an apply error and a generic fallback for non-Error rejections', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getRegionSettings').mockResolvedValue(fullResponse)
    vi.spyOn(client, 'applyRegionFilter').mockRejectedValueOnce(
      new IptvApiError({
        type: 'urn:iptv:error:region-filter',
        title: 'Apply failed',
        status: 503,
        detail: 'Region service busy.',
      }),
    )
    vi.spyOn(client, 'applyRegionFilter').mockRejectedValueOnce('unexpected')
    renderWithQuery(<RegionFilter client={client} />)
    await screen.findByText('Auto-detected')
    await userEvent.click(screen.getByRole('button', { name: 'Apply region filter' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Region service busy.')
    await userEvent.click(screen.getByRole('button', { name: 'Apply region filter' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Apply failed.')
  })

  it('collapses and expands the card body', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getRegionSettings').mockResolvedValue(fullResponse)
    renderWithQuery(<RegionFilter client={client} />)
    const collapse = await screen.findByRole('button', { name: 'Collapse region filter' })
    await userEvent.click(collapse)
    expect(screen.getByRole('button', { name: 'Expand region filter' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Apply region filter' })).not.toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Expand region filter' }))
    expect(await screen.findByRole('button', { name: 'Apply region filter' })).toBeInTheDocument()
  })

  it('labels a manually configured timezone instead of auto-detected', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getRegionSettings').mockResolvedValue({
      ...fullResponse,
      settings: { ...fullResponse.settings, autoDetected: false },
    })
    renderWithQuery(<RegionFilter client={client} />)
    expect(await screen.findByText('Manual')).toBeInTheDocument()
    expect(screen.queryByText('Auto-detected')).not.toBeInTheDocument()
  })
})
