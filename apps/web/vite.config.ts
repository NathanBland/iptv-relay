import tailwindcss from '@tailwindcss/vite'
import { tanstackStart } from '@tanstack/react-start/plugin/vite'
import react from '@vitejs/plugin-react'
import { nitro } from 'nitro/vite'
import { defineConfig } from 'vite'

export default defineConfig({
  resolve: {
    tsconfigPaths: true,
  },
  server: {
    host: '0.0.0.0',
    port: 3000,
    proxy: {
      '/api': 'http://127.0.0.1:8081',
      '/auth': 'http://127.0.0.1:8081',
      '/health': 'http://127.0.0.1:8081',
      '/metrics': 'http://127.0.0.1:8081',
      '/out': 'http://127.0.0.1:8081',
    },
  },
  plugins: [tailwindcss(), tanstackStart(), react(), nitro()],
})
