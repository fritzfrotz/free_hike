// SPDX-License-Identifier: Apache-2.0
/**
 * viewClamp.ts — the map's zoom/pan limits and start view, taken from the
 * bound archive's PMTiles header (P9.C9).
 *
 * The compiler writes the job's bbox as the header bounds and the lowest
 * zoom that has tiles as `minZoom` (freehike-core tiles/finalize.rs). Outside
 * those the map is blank, so MapView sets `minZoom`, clamps panning to the
 * bounds (`maxBounds`) and starts by fitting them. Note MapLibre keeps the
 * whole viewport inside `maxBounds`, so on a phone the effective minimum
 * zoom is where the bbox fills the screen, usually above `minZoom`.
 */

/** The header fields this needs (a subset of pmtiles' Header). */
export interface ArchiveHeaderView {
  minZoom: number;
  maxZoom: number;
  minLon: number;
  minLat: number;
  maxLon: number;
  maxLat: number;
}

export interface ViewClamp {
  minZoom: number;
  /** [[west, south], [east, north]] — maxBounds and the start fit. */
  bounds: [[number, number], [number, number]];
}

/** The clamp for a header, or null when the header is not usable. Pure. */
export function viewClampFromHeader(h: ArchiveHeaderView): ViewClamp | null {
  const values = [h.minZoom, h.maxZoom, h.minLon, h.minLat, h.maxLon, h.maxLat];
  if (!values.every(Number.isFinite)) return null;
  if (h.minZoom < 0 || h.minZoom > h.maxZoom) return null;
  if (h.minLon < -180 || h.maxLon > 180 || h.minLat < -90 || h.maxLat > 90) return null;
  if (h.minLon >= h.maxLon || h.minLat >= h.maxLat) return null;
  return {
    minZoom: h.minZoom,
    bounds: [
      [h.minLon, h.minLat],
      [h.maxLon, h.maxLat],
    ],
  };
}
