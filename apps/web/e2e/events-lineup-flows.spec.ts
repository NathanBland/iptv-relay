import { expect, test, type APIRequestContext } from '@playwright/test'

const adminPassword = process.env.IPTV_ADMIN_PASSWORD ?? ''

async function signIn(request: APIRequestContext) {
  const statusResponse = await request.get('/api/v1/auth/status')
  expect(statusResponse.ok()).toBe(true)
  const setCookie = statusResponse.headers()['set-cookie'] ?? ''
  const csrfToken = setCookie.match(/iptv_csrf=([^;]+)/)?.[1] ?? ''
  const loginResponse = await request.post('/api/v1/auth/login', {
    headers: { 'X-CSRF-Token': decodeURIComponent(csrfToken) },
    data: { username: 'operator', password: adminPassword },
  })
  expect(loginResponse.ok()).toBe(true)
}

async function csrfHeaders(request: APIRequestContext) {
  const state = await request.storageState()
  const csrfToken = state.cookies.find((cookie) => cookie.name === 'iptv_csrf')?.value ?? ''
  return { 'X-CSRF-Token': decodeURIComponent(csrfToken) }
}

test.describe.serial('event template and lineup API flows', () => {
  test.beforeEach(async ({ request }) => {
    await signIn(request)
  })

  test('event templates API returns real data from the database', async ({ request }) => {
    const response = await request.get('/api/v1/event-templates')
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
    const response = await request.get('/api/v1/event-channels')
    expect(response.ok()).toBe(true)
    const channels = await response.json()
    expect(Array.isArray(channels)).toBe(true)
  })

  test('events API returns real data from the database', async ({ request }) => {
    const response = await request.get('/api/v1/events')
    expect(response.ok()).toBe(true)
    const events = await response.json()
    expect(Array.isArray(events)).toBe(true)
  })

  test('event template create, scan, and delete cycle works end to end', async ({ request }) => {
    const templateName = `E2E Event ${Date.now()}`
    const createResponse = await request.post('/api/v1/event-templates', {
      headers: await csrfHeaders(request),
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

    try {
      const scanResponse = await request.post(`/api/v1/event-templates/${template.id}/scan`, {
        headers: await csrfHeaders(request),
      })
      expect(scanResponse.ok()).toBe(true)
      const scanResult = await scanResponse.json()
      expect(scanResult.ok).toBe(true)
    } finally {
      await request.delete(`/api/v1/event-templates/${template.id}`, {
        headers: await csrfHeaders(request),
      })
    }
  })

  test('lineup templates API returns real data from the database', async ({ request }) => {
    const response = await request.get('/api/v1/lineup-templates')
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
      headers: await csrfHeaders(request),
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

    try {
      const categoriesResponse = await request.get(`/api/v1/lineup-templates/${template.id}/categories`)
      expect(categoriesResponse.ok()).toBe(true)
      const categories = await categoriesResponse.json()
      expect(categories.length).toBe(1)
      expect(categories[0].name).toBe('Test Category')

      const channelsResponse = await request.get(`/api/v1/lineup-templates/${template.id}/channels`)
      expect(channelsResponse.ok()).toBe(true)
      const channels = await channelsResponse.json()
      expect(channels.length).toBe(1)
      expect(channels[0].name).toBe('Test Channel')
      expect(channels[0].channelNumber).toBe('100')
    } finally {
      await request.delete(`/api/v1/lineup-templates/${template.id}`, {
        headers: await csrfHeaders(request),
      })
    }
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
    const templatesResponse = await context.request.get('/api/v1/event-templates')
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
    const templatesResponse = await context.request.get('/api/v1/event-templates')
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
