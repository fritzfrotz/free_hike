# dev_assets — preview-only map archives

`dev_assets/local/*.pmtiles` are pre-compiled archives used ONLY by the Vite
dev server (see the `freehike-dev-local-archives` plugin in `vite.config.ts`),
which serves them at `/local/<file>` so the web preview has a map to render.

They are deliberately outside `public/`: `vite build` never emits them, so a
Capacitor `dist` (and therefore the APK / iOS bundle) cannot contain them. A
native build boots to an empty OPFS and the "Map data unavailable" banner
until the on-device compiler produces a real archive (ARCHITECTURE.md P10;
LOOPLOG P-FE.C3, decision D4 option c).

The `.pmtiles` files themselves are gitignored — copy them here from
`offline_sandbox/output/` after running `scripts/compile_sandbox_data.sh`.
