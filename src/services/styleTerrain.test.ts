// SPDX-License-Identifier: Apache-2.0
/// <reference types="node" />
// P-SOV.C3b (DL-004, S4 (c′)) — the no-terrain style transform and the
// MAP_INIT outcome decision, tested pure against the REAL shipped style.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { StyleSpecification } from 'maplibre-gl';
import { mapInitOutcome, stripTerrain } from './styleTerrain';

const TERRAIN = 'alps_terrain.pmtiles';

function realStyle(): StyleSpecification {
  const path = join(process.cwd(), 'public/styles/high_contrast_outdoor_style.json');
  return JSON.parse(readFileSync(path, 'utf8')) as StyleSpecification;
}

describe('stripTerrain', () => {
  it('strips_terrain_source_block_and_hillshade', () => {
    const before = realStyle();
    expect(before.layers).toHaveLength(22);
    const after = stripTerrain(before);
    expect(after.layers).toHaveLength(21);
    expect(after.sources['terrain-local']).toBeUndefined();
    expect(after.sources['basemap-local']).toBeDefined();
    expect(after.terrain).toBeUndefined();
    expect(after.layers.some((l) => l.id === 'dynamic-hillshading')).toBe(false);
  });

  it('no_layer_references_a_missing_source', () => {
    const after = stripTerrain(realStyle());
    for (const layer of after.layers) {
      if ('source' in layer && typeof layer.source === 'string') {
        expect(after.sources[layer.source], `${layer.id} → ${layer.source}`).toBeDefined();
      }
    }
  });

  it('does_not_mutate_input', () => {
    const before = realStyle();
    const snapshot = JSON.stringify(before);
    stripTerrain(before);
    expect(JSON.stringify(before)).toBe(snapshot);
  });

  it('idempotent', () => {
    const once = stripTerrain(realStyle());
    expect(stripTerrain(once)).toEqual(once);
  });
});

describe('mapInitOutcome', () => {
  it('missing_terrain_is_not_a_user_facing_failure', () => {
    expect(
      mapInitOutcome({ provisionFailures: [], optionalMissing: [TERRAIN] }, TERRAIN),
    ).toEqual({ hasTerrain: false, userFacingFailures: [] });
  });

  it('missing_basemap_still_is', () => {
    expect(
      mapInitOutcome(
        { provisionFailures: ['alps_basemap.pmtiles'], optionalMissing: [] },
        TERRAIN,
      ),
    ).toEqual({ hasTerrain: true, userFacingFailures: ['alps_basemap.pmtiles'] });
  });
});
