import { createFileRoute } from '@tanstack/react-router'
import { ChannelsPage } from '@/pages/channels-page'

export const Route = createFileRoute('/channels')({ component: ChannelsPage })
