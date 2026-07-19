// SPDX-License-Identifier: Apache-2.0
// P-FE.C1 priority 3 — the P9.C1 background-handoff lifecycle: cold-boot
// discovery, the doorbell re-query, acknowledge-after-durable-close
// ordering, and the foreground finish path (which asserts the FIXED
// contract from P9.C7 — tracker item D008 was closed earlier the same
// day this suite landed, so there is no leaky behavior left to
// characterize; these tests pin the fix instead).
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { FakeOpfs } from '../test/fakeOpfs';
import {
  resetFilesystemBackend,
  type FakeFilesystemBackend,
} from '../test/fakeFilesystem';

const mapCompiler = vi.hoisted(() => ({
  queryBackgroundJob: vi.fn<() => Promise<Record<string, unknown>>>(),
  acknowledgeBackgroundJob: vi.fn<(o: { jobId: string }) => Promise<{ cleared: boolean }>>(),
}));

vi.mock('@capacitor/core', () => ({
  Capacitor: { isNativePlatform: () => true },
  registerPlugin: () => ({}),
}));
vi.mock('@capacitor/filesystem', async () => {
  const { FilesystemMock } = await import('../test/fakeFilesystem');
  return { Filesystem: FilesystemMock, Directory: { Data: 'DATA' } };
});
vi.mock('../plugins/MapCompiler', () => ({ MapCompiler: mapCompiler }));
// Spy-wrap (not replace) the progress bus so tests can observe how the
// denominator is seeded while the real slot keeps working.
vi.mock('../services/handoffProgress', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/handoffProgress')>();
  return {
    ...actual,
    resetHandoffProgress: vi.fn(actual.resetHandoffProgress),
  };
});
import { resetHandoffProgress } from '../services/handoffProgress';

import { useCompilerStore } from './compilerStore';
import { useMapStore } from './mapStore';
import { resetStores } from '../test/resetStores';

function patternBytes(n: number): Uint8Array {
  const b = new Uint8Array(n);
  for (let i = 0; i < n; i++) b[i] = (i * 13 + 5) & 0xff;
  return b;
}

let backend: FakeFilesystemBackend;
let opfs: FakeOpfs;

beforeEach(() => {
  resetStores();
  backend = resetFilesystemBackend();
  opfs = new FakeOpfs();
  opfs.install();
  mapCompiler.queryBackgroundJob.mockReset();
  mapCompiler.acknowledgeBackgroundJob.mockReset();
  mapCompiler.acknowledgeBackgroundJob.mockResolvedValue({ cleared: true });
});

describe('discoverBackgroundJobs — cold-boot record dispatch', () => {
  it('idle record clears isBackgroundCompiling', async () => {
    useCompilerStore.setState({ isBackgroundCompiling: true });
    mapCompiler.queryBackgroundJob.mockResolvedValue({ state: 'idle' });
    await useCompilerStore.getState().discoverBackgroundJobs();
    expect(useCompilerStore.getState().isBackgroundCompiling).toBe(false);
  });

  it('pending record sets isBackgroundCompiling and nothing else', async () => {
    mapCompiler.queryBackgroundJob.mockResolvedValue({ state: 'pending', jobId: 'bg_a' });
    await useCompilerStore.getState().discoverBackgroundJobs();
    const s = useCompilerStore.getState();
    expect(s.isBackgroundCompiling).toBe(true);
    expect(s.pendingHandoffJobs).toEqual([]);
    expect(mapCompiler.acknowledgeBackgroundJob).not.toHaveBeenCalled();
  });

  it('failed record surfaces the reason and acknowledges (releases) it', async () => {
    mapCompiler.queryBackgroundJob.mockResolvedValue({
      state: 'failed',
      jobId: 'bg_b',
      reason: 'disk full',
    });
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    await useCompilerStore.getState().discoverBackgroundJobs();
    const s = useCompilerStore.getState();
    expect(s.isBackgroundCompiling).toBe(false);
    expect(s.backgroundProgress).toEqual({ stage: 'error', jobId: 'bg_b', error: 'disk full' });
    expect(mapCompiler.acknowledgeBackgroundJob).toHaveBeenCalledWith({ jobId: 'bg_b' });
    errSpy.mockRestore();
  });

  it('no native bridge (query rejects) is swallowed', async () => {
    mapCompiler.queryBackgroundJob.mockRejectedValue(new Error('not implemented'));
    await expect(useCompilerStore.getState().discoverBackgroundJobs()).resolves.toBeUndefined();
  });
});

describe('finished-record ingest — acknowledge-after-durable-close ordering', () => {
  const BYTES = patternBytes(2048 + 77);

  function seedFinished(jobId = 'bg_ok') {
    const uri = backend.seed(`map_jobs/${jobId}.pmtiles`, BYTES);
    mapCompiler.queryBackgroundJob.mockResolvedValue({
      state: 'finished',
      jobId,
      archivePath: uri,
      bytesWritten: BYTES.byteLength,
      blocksTotal: 9,
    });
    return uri;
  }

  it('copies into OPFS, acknowledges ONLY after the copy is durable, then hot-swaps', async () => {
    seedFinished();
    let durableAtAck: Uint8Array | null = null;
    mapCompiler.acknowledgeBackgroundJob.mockImplementation(async () => {
      durableAtAck = opfs.committedBytes('bg_ok.pmtiles');
      return { cleared: true };
    });

    await useCompilerStore.getState().discoverBackgroundJobs();

    // Durable-before-ack: at acknowledge time the OPFS copy was already
    // committed byte-for-byte.
    expect(durableAtAck).toEqual(BYTES);
    expect(mapCompiler.acknowledgeBackgroundJob).toHaveBeenCalledTimes(1);
    expect(mapCompiler.acknowledgeBackgroundJob).toHaveBeenCalledWith({ jobId: 'bg_ok' });

    const s = useCompilerStore.getState();
    expect(s.pendingHandoffJobs).toEqual([]);
    expect(s.backgroundProgress.stage).toBe('done');
    expect(useMapStore.getState().activeRegion).toEqual({
      regionLabel: 'bg_ok',
      basemapFile: 'bg_ok.pmtiles',
      terrainFile: 'alps_terrain.pmtiles',
    });
  });

  it('does NOT acknowledge when the OPFS write fails; the job survives for retry', async () => {
    seedFinished('bg_fail');
    opfs.failWrites.add('bg_fail.pmtiles');
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    await useCompilerStore.getState().discoverBackgroundJobs();

    expect(mapCompiler.acknowledgeBackgroundJob).not.toHaveBeenCalled();
    const s = useCompilerStore.getState();
    expect(s.backgroundProgress.stage).toBe('error');
    expect(s.backgroundProgress.jobId).toBe('bg_fail');
    // Retry-safe: the job stays queued and no complete-looking OPFS file exists.
    expect(s.pendingHandoffJobs.map((j) => j.jobId)).toEqual(['bg_fail']);
    expect(opfs.committedBytes('bg_fail.pmtiles')).toEqual(new Uint8Array(0));
    expect(useMapStore.getState().activeRegion).toBeNull();
    errSpy.mockRestore();
  });

  it('doorbell + cold-boot racing the same record ingests exactly once (re-entrancy guard)', async () => {
    seedFinished('bg_race');
    let release!: () => void;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    backend.chunkGate = () => gate;

    // Cold-boot discovery and the backgroundCompile doorbell both re-query
    // the durable record; the second must not double-copy or double-ack.
    const first = useCompilerStore.getState().discoverBackgroundJobs();
    const second = useCompilerStore.getState().discoverBackgroundJobs();
    release();
    await Promise.all([first, second]);

    expect(mapCompiler.acknowledgeBackgroundJob).toHaveBeenCalledTimes(1);
    expect(useCompilerStore.getState().pendingHandoffJobs).toEqual([]);
    expect(opfs.committedBytes('bg_race.pmtiles')).toEqual(BYTES);
  });

  it('B006 (current behavior): seeds the handoff denominator from LOGICAL bytesWritten, not archive size', async () => {
    // Tracker item B006: the record's bytesWritten is the engine's logical
    // accounting (index bytes + payload), which exceeds the real archive
    // size — so the progress bar's initial denominator is wrong until the
    // mover's first onProgress overwrites it with the stat total. This
    // test CHARACTERIZES the bug; when B006 is fixed (seed from the
    // archive's stat size), flip this assertion deliberately and bury the
    // id with `closes B006` in LOOPLOG.
    const uri = backend.seed('map_jobs/bg_b006.pmtiles', BYTES);
    const logicalBytes = BYTES.byteLength * 40; // engine accounting ≫ archive size
    mapCompiler.queryBackgroundJob.mockResolvedValue({
      state: 'finished',
      jobId: 'bg_b006',
      archivePath: uri,
      bytesWritten: logicalBytes,
      blocksTotal: 9,
    });

    await useCompilerStore.getState().discoverBackgroundJobs();

    expect(resetHandoffProgress).toHaveBeenCalledWith(logicalBytes); // ← the B006 wrong seed
    expect(mapCompiler.acknowledgeBackgroundJob).toHaveBeenCalledTimes(1);
  });

  it('finished record missing jobId/archivePath is rejected without crashing', async () => {
    mapCompiler.queryBackgroundJob.mockResolvedValue({ state: 'finished' });
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    await useCompilerStore.getState().discoverBackgroundJobs();
    expect(useCompilerStore.getState().pendingHandoffJobs).toEqual([]);
    expect(mapCompiler.acknowledgeBackgroundJob).not.toHaveBeenCalled();
    errSpy.mockRestore();
  });
});

describe('handleJobFinished — foreground finish (P9.C7 fixed contract, ex-D008)', () => {
  const BYTES = patternBytes(4096 + 33);

  it('verifies OPFS size against the sandbox archive, deletes the sandbox copy, then swaps', async () => {
    const uri = backend.seed('map_jobs/fg1.pmtiles', BYTES);

    await useCompilerStore.getState().handleJobFinished('fg1');

    expect(opfs.committedBytes('fg1.pmtiles')).toEqual(BYTES);
    // The D008 fix: the sandbox copy is released after verification.
    expect(backend.deletedPaths).toEqual([uri]);
    expect(useMapStore.getState().activeRegion).toEqual({
      regionLabel: 'fg1',
      basemapFile: 'fg1.pmtiles',
      terrainFile: 'alps_terrain.pmtiles',
    });
    expect(useCompilerStore.getState().isTransferringToOPFS).toBe(false);
  });

  it('on post-copy size mismatch: keeps the sandbox copy, surfaces a BUG-grade log, skips the swap', async () => {
    backend.seed('map_jobs/fg2.pmtiles', BYTES);
    // Force the independent verification (not the mover's own check) to
    // disagree: OPFS reports one byte short of the sandbox size.
    opfs.sizeOverrides.set('fg2.pmtiles', BYTES.byteLength - 1);
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    await useCompilerStore.getState().handleJobFinished('fg2');

    expect(backend.deletedPaths).toEqual([]); // sandbox copy kept
    expect(useMapStore.getState().activeRegion).toBeNull(); // swap skipped
    expect(errSpy).toHaveBeenCalledWith(expect.stringContaining('BUG'));
    expect(useCompilerStore.getState().isTransferringToOPFS).toBe(false);
    errSpy.mockRestore();
  });

  it('a mover failure is contained: no delete, no swap, flag cleared', async () => {
    const uri = backend.seed('map_jobs/fg3.pmtiles', BYTES);
    backend.statSizeOverride.set(uri, BYTES.byteLength + 9); // torn transfer inside the mover
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    await useCompilerStore.getState().handleJobFinished('fg3');

    expect(backend.deletedPaths).toEqual([]);
    expect(useMapStore.getState().activeRegion).toBeNull();
    expect(useCompilerStore.getState().isTransferringToOPFS).toBe(false);
    errSpy.mockRestore();
  });
});
