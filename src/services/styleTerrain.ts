// SPDX-License-Identifier: Apache-2.0
/**
 * styleTerrain.ts — the no-terrain map path (P-SOV.C3b, DL-004, S4 (c′)).
 *
 * Terrain is not compiled in v0.1, so a native build boots with no terrain
 * archive at all. The style JSON still declares one (the dev preview keeps
 * terrain via dev_assets), and MapLibre requests a raster-dem source's
 * TileJSON at style load — before 'load' fires — so removing the source
 * afterwards is too late (the pmtiles registry's fail-loud miss guard fires
 * first). MapView therefore fetches the style itself and, when no terrain
 * archive exists, hands MapLibre the stripped object at construction: one
 * style file, no fork, no runtime style swap (P8a).
 */
import type { StyleSpecification } from 'maplibre-gl';

/**
 * Drops every `raster-dem` source, the `terrain` block, and every layer
 * drawn from a dropped source (today: `dynamic-hillshading`). Returns a new
 * object; the input is never mutated.
 */
export function stripTerrain(style: StyleSpecification): StyleSpecification {
  const dropped = new Set(
    Object.entries(style.sources)
      .filter(([, source]) => source.type === 'raster-dem')
      .map(([id]) => id),
  );
  const out: StyleSpecification = {
    ...style,
    sources: Object.fromEntries(
      Object.entries(style.sources).filter(([id]) => !dropped.has(id)),
    ),
    layers: style.layers.filter(
      (layer) => !('source' in layer && typeof layer.source === 'string' && dropped.has(layer.source)),
    ),
  };
  delete out.terrain;
  return out;
}

/** What the mapData worker reports for MAP_INIT (see MapInitSuccessPayload). */
export interface MapInitReport {
  /** Required files that could not be provisioned — user-facing. */
  provisionFailures: string[];
  /** Optional files that are simply absent — not an error. */
  optionalMissing: string[];
}

export interface MapInitOutcome {
  /** True when the terrain archive holds bytes and may be wired up. */
  hasTerrain: boolean;
  /** Failures worth a "map data unavailable" banner. */
  userFacingFailures: string[];
}

/**
 * A missing terrain archive is the normal v0.1 state on device, so it turns
 * terrain off instead of raising the banner. The terrain file is filtered
 * out of `provisionFailures` too, so a worker that reports it there (not as
 * optional) still never surfaces it to the user.
 */
export function mapInitOutcome(report: MapInitReport, terrainFile: string): MapInitOutcome {
  const terrainMissing =
    report.optionalMissing.includes(terrainFile) || report.provisionFailures.includes(terrainFile);
  return {
    hasTerrain: !terrainMissing,
    userFacingFailures: report.provisionFailures.filter((f) => f !== terrainFile),
  };
}
