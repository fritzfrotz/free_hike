// SPDX-License-Identifier: Apache-2.0
// P9.C8 — the compileProgress ref sink and the state word derived from it.
// The sink is seeded by background-job discovery (polled every 3 s while a
// job is pending); BackgroundHandoffBar paints the label straight to the DOM.
import { describe, expect, it } from 'vitest';
import {
  PAUSED_AFTER_MS,
  clearCompileProgress,
  compileStateLabel,
  readCompileProgress,
  seedCompileProgress,
} from './compileProgress';

describe('compileProgress', () => {
  it('compileProgress: sink round-trip (seed, read, clear)', () => {
    clearCompileProgress();
    expect(readCompileProgress()).toEqual({ jobId: null, percent: null, atMs: null });

    seedCompileProgress('bg_a', 42, 1_000);
    expect(readCompileProgress()).toEqual({ jobId: 'bg_a', percent: 42, atMs: 1_000 });

    // A pending job with nothing saved yet: known job, no number.
    seedCompileProgress('bg_b', null, null);
    expect(readCompileProgress()).toEqual({ jobId: 'bg_b', percent: null, atMs: null });

    clearCompileProgress();
    expect(readCompileProgress()).toEqual({ jobId: null, percent: null, atMs: null });
  });

  it('compileStateLabel: queued / compiling / paused by staleness', () => {
    const now = 100_000;

    // No percent saved yet (also the permanent iOS case): queued, no number.
    expect(compileStateLabel({ jobId: 'j', percent: null, atMs: null }, now)).toEqual({
      word: 'queued',
      text: 'Queued',
    });

    // Fresh save: compiling.
    expect(compileStateLabel({ jobId: 'j', percent: 42, atMs: now - 3_000 }, now)).toEqual({
      word: 'compiling',
      text: 'Compiling 42%',
    });

    // Exactly at the threshold still counts as compiling; one ms past it pauses.
    expect(compileStateLabel({ jobId: 'j', percent: 42, atMs: now - PAUSED_AFTER_MS }, now).word).toBe(
      'compiling',
    );
    expect(compileStateLabel({ jobId: 'j', percent: 42, atMs: now - PAUSED_AFTER_MS - 1 }, now)).toEqual({
      word: 'paused',
      text: 'Paused at 42% — continues while charging',
    });

    // A save stamped slightly ahead of the WebView clock is not stale.
    expect(compileStateLabel({ jobId: 'j', percent: 7, atMs: now + 500 }, now).word).toBe('compiling');

    // 0% is a real number, not "queued".
    expect(compileStateLabel({ jobId: 'j', percent: 0, atMs: now }, now).text).toBe('Compiling 0%');
  });
});
