// SPDX-License-Identifier: Apache-2.0
// P-FE.C1 priority 4 — the cold-boot validate-before-bind rule (P9.C2),
// tested pure after its mechanical extraction from MapView's load handler.
import { describe, expect, it } from 'vitest';
import { decideRegionBoot, regionFilesToVerify } from './regionBootPolicy';

const DEFAULTS = { basemapFile: 'alps_basemap.pmtiles', terrainFile: 'alps_terrain.pmtiles' };

describe('regionFilesToVerify', () => {
  it('probes only files that differ from the booted defaults', () => {
    expect(
      regionFilesToVerify(
        { regionLabel: 'x', basemapFile: 'bg_x.pmtiles', terrainFile: 'alps_terrain.pmtiles' },
        DEFAULTS,
      ),
    ).toEqual(['bg_x.pmtiles']);
  });

  it('returns nothing when the persisted region IS the default binding', () => {
    expect(
      regionFilesToVerify(
        { regionLabel: 'default', ...DEFAULTS },
        DEFAULTS,
      ),
    ).toEqual([]);
  });

  it('probes both files when both differ', () => {
    expect(
      regionFilesToVerify(
        { regionLabel: 'y', basemapFile: 'bg_y.pmtiles', terrainFile: 'bg_y_terrain.pmtiles' },
        DEFAULTS,
      ),
    ).toEqual(['bg_y.pmtiles', 'bg_y_terrain.pmtiles']);
  });
});

describe('decideRegionBoot', () => {
  it('binds when every probed file holds bytes', () => {
    expect(decideRegionBoot(['bg_x.pmtiles'], [true])).toEqual({ action: 'bind' });
  });

  it('clears when any file is missing — never fabricates an empty archive', () => {
    // A zero-byte OPFS entry probes false (the probe requires size > 0),
    // so eviction AND truncation both land here.
    expect(decideRegionBoot(['bg_x.pmtiles', 'bg_t.pmtiles'], [true, false])).toEqual({
      action: 'clear',
      missing: ['bg_t.pmtiles'],
    });
    expect(decideRegionBoot(['bg_x.pmtiles'], [false])).toEqual({
      action: 'clear',
      missing: ['bg_x.pmtiles'],
    });
  });

  it('no-ops when there was nothing to verify (default binding)', () => {
    expect(decideRegionBoot([], [])).toEqual({ action: 'noop' });
  });

  it('reports every missing file, in probe order', () => {
    expect(decideRegionBoot(['a', 'b'], [false, false])).toEqual({
      action: 'clear',
      missing: ['a', 'b'],
    });
  });
});
