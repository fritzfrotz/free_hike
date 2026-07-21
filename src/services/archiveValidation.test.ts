// SPDX-License-Identifier: Apache-2.0
// P-FE.C2 — hostile-response guards for the region download flow (ex-B004).
import { describe, expect, it } from 'vitest';
import { looksLikeHtmlFallback, looksLikePmtiles } from './archiveValidation';

function bufferOf(text: string): ArrayBuffer {
  return new TextEncoder().encode(text).buffer as ArrayBuffer;
}

describe('looksLikePmtiles', () => {
  it('accepts a buffer starting with the PMTiles magic', () => {
    expect(looksLikePmtiles(bufferOf('PMTilesrest-of-archive'))).toBe(true);
  });

  it('rejects an SPA-fallback HTML page — the exact B004 poisoning case', () => {
    expect(looksLikePmtiles(bufferOf('<!doctype html><html><head>…'))).toBe(false);
  });

  it('rejects buffers too short to carry the magic', () => {
    expect(looksLikePmtiles(bufferOf('PMT'))).toBe(false);
    expect(looksLikePmtiles(new ArrayBuffer(0))).toBe(false);
  });

  it('rejects a near-miss magic', () => {
    expect(looksLikePmtiles(bufferOf('PMTilez'))).toBe(false);
  });
});

describe('looksLikeHtmlFallback', () => {
  it('flags text/html content types, any casing or charset suffix', () => {
    expect(looksLikeHtmlFallback('text/html')).toBe(true);
    expect(looksLikeHtmlFallback('Text/HTML; charset=utf-8')).toBe(true);
  });

  it('passes binary content types and missing headers through', () => {
    expect(looksLikeHtmlFallback('application/octet-stream')).toBe(false);
    expect(looksLikeHtmlFallback(null)).toBe(false);
  });
});
