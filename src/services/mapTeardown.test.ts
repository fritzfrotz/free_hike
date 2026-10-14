// SPDX-License-Identifier: Apache-2.0
// P9.C11 — MapView's cleanup holds the same map under two names (`map` and
// `mapRef.current`) and removed it through both. MapLibre 5.24's remove()
// deletes its handler set on the first call, so the second threw and took
// the page down at the first-region remount (RC1 step 6).
import { describe, expect, it, vi } from 'vitest';
import { removeMapOnce } from './mapTeardown';

/** Mirrors the part of MapLibre 5.24's remove() that broke: the second call
 *  reads a property the first call deleted. */
function fakeMap() {
  const handlers: { destroy: () => void } | undefined = { destroy: () => {} };
  const map = {
    handlers,
    calls: 0,
    remove() {
      map.calls += 1;
      map.handlers!.destroy();
      delete (map as { handlers?: unknown }).handlers;
    },
  };
  return map;
}

describe('removeMapOnce', () => {
  it("the cleanup's two references to one map remove it exactly once", () => {
    const map = fakeMap();
    const mapRef = { current: map };
    expect(() => {
      removeMapOnce(map);
      removeMapOnce(mapRef.current);
    }).not.toThrow();
    expect(map.calls).toBe(1);
  });

  it('a second call on the same instance is a no-op', () => {
    const map = fakeMap();
    expect(removeMapOnce(map)).toBe(true);
    expect(removeMapOnce(map)).toBe(false);
    expect(map.calls).toBe(1);
  });

  it('null and undefined are harmless', () => {
    expect(removeMapOnce(null)).toBe(false);
    expect(removeMapOnce(undefined)).toBe(false);
  });

  it('a throwing remove() is logged, not rethrown', () => {
    const log = vi.fn();
    const map = { remove: vi.fn(() => { throw new Error('gl gone'); }) };
    expect(() => removeMapOnce(map, log)).not.toThrow();
    expect(log).toHaveBeenCalledTimes(1);
    expect(String(log.mock.calls[0][0])).toContain('gl gone');
    // Marked before the call: a throwing instance is never retried.
    expect(removeMapOnce(map, log)).toBe(false);
    expect(map.remove).toHaveBeenCalledTimes(1);
  });
});
