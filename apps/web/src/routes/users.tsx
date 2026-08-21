import { createFileRoute } from '@tanstack/react-router'
import { UsersPage } from '@/pages/users-page'

export const Route = createFileRoute('/users')({ component: UsersPage })
