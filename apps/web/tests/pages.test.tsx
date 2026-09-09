import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { ChannelsPage } from '@/pages/channels-page'
import { EpgPage } from '@/pages/epg-page'
import { EpgMappingsPage } from '@/pages/epg-mappings-page'
import { GroupsPage } from '@/pages/groups-page'
import { EventsPage } from '@/pages/events-page'
import { JellyfinPage } from '@/pages/jellyfin-page'
import { LoginPage } from '@/pages/login-page'
import { OverviewPage } from '@/pages/overview-page'
import { SessionsPage, ringPercent, sessionHealth } from '@/pages/sessions-page'
import { SourcesPage } from '@/pages/sources-page'
import { FetchIptvApiClient, IptvApiError, MockIptvApiClient } from '@/lib/api/client'
import type { AuthStatus, Session } from '@/lib/api/types'
import { renderWithQuery } from './test-utils'

describe('management pages', () => {
  it('does not seed production fetch pages with mock data', () => {
    const pendingFetch = vi.fn(() => new Promise<Response>(() => {}))
    renderWithQuery(<OverviewPage client={new FetchIptvApiClient(pendingFetch)} />)
    expect(screen.getByRole('status')).toHaveTextContent('Wait while the system loads system overview')
    expect(screen.queryByText('Published channels')).not.toBeInTheDocument()
  })

  it('signs in through the accessible login form and preserves a safe destination', async () => {
    const onAuthenticated = vi.fn()
    renderWithQuery(<LoginPage client={new MockIptvApiClient()} next="/channels?filter=local" onAuthenticated={onAuthenticated} />)
    expect(screen.getByRole('heading', { name: 'Sign in' })).toBeInTheDocument()
    expect(screen.getByLabelText('Username')).toHaveAttribute('autocomplete', 'username')
    expect(screen.getByLabelText('Password')).toHaveAttribute('autocomplete', 'current-password')
    await userEvent.type(screen.getByLabelText('Username'), 'operator')
    await userEvent.type(screen.getByLabelText('Password'), 'secret')
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }))
    await waitFor(() => expect(onAuthenticated).toHaveBeenCalledWith('/channels?filter=local'))
  })

  it('offers the configured OIDC sign-in option', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getAuthStatus').mockResolvedValue({ authenticated: false, oidcEnabled: true })
    renderWithQuery(<LoginPage client={client} />)
    expect(await screen.findByRole('link', { name: 'Continue with SSO' })).toHaveAttribute('href', '/api/v1/auth/oidc/start')
  })

  it('shows validation and bounded authentication failures', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'login').mockRejectedValue(new IptvApiError({
      type: 'urn:iptv:error:invalid-credentials', title: 'Sign-in failed', status: 401, detail: 'Username or password is incorrect.',
    }))
    renderWithQuery(<LoginPage client={client} />)
    const username = screen.getByLabelText('Username')
    await userEvent.type(username, 'x')
    await userEvent.clear(username)
    expect(screen.getByText('Enter your username.')).toBeInTheDocument()
    await userEvent.type(username, 'operator')
    await userEvent.type(screen.getByLabelText('Password'), 'bad-password')
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Username or password is incorrect.')
  })

  it('offers a continuation when a session already exists', async () => {
    const client = new MockIptvApiClient()
    await client.login({ username: 'operator', password: 'secret' })
    renderWithQuery(<LoginPage client={client} next="//attacker.example" />)
    expect(await screen.findByRole('status')).toHaveTextContent('Signed in as Demo operator.')
    expect(screen.getByRole('link', { name: 'Continue to Relay Control' })).toHaveAttribute('href', '/')
  })

  it('handles a server that declines to establish a session', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'login').mockResolvedValue({ authenticated: false } satisfies AuthStatus)
    renderWithQuery(<LoginPage client={client} />)
    await userEvent.type(screen.getByLabelText('Username'), 'operator')
    await userEvent.type(screen.getByLabelText('Password'), 'secret')
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('did not establish')
  })

  it('blocks sign-in when session status cannot be checked safely', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getAuthStatus').mockRejectedValue(new Error('internal transport detail'))
    renderWithQuery(<LoginPage client={client} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('Unable to check sign-in status')
    expect(screen.getByRole('button', { name: 'Sign in' })).toBeDisabled()
    expect(screen.queryByText('internal transport detail')).not.toBeInTheDocument()
  })

  it('renders and refreshes overview operations', async () => {
    const client = new MockIptvApiClient()
    const overviewSpy = vi.spyOn(client, 'getOverview')
    renderWithQuery(<OverviewPage client={client} />)
    expect(await screen.findByRole('heading', { name: 'Overview' })).toBeInTheDocument()
    expect(screen.getByText('Published channels')).toBeInTheDocument()
    expect(screen.getByText('6 downstream sessions')).toBeInTheDocument()
    await waitFor(() => expect(overviewSpy).toHaveBeenCalled())
  })

  it('adds a validated source through TanStack Form and Query', async () => {
    renderWithQuery(<SourcesPage client={new MockIptvApiClient()} />)
    expect(await screen.findByText('Prime IPTV')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: /Add source/ }))
    const name = screen.getByLabelText('Display name')
    const endpoint = screen.getByLabelText('Endpoint')
    await userEvent.type(name, 'B')
    await userEvent.tab()
    expect(screen.getByText('Enter at least two characters.')).toBeInTheDocument()
    await userEvent.type(name, 'ackup feed')
    await userEvent.type(endpoint, 'https://backup.invalid/list.m3u')
    await userEvent.click(screen.getAllByRole('button', { name: 'Add source' })[1]!)
    expect(await screen.findByRole('status')).toHaveTextContent('Backup feed was added')
    expect(screen.getByText('Backup feed')).toBeInTheDocument()
  })

  it('shows separate Xtream credentials and submits them without a provider URL', async () => {
    const client = new MockIptvApiClient()
    const createSource = vi.spyOn(client, 'createSource')
    renderWithQuery(<SourcesPage client={client} />)
    await screen.findByText('Prime IPTV')
    await userEvent.click(screen.getByRole('button', { name: /Add source/ }))
    await userEvent.selectOptions(screen.getByRole('combobox', { name: 'Source type' }), 'Xtream')
    await userEvent.type(screen.getByLabelText('Display name'), 'Provider account')
    await userEvent.type(screen.getByPlaceholderText('https://provider.example:8080'), 'https://provider.example:8080')
    await userEvent.type(screen.getByLabelText('Username'), 'operator')
    await userEvent.type(screen.getByLabelText('Password'), 'secret')
    await userEvent.click(screen.getAllByRole('button', { name: 'Add source' })[1]!)
    await waitFor(() => expect(createSource).toHaveBeenCalledWith(expect.objectContaining({
      kind: 'Xtream',
      serverUrl: 'https://provider.example:8080',
      username: 'operator',
      password: 'secret',
      endpoint: '',
    })))
    await userEvent.click(screen.getByRole('button', { name: /Add source/ }))
    await userEvent.selectOptions(screen.getByRole('combobox', { name: 'Source type' }), 'Xtream')
    expect(screen.getByLabelText('Password')).toHaveValue('')
  })

  it('filters and sorts the TanStack channel table', async () => {
    renderWithQuery(<ChannelsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByText('KWGN Denver')).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent('16 channels')
    await userEvent.click(screen.getByRole('button', { name: /No./ }))
    expect(screen.getByRole('status')).toHaveTextContent('16 channels')
  })

  it('shows channel groups with enable and disable controls', async () => {
    renderWithQuery(<GroupsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'Groups', level: 1 })).toBeInTheDocument()
    const enableButtons = screen.getAllByRole('button', { name: /Enable all/ })
    const disableButtons = screen.getAllByRole('button', { name: /Disable all/ })
    expect(enableButtons.length).toBeGreaterThan(0)
    expect(disableButtons.length).toBeGreaterThan(0)
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('shows bulk enable and disable all groups buttons', async () => {
    renderWithQuery(<GroupsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'Groups', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Enable all groups/ })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Disable all groups/ })).toBeInTheDocument()
  })

  it('filters the virtualized programme guide', async () => {
    renderWithQuery(<EpgPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'EPG' })).toBeInTheDocument()
    expect(screen.getByText('Colorado’s Own News at 6')).toBeInTheDocument()
    await userEvent.type(screen.getByLabelText('Search programme guide'), 'Broncos')
    expect(await screen.findByText('Denver Broncos vs Kansas City Chiefs')).toBeInTheDocument()
    expect(await screen.findByText('1 slots')).toBeInTheDocument()
  })

  it('shows the empty EPG state and pages across multiple pages', async () => {
    const empty = new MockIptvApiClient()
    vi.spyOn(empty, 'getProgrammes').mockResolvedValue({ total: 0, limit: 100, offset: 0, items: [] })
    renderWithQuery(<EpgPage client={empty} />)
    expect(await screen.findByText('No programme data available.')).toBeInTheDocument()

    const paged = new MockIptvApiClient()
    vi.spyOn(paged, 'getProgrammes').mockResolvedValue({
      total: 150, limit: 100, offset: 0,
      items: [{ id: 'p-1', channel: 'KWGN', title: 'Late news', start: '2026-08-20T12:00:00Z', end: '2026-08-20T13:00:00Z', source: 'XMLTV', confidence: 100 }],
    })
    renderWithQuery(<EpgPage client={paged} />)
    expect(await screen.findByText('Page 1 of 2')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Next/ })).toBeEnabled()
  })

  it('shows event parsing decisions and templates', async () => {
    renderWithQuery(<EventsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByText('No event templates configured. Create a template to start detecting events from provider streams.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Configure templates/ })).toBeInTheDocument()
  })

  it('shows honest shared-session fan-out and recovery telemetry', async () => {
    renderWithQuery(<SessionsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByText('source-kwgn-hd')).toBeInTheDocument()
    expect(screen.getByText('Downstream viewers').previousSibling).toHaveTextContent('6')
    expect(screen.getByText(/Shared upstreams/).previousSibling).toHaveTextContent('3')
    expect(screen.getByText(/HTTP/)).toBeInTheDocument()
    expect(screen.getAllByRole('progressbar')).toHaveLength(3)
  })

  it('terminates a selected session after confirmation', async () => {
    const client = new MockIptvApiClient()
    const terminate = vi.spyOn(client, 'terminateSession')
    vi.spyOn(window, 'confirm').mockReturnValue(true)
    renderWithQuery(<SessionsPage client={client} />)
    await screen.findByText('source-kwgn-hd')
    await userEvent.click(screen.getAllByRole('button', { name: /Terminate/ })[0]!)
    await waitFor(() => expect(terminate).toHaveBeenCalled())
    vi.restoreAllMocks()
  })

  it('shows an empty session list without provider slots', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getSessions').mockResolvedValue([])
    renderWithQuery(<SessionsPage client={client} />)
    expect(await screen.findByText('No live shared sessions.')).toBeInTheDocument()
    expect(screen.getByText(/Shared upstreams/).previousSibling).toHaveTextContent('0')
  })

  it('maps every actor state and safely bounds ring utilization', () => {
    expect(sessionHealth('streaming')).toBe('healthy')
    expect(sessionHealth('recovering')).toBe('degraded')
    expect(sessionHealth('failing-over')).toBe('degraded')
    expect(sessionHealth('failed')).toBe('offline')
    expect(sessionHealth('priming')).toBe('syncing')
    const session = { capacityPackets: 0, retainedPackets: 10 } as Session
    expect(ringPercent(session)).toBe(0)
    expect(ringPercent({ ...session, capacityPackets: 10, retainedPackets: 20 })).toBe(100)
  })

  it('shows published Jellyfin setup URLs and copies an endpoint', async () => {
    renderWithQuery(<JellyfinPage client={new MockIptvApiClient()} />)
    const playlist = await screen.findByTestId('endpoint-M3U tuner URL')
    expect(playlist).toHaveTextContent('/out/mock-token/playlist.m3u')
    expect(screen.getByTestId('endpoint-XMLTV guide URL')).toHaveTextContent('/out/mock-token/xmltv.xml')
    expect(screen.getByTestId('endpoint-HDHomeRun device URL')).toHaveTextContent('/out/mock-token/hdhr/device.xml')
    await userEvent.click(screen.getByRole('button', { name: 'Copy M3U tuner URL' }))
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('http://iptv-web:3000/out/mock-token/playlist.m3u')
  })

  it('rotates the publish token and shows new URLs once', async () => {
    renderWithQuery(<JellyfinPage client={new MockIptvApiClient()} />)
    await screen.findByTestId('endpoint-M3U tuner URL')
    expect(screen.getByTestId('endpoint-M3U tuner URL')).toHaveTextContent('/out/mock-token/playlist.m3u')
    await userEvent.click(screen.getByRole('button', { name: 'Rotate publish token' }))
    expect(await screen.findByText('New token published. Copy the URLs below now; they show once.')).toBeInTheDocument()
    expect(screen.getByTestId('endpoint-M3U tuner URL')).toHaveTextContent('/out/rotated-token/playlist.m3u')
    expect(screen.getByTestId('endpoint-XMLTV guide URL')).toHaveTextContent('/out/rotated-token/xmltv.xml')
  })

  it('shows a regeneration-required message when the setup status is not available', async () => {
    const mock = new MockIptvApiClient()
    mock.getJellyfinSetup = vi.fn().mockResolvedValue({ status: 'regeneration-required', guideDaysMax: 30 })
    renderWithQuery(<JellyfinPage client={mock} />)
    expect(await screen.findByTestId('regeneration-required')).toHaveTextContent('A token rotation occurred. Rotate the publish token to display new URLs.')
    expect(screen.queryByTestId('endpoint-M3U tuner URL')).not.toBeInTheDocument()
  })

  it('removes token-bearing Jellyfin setup data from the query cache on unmount', async () => {
    const { queryClient, unmount } = renderWithQuery(<JellyfinPage client={new MockIptvApiClient()} />)
    await screen.findByTestId('endpoint-M3U tuner URL')
    expect(queryClient.getQueryData(['jellyfin-setup'])).toBeDefined()
    unmount()
    // The unmount cleanup removes the token-bearing setup data from the
    // TanStack Query cache so the URLs do not persist after navigation.
    expect(queryClient.getQueryData(['jellyfin-setup'])).toBeUndefined()
  })

  it('reports a token rotation failure and a published endpoints load failure', async () => {
    const rotateFail = new MockIptvApiClient()
    vi.spyOn(rotateFail, 'rotateJellyfinToken').mockRejectedValue(new Error('Token service offline.'))
    renderWithQuery(<JellyfinPage client={rotateFail} />)
    await screen.findByTestId('endpoint-M3U tuner URL')
    await userEvent.click(screen.getByRole('button', { name: 'Rotate publish token' }))
    expect(await screen.findByText('Token rotation failed.')).toBeInTheDocument()

    const loadFail = new MockIptvApiClient()
    vi.spyOn(loadFail, 'getJellyfinSetup').mockRejectedValue(new Error('Setup endpoint unavailable.'))
    renderWithQuery(<JellyfinPage client={loadFail} />)
    expect(await screen.findByText('The published endpoints could not load.')).toBeInTheDocument()
  })

  it('skips the copy action when a published endpoint value is missing', async () => {
    const mock = new MockIptvApiClient()
    vi.spyOn(mock, 'getJellyfinSetup').mockResolvedValue({
      status: 'available',
      playlistUrl: 'http://iptv-web:3000/out/mock-token/playlist.m3u',
      xmltvUrl: undefined,
      hdhrDeviceUrl: 'http://iptv-web:3000/out/mock-token/hdhr.xml',
      guideDaysMax: 30,
    })
    renderWithQuery(<JellyfinPage client={mock} />)
    await screen.findByTestId('endpoint-M3U tuner URL')
    await userEvent.click(screen.getByRole('button', { name: 'Copy XMLTV guide URL' }))
    // The missing value short-circuits the copy action and writes nothing.
    expect(navigator.clipboard.writeText).not.toHaveBeenCalledWith(undefined)
  })

  it('shows EPG mappings with confidence and method badges', async () => {
    const mock = new MockIptvApiClient()
    mock.getEpgMappings = vi.fn().mockResolvedValue({
      total: 1,
      limit: 50,
      offset: 0,
      items: [{
        channelId: 'ch-1',
        epgChannelId: 'epg-1',
        method: 'tvg-id',
        confidence: 0.99,
        evidence: { match: 'tvg-id-exact' },
        reviewStatus: 'applied',
        reviewedBy: null,
        reviewedAt: null,
        revision: 1,
        updatedAt: '2026-01-01T00:00:00Z',
        channelName: 'KUSA HD',
        canonicalKey: 'kusa.denver.example',
        epgXmltvId: 'kusa.denver.example',
        epgDisplayName: 'KUSA',
      }],
    })
    renderWithQuery(<EpgMappingsPage client={mock} />)
    expect(await screen.findByRole('heading', { name: 'EPG Mappings' })).toBeInTheDocument()
    expect(await screen.findByText('KUSA HD')).toBeInTheDocument()
    expect(screen.getByText('tvg-id')).toBeInTheDocument()
    expect(screen.getByText('99%')).toBeInTheDocument()
  })

  it('switches to the unmapped tab and shows empty state', async () => {
    const mock = new MockIptvApiClient()
    mock.getUnmappedChannels = vi.fn().mockResolvedValue({
      total: 0,
      limit: 50,
      offset: 0,
      items: [],
    })
    renderWithQuery(<EpgMappingsPage client={mock} />)
    expect(await screen.findByRole('heading', { name: 'EPG Mappings' })).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Unmapped' }))
    expect(await screen.findByText('All channels have EPG mappings.')).toBeInTheDocument()
  })
})
