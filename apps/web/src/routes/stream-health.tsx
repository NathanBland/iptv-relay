import { createFileRoute } from '@tanstack/react-router'
import { StreamHealthPage } from '@/pages/stream-health-page'

export const Route = createFileRoute('/stream-health')({ component: StreamHealthPage })
