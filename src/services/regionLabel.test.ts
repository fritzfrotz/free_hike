// SPDX-License-Identifier: Apache-2.0
// P9.C10 — the name the demo screen shows for the bound archive. Compiled
// regions are named by job ID (`bg_<slug>_<base36 time>`, regionCompiler),
// so the slug is the only readable name the device holds.
import { describe, expect, it } from 'vitest';
import { regionDisplayName, savedRegionMissingMessage } from './regionLabel';

describe('regionLabel', () => {
  it('regionDisplayName: job ID slug becomes the region name', () => {
    expect(regionDisplayName('bg_portugal_m3x9k2')).toBe('Portugal');
    expect(regionDisplayName('bg_portugal_m3x9k2.pmtiles')).toBe('Portugal');
    expect(regionDisplayName('bg_lisbon-sintra_ab12')).toBe('Lisbon Sintra');
  });

  it('regionDisplayName: anything else is shown as given, minus .pmtiles', () => {
    expect(regionDisplayName('alps_basemap.pmtiles')).toBe('alps_basemap');
    expect(regionDisplayName('debug-compile')).toBe('debug-compile');
    // Not the job-ID shape (no time suffix): shown as given.
    expect(regionDisplayName('bg_portugal')).toBe('bg_portugal');
  });

  it('savedRegionMissingMessage: names the region', () => {
    expect(savedRegionMissingMessage('bg_portugal_m3x9k2')).toBe(
      'The offline map for Portugal is no longer on this device — download it again.',
    );
  });
});
