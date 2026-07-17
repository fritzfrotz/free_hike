/**
 * handoffProgress.ts — imperative byte-progress bus for the background-job
 * OPFS ingestion (P9.C1).
 *
 * The copy loop (compilerStore.ingestHandoffJob → opfsMover) reports absolute
 * byte counts once per native chunk. Per the app-wide rule for high-frequency
 * telemetry, those updates must never touch React or Zustand: this module is
 * a plain mutable slot the producer writes into and the one consumer
 * (BackgroundHandoffBar) drains from inside a requestAnimationFrame loop,
 * painting the DOM directly.
 *
 * Deliberately not a Zustand store and not an event emitter — one producer,
 * at most one consumer, no history, no subscriptions to leak.
 */

export interface HandoffProgressSnapshot {
  bytesWritten: number;
  totalBytes: number;
}

const snapshot: HandoffProgressSnapshot = { bytesWritten: 0, totalBytes: 0 };

/** Producer side: zero the counters ahead of a new copy. */
export function resetHandoffProgress(totalBytes: number): void {
  snapshot.bytesWritten = 0;
  snapshot.totalBytes = totalBytes;
}

/** Producer side: absolute progress after a chunk lands in OPFS. */
export function reportHandoffProgress(bytesWritten: number, totalBytes: number): void {
  snapshot.bytesWritten = bytesWritten;
  snapshot.totalBytes = totalBytes;
}

/**
 * Consumer side: read the current counters. Returns the live object (never a
 * copy) — callers must treat it as read-only and must not retain mutations.
 */
export function readHandoffProgress(): Readonly<HandoffProgressSnapshot> {
  return snapshot;
}
