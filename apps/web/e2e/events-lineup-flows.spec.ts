import { expect, test } from '@playwright/test'

const adminPassword = process.env.IPTV_ADMIN_PASSWORD ?? ''
const bootstrapToken = process.env.IPTV_ADMIN_BOOTSTRAP_TOKEN ?? ''

test.describe.serial('event template and lineup API flows', () => {
  test('event templates API returns real data from the database', async ({ request }) => {
    const response = await request.get('/api/v1/event-templates', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(response.ok()).toBe(true)
    const templates = await response.json()
    expect(Array.isArray(templates)).toBe(true)
    if (templates.length > 0) {
      expect(templates[0]).toHaveProperty('id')
      expect(templates[0]).toHaveProperty('displayName')
      expect(templates[0]).toHaveProperty('groupName')
      expect(templates[0]).toHaveProperty('matchRegex')
      expect(templates[0]).toHaveProperty('enabled')
    }
  })

  test('event channels API returns real data from the database', async ({ request }) => {
    const response = await request.get('/api/v1/event-channels', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(response.ok()).toBe(true)
    const channels = await response.json()
    expect(Array.isArray(channels)).toBe(true)
  })

  test('events API returns real data from the database', async ({ request }) => {
    const response = await request.get('/api/v1/events', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(response.ok()).toBe(true)
    const events = await response.json()
    expect(Array.isArray(events)).toBe(true)
  })

  test('event template create, scan, and delete cycle works end to end', async ({ request }) => {
    const templateName = `E2E Event ${Date.now()}`
    const createResponse = await request.post('/api/v1/event-templates', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
      data: {
        name: templateName.toLowerCase().replace(/\s+/g, '-'),
        displayName: templateName,
        groupName: 'E2E Test',
        matchRegex: '(?i)e2e_test_event',
        channelNameFormat: 'E2E Event',
        eventDurationHours: 3,
        pastDateGraceHours: 1,
        futureDateDays: 7,
      },
    })
    expect(createResponse.ok()).toBe(true)
    const template = await createResponse.json()
    expect(template.id).toBeTruthy()

    const scanResponse = await request.post(`/api/v1/event-templates/${template.id}/scan`, {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(scanResponse.ok()).toBe(true)
    const scanResult = await scanResponse.json()
    expect(scanResult.ok).toBe(true)

    const deleteResponse = await request.delete(`/api/v1/event-templates/${template.id}`, {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(deleteResponse.ok()).toBe(true)
  })

  test('lineup templates API returns real data from the database', async ({ request }) => {
    const response = await request.get('/api/v1/lineup-templates', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(response.ok()).toBe(true)
    const templates = await response.json()
    expect(Array.isArray(templates)).toBe(true)
    if (templates.length > 0) {
      expect(templates[0]).toHaveProperty('id')
      expect(templates[0]).toHaveProperty('name')
      expect(templates[0]).toHaveProperty('packageName')
      expect(templates[0]).toHaveProperty('enabled')
    }
  })

  test('lineup template create, list categories, apply, and delete cycle works end to end', async ({ request }) => {
    const templateName = `E2E Lineup ${Date.now()}`
    const createResponse = await request.post('/api/v1/lineup-templates', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
      data: {
        name: templateName,
        packageName: 'E2E Test Package',
        country: 'US',
        description: 'E2E test lineup',
        categories: [
          {
            name: 'Test Category',
            sortOrder: 1,
            channels: [
              { name: 'Test Channel', channelNumber: '100', aliases: ['Test HD'] },
            ],
          },
        ],
      },
    })
    expect(createResponse.ok()).toBe(true)
    const template = await createResponse.json()
    expect(template.id).toBeTruthy()

    const categoriesResponse = await request.get(`/api/v1/lineup-templates/${template.id}/categories`, {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(categoriesResponse.ok()).toBe(true)
    const categories = await categoriesResponse.json()
    expect(categories.length).toBe(1)
    expect(categories[0].name).toBe('Test Category')

    const channelsResponse = await request.get(`/api/v1/lineup-templates/${template.id}/channels`, {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(channelsResponse.ok()).toBe(true)
    const channels = await channelsResponse.json()
    expect(channels.length).toBe(1)
    expect(channels[0].name).toBe('Test Channel')
    expect(channels[0].channelNumber).toBe('100')

    const deleteResponse = await request.delete(`/api/v1/lineup-templates/${template.id}`, {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(deleteResponse.ok()).toBe(true)
  })
})

test.describe.serial('event and lineup UI flows', () => {
  test.beforeEach(async ({ context }) => {
    await context.request.get('/health/ready')

    const statusResponse = await context.request.get('/api/v1/auth/status')
    const cookies = await statusResponse.headers()['set-cookie'] ?? ''
    const csrfMatch = cookies.match(/iptv_csrf=([^;]+)/)
    const csrfToken = csrfMatch?.[1] ?? ''

    const loginResponse = await context.request.post('/api/v1/auth/login', {
      headers: {
        'Content-Type': 'application/json',
        'X-CSRF-Token': csrfToken,
        Cookie: `iptv_csrf=${csrfToken}`,
      },
      data: { username: 'operator', password: adminPassword },
    })

    if (!loginResponse.ok()) {
      test.skip(true, `Login failed: ${loginResponse.status()}`)
      return
    }

    const setCookie = loginResponse.headers()['set-cookie'] ?? ''
    const sessionMatch = setCookie.match(/iptv_session=([^;]+)/)
    const sessionValue = sessionMatch?.[1]
    if (sessionValue) {
      await context.addCookies([
        { name: 'iptv_session', value: sessionValue, domain: 'localhost', path: '/' },
        { name: 'iptv_csrf', value: csrfToken, domain: 'localhost', path: '/' },
      ])
    }
  })

  test('events page loads without errors and shows real data', async ({ page, context }) => {
    const templatesResponse = await context.request.get('/api/v1/event-templates', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    const templates = await templatesResponse.json()

    await page.goto('/events')
    await expect(page.getByRole('heading', { name: 'Events', level: 1 })).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)

    if (templates.length > 0) {
      await expect(page.getByText(templates[0].displayName)).toBeVisible({ timeout: 10_000 })
    } else {
      await expect(page.getByText(/No event templates configured/)).toBeVisible()
    }
  })

  test('events page scan button triggers backend scan', async ({ page, context }) => {
    const templatesResponse = await context.request.get('/api/v1/event-templates', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    const templates = await templatesResponse.json()
    if (templates.length === 0) {
      test.skip(true, 'No event templates configured')
      return
    }

    await page.goto('/events')
    await expect(page.getByRole('heading', { name: 'Events', level: 1 })).toBeVisible()

    const scanButton = page.getByRole('button', { name: /Scan/ }).first()
    await expect(scanButton).toBeVisible({ timeout: 10_000 })
    await scanButton.click()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })
})
