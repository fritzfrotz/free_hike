// SPDX-License-Identifier: Apache-2.0
// Harness self-test: the fake's lock semantics are load-bearing for every
// suite that relies on them (the swap-vs-bound-file rule — see tracker
// item B005 context), so the fake itself is pinned by tests.
import { describe, expect, it } from 'vitest';
import { FakeOpfs } from './fakeOpfs';

describe('FakeOpfs exclusivity (B005-class lock semantics)', () => {
  it('an open SyncAccessHandle blocks createWritable on the same file', async () => {
    const opfs = new FakeOpfs();
    opfs.seed('bound.pmtiles', new Uint8Array([1, 2, 3]));
    const handle = await opfs.root.getFileHandle('bound.pmtiles');
    const sync = await handle.createSyncAccessHandle();

    await expect(handle.createWritable()).rejects.toMatchObject({
      name: 'NoModificationAllowedError',
    });

    sync.close();
    await expect(handle.createWritable()).resolves.toBeDefined();
  });

  it('a second SyncAccessHandle is refused until the first closes', async () => {
    const opfs = new FakeOpfs();
    opfs.seed('f.bin', new Uint8Array(4));
    const handle = await opfs.root.getFileHandle('f.bin');
    const first = await handle.createSyncAccessHandle();
    await expect(handle.createSyncAccessHandle()).rejects.toMatchObject({
      name: 'NoModificationAllowedError',
    });
    first.close();
    const second = await handle.createSyncAccessHandle();
    expect(second.getSize()).toBe(4);
    second.close();
  });

  it('writable commits swap-on-close; abort leaves prior content intact', async () => {
    const opfs = new FakeOpfs();
    opfs.seed('swap.bin', new Uint8Array([9, 9]));
    const handle = await opfs.root.getFileHandle('swap.bin');

    const aborted = await handle.createWritable();
    await aborted.write(new Uint8Array([1]));
    await aborted.abort();
    expect(opfs.committedBytes('swap.bin')).toEqual(new Uint8Array([9, 9]));

    const committed = await handle.createWritable();
    await committed.write(new Uint8Array([1, 2]));
    await committed.write(new Uint8Array([3]));
    // Not visible until close (swap semantics)...
    expect(opfs.committedBytes('swap.bin')).toEqual(new Uint8Array([9, 9]));
    await committed.close();
    expect(opfs.committedBytes('swap.bin')).toEqual(new Uint8Array([1, 2, 3]));
  });

  it('sync handle read/write/truncate round-trips through close()', async () => {
    const opfs = new FakeOpfs();
    const handle = await opfs.root.getFileHandle('rw.bin', { create: true });
    const sync = await handle.createSyncAccessHandle();
    sync.write(new Uint8Array([10, 20, 30, 40]), { at: 0 });
    sync.truncate(3);
    const out = new Uint8Array(3);
    expect(sync.read(out, { at: 0 })).toBe(3);
    expect(out).toEqual(new Uint8Array([10, 20, 30]));
    sync.close();
    expect(opfs.committedBytes('rw.bin')).toEqual(new Uint8Array([10, 20, 30]));
  });
});
