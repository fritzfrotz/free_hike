// SPDX-License-Identifier: Apache-2.0
// P-FE.C1 priority 5 — durable-storage request + quota estimation.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { requestPersistentStorage } from './storageGuard';

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
