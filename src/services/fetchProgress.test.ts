// SPDX-License-Identifier: Apache-2.0
// P-SOV.C2b — the fetchProgress ref sink: one producer (the native
// `fetchProgress` listener), one consumer (FetchProgressBar's rAF loop),
// no React/Zustand in between (ARCHITECTURE.md P8).
import { describe, expect, it } from 'vitest';
import { readFetchProgress, reportFetchProgress, resetFetchProgress } from './fetchProgress';

describe('fetchProgress sink', () => {
  it('sink_reports_latest', () => {
    resetFetchProgress();
    reportFetchProgress(12.5, 'fetch: 50.4 MiB / 403.0 MiB (portugal-260910.osm.pbf)');
    reportFetchProgress(13.0, 'fetch: 52.4 MiB / 403.0 MiB (portugal-260910.osm.pbf)');
    const snap = readFetchProgress();
    expect(snap.percentage).toBe(13.0);
    expect(snap.status).toContain('52.4 MiB');
  });

  it('reset_zeroes', () => {
    reportFetchProgress(99, 'almost');
    resetFetchProgress();
    expect(readFetchProgress()).toEqual({ percentage: 0, status: '' });
  });

  it('returns the live object, never a copy', () => {
    resetFetchProgress();
    const a = readFetchProgress();
    reportFetchProgress(42, 'x');
    expect(a.percentage).toBe(42);
  });
});
