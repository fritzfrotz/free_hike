/**
 * opfsMover.ts — copies a file out of native Capacitor storage into OPFS.
 *
 * The Rust compiler (freehike-core) writes its finished `.pmtiles` archive to
 * the app's native filesystem (`archive_path(output_dir, job_id)`), but
 * MapLibre's WorkerPMTilesSource only ever reads from OPFS via a
 * FileSystemSyncAccessHandle held in mapData.worker. This module is the seam
 * between those two storage worlds.
 *
 * Large-file constraint: offline regions can exceed 2GB, so the source file
 * is never materialized whole in JS. `@capacitor/filesystem`'s
 * `readFileInChunks` streams bounded base64 chunks off the native side; each
 * chunk is decoded and written immediately, so peak memory stays proportional
 * to `chunkSizeBytes`, not the archive size.
 *
 * Runs on the MAIN THREAD, not a worker: `Filesystem.readFileInChunks` calls
 * the native bridge, which is only reachable from the WebView's main JS
 * context, while `createSyncAccessHandle()` (used everywhere else in this
 * app) is worker-only per the File System Access spec. The main-thread
 * counterpart — `FileSystemWritableFileStream` via `createWritable()` — is
 * the one OPFS write primitive available here, hence its use in place of the
 * SyncAccessHandle pattern the rest of the app follows.
 */

import { Capacitor } from '@capacitor/core';
import { Filesystem } from '@capacitor/filesystem';
import type { ReadFileResult } from '@capacitor/filesystem';

/** 8 MiB: bounds the transient base64 string + decoded buffer per chunk to
 *  well under mobile memory pressure thresholds, regardless of total archive
 *  size, while keeping chunk count (and progress granularity) reasonable for
 *  multi-GB regions (~256 chunks for a 2GB archive). */
const DEFAULT_CHUNK_BYTES = 8 * 1024 * 1024;

export interface OpfsMoveOptions {
  /** Absolute native path/URI to the source file (e.g. a Rust-produced
   *  `file:///.../job_123.pmtiles`). Passed to Filesystem verbatim, with no
   *  `directory` option, so it is resolved as an absolute path. */
  nativeFilePath: string;
  /** Destination filename at the OPFS root (e.g. 'job_123.pmtiles'). */
  opfsFilename: string;
  /** Bytes read per native chunk. Defaults to 8 MiB. */
  chunkSizeBytes?: number;
  /** Invoked after each chunk is durably written to OPFS. */
  onProgress?: (bytesWritten: number, totalBytes: number) => void;
}

export interface OpfsMoveResult {
  opfsFilename: string;
  bytesWritten: number;
}

/** Decodes one base64 chunk into raw bytes. Bounded by chunkSizeBytes, so
 *  this never touches more than one chunk's worth of memory at a time. */
function base64ToBytes(base64: string): Uint8Array<ArrayBuffer> {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Wraps Filesystem.readFileInChunks' callback API in a Promise that settles
 * once the native side signals end-of-file (a null or empty chunk) or an
 * error. `onChunk` is invoked once per chunk in file order; the returned
 * promise waits for all `onChunk` calls to resolve, not just for native
 * reading to finish, so a slow OPFS write can never be dropped.
 */
async function readNativeFileInChunks(
  path: string,
  chunkSize: number,
  onChunk: (bytes: Uint8Array<ArrayBuffer>) => Promise<void>,
): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    let settled = false;
    const settle = (fn: () => void) => {
      if (settled) return;
      settled = true;
      fn();
    };

    Filesystem.readFileInChunks({ path, chunkSize }, (chunkRead: ReadFileResult | null, err?: unknown) => {
      if (settled) return;

      if (err) {
        settle(() => reject(err instanceof Error ? err : new Error(String(err))));
        return;
      }

      if (chunkRead == null || chunkRead.data === '') {
        settle(resolve); // end of file
        return;
      }
      if (typeof chunkRead.data !== 'string') {
        // Native-only path (guarded above) always yields base64 strings —
        // a Blob here would mean web's ReadFileResult shape leaked through.
        settle(() => reject(new Error('[opfsMover] Expected base64 string chunk data, got Blob.')));
        return;
      }

      onChunk(base64ToBytes(chunkRead.data)).catch((writeErr: unknown) => {
        settle(() => reject(writeErr instanceof Error ? writeErr : new Error(String(writeErr))));
      });
    }).catch((startErr: unknown) => {
      settle(() => reject(startErr instanceof Error ? startErr : new Error(String(startErr))));
    });
  });
}

/**
 * Copies a native file into OPFS in bounded chunks, without ever holding the
 * whole file in memory. Resolves once the OPFS file is closed and its byte
 * count has been verified against the source's reported size.
 */
export async function moveNativeFileToOPFS({
  nativeFilePath,
  opfsFilename,
  chunkSizeBytes = DEFAULT_CHUNK_BYTES,
  onProgress,
}: OpfsMoveOptions): Promise<OpfsMoveResult> {
  if (!Capacitor.isNativePlatform() || typeof Filesystem.readFileInChunks !== 'function') {
    throw new Error(
      '[opfsMover] Native Filesystem bridge unavailable — moveNativeFileToOPFS requires an iOS/Android shell.',
    );
  }

  const stat = await Filesystem.stat({ path: nativeFilePath });
  const totalBytes = stat.size;

  const root = await navigator.storage.getDirectory();
  const fileHandle = await root.getFileHandle(opfsFilename, { create: true });
  const writable = await fileHandle.createWritable();

  let bytesWritten = 0;
  // Chunks are written through a single promise chain so a native callback
  // firing again before the previous chunk's write settles can never race
  // the writable stream (FileSystemWritableFileStream.write() is not
  // safe to call concurrently with itself).
  let writeQueue: Promise<void> = Promise.resolve();
  const enqueueChunk = (bytes: Uint8Array<ArrayBuffer>): Promise<void> => {
    writeQueue = writeQueue.then(async () => {
      await writable.write(bytes);
      bytesWritten += bytes.byteLength;
      onProgress?.(bytesWritten, totalBytes);
    });
    return writeQueue;
  };

  try {
    await readNativeFileInChunks(nativeFilePath, chunkSizeBytes, enqueueChunk);
    await writeQueue; // drain the last enqueued write
    if (bytesWritten !== totalBytes) {
      throw new Error(
        `[opfsMover] Torn transfer: wrote ${bytesWritten} bytes, source reported ${totalBytes}.`,
      );
    }
    await writable.close();
  } catch (err) {
    await writable.abort(err instanceof Error ? err.message : String(err)).catch(() => {});
    throw err;
  }

  return { opfsFilename, bytesWritten };
}
