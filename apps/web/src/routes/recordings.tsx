import { createFileRoute } from '@tanstack/react-router'
import { RecordingsPage } from '@/pages/recordings-page'

export const Route = createFileRoute('/recordings')({ component: RecordingsPage })
