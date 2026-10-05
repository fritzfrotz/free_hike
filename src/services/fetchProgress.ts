// SPDX-License-Identifier: Apache-2.0
/**
 * fetchProgress.ts — imperative progress bus for the native raw-input fetch
 * (P-SOV.C2b), the twin of handoffProgress.ts.
 *
 * The 'fetchProgress' listener (attached for the duration of
 * regionCompiler.ensureInputs) writes the latest Rust callback here; the one
 * consumer (FetchProgressBar) drains it from a requestAnimationFrame loop and
 * paints the DOM directly. Per ARCHITECTURE.md P8, this never touches React
 * or Zustand — one producer, at most one consumer, no history.
 */

export interface FetchProgressSnapshot {
  /** 0–100 across the whole download. */
  percentage: number;
  /** Rust's human-readable line, e.g. "fetch: 50.4 MiB / 403.0 MiB (…)". */
  status: string;
}

const snapshot: FetchProgressSnapshot = { percentage: 0, status: '' };

/** Producer side: zero the slot ahead of a new download. */
export function resetFetchProgress(): void {
  snapshot.percentage = 0;
  snapshot.status = '';
}

/** Producer side: latest native progress event. */
export function reportFetchProgress(percentage: number, status: string): void {
  snapshot.percentage = percentage;
  snapshot.status = status;
}

/**
 * Consumer side: the live object (never a copy) — read-only by contract.
 */
export function readFetchProgress(): Readonly<FetchProgressSnapshot> {
  return snapshot;
}
