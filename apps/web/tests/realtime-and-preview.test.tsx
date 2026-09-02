import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import type { ReactNode } from 'react'
import { describe, expect, it, vi } from 'vitest'

const player = vi.hoisted(() => ({
  attachMediaElement: vi.fn(),
  load: vi.fn(),
  play: vi.fn(),
  pause: vi.fn(),
  unload: vi.fn(),
  detachMediaElement: vi.fn(),
  destroy: vi.fn(),
}))
const mpegts = vi.hoisted(() => ({
  supported: true,
  getFeatureList: vi.fn(() => ({ mseLivePlayback: mpegts.supported })),
  createPlayer: vi.fn(() => player),
}))

vi.mock('mpegts.js', () => ({ default: mpegts }))

import { StreamPreview } from '@/components/stream-preview'
import { MockIptvApiClient } from '@/lib/api/client'
import { useCatalogEvents } from '@/lib/api/use-catalog-events'

function EventSubscriber() {
  useCatalogEvents()
  return <p>subscribed</p>
}

function QueryWrapper({ client, children }: { client: QueryClient; children: ReactNode }) {
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>
}

class EventSourceStub {
  static instances: EventSourceStub[] = []
  readonly listeners = new Map<string, (event: { data: string }) => void>()
  readonly close = vi.fn()

  constructor(readonly url: string) {
    EventSourceStub.instances.push(this)
  }

  addEventListener(type: string, listener: (event: { data: string }) => void) {
    this.listeners.set(type, listener)
  }

  emit(type: string, data = '') {
    this.listeners.get(type)?.({ data })
  }
}

describe('stream preview', () => {
  it('plays, stops, resumes, closes, and destroys an MPEG-TS preview', async () => {
    mpegts.supported = true
    player.load.mockImplementation(() => undefined)
    player.play.mockImplementation(() => undefined)
    const client = new MockIptvApiClient()
    const onClose = vi.fn()
    const view = render(<StreamPreview channelId="channel / one" channelName="KUSA" client={client} onClose={onClose} />)
    expect(await screen.findByText('Live')).toBeInTheDocument()
    expect(mpegts.createPlayer).toHaveBeenCalledWith(expect.objectContaining({ url: '/api/v1/channels/channel%20%2F%20one/stream' }), expect.any(Object))
    await userEvent.click(screen.getByRole('button', { name: 'Stop' }))
    expect(screen.getByText('Stopped')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Resume' }))
    expect(await screen.findByText('Live')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Close' }))
    expect(onClose).toHaveBeenCalledOnce()
    view.unmount()
    expect(player.pause).toHaveBeenCalled()
    expect(player.detachMediaElement).toHaveBeenCalled()
    expect(player.destroy).toHaveBeenCalled()
  })

  it('reports missing browser support and client failures', async () => {
    mpegts.supported = false
    const client = new MockIptvApiClient()
    const first = render(<StreamPreview channelId="one" channelName="One" client={client} onClose={vi.fn()} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('does not support MPEG-TS')
    first.unmount()

    mpegts.supported = true
    vi.spyOn(client, 'getChannelPreview').mockRejectedValue(new Error('Preview denied.'))
    const second = render(<StreamPreview channelId="two" channelName="Two" client={client} onClose={vi.fn()} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('Preview denied.')
    second.unmount()
  })

  it('reports resume failures', async () => {
    mpegts.supported = true
    player.load.mockImplementation(() => undefined)
    const client = new MockIptvApiClient()
    render(<StreamPreview channelId="three" channelName="Three" client={client} onClose={vi.fn()} />)
    expect(await screen.findByText('Live')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Stop' }))
    player.load.mockImplementationOnce(() => { throw new Error('resume') })
    await userEvent.click(screen.getByRole('button', { name: 'Resume' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Failed to resume playback.')
  })

  it('reports a generic message for non-Error preview failures', async () => {
    mpegts.supported = true
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'getChannelPreview').mockRejectedValue('unexpected failure')
    render(<StreamPreview channelId="four" channelName="Four" client={client} onClose={vi.fn()} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('Failed to load stream preview.')
  })
})

describe('catalog event subscription', () => {
  it('invalidates caches, writes bounded progress, and closes both sources', async () => {
    EventSourceStub.instances = []
    Object.defineProperty(window, 'EventSource', { configurable: true, value: EventSourceStub })
    const queryClient = new QueryClient()
    const invalidate = vi.spyOn(queryClient, 'invalidateQueries')
    const setData = vi.spyOn(queryClient, 'setQueryData')
    const view = render(<EventSubscriber />, { wrapper: ({ children }) => <QueryWrapper client={queryClient}>{children}</QueryWrapper> })
    expect(screen.getByText('subscribed')).toBeInTheDocument()
    expect(EventSourceStub.instances.map((source) => source.url)).toEqual(['/api/v1/catalog-events', '/api/v1/session-events'])
    const catalog = EventSourceStub.instances[0]!
    const sessions = EventSourceStub.instances[1]!
    catalog.emit('overview')
    catalog.emit('heartbeat')
    sessions.emit('sessions')
    catalog.emit('source-sync-progress', JSON.stringify({
      sourceId: 'source-1',
      jobId: 'job-1',
      status: 'running',
      stage: 'parsing',
      percent: 55,
      message: 'Parse data',
      bytesDownloaded: 123,
      recordsProcessed: 44,
      updatedAt: '2026-08-20T12:00:00Z',
    }))
    catalog.emit('source-sync-progress', '{')
    catalog.emit('source-sync-progress', JSON.stringify({ sourceId: 'source-1' }))
    await waitFor(() => expect(invalidate).toHaveBeenCalledTimes(3))
    expect(setData).toHaveBeenCalledWith(['source-sync-status', 'source-1'], expect.objectContaining({ percent: 55, bytesDownloaded: 123 }))
    view.unmount()
    expect(catalog.close).toHaveBeenCalledOnce()
    expect(sessions.close).toHaveBeenCalledOnce()
  })

  it('does not subscribe when EventSource is unavailable', () => {
    Object.defineProperty(window, 'EventSource', { configurable: true, value: undefined })
    const queryClient = new QueryClient()
    render(<EventSubscriber />, { wrapper: ({ children }) => <QueryWrapper client={queryClient}>{children}</QueryWrapper> })
    expect(screen.getByText('subscribed')).toBeInTheDocument()
  })

  it('writes terminal sync status and defaults missing optional progress fields', async () => {
    EventSourceStub.instances = []
    Object.defineProperty(window, 'EventSource', { configurable: true, value: EventSourceStub })
    const queryClient = new QueryClient()
    const setData = vi.spyOn(queryClient, 'setQueryData')
    const removeQueries = vi.spyOn(queryClient, 'removeQueries')
    render(<EventSubscriber />, { wrapper: ({ children }) => <QueryWrapper client={queryClient}>{children}</QueryWrapper> })
    const catalog = EventSourceStub.instances[0]!

    // A terminal status writes the final state and schedules cache removal.
    catalog.emit('source-sync-progress', JSON.stringify({
      sourceId: 'source-2', jobId: 'job-2', status: 'succeeded', stage: 'completed', percent: 100,
      message: 'Done.', bytesDownloaded: 500, recordsProcessed: 10, updatedAt: '2026-08-20T12:00:00Z',
    }))
    await waitFor(() => expect(setData).toHaveBeenCalledWith(['source-sync-status', 'source-2'], expect.objectContaining({ status: 'succeeded' })))

    // An active status with only required fields defaults the optional fields.
    catalog.emit('source-sync-progress', JSON.stringify({
      sourceId: 'source-3', jobId: 'job-3', status: 'running', stage: 'downloading',
    }))
    await waitFor(() => expect(setData).toHaveBeenCalledWith(['source-sync-status', 'source-3'], expect.objectContaining({
      percent: 0, message: '', bytesDownloaded: 0, recordsProcessed: 0,
    })))
    expect(removeQueries).not.toHaveBeenCalled()
  })
})
