// SPDX-License-Identifier: Apache-2.0
/**
 * compileProgress.ts — the background compile's last saved percent (P9.C8),
 * the polled twin of fetchProgress.ts.
 *
 * The Android worker saves the engine's integer percent and a timestamp into
 * the native job record (on integer change or a 5 s heartbeat).
 * discoverBackgroundJobs seeds this slot from that record; while a job is
 * pending, BackgroundHandoffBar re-runs discovery every 3 s and paints
 * compileStateLabel straight to the DOM. Per ARCHITECTURE.md P8 the number
 * never touches React or Zustand.
 *
 * iOS saves no progress (D004), so there the label stays queued.
 */

export interface CompileProgressSnapshot {
  /** Pending job the slot describes; null when no job is pending. */
  jobId: string | null;
  /** Last saved integer percent; null when nothing was saved for this job. */
  percent: number | null;
  /** Device wall-clock ms of that save; null with `percent`. */
  atMs: number | null;
}

/** A save older than this means the worker is not running right now. */
export const PAUSED_AFTER_MS = 15_000;

export type CompileStateWord = 'queued' | 'compiling' | 'paused';

const snapshot: CompileProgressSnapshot = { jobId: null, percent: null, atMs: null };

/** Producer side: the pending job and its last saved progress (or none). */
export function seedCompileProgress(jobId: string, percent: number | null, atMs: number | null): void {
  snapshot.jobId = jobId;
  snapshot.percent = percent;
  snapshot.atMs = percent === null ? null : atMs;
}

/** Producer side: no job is pending. */
export function clearCompileProgress(): void {
  snapshot.jobId = null;
  snapshot.percent = null;
  snapshot.atMs = null;
}

/** Consumer side: the live object (never a copy) — read-only by contract. */
export function readCompileProgress(): Readonly<CompileProgressSnapshot> {
  return snapshot;
}

/** The state word and bar text for a snapshot at `nowMs`. Pure. */
export function compileStateLabel(
  snap: Readonly<CompileProgressSnapshot>,
  nowMs: number,
): { word: CompileStateWord; text: string } {
  if (snap.percent === null || snap.atMs === null) {
    return { word: 'queued', text: 'Queued — runs while the device is charging' };
  }
  if (nowMs - snap.atMs > PAUSED_AFTER_MS) {
    return { word: 'paused', text: `Paused at ${snap.percent}% — continues while charging` };
  }
  return { word: 'compiling', text: `Compiling ${snap.percent}%` };
}
