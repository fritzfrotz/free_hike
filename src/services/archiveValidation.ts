// SPDX-License-Identifier: Apache-2.0
/**
 * archiveValidation.ts — hostile-response guards for the region-download
 * flow (P-FE.C2, tracker B004).
 *
 * The P6 pillar's lesson applied at the JS seam: a dev-server SPA fallback
 * (or a captive portal, or a mirror redirect) answers ANY path with a 200
 * `text/html` page — saving that as a "successful" archive poisons the
 * pipeline downstream, exactly like the Geofabrik-redirect incident the
 * fetcher crate encodes. Validate magic bytes before trusting a body.
 */

/** True when the buffer starts with the PMTiles v3 magic ("PMTiles"). */
export function looksLikePmtiles(buffer: ArrayBuffer): boolean {
  if (buffer.byteLength < 8) return false;
  const magic = new Uint8Array(buffer, 0, 7);
  const expected = 'PMTiles';
  for (let i = 0; i < expected.length; i++) {
    if (magic[i] !== expected.charCodeAt(i)) return false;
  }
  return true;
}

/**
 * True when a fetch Response smells like an HTML fallback rather than the
 * binary asset that was requested (SPA fallback answers 200 for any path).
 */
export function looksLikeHtmlFallback(contentType: string | null): boolean {
  return contentType !== null && contentType.toLowerCase().includes('text/html');
}
