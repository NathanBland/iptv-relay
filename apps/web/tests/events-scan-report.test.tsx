import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { EventsPage } from '@/pages/events-page'
import { MockIptvApiClient } from '@/lib/api/client'
import type { EventScanResult, EventTemplate } from '@/lib/api/types'
import { renderWithQuery } from './test-utils'

const template: EventTemplate = {
  id: 'tpl-1', name: 'nhl', displayName: 'NHL events', matchRegex: '^NHL', channelNameFormat: '{slot}', groupName: 'Hockey',
  eventDurationHours: 3, pastDateGraceHours: 4, futureDateDays: 2, timezone: 'UTC', fillerTitle: 'Off air', enabled: true,
}

function setup(report: EventScanResult) {
  const client = new MockIptvApiClient()
  vi.spyOn(client, 'getEventTemplates').mockResolvedValue([template])
  vi.spyOn(client, 'getEventChannels').mockResolvedValue([
    {
      id: 'e1', templateId: 'tpl-1', channelId: 'c1', slotNumber: 1, eventTitle: 'Senators at Sabres',
      eventStart: '2026-11-05T00:30:00Z', eventEnd: '2026-11-05T02:30:00Z', rawStreamName: 'NHL 01 Senators @ Sabres',
      outputName: 'NHL 01', state: 'live',
    },
  ])
  const scan = vi.spyOn(client, 'scanEventTemplate').mockResolvedValue(report)
  renderWithQuery(<EventsPage client={client} />)
  return scan
}

describe('events scan report', () => {
  it('shows the counts after a scan', async () => {
    const scan = setup({ ok: true, message: 'Scanned 9 channels: 2 matched, 1 scheduled, 1 skipped', channelsScanned: 9, matched: 2, scheduled: 1, skipped: 1, pruned: 0, skips: [
      { templateId: 'tpl-1', channelId: 'c2', streamName: 'NHL 02 Rangers maybe later', reason: 'The title has no start time.' },
    ] })
    await screen.findByText('NHL events')
    expect(screen.queryByRole('region', { name: 'Scan report' })).not.toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Scan' }))
    await waitFor(() => expect(scan).toHaveBeenCalledWith('tpl-1'))
    const panel = await screen.findByRole('region', { name: 'Scan report' })
    expect(panel).toHaveTextContent('Scanned 9 channels: 2 matched, 1 scheduled, 1 skipped')
    expect(panel).toHaveTextContent('The title has no start time.')
    expect(panel).toHaveTextContent('NHL 02 Rangers maybe later')
  })

  it('shows the skip reasons when a template has zero matches', async () => {
    setup({ ok: true, message: 'Scanned 4 channels: 0 matched, 0 scheduled, 0 skipped', channelsScanned: 4, matched: 0, scheduled: 0, skipped: 0, pruned: 0, skips: [
      { templateId: 'tpl-1', channelId: null, streamName: '', reason: 'matchRegex is not a valid Rust regex: unclosed group' },
      { templateId: 'other', channelId: null, streamName: '', reason: 'Reason for another template' },
    ] })
    await screen.findByText('NHL events')
    await userEvent.click(screen.getByRole('button', { name: 'Scan' }))
    const panel = await screen.findByRole('region', { name: 'Scan report' })
    expect(panel).toHaveTextContent('Skip reasons')
    expect(panel).toHaveTextContent('matchRegex is not a valid Rust regex')
    expect(panel).not.toHaveTextContent('another template')
  })

  it('shows the output name and the state of each event channel', async () => {
    setup({ ok: true, message: 'x', channelsScanned: 0, matched: 0, scheduled: 0, skipped: 0, pruned: 0, skips: [] })
    expect(await screen.findByText('Channel: NHL 01')).toBeInTheDocument()
    expect(screen.getByText('live')).toBeInTheDocument()
    expect(screen.getByText(/Starts .*2026|Starts .*Nov/)).toBeInTheDocument()
  })
})
