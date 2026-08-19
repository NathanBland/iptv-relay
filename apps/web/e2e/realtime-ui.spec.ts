import { expect, test } from '@playwright/test'

const adminUsername = process.env.IPTV_ADMIN_USERNAME ?? 'operator'
const adminPassword = process.env.IPTV_ADMIN_PASSWORD ?? ''

test.describe('realtime UI and management routes', () => {
  test.beforeEach(async ({ page, context }) => {
    await context.request.get('/health/ready')
    await page.goto('/login')
    await page.getByLabel('Username').fill(adminUsername)
    await page.getByLabel('Password').fill(adminPassword)
    await page.getByRole('button', { name: 'Sign in' }).click()
    await expect(page).not.toHaveURL(/\/login/)
  })

  test('catalog SSE stream connects and delivers overview events', async ({ page }) => {
    await page.goto('/')
    const sseEvents = await page.evaluate(async () => {
      const events: string[] = []
      return new Promise<string[]>((resolve) => {
        const source = new EventSource('/api/v1/catalog-events')
        const timeout = setTimeout(() => {
          source.close()
          resolve(events)
        }, 8_000)
        source.addEventListener('overview', (event) => {
          events.push(`overview:${event.data}`)
          if (events.length >= 1) {
            clearTimeout(timeout)
            source.close()
            resolve(events)
          }
        })
        source.addEventListener('heartbeat', (event) => {
          events.push(`heartbeat:${event.data}`)
        })
        source.onerror = () => {
          clearTimeout(timeout)
          source.close()
          resolve(events)
        }
      })
    })
    expect(sseEvents.length).toBeGreaterThan(0)
    expect(sseEvents.some((event) => event.startsWith('overview:'))).toBe(true)
  })

  test('overview page shows real backend data without refresh buttons', async ({ page }) => {
    await page.goto('/')
    await expect(page.getByRole('heading', { name: 'Overview', level: 1 })).toBeVisible()
    await expect(page.getByText('Published channels')).toBeVisible()
    await expect(page.getByRole('heading', { name: 'Guide coverage' })).toBeVisible()
    await expect(page.getByRole('button', { name: /Refresh/ })).toHaveCount(0)
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('channels page loads paginated channel data', async ({ page, context }) => {
    await page.goto('/channels')
    await expect(page.getByRole('heading', { name: 'Channels', level: 1 })).toBeVisible()
    const channelsResponse = await context.request.get('/api/v1/channels?limit=10')
    expect(channelsResponse.ok()).toBe(true)
    const body = await channelsResponse.json()
    expect(body).toHaveProperty('items')
    expect(body).toHaveProperty('total')
    expect(body).toHaveProperty('limit')
    expect(body).toHaveProperty('offset')
    expect(Array.isArray(body.items)).toBe(true)
    await expect(page.getByRole('status')).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('programmes page loads paginated programme data', async ({ page, context }) => {
    await page.goto('/epg')
    await expect(page.getByRole('heading', { name: 'EPG', level: 1 })).toBeVisible()
    const programmesResponse = await context.request.get('/api/v1/programmes?limit=10')
    expect(programmesResponse.ok()).toBe(true)
    const body = await programmesResponse.json()
    expect(body).toHaveProperty('items')
    expect(body).toHaveProperty('total')
    expect(Array.isArray(body.items)).toBe(true)
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('events page shows empty state when no events exist', async ({ page, context }) => {
    await page.goto('/events')
    await expect(page.getByRole('heading', { name: 'Events', level: 1 })).toBeVisible()
    const eventsResponse = await context.request.get('/api/v1/events')
    expect(eventsResponse.ok()).toBe(true)
    const events = await eventsResponse.json()
    if (Array.isArray(events) && events.length === 0) {
      await expect(page.getByText('No dynamic events configured.')).toBeVisible()
    }
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('sessions page shows real session telemetry', async ({ page }) => {
    await page.goto('/sessions')
    await expect(page.getByRole('heading', { name: 'Sessions', level: 1 })).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('sources page has no manual refresh button', async ({ page }) => {
    await page.goto('/sources')
    await expect(page.getByRole('heading', { name: 'Sources', level: 1 })).toBeVisible()
    await expect(page.getByRole('button', { name: /Refresh/ })).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'Add source' })).toBeVisible()
  })

  test('jellyfin setup page loads without errors', async ({ page }) => {
    await page.goto('/jellyfin')
    await expect(page.getByRole('heading', { name: 'Jellyfin setup', level: 1 })).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('API errors surface as visible UI alerts', async ({ page, context }) => {
    await context.route('**/api/v1/system', (route) => route.fulfill({ status: 500, json: { type: 'urn:iptv:error:internal', title: 'Internal error' } }))
    await page.goto('/')
    await expect(page.getByRole('alert')).toBeVisible({ timeout: 10_000 })
    await context.unroute('**/api/v1/system')
  })

  test('sign out returns to login page', async ({ page }) => {
    await page.goto('/sources')
    await page.getByRole('button', { name: 'Sign out' }).click()
    await expect(page).toHaveURL(/\/login(?:\?|$)/)
    await expect(page.getByRole('heading', { name: 'Sign in', level: 1 })).toBeVisible()
  })
})
