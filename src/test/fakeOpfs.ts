// SPDX-License-Identifier: Apache-2.0
/**
 * fakeOpfs.ts — in-memory OPFS test double (P-FE.C1 harness).
 *
 * Implements exactly the subset this app uses:
 *   navigator.storage.getDirectory() → root
 *   root.getFileHandle(name, {create}) → handle
 *   handle.getFile()                  → { name, size, arrayBuffer() }
 *   handle.createWritable()           → main-thread write path (opfsMover)
 *   handle.createSyncAccessHandle()   → worker path (mapData.worker-shaped)
 *
 * Load-bearing semantics, faithfully reproduced:
 *   - SINGLE-HANDLE EXCLUSIVITY: an open SyncAccessHandle blocks
 *     createWritable AND further sync handles with a
 *     NoModificationAllowedError-named throw until close(). The app's
 *     swap-vs-bound-file rule depends on this (see tracker item B005
 *     context: the worker's bound basemap handle must make same-name
 *     writes fail, which is why ingests target `{jobId}.pmtiles`).
 *   - SWAP-ON-CLOSE: createWritable buffers writes and commits atomically
 *     at close(); abort() discards, leaving prior committed content — an
 *     aborted copy can never present a partial file as complete.
 *
 * Test hooks (never part of the real API):
 *   - `sizeOverrides`: make getFile().size lie, to exercise post-copy
 *     verification mismatch branches.
 *   - `failWrites`: make writable.write() throw, to exercise
 *     durability-ordering (ack-after-close) branches.
 *
 * This header comment is the harness documentation (chunk spec allows a
 * header comment in place of a README section).
 */
import { vi } from 'vitest';

function domError(name: string, message: string): Error {
  const err = new Error(message);
  err.name = name;
  return err;
}

function concat(chunks: Uint8Array[]): Uint8Array {
  const total = chunks.reduce((n, c) => n + c.byteLength, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.byteLength;
  }
  return out;
}

export interface FakeSyncAccessHandle {
  getSize(): number;
  read(out: Uint8Array, opts?: { at?: number }): number;
  write(data: Uint8Array, opts?: { at?: number }): number;
  truncate(size: number): void;
  flush(): void;
  close(): void;
}

export interface FakeWritable {
  write(chunk: Uint8Array): Promise<void>;
  close(): Promise<void>;
  abort(reason?: string): Promise<void>;
}

export interface FakeFile {
  name: string;
  size: number;
  arrayBuffer(): Promise<ArrayBuffer>;
}

export interface FakeFileHandle {
  readonly name: string;
  getFile(): Promise<FakeFile>;
  createWritable(): Promise<FakeWritable>;
  createSyncAccessHandle(): Promise<FakeSyncAccessHandle>;
}

export class FakeOpfs {
  private readonly committed = new Map<string, Uint8Array>();
  private readonly syncLocks = new Set<string>();
  private readonly openWritables = new Set<string>();
  /** Test hook: force getFile().size to report this value instead. */
  readonly sizeOverrides = new Map<string, number>();
  /** Test hook: writable.write() throws for these filenames. */
  readonly failWrites = new Set<string>();

  seed(name: string, bytes: Uint8Array): void {
    this.committed.set(name, bytes.slice());
  }

  /** Committed (durable) content, or null if the entry doesn't exist. */
  committedBytes(name: string): Uint8Array | null {
    const b = this.committed.get(name);
    return b ? b.slice() : null;
  }

  has(name: string): boolean {
    return this.committed.has(name);
  }

  readonly root = {
    getFileHandle: async (
      name: string,
      opts?: { create?: boolean },
    ): Promise<FakeFileHandle> => {
      if (!this.committed.has(name)) {
        if (!opts?.create) throw domError('NotFoundError', `no OPFS entry '${name}'`);
        // Real OPFS creates the (empty) entry immediately; content written
        // through a writable only becomes visible at close().
        this.committed.set(name, new Uint8Array(0));
      }
      return this.handle(name);
    },
  };

  /** Stubs `navigator.storage.getDirectory()` to serve this fake root.
   *  Cleared by the global `vi.unstubAllGlobals()` in setup.ts. */
  install(): void {
    vi.stubGlobal('navigator', {
      storage: { getDirectory: async () => this.root },
    });
  }

  private handle(name: string): FakeFileHandle {
    return {
      name,
      getFile: async (): Promise<FakeFile> => {
        const data = this.committed.get(name);
        if (!data) throw domError('NotFoundError', `no OPFS entry '${name}'`);
        const copy = data.slice();
        return {
          name,
          size: this.sizeOverrides.get(name) ?? copy.byteLength,
          arrayBuffer: async () => copy.buffer as ArrayBuffer,
        };
      },

      createWritable: async (): Promise<FakeWritable> => {
        if (this.syncLocks.has(name) || this.openWritables.has(name)) {
          throw domError('NoModificationAllowedError', `'${name}' is locked by another handle`);
        }
        this.openWritables.add(name);
        const chunks: Uint8Array[] = [];
        let open = true;
        return {
          write: async (chunk: Uint8Array): Promise<void> => {
            if (!open) throw domError('InvalidStateError', 'writable already closed');
            if (this.failWrites.has(name)) {
              throw domError('QuotaExceededError', `fake write failure for '${name}'`);
            }
            chunks.push(chunk.slice());
          },
          close: async (): Promise<void> => {
            if (!open) return;
            open = false;
            this.openWritables.delete(name);
            this.committed.set(name, concat(chunks)); // swap-on-close
          },
          abort: async (): Promise<void> => {
            if (!open) return;
            open = false;
            this.openWritables.delete(name); // prior committed content stands
          },
        };
      },

      createSyncAccessHandle: async (): Promise<FakeSyncAccessHandle> => {
        if (this.syncLocks.has(name) || this.openWritables.has(name)) {
          throw domError('NoModificationAllowedError', `'${name}' is locked by another handle`);
        }
        this.syncLocks.add(name);
        let buf = (this.committed.get(name) ?? new Uint8Array(0)).slice();
        let open = true;
        const commit = () => this.committed.set(name, buf.slice());
        return {
          getSize: () => buf.byteLength,
          read(out: Uint8Array, opts?: { at?: number }): number {
            const at = opts?.at ?? 0;
            const n = Math.max(Math.min(out.byteLength, buf.byteLength - at), 0);
            out.set(buf.subarray(at, at + n));
            return n;
          },
          write(data: Uint8Array, opts?: { at?: number }): number {
            const at = opts?.at ?? 0;
            const end = at + data.byteLength;
            if (end > buf.byteLength) {
              const grown = new Uint8Array(end);
              grown.set(buf);
              buf = grown;
            }
            buf.set(data, at);
            return data.byteLength;
          },
          truncate(size: number): void {
            const next = new Uint8Array(size);
            next.set(buf.subarray(0, Math.min(size, buf.byteLength)));
            buf = next;
          },
          flush: commit,
          close: (): void => {
            if (!open) return;
            open = false;
            commit();
            this.syncLocks.delete(name);
          },
        };
      },
    };
  }
}
