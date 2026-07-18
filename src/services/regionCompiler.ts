/**
 * regionCompiler.ts — the one entry point for queuing a background region
 * compile (P9.C2/C3).
 *
 * Both producers — RegionPicker's preset cards and RegionSelectorOverlay's
 * custom reticle — funnel through enqueueRegionDownload so the jobId
 * convention, the single-job guard, and the post-enqueue discovery re-query
 * cannot drift apart. Returns a result object instead of throwing: both
 * callers render the failure inline, and the web build (no native bridge)
 * fails on every call by design.
 */

import { MapCompiler } from '../plugins/MapCompiler';
import { useCompilerStore } from '../store/compilerStore';

/** Vector tile range compiled by the engine. The terrain raster pyramid
 *  (z5–12) is derived engine-side from the same job — not a JS knob. */
export const COMPILE_MIN_ZOOM = 5;
export const COMPILE_MAX_ZOOM = 14;

export interface EnqueueRegionResult {
  queued: boolean;
  /** Present when queued — also names the eventual `{jobId}.pmtiles`. */
  jobId?: string;
  /** Human-readable failure when not queued. */
  error?: string;
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
 * Queues a background compile for `bbox` ("west,south,east,north" WGS84).
 *
 * Refuses while a job is already queued/running: the native PendingJobStore
 * is single-job by design and a second enqueue would OVERWRITE the durable
 * record. On success, isBackgroundCompiling flips via discoverBackgroundJobs
 * — the durable native record stays the source of truth, never an
 * optimistic JS-side assumption.
 */
export async function enqueueRegionDownload(
  regionLabel: string,
  bbox: string,
): Promise<EnqueueRegionResult> {
  if (useCompilerStore.getState().isBackgroundCompiling) {
    return { queued: false, error: 'A background compile is already queued — one region at a time.' };
  }

  // Timestamp suffix keeps re-compiles of the same region from colliding
  // with an older OPFS archive of the same name; base36 keeps it short.
  const jobId = `bg_${slugify(regionLabel)}_${Date.now().toString(36)}`;

  try {
    await MapCompiler.enqueueBackgroundJob({
      bbox,
      jobId,
      minZoom: COMPILE_MIN_ZOOM,
      maxZoom: COMPILE_MAX_ZOOM,
    });
    await useCompilerStore.getState().discoverBackgroundJobs();
    return { queued: true, jobId };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return {
      queued: false,
      error: message.toLowerCase().includes('not implemented')
        ? 'Background compiling needs the iOS/Android app — the web build has no native compile engine.'
        : message,
    };
  }
}
