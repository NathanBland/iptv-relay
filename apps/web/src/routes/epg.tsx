import { createFileRoute } from '@tanstack/react-router'
import { EpgPage } from '@/pages/epg-page'

export const Route = createFileRoute('/epg')({ component: EpgPage })
