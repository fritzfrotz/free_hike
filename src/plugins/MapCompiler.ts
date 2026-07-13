/**
 * MapCompiler — JS-side interface to the Layer 2 Capacitor native plugin
 * (MapCompilerPlugin.swift / MapCompilerPlugin.kt), which wraps the UniFFI
 * bridge into the freehike-core Rust compiler.
 *
 * Tri-layer path: this module → Capacitor bridge → Swift/Kotlin plugin →
 * UniFFI → Rust. Bulk data never crosses this boundary: a bbox string goes
 * down, small JSON envelopes and progress events come back up.
 *
 * On the web (dev browser, no native shell) every method rejects with
 * Capacitor's "not implemented" error — callers must handle that path.
 */

import { registerPlugin } from '@capacitor/core';
import type { PluginListenerHandle } from '@capacitor/core';

/** Payload of the 'compilationProgress' event emitted by the native layer. */
export interface CompilationProgressEvent {
  /** 0–100. */
  percentage: number;
  /** Human-readable phase label (e.g. "pass1: indexing nodes"). */
  status: string;
}

export interface MapCompilerPlugin {
  /**
   * Walking-skeleton compile entry point. `bbox` is "west,south,east,north"
   * in WGS84 degrees. Resolves the Rust core's JSON status envelope verbatim.
   */
  startJob(options: { bbox: string }): Promise<{ result: string }>;

  /**
   * Cancels the active compile job.
   *
   * NOT YET IMPLEMENTED natively — part of the planned Phase 7 chunked
   * state-machine surface (`startJob` / `cancelJob` / `queryState`). Calling
   * it today rejects with Capacitor's "method not implemented" error.
   */
  cancelJob(): Promise<void>;

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

  removeAllListeners(): Promise<void>;
}

export const MapCompiler = registerPlugin<MapCompilerPlugin>('MapCompiler');
