// SPDX-License-Identifier: Apache-2.0
// ─── Worker RPC contracts (unchanged) ────────────────────────────────────────

export type WorkerRequestType =
  | 'GEOPROCESS'
  | 'VALHALLA_ROUTE'
  | 'MAP_INIT'
  | 'MAP_READ_BYTES'
  | 'TRAILS_FETCH_BOUNDS'
  | 'TRAILS_QUERY_NEAREST'
  | 'ROUTE_CALCULATE_REQUEST'
  | 'ROUTE_TILE_FETCH_REQUEST'
  | 'ROUTE_TILE_WRITE_REQUEST'
  | 'ELEVATION_PROFILE_REQUEST'
  // Phase 10: Dynamic OPFS download manager
  | 'DOWNLOAD_REGION_REQUEST'
  // Phase 11: Dynamic multi-file OPFS source routing
  | 'LOAD_OFFLINE_REGION'
  | 'MAP_CLOSE';

export type WorkerResponseType =
  | 'SUCCESS'
  | 'ERROR'
  | 'MAP_INIT_SUCCESS'
  | 'MAP_BYTES_RESPONSE'
  | 'TRAILS_INDEX_COMPLIANCE'
  | 'TRAILS_NEAREST_RESPONSE'
  | 'ROUTE_CALCULATE_SUCCESS'
  | 'ROUTE_TILE_WRITE_SUCCESS'
  | 'ELEVATION_PROFILE_SUCCESS'
  // Phase 10: Dynamic OPFS download manager
  | 'DOWNLOAD_REGION_SUCCESS'
  | 'DOWNLOAD_REGION_ERROR'
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

// ─── Phase 4: User Data Sovereignty & Sync ───────────────────────────────────

/** Which cloud provider is currently configured. */
export type SyncProvider = 'google' | 'dropbox' | 'none';

/** Live state machine for the Cloud Sync UI. */
export type SyncConnectionStatus =
  | 'disconnected'
  | 'connecting'
  | 'connected'
  | 'syncing'
  | 'error';

/** Persisted metadata written to IndexedDB after each successful sync. */
export interface SyncMetadata {
  provider: SyncProvider;
  /** Email address returned from the provider's userinfo endpoint. */
  accountEmail?: string;
  /** ISO-8601 UTC timestamp of the last successful upload. */
  lastSynced: string;
  /** Total bytes uploaded in the last batch. */
  lastFileSize: number;
  /** Number of files uploaded in the last batch. */
  filesUploaded: number;
}

/**
 * OAuth token envelope stored in localStorage under a provider-prefixed key.
 * Access tokens and refresh tokens are short-lived; expiresAt drives
 * the auto-refresh guard in each provider service module.
 */
export interface OAuthTokenRecord {
  provider: SyncProvider;
  accessToken: string;
  /** Present for Google (offline) and Dropbox (offline) flows. */
  refreshToken?: string;
  /** Epoch milliseconds at which the accessToken expires. */
  expiresAt: number;
  scope: string;
}

/** Top-level record stored in the `sync_manifest` IndexedDB object store. */
export interface SyncManifestRecord {
  /** Fixed primary key — only one manifest record exists at a time. */
  id: 'sync_manifest';
  metadata: SyncMetadata;
  tokenRecord: OAuthTokenRecord;
}

/**
 * Shape of each feature object written by spatial.worker.ts to
 * `trails_features.json` in OPFS.  Used by gpxSerializer.ts and
 * consumed by the sync pipeline on the main thread.
 */
export interface CachedTrailFeature {
  id: number;
  name: string;
  highway: string;
  /** Flat coordinate ring: [lng₀, lat₀, lng₁, lat₁, …] */
  coords: number[];
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

// ─── Phase 5: Offline Routing Engine (Valhalla WASM) ─────────────────────────

export interface RouteCalculateRequestPayload {
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  costingModel?: 'pedestrian' | 'bicycle' | 'auto'; // defaults to "pedestrian"
}

export interface RouteCalculateSuccessPayload {
  geojson: {
    type: 'Feature';
    geometry: {
      type: 'LineString';
      coordinates: number[][];
    };
    properties: {
      distanceMeters: number;
      costingModel: string;
    };
  };
  distanceMeters: number;
  /** Flat coordinates buffer [lng0, lat0, lng1, lat1, ...] for zero-copy transfer. */
  coordinatesBuffer: ArrayBuffer;
}

export interface RouteTileFetchRequestPayload {
  /** [south, west, north, east] bounding box */
  bbox: [number, number, number, number];
}

export interface RouteTileWriteRequestPayload {
  filename: string;
  /** Binary contents of the Valhalla .tar file */
  tarBuffer: ArrayBuffer;
}

// ─── Phase 7: Elevation Profiling & GEOPROCESS ───────────────────────────────

export interface ElevationProfileRequestPayload {
  /** Flat coordinate buffer [lng0, lat0, lng1, lat1, ...] */
  coordinates: Float64Array;
}

export interface ElevationProfileSuccessPayload {
  totalAscent: number;
  totalDescent: number;
  /** Smoothed Z-values for each coordinate point (flat Float64Array) */
  elevations: Float64Array;
}

// ─── Phase 10: Offline Download Manager ──────────────────────────────────────

/**
 * Payload sent main-thread → mapData.worker via transferable ArrayBuffers.
 * Both buffers are zero-copy transferred so the main thread loses ownership
 * immediately after postMessage; the worker owns them during the write.
 */
export interface DownloadRegionRequestPayload {
  /** Raw PMTiles file bytes — written to OPFS as `active_map.pmtiles`. */
  pmtilesBuffer: ArrayBuffer;
  /** Raw Valhalla .tar file bytes — written to OPFS as `active_routing.tar`.
   *  May be an empty (0-byte) buffer when routing data is unavailable. */
  routingBuffer: ArrayBuffer;
  /** Human-readable label shown in telemetry (e.g. 'Andorra'). */
  regionLabel: string;
}

/** Payload returned by the worker on success. */
export interface DownloadRegionSuccessPayload {
  regionLabel:   string;
  pmtilesBytes:  number;
  routingBytes:  number;
  writtenAt:     string; // ISO-8601
}

