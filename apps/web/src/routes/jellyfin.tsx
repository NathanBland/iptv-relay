import { createFileRoute } from '@tanstack/react-router'
import { JellyfinPage } from '@/pages/jellyfin-page'

export const Route = createFileRoute('/jellyfin')({ component: JellyfinPage })
