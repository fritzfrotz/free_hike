// SPDX-License-Identifier: Apache-2.0
/**
 * opfsRetry.ts — bounded retry for exclusive OPFS sync-handle acquisition
 * (P-FE.C2, tracker B005).
 *
 * The failure mode: on a dev-server reload (and, on some WebViews, a fast
 * app relaunch) the PREVIOUS worker still holds the exclusive
 * SyncAccessHandle for a beat while the browser tears it down. The new
 * worker's `createSyncAccessHandle()` then throws
 * `NoModificationAllowedError`, boot dies, and the map's 'load' event
 * never fires. The lock always clears once the old worker finishes
 * terminating — so a short, bounded retry bridges the gap; anything that
 * still holds the lock after the budget is a REAL second holder and must
 * fail loudly (the B005-class exclusivity rule is load-bearing; waiting
 * forever would deadlock a genuine conflict).
 */

/** Error names thrown when another handle holds the exclusive lock. */
const LOCK_ERROR_NAMES = new Set(['NoModificationAllowedError', 'InvalidStateError']);

export interface SyncHandleRetryOptions {
  /** Total acquisition attempts (default 10). */
  attempts?: number;
  /** Delay between attempts in ms (default 150 — bridges worker teardown). */
  delayMs?: number;
  /** Injectable sleeper for deterministic tests. */
  sleep?: (ms: number) => Promise<void>;
}

const defaultSleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/**
 * Acquires `fileHandle.createSyncAccessHandle()`, retrying lock-shaped
 * failures (`NoModificationAllowedError` / `InvalidStateError`) up to the
 * attempt budget. Any other error rethrows immediately — a missing file or
 * quota failure will not fix itself by waiting.
 */
export async function createSyncHandleWithRetry<H>(
  fileHandle: { createSyncAccessHandle(): Promise<H> },
  filename: string,
  opts: SyncHandleRetryOptions = {},
): Promise<H> {
  const attempts = opts.attempts ?? 10;
  const delayMs = opts.delayMs ?? 150;
  const sleep = opts.sleep ?? defaultSleep;

  let lastError: unknown;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    try {
      return await fileHandle.createSyncAccessHandle();
    } catch (err) {
      if (!(err instanceof Error) || !LOCK_ERROR_NAMES.has(err.name)) throw err;
      lastError = err;
      if (attempt < attempts) await sleep(delayMs);
    }
  }
  throw new Error(
    `OPFS sync handle for "${filename}" still locked after ${attempts} attempts ` +
      `(${attempts * delayMs}ms) — another live holder owns it: ${String(lastError)}`,
  );
}
