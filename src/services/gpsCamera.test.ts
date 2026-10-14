// SPDX-License-Identifier: Apache-2.0
// P9.C11 addendum — the GPS camera. RC1 (read live on the phone): the dot
// was right, the camera sat pinned at the region bounds' north-west corner.
// flyTo's arc zooms out mid-flight, below the clamp's minimum zoom (14, from
// the archive header), and tracking restarted it on every fix. The camera
// now only eases: a pan while tracking, a pan + zoom on the tap.
import { describe, expect, it, vi } from 'vitest';
import { centerOnFix, followFix } from './gpsCamera';

const LISBON = { lng: -9.1533616, lat: 38.7067522 };
const CLAMP_MIN_ZOOM = 14;

function fakeMap(startZoom: number) {
  return {
    easeTo: vi.fn(),
    flyTo: vi.fn(),
    getZoom: () => startZoom,
  };
}

describe('followFix', () => {
  it('pans with easeTo and never sets a zoom', () => {
    const map = fakeMap(16);
    followFix(map, LISBON);
    expect(map.flyTo).not.toHaveBeenCalled();
    expect(map.easeTo).toHaveBeenCalledTimes(1);
    const opts = map.easeTo.mock.calls[0][0];
    expect(opts.center).toEqual([LISBON.lng, LISBON.lat]);
    expect(opts).not.toHaveProperty('zoom');
  });
});

describe('centerOnFix', () => {
  it('eases to the target zoom and never calls flyTo', () => {
    const map = fakeMap(14);
    centerOnFix(map, LISBON, 15);
    expect(map.flyTo).not.toHaveBeenCalled();
    expect(map.easeTo).toHaveBeenCalledTimes(1);
    const opts = map.easeTo.mock.calls[0][0];
    expect(opts.center).toEqual([LISBON.lng, LISBON.lat]);
    expect(opts.zoom).toBe(15);
  });

  it('an ease from any start zoom at or above the clamp stays at or above it', () => {
    // An ease interpolates zoom between its two endpoints (no arc), so it
    // stays at or above the clamp when both endpoints are.
    for (const start of [14, 15, 17]) {
      const map = fakeMap(start);
      centerOnFix(map, LISBON, 15);
      const opts = map.easeTo.mock.calls[0][0];
      expect(Math.min(start, opts.zoom)).toBeGreaterThanOrEqual(CLAMP_MIN_ZOOM);
      expect(opts).not.toHaveProperty('curve');
      expect(opts).not.toHaveProperty('minZoom');
    }
  });
});
