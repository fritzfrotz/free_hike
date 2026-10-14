// SPDX-License-Identifier: Apache-2.0
/**
 * mapDiagnostics.ts — one log line describing the map's camera and drawing
 * surface (P9.C11).
 *
 * The RC1 GPS tap blanked the map with nothing logged. Every camera move and
 * map lifecycle event now carries this line, so a capture can tell a bad
 * camera (non-finite centre or zoom) from a lost or zero-sized canvas.
 * Never throws: a field that cannot be read prints as "?".
 */

/** The map methods this reads (a subset of maplibregl.Map). */
export interface DiagnosableMap {
  getCenter(): { lng: number; lat: number };
  getZoom(): number;
  getPitch(): number;
  getCanvas(): { width: number; height: number; clientWidth: number; clientHeight: number };
}

export interface DiagnosableContainer {
  clientWidth: number;
  clientHeight: number;
}

function read(fn: () => string): string {
  try {
    return fn();
  } catch {
    return '?';
  }
}

/** Pure apart from the reads; non-finite values print as JS prints them (NaN). */
export function mapDiagLine(map: DiagnosableMap, container: DiagnosableContainer | null): string {
  const center = read(() => {
    const c = map.getCenter();
    return `${c.lng.toFixed(5)},${c.lat.toFixed(5)}`;
  });
  const zoom = read(() => map.getZoom().toFixed(2));
  const pitch = read(() => map.getPitch().toFixed(1));
  const canvas = read(() => {
    const c = map.getCanvas();
    return `${c.width}x${c.height} css=${c.clientWidth}x${c.clientHeight}`;
  });
  const box = container ? `${container.clientWidth}x${container.clientHeight}` : 'none';
  return `center=${center} zoom=${zoom} pitch=${pitch} canvas=${canvas} container=${box}`;
}
