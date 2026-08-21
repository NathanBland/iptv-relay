import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

export default defineConfig({
  resolve: {
    tsconfigPaths: true,
  },
  plugins: [react()],
  test: {
    include: ['tests/**/*.test.{ts,tsx}'],
    environment: 'jsdom',
    setupFiles: ['./tests/setup.ts'],
    maxWorkers: 4,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json-summary', 'lcov'],
      include: ['src/components/**/*.{ts,tsx}', 'src/lib/**/*.{ts,tsx}', 'src/pages/**/*.{ts,tsx}'],
      exclude: ['src/lib/api/types.ts'],
      thresholds: {
        branches: 86,
        functions: 86,
        lines: 86,
        statements: 86,
      },
    },
  },
})
