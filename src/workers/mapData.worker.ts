/**
 * mapData.worker.ts
 *
 * Responsibilities:
 *   MAP_INIT             – open / seed `hike.pmtiles` in OPFS via SyncAccessHandle
 *   MAP_READ_BYTES       – serve arbitrary byte ranges to WorkerPMTilesSource
 *   DOWNLOAD_REGION_REQUEST – receive zero-copy ArrayBuffer pair from main thread,
 *                             write them as `active_map.pmtiles` and
 *                             `active_routing.tar` inside OPFS entirely off-thread,
 *                             then swap the active SyncAccessHandle to the new file.
 *
 * Storage strategy
 * ────────────────
 * Phase 1-9 used a single SyncAccessHandle bound to `hike.pmtiles`.
 * Phase 10 adds a second file slot (`active_map.pmtiles`) that represents
 * whatever region the user last downloaded.  After a successful write the
 * worker closes the old handle and opens the new file so subsequent
 * MAP_READ_BYTES requests are served from the freshly written region.
 */

import type {
  WorkerRequestMessage,
  WorkerResponseMessage,
  DownloadRegionRequestPayload,
  DownloadRegionSuccessPayload,
} from '../shared/types';

// ---------------------------------------------------------------------------
// Module-level SyncAccessHandle (one active at a time)
// ---------------------------------------------------------------------------

let accessHandle: FileSystemSyncAccessHandle | null = null;

// ---------------------------------------------------------------------------
// Message dispatcher
// ---------------------------------------------------------------------------

self.addEventListener('message', async (event: MessageEvent<WorkerRequestMessage>) => {
  const { id, type, payload } = event.data;

  try {
    // ── MAP_INIT ─────────────────────────────────────────────────────────────
    if (type === 'MAP_INIT') {
      const result = await initMap();
      const response: WorkerResponseMessage = {
        id,
        type: 'MAP_INIT_SUCCESS',
        payload: result,
      };
      self.postMessage(response);
      return;
    }

    // ── MAP_READ_BYTES ────────────────────────────────────────────────────────
    if (type === 'MAP_READ_BYTES') {
      if (!accessHandle) {
        throw new Error('Map storage handle is not initialized. Call MAP_INIT first.');
      }
      const { offset, length } = payload as { offset: number; length: number };
      const fileSize   = accessHandle.getSize();
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
      const bytesRead = accessHandle.read(buffer, { at: offset });

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

    // ── DOWNLOAD_REGION_REQUEST ───────────────────────────────────────────────
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

    // ── Unknown type ──────────────────────────────────────────────────────────
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
// MAP_INIT — open hike.pmtiles, seed it on first run
// ---------------------------------------------------------------------------

async function initMap(): Promise<{ size: number }> {
  if (accessHandle) {
    return { size: accessHandle.getSize() };
  }

  const root       = await navigator.storage.getDirectory();
  const fileHandle = await root.getFileHandle('hike.pmtiles', { create: true });
  accessHandle     = await fileHandle.createSyncAccessHandle();

  let size = accessHandle.getSize();
  if (size === 0) {
    console.log('[mapData.worker] hike.pmtiles is empty — seeding with sample dataset…');
    try {
      const res = await fetch('https://pmtiles.io/stamen_toner(raster)CC-BY+ODbL_z3.pmtiles');
      if (!res.ok) throw new Error(`Fetch failed: ${res.statusText}`);
      const buf = await res.arrayBuffer();
      accessHandle.write(new Uint8Array(buf), { at: 0 });
      accessHandle.flush();
      size = accessHandle.getSize();
      console.log(`[mapData.worker] Sample PMTiles written. Size: ${size} bytes`);
    } catch (err) {
      console.error('[mapData.worker] Seed fetch failed — writing stub header:', err);
      // Minimal PMTiles v3 magic + version byte so the library doesn't crash.
      const stub = new Uint8Array(127);
      stub.set([0x50, 0x4D, 0x54, 0x69, 0x6C, 0x65, 0x73, 3]);   // "PMTiles" + v3
      accessHandle.write(stub, { at: 0 });
      accessHandle.flush();
      size = accessHandle.getSize();
    }
  }

  return { size };
}

// ---------------------------------------------------------------------------
// DOWNLOAD_REGION_REQUEST — write both files to OPFS, swap active handle
// ---------------------------------------------------------------------------

/**
 * Writes `pmtilesBuffer` → `active_map.pmtiles` and
 *         `routingBuffer` → `active_routing.tar`
 * using createSyncAccessHandle() for maximum off-thread throughput.
 *
 * After a successful PMTiles write the function closes the old SyncAccessHandle
 * and opens the newly written file, making subsequent MAP_READ_BYTES requests
 * serve from the freshly downloaded region.
 */
async function writeRegionToOPFS(
  pmtilesBuffer: ArrayBuffer,
  routingBuffer: ArrayBuffer,
  regionLabel:   string,
): Promise<DownloadRegionSuccessPayload> {
  const root = await navigator.storage.getDirectory();

  // ── Write PMTiles ──────────────────────────────────────────────────────────
  const pmHandle   = await root.getFileHandle('active_map.pmtiles', { create: true });
  const pmAccess   = await pmHandle.createSyncAccessHandle();
  // Truncate first to avoid stale trailing bytes from a prior smaller file.
  pmAccess.truncate(0);
  pmAccess.write(new Uint8Array(pmtilesBuffer), { at: 0 });
  pmAccess.flush();
  const pmtilesBytes = pmAccess.getSize();
  pmAccess.close();

  // ── Write routing tar (may be empty) ──────────────────────────────────────
  let routingBytes = 0;
  if (routingBuffer.byteLength > 0) {
    const tarHandle  = await root.getFileHandle('active_routing.tar', { create: true });
    const tarAccess  = await tarHandle.createSyncAccessHandle();
    tarAccess.truncate(0);
    tarAccess.write(new Uint8Array(routingBuffer), { at: 0 });
    tarAccess.flush();
    routingBytes = tarAccess.getSize();
    tarAccess.close();
  }

  // ── Swap the active read handle to the new PMTiles file ──────────────────
  // Close the old handle (if open) so MAP_READ_BYTES now serves the new region.
  if (accessHandle) {
    accessHandle.close();
    accessHandle = null;
  }
  const newHandle  = await root.getFileHandle('active_map.pmtiles', { create: false });
  accessHandle     = await newHandle.createSyncAccessHandle();

  console.log(
    `[mapData.worker] Region "${regionLabel}" committed to OPFS — ` +
    `PMTiles: ${pmtilesBytes} B, routing: ${routingBytes} B`,
  );

  return {
    regionLabel,
    pmtilesBytes,
    routingBytes,
    writtenAt: new Date().toISOString(),
  };
}
