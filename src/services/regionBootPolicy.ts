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
 * them), and probing an already-locked file is pointless.
 */
export function regionFilesToVerify(
  persisted: OfflineRegion,
  defaults: RegionBootDefaults,
): string[] {
  return [
    ...(persisted.basemapFile !== defaults.basemapFile ? [persisted.basemapFile] : []),
    ...(persisted.terrainFile !== defaults.terrainFile ? [persisted.terrainFile] : []),
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
