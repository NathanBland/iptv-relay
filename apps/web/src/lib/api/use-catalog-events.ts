import { useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import type { SourceSyncStatus } from './types'

type EventSourceLike = {
  addEventListener: (type: string, listener: (event: { data: string }) => void) => void
  close: () => void
}

function createEventSource(url: string): EventSourceLike | undefined {
  if (typeof window === 'undefined') return undefined
  const EventSourceCtor = window.EventSource
  if (!EventSourceCtor) return undefined
  return new EventSourceCtor(url) as unknown as EventSourceLike
}

/**
 * Subscribes to the catalog and session SSE streams and invalidates React
 * Query caches when the backend publishes updates. This replaces manual
 * refresh buttons with realtime cache invalidation.
 *
 * The catalog stream provides `overview`, `heartbeat`, and
 * `source-sync-progress` events. The session stream provides `sessions`
 * events with the full session list.
 *
 * The `source-sync-progress` events write progress data directly into the
 * `source-sync-status` query cache so that the sources page shows live
 * progress without polling.
 */
export function useCatalogEvents() {
  const queryClient = useQueryClient()

  useEffect(() => {
    const catalogSource = createEventSource('/api/v1/catalog-events')
    const sessionSource = createEventSource('/api/v1/session-events')

    if (catalogSource) {
      catalogSource.addEventListener('overview', () => {
        void queryClient.invalidateQueries({ queryKey: ['overview'] })
      })

      catalogSource.addEventListener('heartbeat', () => {
        void queryClient.invalidateQueries({ queryKey: ['sources'] })
      })

      catalogSource.addEventListener('source-sync-progress', (event) => {
        const parsed = parseSyncProgress(event.data)
        if (!parsed) return
        queryClient.setQueryData<SourceSyncStatus>(
          ['source-sync-status', parsed.sourceId],
          {
            jobId: parsed.jobId,
            status: parsed.status as SourceSyncStatus['status'],
            stage: parsed.stage as SourceSyncStatus['stage'],
            percent: parsed.percent,
            message: parsed.message,
            bytesDownloaded: parsed.bytesDownloaded,
            recordsProcessed: parsed.recordsProcessed,
            updatedAt: parsed.updatedAt,
          },
        )
      })
    }

    if (sessionSource) {
      sessionSource.addEventListener('sessions', () => {
        void queryClient.invalidateQueries({ queryKey: ['sessions'] })
      })
    }

    return () => {
      catalogSource?.close()
      sessionSource?.close()
    }
  }, [queryClient])
}

interface SyncProgressEvent {
  sourceId: string
  jobId: string
  status: string
  stage: string
  percent: number
  message: string
  bytesDownloaded: number
  recordsProcessed: number
  updatedAt: string
}

function parseSyncProgress(data: string): SyncProgressEvent | undefined {
  try {
    const value = JSON.parse(data) as Partial<SyncProgressEvent>
    if (!value.sourceId || !value.jobId || !value.status || !value.stage) return undefined
    return {
      sourceId: value.sourceId,
      jobId: value.jobId,
      status: value.status,
      stage: value.stage,
      percent: value.percent ?? 0,
      message: value.message ?? '',
      bytesDownloaded: value.bytesDownloaded ?? 0,
      recordsProcessed: value.recordsProcessed ?? 0,
      updatedAt: value.updatedAt ?? new Date().toISOString(),
    }
  } catch {
    return undefined
  }
}
