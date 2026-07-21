// SPDX-License-Identifier: Apache-2.0
/**
 * mapData.worker.ts
 *
 * Responsibilities:
 *   MAP_INIT             – open SyncAccessHandles for the initial OPFS file set
 *                          (filenames supplied in the payload; falls back to
 *                          'hike.pmtiles' for backward compatibility).
 *   MAP_READ_BYTES       – serve arbitrary byte ranges to WorkerPMTilesSource.
 *                          The request payload now includes a `filename` field
 *                          so the correct SyncAccessHandle is selected from the
 *                          per-file map \u2014 multiple files can be read concurrently.
 *   LOAD_OFFLINE_REGION  – open SyncAccessHandles for newly chosen region files
 *                          (called by MapView.loadOfflineRegion before swapping
 *                          MapLibre source URLs).
 *   DOWNLOAD_REGION_REQUEST \u2013 receive zero-copy ArrayBuffer pair from main thread,
 *                          write them to OPFS entirely off-thread, then open
 *                          the freshly written files so subsequent reads work.
 *
 * Storage strategy (Phase 11)
 * \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
 * Phase 1-10 kept a single SyncAccessHandle in a module-level variable.
 * Phase 11 replaces that with a Map<filename, FileSystemSyncAccessHandle>.
 * Every MAP_READ_BYTES request routes to the correct handle via payload.filename.
 * All handles opened during MAP_INIT or LOAD_OFFLINE_REGION are kept open for
 * the lifetime of the worker so repeated reads incur zero open/close overhead.
 */

import { createSyncHandleWithRetry } from './opfsRetry';
import type {
  WorkerRequestMessage,
  WorkerResponseMessage,
  DownloadRegionRequestPayload,
  DownloadRegionSuccessPayload,
  MapInitSuccessPayload,
} from '../shared/types';

// ---------------------------------------------------------------------------
// Per-file SyncAccessHandle registry
// ---------------------------------------------------------------------------

/**
 * All currently open OPFS handles, keyed by filename.
 * Every file opened here stays open for the lifetime of the worker so repeated
 * byte-range reads have zero open/close overhead.
 */
const handles = new Map<string, FileSystemSyncAccessHandle>();

/** Open (or return the already-open) SyncAccessHandle for a given OPFS file. */
async function getHandle(filename: string): Promise<FileSystemSyncAccessHandle> {
  const cached = handles.get(filename);
  if (cached) return cached;

  const root       = await navigator.storage.getDirectory();
  const fileHandle = await root.getFileHandle(filename, { create: true });
  // Bounded retry (P-FE.C2, closes tracker B005): a reload-killed previous
  // worker can hold the exclusive lock for a beat while the browser tears
  // it down; retrying bridges that gap, while a REAL second holder still
  // fails loudly after the budget.
  const handle     = await createSyncHandleWithRetry(fileHandle, filename);
  handles.set(filename, handle);
  console.log(`[mapData.worker] Opened SyncAccessHandle for "${filename}" (${handle.getSize()} bytes)`);
  return handle;
}

/** Open handles for every filename in the list, ignoring already-open ones. */
async function openHandles(filenames: string[]): Promise<void> {
  await Promise.all(filenames.map(getHandle));
}

/** Close and release all active FileSystemSyncAccessHandle file locks. */
function closeAllHandles(): void {
  for (const [filename, handle] of handles.entries()) {
    try {
      handle.close();
      console.log(`[mapData.worker] Closed SyncAccessHandle for "${filename}"`);
    } catch (err) {
      console.warn(`[mapData.worker] Failed to close handle for "${filename}":`, err);
    }
  }
  handles.clear();
}

// ---------------------------------------------------------------------------
// Message dispatcher
// ---------------------------------------------------------------------------

self.addEventListener('message', async (event: MessageEvent<WorkerRequestMessage>) => {
  const { id, type, payload } = event.data;

  try {

    // ── MAP_INIT ────────────────────────────────────────────────────────────
    if (type === 'MAP_INIT') {
      // Aggressively close any existing open handles first to release the locks
      // on the OS files, preventing NoModificationAllowedError concurrency collision.
      closeAllHandles();

      // Accept an optional filenames array from the caller.
      // Fall back to the legacy single-file path for backward compatibility.
      const filenames: string[] =
        Array.isArray(payload?.filenames) && payload.filenames.length > 0
          ? payload.filenames
          : ['hike.pmtiles'];

      const provisionFailures = await initFiles(filenames);

      // Report the size of the first file for the UI status bar.
      const primaryHandle = handles.get(filenames[0]);
      const size = primaryHandle ? primaryHandle.getSize() : 0;

      const response: WorkerResponseMessage = {
        id,
        type: 'MAP_INIT_SUCCESS',
        payload: { size, provisionFailures } satisfies MapInitSuccessPayload,
      };
      self.postMessage(response);
      return;
    }

    // ── MAP_CLOSE ────────────────────────────────────────────────────────────
    if (type === 'MAP_CLOSE') {
      closeAllHandles();
      const response: WorkerResponseMessage = {
        id,
        type: 'SUCCESS',
        payload: null,
      };
      self.postMessage(response);
      return;
    }

    // ── MAP_READ_BYTES ───────────────────────────────────────────────────────
    if (type === 'MAP_READ_BYTES') {
      // payload.filename routes to the right handle; fall back to the first
      // open handle for callers that predate the filename field.
      const filename = (payload?.filename as string | undefined)
        ?? handles.keys().next().value;

      if (!filename) {
        throw new Error('MAP_READ_BYTES: no filename supplied and no open handles.');
      }

      const handle = handles.get(filename)
        ?? await getHandle(filename); // lazy-open if not yet warmed

      const { offset, length } = payload as { filename: string; offset: number; length: number };
      const fileSize   = handle.getSize();
      const readLength = Math.min(length, fileSize - offset);

      if (readLength <= 0) {
        const empty = new ArrayBuffer(0);
        const response: WorkerResponseMessage = {
          id,
          type: 'MAP_BYTES_RESPONSE',
          payload: { buffer: empty, bytesRead: 0, offset, length },
        };
        self.postMessage(response, [empty]);
        return;
      }

      const buffer    = new Uint8Array(readLength);
      const bytesRead = handle.read(buffer, { at: offset });

      const finalBuffer: ArrayBuffer =
        bytesRead < readLength ? buffer.buffer.slice(0, bytesRead) : buffer.buffer;

      const response: WorkerResponseMessage = {
        id,
        type: 'MAP_BYTES_RESPONSE',
        payload: { buffer: finalBuffer, bytesRead, offset, length },
      };
      self.postMessage(response, [finalBuffer]);
      return;
    }

    // ── LOAD_OFFLINE_REGION ─────────────────────────────────────────────────
    // Called by MapView.loadOfflineRegion() before swapping MapLibre source URLs.
    // Opens SyncAccessHandles for the newly selected files so subsequent
    // MAP_READ_BYTES requests are served without a lazy-open latency spike.
    if (type === 'LOAD_OFFLINE_REGION') {
      const filenames: string[] = Array.isArray(payload?.filenames)
        ? payload.filenames
        : [];

      if (filenames.length === 0) {
        throw new Error('LOAD_OFFLINE_REGION: payload.filenames must be a non-empty array.');
      }

      await openHandles(filenames);
      console.log(`[mapData.worker] LOAD_OFFLINE_REGION: handles ready for [${filenames.join(', ')}]`);

      const response: WorkerResponseMessage = {
        id,
        type: 'LOAD_OFFLINE_REGION_SUCCESS',
        payload: { filenames },
      };
      self.postMessage(response);
      return;
    }

    // ── DOWNLOAD_REGION_REQUEST ─────────────────────────────────────────────
    if (type === 'DOWNLOAD_REGION_REQUEST') {
      const { pmtilesBuffer, routingBuffer, regionLabel } =
        payload as DownloadRegionRequestPayload;

      const result = await writeRegionToOPFS(pmtilesBuffer, routingBuffer, regionLabel);

      const response: WorkerResponseMessage = {
        id,
        type: 'DOWNLOAD_REGION_SUCCESS',
        payload: result satisfies DownloadRegionSuccessPayload,
      };
      self.postMessage(response);
      return;
    }

    // ── Unknown type ─────────────────────────────────────────────────────────
    const response: WorkerResponseMessage = {
      id,
      type: 'ERROR',
      payload: null,
      error: `Unknown request type: ${type}`,
    };
    self.postMessage(response);

  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : 'Unknown worker error';
    console.error('[mapData.worker] Error handling message:', type, err);
    self.postMessage({
      id,
      type: type === 'DOWNLOAD_REGION_REQUEST' ? 'DOWNLOAD_REGION_ERROR' : 'ERROR',
      payload: null,
      error: message,
    } satisfies WorkerResponseMessage);
  }
});

// ---------------------------------------------------------------------------
// MAP_INIT — open (and optionally seed) a set of OPFS files
// ---------------------------------------------------------------------------

/**
 * Opens SyncAccessHandles for each filename in the list.
 * For the primary file ('hike.pmtiles' legacy path) an empty file is seeded
 * with a minimal PMTiles v3 stub header so the PMTiles library never reads
 * zero bytes and crashes during the first parse attempt.
 *
 * @returns Filenames that could not be provisioned (left as empty/stub OPFS
 *          files) so the caller can surface a user-facing error instead of
 *          only logging to the console.
 */
async function initFiles(filenames: string[]): Promise<string[]> {
  const provisionFailures: string[] = [];

  for (const filename of filenames) {
    const handle = await getHandle(filename);

    if (handle.getSize() === 0) {
      if (filename === 'hike.pmtiles') {
        console.log('[mapData.worker] hike.pmtiles is empty — seeding with sample dataset…');
        try {
          const res = await fetch('https://pmtiles.io/stamen_toner(raster)CC-BY+ODbL_z3.pmtiles');
          if (!res.ok) throw new Error(`Fetch failed: ${res.statusText}`);
          const buf = await res.arrayBuffer();
          handle.write(new Uint8Array(buf), { at: 0 });
          handle.flush();
          console.log(`[mapData.worker] Sample PMTiles written. Size: ${handle.getSize()} bytes`);
        } catch (err) {
          console.error('[mapData.worker] Seed fetch failed — writing stub header:', err);
          // Minimal PMTiles v3 magic + version byte so the library does not crash.
          const stub = new Uint8Array(127);
          stub.set([0x50, 0x4D, 0x54, 0x69, 0x6C, 0x65, 0x73, 3]); // "PMTiles" + v3
          handle.write(stub, { at: 0 });
          handle.flush();
          provisionFailures.push(filename);
        }
      } else {
        console.log(`[mapData.worker] "${filename}" is empty — provisioning from local assets /local/${filename}…`);
        try {
          const res = await fetch(`/local/${filename}`);
          if (!res.ok) throw new Error(`Fetch failed: ${res.statusText}`);
          const buf = await res.arrayBuffer();

          // Dev servers (and some static hosts) return HTTP 200 with an HTML
          // fallback page for a missing asset instead of a real 404 — verify
          // the "PMTiles" magic header so a bad fetch doesn't get silently
          // treated as a successful provision.
          const magic = new Uint8Array(buf.slice(0, 7));
          const isPMTiles = String.fromCharCode(...magic) === 'PMTiles';
          if (!isPMTiles) {
            throw new Error(`"${filename}" did not resolve to a valid PMTiles archive (got ${buf.byteLength} bytes — likely a missing/404 asset)`);
          }

          handle.write(new Uint8Array(buf), { at: 0 });
          handle.flush();
          console.log(`[mapData.worker] Provisioned "${filename}" from local assets. Size: ${handle.getSize()} bytes`);
        } catch (err) {
          console.error(`[mapData.worker] Failed to provision "${filename}" from local assets:`, err);
          provisionFailures.push(filename);
        }
      }
    }
  }

  return provisionFailures;
}

// ---------------------------------------------------------------------------
// DOWNLOAD_REGION_REQUEST — write both files to OPFS, open new handles
// ---------------------------------------------------------------------------

/**
 * Writes `pmtilesBuffer` \u2192 `active_map.pmtiles` and
 *         `routingBuffer` \u2192 `active_routing.tar`
 * using createSyncAccessHandle() for maximum off-thread throughput.
 *
 * After writing, the function closes any stale handle for `active_map.pmtiles`
 * and opens a fresh one so subsequent MAP_READ_BYTES requests serve the new
 * region without an explicit LOAD_OFFLINE_REGION call.
 */
async function writeRegionToOPFS(
  pmtilesBuffer: ArrayBuffer,
  routingBuffer: ArrayBuffer,
  regionLabel:   string,
): Promise<DownloadRegionSuccessPayload> {
  const root = await navigator.storage.getDirectory();

  // ── Write PMTiles ─────────────────────────────────────────────────────────
  const pmHandle = await root.getFileHandle('active_map.pmtiles', { create: true });
  const pmAccess = await pmHandle.createSyncAccessHandle();
  pmAccess.truncate(0);
  pmAccess.write(new Uint8Array(pmtilesBuffer), { at: 0 });
  pmAccess.flush();
  const pmtilesBytes = pmAccess.getSize();
  pmAccess.close();

  // ── Write routing tar (may be empty) ─────────────────────────────────────
  let routingBytes = 0;
  if (routingBuffer.byteLength > 0) {
    const tarHandle = await root.getFileHandle('active_routing.tar', { create: true });
    const tarAccess = await tarHandle.createSyncAccessHandle();
    tarAccess.truncate(0);
    tarAccess.write(new Uint8Array(routingBuffer), { at: 0 });
    tarAccess.flush();
    routingBytes = tarAccess.getSize();
    tarAccess.close();
  }

  // ── Swap the registry entry for active_map.pmtiles ───────────────────────
  // Close any cached handle (it points at stale bytes after truncate+write)
  // and open a fresh one so MAP_READ_BYTES is immediately consistent.
  const stale = handles.get('active_map.pmtiles');
  if (stale) {
    try { stale.close(); } catch { /* ignore if already closed */ }
    handles.delete('active_map.pmtiles');
  }
  await getHandle('active_map.pmtiles');

  console.log(
    `[mapData.worker] Region "${regionLabel}" committed to OPFS \u2014 ` +
    `PMTiles: ${pmtilesBytes} B, routing: ${routingBytes} B`,
  );

  return {
    regionLabel,
    pmtilesBytes,
    routingBytes,
    writtenAt: new Date().toISOString(),
  };
}
