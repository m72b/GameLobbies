import { resolve } from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// The renderer is a plain Vite app; Tauri serves out/renderer in builds and
// loads the dev server (port 5173) during `npm run dev`.
export default defineConfig({
  root: resolve('src/renderer'),
  plugins: [react(), tailwindcss()],
  resolve: { alias: { '@': resolve('src/renderer'), '@shared': resolve('src/shared') } },
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    outDir: resolve('out/renderer'),
    emptyOutDir: true,
    target: 'chrome120',
    rollupOptions: { input: { main: resolve('src/renderer/index.html'), settings: resolve('src/renderer/settings.html') } }
  }
})
