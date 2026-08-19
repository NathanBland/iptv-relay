import {
  Activity,
  CalendarClock,
  CalendarRange,
  Cable,
  CircleGauge,
  Library,
  RadioTower,
  Settings2,
  Tv,
} from 'lucide-react'
import { Link } from '@tanstack/react-router'
import type { ReactNode } from 'react'
import { LogoutButton } from '@/components/logout-button'
import { cn } from '@/lib/utils'

const navigation = [
  { to: '/', label: 'Overview', icon: CircleGauge, exact: true },
  { to: '/sources', label: 'Sources', icon: RadioTower },
  { to: '/channels', label: 'Channels', icon: Tv },
  { to: '/epg', label: 'EPG', icon: CalendarRange },
  { to: '/events', label: 'Events', icon: CalendarClock },
  { to: '/sessions', label: 'Sessions', icon: Activity },
  { to: '/jellyfin', label: 'Jellyfin setup', icon: Settings2 },
] as const

function Navigation({ compact = false }: { compact?: boolean }) {
  return (
    <nav aria-label="Primary navigation" className={cn('flex gap-1', compact ? 'overflow-x-auto px-3 py-2' : 'flex-col px-3')}>
      {navigation.map((item) => {
        const { to, label, icon: Icon } = item
        return (
        <Link
          key={to}
          to={to}
          activeOptions={{ exact: 'exact' in item ? item.exact : false }}
          className={cn(
            'flex min-h-10 items-center gap-3 rounded-lg px-3 text-sm font-medium text-slate-400 transition-colors hover:bg-white/6 hover:text-white',
            compact && 'shrink-0',
          )}
          activeProps={{
            className: 'bg-ocean-400/12 text-mint-400 ring-1 ring-inset ring-ocean-400/18',
            'aria-current': 'page',
          }}
        >
          <Icon aria-hidden="true" className="size-4" />
          {label}
        </Link>
        )
      })}
    </nav>
  )
}

export function AppShell({ children }: { children: ReactNode }) {
  return (
    <div className="min-h-screen lg:grid lg:grid-cols-[15rem_1fr]">
      <a
        href="#main-content"
        className="fixed left-4 top-4 z-50 -translate-y-20 rounded-lg bg-mint-400 px-4 py-2 font-semibold text-ink-950 focus:translate-y-0"
      >
        Skip to content
      </a>

      <aside className="hidden border-r border-white/8 bg-ink-950/75 lg:flex lg:flex-col">
        <div className="flex h-17 items-center gap-3 px-6">
          <span className="grid size-9 place-items-center rounded-xl bg-ocean-400 text-ink-950 shadow-[0_0_24px_rgba(34,199,189,.2)]">
            <Cable aria-hidden="true" className="size-5" />
          </span>
          <div>
            <p className="text-sm font-bold tracking-wide text-white">Relay Control</p>
            <p className="text-[0.68rem] uppercase tracking-[0.16em] text-slate-500">IPTV orchestration</p>
          </div>
        </div>
        <Navigation />
        <div className="mt-auto p-4">
          <div className="rounded-xl border border-white/8 bg-white/[0.025] p-4">
            <div className="flex items-center justify-between text-xs text-slate-400">
              <span>Provider budget</span>
              <span className="font-mono text-mint-400">3 / 3</span>
            </div>
            <div className="mt-3 h-1.5 overflow-hidden rounded-full bg-white/8">
              <div className="h-full w-full rounded-full bg-ocean-400" />
            </div>
            <p className="mt-2 text-xs leading-5 text-slate-500">Six viewers share three upstream streams.</p>
          </div>
        </div>
      </aside>

      <div className="min-w-0">
        <header className="flex h-17 items-center justify-between border-b border-white/8 bg-ink-950/45 px-4 backdrop-blur lg:px-8">
          <div className="flex items-center gap-3 lg:hidden">
            <span className="grid size-8 place-items-center rounded-lg bg-ocean-400 text-ink-950">
              <Library aria-hidden="true" className="size-4" />
            </span>
            <span className="text-sm font-bold text-white">Relay Control</span>
          </div>
          <p className="hidden text-sm text-slate-400 lg:block">System healthy · Last inventory sync 3m ago</p>
          <div className="flex items-center gap-2">
            <div className="hidden items-center gap-2 text-xs text-slate-400 sm:flex">
              <span aria-hidden="true" className="size-2 rounded-full bg-emerald-400 shadow-[0_0_10px_rgba(52,211,153,.65)]" />
              <span>All services operational</span>
            </div>
            <LogoutButton />
          </div>
        </header>
        <div className="border-b border-white/8 bg-ink-950/65 lg:hidden">
          <Navigation compact />
        </div>
        <main id="main-content" tabIndex={-1} className="mx-auto w-full max-w-[96rem] p-4 sm:p-6 lg:p-8">
          {children}
        </main>
      </div>
    </div>
  )
}
