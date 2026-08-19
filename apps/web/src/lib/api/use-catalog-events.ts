import { useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'

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
 * Subscribes to the catalog SSE stream and invalidates React Query caches
 * when the backend publishes overview or source updates. This replaces
 * manual refresh buttons with realtime cache invalidation.
 */
export function useCatalogEvents() {
  const queryClient = useQueryClient()

  useEffect(() => {
    const eventSource = createEventSource('/api/v1/catalog-events')
    if (!eventSource) return

    eventSource.addEventListener('overview', () => {
      void queryClient.invalidateQueries({ queryKey: ['overview'] })
    })

    eventSource.addEventListener('heartbeat', () => {
      void queryClient.invalidateQueries({ queryKey: ['sources'] })
    })

    return () => eventSource.close()
  }, [queryClient])
}
