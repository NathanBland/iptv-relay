import { createFileRoute } from '@tanstack/react-router'
import { SourcesPage } from '@/pages/sources-page'

export const Route = createFileRoute('/sources')({ component: SourcesPage })
