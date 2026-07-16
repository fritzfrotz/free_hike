import { create } from 'zustand';
import { Directory, Filesystem } from '@capacitor/filesystem';
import { moveNativeFileToOPFS } from '../services/opfsMover';
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

  setCompiling: (isCompiling: boolean) => void;
  setPhase: (phase: string) => void;
  setThermalThrottling: (throttling: boolean) => void;
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
export const useCompilerStore = create<CompilerState>((set) => ({
  isCompiling: false,
  currentPhase: '',
  thermalThrottling: false,
  isTransferringToOPFS: false,

  setCompiling: (isCompiling) => set({ isCompiling }),
  setPhase: (currentPhase) => set({ currentPhase }),
  setThermalThrottling: (thermalThrottling) => set({ thermalThrottling }),

  handleJobFinished: async (jobId) => {
    set({ isTransferringToOPFS: true });
    try {
      const { uri } = await Filesystem.getUri({
        path: nativeArchiveRelativePath(jobId),
        directory: Directory.Data,
      });

      const { opfsFilename } = await moveNativeFileToOPFS({
        nativeFilePath: uri,
        opfsFilename: `${jobId}.pmtiles`,
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
