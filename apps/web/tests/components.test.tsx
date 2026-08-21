import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

vi.mock('@tanstack/react-router', () => ({
  Link: ({ children, to, activeProps: _activeProps, activeOptions: _activeOptions, ...props }: { children: React.ReactNode; to: string; activeProps?: object; activeOptions?: object }) => <a href={to} {...props}>{children}</a>,
}))

import { AppShell } from '@/components/app-shell'
import { HealthBadge } from '@/components/health-badge'
import { LogoutButton } from '@/components/logout-button'
import { PageHeader } from '@/components/page-header'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Combobox } from '@/components/ui/combobox'
import { FieldMessage, Input, Select } from '@/components/ui/input'
import { IptvApiError, MockIptvApiClient } from '@/lib/api/client'

describe('owned management components', () => {
  it('renders the accessible shell and both navigation layouts', () => {
    render(<AppShell><h1>Current page</h1></AppShell>)
    expect(screen.getByRole('link', { name: 'Skip to content' })).toHaveAttribute('href', '#main-content')
    expect(screen.getAllByRole('navigation', { name: 'Primary navigation' })).toHaveLength(2)
    expect(screen.getAllByRole('link', { name: 'Jellyfin setup' })).toHaveLength(2)
    expect(screen.getByRole('main')).toHaveTextContent('Current page')
  })

  it('covers status and owned primitive variants', async () => {
    const onClick = vi.fn()
    render(
      <div>
        {(['healthy', 'degraded', 'offline', 'syncing'] as const).map((state) => <HealthBadge key={state} state={state} />)}
        {(['neutral', 'success', 'warning', 'danger', 'info'] as const).map((tone) => <Badge key={tone} tone={tone}>{tone}</Badge>)}
        {(['primary', 'secondary', 'ghost', 'danger'] as const).map((variant) => <Button key={variant} variant={variant} size={variant === 'ghost' ? 'icon' : variant === 'danger' ? 'sm' : 'default'} onClick={onClick}>{variant}</Button>)}
        <Card><CardHeader>Header</CardHeader><CardContent>Content</CardContent></Card>
        <Input aria-label="Name" defaultValue="Relay" />
        <Select aria-label="Type" defaultValue="M3U"><option>M3U</option></Select>
        <FieldMessage>Problem</FieldMessage>
        <FieldMessage>{undefined}</FieldMessage>
      </div>,
    )
    await userEvent.click(screen.getByRole('button', { name: 'primary' }))
    expect(onClick).toHaveBeenCalledOnce()
    expect(screen.getByText('Problem')).toBeInTheDocument()
  })

  it('renders page headers with and without actions', () => {
    const { rerender } = render(<PageHeader eyebrow="Guide" title="EPG" description="Description" actions={<Button>Action</Button>} />)
    expect(screen.getByRole('heading', { name: 'EPG' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Action' })).toBeInTheDocument()
    rerender(<PageHeader eyebrow="Relay" title="Sessions" description="Description" />)
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })

  it('signs out and exposes safe failures to assistive technology', async () => {
    const onLoggedOut = vi.fn()
    const successClient = new MockIptvApiClient()
    const failureClient = new MockIptvApiClient()
    vi.spyOn(failureClient, 'logout').mockRejectedValue(new IptvApiError({
      type: 'urn:iptv:error:logout', title: 'Sign out failed', status: 503, detail: 'Try again shortly.',
    }))
    render(
      <>
        <LogoutButton client={successClient} onLoggedOut={onLoggedOut} />
        <LogoutButton client={failureClient} onLoggedOut={onLoggedOut} />
      </>,
    )
    const buttons = screen.getAllByRole('button', { name: 'Sign out' })
    await userEvent.click(buttons[0]!)
    expect(onLoggedOut).toHaveBeenCalledOnce()
    await userEvent.click(buttons[1]!)
    expect(await screen.findByRole('alert')).toHaveTextContent('Try again shortly.')
  })

  it('keeps the operator on screen when logout is refused', async () => {
    const client = new MockIptvApiClient()
    vi.spyOn(client, 'logout').mockResolvedValue({ ok: false, message: 'Session could not be cleared.' })
    render(<LogoutButton client={client} onLoggedOut={vi.fn()} />)
    await userEvent.click(screen.getByRole('button', { name: 'Sign out' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Session could not be cleared.')
  })

  it('filters combobox items by search query and selects on click', async () => {
    const onChange = vi.fn()
    const items = [
      { value: 'sports', label: 'Sports' },
      { value: 'news', label: 'News' },
      { value: 'movies', label: 'Movies' },
    ]
    render(<Combobox items={items} value="" onChange={onChange} placeholder="All groups" />)
    const trigger = screen.getByRole('button', { name: 'All groups' })
    await userEvent.click(trigger)
    expect(screen.getByRole('listbox')).toBeVisible()
    expect(screen.getByRole('option', { name: /Sports/ })).toBeVisible()
    expect(screen.getByRole('option', { name: /News/ })).toBeVisible()
    expect(screen.getByRole('option', { name: /Movies/ })).toBeVisible()
    const searchInput = screen.getByRole('combobox')
    await userEvent.type(searchInput, 'new')
    expect(screen.queryByRole('option', { name: /Sports/ })).not.toBeInTheDocument()
    expect(screen.queryByRole('option', { name: /Movies/ })).not.toBeInTheDocument()
    expect(screen.getByRole('option', { name: /News/ })).toBeVisible()
    await userEvent.click(screen.getByRole('option', { name: /News/ }).querySelector('button')!)
    expect(onChange).toHaveBeenCalledWith('news')
  })

  it('shows empty text when no items match the search', async () => {
    const onChange = vi.fn()
    const items = [{ value: 'sports', label: 'Sports' }]
    render(<Combobox items={items} value="" onChange={onChange} placeholder="All groups" />)
    await userEvent.click(screen.getByRole('button', { name: 'All groups' }))
    await userEvent.type(screen.getByRole('combobox'), 'xyz')
    expect(screen.getByText('No items found.')).toBeVisible()
  })

  it('closes the popover on Escape', async () => {
    const onChange = vi.fn()
    const items = [{ value: 'a', label: 'Alpha' }]
    render(<Combobox items={items} value="" onChange={onChange} placeholder="Pick" />)
    await userEvent.click(screen.getByRole('button', { name: 'Pick' }))
    expect(screen.getByRole('listbox')).toBeVisible()
    await userEvent.keyboard('{Escape}')
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument()
  })
})
