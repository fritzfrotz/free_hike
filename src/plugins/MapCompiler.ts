// SPDX-License-Identifier: Apache-2.0
/**
 * MapCompiler — JS-side interface to the Layer 2 Capacitor native plugin
 * (MapCompilerPlugin.swift / MapCompilerPlugin.kt), which wraps the UniFFI
 * bridge into the freehike-core Rust compiler.
 *
 * Surface v1 (suspendable state machine): the native layer drives the
 * budget-yield loop — `compile_chunk` slices re-invoked while the Rust
 * engine returns Yielded, resume state owned durably by the engine on disk.
 * From JS, `startJob` is therefore a single long-lived promise that resolves
 * with the terminal status, while `compilationProgress` (per-block) and
 * `compilationStatus` (per-slice boundary) events stream in.
 *
 * On the web (dev browser, no native shell) every method rejects with
 * Capacitor's "not implemented" error — callers must handle that path.
 */

import { registerPlugin } from '@capacitor/core';
import type { PluginListenerHandle } from '@capacitor/core';

/** Payload of the 'compilationProgress' event (one per processed block). */
export interface CompilationProgressEvent {
  /** 0–100 across the whole job (not the slice). */
  percentage: number;
  /** Human-readable phase label, e.g. "pass1: indexing nodes (12/62)". */
  status: string;
}

/** Payload of the 'compilationStatus' event (one per slice boundary). */
export interface CompilationStatusEvent {
  state: 'yielded' | 'finished' | 'failed' | 'cancelled';
  jobId: string;
  /** Number of compile_chunk slices executed so far for this job. */
  slices: number;
}

/** Terminal result resolved by startJob. */
export interface StartJobResult {
  status: 'finished' | 'failed' | 'cancelled';
  jobId: string;
  slices: number;
  /** Present when status === 'finished'. */
  blocksTotal?: number;
  bytesWritten?: number;
  /** Present when status === 'failed'. */
  reason?: string;
}

/**
 * Lifecycle of the ONE durable background-job record (native PendingJobStore:
 * SharedPreferences on Android, atomic-rename JSON on iOS). Single-job by
 * design — Surface v1 compiles one region at a time.
 */
export type BackgroundJobState = 'idle' | 'pending' | 'finished' | 'failed';

/** Result of queryBackgroundJob — resume-time discovery of the durable record. */
export interface BackgroundJobQueryResult {
  state: BackgroundJobState;
  /** Absent only when state === 'idle'. */
  jobId?: string;
  /**
   * Present when state === 'finished': absolute path of the compiled
   * `.pmtiles` inside the native app sandbox (NOT OPFS — the JS layer owns
   * the stream-copy across that seam, then acknowledges).
   */
  archivePath?: string;
  blocksTotal?: number;
  bytesWritten?: number;
  /** Present when state === 'failed'. */
  reason?: string;
}

/**
 * Payload of the 'backgroundCompile' event, emitted when a background window
 * (BGProcessingTask / WorkManager) reaches a terminal state while a WebView
 * is alive. Headless runs emit nothing — cold-boot discovery via
 * queryBackgroundJob is the always-correct path; treat this event as a
 * doorbell to re-run that discovery, not as a data source.
 */
export interface BackgroundCompileEvent {
  state: 'finished' | 'failed';
  jobId: string;
  archivePath?: string;
  blocksTotal?: number;
  bytesWritten?: number;
  reason?: string;
}

export interface StartJobOptions {
  /** "west,south,east,north" in WGS84 degrees. Required. */
  bbox: string;
  /** Stable ID enables resume across app restarts; defaults to a UUID. */
  jobId?: string;
  /** Zoom range to compile (defaults 5–14). */
  minZoom?: number;
  maxZoom?: number;
  /**
   * Per-slice time budget in ms (default 250). Small values force frequent
   * Yielded checkpoints — useful for exercising the resume machinery.
   */
  budgetMs?: number;
}

export interface MapCompilerPlugin {
  /**
   * Runs a compile job to completion via the native budget-yield loop.
   * Resolves with the terminal status; progress/status events stream while
   * it runs. Safe across process death: re-invoking with the same jobId
   * resumes from the engine's durable checkpoint.
   */
  startJob(options: StartJobOptions): Promise<StartJobResult>;

  /** Requests cancellation of the active job (honored between slices). */
  cancelJob(): Promise<{ requested: boolean }>;

  /**
   * Cold-start resume detection: returns the engine's durable checkpoint for
   * a job if one survives on disk (e.g. after the OS killed the process
   * mid-compilation). Pair with startJob({ jobId }) to resume it.
   */
  queryJob(options: { jobId: string }): Promise<{
    found: boolean;
    phase?: string;
    nextBlock?: number;
    pbfByteOffset?: number;
    bytesWritten?: number;
  }>;

  /**
   * Queues a compile job for background execution (iOS BGProcessingTask /
   * Android WorkManager + dataSync FGS). The OS decides when the window
   * opens (external power required); results are discovered via
   * queryBackgroundJob or the 'backgroundCompile' event.
   */
  enqueueBackgroundJob(options: {
    bbox: string;
    jobId?: string;
    minZoom?: number;
    maxZoom?: number;
  }): Promise<{ scheduled: boolean; jobId: string }>;

  /**
   * Resume-time discovery of the durable background-job record. Call on
   * every cold boot AND on each 'backgroundCompile' event — the record, not
   * the event, is the source of truth (a headless background run has no
   * WebView to emit to).
   */
  queryBackgroundJob(): Promise<BackgroundJobQueryResult>;

  /**
   * Releases a terminal (finished/failed) record — deleting the sandbox
   * archive and purging leftover job state — once the JS layer has durably
   * imported the archive into OPFS (writable closed + byte-count verified)
   * or surfaced the failure. Targeted: rejects if `jobId` doesn't match the
   * stored record (stale ack) or if the record is still 'pending'. Resolves
   * `{ cleared: false }` when the store is already empty (idempotent retry).
   */
  acknowledgeBackgroundJob(options: { jobId: string }): Promise<{ cleared: boolean }>;

  /**
   * Hard-cancels the queued/running background job: stops the WorkManager
   * chain, clears the durable record, and wipes the job's disk footprint
   * (checkpoint, redb index, scratch files, any assembled archive). The
   * running slice stops at its next boundary (≤ a few seconds). Rejects if
   * the stored record is terminal — acknowledge that instead.
   */
  cancelBackgroundJob(): Promise<{ cancelled: boolean; jobId?: string }>;

  /** Smoke test: proves the Rust core is linked and callable. */
  getEngineVersion(): Promise<{ version: string }>;

  /**
   * Debug: asks the Rust core to emit `steps` synthetic progress ticks, each
   * delivered as a 'compilationProgress' event. Resolves the count sent.
   */
  emitTestProgress(options: { steps: number }): Promise<{ sent: number }>;

  addListener(
    eventName: 'compilationProgress',
    listenerFunc: (event: CompilationProgressEvent) => void,
  ): Promise<PluginListenerHandle>;

  addListener(
    eventName: 'compilationStatus',
    listenerFunc: (event: CompilationStatusEvent) => void,
  ): Promise<PluginListenerHandle>;

  addListener(
    eventName: 'backgroundCompile',
    listenerFunc: (event: BackgroundCompileEvent) => void,
  ): Promise<PluginListenerHandle>;

  removeAllListeners(): Promise<void>;
}

export const MapCompiler = registerPlugin<MapCompilerPlugin>('MapCompiler');
