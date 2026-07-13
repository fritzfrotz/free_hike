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

  removeAllListeners(): Promise<void>;
}

export const MapCompiler = registerPlugin<MapCompilerPlugin>('MapCompiler');
