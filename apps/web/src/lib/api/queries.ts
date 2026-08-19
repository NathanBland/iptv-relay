import { queryOptions } from '@tanstack/react-query'
import { apiClient } from './client'
import type { ChannelQuery, IptvApiClient, ProgrammeQuery } from './types'

export function apiQueries(client: IptvApiClient = apiClient) {
  return {
    authStatus: queryOptions({ queryKey: ['auth', 'status'], queryFn: () => client.getAuthStatus(), retry: false }),
    overview: queryOptions({ queryKey: ['overview'], queryFn: () => client.getOverview() }),
    sources: queryOptions({ queryKey: ['sources'], queryFn: () => client.getSources() }),
    channels: (query?: ChannelQuery) => queryOptions({ queryKey: ['channels', query ?? null], queryFn: () => client.getChannels(query) }),
    programmes: (query?: ProgrammeQuery) => queryOptions({ queryKey: ['programmes', query ?? null], queryFn: () => client.getProgrammes(query) }),
    events: queryOptions({ queryKey: ['events'], queryFn: () => client.getEvents() }),
    sessions: queryOptions({ queryKey: ['sessions'], queryFn: () => client.getSessions() }),
  }
}
