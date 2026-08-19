import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createRootRoute, HeadContent, Outlet, Scripts, useRouterState } from '@tanstack/react-router'
import { useState, type ReactNode } from 'react'
import { AppShell } from '@/components/app-shell'
import { useCatalogEvents } from '@/lib/api/use-catalog-events'
import appCss from '@/styles.css?url'

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: 'utf-8' },
      { name: 'viewport', content: 'width=device-width, initial-scale=1' },
      { name: 'theme-color', content: '#07111f' },
      { title: 'Relay Control · IPTV management' },
      { name: 'description', content: 'Manage IPTV sources, channels, guide data, events, sessions, and Jellyfin publishing.' },
    ],
    links: [{ rel: 'stylesheet', href: appCss }],
  }),
  component: RootComponent,
  notFoundComponent: () => (
    <AppShell>
      <div className="rounded-xl border border-white/10 bg-ink-900 p-8">
        <p className="text-xs font-bold uppercase tracking-widest text-ocean-400">404</p>
        <h1 className="mt-2 text-2xl font-semibold text-white">Page not found</h1>
        <p className="mt-2 text-sm text-slate-400">The requested management route does not exist.</p>
      </div>
    </AppShell>
  ),
})

function RootComponent() {
  const isLogin = useRouterState({ select: (state) => state.location.pathname === '/login' })
  return (
    <RootDocument>
      {isLogin ? <Outlet /> : <ManagementLayout><Outlet /></ManagementLayout>}
    </RootDocument>
  )
}

function ManagementLayout({ children }: { children: ReactNode }) {
  useCatalogEvents()
  return <AppShell>{children}</AppShell>
}

function RootDocument({ children }: { children: ReactNode }) {
  const [queryClient] = useState(() => new QueryClient({
    defaultOptions: { queries: { staleTime: 30_000, refetchOnWindowFocus: false, retry: 1 } },
  }))
  return (
    <html lang="en">
      <head><HeadContent /></head>
      <body>
        <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
        <Scripts />
      </body>
    </html>
  )
}
