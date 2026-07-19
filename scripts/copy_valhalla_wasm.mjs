// SPDX-License-Identifier: Apache-2.0
//
// Copies valhalla.wasm out of the npm package into public/ so Vite serves it
// at /valhalla.wasm (routing.worker.ts loads it from there at runtime).
// The binary is NOT committed — it is installed via npm and refreshed here on
// every `npm run dev` / `npm run build` (predev/prebuild hooks).

import { copyFile, mkdir } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const src = join(
  root,
  'node_modules/@jansoft/mbujkanji-valhalla-wasm/dist/valhalla.wasm',
);
const dest = join(root, 'public/valhalla.wasm');

await mkdir(dirname(dest), { recursive: true });
await copyFile(src, dest);
console.log(`[copy_valhalla_wasm] ${src} -> ${dest}`);
