// SPDX-License-Identifier: Apache-2.0
// P-FE.C1 priority 5 — the rAF-drained byte-progress bus.
import { describe, expect, it } from 'vitest';
import {
  readHandoffProgress,
  reportHandoffProgress,
  resetHandoffProgress,
} from './handoffProgress';

describe('handoffProgress', () => {
  it('reset zeroes bytesWritten and arms the total', () => {
    resetHandoffProgress(1000);
    expect(readHandoffProgress()).toEqual({ bytesWritten: 0, totalBytes: 1000 });
  });

  it('report overwrites with absolute values (no accumulation)', () => {
    resetHandoffProgress(1000);
    reportHandoffProgress(300, 1000);
    reportHandoffProgress(750, 1000);
    expect(readHandoffProgress()).toEqual({ bytesWritten: 750, totalBytes: 1000 });
  });

  it('read returns the LIVE object (documented contract: one slot, no copies)', () => {
    resetHandoffProgress(10);
    const a = readHandoffProgress();
    reportHandoffProgress(5, 10);
    // Same identity, updated in place — this is what lets the rAF consumer
    // poll without allocation.
    expect(readHandoffProgress()).toBe(a);
    expect(a.bytesWritten).toBe(5);
  });
});
