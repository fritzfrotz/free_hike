/**
 * gpxSerializer.ts
 *
 * Converts the spatial index feature cache (trails_features.json written by
 * spatial.worker.ts to OPFS) into a valid GPX 1.1 XML document suitable for
 * backup to Google Drive or Dropbox.
 *
 * GPX 1.1 spec: http://www.topografix.com/GPX/1/1/gpx.xsd
 *
 * Each OSM way feature becomes one <trk> element containing a single
 * <trkseg> with one <trkpt> per coordinate pair.
 * Feature properties (name, highway type) are written as <name> and <type>.
 *
 * No external dependencies — uses template-literal string construction.
 * All XML special characters in feature metadata are escaped before emission.
 */

import type { CachedTrailFeature } from '../../shared/types';

// ─── XML helpers ──────────────────────────────────────────────────────────────

/** Escapes the five XML special characters to their entity references. */
function escapeXml(raw: string): string {
  return raw
    .replace(/&/g,  '&amp;')
    .replace(/</g,  '&lt;')
    .replace(/>/g,  '&gt;')
    .replace(/"/g,  '&quot;')
    .replace(/'/g,  '&apos;');
}

// ─── Serialiser ───────────────────────────────────────────────────────────────

/**
 * Converts an array of CachedTrailFeature objects into a GPX 1.1 XML string.
 *
 * Coordinate format:
 *   features[n].coords is a flat Float64/number array [lng₀, lat₀, lng₁, lat₁, …].
 *   Each pair becomes a <trkpt lat="..." lon="..."/> element.
 *   Values are rounded to 7 decimal places (≈ 1 cm precision).
 *
 * Empty feature arrays produce a valid GPX document with no <trk> elements.
 */
export function featuresToGpx(features: CachedTrailFeature[]): string {
  const tracksXml = features.map((f) => {
    const trackPoints: string[] = [];
    for (let i = 0; i < f.coords.length - 1; i += 2) {
      const lng = f.coords[i].toFixed(7);
      const lat = f.coords[i + 1].toFixed(7);
      trackPoints.push(`      <trkpt lat="${lat}" lon="${lng}"/>`);
    }

    return [
      '  <trk>',
      `    <name>${escapeXml(f.name)}</name>`,
      `    <type>${escapeXml(f.highway)}</type>`,
      '    <trkseg>',
      ...trackPoints,
      '    </trkseg>',
      '  </trk>',
    ].join('\n');
  });

  return [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<gpx version="1.1"',
    '  creator="FreeHike — Antigravity Local-First Geospatial Engine"',
    '  xmlns="http://www.topografix.com/GPX/1/1"',
    '  xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"',
    '  xsi:schemaLocation="http://www.topografix.com/GPX/1/1',
    '                       http://www.topografix.com/GPX/1/1/gpx.xsd">',
    '  <metadata>',
    '    <name>FreeHike Trail Cache</name>',
    `    <desc>${features.length} OSM trail features exported from local Flatbush index</desc>`,
    `    <time>${new Date().toISOString()}</time>`,
    '  </metadata>',
    ...tracksXml,
    '</gpx>',
  ].join('\n');
}
