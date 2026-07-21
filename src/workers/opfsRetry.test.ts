// SPDX-License-Identifier: Apache-2.0
// P-FE.C2 — bounded sync-handle retry (ex-B005) against the fake OPFS's
// real exclusivity semantics.
import { describe, expect, it } from 'vitest';
import { FakeOpfs } from '../test/fakeOpfs';
import { createSyncHandleWithRetry } from './opfsRetry';

const noSleep = async () => {};

describe('createSyncHandleWithRetry', () => {
  it('acquires immediately when the file is unlocked (one attempt)', async () => {
    const opfs = new FakeOpfs();
    opfs.seed('map.pmtiles', new Uint8Array([1, 2, 3]));
    const fileHandle = await opfs.root.getFileHandle('map.pmtiles');

    const handle = await createSyncHandleWithRetry(fileHandle, 'map.pmtiles', { sleep: noSleep });
    expect(handle.getSize()).toBe(3);
    handle.close();
  });

  it('bridges a transient holder: succeeds once the previous lock releases', async () => {
    // The B005 scenario: the reload-killed previous worker still holds the
    // exclusive handle for a beat; the new worker retries and wins.
    const opfs = new FakeOpfs();
    opfs.seed('bound.pmtiles', new Uint8Array(8));
    const fileHandle = await opfs.root.getFileHandle('bound.pmtiles');
    const previousWorkerHandle = await fileHandle.createSyncAccessHandle();

    let sleeps = 0;
    const releasingSleep = async () => {
      sleeps += 1;
      if (sleeps === 2) previousWorkerHandle.close(); // teardown finishes
    };

    const handle = await createSyncHandleWithRetry(fileHandle, 'bound.pmtiles', {
      attempts: 5,
      sleep: releasingSleep,
    });
    expect(sleeps).toBe(2);
    expect(handle.getSize()).toBe(8);
    handle.close();
  });

  it('fails loudly after the attempt budget when a REAL holder keeps the lock', async () => {
    const opfs = new FakeOpfs();
    opfs.seed('held.pmtiles', new Uint8Array(4));
    const fileHandle = await opfs.root.getFileHandle('held.pmtiles');
    const holder = await fileHandle.createSyncAccessHandle();

    await expect(
      createSyncHandleWithRetry(fileHandle, 'held.pmtiles', { attempts: 3, sleep: noSleep }),
    ).rejects.toThrow(/still locked after 3 attempts/);
    holder.close();
  });

  it('rethrows non-lock errors immediately without retrying', async () => {
    let calls = 0;
    const brokenHandle = {
      createSyncAccessHandle: async (): Promise<never> => {
        calls += 1;
        const err = new Error('quota exhausted');
        err.name = 'QuotaExceededError';
        throw err;
      },
    };
    await expect(
      createSyncHandleWithRetry(brokenHandle, 'x.pmtiles', { attempts: 5, sleep: noSleep }),
    ).rejects.toThrow('quota exhausted');
    expect(calls).toBe(1);
  });
});
