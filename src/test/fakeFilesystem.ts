// SPDX-License-Identifier: Apache-2.0
/**
 * fakeFilesystem.ts — @capacitor/filesystem test double (P-FE.C1 harness).
 *
 * A mutable backend singleton the `vi.mock('@capacitor/filesystem')`
 * factory delegates to, so each test swaps state via
 * `resetFilesystemBackend()` without re-mocking the module. Faithful to
 * the pieces the app uses:
 *   - `readFileInChunks({path, chunkSize}, cb)` — callback API emitting
 *     deterministic base64 chunks in file order, a FINAL SHORT CHUNK when
 *     size % chunkSize !== 0, and `cb(null)` at EOF; returns a Promise
 *     (rejected if the file is missing), matching Capacitor's shape.
 *   - `stat` / `getUri` / `deleteFile` with absolute `file://` URIs keyed
 *     the same way the app passes them.
 *
 * Test hooks:
 *   - `errorAtChunk`: emit an error via the callback INSTEAD of chunk N
 *     (0-based) — the mid-stream failure case.
 *   - `chunkGate`: awaited before every chunk emission; lets ordering /
 *     concurrency tests hold the stream open deterministically.
 *   - `statSizeOverride`: per-path stat size lies (torn-transfer case).
 *   - `deletedPaths` / `calls`: observability for assertion of side
 *     effects and their order.
 */

export function bytesToBase64(bytes: Uint8Array): string {
  let bin = '';
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

interface ChunkResult {
  data: string;
}
type ChunkCallback = (chunk: ChunkResult | null, err?: unknown) => void;

export class FakeFilesystemBackend {
  /** Files keyed by the absolute URI (what the app passes after getUri). */
  readonly files = new Map<string, Uint8Array>();
  readonly deletedPaths: string[] = [];
  /** Ordered op log: `stat:`, `chunk:<i>:<len>`, `delete:` entries. */
  readonly calls: string[] = [];
  errorAtChunk: number | null = null;
  chunkGate: (() => Promise<void>) | null = null;
  readonly statSizeOverride = new Map<string, number>();

  uriFor(relPath: string): string {
    return `file:///data/${relPath}`;
  }

  seed(relPath: string, bytes: Uint8Array): string {
    const uri = this.uriFor(relPath);
    this.files.set(uri, bytes.slice());
    return uri;
  }

  async getUri({ path }: { path: string; directory?: string }): Promise<{ uri: string }> {
    return { uri: this.uriFor(path) };
  }

  async stat({ path }: { path: string }): Promise<{ size: number; type: string; mtime: number; uri: string }> {
    this.calls.push(`stat:${path}`);
    const bytes = this.files.get(path);
    if (!bytes) throw new Error(`File does not exist: ${path}`);
    return {
      size: this.statSizeOverride.get(path) ?? bytes.byteLength,
      type: 'file',
      mtime: 0,
      uri: path,
    };
  }

  async deleteFile({ path }: { path: string }): Promise<void> {
    this.calls.push(`delete:${path}`);
    this.deletedPaths.push(path);
    if (!this.files.delete(path)) throw new Error(`File does not exist: ${path}`);
  }

  readFileInChunks(
    opts: { path: string; chunkSize: number },
    cb: ChunkCallback,
  ): Promise<void> {
    const run = async (): Promise<void> => {
      const bytes = this.files.get(opts.path);
      if (!bytes) throw new Error(`File does not exist: ${opts.path}`);
      let index = 0;
      for (let off = 0; off < bytes.byteLength; off += opts.chunkSize, index++) {
        if (this.chunkGate) await this.chunkGate();
        if (this.errorAtChunk === index) {
          cb(null, new Error('fake EIO mid-stream'));
          return;
        }
        const chunk = bytes.subarray(off, Math.min(off + opts.chunkSize, bytes.byteLength));
        this.calls.push(`chunk:${index}:${chunk.byteLength}`);
        cb({ data: bytesToBase64(chunk) });
        // Yield the microtask queue between chunks so consumer-side write
        // chains interleave the way the real bridge does.
        await Promise.resolve();
      }
      cb(null); // EOF
    };
    return run();
  }
}

/** Live backend the module mock delegates to; swap per test. */
export const fsBackend: { current: FakeFilesystemBackend } = {
  current: new FakeFilesystemBackend(),
};

export function resetFilesystemBackend(): FakeFilesystemBackend {
  fsBackend.current = new FakeFilesystemBackend();
  return fsBackend.current;
}

/** The object to return as `Filesystem` from the vi.mock factory. */
export const FilesystemMock = {
  getUri: (o: { path: string; directory?: string }) => fsBackend.current.getUri(o),
  stat: (o: { path: string }) => fsBackend.current.stat(o),
  deleteFile: (o: { path: string }) => fsBackend.current.deleteFile(o),
  readFileInChunks: (o: { path: string; chunkSize: number }, cb: ChunkCallback) =>
    fsBackend.current.readFileInChunks(o, cb),
};
