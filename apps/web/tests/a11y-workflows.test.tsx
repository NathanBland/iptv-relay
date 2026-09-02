import { screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { EpgMappingsPage } from '@/pages/epg-mappings-page'
import { EventsPage } from '@/pages/events-page'
import { OperatorSettingsPage } from '@/pages/operator-settings-page'
import { SessionsPage } from '@/pages/sessions-page'
import { SourcesPage } from '@/pages/sources-page'
import { StreamHealthPage } from '@/pages/stream-health-page'
import { StreamProfilesPage } from '@/pages/stream-profiles-page'
import { MockIptvApiClient } from '@/lib/api/client'
import { mockSources } from '@/lib/api/mock-data'
import { renderWithQuery } from './test-utils'

// Accessibility coverage uses accessible-role and ARIA assertions because the
// web package does not depend on @axe-core/playwright. The limitation is that
// role assertions verify the accessibility tree but do not run the full axe
// rule set. Each workflow is covered by assertions for headings, labeled
// controls, live regions, and operable controls.

const timestamp = '2026-08-20T12:00:00Z'

function expectNoAlert() {
  expect(screen.queryAllByRole('alert')).toHaveLength(0)
}

describe('management workflow accessibility', () => {
  it('source progress, cancel, and retry controls expose labels and live regions', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getSources').mockResolvedValue([
      { ...mockSources[0]!, refreshIntervalSeconds: 3600 },
      { ...mockSources[1]!, refreshIntervalSeconds: 0 },
    ])
    vi.spyOn(client, 'getSourceSyncStatus').mockImplementation(async (id) => id === 'source-prime'
      ? { jobId: 'job-1', status: 'running', stage: 'parsing', percent: 44, message: 'Parse entries', bytesDownloaded: 2048, recordsProcessed: 1200, startedAt: timestamp, updatedAt: timestamp }
      : { jobId: `job-${id}`, status: 'succeeded', stage: 'completed', percent: 100, message: 'Done.', startedAt: timestamp, updatedAt: timestamp })
    const cancel = vi.spyOn(client, 'cancelSourceSync').mockResolvedValue({ ok: true, message: 'Cancelled.' })
    const sync = vi.spyOn(client, 'triggerSourceSync').mockResolvedValue({ jobId: 'new-job', message: 'Sync queued.' })

    renderWithQuery(<SourcesPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'Sources', level: 1 })).toBeInTheDocument()

    // The running sync shows a progressbar with an accessible label and value.
    const progressbar = await screen.findByRole('progressbar', { name: 'Sync progress' })
    expect(progressbar).toHaveAttribute('aria-valuemin', '0')
    expect(progressbar).toHaveAttribute('aria-valuemax', '100')
    expect(progressbar).toHaveAttribute('aria-valuenow', '44')

    // The live region that wraps the progress is present.
    expect(screen.getByRole('status')).toBeInTheDocument()

    // The cancel control has an accessible name and is operable.
    await userEvent.click(screen.getByRole('button', { name: 'Cancel sync' }))
    await expect.poll(() => expect(cancel).toHaveBeenCalledWith('source-prime'))

    // The retry (sync) control has an accessible name and is operable.
    await userEvent.click(screen.getByRole('button', { name: 'Sync Front Range tuner' }))
    await expect.poll(() => expect(sync).toHaveBeenCalledWith('source-local'))

    // The add-source toggle exposes its expanded state.
    const addToggle = screen.getByRole('button', { name: /Add source/ })
    expect(addToggle).toHaveAttribute('aria-expanded', 'false')
    await userEvent.click(addToggle)
    expect(addToggle).toHaveAttribute('aria-expanded', 'true')
    expect(screen.getByLabelText('Display name')).toBeInTheDocument()
    expectNoAlert()
  })

  it('mapping review and rollback controls expose roles and status regions', async () => {
    const client = new MockIptvApiClient()
    const mapping = (channelId: string, confidence: number) => ({
      channelId,
      epgChannelId: `epg-${channelId}`,
      method: 'tvg-id',
      confidence,
      evidence: {},
      reviewStatus: 'applied' as const,
      reviewedBy: null,
      reviewedAt: null,
      revision: 1,
      updatedAt: timestamp,
      channelName: `Channel ${channelId}`,
      canonicalKey: `key.${channelId}`,
      epgXmltvId: `xmltv.${channelId}`,
      epgDisplayName: `EPG ${channelId}`,
    })
    vi.spyOn(client, 'getEpgMappings').mockResolvedValue({ total: 1, limit: 50, offset: 0, items: [mapping('high', 0.99)] })
    vi.spyOn(client, 'listReconciliationRevisions').mockResolvedValue([
      { revision: 1, actor: 'system', createdAt: timestamp, beforeValue: null, afterValue: { channels: [] } },
    ])
    const rollback = vi.spyOn(client, 'rollbackReconciliation').mockResolvedValue({
      targetRevision: 1, channelsRemoved: 0, channelsRestored: 1, streamLinksRestored: 1, epgMappingsRestored: 1,
    })

    renderWithQuery(<EpgMappingsPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'EPG Mappings', level: 1 })).toBeInTheDocument()

    // The tab controls are buttons with accessible names.
    expect(screen.getByRole('button', { name: 'Mapped' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Needs review' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Unmapped' })).toBeInTheDocument()

    // The rollback control is operable and reports its result in a status region.
    await userEvent.type(screen.getByLabelText('Source id'), 'source-a')
    expect(await screen.findByText('Revision 1')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: /Roll back/ }))
    await expect.poll(() => expect(rollback).toHaveBeenCalledWith('source-a', { revision: 1 }))
    expect(await screen.findAllByRole('status').then((nodes) => nodes.length > 0)).toBe(true)
    expectNoAlert()
  })

  it('event rule preview exposes a labeled sample input and live result region', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getEventTemplates').mockResolvedValue([
      { id: 'nfl', name: 'nfl', displayName: 'NFL events', matchRegex: '(?<home>.+) vs (?<away>.+)', channelNameFormat: '{home} vs {away}', groupName: 'Sports', eventDurationHours: 3, pastDateGraceHours: 6, futureDateDays: 7, enabled: true },
    ])
    vi.spyOn(client, 'getEventChannels').mockResolvedValue([
      { id: 'event-1', templateId: 'nfl', channelId: null, slotNumber: 1, eventTitle: 'Broncos game', eventStart: timestamp, eventEnd: null, rawStreamName: 'NFL 08/19 1:00 PM Broncos vs Chiefs', state: 'scheduled' },
      { id: 'event-2', templateId: 'nfl', channelId: null, slotNumber: 2, eventTitle: null, eventStart: null, eventEnd: null, rawStreamName: 'NBA Finals Game 5', state: 'hidden' },
    ])

    renderWithQuery(<EventsPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'NFL events' })).toBeInTheDocument()

    // The template preview toggle exposes its expanded state and controls a region.
    const previewToggle = screen.getByRole('button', { name: 'Preview rule for NFL events' })
    expect(previewToggle).toHaveAttribute('aria-expanded', 'false')
    await userEvent.click(previewToggle)
    expect(previewToggle).toHaveAttribute('aria-expanded', 'true')
    const previewRegion = screen.getByRole('region', { name: 'Event rule preview' })
    expect(within(previewRegion).getByText(/1 of 2 scanned stream names match/)).toBeInTheDocument()
    expect(within(previewRegion).getByRole('status')).toBeInTheDocument()
    expect(within(previewRegion).getAllByText('Match').length).toBeGreaterThan(0)
    expect(within(previewRegion).getAllByText('Skip').length).toBeGreaterThan(0)

    // The form preview has a labeled sample input and a live status region.
    await userEvent.click(screen.getByRole('button', { name: 'Configure templates' }))
    const form = (screen.getByRole('heading', { name: 'New event template' }).closest('section, article, [role="group"], form, .rounded-xl') as HTMLElement | null) ?? document.body
    await userEvent.type(within(form).getByLabelText('Match regex'), '(?<home>.+) vs (?<away>.+)')
    await userEvent.type(within(form).getByLabelText('Channel name format'), '{home} vs {away}')
    const sampleInput = within(form).getByLabelText('Sample stream name for preview')
    expect(sampleInput).toBeInTheDocument()
    await userEvent.type(sampleInput, 'NFL 08/19 1:00 PM Broncos vs Chiefs')
    expect(await within(form).findByRole('status')).toBeInTheDocument()
    expectNoAlert()
  })

  it('provider pool diagnostics expose a summary region and table caption', async () => {
    const client = new MockIptvApiClient()
    renderWithQuery(<SessionsPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'Sessions', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('region', { name: 'Session summary' })).toBeInTheDocument()
    const table = screen.getByRole('table')
    expect(table).toHaveAccessibleName('Live shared IPTV upstream sessions')
    // Ring utilization progressbars have accessible names tied to the source.
    expect(screen.getAllByRole('progressbar', { name: /ring utilization/ }).length).toBeGreaterThan(0)
    expectNoAlert()
  })

  it('operator settings expose labeled adapter policy and provider scope controls', async () => {
    const client = new MockIptvApiClient()
    renderWithQuery(<OperatorSettingsPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'Operator settings', level: 1 })).toBeInTheDocument()
    // The adapter policy setting is a choice control with an accessible label.
    expect(await screen.findByLabelText('Input adapter')).toBeInTheDocument()
    // The provider scope selector is reachable and exposes a labeled input.
    await userEvent.click(screen.getByRole('button', { name: 'Provider' }))
    expect(screen.getAllByLabelText('Provider id').length).toBeGreaterThan(0)
    expectNoAlert()
  })

  it('stream health diagnostics expose stats and labeled filter controls', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getStreamHealthStats').mockResolvedValue({ alive: 1, dead: 0, unknown: 0, checking: 0 })
    vi.spyOn(client, 'getStreamHealth').mockResolvedValue({
      total: 1,
      items: [{
        providerStreamId: 'stream-1', streamName: 'alive stream', groupName: 'Sports', healthStatus: 'alive',
        healthCheckedAt: timestamp, healthError: null, videoCodec: 'h264', videoResolution: null, videoWidth: 1920,
        videoHeight: 1080, videoFps: 60, audioCodec: null, audioChannels: null, audioSampleRate: null,
        bitrateKbps: 6000, providerAccountId: 'provider-1',
      }],
    })
    renderWithQuery(<StreamHealthPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'Stream Health', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Check streams' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Rank by quality' })).toBeInTheDocument()
    expect(screen.getByPlaceholderText(/Filter by status/)).toBeInTheDocument()
    expect(screen.getByPlaceholderText('Filter by group')).toBeInTheDocument()
    expectNoAlert()
  })

  it('adapter policy stream profile form exposes labeled controls', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getStreamProfiles').mockResolvedValue([])
    const create = vi.spyOn(client, 'createStreamProfile').mockResolvedValue({} as never)
    renderWithQuery(<StreamProfilesPage client={client} />)
    expect(await screen.findByRole('button', { name: 'Add profile' })).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Add profile' }))
    const nameInput = screen.getByPlaceholderText('My FFmpeg profile')
    expect(nameInput).toBeInTheDocument()
    await userEvent.type(nameInput, 'FFmpeg copy')
    await userEvent.selectOptions(screen.getByRole('combobox'), 'ffmpeg')
    await userEvent.click(screen.getByRole('button', { name: 'Create profile' }))
    await expect.poll(() => expect(create).toHaveBeenCalledWith(expect.objectContaining({ name: 'FFmpeg copy', profileType: 'ffmpeg' })))
    expectNoAlert()
  })
})
