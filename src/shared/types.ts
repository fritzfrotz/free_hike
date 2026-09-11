// SPDX-License-Identifier: Apache-2.0
// ─── Worker RPC contracts (mapData.worker.ts ⇄ main thread) ──────────────────

export type WorkerRequestType =
  | 'MAP_INIT'
  | 'MAP_READ_BYTES'
  // Phase 11: Dynamic multi-file OPFS source routing
  | 'LOAD_OFFLINE_REGION'
  | 'MAP_CLOSE';

export type WorkerResponseType =
  | 'SUCCESS'
  | 'ERROR'
  | 'MAP_INIT_SUCCESS'
  | 'MAP_BYTES_RESPONSE'
  // Phase 11: Dynamic multi-file OPFS source routing
  | 'LOAD_OFFLINE_REGION_SUCCESS';

export interface WorkerRequestMessage {
  id: string;
  type: WorkerRequestType;
  payload: any; // eslint-disable-line @typescript-eslint/no-explicit-any
}

export interface WorkerResponseMessage {
  id: string;
  type: WorkerResponseType;
  payload: any; // eslint-disable-line @typescript-eslint/no-explicit-any
  error?: string;
}

/** Payload returned by mapData.worker.ts on MAP_INIT_SUCCESS. */
export interface MapInitSuccessPayload {
  /** Byte size of the first (primary) file in the requested filename set. */
  size: number;
  /**
   * Filenames that could not be provisioned from /local assets (e.g. a 404
   * on the static asset) and were left as empty OPFS stubs.
   */
  provisionFailures: string[];
}
