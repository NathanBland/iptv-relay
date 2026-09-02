import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { ChannelAliasesPage } from '@/pages/channel-aliases-page'
import { ChannelsPage } from '@/pages/channels-page'
import { EpgMappingsPage } from '@/pages/epg-mappings-page'
import { EventsPage } from '@/pages/events-page'
import { GroupsPage } from '@/pages/groups-page'
import { OverviewPage } from '@/pages/overview-page'
import { OperatorSettingsPage } from '@/pages/operator-settings-page'
import { RecordingsPage } from '@/pages/recordings-page'
import { SourcesPage } from '@/pages/sources-page'
import { StreamHealthPage } from '@/pages/stream-health-page'
import { StreamProfilesPage } from '@/pages/stream-profiles-page'
import { TvGuidePage } from '@/pages/tv-guide-page'
import { UsersPage } from '@/pages/users-page'
import { IptvApiError, MockIptvApiClient } from '@/lib/api/client'
import { mockChannels, mockSources } from '@/lib/api/mock-data'
import { renderWithQuery } from './test-utils'

const timestamp = '2026-08-20T12:00:00Z'

describe('expanded management pages', () => {
  it('renders dynamic event states and starts a template scan', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getEventTemplates').mockResolvedValue([
      { id: 'enabled', name: 'enabled', displayName: 'NFL events', matchRegex: '(?<home>.+) vs (?<away>.+)', channelNameFormat: '{home} vs {away}', groupName: 'Sports', eventDurationHours: 3, pastDateGraceHours: 6, futureDateDays: 7, enabled: true },
      { id: 'disabled', name: 'disabled', displayName: 'Empty events', matchRegex: 'x', channelNameFormat: 'x', groupName: 'Events', eventDurationHours: 2, pastDateGraceHours: 1, futureDateDays: 2, enabled: false },
    ])
    vi.spyOn(client, 'getEventChannels').mockResolvedValue((['live', 'scheduled', 'ended', 'hidden'] as const).map((state, index) => ({
      id: `event-${index}`,
      templateId: 'enabled',
      channelId: index === 0 ? 'channel-1' : null,
      slotNumber: index + 1,
      eventTitle: index === 3 ? null : `${state} game`,
      eventStart: index === 2 ? null : timestamp,
      eventEnd: null,
      rawStreamName: index === 1 ? null : `${state} source`,
      state,
    })))
    const scan = vi.spyOn(client, 'scanEventTemplate').mockResolvedValue({ ok: true, message: 'Scan complete.' })
    renderWithQuery(<EventsPage client={client} />)
    expect(await screen.findByText('NFL events')).toBeInTheDocument()
    expect(screen.getByText('Slot 4')).toBeInTheDocument()
    expect(screen.getByText(/Not scheduled/)).toBeInTheDocument()
    expect(screen.getByText('No event channels found. Scan provider streams to detect events.')).toBeInTheDocument()
    await userEvent.click(screen.getAllByRole('button', { name: 'Scan' })[0]!)
    await waitFor(() => expect(scan).toHaveBeenCalledWith('enabled'))
  })

  it('toggles event template enabled state through the partial PATCH control', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getEventTemplates').mockResolvedValue([
      { id: 'enabled', name: 'enabled', displayName: 'NFL events', matchRegex: 'x', channelNameFormat: 'x', groupName: 'Sports', eventDurationHours: 3, pastDateGraceHours: 6, futureDateDays: 7, enabled: true },
    ])
    vi.spyOn(client, 'getEventChannels').mockResolvedValue([])
    const update = vi.spyOn(client, 'updateEventTemplate').mockResolvedValue({
      id: 'enabled', name: 'enabled', displayName: 'NFL events', matchRegex: 'x', channelNameFormat: 'x', groupName: 'Sports', eventDurationHours: 3, pastDateGraceHours: 6, futureDateDays: 7, enabled: false,
    })
    renderWithQuery(<EventsPage client={client} />)
    const disableButton = await screen.findByRole('button', { name: 'Disable NFL events' })
    expect(disableButton).toHaveAttribute('aria-pressed', 'true')
    await userEvent.click(disableButton)
    await waitFor(() => expect(update).toHaveBeenCalledWith('enabled', { enabled: false }))
  })

  it('submits duration, grace, and future window fields from the manual event form', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getEventTemplates').mockResolvedValue([])
    vi.spyOn(client, 'getEventChannels').mockResolvedValue([])
    const create = vi.spyOn(client, 'createEventTemplate').mockResolvedValue({
      id: 'new', name: 'nba', displayName: 'NBA Games', matchRegex: 'NBA.*vs.*', channelNameFormat: '{event}', groupName: 'Sports', eventDurationHours: 4, pastDateGraceHours: 2, futureDateDays: 5, enabled: true,
    })
    renderWithQuery(<EventsPage client={client} />)
    await screen.findByText(/No event templates configured/)
    await userEvent.click(screen.getByRole('button', { name: 'Configure templates' }))
    await userEvent.type(screen.getByLabelText('Name'), 'nba')
    await userEvent.type(screen.getByLabelText('Display name'), 'NBA Games')
    await userEvent.type(screen.getByLabelText('Match regex'), 'NBA.*vs.*')
    await userEvent.type(screen.getByLabelText('Channel name format'), '{event}')
    await userEvent.clear(screen.getByLabelText('Duration (hours)'))
    await userEvent.type(screen.getByLabelText('Duration (hours)'), '4')
    await userEvent.clear(screen.getByLabelText('Past grace (hours)'))
    await userEvent.type(screen.getByLabelText('Past grace (hours)'), '2')
    await userEvent.clear(screen.getByLabelText('Future window (days)'))
    await userEvent.type(screen.getByLabelText('Future window (days)'), '5')
    await userEvent.click(screen.getByRole('button', { name: 'Create template' }))
    await waitFor(() => expect(create).toHaveBeenCalledWith(expect.objectContaining({
      name: 'nba', displayName: 'NBA Games', matchRegex: 'NBA.*vs.*', channelNameFormat: '{event}', groupName: 'Sports', eventDurationHours: 4, pastDateGraceHours: 2, futureDateDays: 5,
    })))
  })

  it('renders event timestamps in the browser timezone instead of America/Denver', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getEventTemplates').mockResolvedValue([
      { id: 'enabled', name: 'enabled', displayName: 'NFL events', matchRegex: 'x', channelNameFormat: 'x', groupName: 'Sports', eventDurationHours: 3, pastDateGraceHours: 6, futureDateDays: 7, enabled: true },
    ])
    vi.spyOn(client, 'getEventChannels').mockResolvedValue([
      { id: 'event-1', templateId: 'enabled', channelId: null, slotNumber: 1, eventTitle: 'Broncos game', eventStart: '2026-09-14T00:20:00Z', eventEnd: null, rawStreamName: 'Broncos source', state: 'scheduled' },
    ])
    const toLocaleStringSpy = vi.spyOn(Date.prototype, 'toLocaleString')
    renderWithQuery(<EventsPage client={client} />)
    await screen.findByText('Broncos game')
    const calls = toLocaleStringSpy.mock.calls
    expect(calls.length).toBeGreaterThan(0)
    // No call passes a hardcoded timeZone option. The browser timezone is used.
    for (const [, options] of calls) {
      expect(options).not.toEqual(expect.objectContaining({ timeZone: 'America/Denver' }))
    }
    toLocaleStringSpy.mockRestore()
  })

  it('filters and changes individual and bulk group states', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getGroups').mockResolvedValue([
      { name: 'All enabled', channelCount: 2, enabledCount: 2 },
      { name: 'All disabled', channelCount: 3, enabledCount: 0 },
      { name: 'Partial group', channelCount: 4, enabledCount: 2 },
    ])
    const toggle = vi.spyOn(client, 'setGroupEnabled').mockResolvedValue({ ok: true, message: 'Done.' })
    const bulk = vi.spyOn(client, 'setAllGroupsEnabled').mockResolvedValue({ ok: true, message: 'Done.' })
    renderWithQuery(<GroupsPage client={client} />)
    expect(await screen.findByText('all enabled')).toBeInTheDocument()
    expect(screen.getByText('all disabled')).toBeInTheDocument()
    expect(screen.getByText('partial')).toBeInTheDocument()
    const partial = screen.getByText('Partial group').closest('.rounded-xl') as HTMLElement
    await userEvent.click(within(partial).getByRole('button', { name: 'Enable all' }))
    await waitFor(() => expect(toggle).toHaveBeenCalledWith('Partial group', true))
    await userEvent.click(screen.getByRole('button', { name: 'Disable all groups' }))
    await waitFor(() => expect(bulk).toHaveBeenCalledWith(false))
    await userEvent.type(screen.getByPlaceholderText('Search by group name'), 'missing')
    expect(screen.getByText('No groups match your search.')).toBeInTheDocument()
  })

  it('filters, pages, and changes channel and group visibility', async () => {
    const client = new MockIptvApiClient()
    const channels = [
      { ...mockChannels[0]!, enabled: true },
      { ...mockChannels[8]!, enabled: false },
    ]
    const getChannels = vi.spyOn(client, 'getChannels').mockResolvedValue({ total: 101, limit: 50, offset: 0, items: channels })
    vi.spyOn(client, 'getGroups').mockResolvedValue([{ name: 'Sports', channelCount: 8, enabledCount: 7 }])
    const channelToggle = vi.spyOn(client, 'setChannelEnabled').mockResolvedValue({ ok: true, message: 'Done.' })
    const groupToggle = vi.spyOn(client, 'setGroupEnabled').mockResolvedValue({ ok: true, message: 'Done.' })
    renderWithQuery(<ChannelsPage client={client} />)
    expect(await screen.findByText('NBA TV')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Preview NBA TV' })).toBeDisabled()
    await userEvent.click(screen.getByRole('button', { name: 'Disable KWGN Denver' }))
    await waitFor(() => expect(channelToggle).toHaveBeenCalledWith('channel-1', false))
    await userEvent.click(screen.getByRole('button', { name: 'Filter by group' }))
    await userEvent.click(screen.getByRole('option', { name: /Sports/ }).querySelector('button')!)
    expect(screen.getByRole('button', { name: 'Enable all' })).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Disable all' }))
    await waitFor(() => expect(groupToggle).toHaveBeenCalledWith('Sports', false))
    await userEvent.click(screen.getByRole('button', { name: 'Filter by enabled state' }))
    await userEvent.click(screen.getByRole('option', { name: 'Disabled' }).querySelector('button')!)
    await userEvent.type(screen.getByLabelText('Search channels'), 'NBA')
    await waitFor(() => expect(getChannels).toHaveBeenCalledWith(expect.objectContaining({ search: 'NBA', group: 'Sports', enabled: false })), { timeout: 1000 })
    await userEvent.click(screen.getByRole('button', { name: /Next/ }))
    await waitFor(() => expect(getChannels).toHaveBeenCalledWith(expect.objectContaining({ offset: 50 })))
    await userEvent.click(screen.getByRole('button', { name: 'Prev' }))
  })

  it('edits source settings, runs sync actions, and confirms removal', async () => {
    const client = new MockIptvApiClient()
    const sources = [
      { ...mockSources[0]!, refreshIntervalSeconds: 30 },
      { ...mockSources[1]!, refreshIntervalSeconds: 300 },
      { ...mockSources[2]!, refreshIntervalSeconds: 90000 },
    ]
    vi.spyOn(client, 'getSources').mockResolvedValue(sources)
    vi.spyOn(client, 'getSourceSyncStatus').mockImplementation(async (id) => id === 'source-prime'
      ? { jobId: 'job-1', status: 'running', stage: 'parsing', percent: 44, message: 'Parse entries', bytesDownloaded: 2048, recordsProcessed: 1200, startedAt: timestamp, updatedAt: timestamp }
      : { jobId: `job-${id}`, status: 'succeeded', stage: 'completed', percent: 100, message: 'Done.', startedAt: timestamp, updatedAt: timestamp })
    const cancel = vi.spyOn(client, 'cancelSourceSync').mockResolvedValue({ ok: true, message: 'Cancelled.' })
    const interval = vi.spyOn(client, 'setSourceRefreshInterval').mockResolvedValue()
    const update = vi.spyOn(client, 'updateSource').mockResolvedValue()
    const sync = vi.spyOn(client, 'triggerSourceSync').mockResolvedValue({ jobId: 'new-job', message: 'Sync queued.' })
    const remove = vi.spyOn(client, 'deleteSource').mockResolvedValue()
    renderWithQuery(<SourcesPage client={client} />)
    expect(await screen.findByText('Parse entries')).toBeInTheDocument()
    expect(screen.getByText(/2.0 KB/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Cancel sync' }))
    await waitFor(() => expect(cancel).toHaveBeenCalledWith('source-prime'))

    await userEvent.click(screen.getByText('5m'))
    const intervalInput = screen.getByLabelText('Refresh interval in seconds')
    await userEvent.clear(intervalInput)
    await userEvent.type(intervalInput, '600')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(interval).toHaveBeenCalledWith('source-local', 600))

    await userEvent.click(screen.getByRole('button', { name: 'Edit North America guide' }))
    const dialog = screen.getByRole('dialog')
    await userEvent.clear(within(dialog).getByLabelText('Max connections'))
    await userEvent.type(within(dialog).getByLabelText('Max connections'), '3')
    await userEvent.clear(within(dialog).getByLabelText('Timezone'))
    await userEvent.type(within(dialog).getByLabelText('Timezone'), 'America/Denver')
    await userEvent.click(within(dialog).getByRole('checkbox'))
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save changes' }))
    await waitFor(() => expect(update).toHaveBeenCalledWith('source-guide', { maxConnections: 3, timezone: 'America/Denver', enabled: false }))

    await userEvent.click(screen.getByRole('button', { name: 'Sync Front Range tuner' }))
    await waitFor(() => expect(sync).toHaveBeenCalledWith('source-local'))
    await userEvent.click(screen.getByRole('button', { name: 'Remove Front Range tuner' }))
    await userEvent.click(screen.getByRole('button', { name: 'Yes' }))
    await waitFor(() => expect(remove).toHaveBeenCalledWith('source-local'))
  })

  it('reconciles, removes, reviews, and pages EPG mappings', async () => {
    const client = new MockIptvApiClient()
    const mapping = (channelId: string, confidence: number, withNames = true) => ({
      channelId,
      epgChannelId: `epg-${channelId}`,
      method: confidence > 0.9 ? 'tvg-id' : 'fuzzy',
      confidence,
      evidence: {},
      reviewStatus: 'applied' as const,
      reviewedBy: null,
      reviewedAt: null,
      revision: 1,
      updatedAt: timestamp,
      channelName: `Channel ${channelId}`,
      canonicalKey: withNames ? `key.${channelId}` : null,
      epgXmltvId: withNames ? `xmltv.${channelId}` : null,
      epgDisplayName: withNames ? `EPG ${channelId}` : null,
    })
    const getMappings = vi.spyOn(client, 'getEpgMappings').mockImplementation(async (status, limit = 50, offset = 0) => ({
      total: 101,
      limit,
      offset,
      items: status === 'review'
        ? [{ ...mapping('review', 0.82, false), reviewStatus: 'review' as const }]
        : [mapping('high', 0.99), mapping('medium', 0.9), mapping('low', 0.5, false)],
    }))
    const reconcile = vi.spyOn(client, 'reconcileEpg').mockResolvedValue({ mappingsApplied: 12, mappingsRemoved: 2, reviewQueued: 3 })
    const remove = vi.spyOn(client, 'removeChannelEpgMapping').mockResolvedValue()
    vi.spyOn(client, 'getReviewCandidates').mockResolvedValue([
      { id: 'candidate-1', channelId: 'review', epgChannelId: 'epg-one', method: 'name', confidence: 0.97, evidence: {}, createdAt: timestamp, epgXmltvId: 'one.xmltv', epgDisplayName: 'Candidate One' },
      { id: 'candidate-2', channelId: 'review', epgChannelId: 'epg-two', method: 'fuzzy', confidence: 0.88, evidence: {}, createdAt: timestamp, epgXmltvId: null, epgDisplayName: null },
    ])
    const resolve = vi.spyOn(client, 'resolveReview').mockResolvedValue()

    renderWithQuery(<EpgMappingsPage client={client} />)
    expect(await screen.findByText('Channel high')).toBeInTheDocument()
    expect(screen.getByText('50%')).toBeInTheDocument()
    expect(screen.getAllByText('—')).not.toHaveLength(0)
    await userEvent.click(screen.getByRole('button', { name: 'Reconcile now' }))
    expect(await screen.findByText(/Applied:/)).toHaveTextContent('12')
    expect(reconcile).toHaveBeenCalledOnce()
    await userEvent.click(screen.getByRole('button', { name: 'Remove mapping for Channel low' }))
    await waitFor(() => expect(remove).toHaveBeenCalledWith('low'))
    await userEvent.click(screen.getByRole('button', { name: 'Next' }))
    await waitFor(() => expect(getMappings).toHaveBeenCalledWith('applied', 50, 50))
    await userEvent.click(screen.getByRole('button', { name: 'Prev' }))

    await userEvent.click(screen.getByRole('button', { name: 'Needs review' }))
    expect(await screen.findByText('Channel review')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Review' }))
    expect(await screen.findByText('Candidate One')).toBeInTheDocument()
    expect(screen.getByText('Unknown')).toBeInTheDocument()
    await userEvent.click(screen.getAllByRole('button', { name: 'Accept' })[0]!)
    await waitFor(() => expect(resolve).toHaveBeenCalledWith('review', true, 'epg-one'))
    await userEvent.click(screen.getByRole('button', { name: 'Review' }))
    await userEvent.click(await screen.findByRole('button', { name: 'Reject all' }))
    await waitFor(() => expect(resolve).toHaveBeenCalledWith('review', false, undefined))
  })

  it('lists reconciliation revisions and rolls back a source', async () => {
    const client = new MockIptvApiClient()
    const listRevisions = vi.spyOn(client, 'listReconciliationRevisions').mockResolvedValue([
      { revision: 2, actor: 'system', createdAt: '2026-08-20T12:02:00Z', beforeValue: null, afterValue: { channels: [] } },
      { revision: 1, actor: 'system', createdAt: '2026-08-20T12:00:00Z', beforeValue: null, afterValue: { channels: [{ id: 'ch-1' }] } },
    ])
    const rollback = vi.spyOn(client, 'rollbackReconciliation').mockResolvedValue({
      targetRevision: 1,
      channelsRemoved: 1,
      channelsRestored: 1,
      streamLinksRestored: 1,
      epgMappingsRestored: 1,
    })

    renderWithQuery(<EpgMappingsPage client={client} />)
    await screen.findByRole('heading', { name: 'EPG Mappings' })

    const sourceInput = screen.getByLabelText('Source id')
    await userEvent.type(sourceInput, 'source-rollback')
    await waitFor(() => expect(listRevisions).toHaveBeenCalledWith('source-rollback'))
    expect(await screen.findByText('Revision 2')).toBeInTheDocument()

    await userEvent.click(screen.getAllByRole('button', { name: /Roll back/ })[1]!)
    await waitFor(() => expect(rollback).toHaveBeenCalledWith('source-rollback', { revision: 1 }))
    expect(await screen.findByText(/Restored 1 channel/)).toBeInTheDocument()

    // A missing revision surfaces the not-found error.
    rollback.mockRejectedValueOnce(new IptvApiError({
      type: 'urn:iptv:error:reconciliation-revision-not-found',
      title: 'Reconciliation revision not found',
      status: 404,
      detail: 'The target revision does not exist for this source.',
    }))
    await userEvent.click(screen.getAllByRole('button', { name: /Roll back/ })[0]!)
    expect(await screen.findByRole('alert')).toHaveTextContent('target revision does not exist')
  })

  it('searches and links unmapped channels to EPG channels', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getEpgMappings').mockResolvedValue({ total: 0, limit: 50, offset: 0, items: [] })
    const getUnmapped = vi.spyOn(client, 'getUnmappedChannels').mockResolvedValue({
      total: 2,
      limit: 50,
      offset: 0,
      items: [
        { id: 'unmapped-1', name: 'KUSA Denver', canonicalKey: 'kusa.denver', groupName: 'Local' },
        { id: 'unmapped-2', name: 'Mystery', canonicalKey: null, groupName: null },
      ],
    })
    const search = vi.spyOn(client, 'searchEpgChannels').mockImplementation(async (query) => query === 'none'
      ? []
      : [
          { id: 'epg-1', xmltvId: 'kusa.xmltv', displayName: 'KUSA' },
          { id: 'epg-2', xmltvId: 'raw.xmltv', displayName: null },
        ])
    const setMapping = vi.spyOn(client, 'setChannelEpgMapping').mockResolvedValue()
    renderWithQuery(<EpgMappingsPage client={client} />)
    await screen.findByRole('heading', { name: 'EPG Mappings' })
    await userEvent.click(screen.getByRole('button', { name: 'Unmapped' }))
    expect(await screen.findByText('KUSA Denver')).toBeInTheDocument()
    fireEvent.change(screen.getByLabelText('Search unmapped channels'), { target: { value: 'local' } })
    await waitFor(() => expect(getUnmapped).toHaveBeenCalledWith('local', 50, 0))
    const kusaRow = (await screen.findByText('KUSA Denver')).closest('tr')!
    await userEvent.click(within(kusaRow).getByRole('button', { name: 'Link' }))
    const epgSearch = screen.getByLabelText('Search EPG channels')
    await userEvent.type(epgSearch, 'KUSA')
    expect(await screen.findByText('raw.xmltv')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: /KUSAkusa\.xmltv/ }))
    await waitFor(() => expect(setMapping).toHaveBeenCalledWith('unmapped-1', 'epg-1'))

    const mysteryRow = screen.getByText('Mystery').closest('tr')!
    await userEvent.click(within(mysteryRow).getByRole('button', { name: 'Link' }))
    await userEvent.type(screen.getByLabelText('Search EPG channels'), 'none')
    expect(await screen.findByText('No EPG channels found. Try a different search term.')).toBeInTheDocument()
    expect(search).toHaveBeenCalledWith('none', 20)
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  })

  it('resolves, creates, cancels, and deletes channel aliases', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getChannelAliases').mockResolvedValue({
      total: 2,
      items: [
        { id: 'alias-1', canonicalName: 'ESPN', alias: 'ESPN HD', country: 'US', category: 'sports', createdAt: timestamp },
        { id: 'alias-2', canonicalName: 'BBC One', alias: 'BBC1', country: null, category: null, createdAt: timestamp },
      ],
    })
    const resolve = vi.spyOn(client, 'resolveChannelAlias')
      .mockResolvedValueOnce({ input: 'ESPN HD', canonicalName: 'ESPN' })
      .mockResolvedValueOnce({ input: 'missing', canonicalName: null })
    const create = vi.spyOn(client, 'createChannelAlias').mockResolvedValue({ id: 'new', canonicalName: 'CNN', alias: 'CNN HD', country: 'US', category: 'news', createdAt: timestamp })
    const remove = vi.spyOn(client, 'deleteChannelAlias').mockResolvedValue()

    renderWithQuery(<ChannelAliasesPage client={client} />)
    expect(await screen.findByText(/ESPN HD/)).toBeInTheDocument()
    const resolveInput = screen.getByPlaceholderText('Enter a channel name to resolve')
    await userEvent.type(resolveInput, 'ESPN HD')
    await userEvent.click(screen.getByRole('button', { name: 'Resolve' }))
    expect(await screen.findByText('ESPN', { selector: 'span' })).toBeInTheDocument()
    await userEvent.clear(resolveInput)
    await userEvent.type(resolveInput, 'missing')
    await userEvent.click(screen.getByRole('button', { name: 'Resolve' }))
    expect(await screen.findByText('No alias found for this name.')).toBeInTheDocument()
    expect(resolve).toHaveBeenCalledTimes(2)

    await userEvent.click(screen.getByRole('button', { name: 'Add alias' }))
    await userEvent.type(screen.getByPlaceholderText('ESPN'), 'CNN')
    await userEvent.type(screen.getByPlaceholderText('ESPN HD'), 'CNN HD')
    await userEvent.type(screen.getByPlaceholderText('US'), 'US')
    await userEvent.type(screen.getByPlaceholderText('sports'), 'news')
    await userEvent.click(screen.getByRole('button', { name: 'Create alias' }))
    await waitFor(() => expect(create).toHaveBeenCalledWith({ canonicalName: 'CNN', alias: 'CNN HD', country: 'US', category: 'news' }))

    await userEvent.click(screen.getByRole('button', { name: 'Add alias' }))
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    const aliasCard = screen.getByText(/BBC1/).closest('.flex.items-center.justify-between') as HTMLElement
    await userEvent.click(within(aliasCard).getByRole('button'))
    await waitFor(() => expect(remove).toHaveBeenCalledWith('alias-2'))
  })

  it('renders all recording states and manages rules and recordings', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getRecordingStats').mockResolvedValue({ scheduled: 1, recording: 1, completed: 1, failed: 1, totalBytes: 2 * 1024 ** 3 })
    vi.spyOn(client, 'getRecordingRules').mockResolvedValue([
      { id: 'rule-1', name: 'News', channelId: 'channel-1', ruleType: 'recurring', titleFilter: 'Daily', categoryFilter: null, startPaddingMinutes: 1, endPaddingMinutes: 2, maxRecordings: null, keepUntil: 'one-week', enabled: false, createdAt: timestamp, updatedAt: timestamp },
    ])
    vi.spyOn(client, 'getRecordings').mockResolvedValue({
      total: 5,
      items: (['scheduled', 'recording', 'completed', 'failed', 'cancelled'] as const).map((status, index) => ({
        id: `recording-${index}`,
        ruleId: null,
        channelId: 'channel-1',
        programmeId: null,
        title: `${status} show`,
        description: null,
        startsAt: timestamp,
        endsAt: '2026-08-20T13:00:00Z',
        status,
        filePath: null,
        fileSizeBytes: index === 2 ? 1024 ** 2 : null,
        durationSeconds: index === 2 ? 3600 : null,
        errorMessage: null,
        createdAt: timestamp,
        updatedAt: timestamp,
      })),
    })
    const create = vi.spyOn(client, 'createRecordingRule').mockResolvedValue({} as never)
    const deleteRule = vi.spyOn(client, 'deleteRecordingRule').mockResolvedValue()
    const deleteRecording = vi.spyOn(client, 'deleteRecording').mockResolvedValue()

    renderWithQuery(<RecordingsPage client={client} />)
    expect(await screen.findByText('2.0 GB')).toBeInTheDocument()
    expect(screen.getByText('Completed', { selector: 'span' })).toBeInTheDocument()
    expect(screen.getByText(/60 min/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Add rule' }))
    await userEvent.type(screen.getByPlaceholderText('Daily news'), 'Evening news')
    await userEvent.type(screen.getByPlaceholderText('UUID'), 'channel-2')
    await userEvent.selectOptions(screen.getByRole('combobox'), 'series')
    await userEvent.click(screen.getByRole('button', { name: 'Create rule' }))
    await waitFor(() => expect(create).toHaveBeenCalledWith({ name: 'Evening news', channelId: 'channel-2', ruleType: 'series' }))

    const rule = screen.getByText('News').closest('.flex.items-center.justify-between') as HTMLElement
    await userEvent.click(within(rule).getByRole('button'))
    await waitFor(() => expect(deleteRule).toHaveBeenCalledWith('rule-1'))
    const recording = screen.getByText('completed show').closest('.flex.items-center.justify-between') as HTMLElement
    await userEvent.click(within(recording).getByRole('button'))
    await waitFor(() => expect(deleteRecording).toHaveBeenCalledWith('recording-2'))
  })

  it('shows all stream health states and starts health operations', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getStreamHealthStats').mockResolvedValue({ alive: 1, dead: 1, unknown: 1, checking: 1 })
    const getHealth = vi.spyOn(client, 'getStreamHealth').mockResolvedValue({
      total: 4,
      items: (['alive', 'dead', 'unknown', 'checking'] as const).map((healthStatus, index) => ({
        providerStreamId: `stream-${index}`,
        streamName: `${healthStatus} stream`,
        groupName: index === 0 ? 'Sports' : null,
        healthStatus,
        healthCheckedAt: timestamp,
        healthError: null,
        videoCodec: index === 0 ? 'h264' : null,
        videoResolution: null,
        videoWidth: index === 0 ? 1920 : null,
        videoHeight: index === 0 ? 1080 : null,
        videoFps: index === 0 ? 60 : null,
        audioCodec: null,
        audioChannels: null,
        audioSampleRate: null,
        bitrateKbps: index === 0 ? 6000 : null,
        providerAccountId: 'provider-1',
      })),
    })
    const check = vi.spyOn(client, 'triggerHealthCheck').mockResolvedValue({ queued: 4 })
    const rank = vi.spyOn(client, 'rankAllStreams').mockResolvedValue({ ranked: 4 })

    renderWithQuery(<StreamHealthPage client={client} />)
    expect(await screen.findByText('alive stream')).toBeInTheDocument()
    expect(screen.getByText(/1920x1080/)).toHaveTextContent('@ 60fps')
    await userEvent.click(screen.getByRole('button', { name: 'Check streams' }))
    await userEvent.click(screen.getByRole('button', { name: 'Rank by quality' }))
    await waitFor(() => expect(check).toHaveBeenCalledWith(50))
    expect(rank).toHaveBeenCalledOnce()
    await userEvent.type(screen.getByPlaceholderText(/Filter by status/), 'dead')
    await userEvent.type(screen.getByPlaceholderText('Filter by group'), 'Sports')
    await waitFor(() => expect(getHealth).toHaveBeenCalledWith('dead', 'Sports', 100))
  })

  it('creates and deletes stream profiles', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getStreamProfiles').mockResolvedValue([
      { id: 'direct', name: 'Direct', profileType: 'direct', command: null, arguments: [], bufferSeconds: 0, userAgent: null, referer: null, enabled: true, createdAt: timestamp, updatedAt: timestamp },
      { id: 'vlc', name: 'VLC safe', profileType: 'vlc', command: '/usr/bin/vlc', arguments: [], bufferSeconds: 8, userAgent: 'VLC', referer: null, enabled: false, createdAt: timestamp, updatedAt: timestamp },
    ])
    const create = vi.spyOn(client, 'createStreamProfile').mockResolvedValue({} as never)
    const remove = vi.spyOn(client, 'deleteStreamProfile').mockResolvedValue()
    renderWithQuery(<StreamProfilesPage client={client} />)
    expect(await screen.findByText('VLC safe')).toBeInTheDocument()
    expect(screen.getByText(/8s buffer/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Add profile' }))
    await userEvent.type(screen.getByPlaceholderText('My FFmpeg profile'), 'FFmpeg copy')
    await userEvent.selectOptions(screen.getByRole('combobox'), 'ffmpeg')
    await userEvent.type(screen.getByPlaceholderText('/usr/bin/ffmpeg'), '/opt/ffmpeg')
    await userEvent.clear(screen.getByPlaceholderText('0'))
    await userEvent.type(screen.getByPlaceholderText('0'), '5')
    await userEvent.type(screen.getByPlaceholderText('VLC/3.0'), 'Relay')
    await userEvent.click(screen.getByRole('button', { name: 'Create profile' }))
    await waitFor(() => expect(create).toHaveBeenCalledWith({ name: 'FFmpeg copy', profileType: 'ffmpeg', command: '/opt/ffmpeg', bufferSeconds: 5, userAgent: 'Relay' }))
    const profile = screen.getByText('VLC safe').closest('.flex.items-center.justify-between') as HTMLElement
    await userEvent.click(within(profile).getByRole('button'))
    await waitFor(() => expect(remove).toHaveBeenCalledWith('vlc'))
  })

  it('renders current, upcoming, and missing-guide channels in the TV guide', async () => {
    const client = new MockIptvApiClient()
    const now = Date.now()
    vi.spyOn(client, 'getProgrammes').mockResolvedValue({
      total: 2,
      limit: 200,
      offset: 0,
      items: [
        { id: 'current', channel: 'KWGN Denver', title: 'Current news', start: new Date(now - 60_000).toISOString(), end: new Date(now + 60_000).toISOString(), source: 'XMLTV', confidence: 100 },
        { id: 'next', channel: 'KWGN Denver', title: 'Next news', start: new Date(now + 120_000).toISOString(), end: new Date(now + 180_000).toISOString(), source: 'XMLTV', confidence: 100 },
      ],
    })
    vi.spyOn(client, 'getChannels').mockResolvedValue({ total: 2, limit: 500, offset: 0, items: [mockChannels[0]!, { ...mockChannels[1]!, name: 'No Guide Channel' }] })
    renderWithQuery(<TvGuidePage client={client} />)
    expect(await screen.findByText('Current news')).toBeInTheDocument()
    expect(screen.getByText('Next news')).toBeInTheDocument()
    expect(screen.getByText('No Guide Channel')).toBeInTheDocument()
    await userEvent.type(screen.getByLabelText('Search TV guide'), 'no guide')
    await waitFor(() => expect(screen.getByText('No Guide Channel')).toBeInTheDocument(), { timeout: 1000 })
  })

  it('renders empty guide states and a zero provider capacity', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getProgrammes').mockResolvedValue({ total: 0, limit: 200, offset: 0, items: [] })
    vi.spyOn(client, 'getChannels').mockResolvedValue({ total: 0, limit: 500, offset: 0, items: [] })
    const guide = renderWithQuery(<TvGuidePage client={client} />)
    expect(await screen.findByText('No programmes are currently airing.')).toBeInTheDocument()
    expect(screen.getByText('No upcoming programmes found.')).toBeInTheDocument()
    guide.unmount()

    vi.spyOn(client, 'getOverview').mockResolvedValue({ channels: 0, healthyStreams: 0, activeSessions: 0, guideCoverage: 0, providerConnections: 0, providerLimit: 0 })
    renderWithQuery(<OverviewPage client={client} />)
    expect(await screen.findByText((_, element) => element?.tagName === 'P' && element.textContent?.replace(/\s+/g, ' ').trim() === '0 / 0')).toBeInTheDocument()
  })

  it('shows the bounded overview error state', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getOverview').mockRejectedValue(new Error('secret detail'))
    renderWithQuery(<OverviewPage client={client} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('The system overview is not available')
    expect(screen.queryByText('secret detail')).not.toBeInTheDocument()
  })

  it('creates, toggles, and deletes users across all roles', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getUsers').mockResolvedValue((['admin', 'operator', 'viewer'] as const).map((role, index) => ({
      id: `user-${index}`,
      username: role,
      displayName: `${role} user`,
      role,
      enabled: index !== 2,
      lastLoginAt: index === 0 ? timestamp : null,
      createdAt: timestamp,
      updatedAt: timestamp,
    })))
    const create = vi.spyOn(client, 'createUser').mockResolvedValue({} as never)
    const update = vi.spyOn(client, 'updateUser').mockResolvedValue({} as never)
    const remove = vi.spyOn(client, 'deleteUser').mockResolvedValue()
    renderWithQuery(<UsersPage client={client} />)
    expect(await screen.findByText('admin user')).toBeInTheDocument()
    expect(screen.getByText('Disabled')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Add user' }))
    await userEvent.type(screen.getByPlaceholderText('username'), 'new-user')
    await userEvent.type(screen.getByPlaceholderText('Display name'), 'New User')
    await userEvent.type(screen.getByPlaceholderText('Password'), 'correct horse')
    await userEvent.selectOptions(screen.getByRole('combobox'), 'operator')
    await userEvent.click(screen.getByRole('button', { name: 'Create user' }))
    await waitFor(() => expect(create).toHaveBeenCalledWith({ username: 'new-user', displayName: 'New User', password: 'correct horse', role: 'operator' }))
    const viewer = screen.getByText('viewer user').closest('.flex.items-center.justify-between') as HTMLElement
    await userEvent.click(within(viewer).getByRole('button', { name: 'Enable' }))
    await waitFor(() => expect(update).toHaveBeenCalledWith('user-2', { enabled: true }))
    await userEvent.click(within(viewer).getAllByRole('button')[1]!)
    await waitFor(() => expect(remove).toHaveBeenCalledWith('user-2'))
  })

  it('renders effective settings with inheritance source and apply requirements', async () => {
    const client = new MockIptvApiClient()
    renderWithQuery(<OperatorSettingsPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'Operator settings' })).toBeInTheDocument()
    expect(await screen.findByText('Live ring duration')).toBeInTheDocument()
    expect(screen.getAllByText('System default').length).toBeGreaterThan(0)
    expect(screen.getByText('Immediate')).toBeInTheDocument()
    expect(screen.getByText('Reimport')).toBeInTheDocument()
  })

  it('saves global overrides, surfaces a stale ETag conflict, and rolls back', async () => {
    const client = new MockIptvApiClient()
    const replace = vi.spyOn(client, 'replaceOperatorScope')
    const rollback = vi.spyOn(client, 'rollbackOperatorScope')
    renderWithQuery(<OperatorSettingsPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'Operator settings' })).toBeInTheDocument()

    // Wait for the global scope editor to load, then edit the ring duration.
    const ringInput = await screen.findByLabelText('Live ring duration')
    await userEvent.clear(ringInput)
    await userEvent.type(ringInput, '14')
    await userEvent.click(screen.getByRole('button', { name: 'Save overrides' }))
    await waitFor(() => expect(replace).toHaveBeenCalledWith('global', '', expect.objectContaining({ overrides: expect.objectContaining({ 'media.ring.duration_seconds': 14 }) })))

    // A stale If-Match (412) surfaces a conflict message.
    replace.mockRejectedValueOnce(new IptvApiError({
      type: 'urn:iptv:error:setting-revision-conflict',
      title: 'Setting revision conflict',
      status: 412,
      detail: 'The stored revision does not match the supplied If-Match.',
    }))
    await userEvent.click(screen.getByRole('button', { name: 'Save overrides' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('stored revision changed')

    // Roll back to revision 2 restores the earlier value.
    rollback.mockResolvedValueOnce({ scope: 'global', scopeId: '', overrides: { 'media.ring.duration_seconds': 12 }, revision: 3 })
    await userEvent.click(screen.getAllByRole('button', { name: /Roll back/ })[0]!)
    await waitFor(() => expect(rollback).toHaveBeenCalledWith('global', '', { revision: 2 }))
  })
})
