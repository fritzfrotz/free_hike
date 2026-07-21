// SPDX-License-Identifier: Apache-2.0
import { create } from 'zustand';
import { Directory, Filesystem } from '@capacitor/filesystem';
import { moveNativeFileToOPFS } from '../services/opfsMover';
import { resetHandoffProgress, reportHandoffProgress } from '../services/handoffProgress';
import { MapCompiler } from '../plugins/MapCompiler';
import { useMapStore } from './mapStore';

/**
 * Relative path (under Capacitor's Directory.Data) to a job's finished
 * archive. Mirrors freehike-core's own `archive_path(output_dir, job_id)`
 * convention — the LOOPLOG's P2.C2 kill-resume torture test evidenced the
 * native plugin's checkpoints living at `files/map_jobs/{job_id}.checkpoint`
 * on Android (`files/` = Directory.Data), and the archive is written
 * alongside the checkpoint in that same output_dir.
 *
 * This is inferred from the native layer's on-disk layout, not read back
 * from it — MapCompilerPlugin's Surface v1 doesn't yet return the archive's
 * path directly. If that surface is extended, resolving the path here should
 * be replaced with whatever it reports.
 */
function nativeArchiveRelativePath(jobId: string): string {
  return `map_jobs/${jobId}.pmtiles`;
}

/** Default terrain source — no compile job produces terrain tiles yet
 *  (Phase 6 is deferred), so every hot-swapped region keeps the shared
 *  default terrain archive rather than trying to substitute one. */
const DEFAULT_TERRAIN_FILE = 'alps_terrain.pmtiles';

/**
 * A finished background compile awaiting its cross-bridge handoff: the
 * archive exists in the native app sandbox and must be stream-copied into
 * OPFS before the native record can be acknowledged (released).
 */
export interface PendingHandoffJob {
  jobId: string;
  /** Absolute path of the compiled `.pmtiles` in the native app sandbox. */
  archivePath: string;
  /** Archive size as reported by the native compile summary. */
  bytesTotal: number;
  blocksTotal: number;
}

/**
 * Coarse, low-frequency stage of the handoff pipeline. Byte-level copy
 * progress deliberately never appears here — it flows through
 * services/handoffProgress (a ref sink drained by a rAF loop in
 * BackgroundHandoffBar), bypassing React/Zustand entirely.
 */
export type BackgroundHandoffStage = 'idle' | 'copying' | 'swapping' | 'done' | 'error';

export interface BackgroundProgress {
  stage: BackgroundHandoffStage;
  /** Job the stage refers to; null when stage === 'idle'. */
  jobId: string | null;
  /** Human-readable failure when stage === 'error'. */
  error: string | null;
}

const IDLE_PROGRESS: BackgroundProgress = { stage: 'idle', jobId: null, error: null };

/**
 * Re-entrancy guard for ingestHandoffJob: cold-boot discovery and a
 * 'backgroundCompile' event can race (both call discoverBackgroundJobs, and
 * the native record stays 'finished' until acknowledged), so the same job
 * must never stream-copy twice concurrently. Module-level rather than store
 * state: it guards async control flow, not anything the UI renders.
 */
const ingestingJobIds = new Set<string>();

interface CompilerState {
  /** Whether a native Rust compile job is currently running (or resuming). */
  isCompiling: boolean;
  /** Human-readable phase label, e.g. "pass1: indexing nodes (12/62)". */
  currentPhase: string;
  /** Set when the native layer reports the compile thread pool was throttled. */
  thermalThrottling: boolean;
  /** True while the finished archive is being copied from native storage
   *  into OPFS — the one step between a Rust "Finished" and a renderable
   *  map, so the UI can hold a "finalizing…" state across it. */
  isTransferringToOPFS: boolean;

  // ── P9.C1: background-job handoff ─────────────────────────────────────────
  /** True while the durable native record reports a queued/running
   *  background compile ('pending'). The OS decides when it actually runs
   *  (charging window), so this can stay true across many app sessions. */
  isBackgroundCompiling: boolean;
  /** Coarse handoff stage — see BackgroundProgress for what is (and is
   *  deliberately not) carried here. */
  backgroundProgress: BackgroundProgress;
  /** Finished background jobs discovered but not yet ingested into OPFS.
   *  Normally drains immediately; a job lingers here only if its copy
   *  failed (native record unacknowledged, so retry is safe). */
  pendingHandoffJobs: PendingHandoffJob[];

  setCompiling: (isCompiling: boolean) => void;
  setPhase: (phase: string) => void;
  setThermalThrottling: (throttling: boolean) => void;

  /**
   * Discovers the durable background-job record and dispatches on its state.
   * Called eagerly on app cold boot AND on every 'backgroundCompile' event —
   * the event is only a doorbell; this re-query is the single code path, and
   * it is idempotent (the native record persists until acknowledged, and
   * ingestion is re-entrancy-guarded per jobId).
   *
   * On the web (no native shell) queryBackgroundJob rejects with Capacitor's
   * "not implemented" — swallowed here, background compiles are native-only.
   */
  discoverBackgroundJobs: () => Promise<void>;

  /**
   * The cross-bridge handoff for one finished job:
   *   1. Stream-copy the sandbox archive into OPFS in bounded chunks
   *      (opfsMover; byte progress → handoffProgress ref sink).
   *   2. Only after the OPFS writable is closed and byte-verified,
   *      acknowledgeBackgroundJob(jobId) releases the native temp file.
   *   3. Hot-swap the live map via useMapStore.setActiveRegion — MapView's
   *      activeRegion effect drives loadOfflineRegion() in place, without
   *      tearing down the WebGL canvas.
   *
   * On copy failure the native record is NOT acknowledged, so the job
   * survives in pendingHandoffJobs and the archive survives on disk — the
   * next discovery retries from scratch (the OPFS copy is idempotent).
   */
  ingestHandoffJob: (job: PendingHandoffJob) => Promise<void>;
  /**
   * Called once a compile job's terminal `compilationStatus` event reports
   * `state: 'finished'`. Copies the job's `.pmtiles` archive out of native
   * storage into OPFS, then hot-swaps MapLibre's active basemap source by
   * writing `useMapStore`'s `activeRegion` — MapView's existing
   * `loadOfflineRegion` effect picks that up and swaps the live source
   * without tearing down the WebGL context.
   *
   * Failures are logged and swallowed rather than thrown: this is invoked
   * fire-and-forget from a synchronous native event listener (see App.tsx),
   * so there is no caller left to catch a rejection.
   */
  handleJobFinished: (jobId: string) => Promise<void>;
}

/**
 * Low-frequency compilation state (phase transitions, job status). Per-block
 * byte/percentage telemetry is intentionally excluded — that data streams at
 * 50-100 events/sec from the native bridge and must bypass React state
 * entirely (rAF + ref) to avoid VDOM thrashing.
 */
export const useCompilerStore = create<CompilerState>((set, get) => ({
  isCompiling: false,
  currentPhase: '',
  thermalThrottling: false,
  isTransferringToOPFS: false,
  isBackgroundCompiling: false,
  backgroundProgress: IDLE_PROGRESS,
  pendingHandoffJobs: [],

  setCompiling: (isCompiling) => set({ isCompiling }),
  setPhase: (currentPhase) => set({ currentPhase }),
  setThermalThrottling: (thermalThrottling) => set({ thermalThrottling }),

  discoverBackgroundJobs: async () => {
    let record;
    try {
      record = await MapCompiler.queryBackgroundJob();
    } catch {
      // Web dev browser: no native bridge, nothing to discover.
      return;
    }

    switch (record.state) {
      case 'idle':
        set({ isBackgroundCompiling: false });
        return;

      case 'pending':
        // Queued or mid-window; the durable checkpoint makes any partial
        // work resumable, so there is nothing for JS to do but reflect it.
        set({ isBackgroundCompiling: true });
        return;

      case 'failed': {
        const reason = record.reason ?? 'Background compile failed.';
        console.error(`[compilerStore] Background job "${record.jobId}" failed: ${reason}`);
        set({
          isBackgroundCompiling: false,
          backgroundProgress: { stage: 'error', jobId: record.jobId ?? null, error: reason },
        });
        // Failure is now surfaced — release the record so it doesn't
        // re-report on every boot. Fatal failures are not retried (Surface
        // v1 contract: bad input / corrupt state / disk).
        if (record.jobId) {
          await MapCompiler.acknowledgeBackgroundJob({ jobId: record.jobId }).catch((err) => {
            console.error('[compilerStore] Failed to acknowledge failed job:', err);
          });
        }
        return;
      }

      case 'finished': {
        set({ isBackgroundCompiling: false });
        if (!record.jobId || !record.archivePath) {
          console.error('[compilerStore] Finished record missing jobId/archivePath:', record);
          return;
        }
        const job: PendingHandoffJob = {
          jobId: record.jobId,
          archivePath: record.archivePath,
          bytesTotal: record.bytesWritten ?? 0,
          blocksTotal: record.blocksTotal ?? 0,
        };
        if (!get().pendingHandoffJobs.some((j) => j.jobId === job.jobId)) {
          set((s) => ({ pendingHandoffJobs: [...s.pendingHandoffJobs, job] }));
        }
        await get().ingestHandoffJob(job);
        return;
      }
    }
  },

  ingestHandoffJob: async (job) => {
    if (ingestingJobIds.has(job.jobId)) return;
    ingestingJobIds.add(job.jobId);

    // The compiled archive becomes a job-scoped OPFS file, hot-swapped in via
    // loadOfflineRegion — NOT written over the live default file: the mapData
    // worker holds an exclusive SyncAccessHandle on the bound basemap for the
    // worker's lifetime, so a main-thread createWritable() on that same name
    // would throw NoModificationAllowedError (and a same-name swap is a no-op
    // in loadOfflineRegion by design).
    const opfsFilename = `${job.jobId}.pmtiles`;

    set({ backgroundProgress: { stage: 'copying', jobId: job.jobId, error: null } });

    try {
      // Seed the progress denominator from the archive's REAL on-disk size
      // (P-FE.C2, closes tracker B006): the record's bytesTotal carries the
      // engine's LOGICAL accounting (index bytes + payload), which
      // overshoots the archive and made the bar's initial total wrong. A
      // stat failure lands in the catch below like any other copy failure.
      const { size: archiveBytes } = await Filesystem.stat({ path: job.archivePath });
      resetHandoffProgress(archiveBytes);

      await moveNativeFileToOPFS({
        nativeFilePath: job.archivePath,
        opfsFilename,
        // High-frequency path: absolute byte counts go to the ref sink only;
        // BackgroundHandoffBar paints them via its own rAF loop.
        onProgress: reportHandoffProgress,
      });

      // moveNativeFileToOPFS resolves only after writable.close() succeeded
      // and the byte count verified against the source — the OPFS copy is
      // durable, so the native temp archive can now be released.
      await MapCompiler.acknowledgeBackgroundJob({ jobId: job.jobId });

      set((s) => ({
        backgroundProgress: { stage: 'swapping', jobId: job.jobId, error: null },
        pendingHandoffJobs: s.pendingHandoffJobs.filter((j) => j.jobId !== job.jobId),
      }));

      // Hot-swap: MapView's activeRegion effect calls loadOfflineRegion(),
      // which swaps tile sources in place (setUrl) — the WebGL canvas stays
      // mounted throughout. Background jobs compile the basemap only, so the
      // shared default terrain archive is kept.
      useMapStore.getState().setActiveRegion({
        regionLabel: job.jobId,
        basemapFile: opfsFilename,
        terrainFile: DEFAULT_TERRAIN_FILE,
      });

      set({ backgroundProgress: { stage: 'done', jobId: job.jobId, error: null } });
      // Let the "region ready" confirmation breathe, then clear the banner.
      setTimeout(() => {
        const current = get().backgroundProgress;
        if (current.stage === 'done' && current.jobId === job.jobId) {
          set({ backgroundProgress: IDLE_PROGRESS });
        }
      }, 4_000);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.error(`[compilerStore] Handoff of job "${job.jobId}" failed:`, err);
      // No acknowledge: the native record + archive survive, and the job
      // stays in pendingHandoffJobs — the next discovery retries the copy.
      set({ backgroundProgress: { stage: 'error', jobId: job.jobId, error: message } });
    } finally {
      ingestingJobIds.delete(job.jobId);
    }
  },

  handleJobFinished: async (jobId) => {
    set({ isTransferringToOPFS: true });
    try {
      const { uri } = await Filesystem.getUri({
        path: nativeArchiveRelativePath(jobId),
        directory: Directory.Data,
      });
      // Size of the sandbox archive, captured BEFORE the copy: the
      // reference for the post-copy verification below.
      const { size: sandboxBytes } = await Filesystem.stat({ path: uri });

      const { opfsFilename } = await moveNativeFileToOPFS({
        nativeFilePath: uri,
        opfsFilename: `${jobId}.pmtiles`,
      });

      // Independent post-copy verification (P9.C7, closes D008): the mover
      // already byte-verifies its own writes, but the sandbox copy is only
      // released against a second opinion — the OPFS destination's actual
      // file size must equal the sandbox archive's size.
      const root = await navigator.storage.getDirectory();
      const opfsFile = await (await root.getFileHandle(opfsFilename)).getFile();
      if (opfsFile.size !== sandboxBytes) {
        // Keep the sandbox copy (retry stays possible) and do NOT bind a
        // size-mismatched archive as the live basemap.
        console.error(
          `[compilerStore] BUG: OPFS size mismatch after copy of "${jobId}" — ` +
            `sandbox ${sandboxBytes} bytes vs OPFS ${opfsFile.size} bytes; ` +
            `keeping the sandbox archive and skipping the hot-swap.`,
        );
        return;
      }

      // Verified durable in OPFS — the sandbox copy is now redundant.
      // A failed delete is the pre-fix status quo (a leak), never fatal.
      await Filesystem.deleteFile({ path: uri }).catch((err: unknown) => {
        console.error(
          `[compilerStore] Sandbox archive delete failed for "${jobId}" (leaks the copy, non-fatal):`,
          err,
        );
      });

      useMapStore.getState().setActiveRegion({
        regionLabel: jobId,
        basemapFile: opfsFilename,
        terrainFile: DEFAULT_TERRAIN_FILE,
      });
    } catch (err) {
      console.error(`[compilerStore] Failed to move job "${jobId}" into OPFS:`, err);
    } finally {
      set({ isTransferringToOPFS: false });
    }
  },
}));
