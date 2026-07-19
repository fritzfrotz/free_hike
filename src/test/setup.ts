// SPDX-License-Identifier: Apache-2.0
/**
 * setup.ts — global vitest setup (P-FE.C1 harness).
 *
 * 1. In-memory localStorage: zustand/persist (mapStore) rehydrates
 *    synchronously at module import; without a Storage the middleware
 *    warns on every suite. The stub is installed via defineProperty
 *    (NOT vi.stubGlobal) so per-test `vi.unstubAllGlobals()` never
 *    removes it.
 * 2. `vi.unstubAllGlobals()` after every test: suites stub `navigator`
 *    (FakeOpfs.install, storageGuard scenarios) and must not leak stubs
 *    into each other.
 */
import { afterEach, vi } from 'vitest';

const mem = new Map<string, string>();
const localStorageStub = {
  getItem: (k: string) => mem.get(k) ?? null,
  setItem: (k: string, v: string) => {
    mem.set(k, String(v));
  },
  removeItem: (k: string) => {
    mem.delete(k);
  },
  clear: () => mem.clear(),
  key: (i: number) => [...mem.keys()][i] ?? null,
  get length() {
    return mem.size;
  },
};

Object.defineProperty(globalThis, 'localStorage', {
  value: localStorageStub,
  configurable: true,
});

afterEach(() => {
  vi.unstubAllGlobals();
});
