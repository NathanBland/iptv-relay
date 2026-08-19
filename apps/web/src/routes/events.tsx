import { createFileRoute } from '@tanstack/react-router'
import { EventsPage } from '@/pages/events-page'

export const Route = createFileRoute('/events')({ component: EventsPage })
