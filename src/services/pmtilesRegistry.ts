/**
 * pmtilesRegistry.ts — the ONE owner of MapLibre's `pmtiles://` protocol
 * registration and its source registry (P9.C4).
 *
 * Why this module exists — the flaky-boot post-mortem (P9.C4):
 *
 * TWO compounding defects, both fixed here:
 *
 * (1) THE KEY MISMATCH (the big one). pmtiles v4's `Protocol.add(p)` stores
 *     under `p.source.getKey()`, but `Protocol.tile` looks entries up by the
 *     SCHEME-STRIPPED archive URL: `url.substr("pmtiles://".length)` for
 *     TileJSON requests and the equivalent regex capture for tile requests —
 *     i.e. `local/alps_basemap.pmtiles`. Our source's getKey() returned
 *     `pmtiles://local/<file>` (and before commit 659a203, bare `<file>`) —
 *     so the lookup has NEVER matched, on any boot, ever. Every request took
 *     the Protocol's silent miss path, which fabricates a fetch-backed
 *     archive from the stripped URL and caches it — and because real copies
 *     of both default archives sit in `public/local/` (the worker's
 *     provisioning source), Vite served them over HTTP 206 and the map
 *     LOOKED offline-capable while never actually reading tiles from OPFS.
 *     Any archive without an HTTP twin (every compiled `{jobId}.pmtiles`
 *     hot-swap target) got index.html bytes → "Wrong magic number". The
 *     "flaky boot" was this HTTP path's nondeterminism (dev-server ETag /
 *     range / timing behavior), not OPFS locks.
 *     Fix: getKey() now returns the scheme-stripped key the Protocol
 *     actually looks up, so worker-backed OPFS serving finally engages.
 *
 * (2) MODULE-SCOPE SINGLETON vs HMR DUPLICATION. The registration lived in
 *     MapView.tsx behind a module-scoped `let globalProtocol`. After an HMR
 *     pass, Vite can serve BOTH `MapView.tsx` and `MapView.tsx?t=<stamp>` as
 *     live, distinct module instances (observed in a stalled boot's resource
 *     log) — each with its own singleton slot, each calling
 *     `maplibregl.addProtocol`, last one winning, while the rendered
 *     component registers sources into ITS instance's registry.
 *     Fix: the singleton lives on `globalThis`, which every module instance
 *     shares; `maplibregl.addProtocol` runs exactly once per page, at module
 *     evaluation, entirely outside the React lifecycle.
 *
 * Hardening on top:
 *  - The handler FAILS LOUD on a registry miss for `pmtiles://` URLs instead
 *    of inheriting the silent fetch fallback: a key/boot-order bug now
 *    surfaces as one explicit error on the map's 'error' event, never a
 *    silent HTTP fallback or hang.
 *  - Registration is last-write-wins by key (`Map.set` semantics), so each
 *    MapView mount re-binds the canonical keys to sources backed by ITS
 *    live worker; stale entries from a torn-down mount are overwritten
 *    before the new map instance ever requests a tile. Nothing ever needs
 *    to `tiles.clear()`.
 */

import maplibregl from 'maplibre-gl';
import { Protocol, PMTiles } from 'pmtiles';
import type { Source, RangeResponse } from 'pmtiles';
import type { WorkerRequestMessage, WorkerResponseMessage } from '../shared/types';

// ---------------------------------------------------------------------------
// Telemetry shape (OPFS byte-range reads surfaced to the MapView HUD)
// ---------------------------------------------------------------------------

export interface TelemetryData {
  activeRequests: number;
  lastFetchTime: number;
  lastFetchSize: number;
  totalBytes: number;
}

// ---------------------------------------------------------------------------
// WorkerPMTilesSource — bridges PMTiles byte-range calls to a mapData worker
// ---------------------------------------------------------------------------

export class WorkerPMTilesSource implements Source {
  /**
   * The OPFS filename this source reads from (e.g. 'alps_basemap.pmtiles').
   * Sent with every MAP_READ_BYTES request so the worker can dispatch to the
   * correct SyncAccessHandle — one handle per file, all kept open concurrently.
   */
  readonly filename: string;

  private worker: Worker;
  private onTelemetry?: (data: TelemetryData) => void;
  private activeRequests = 0;
  private totalBytes = 0;

  constructor(filename: string, worker: Worker, onTelemetry?: (data: TelemetryData) => void) {
    this.filename = filename;
    this.worker   = worker;
    this.onTelemetry = onTelemetry;
  }

  /**
   * Registry key. MUST be the SCHEME-STRIPPED form of the style's source
   * URL (`pmtiles://local/<file>` → `local/<file>`): pmtiles v4's
   * Protocol.tile resolves archives by `url.substr("pmtiles://".length)`,
   * not by the full URL. Returning the full URL here (the pre-P9.C4 bug)
   * makes every lookup miss and silently re-routes all tile traffic to the
   * Protocol's fetch-backed fallback.
   */
  getKey() { return `local/${this.filename}`; }

  async getBytes(offset: number, length: number, signal?: AbortSignal): Promise<RangeResponse> {
    const startTime = performance.now();
    this.activeRequests++;
    this.triggerTelemetry(0, 0);

    return new Promise<RangeResponse>((resolve, reject) => {
      const requestId = Math.random().toString(36).substring(2, 9);

      const onMessage = (event: MessageEvent<WorkerResponseMessage>) => {
        const response = event.data;
        if (response.id !== requestId) return;
        this.worker.removeEventListener('message', onMessage);
        this.activeRequests--;
        const duration = performance.now() - startTime;

        if (response.type === 'MAP_BYTES_RESPONSE') {
          const buffer = response.payload.buffer as ArrayBuffer;
          this.totalBytes += buffer.byteLength;
          this.triggerTelemetry(duration, buffer.byteLength);
          resolve({ data: buffer });
        } else {
          this.triggerTelemetry(duration, 0);
          reject(new Error(response.error ?? 'Failed to read bytes from worker'));
        }
      };

      this.worker.addEventListener('message', onMessage);

      if (signal) {
        signal.addEventListener('abort', () => {
          this.worker.removeEventListener('message', onMessage);
          this.activeRequests--;
          this.triggerTelemetry(performance.now() - startTime, 0);
          reject(new DOMException('Aborted', 'AbortError'));
        });
      }

      // Include the target filename so the worker routes the read to the
      // correct OPFS SyncAccessHandle rather than a single shared slot.
      const req: WorkerRequestMessage = {
        id:      requestId,
        type:    'MAP_READ_BYTES',
        payload: { filename: this.filename, offset, length },
      };
      this.worker.postMessage(req);
    });
  }

  private triggerTelemetry(duration: number, bytesRead: number) {
    this.onTelemetry?.({
      activeRequests: this.activeRequests,
      lastFetchTime: duration,
      lastFetchSize: bytesRead,
      totalBytes: this.totalBytes,
    });
  }
}

// ---------------------------------------------------------------------------
// globalThis-anchored singleton
// ---------------------------------------------------------------------------

interface PmtilesRegistrySingleton {
  protocol: Protocol;
}

const GLOBAL_KEY = '__freehike_pmtiles_registry__';

function getSingleton(): PmtilesRegistrySingleton {
  const g = globalThis as unknown as Record<string, PmtilesRegistrySingleton | undefined>;
  const existing = g[GLOBAL_KEY];
  if (existing) return existing;

  const protocol = new Protocol();

  // Guard wrapper: every pmtiles:// URL this app issues refers to an
  // OPFS-backed archive that MUST have been registered. The stock
  // Protocol.tile would fall back to fetching the scheme-stripped path over
  // HTTP on a miss — the silent-hang failure mode this module exists to
  // kill. Reject loudly instead; MapLibre surfaces it via the map 'error'
  // event, which MapView already logs.
  // One boot-diagnostic line per archive key, first time it is served — a
  // healthy boot logs exactly one line per registered archive; its absence
  // while the map hangs localizes the fault to MapLibre-side request flow.
  const servedKeys = new Set<string>();

  const guardedTile: maplibregl.AddProtocolAction = (params, abortController) => {
    // Registry keys are scheme-stripped (see WorkerPMTilesSource.getKey);
    // compare against the same stripped form the Protocol itself uses.
    const stripped = params.url.replace(/^pmtiles:\/\//, '');
    let known = false;
    for (const key of protocol.tiles.keys()) {
      if (stripped === key || stripped.startsWith(`${key}/`)) {
        if (!servedKeys.has(key)) {
          servedKeys.add(key);
          console.log(`[pmtilesRegistry] Serving "${key}" from its OPFS-backed worker source (first request: ${params.type}).`);
        }
        known = true;
        break;
      }
    }
    if (!known) {
      return Promise.reject(
        new Error(
          `[pmtilesRegistry] No registered source for "${params.url}" — ` +
          'an OPFS-backed archive must be registered (registerPMTilesSource) before the style references it. ' +
          `Registered keys: [${[...protocol.tiles.keys()].join(', ')}]`,
        ),
      );
    }
    return protocol.tile(params, abortController);
  };

  maplibregl.addProtocol('pmtiles', guardedTile);

  const singleton: PmtilesRegistrySingleton = { protocol };
  g[GLOBAL_KEY] = singleton;
  return singleton;
}

/**
 * Evaluated at import time: the protocol handler is registered with
 * maplibre before any React component can mount, exactly once per page.
 */
const registry = getSingleton();

/**
 * Creates a worker-backed PMTiles source for `filename` and registers it
 * under its canonical `local/<filename>` key. Last write wins:
 * re-registering a filename (new mount, hot-swap re-bind) atomically
 * replaces any stale instance bound to a dead worker.
 *
 * Returns the PMTiles instance for callers that also read the archive
 * directly (e.g. the contour DemSource).
 */
export function registerPMTilesSource(
  filename: string,
  worker: Worker,
  onTelemetry?: (data: TelemetryData) => void,
): PMTiles {
  const instance = new PMTiles(new WorkerPMTilesSource(filename, worker, onTelemetry));
  registry.protocol.add(instance);
  return instance;
}
