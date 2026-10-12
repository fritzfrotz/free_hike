// SPDX-License-Identifier: Apache-2.0
// P-FE.C1 priority 4 — the cold-boot validate-before-bind rule (P9.C2),
// tested pure after its mechanical extraction from MapView's load handler.
import { describe, expect, it } from 'vitest';
import { decideRegionBoot, planBoot, regionFilesToVerify } from './regionBootPolicy';

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

  // P-SOV.C3b (DL-004): compiled regions carry no terrain archive.
  it('null_terrain_is_never_probed', () => {
    expect(
      regionFilesToVerify(
        { regionLabel: 'pt', basemapFile: 'job-pt.pmtiles', terrainFile: null },
        DEFAULTS,
      ),
    ).toEqual(['job-pt.pmtiles']);
  });

  it('null_terrain_region_binds_on_basemap_alone', () => {
    const toVerify = regionFilesToVerify(
      { regionLabel: 'pt', basemapFile: 'job-pt.pmtiles', terrainFile: null },
      DEFAULTS,
    );
    expect(decideRegionBoot(toVerify, [true])).toEqual({ action: 'bind' });
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

// P9.C9 — the boot picks its files BEFORE the worker opens anything: a
// native boot never opens (and so never fabricates) a dev preview archive.
describe('planBoot', () => {
  const probe = (withBytes: string[]) => {
    const asked: string[] = [];
    const hasBytes = async (f: string) => {
      asked.push(f);
      return withBytes.includes(f);
    };
    return { asked, hasBytes };
  };
  const PT = { regionLabel: 'bg_portugal_x', basemapFile: 'bg_portugal_x.pmtiles', terrainFile: null };

  it('planBoot: binds the saved region directly when its file holds bytes', async () => {
    const p = probe(['bg_portugal_x.pmtiles']);
    expect(await planBoot(PT, { isDev: false, devDefaults: DEFAULTS, hasBytes: p.hasBytes })).toEqual({
      basemap: 'bg_portugal_x.pmtiles',
      terrain: null,
      dropped: null,
    });
    expect(p.asked).toEqual(['bg_portugal_x.pmtiles']);
  });

  it('planBoot: native with no usable region boots with no basemap and never names a dev archive', async () => {
    const p = probe([]);
    expect(await planBoot(null, { isDev: false, devDefaults: DEFAULTS, hasBytes: p.hasBytes })).toEqual({
      basemap: null,
      terrain: null,
      dropped: null,
    });
    expect(p.asked).toEqual([]);

    // A region saved by an older build that points at the preview archive:
    // on native it is probed like any other file (the stub has no bytes).
    const stale = { regionLabel: 'default', ...DEFAULTS };
    const q = probe([]);
    const plan = await planBoot(stale, { isDev: false, devDefaults: DEFAULTS, hasBytes: q.hasBytes });
    expect(plan).toEqual({ basemap: null, terrain: null, dropped: stale });
  });

  it('planBoot: dev server falls back to the preview archives', async () => {
    const p = probe([]);
    expect(await planBoot(null, { isDev: true, devDefaults: DEFAULTS, hasBytes: p.hasBytes })).toEqual({
      basemap: 'alps_basemap.pmtiles',
      terrain: 'alps_terrain.pmtiles',
      dropped: null,
    });
    expect(p.asked).toEqual([]);
  });

  it('planBoot: a saved region with a missing file is dropped', async () => {
    const p = probe([]);
    expect(await planBoot(PT, { isDev: false, devDefaults: DEFAULTS, hasBytes: p.hasBytes })).toEqual({
      basemap: null,
      terrain: null,
      dropped: PT,
    });
    // On the dev server the drop falls back to the preview archives.
    const q = probe([]);
    expect(await planBoot(PT, { isDev: true, devDefaults: DEFAULTS, hasBytes: q.hasBytes })).toEqual({
      basemap: 'alps_basemap.pmtiles',
      terrain: 'alps_terrain.pmtiles',
      dropped: PT,
    });
  });
});
