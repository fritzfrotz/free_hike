// SPDX-License-Identifier: Apache-2.0
// P-FE.C1 priority 5 — durable-storage request + quota estimation.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { requestPersistentStorage, showStorageWarning } from './storageGuard';

const GB = 1024 * 1024 * 1024;

let logSpy: ReturnType<typeof vi.spyOn>;
let warnSpy: ReturnType<typeof vi.spyOn>;
let errorSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
  warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
});

afterEach(() => {
  logSpy.mockRestore();
  warnSpy.mockRestore();
  errorSpy.mockRestore();
});

describe('requestPersistentStorage', () => {
  it('reports persistence + quota/usage in GB (3 decimals) when granted', async () => {
    vi.stubGlobal('navigator', {
      storage: {
        persist: async () => true,
        estimate: async () => ({ usage: 1.5 * GB, quota: 10 * GB }),
      },
    });
    const status = await requestPersistentStorage();
    expect(status).toEqual({ isPersistent: true, usageGb: 1.5, quotaGb: 10 });
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it('falls back to persisted() when persist() is absent', async () => {
    vi.stubGlobal('navigator', {
      storage: {
        persisted: async () => true,
        estimate: async () => ({ usage: 0, quota: GB }),
      },
    });
    const status = await requestPersistentStorage();
    expect(status.isPersistent).toBe(true);
  });

  it('warns and stays non-persistent when the request is denied', async () => {
    vi.stubGlobal('navigator', {
      storage: {
        persist: async () => false,
        estimate: async () => ({ usage: 0, quota: GB }),
      },
    });
    const status = await requestPersistentStorage();
    expect(status.isPersistent).toBe(false);
    expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining('not persistent'));
  });

  it('returns zeroed status when navigator.storage is unavailable', async () => {
    vi.stubGlobal('navigator', {});
    const status = await requestPersistentStorage();
    expect(status).toEqual({ isPersistent: false, quotaGb: 0, usageGb: 0 });
    expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining('not supported'));
  });

  it('contains API failures instead of throwing', async () => {
    vi.stubGlobal('navigator', {
      storage: {
        persist: async () => {
          throw new Error('boom');
        },
      },
    });
    const status = await requestPersistentStorage();
    expect(status).toEqual({ isPersistent: false, quotaGb: 0, usageGb: 0 });
    expect(errorSpy).toHaveBeenCalled();
  });
});

// P9.C10 — Android's web view returned persist() === false on every boot
// (device runs 10-07, 10-12) and offers no way to grant it, so on native the
// warning is noise the user cannot act on.
describe('showStorageWarning', () => {
  it('showStorageWarning: never on native, only when a web build is not persistent', () => {
    expect(showStorageWarning(true, false)).toBe(false);
    expect(showStorageWarning(true, true)).toBe(false);
    expect(showStorageWarning(true, null)).toBe(false);
    expect(showStorageWarning(false, false)).toBe(true);
    expect(showStorageWarning(false, true)).toBe(false);
    // Not answered yet: no warning.
    expect(showStorageWarning(false, null)).toBe(false);
  });
});
