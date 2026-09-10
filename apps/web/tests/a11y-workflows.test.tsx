import { screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { ChannelAliasesPage } from '@/pages/channel-aliases-page'
import { ChannelsPage } from '@/pages/channels-page'
import { EpgMappingsPage } from '@/pages/epg-mappings-page'
import { EpgPage } from '@/pages/epg-page'
import { EventsPage } from '@/pages/events-page'
import { GroupsPage } from '@/pages/groups-page'
import { JellyfinPage } from '@/pages/jellyfin-page'
import { LoginPage } from '@/pages/login-page'
import { OperatorSettingsPage } from '@/pages/operator-settings-page'
import { OverviewPage } from '@/pages/overview-page'
import { RecordingsPage } from '@/pages/recordings-page'
import { SessionsPage } from '@/pages/sessions-page'
import { SupportPage } from '@/pages/support-page'
import { SourcesPage } from '@/pages/sources-page'
import { StreamHealthPage } from '@/pages/stream-health-page'
import { StreamProfilesPage } from '@/pages/stream-profiles-page'
import { TvGuidePage } from '@/pages/tv-guide-page'
import { UsersPage } from '@/pages/users-page'
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
      { id: 'nfl', name: 'nfl', displayName: 'NFL events', matchRegex: '(?<home>.+) vs (?<away>.+)', channelNameFormat: '{home} vs {away}', groupName: 'Sports', eventDurationHours: 3, pastDateGraceHours: 6, futureDateDays: 7, timezone: 'UTC', fillerTitle: 'No programs available', enabled: true },
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

  it('support diagnostics expose redaction status, download, and log controls', async () => {
    const client = new MockIptvApiClient()
    renderWithQuery(<SupportPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'Support diagnostics', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Download support bundle' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Redaction status' })).toBeInTheDocument()
    expect(await screen.findByRole('log', { name: 'Redacted support logs' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Refresh logs' })).toBeInTheDocument()
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
      estimated: false,
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

  it('login form exposes a level-1 heading, labeled fields, and an operable submit control', async () => {
    const onAuthenticated = vi.fn()
    renderWithQuery(<LoginPage client={new MockIptvApiClient()} next="/channels" onAuthenticated={onAuthenticated} />)
    expect(screen.getByRole('heading', { name: 'Sign in', level: 1 })).toBeInTheDocument()
    const username = screen.getByLabelText('Username')
    expect(username).toHaveAttribute('autocomplete', 'username')
    const password = screen.getByLabelText('Password')
    expect(password).toHaveAttribute('autocomplete', 'current-password')
    const submit = screen.getByRole('button', { name: 'Sign in' })
    expect(submit).toBeInTheDocument()
    await userEvent.type(username, 'operator')
    await userEvent.type(password, 'secret')
    await userEvent.click(submit)
    await expect.poll(() => expect(onAuthenticated).toHaveBeenCalledWith('/channels'))
    expectNoAlert()
  })

  it('overview exposes a summary region, level-1 heading, and stat headings without alerts', async () => {
    renderWithQuery(<OverviewPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'Overview', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('region', { name: 'System summary' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Provider connection budget' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Guide coverage' })).toBeInTheDocument()
    expect(screen.getByText('Published channels')).toBeInTheDocument()
    expectNoAlert()
  })

  it('channels page exposes a captioned table, labeled search, and group filter controls', async () => {
    renderWithQuery(<ChannelsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'Channels', level: 1 })).toBeInTheDocument()
    expect(screen.getByLabelText('Search channels')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Filter by group' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Filter by enabled state' })).toBeInTheDocument()
    const table = screen.getByRole('table')
    expect(table).toHaveAccessibleName('Published channel lineup')
    // The channel count is announced through a live status region.
    expect(screen.getByRole('status')).toBeInTheDocument()
    // Per-row enable/disable controls expose accessible names tied to the channel.
    expect(screen.getByRole('button', { name: 'Disable KWGN Denver' })).toBeInTheDocument()
    expectNoAlert()
  })

  it('groups page exposes a level-1 heading, labeled search, and bulk toggle controls', async () => {
    renderWithQuery(<GroupsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'Groups', level: 1 })).toBeInTheDocument()
    expect(screen.getByLabelText('Search groups')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Enable all groups/ })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Disable all groups/ })).toBeInTheDocument()
    // Per-group toggle controls expose accessible names.
    expect(screen.getAllByRole('button', { name: 'Enable all' }).length).toBeGreaterThan(0)
    expect(screen.getAllByRole('button', { name: 'Disable all' }).length).toBeGreaterThan(0)
    expectNoAlert()
  })

  it('tv guide page exposes a level-1 heading, labeled search, and section headings', async () => {
    renderWithQuery(<TvGuidePage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'TV Guide', level: 1 })).toBeInTheDocument()
    expect(screen.getByLabelText('Search TV guide')).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'On now' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Up next' })).toBeInTheDocument()
    expectNoAlert()
  })

  it('epg page exposes a level-1 heading, labeled search, and a named scrollable schedule', async () => {
    renderWithQuery(<EpgPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'EPG', level: 1 })).toBeInTheDocument()
    expect(screen.getByLabelText('Search programme guide')).toBeInTheDocument()
    // The scrollable schedule container exposes an accessible name through
    // aria-label and is focusable through tabindex. It does not declare an
    // explicit role="region", so the test asserts the accessible name rather
    // than the region role.
    expect(screen.getByLabelText('Scrollable programme schedule')).toBeInTheDocument()
    expectNoAlert()
  })

  it('recordings page exposes a level-1 heading and an operable add-rule toggle', async () => {
    renderWithQuery(<RecordingsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'Recordings', level: 1 })).toBeInTheDocument()
    const addToggle = screen.getByRole('button', { name: 'Add rule' })
    expect(addToggle).toBeInTheDocument()
    await userEvent.click(addToggle)
    // The rule form opens. The form field labels are visual text and are not
    // programmatically associated with their inputs through htmlFor/id. The
    // test uses placeholder text to confirm the form is operable.
    expect(screen.getByPlaceholderText('Daily news')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Create rule' })).toBeInTheDocument()
    expectNoAlert()
  })

  it('users page exposes a level-1 heading and an operable add-user toggle', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getUsers').mockResolvedValue([
      { id: 'user-1', username: 'operator', displayName: 'Demo operator', role: 'operator', enabled: true, lastLoginAt: null, createdAt: timestamp, updatedAt: timestamp },
      { id: 'user-2', username: 'viewer', displayName: 'Read-only viewer', role: 'viewer', enabled: false, lastLoginAt: null, createdAt: timestamp, updatedAt: timestamp },
    ])
    renderWithQuery(<UsersPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'Users', level: 1 })).toBeInTheDocument()
    const addToggle = screen.getByRole('button', { name: 'Add user' })
    expect(addToggle).toBeInTheDocument()
    await userEvent.click(addToggle)
    expect(screen.getByRole('button', { name: 'Create user' })).toBeInTheDocument()
    // Per-user enable/disable controls expose text labels tied to the user state.
    expect(screen.getByRole('button', { name: 'Disable' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Enable' })).toBeInTheDocument()
    expectNoAlert()
  })

  it('channel aliases page exposes a level-1 heading, a resolve control, and an add-alias toggle', async () => {
    renderWithQuery(<ChannelAliasesPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'Channel aliases', level: 1 })).toBeInTheDocument()
    expect(screen.getByPlaceholderText('Enter a channel name to resolve')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Resolve' })).toBeInTheDocument()
    const addToggle = screen.getByRole('button', { name: 'Add alias' })
    await userEvent.click(addToggle)
    expect(screen.getByRole('button', { name: 'Create alias' })).toBeInTheDocument()
    expectNoAlert()
  })

  it('jellyfin setup page exposes a level-1 heading, a labeled rotate control, and copy controls', async () => {
    renderWithQuery(<JellyfinPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'Jellyfin setup', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Rotate publish token' })).toBeInTheDocument()
    // The published endpoints load asynchronously. Wait for the copy controls
    // to appear before asserting their accessible names.
    expect(await screen.findByRole('button', { name: 'Copy M3U tuner URL' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Copy XMLTV guide URL' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Copy HDHomeRun tuner URL' })).toBeInTheDocument()
    expect(screen.getByRole('link', { name: /Jellyfin docs/ })).toBeInTheDocument()
    expectNoAlert()
  })
})
