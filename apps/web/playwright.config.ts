import { defineConfig, devices } from '@playwright/test'
import { existsSync } from 'node:fs'
import { loadEnvFile } from 'node:process'
import { fileURLToPath } from 'node:url'
import { resolve } from 'node:path'

const webRoot = fileURLToPath(new URL('.', import.meta.url))
const workspaceRoot = resolve(webRoot, '../..')
const environmentFile = resolve(workspaceRoot, '.env')

if (!existsSync(environmentFile)) throw new Error('The root .env file is required for the real-source browser test.')
loadEnvFile(environmentFile)

const baseURL = (process.env.IPTV_E2E_BASE_URL ?? process.env.IPTV_PUBLIC_BASE_URL ?? 'http://127.0.0.1:8080').replace(/\/$/, '')

export default defineConfig({
  testDir: './e2e',
  outputDir: './test-results',
  timeout: 20 * 60 * 1_000,
  expect: { timeout: 20_000 },
  fullyParallel: true,
  workers: process.env.CI ? 1 : 3,
  retries: 0,
  forbidOnly: Boolean(process.env.CI),
  reporter: [
    ['list'],
    ['html', { outputFolder: 'playwright-report', open: 'never' }],
    ['json', { outputFile: 'test-results/e2e-results.json' }],
  ],
  use: {
    baseURL,
    actionTimeout: 20_000,
    navigationTimeout: 30_000,
    screenshot: 'off',
    trace: 'off',
    video: 'off',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
})
