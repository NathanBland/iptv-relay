import { createFileRoute } from '@tanstack/react-router'
import { OperatorSettingsPage } from '@/pages/operator-settings-page'

export const Route = createFileRoute('/operator-settings')({ component: OperatorSettingsPage })
