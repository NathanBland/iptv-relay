import { expect, test } from '@playwright/test'

const adminUsername = process.env.IPTV_ADMIN_USERNAME ?? 'operator'
const adminPassword = process.env.IPTV_ADMIN_PASSWORD ?? ''

test.describe.serial('source deletion and channel filtering', () => {
  test.beforeEach(async ({ page, context }) => {
    await context.request.get('/health/ready')
    await page.goto('/login')
    await page.getByLabel('Username').fill(adminUsername)
    await page.getByLabel('Password').fill(adminPassword)
    await page.getByRole('button', { name: 'Sign in' }).click()
    await expect(page).not.toHaveURL(/\/login/)
  })

  test('groups API returns group names and channel counts', async ({ context }) => {
    const response = await context.request.get('/api/v1/groups')
    expect(response.ok()).toBe(true)
    const groups = await response.json()
    expect(Array.isArray(groups)).toBe(true)
    if (groups.length > 0) {
      expect(groups[0]).toHaveProperty('name')
      expect(groups[0]).toHaveProperty('channelCount')
      expect(typeof groups[0].name).toBe('string')
      expect(typeof groups[0].channelCount).toBe('number')
    }
  })

  test('channels page shows searchable group filter combobox', async ({ page }) => {
    await page.goto('/channels')
    await expect(page.getByRole('heading', { name: 'Channels', level: 1 })).toBeVisible()
    const groupFilter = page.getByRole('button', { name: 'Filter by group' })
    await expect(groupFilter).toBeVisible()
    await groupFilter.click()
    await expect(page.getByRole('listbox')).toBeVisible()
    await expect(page.getByRole('combobox')).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('channels page group filter searches and filters groups', async ({ page, context }) => {
    const groupsResponse = await context.request.get('/api/v1/groups')
    expect(groupsResponse.ok()).toBe(true)
    const groups = await groupsResponse.json()
    if (groups.length === 0) return

    await page.goto('/channels')
    await expect(page.getByRole('heading', { name: 'Channels', level: 1 })).toBeVisible()
    const groupFilter = page.getByRole('button', { name: 'Filter by group' })
    await groupFilter.click()
    const searchInput = page.getByRole('combobox')
    await searchInput.fill(groups[0].name)
    await expect(page.getByRole('option', { name: groups[0].name })).toBeVisible({ timeout: 5_000 })
    await page.getByRole('option', { name: groups[0].name }).locator('button').click()
    await expect(page.getByRole('button', { name: 'Filter by group' })).toHaveText(groups[0].name)
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('channels page applies group filter from API', async ({ page, context }) => {
    const groupsResponse = await context.request.get('/api/v1/groups')
    expect(groupsResponse.ok()).toBe(true)
    const groups = await groupsResponse.json()
    if (groups.length === 0) return

    await page.goto('/channels')
    await expect(page.getByRole('heading', { name: 'Channels', level: 1 })).toBeVisible()
    await page.getByRole('button', { name: 'Filter by group' }).click()
    await page.getByRole('option', { name: groups[0].name }).locator('button').click()
    await expect(page.getByRole('status')).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('channels page supports server-side search', async ({ page }) => {
    await page.goto('/channels')
    await expect(page.getByRole('heading', { name: 'Channels', level: 1 })).toBeVisible()
    const searchInput = page.getByLabel('Search channels')
    await expect(searchInput).toBeVisible()
    await searchInput.fill('test')
    await expect(page.getByRole('status')).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('server-side search returns filtered results from the database', async ({ page, context }) => {
    await page.goto('/channels')
    await expect(page.getByRole('heading', { name: 'Channels', level: 1 })).toBeVisible()

    const baseline = await context.request.get('/api/v1/channels?limit=50')
    expect(baseline.ok()).toBe(true)
    const baselineBody = await baseline.json()
    expect(baselineBody.total).toBeGreaterThan(50)

    await page.getByLabel('Search channels').fill('news')
    await expect(page.getByRole('status')).toContainText(/\d|Searching/, { timeout: 10_000 })
    await expect(page.getByRole('alert')).toHaveCount(0)

    const filtered = await context.request.get('/api/v1/channels?limit=50&search=news')
    expect(filtered.ok()).toBe(true)
    const filteredBody = await filtered.json()
    expect(filteredBody.total).toBeLessThan(baselineBody.total)
    expect(filteredBody.total).toBeGreaterThan(0)
  })

  test('sources page shows delete button for each source', async ({ page }) => {
    await page.goto('/sources')
    await expect(page.getByRole('heading', { name: 'Sources', level: 1 })).toBeVisible()
    const deleteButtons = page.getByRole('button', { name: /Remove .*/ })
    const count = await deleteButtons.count()
    expect(count).toBeGreaterThan(0)
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('sources page shows confirmation flow before deletion', async ({ page }) => {
    await page.goto('/sources')
    await expect(page.getByRole('heading', { name: 'Sources', level: 1 })).toBeVisible()
    const deleteButtons = page.getByRole('button', { name: /Remove .*/ })
    const count = await deleteButtons.count()
    if (count === 0) return

    await deleteButtons.first().click()
    await expect(page.getByText('Remove?')).toBeVisible()
    await expect(page.getByRole('button', { name: 'No' })).toBeVisible()
    await page.getByRole('button', { name: 'No' }).click()
    await expect(page.getByText('Remove?')).toHaveCount(0)
  })

  test('source creation and deletion cycle works end to end', async ({ page, context }) => {
    const sourceName = `E2E delete test ${Date.now()}`
    await page.goto('/sources')
    await expect(page.getByRole('heading', { name: 'Sources', level: 1 })).toBeVisible()

    await page.getByRole('button', { name: /Add source/ }).first().click()
    await page.getByLabel('Display name').fill(sourceName)
    await page.getByLabel('Endpoint').fill('https://example.invalid/test.m3u')
    await page.locator('form').getByRole('button', { name: 'Add source' }).click()
    await expect(page.getByText(sourceName, { exact: true })).toBeVisible({ timeout: 10_000 })

    const sourcesResponse = await context.request.get('/api/v1/sources')
    expect(sourcesResponse.ok()).toBe(true)
    const sources = await sourcesResponse.json()
    const created = sources.find((s: { name: string; id: string }) => s.name === sourceName)
    expect(created).toBeDefined()

    await page.getByRole('button', { name: `Remove ${sourceName}` }).click()
    await expect(page.getByText('Remove?')).toBeVisible()
    await page.getByRole('button', { name: 'Yes' }).click()
    await expect(page.getByText(/Source removed/)).toBeVisible({ timeout: 10_000 })
    await expect(page.getByText(sourceName)).toHaveCount(0)
  })

  test('channels page shows preview button and opens preview player', async ({ page, context }) => {
    await page.goto('/channels')
    await expect(page.getByRole('heading', { name: 'Channels', level: 1 })).toBeVisible()

    const channelsResponse = await context.request.get('/api/v1/channels?limit=1')
    expect(channelsResponse.ok()).toBe(true)
    const channelsBody = await channelsResponse.json()
    expect(channelsBody.items.length).toBeGreaterThan(0)
    const channelId = channelsBody.items[0].id
    const channelName = channelsBody.items[0].name

    const previewResponse = await context.request.get(`/api/v1/channels/${channelId}/preview`)
    expect(previewResponse.ok()).toBe(true)
    const previewBody = await previewResponse.json()
    expect(previewBody.streamUrl).toContain(channelId)
    expect(previewBody.contentType).toBe('video/mp2t')

    const previewButton = page.getByRole('button', { name: `Preview ${channelName}` }).first()
    await expect(previewButton).toBeVisible()
    await previewButton.click()

    await expect(page.getByText(`Preview: ${channelName}`)).toBeVisible({ timeout: 10_000 })
    await expect(page.locator('video')).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('sources API returns refresh interval and last refreshed fields', async ({ context }) => {
    const response = await context.request.get('/api/v1/sources')
    expect(response.ok()).toBe(true)
    const sources = await response.json()
    if (sources.length === 0) return
    expect(sources[0]).toHaveProperty('refreshIntervalSeconds')
    expect(sources[0]).toHaveProperty('lastRefreshedAt')
    expect(typeof sources[0].refreshIntervalSeconds).toBe('number')
  })

  test('set source refresh interval API updates the interval', async ({ context }) => {
    const listResponse = await context.request.get('/api/v1/sources')
    expect(listResponse.ok()).toBe(true)
    const sources = await listResponse.json()
    if (sources.length === 0) return

    const sourceId = sources[0].id
    const csrfCookie = await context.cookies()
    const csrfToken = csrfCookie.find((c) => c.name === 'iptv_csrf')?.value ?? ''
    const updateResponse = await context.request.patch(`/api/v1/sources/${sourceId}/refresh-interval`, {
      headers: {
        'Content-Type': 'application/json',
        'X-CSRF-Token': csrfToken,
      },
      data: { refreshIntervalSeconds: 3600 },
    })
    expect(updateResponse.status()).toBe(204)

    const verifyResponse = await context.request.get('/api/v1/sources')
    const verifySources = await verifyResponse.json()
    const verified = verifySources.find((s: { id: string }) => s.id === sourceId)
    expect(verified).toBeTruthy()
    expect(verified.refreshIntervalSeconds).toBe(3600)
  })

  test('sources page shows refresh interval column', async ({ page, context }) => {
    const sourcesResponse = await context.request.get('/api/v1/sources')
    expect(sourcesResponse.ok()).toBe(true)
    const sources = await sourcesResponse.json()
    if (sources.length === 0) {
      test.skip(true, 'No sources configured')
      return
    }

    await page.goto('/sources')
    await expect(page.getByRole('heading', { name: 'Sources', level: 1 })).toBeVisible()
    await expect(page.getByText(sources[0].name).first()).toBeVisible({ timeout: 10_000 })
    await expect(page.getByText('Refresh').first()).toBeVisible({ timeout: 10_000 })
    await expect(page.getByRole('alert')).toHaveCount(0)
  })

  test('sources API returns max connections, timezone, and enabled fields', async ({ context }) => {
    const response = await context.request.get('/api/v1/sources')
    expect(response.ok()).toBe(true)
    const sources = await response.json()
    if (sources.length === 0) return
    expect(sources[0]).toHaveProperty('maxConnections')
    expect(sources[0]).toHaveProperty('timezone')
    expect(sources[0]).toHaveProperty('enabled')
    expect(typeof sources[0].maxConnections).toBe('number')
    expect(typeof sources[0].timezone).toBe('string')
    expect(typeof sources[0].enabled).toBe('boolean')
  })

  test('update source API changes max connections and timezone', async ({ context }) => {
    const listResponse = await context.request.get('/api/v1/sources')
    expect(listResponse.ok()).toBe(true)
    const sources = await listResponse.json()
    if (sources.length === 0) return

    const sourceId = sources[0].id
    const csrfCookie = await context.cookies()
    const csrfToken = csrfCookie.find((c) => c.name === 'iptv_csrf')?.value ?? ''
    const updateResponse = await context.request.patch(`/api/v1/sources/${sourceId}`, {
      headers: {
        'Content-Type': 'application/json',
        'X-CSRF-Token': csrfToken,
      },
      data: { maxConnections: 5, timezone: 'America/New_York', enabled: true },
    })
    expect(updateResponse.status()).toBe(204)

    const verifyResponse = await context.request.get('/api/v1/sources')
    const verifySources = await verifyResponse.json()
    const verified = verifySources.find((s: { id: string }) => s.id === sourceId)
    expect(verified).toBeTruthy()
    expect(verified.maxConnections).toBe(5)
    expect(verified.timezone).toBe('America/New_York')
  })

  test('sources page shows edit button and opens edit dialog', async ({ page, context }) => {
    const sourcesResponse = await context.request.get('/api/v1/sources')
    expect(sourcesResponse.ok()).toBe(true)
    const sources = await sourcesResponse.json()
    if (sources.length === 0) {
      test.skip(true, 'No sources configured')
      return
    }

    await page.goto('/sources')
    await expect(page.getByRole('heading', { name: 'Sources', level: 1 })).toBeVisible()
    await expect(page.getByText(sources[0].name).first()).toBeVisible({ timeout: 10_000 })

    const editButton = page.getByRole('button', { name: `Edit ${sources[0].name}` }).first()
    await expect(editButton).toBeVisible({ timeout: 10_000 })
    await editButton.click()

    await expect(page.getByRole('dialog')).toBeVisible()
    await expect(page.getByText(`Edit ${sources[0].name}`).first()).toBeVisible()
    await expect(page.getByLabel('Max connections')).toBeVisible()
    await expect(page.getByLabel('Timezone')).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
  })
})
