import { createFileRoute } from '@tanstack/react-router'
import { ChannelAliasesPage } from '@/pages/channel-aliases-page'

export const Route = createFileRoute('/channel-aliases')({ component: ChannelAliasesPage })
