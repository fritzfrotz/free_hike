// SPDX-License-Identifier: Apache-2.0
// P-SOV.C2b — the two-step region flow: ensureInputs (native fetch_chunk
// loop behind MapCompiler.fetchInputs) then enqueueRegionDownload, which
// refuses unless the native fetch state reports a verified file. The
// WebView passes a SOURCE ID across the bridge, never a URL (P10a).
import { beforeEach, describe, expect, it, vi } from 'vitest';

const mapCompiler = vi.hoisted(() => ({
  queryFetch: vi.fn<(o: { sourceId: string }) => Promise<Record<string, unknown>>>(),
  fetchInputs: vi.fn<(o: { sourceId: string; budgetMs?: number }) => Promise<Record<string, unknown>>>(),
  enqueueBackgroundJob: vi.fn<(o: Record<string, unknown>) => Promise<{ scheduled: boolean; jobId: string }>>(),
  queryBackgroundJob: vi.fn<() => Promise<Record<string, unknown>>>(),
  addListener: vi.fn<() => Promise<{ remove: () => Promise<void> }>>(),
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

import { DEMO_SOURCE_ID, ensureInputs, enqueueRegionDownload } from './regionCompiler';
import { useCompilerStore } from '../store/compilerStore';
import { resetStores } from '../test/resetStores';

const BBOX = '-9.55,38.38,-8.75,38.95';
const RAW = '/data/user/0/app/files/map_jobs/raw/portugal-260910.osm.pbf';

function verified(path = RAW) {
  return {
    found: true,
    state: {
      sourceId: DEMO_SOURCE_ID,
      pinnedUrl: 'pinned',
      path,
      bytesHave: 10,
      bytesTotal: 10,
      verified: true,
      restarts: 0,
    },
  };
}

beforeEach(() => {
  resetStores();
  for (const fn of Object.values(mapCompiler)) fn.mockReset();
  mapCompiler.addListener.mockResolvedValue({ remove: vi.fn(async () => {}) });
  mapCompiler.queryBackgroundJob.mockResolvedValue({ state: 'idle' });
});

describe('ensureInputs — native fetch loop behind one promise', () => {
  it('ensure_inputs_resolves_finished_path', async () => {
    mapCompiler.queryFetch.mockResolvedValue({ found: false });
    mapCompiler.fetchInputs.mockResolvedValue({
      status: 'finished',
      sourceId: DEMO_SOURCE_ID,
      slices: 3,
      path: RAW,
    });

    const result = await ensureInputs(DEMO_SOURCE_ID);

    expect(result).toEqual({ ready: true, path: RAW });
    expect(mapCompiler.fetchInputs).toHaveBeenCalledTimes(1);
    expect(mapCompiler.fetchInputs.mock.calls[0][0].sourceId).toBe(DEMO_SOURCE_ID);
    const s = useCompilerStore.getState();
    expect(s.fetchStage).toBe('ready');
    expect(s.activeSourceId).toBe(DEMO_SOURCE_ID);
    expect(s.fetchError).toBeNull();
  });

  it('ensure_inputs_skips_fetch_when_already_verified', async () => {
    mapCompiler.queryFetch.mockResolvedValue(verified());

    const result = await ensureInputs(DEMO_SOURCE_ID);

    expect(result).toEqual({ ready: true, path: RAW });
    expect(mapCompiler.fetchInputs).not.toHaveBeenCalled();
    expect(useCompilerStore.getState().fetchStage).toBe('ready');
  });

  it('fetch_failure_surfaces_reason', async () => {
    mapCompiler.queryFetch.mockResolvedValue({ found: false });
    mapCompiler.fetchInputs.mockResolvedValue({
      status: 'failed',
      sourceId: DEMO_SOURCE_ID,
      slices: 2,
      reason: 'md5 mismatch for portugal-260910.osm.pbf',
      transient: false,
    });

    const result = await ensureInputs(DEMO_SOURCE_ID);

    expect(result.ready).toBe(false);
    expect(result.error).toContain('md5');
    const s = useCompilerStore.getState();
    expect(s.fetchStage).toBe('error');
    expect(s.fetchError).toContain('md5');
  });

  it('fetch_cancelled_returns_to_idle', async () => {
    mapCompiler.queryFetch.mockResolvedValue({ found: false });
    mapCompiler.fetchInputs.mockResolvedValue({
      status: 'cancelled',
      sourceId: DEMO_SOURCE_ID,
      slices: 1,
    });

    const result = await ensureInputs(DEMO_SOURCE_ID);

    expect(result).toEqual({ ready: false, cancelled: true });
    expect(useCompilerStore.getState().fetchStage).toBe('idle');
  });

  it('fetch_not_implemented_on_web_is_explained', async () => {
    mapCompiler.queryFetch.mockRejectedValue(new Error('Method not implemented.'));

    const result = await ensureInputs(DEMO_SOURCE_ID);

    expect(result.ready).toBe(false);
    expect(result.error).toContain('iOS/Android app');
    expect(mapCompiler.fetchInputs).not.toHaveBeenCalled();
  });

  it('progress listener is attached for the fetch and removed after', async () => {
    const remove = vi.fn(async () => {});
    mapCompiler.addListener.mockResolvedValue({ remove });
    mapCompiler.queryFetch.mockResolvedValue({ found: false });
    mapCompiler.fetchInputs.mockResolvedValue({
      status: 'finished',
      sourceId: DEMO_SOURCE_ID,
      slices: 1,
      path: RAW,
    });

    await ensureInputs(DEMO_SOURCE_ID);

    expect(mapCompiler.addListener).toHaveBeenCalledWith('fetchProgress', expect.any(Function));
    expect(remove).toHaveBeenCalledTimes(1);
  });
});

describe('enqueueRegionDownload — the D6 gate, JS side', () => {
  it('enqueue_refuses_unverified_state', async () => {
    mapCompiler.queryFetch.mockResolvedValue({
      ...verified(),
      state: { ...verified().state, verified: false, bytesHave: 5 },
    });

    const result = await enqueueRegionDownload(DEMO_SOURCE_ID, 'Portugal', BBOX);

    expect(result.queued).toBe(false);
    expect(result.error?.toLowerCase()).toContain('download');
    expect(mapCompiler.enqueueBackgroundJob).not.toHaveBeenCalled();
  });

  it('enqueue_refuses_when_nothing_was_fetched', async () => {
    mapCompiler.queryFetch.mockResolvedValue({ found: false });

    const result = await enqueueRegionDownload(DEMO_SOURCE_ID, 'Portugal', BBOX);

    expect(result.queued).toBe(false);
    expect(mapCompiler.enqueueBackgroundJob).not.toHaveBeenCalled();
  });

  it('enqueue_passes_source_id_never_a_url', async () => {
    mapCompiler.queryFetch.mockResolvedValue(verified());
    mapCompiler.enqueueBackgroundJob.mockImplementation(async (o) => ({
      scheduled: true,
      jobId: String(o.jobId),
    }));
    mapCompiler.queryBackgroundJob.mockResolvedValue({ state: 'pending', jobId: 'x' });

    const result = await enqueueRegionDownload(DEMO_SOURCE_ID, 'Portugal', BBOX);

    expect(result.queued).toBe(true);
    expect(result.jobId).toMatch(/^bg_portugal_/);
    const args = mapCompiler.enqueueBackgroundJob.mock.calls[0][0];
    expect(args.sourceId).toBe(DEMO_SOURCE_ID);
    expect(args.bbox).toBe(BBOX);
    expect(args.minZoom).toBe(5);
    expect(args.maxZoom).toBe(14);
    expect(JSON.stringify(args)).not.toMatch(/https?:/);
    expect(useCompilerStore.getState().isBackgroundCompiling).toBe(true);
  });

  it('native reject is surfaced verbatim (the native gate is the authority)', async () => {
    mapCompiler.queryFetch.mockResolvedValue(verified());
    mapCompiler.enqueueBackgroundJob.mockRejectedValue(
      new Error('Inputs for geofabrik-portugal are not verified; fetch them first'),
    );

    const result = await enqueueRegionDownload(DEMO_SOURCE_ID, 'Portugal', BBOX);

    expect(result.queued).toBe(false);
    expect(result.error).toContain('not verified');
  });

  it('refuses while a background compile is already queued', async () => {
    useCompilerStore.setState({ isBackgroundCompiling: true });
    mapCompiler.queryFetch.mockResolvedValue(verified());

    const result = await enqueueRegionDownload(DEMO_SOURCE_ID, 'Portugal', BBOX);

    expect(result.queued).toBe(false);
    expect(mapCompiler.enqueueBackgroundJob).not.toHaveBeenCalled();
  });
});
