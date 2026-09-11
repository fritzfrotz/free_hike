import { defineConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { createReadStream, existsSync, statSync } from 'node:fs'
import { join, normalize } from 'node:path'
import { fileURLToPath } from 'node:url'

/**
 * Serves dev_assets/local/<file> at /local/<file> — DEV SERVER ONLY
 * (`apply: 'serve'`). The pre-compiled preview archives live outside
 * public/ on purpose: `vite build` must never emit them, so a native
 * build cannot ship a map the on-device compiler did not produce
 * (ARCHITECTURE.md P10; LOOPLOG P-FE.C3 decision D4c). A missing file
 * answers a real 404, never the SPA index.html fallback.
 */
function devLocalArchives(): Plugin {
  const root = join(fileURLToPath(new URL('./dev_assets', import.meta.url)), 'local')
  return {
    name: 'freehike-dev-local-archives',
    apply: 'serve',
    configureServer(server) {
      server.middlewares.use('/local', (req, res) => {
        const rel = decodeURIComponent((req.url ?? '/').split('?')[0])
        const file = normalize(join(root, rel))
        if (!file.startsWith(root) || !existsSync(file) || !statSync(file).isFile()) {
          res.statusCode = 404
          res.setHeader('Content-Type', 'text/plain')
          res.end('not found')
          return
        }
        res.setHeader('Content-Type', 'application/octet-stream')
        res.setHeader('Content-Length', String(statSync(file).size))
        createReadStream(file).pipe(res)
      })
    },
  }
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss(), devLocalArchives()],
})
