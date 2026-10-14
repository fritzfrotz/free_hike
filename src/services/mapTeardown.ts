// SPDX-License-Identifier: Apache-2.0
/**
 * mapTeardown.ts — remove a MapLibre map exactly once (P9.C11).
 *
 * MapView's boot cleanup holds the same map under two names (the effect's
 * local `map` and `mapRef.current`). MapLibre 5.24's remove() deletes its
 * handler set on the first call, so a second call throws ("reading
 * 'destroy'"), and a throw inside a React cleanup unmounts the whole page —
 * the white screen at RC1's first-region remount.
 */

/** The one map method this needs. */
export interface RemovableMap {
  remove(): void;
}

const removed = new WeakSet<RemovableMap>();

/**
 * Removes `map` unless it was already removed through here. The instance is
 * marked BEFORE remove() runs, so a remove() that throws is never retried;
 * the throw is logged and swallowed — the caller's cleanup must still reach
 * the worker close. Returns true when remove() was called.
 */
export function removeMapOnce(
  map: RemovableMap | null | undefined,
  log: (message: string) => void = (m) => console.error(m),
): boolean {
  if (!map || removed.has(map)) return false;
  removed.add(map);
  try {
    map.remove();
  } catch (err) {
    log(`[MapView] map.remove() threw: ${err instanceof Error ? err.message : String(err)}`);
  }
  return true;
}
