import { createFileRoute } from '@tanstack/react-router'
import { StreamProfilesPage } from '@/pages/stream-profiles-page'

export const Route = createFileRoute('/stream-profiles')({ component: StreamProfilesPage })
