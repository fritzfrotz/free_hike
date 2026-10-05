// SPDX-License-Identifier: Apache-2.0
/**
 * regionCompiler.ts — the two-step region flow (P9.C2/C3, P-SOV.C2b).
 *
 *   1. ensureInputs(sourceId)   → the native fetch_chunk loop lands a
 *      verified raw extract in the sandbox (resumable, pinned, MD5-checked).
 *   2. enqueueRegionDownload(sourceId, label, bbox) → a background compile
 *      whose pbfPath is that verified file.
 *
 * The WebView passes a SOURCE ID across the bridge, never a URL: the source
 * table lives in Rust (ARCHITECTURE.md P10a keeps absolute URLs out of
 * src/). Step 2 refuses unless the native fetch state reports `verified`;
 * the JS pre-check exists so the UI can explain — the native reject in
 * enqueueBackgroundJob is the authority (D6).
 *
 * Both producers — RegionPicker's preset and RegionSelectorOverlay's custom
 * reticle — funnel through here so the jobId convention, the single-job
 * guard and the post-enqueue discovery re-query cannot drift apart. Results
 * are returned, not thrown: the web build (no native bridge) fails every
 * call by design and the callers render that inline.
 */

import { MapCompiler } from '../plugins/MapCompiler';
import { useCompilerStore } from '../store/compilerStore';
import { reportFetchProgress, resetFetchProgress } from './fetchProgress';

/** Vector tile range compiled by the engine. */
export const COMPILE_MIN_ZOOM = 5;
export const COMPILE_MAX_ZOOM = 14;

/** The v0.1 demo source (DL-003): Geofabrik's Portugal extract. */
export const DEMO_SOURCE_ID = 'geofabrik-portugal';

/** Per-slice budget for the foreground fetch loop: long enough that a
 *  reconnect per slice is noise, short enough that cancel feels immediate. */
export const FETCH_SLICE_BUDGET_MS = 5_000;

const WEB_HINT =
  'Region data needs the iOS/Android app — the web build has no native fetcher or compile engine.';

export interface EnqueueRegionResult {
  queued: boolean;
  /** Present when queued — also names the eventual `{jobId}.pmtiles`. */
  jobId?: string;
  /** Human-readable failure when not queued. */
  error?: string;
}

export interface EnsureInputsResult {
  ready: boolean;
  /** Present when ready: absolute sandbox path of the verified extract. */
  path?: string;
  /** Human-readable failure when not ready (absent when cancelled). */
  error?: string;
  /** True when the user cancelled the download. */
  cancelled?: boolean;
}

function explain(err: unknown): string {
  const message = err instanceof Error ? err.message : String(err);
  return message.toLowerCase().includes('not implemented') ? WEB_HINT : message;
}

/** Collapses a display label into a filesystem-safe jobId fragment: the
 *  jobId names the native archive AND its OPFS copy (`{jobId}.pmtiles`). */
function slugify(label: string): string {
  return label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '') || 'region';
}

/**
 * The verified extract's path for `sourceId`, or null when nothing verified
 * exists. Throws on a bridge failure (web build) so callers can explain it.
 */
export async function verifiedInputPath(sourceId: string): Promise<string | null> {
  const { found, state } = await MapCompiler.queryFetch({ sourceId });
  if (!found || !state?.verified || !state.path) return null;
  return state.path;
}

/**
 * Step 1: make sure a verified raw extract for `sourceId` is on disk,
 * downloading it if needed. One promise for the whole native loop; the
 * 'fetchProgress' listener lives exactly as long as the download and feeds
 * the fetchProgress ref sink (never React state).
 */
export async function ensureInputs(sourceId: string): Promise<EnsureInputsResult> {
  const store = useCompilerStore.getState();

  let existing: string | null;
  try {
    existing = await verifiedInputPath(sourceId);
  } catch (err) {
    return { ready: false, error: explain(err) };
  }
  if (existing) {
    store.setFetchStage('ready', sourceId);
    return { ready: true, path: existing };
  }

  store.setFetchStage('fetching', sourceId);
  resetFetchProgress();
  const listener = await MapCompiler.addListener('fetchProgress', (event) => {
    reportFetchProgress(event.percentage, event.status);
  }).catch(() => null);

  try {
    const result = await MapCompiler.fetchInputs({ sourceId, budgetMs: FETCH_SLICE_BUDGET_MS });
    if (result.status === 'finished' && result.path) {
      store.setFetchStage('ready', sourceId);
      return { ready: true, path: result.path };
    }
    if (result.status === 'cancelled') {
      store.setFetchStage('idle');
      return { ready: false, cancelled: true };
    }
    const reason = result.reason ?? 'Download failed.';
    store.setFetchStage('error', sourceId, reason);
    return { ready: false, error: reason };
  } catch (err) {
    const message = explain(err);
    store.setFetchStage('error', sourceId, message);
    return { ready: false, error: message };
  } finally {
    await listener?.remove().catch(() => undefined);
  }
}

/** Asks the native loop to stop at its next slice boundary. */
export async function cancelInputsFetch(): Promise<void> {
  await MapCompiler.cancelFetch().catch(() => undefined);
}

/**
 * Step 2: queues a background compile for `bbox` ("west,south,east,north"
 * WGS84) against the verified extract of `sourceId`.
 *
 * Refuses while a job is already queued/running (the native PendingJobStore
 * is single-job by design) and when no verified extract exists (D6). On
 * success, isBackgroundCompiling flips via discoverBackgroundJobs — the
 * durable native record stays the source of truth.
 */
export async function enqueueRegionDownload(
  sourceId: string,
  regionLabel: string,
  bbox: string,
): Promise<EnqueueRegionResult> {
  if (useCompilerStore.getState().isBackgroundCompiling) {
    return { queued: false, error: 'A background compile is already queued — one region at a time.' };
  }

  let path: string | null;
  try {
    path = await verifiedInputPath(sourceId);
  } catch (err) {
    return { queued: false, error: explain(err) };
  }
  if (!path) {
    return {
      queued: false,
      error: 'Download the region data first — no verified extract for this source is on the device.',
    };
  }

  // Timestamp suffix keeps re-compiles of the same region from colliding
  // with an older OPFS archive of the same name; base36 keeps it short.
  const jobId = `bg_${slugify(regionLabel)}_${Date.now().toString(36)}`;

  try {
    await MapCompiler.enqueueBackgroundJob({
      sourceId,
      bbox,
      jobId,
      minZoom: COMPILE_MIN_ZOOM,
      maxZoom: COMPILE_MAX_ZOOM,
    });
    await useCompilerStore.getState().discoverBackgroundJobs();
    return { queued: true, jobId };
  } catch (err) {
    return { queued: false, error: explain(err) };
  }
}
