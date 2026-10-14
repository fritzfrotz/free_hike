// SPDX-License-Identifier: Apache-2.0
// P9.C11 — the GPS-tap blank map (RC1) logged nothing. Every camera move and
// map lifecycle event now logs one line with the camera and the canvas and
// container sizes, so RC2's capture can tell a bad camera from a lost or
// zero-sized canvas.
import { describe, expect, it } from 'vitest';
import { mapDiagLine } from './mapDiagnostics';

function fakeMap(camera: { lng: number; lat: number; zoom: number; pitch: number }) {
  return {
    getCenter: () => ({ lng: camera.lng, lat: camera.lat }),
    getZoom: () => camera.zoom,
    getPitch: () => camera.pitch,
    getCanvas: () => ({ width: 2160, height: 1800, clientWidth: 720, clientHeight: 600 }),
  };
}

describe('mapDiagLine', () => {
  it('camera, canvas and container size in one line', () => {
    const line = mapDiagLine(
      fakeMap({ lng: -9.1393, lat: 38.7223, zoom: 15, pitch: 45 }),
      { clientWidth: 720, clientHeight: 600 },
    );
    expect(line).toBe(
      'center=-9.13930,38.72230 zoom=15.00 pitch=45.0 canvas=2160x1800 css=720x600 container=720x600',
    );
  });

  it('a non-finite camera or a missing container still yields a line', () => {
    const line = mapDiagLine(fakeMap({ lng: NaN, lat: NaN, zoom: NaN, pitch: 45 }), null);
    expect(line).toContain('center=NaN,NaN');
    expect(line).toContain('zoom=NaN');
    expect(line).toContain('container=none');

    const broken = {
      getCenter: () => { throw new Error('removed'); },
      getZoom: () => 3,
      getPitch: () => 0,
      getCanvas: () => { throw new Error('removed'); },
    };
    const brokenLine = mapDiagLine(broken, null);
    expect(brokenLine).toContain('center=?');
    expect(brokenLine).toContain('canvas=?');
  });
});
