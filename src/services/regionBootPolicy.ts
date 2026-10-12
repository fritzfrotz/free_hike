// SPDX-License-Identifier: Apache-2.0
/**
 * regionBootPolicy.ts — the cold-boot validate-before-bind decision
 * (P9.C2 rule), extracted MECHANICALLY from MapView's 'load' handler in
 * P-FE.C1 so it is importable and testable pure. Behavior-identical: the
 * OPFS probe and the MapLibre `loadOfflineRegion` call stay in MapView;
 * this module only computes (a) which files need probing and (b) what the
 * probe results mean.
 *
 * The rule: a persisted region binds only if every non-default OPFS file
 * it references still holds bytes. A missing or zero-byte file drops the
 * persisted binding entirely (`clear`) — binding anyway would fabricate
 * an empty archive, because the worker's handle open uses `{create:true}`.
 */
import type { OfflineRegion } from '../store/mapStore';

export interface RegionBootDefaults {
  basemapFile: string;
  terrainFile: string;
}

/**
 * Files that need an OPFS existence probe: only those differing from the
 * booted defaults — the defaults trivially exist (the worker just opened
 * them), and probing an already-locked file is pointless. A null terrain
 * (P-SOV.C3b: compiled regions bring none) is never probed — the region
 * binds on its basemap alone and keeps whatever terrain the boot found.
 */
export function regionFilesToVerify(
  persisted: OfflineRegion,
  defaults: RegionBootDefaults,
): string[] {
  const { terrainFile } = persisted;
  return [
    ...(persisted.basemapFile !== defaults.basemapFile ? [persisted.basemapFile] : []),
    ...(terrainFile !== null && terrainFile !== defaults.terrainFile ? [terrainFile] : []),
  ];
}

export type RegionBootDecision =
  | { action: 'noop' }
  | { action: 'clear'; missing: string[] }
  | { action: 'bind' };

/**
 * Pure decision over the probe results (`exists[i]` answers "does
 * `toVerify[i]` hold at least one byte in OPFS?"):
 *   - any probe false  → `clear` (drop the persisted binding; stay on the
 *     defaults the map just booted with), reporting which files are gone
 *   - nothing to probe → `noop` (the persisted region IS the default binding)
 *   - all probes true  → `bind` (replay the region)
 */
export function decideRegionBoot(
  toVerify: string[],
  exists: boolean[],
): RegionBootDecision {
  const missing = toVerify.filter((_, i) => !exists[i]);
  if (missing.length > 0) return { action: 'clear', missing };
  if (toVerify.length === 0) return { action: 'noop' };
  return { action: 'bind' };
}

/** The two preview archives the Vite dev server serves (dev_assets/local). */
export interface BootFiles {
  basemapFile: string;
  terrainFile: string;
}

export interface BootPlan {
  /** Archive the map boots on; null = no map yet (native, nothing compiled). */
  basemap: string | null;
  /** Optional terrain archive to try; null = none. */
  terrain: string | null;
  /** The saved region, when it was dropped because a file is gone. */
  dropped: OfflineRegion | null;
}

/**
 * P9.C9 — picks the boot files BEFORE the mapData worker opens anything.
 * The worker opens with `{ create: true }`, so naming a file there
 * fabricates it: a native boot must therefore never name a dev preview
 * archive. Order: the saved region if every file it references holds bytes;
 * else, on the Vite dev server only, the preview archives; else nothing.
 *
 * On the dev server the preview archives are exempt from the probe (the
 * worker provisions them on open); on native every file is probed, so a
 * region saved by an older build that points at a preview archive drops.
 */
export async function planBoot(
  persisted: OfflineRegion | null,
  opts: { isDev: boolean; devDefaults: BootFiles; hasBytes: (filename: string) => Promise<boolean> },
): Promise<BootPlan> {
  const { isDev, devDefaults, hasBytes } = opts;
  let dropped: OfflineRegion | null = null;

  if (persisted) {
    const exempt = isDev ? devDefaults : { basemapFile: '', terrainFile: '' };
    const toVerify = regionFilesToVerify(persisted, exempt);
    const exists = await Promise.all(toVerify.map(hasBytes));
    if (decideRegionBoot(toVerify, exists).action !== 'clear') {
      return {
        basemap: persisted.basemapFile,
        terrain: persisted.terrainFile ?? (isDev ? devDefaults.terrainFile : null),
        dropped: null,
      };
    }
    dropped = persisted;
  }

  if (isDev) {
    return { basemap: devDefaults.basemapFile, terrain: devDefaults.terrainFile, dropped };
  }
  return { basemap: null, terrain: null, dropped };
}
