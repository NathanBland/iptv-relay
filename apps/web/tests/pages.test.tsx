import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { ChannelsPage } from '@/pages/channels-page'
import { EpgPage } from '@/pages/epg-page'
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

  it('filters and sorts the TanStack channel table', async () => {
    renderWithQuery(<ChannelsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByText('KWGN Denver')).toBeInTheDocument()
    await userEvent.type(screen.getByLabelText('Search channels'), 'ESPN2')
    expect(screen.getByText('ESPN2')).toBeInTheDocument()
    expect(screen.queryByText('KWGN Denver')).not.toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: /No./ }))
    expect(screen.getByRole('status')).toHaveTextContent('1 of 16 channels')
  })

  it('filters the virtualized programme guide', async () => {
    renderWithQuery(<EpgPage client={new MockIptvApiClient()} />)
    expect(await screen.findByRole('heading', { name: 'EPG' })).toBeInTheDocument()
    expect(screen.getByText('Colorado’s Own News at 6')).toBeInTheDocument()
    await userEvent.type(screen.getByLabelText('Filter programme guide'), 'Broncos')
    expect(screen.getByText('Denver Broncos vs Kansas City Chiefs')).toBeInTheDocument()
    expect(screen.getByText('1 slots')).toBeInTheDocument()
  })

  it('shows event parsing decisions and templates', async () => {
    renderWithQuery(<EventsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByText('Denver Broncos vs Kansas City Chiefs')).toBeInTheDocument()
    expect(screen.getByText('The system quarantines unparseable or DST-ambiguous titles. The system does not use guesswork to schedule them.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Review Denver Broncos/ })).toBeInTheDocument()
  })

  it('shows honest shared-session fan-out and recovery telemetry', async () => {
    renderWithQuery(<SessionsPage client={new MockIptvApiClient()} />)
    expect(await screen.findByText('source-kwgn-hd')).toBeInTheDocument()
    expect(screen.getByText('Downstream viewers').previousSibling).toHaveTextContent('6')
    expect(screen.getByText(/Shared upstreams/).previousSibling).toHaveTextContent('3')
    expect(screen.getByText(/HTTP/)).toBeInTheDocument()
    expect(screen.getAllByRole('progressbar')).toHaveLength(3)
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

  it('validates and saves Jellyfin setup and copies an endpoint', async () => {
    renderWithQuery(<JellyfinPage client={new MockIptvApiClient()} />)
    const publicUrl = screen.getByLabelText('Relay URL visible to Jellyfin')
    await userEvent.clear(publicUrl)
    await userEvent.type(publicUrl, 'invalid')
    await userEvent.tab()
    expect(screen.getByText('Enter an absolute URL reachable by Jellyfin.')).toBeInTheDocument()
    await userEvent.clear(publicUrl)
    await userEvent.type(publicUrl, 'http://relay:3000')
    await userEvent.click(screen.getByRole('button', { name: 'Save Jellyfin setup' }))
    expect(await screen.findByRole('status')).toHaveTextContent('Saved tuner')
    await userEvent.click(screen.getByRole('button', { name: 'Copy M3U tuner URL' }))
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('http://iptv-web:3000/api/jellyfin/playlist.m3u')
  })
})
