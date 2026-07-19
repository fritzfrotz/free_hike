// SPDX-License-Identifier: Apache-2.0
// P-FE.C1 priority 2 — the chunk loop over fake Filesystem + fake OPFS.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { FakeOpfs } from '../test/fakeOpfs';
import {
  fsBackend,
  resetFilesystemBackend,
  type FakeFilesystemBackend,
} from '../test/fakeFilesystem';

const platform = vi.hoisted(() => ({ native: true }));

vi.mock('@capacitor/core', () => ({
  Capacitor: { isNativePlatform: () => platform.native },
  registerPlugin: () => ({}),
}));
vi.mock('@capacitor/filesystem', async () => {
  const { FilesystemMock } = await import('../test/fakeFilesystem');
  return { Filesystem: FilesystemMock, Directory: { Data: 'DATA' } };
});

import { moveNativeFileToOPFS } from './opfsMover';

function patternBytes(n: number): Uint8Array {
  const b = new Uint8Array(n);
  for (let i = 0; i < n; i++) b[i] = (i * 7 + (i >> 8)) & 0xff;
  return b;
}

let backend: FakeFilesystemBackend;
let opfs: FakeOpfs;

beforeEach(() => {
  platform.native = true;
  backend = resetFilesystemBackend();
  opfs = new FakeOpfs();
  opfs.install();
});

describe('moveNativeFileToOPFS', () => {
  it('reassembles bytes exactly across chunk boundaries (final short chunk included)', async () => {
    const bytes = patternBytes(3 * 1024 + 137); // 3 full chunks + short tail
    const uri = backend.seed('map_jobs/a.pmtiles', bytes);

    const result = await moveNativeFileToOPFS({
      nativeFilePath: uri,
      opfsFilename: 'a.pmtiles',
      chunkSizeBytes: 1024,
    });

    expect(result).toEqual({ opfsFilename: 'a.pmtiles', bytesWritten: bytes.byteLength });
    expect(opfs.committedBytes('a.pmtiles')).toEqual(bytes);
  });

  it('respects the requested chunk size on the native read side', async () => {
    const bytes = patternBytes(2 * 512 + 100);
    const uri = backend.seed('map_jobs/b.pmtiles', bytes);
    await moveNativeFileToOPFS({ nativeFilePath: uri, opfsFilename: 'b.pmtiles', chunkSizeBytes: 512 });

    const chunkLens = fsBackend.current.calls
      .filter((c) => c.startsWith('chunk:'))
      .map((c) => Number(c.split(':')[2]));
    expect(chunkLens).toEqual([512, 512, 100]);
  });

  it('reports monotonic progress ending exactly at the total', async () => {
    const bytes = patternBytes(4 * 256);
    const uri = backend.seed('map_jobs/c.pmtiles', bytes);
    const seen: Array<[number, number]> = [];

    await moveNativeFileToOPFS({
      nativeFilePath: uri,
      opfsFilename: 'c.pmtiles',
      chunkSizeBytes: 256,
      onProgress: (written, total) => seen.push([written, total]),
    });

    expect(seen.length).toBe(4);
    for (let i = 1; i < seen.length; i++) {
      expect(seen[i][0]).toBeGreaterThan(seen[i - 1][0]);
    }
    expect(seen.at(-1)).toEqual([bytes.byteLength, bytes.byteLength]);
    expect(seen.every(([, total]) => total === bytes.byteLength)).toBe(true);
  });

  it('rejects on a mid-stream error and never presents a partial file as complete', async () => {
    const bytes = patternBytes(3 * 1024);
    const uri = backend.seed('map_jobs/d.pmtiles', bytes);
    backend.errorAtChunk = 1; // first chunk lands, second errors

    await expect(
      moveNativeFileToOPFS({ nativeFilePath: uri, opfsFilename: 'd.pmtiles', chunkSizeBytes: 1024 }),
    ).rejects.toThrow('fake EIO mid-stream');

    // Swap-on-close semantics: the aborted writable committed nothing —
    // the entry exists (created) but holds zero bytes, which every
    // consumer (cold-boot probe included) treats as invalid.
    expect(opfs.committedBytes('d.pmtiles')).toEqual(new Uint8Array(0));
  });

  it('rejects a torn transfer when stat-reported size disagrees with bytes read', async () => {
    const bytes = patternBytes(1000);
    const uri = backend.seed('map_jobs/e.pmtiles', bytes);
    backend.statSizeOverride.set(uri, 1005); // native claims 5 bytes more

    await expect(
      moveNativeFileToOPFS({ nativeFilePath: uri, opfsFilename: 'e.pmtiles', chunkSizeBytes: 512 }),
    ).rejects.toThrow(/Torn transfer/);
    expect(opfs.committedBytes('e.pmtiles')).toEqual(new Uint8Array(0));
  });

  it('refuses to run without the native bridge', async () => {
    platform.native = false;
    await expect(
      moveNativeFileToOPFS({ nativeFilePath: 'file:///x', opfsFilename: 'x.pmtiles' }),
    ).rejects.toThrow(/requires an iOS\/Android shell/);
  });
});
