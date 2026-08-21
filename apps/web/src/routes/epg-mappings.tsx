import { createFileRoute } from '@tanstack/react-router'
import { EpgMappingsPage } from '@/pages/epg-mappings-page'

export const Route = createFileRoute('/epg-mappings')({ component: EpgMappingsPage })
