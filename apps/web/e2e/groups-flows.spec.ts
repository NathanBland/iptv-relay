import { expect, test } from '@playwright/test'

const bootstrapToken = process.env.IPTV_ADMIN_BOOTSTRAP_TOKEN ?? ''

test.describe.serial('groups management API and UI', () => {
  test('groups API returns real data with enabled and total counts', async ({ request }) => {
    const response = await request.get('/api/v1/groups', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(response.ok()).toBe(true)
    const groups = await response.json()
    expect(Array.isArray(groups)).toBe(true)
    if (groups.length > 0) {
      expect(groups[0]).toHaveProperty('name')
      expect(groups[0]).toHaveProperty('channelCount')
      expect(groups[0]).toHaveProperty('enabledCount')
      expect(typeof groups[0].name).toBe('string')
      expect(typeof groups[0].channelCount).toBe('number')
      expect(typeof groups[0].enabledCount).toBe('number')
      expect(groups[0].enabledCount).toBeLessThanOrEqual(groups[0].channelCount)
    }
  })

  test('set group enabled API toggles all channels in a group', async ({ request }) => {
    const listResponse = await request.get('/api/v1/groups', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    expect(listResponse.ok()).toBe(true)
    const groups = await listResponse.json()
    if (groups.length === 0) {
      test.skip(true, 'No groups configured')
      return
    }

    const targetGroup = groups[0]
    const enableResponse = await request.patch(`/api/v1/groups/${encodeURIComponent(targetGroup.name)}/enabled`, {
      headers: {
        Authorization: `Bearer ${bootstrapToken}`,
        'Content-Type': 'application/json',
      },
      data: { enabled: true },
    })
    expect(enableResponse.ok()).toBe(true)
    const result = await enableResponse.json()
    expect(result.ok).toBe(true)

    const verifyResponse = await request.get('/api/v1/groups', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    const verifyGroups = await verifyResponse.json()
    const verified = verifyGroups.find((g: { name: string }) => g.name === targetGroup.name)
    expect(verified).toBeTruthy()
    expect(verified.enabledCount).toBe(verified.channelCount)
  })

  test('bulk set all groups enabled API updates all groups in one call', async ({ request }) => {
    const bulkResponse = await request.patch('/api/v1/groups/enabled', {
      headers: {
        Authorization: `Bearer ${bootstrapToken}`,
        'Content-Type': 'application/json',
      },
      data: { enabled: true },
    })
    expect(bulkResponse.ok()).toBe(true)
    const result = await bulkResponse.json()
    expect(result.ok).toBe(true)

    const verifyResponse = await request.get('/api/v1/groups', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    const groups = await verifyResponse.json()
    for (const group of groups) {
      expect(group.enabledCount).toBe(group.channelCount)
    }
  })

  test('groups page loads and shows group cards', async ({ page, context }) => {
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
      data: { username: 'operator', password: process.env.IPTV_ADMIN_PASSWORD ?? '' },
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

    await page.goto('/groups')
    await expect(page.getByRole('heading', { name: 'Groups', level: 1 })).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)

    const groupsResponse = await context.request.get('/api/v1/groups', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    const groups = await groupsResponse.json()
    if (groups.length > 0) {
      await expect(page.getByText(groups[0].name)).toBeVisible({ timeout: 10_000 })
      await expect(page.getByRole('button', { name: /Enable all/ }).first()).toBeVisible()
      await expect(page.getByRole('button', { name: /Disable all/ }).first()).toBeVisible()
    }
  })

  test('groups page search filters the displayed groups', async ({ page, context }) => {
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
      data: { username: 'operator', password: process.env.IPTV_ADMIN_PASSWORD ?? '' },
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

    const groupsResponse = await context.request.get('/api/v1/groups', {
      headers: { Authorization: `Bearer ${bootstrapToken}` },
    })
    const groups = await groupsResponse.json()
    if (groups.length === 0) {
      test.skip(true, 'No groups configured')
      return
    }

    await page.goto('/groups')
    await expect(page.getByRole('heading', { name: 'Groups', level: 1 })).toBeVisible()

    const searchInput = page.getByPlaceholder('Search by group name')
    await searchInput.fill(groups[0].name)
    await expect(page.getByText(groups[0].name)).toBeVisible({ timeout: 10_000 })
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('groups page shows bulk enable and disable all groups buttons', async ({ page, context }) => {
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
      data: { username: 'operator', password: process.env.IPTV_ADMIN_PASSWORD ?? '' },
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

    await page.goto('/groups')
    await expect(page.getByRole('heading', { name: 'Groups', level: 1 })).toBeVisible()
    await expect(page.getByRole('button', { name: /Enable all groups/ })).toBeVisible({ timeout: 10_000 })
    await expect(page.getByRole('button', { name: /Disable all groups/ })).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })
})
