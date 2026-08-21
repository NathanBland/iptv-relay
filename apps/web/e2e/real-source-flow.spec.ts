import { expect, test, type BrowserContext, type Page, type TestInfo } from '@playwright/test'
import { spawnSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'
import { resolve } from 'node:path'

interface BrowserEvents {
  apiFailures: string[]
  consoleErrors: string[]
  consoleWarnings: string[]
  pageErrors: string[]
  requestFailures: string[]
  sourceStates: Array<{ name: string; state: string; time: string }>
}

interface CreatedSource {
  id: string
  kind: 'M3U' | 'XMLTV'
  name: string
}

const workspaceRoot = resolve(fileURLToPath(new URL('.', import.meta.url)), '../../..')
const adminUsername = process.env.IPTV_ADMIN_USERNAME ?? 'operator'
const adminPassword = requiredEnvironment('IPTV_ADMIN_PASSWORD')
const m3uUrl = requiredEnvironment('IPTV_TEST_M3U_URL')
const xmltvUrl = requiredEnvironment('IPTV_TEST_XMLTV_URL')
const secrets = [adminPassword, m3uUrl, xmltvUrl]

function requiredEnvironment(name: string) {
  const value = process.env[name]?.trim()
  if (!value) throw new Error(`${name} must be set in the root .env file.`)
  return value
}

function safeUrl(value: string) {
  try {
    const url = new URL(value)
    return `${url.origin}${url.pathname}`
  } catch {
    return '[INVALID URL]'
  }
}

function redactText(value: string) {
  return secrets.reduce((text, secret) => text.replaceAll(secret, '[REDACTED]'), value)
}

function redact(value: unknown, key = ''): unknown {
  if (/password|token|secret|endpoint|url/i.test(key)) return '[REDACTED]'
  if (typeof value === 'string') return redactText(value)
  if (Array.isArray(value)) return value.map((item) => redact(item))
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([childKey, child]) => [childKey, redact(child, childKey)]))
  }
  return value
}

function observePage(page: Page, events: BrowserEvents) {
  page.on('console', (message) => {
    const rendered = redactText(message.text())
    if (message.type() === 'error') events.consoleErrors.push(rendered)
    if (message.type() === 'warning') events.consoleWarnings.push(rendered)
  })
  page.on('pageerror', (error) => events.pageErrors.push(redactText(error.message)))
  page.on('requestfailed', (request) => {
    const failure = request.failure()?.errorText ?? 'unknown failure'
    if (!failure.includes('ERR_ABORTED')) events.requestFailures.push(`${request.method()} ${safeUrl(request.url())}: ${failure}`)
  })
  page.on('response', (response) => {
    if (response.status() >= 400 && new URL(response.url()).pathname.startsWith('/api/')) {
      events.apiFailures.push(`${response.request().method()} ${safeUrl(response.url())}: ${response.status()}`)
    }
  })
}

async function addSource(page: Page, source: { endpoint: string; kind: 'M3U' | 'XMLTV'; name: string }) {
  await page.getByRole('button', { name: 'Add source' }).click()
  await page.getByLabel('Display name').fill(source.name)
  await page.getByLabel('Source type').selectOption({ label: source.kind })
  await page.getByLabel('Endpoint').fill(source.endpoint)
  const responsePromise = page.waitForResponse((response) => {
    return new URL(response.url()).pathname === '/api/v1/sources' && response.request().method() === 'POST'
  })
  await page.getByRole('button', { name: 'Add source', exact: true }).last().click()
  const response = await responsePromise
  const body = await response.text()
  expect(response.ok(), redactText(body)).toBe(true)
  const created = JSON.parse(body) as CreatedSource
  expect(created.id).toMatch(/^[0-9a-f-]{36}$/)
  await expect(page.getByRole('status')).toContainText(`${source.name} was added`)
  await expect(page.getByRole('row').filter({ hasText: source.name })).toBeVisible()
  return created
}

async function waitForHealthySource(page: Page, events: BrowserEvents, source: CreatedSource) {
  const row = page.getByRole('row').filter({ hasText: source.name })
  await expect.poll(async () => {
    await page.reload()
    await expect(row).toBeVisible()
    const state = (await row.locator('td').nth(2).innerText()).trim()
    events.sourceStates.push({ name: source.name, state, time: new Date().toISOString() })
    return state
  }, {
    message: `${source.kind} source did not become healthy.`,
    timeout: 12 * 60 * 1_000,
    intervals: [1_000, 2_000, 5_000, 10_000],
  }).toBe('healthy')
  const countText = await row.locator('td').nth(3).innerText()
  const count = Number(countText.replaceAll(/\D/g, ''))
  expect(count).toBeGreaterThan(0)
  const rowText = await row.innerText()
  expect(rowText).not.toContain(source.kind === 'M3U' ? m3uUrl : xmltvUrl)
  return count
}

async function apiDiagnostics(context: BrowserContext, page: Page) {
  const origin = new URL(page.url()).origin
  const diagnostics: Record<string, unknown> = {}
  for (const path of [
    '/health/ready',
    '/api/v1/auth/status',
    '/api/v1/system',
    '/api/v1/sources',
    '/api/v1/jobs',
    '/api/v1/channels',
    '/api/v1/programmes',
    '/api/v1/events',
    '/api/v1/sessions',
  ]) {
    try {
      const response = await context.request.get(`${origin}${path}`)
      const text = await response.text()
      let body: unknown = text
      try {
        body = JSON.parse(text)
      } catch {
        body = text.slice(0, 2_000)
      }
      diagnostics[path] = { body, status: response.status() }
    } catch (error) {
      diagnostics[path] = { error: error instanceof Error ? error.message : String(error) }
    }
  }
  return redact(diagnostics)
}

function developmentFeedback(diagnostics: unknown) {
  const records = diagnostics as Record<string, { body?: unknown; status?: number }>
  const system = records['/api/v1/system']?.body as Record<string, unknown> | undefined
  const channels = records['/api/v1/channels']?.body as { items?: unknown[]; total?: number } | undefined
  const programmes = records['/api/v1/programmes']?.body as { items?: unknown[]; total?: number } | undefined
  const sessions = records['/api/v1/sessions']?.body
  const signals = {
    canonicalChannels: typeof channels?.total === 'number' ? channels.total : Array.isArray(channels) ? channels.length : null,
    guideCoverage: typeof system?.guideCoverage === 'number' ? system.guideCoverage : null,
    healthyStreams: typeof system?.healthyStreams === 'number' ? system.healthyStreams : null,
    programmes: typeof programmes?.total === 'number' ? programmes.total : Array.isArray(programmes) ? programmes.length : null,
    sessions: Array.isArray(sessions) ? sessions.length : null,
  }
  const issues: string[] = []
  if (signals.canonicalChannels === 0) issues.push('Activated source snapshots do not create canonical channels.')
  if (signals.healthyStreams === 0) issues.push('The system API does not report imported provider streams.')
  if (signals.programmes === 0) issues.push('The control API does not expose imported programmes.')
  if (signals.guideCoverage === 0) issues.push('The system API reports zero guide coverage after XMLTV activation.')
  return { issues, signals }
}

async function attachJson(testInfo: TestInfo, name: string, value: unknown) {
  await testInfo.attach(name, {
    body: Buffer.from(JSON.stringify(redact(value), null, 2)),
    contentType: 'application/json',
  })
}

async function attachFailureArtifacts(page: Page, context: BrowserContext, testInfo: TestInfo, events: BrowserEvents, error: unknown) {
  const endpoint = page.getByLabel('Endpoint')
  if (await endpoint.isVisible().catch(() => false)) await endpoint.fill('').catch(() => undefined)
  const alerts = await page.getByRole('alert').allInnerTexts().catch(() => [])
  const mainText = await page.locator('main').innerText().catch(() => '')
  await attachJson(testInfo, 'failure-details.json', {
    alerts,
    error: error instanceof Error ? error.stack ?? error.message : String(error),
    events,
    mainText: mainText.slice(0, 10_000),
    title: await page.title().catch(() => ''),
    url: safeUrl(page.url()),
  })
  await attachJson(testInfo, 'api-diagnostics.json', await apiDiagnostics(context, page))
  const screenshot = await page.screenshot({ fullPage: true }).catch(() => undefined)
  if (screenshot) await testInfo.attach('failure-page.png', { body: screenshot, contentType: 'image/png' })
}

function cleanupSources(sources: CreatedSource[]) {
  if (sources.length === 0 || process.env.IPTV_E2E_KEEP_SOURCES === 'true') return { status: 0, stderr: '', stdout: 'Cleanup skipped.' }
  const ids = sources.map(({ id }) => id).filter((id) => /^[0-9a-f-]{36}$/.test(id))
  const names = sources.map(({ name }) => name).filter((name) => /^[A-Za-z0-9 -]+$/.test(name))
  const idList = ids.map((id) => `'${id}'`).join(',') || "'00000000-0000-0000-0000-000000000000'"
  const nameList = names.map((name) => `'${name}'`).join(',') || "''"
  const sql = `BEGIN; DELETE FROM jobs WHERE payload->>'sourceId' IN (${idList}); DELETE FROM audit_events WHERE resource_type = 'source' AND resource_id::text IN (${idList}); DELETE FROM provider_accounts WHERE id::text IN (${idList}) OR name IN (${nameList}); DELETE FROM epg_sources WHERE id::text IN (${idList}) OR name IN (${nameList}); COMMIT;`
  const result = spawnSync('docker-compose', ['exec', '-T', 'postgres', 'psql', '-U', 'iptv', '-d', 'iptv', '-v', 'ON_ERROR_STOP=1', '-c', sql], {
    cwd: workspaceRoot,
    encoding: 'utf8',
  })
  return { status: result.status, stderr: redactText(result.stderr), stdout: redactText(result.stdout) }
}

async function expectManagementPage(page: Page, path: string, heading: string) {
  await page.goto(path)
  await expect(page.getByRole('heading', { name: heading, level: 1 })).toBeVisible()
  await expect(page.getByRole('alert')).toHaveCount(0)
}

test('operator can add and activate real M3U and XMLTV sources', async ({ context, page }, testInfo) => {
  test.skip(process.env.IPTV_E2E_REAL_SOURCES !== 'true', 'Set IPTV_E2E_REAL_SOURCES=true to run this long real-source test.')
  const runId = `${Date.now().toString(36)}-${randomUUID().slice(0, 8)}`
  const requestedSources = [
    { endpoint: m3uUrl, kind: 'M3U' as const, name: `E2E M3U ${runId}` },
    { endpoint: xmltvUrl, kind: 'XMLTV' as const, name: `E2E XMLTV ${runId}` },
  ]
  const events: BrowserEvents = { apiFailures: [], consoleErrors: [], consoleWarnings: [], pageErrors: [], requestFailures: [], sourceStates: [] }
  const createdSources: CreatedSource[] = []
  let traceStarted = false
  let cleaned = false
  let failed = false
  observePage(page, events)

  try {
    const readiness = await context.request.get('/health/ready')
    expect(readiness.ok()).toBe(true)
    await page.goto('/login?next=%2Fsources')
    await page.getByLabel('Username').fill(adminUsername)
    await page.getByLabel('Password').fill(adminPassword)
    await page.getByRole('button', { name: 'Sign in' }).click()
    await expect(page).toHaveURL(/\/sources$/)
    await expect(page.getByRole('heading', { name: 'Sources', level: 1 })).toBeVisible()

    for (const source of requestedSources) createdSources.push(await addSource(page, source))

    await context.tracing.start({ screenshots: true, snapshots: true, sources: true })
    traceStarted = true
    const counts: Record<string, number> = {}
    for (const source of createdSources) counts[source.kind] = await waitForHealthySource(page, events, source)

    for (const [path, heading] of [
      ['/', 'Overview'],
      ['/channels', 'Channels'],
      ['/epg', 'EPG'],
      ['/events', 'Events'],
      ['/sessions', 'Sessions'],
      ['/jellyfin', 'Jellyfin setup'],
      ['/sources', 'Sources'],
    ] as const) await expectManagementPage(page, path, heading)

    expect(events.consoleErrors).toEqual([])
    expect(events.pageErrors).toEqual([])
    expect(events.requestFailures).toEqual([])
    expect(events.apiFailures).toEqual([])
    const diagnostics = await apiDiagnostics(context, page)
    await attachJson(testInfo, 'flow-summary.json', { counts, events, sources: createdSources })
    await attachJson(testInfo, 'api-diagnostics.json', diagnostics)
    await attachJson(testInfo, 'development-feedback.json', developmentFeedback(diagnostics))

    const cleanup = cleanupSources(createdSources)
    cleaned = cleanup.status === 0
    await testInfo.attach('cleanup.txt', { body: Buffer.from(`${cleanup.stdout}\n${cleanup.stderr}`), contentType: 'text/plain' })
    expect(cleanup.status, cleanup.stderr).toBe(0)
    await page.reload()
    for (const source of createdSources) await expect(page.getByRole('row').filter({ hasText: source.name })).toHaveCount(0)

    await page.getByRole('button', { name: 'Sign out' }).click()
    await expect(page).toHaveURL(/\/login(?:\?|$)/)
    await expect(page.getByRole('heading', { name: 'Sign in', level: 1 })).toBeVisible()
  } catch (error) {
    failed = true
    await attachFailureArtifacts(page, context, testInfo, events, error)
    throw error
  } finally {
    if (traceStarted) {
      const tracePath = testInfo.outputPath('post-source-trace.zip')
      await context.tracing.stop({ path: tracePath }).catch(() => undefined)
      if (failed) await testInfo.attach('post-source-trace.zip', { path: tracePath, contentType: 'application/zip' }).catch(() => undefined)
    }
    if (!cleaned) {
      const cleanup = cleanupSources(createdSources)
      await testInfo.attach('cleanup-final.txt', { body: Buffer.from(`${cleanup.stdout}\n${cleanup.stderr}`), contentType: 'text/plain' }).catch(() => undefined)
    }
  }
})
