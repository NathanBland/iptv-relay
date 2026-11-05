import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { MockIptvApiClient } from '@/lib/api/client'
import { mockChannels } from '@/lib/api/mock-data'
import { ChannelsPage } from '@/pages/channels-page'
import { renderWithQuery } from './test-utils'

describe('channel rename control', () => {
  it('renames a channel and shows the provider name on a second line', async () => {
    const client = new MockIptvApiClient()
    const first = mockChannels[0]!
    const getChannels = vi.spyOn(client, 'getChannels').mockResolvedValue({
      total: 1,
      limit: 50,
      offset: 0,
      items: [{ ...first, providerName: 'US: KWGN HD', outputName: 'KWGN Denver', nameOverride: null }],
    })
    const rename = vi.spyOn(client, 'setChannelName').mockResolvedValue({ ...first })
    renderWithQuery(<ChannelsPage client={client} />)
    expect(await screen.findByText('KWGN Denver')).toBeInTheDocument()
    expect(screen.getByText('Provider: US: KWGN HD')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /Reset name/ })).not.toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Rename KWGN Denver' }))
    const input = screen.getByLabelText('Output name for US: KWGN HD')
    await userEvent.clear(input)
    await userEvent.type(input, 'Local 2')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(rename).toHaveBeenCalledWith(first.id, 'Local 2'))
    expect(getChannels).toHaveBeenCalled()
  })

  it('resets an override with the Reset action', async () => {
    const client = new MockIptvApiClient()
    const first = mockChannels[0]!
    vi.spyOn(client, 'getChannels').mockResolvedValue({
      total: 1,
      limit: 50,
      offset: 0,
      items: [{ ...first, providerName: 'KWGN Denver', outputName: 'Local 2', nameOverride: 'Local 2' }],
    })
    const rename = vi.spyOn(client, 'setChannelName').mockResolvedValue({ ...first })
    renderWithQuery(<ChannelsPage client={client} />)
    await userEvent.click(await screen.findByRole('button', { name: 'Reset name of Local 2' }))
    await waitFor(() => expect(rename).toHaveBeenCalledWith(first.id, null))
  })
})
