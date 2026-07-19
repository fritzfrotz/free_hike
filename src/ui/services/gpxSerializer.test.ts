// SPDX-License-Identifier: Apache-2.0
// P-FE.C1 priority 1 — locks the GPX 1.1 export format.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { featuresToGpx } from './gpxSerializer';
import type { CachedTrailFeature } from '../../shared/types';

function feature(over: Partial<CachedTrailFeature> = {}): CachedTrailFeature {
  return {
    id: 1,
    name: 'Goetheweg',
    highway: 'path',
    coords: [11.3908, 47.2757, 11.3921, 47.2769],
    minX: 11.39,
    minY: 47.27,
    maxX: 11.4,
    maxY: 47.28,
    ...over,
  };
}

afterEach(() => {
  vi.useRealTimers();
});

describe('featuresToGpx', () => {
  it('emits one trkpt per coordinate pair at 7-decimal precision', () => {
    const gpx = featuresToGpx([feature()]);
    expect(gpx).toContain('<trkpt lat="47.2757000" lon="11.3908000"/>');
    expect(gpx).toContain('<trkpt lat="47.2769000" lon="11.3921000"/>');
    expect(gpx.match(/<trkpt /g)).toHaveLength(2);
    expect(gpx).toContain('<name>Goetheweg</name>');
    expect(gpx).toContain('<type>path</type>');
  });

  it('escapes XML special characters in UTF-8 names (»…straße«-class input)', () => {
    const gpx = featuresToGpx([
      feature({ name: 'Höttinger Straße <A> & "B" \'C\'', highway: 'track<>' }),
    ]);
    expect(gpx).toContain(
      '<name>Höttinger Straße &lt;A&gt; &amp; &quot;B&quot; &apos;C&apos;</name>',
    );
    expect(gpx).toContain('<type>track&lt;&gt;</type>');
    // No raw specials survive inside element content.
    expect(gpx).not.toContain('<A>');
  });

  it('produces a valid document with zero trk elements for an empty cache', () => {
    const gpx = featuresToGpx([]);
    expect(gpx).toContain('<?xml version="1.0" encoding="UTF-8"?>');
    expect(gpx).toContain('<gpx version="1.1"');
    expect(gpx).toContain('</gpx>');
    expect(gpx).not.toContain('<trk>');
    expect(gpx).toContain('<desc>0 OSM trail features exported from local Flatbush index</desc>');
  });

  it('handles a single-point track (one pair → one trkpt)', () => {
    const gpx = featuresToGpx([feature({ coords: [11.5, 47.5] })]);
    expect(gpx.match(/<trkpt /g)).toHaveLength(1);
    expect(gpx).toContain('<trkpt lat="47.5000000" lon="11.5000000"/>');
  });

  it('stamps metadata time as the current ISO instant', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-07-19T12:34:56.000Z'));
    const gpx = featuresToGpx([]);
    expect(gpx).toContain('<time>2026-07-19T12:34:56.000Z</time>');
  });

  it('drops a trailing odd coordinate rather than emitting a half pair', () => {
    const gpx = featuresToGpx([feature({ coords: [11.5, 47.5, 11.6] })]);
    expect(gpx.match(/<trkpt /g)).toHaveLength(1);
  });
});
