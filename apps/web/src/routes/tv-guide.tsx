import { createFileRoute } from '@tanstack/react-router'
import { TvGuidePage } from '@/pages/tv-guide-page'

export const Route = createFileRoute('/tv-guide')({ component: TvGuidePage })
