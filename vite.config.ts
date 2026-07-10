import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath, URL } from 'node:url'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      // @jansoft/mbujkanji-valhalla-wasm hardcodes an import to '/valhalla.js'.
      // Remap it to our local copy at src/valhalla.js so Vite can bundle it
      // correctly in the worker context without a 404.
      '/valhalla.js': fileURLToPath(new URL('./src/valhalla.js', import.meta.url)),
    },
  },
})

