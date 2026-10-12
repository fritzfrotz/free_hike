// SPDX-License-Identifier: Apache-2.0
// P9.C9 — the archive header decides how far the map may zoom out and pan,
// and where it starts, so a compiled region never boots onto blank ground.
import { describe, expect, it } from 'vitest';
import { viewClampFromHeader } from './viewClamp';

// Shape of the demo region's header (Lisbon / Sintra / Arrábida bbox, z5–14).
const LISBON = {
  minZoom: 5,
  maxZoom: 14,
  minLon: -9.55,
  minLat: 38.38,
  maxLon: -8.75,
  maxLat: 38.95,
};

describe('viewClampFromHeader', () => {
  it('viewClampFromHeader: min zoom, bounds and start view from the archive header', () => {
    expect(viewClampFromHeader(LISBON)).toEqual({
      minZoom: 5,
      bounds: [
        [-9.55, 38.38],
        [-8.75, 38.95],
      ],
    });
  });

  it('viewClampFromHeader: an invalid header gives no clamp', () => {
    // Empty / zeroed header (what a zero-byte or foreign archive yields).
    expect(viewClampFromHeader({ ...LISBON, minLon: 0, minLat: 0, maxLon: 0, maxLat: 0 })).toBeNull();
    // Inverted bounds.
    expect(viewClampFromHeader({ ...LISBON, minLon: -8.75, maxLon: -9.55 })).toBeNull();
    // Out of range.
    expect(viewClampFromHeader({ ...LISBON, maxLat: 95 })).toBeNull();
    expect(viewClampFromHeader({ ...LISBON, minLon: Number.NaN })).toBeNull();
    // Zoom range inverted or negative.
    expect(viewClampFromHeader({ ...LISBON, minZoom: 15 })).toBeNull();
    expect(viewClampFromHeader({ ...LISBON, minZoom: -1 })).toBeNull();
  });
});
