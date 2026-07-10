/**
 * spatial.worker.ts
 *
 * Phase 3 – Spatial Intelligence & Open Ingestion Pipeline
 *
 * Responsibilities (in order of execution):
 *  1. TRAILS_FETCH_BOUNDS  → POST to Overpass API for OSM way geometries inside
 *                            a viewport bounding box, build a static Flatbush
 *                            Hilbert R-Tree, persist both the binary index and
 *                            the raw feature set to OPFS, then reply with
 *                            TRAILS_INDEX_COMPLIANCE.
 *
 *  2. TRAILS_QUERY_NEAREST → Given a [lng, lat] cursor point, search the in-
 *                            memory Flatbush index for the closest way bbox,
 *                            then brute-force the exact shortest perpendicular
 *                            distance (in metres) to any segment of that way.
 *                            Replies with TRAILS_NEAREST_RESPONSE.
 *
 * Threading model:
 *  - This worker runs exclusively off the main thread.
 *  - All OPFS access uses FileSystemSyncAccessHandle (synchronous, zero-copy).
 *  - Index ArrayBuffers are transferred (not copied) to the main thread.
 */

import Flatbush from 'flatbush';
import type {
  WorkerRequestMessage,
  WorkerResponseMessage,
  ElevationProfileRequestPayload,
  ElevationProfileSuccessPayload,
} from '../shared/types';

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/** Live in-memory Flatbush index, rebuilt on every TRAILS_FETCH_BOUNDS call. */
let _index: Flatbush | null = null;

/**
 * Parallel array to the Flatbush index positions (0 … N-1).
 * Each entry corresponds to one OSM way element stored at that index slot.
 */
let _features: OsmWayFeature[] = [];

// ---------------------------------------------------------------------------
// OSM / Overpass types
// ---------------------------------------------------------------------------

/** A decoded OSM way with its full geometry and tag metadata. */
interface OsmWayFeature {
  id: number;
  name: string;
  highway: string;
  /** Flat ring of coordinate pairs: [lng0, lat0, lng1, lat1, …] */
  coords: Float64Array;
  /** Pre-computed bounding box for Flatbush. */
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

/** Minimal subset of the Overpass JSON response we care about. */
interface OverpassElement {
  type: 'node' | 'way' | 'relation';
  id: number;
  tags?: Record<string, string>;
  geometry?: Array<{ lat: number; lon: number }>;
}

interface OverpassResponse {
  elements: OverpassElement[];
}

// ---------------------------------------------------------------------------
// Payload shapes for our RPC messages
// ---------------------------------------------------------------------------

interface FetchBoundsPayload {
  /** [south, west, north, east] in WGS-84 decimal degrees. */
  bbox: [number, number, number, number];
}

interface QueryNearestPayload {
  /** WGS-84 longitude. */
  lng: number;
  /** WGS-84 latitude. */
  lat: number;
}

interface IndexCompliancePayload {
  featureCount: number;
  indexBytes: number;
  geojson?: string;
}

interface NearestResponsePayload {
  found: boolean;
  name?: string;
  highway?: string;
  /** Shortest distance from the query point to the closest segment, in metres. */
  distanceMeters?: number;
  /** The coordinate on the trail that is closest to the cursor [lng, lat]. */
  closestPoint?: [number, number];
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const OVERPASS_ENDPOINT = 'https://overpass-api.de/api/interpreter';
const CLIENT_ID_HEADER = 'Antigravity-Hiking-App/1.0';

/** Overpass QL template. Placeholder `{BBOX}` is replaced at call time.     */
const OVERPASS_QUERY_TPL = `
[out:json][timeout:25];
(
  way["highway"="path"]({{BBOX}});
  way["highway"="footway"]({{BBOX}});
  way["highway"="track"]["tracktype"~"grade[2-5]"]({{BBOX}});
  way["route"="hiking"]({{BBOX}});
  nwr["route"="hiking"]({{BBOX}});
);
out geom;
`.trim();

/** Exponential back-off: base delay 1 s, max 3 retries. */
const BACKOFF_BASE_MS = 1_000;
const MAX_RETRIES = 3;

/** OPFS file names. */
const OPFS_INDEX_FILENAME = 'trails_index.bin';
const OPFS_FEATURES_FILENAME = 'trails_features.json';

// ---------------------------------------------------------------------------
// Message router
// ---------------------------------------------------------------------------

self.addEventListener('message', async (event: MessageEvent<WorkerRequestMessage>) => {
  const { id, type, payload } = event.data;

  try {
    switch (type) {
      case 'TRAILS_FETCH_BOUNDS': {
        await handleFetchBounds(id, payload as FetchBoundsPayload);
        break;
      }
      case 'TRAILS_QUERY_NEAREST': {
        handleQueryNearest(id, payload as QueryNearestPayload);
        break;
      }
      case 'ELEVATION_PROFILE_REQUEST': {
        await handleElevationProfile(id, payload as ElevationProfileRequestPayload);
        break;
      }
      default: {
        reply(id, 'ERROR', null, `[spatial.worker] Unknown request type: ${type}`);
      }
    }
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    console.error('[spatial.worker] Unhandled error for', type, err);
    reply(id, 'ERROR', null, msg);
  }
});

// ---------------------------------------------------------------------------
// Handler: TRAILS_FETCH_BOUNDS
// ---------------------------------------------------------------------------

async function handleFetchBounds(id: string, payload: FetchBoundsPayload): Promise<void> {
  const { bbox } = payload;
  const [south, west, north, east] = bbox;
  const bboxStr = `${south},${west},${north},${east}`;

  console.log(`[spatial.worker] Fetching Overpass for bbox: ${bboxStr}`);

  // 1. Query Overpass with exponential back-off
  const rawJson = await fetchOverpassWithBackoff(bboxStr);

  // 2. Parse OSM elements → OsmWayFeature[]
  const features = parseOsmElements(rawJson.elements);
  console.log(`[spatial.worker] Parsed ${features.length} trail features.`);

  if (features.length === 0) {
    // Nothing to index – reply immediately with zero-feature compliance.
    const emptyGeoJson = JSON.stringify({ type: 'FeatureCollection', features: [] });
    reply<IndexCompliancePayload>(id, 'TRAILS_INDEX_COMPLIANCE', {
      featureCount: 0,
      indexBytes: 0,
      geojson: emptyGeoJson,
    });
    return;
  }

  // 3. Build Flatbush index
  const index = new Flatbush(features.length);
  for (const f of features) {
    index.add(f.minX, f.minY, f.maxX, f.maxY);
  }
  index.finish();

  // 4. Persist to OPFS
  await persistToOpfs(index, features);

  // 5. Stash in module-level state for subsequent TRAILS_QUERY_NEAREST calls
  _index = index;
  _features = features;

  // 6. Build GeoJSON FeatureCollection from parsed ways so the main thread
  //    can inject it directly into the MapLibre source without a second RPC.
  const geojsonFeatures = features.map((f) => {
    const coordPairs: [number, number][] = [];
    for (let i = 0; i < f.coords.length; i += 2) {
      coordPairs.push([f.coords[i], f.coords[i + 1]]);
    }
    return {
      type: 'Feature' as const,
      properties: { name: f.name, highway: f.highway, id: f.id },
      geometry: { type: 'LineString' as const, coordinates: coordPairs },
    };
  });
  const geojson = JSON.stringify({ type: 'FeatureCollection', features: geojsonFeatures });

  // 7. Transfer a copy of the raw index ArrayBuffer to the main thread.
  //    Flatbush.data is typed as ArrayBuffer | SharedArrayBuffer; we normalise
  //    it to a plain ArrayBuffer via Uint8Array so the Transferable list works.
  const transferBuf: ArrayBuffer = new Uint8Array(index.data).slice(0).buffer;

  const compliancePayload: IndexCompliancePayload = {
    featureCount: features.length,
    indexBytes: transferBuf.byteLength,
  };

  const response: WorkerResponseMessage = {
    id,
    type: 'TRAILS_INDEX_COMPLIANCE',
    payload: {
      ...compliancePayload,
      geojson,
      indexBuffer: transferBuf,
    },
  };
  // Transfer ownership of the binary index buffer – zero-copy hand-off.
  // The GeoJSON string is sent by value (structured clone) – acceptable at
  // this frequency since scans are user-triggered, not per-frame.
  self.postMessage(response, [transferBuf]);
}

// ---------------------------------------------------------------------------
// Handler: TRAILS_QUERY_NEAREST
// ---------------------------------------------------------------------------

function handleQueryNearest(id: string, payload: QueryNearestPayload): void {
  const { lng, lat } = payload;

  if (!_index || _features.length === 0) {
    reply<NearestResponsePayload>(id, 'TRAILS_NEAREST_RESPONSE', { found: false });
    return;
  }

  // Flatbush.neighbors returns indices into the insertion order.
  // We ask for 1 nearest bbox centroid, then compute exact segment distance.
  const candidates = _index.neighbors(lng, lat, 3); // check top-3 bbox matches

  if (candidates.length === 0) {
    reply<NearestResponsePayload>(id, 'TRAILS_NEAREST_RESPONSE', { found: false });
    return;
  }

  // For each candidate, compute the exact shortest distance to any segment.
  let bestDist = Infinity;
  let bestClosest: [number, number] = [lng, lat];
  let bestFeature: OsmWayFeature | null = null;

  for (const candidateIdx of candidates) {
    const feature = _features[candidateIdx];
    const result = closestPointOnWay(lng, lat, feature.coords);
    if (result.distanceMeters < bestDist) {
      bestDist = result.distanceMeters;
      bestClosest = result.point;
      bestFeature = feature;
    }
  }

  if (!bestFeature) {
    reply<NearestResponsePayload>(id, 'TRAILS_NEAREST_RESPONSE', { found: false });
    return;
  }

  reply<NearestResponsePayload>(id, 'TRAILS_NEAREST_RESPONSE', {
    found: true,
    name: bestFeature.name,
    highway: bestFeature.highway,
    distanceMeters: Math.round(bestDist),
    closestPoint: bestClosest,
  });
}

// ---------------------------------------------------------------------------
// Overpass fetch with exponential back-off
// ---------------------------------------------------------------------------

async function fetchOverpassWithBackoff(bboxStr: string): Promise<OverpassResponse> {
  const query = OVERPASS_QUERY_TPL.replaceAll('{{BBOX}}', bboxStr);

  for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
    try {
      const response = await fetch(OVERPASS_ENDPOINT, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/x-www-form-urlencoded',
          'User-Agent': CLIENT_ID_HEADER,
          'X-Client-Id': CLIENT_ID_HEADER,
        },
        body: `data=${encodeURIComponent(query)}`,
      });

      if (response.ok) {
        const json = (await response.json()) as OverpassResponse;
        return json;
      }

      // Retryable HTTP errors
      if (response.status === 429 || response.status === 504 || response.status === 503) {
        if (attempt < MAX_RETRIES) {
          const delay = BACKOFF_BASE_MS * Math.pow(2, attempt);
          console.warn(
            `[spatial.worker] Overpass returned ${response.status}. ` +
              `Retrying in ${delay}ms (attempt ${attempt + 1}/${MAX_RETRIES})…`
          );
          await sleep(delay);
          continue;
        }
      }

      // Non-retryable or exhausted retries
      throw new Error(
        `[spatial.worker] Overpass request failed: HTTP ${response.status} ${response.statusText}`
      );
    } catch (networkErr: unknown) {
      // Re-throw network errors on last attempt
      if (attempt === MAX_RETRIES) throw networkErr;
      const delay = BACKOFF_BASE_MS * Math.pow(2, attempt);
      console.warn(
        `[spatial.worker] Network error fetching Overpass (attempt ${attempt + 1}). ` +
          `Retrying in ${delay}ms…`,
        networkErr
      );
      await sleep(delay);
    }
  }

  // Should be unreachable but TypeScript needs a return path.
  throw new Error('[spatial.worker] Overpass fetch exhausted all retries.');
}

// ---------------------------------------------------------------------------
// OSM element parser → OsmWayFeature[]
// ---------------------------------------------------------------------------

function parseOsmElements(elements: OverpassElement[]): OsmWayFeature[] {
  const features: OsmWayFeature[] = [];

  for (const el of elements) {
    // We only handle ways with embedded geometry (out geom; ensures this).
    if (el.type !== 'way' || !el.geometry || el.geometry.length < 2) continue;

    const tags = el.tags ?? {};
    const name = tags['name'] ?? tags['ref'] ?? tags['route'] ?? 'Unnamed Trail';
    const highway = tags['highway'] ?? tags['route'] ?? 'path';

    // Build a flat Float64Array of [lng0, lat0, lng1, lat1, …] pairs.
    // Flatbush uses (minX=minLng, minY=minLat, maxX=maxLng, maxY=maxLat).
    const nodeCount = el.geometry.length;
    const coords = new Float64Array(nodeCount * 2);

    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;

    for (let i = 0; i < nodeCount; i++) {
      const { lat, lon } = el.geometry[i];
      coords[i * 2] = lon;      // x = longitude
      coords[i * 2 + 1] = lat;  // y = latitude

      if (lon < minX) minX = lon;
      if (lat < minY) minY = lat;
      if (lon > maxX) maxX = lon;
      if (lat > maxY) maxY = lat;
    }

    features.push({ id: el.id, name, highway, coords, minX, minY, maxX, maxY });
  }

  return features;
}

// ---------------------------------------------------------------------------
// OPFS persistence (synchronous via createSyncAccessHandle)
// ---------------------------------------------------------------------------

async function persistToOpfs(index: Flatbush, features: OsmWayFeature[]): Promise<void> {
  try {
    const root = await navigator.storage.getDirectory();

    // --- Persist binary Flatbush index ---
    const indexHandle = await root.getFileHandle(OPFS_INDEX_FILENAME, { create: true });
    const indexSync = await indexHandle.createSyncAccessHandle();
    try {
      indexSync.truncate(0);
      indexSync.write(new Uint8Array(index.data), { at: 0 });
      indexSync.flush();
    } finally {
      indexSync.close();
    }
    console.log(
      `[spatial.worker] Wrote ${index.data.byteLength} bytes → OPFS/${OPFS_INDEX_FILENAME}`
    );

    // --- Persist feature metadata as JSON (coords excluded to save space) ---
    const metaArray = features.map((f) => ({
      id: f.id,
      name: f.name,
      highway: f.highway,
      minX: f.minX,
      minY: f.minY,
      maxX: f.maxX,
      maxY: f.maxY,
      // coords are stored as a regular array for JSON serialisability
      coords: Array.from(f.coords),
    }));
    const featuresJson = JSON.stringify(metaArray);
    const featuresBytes = new TextEncoder().encode(featuresJson);

    const featuresHandle = await root.getFileHandle(OPFS_FEATURES_FILENAME, { create: true });
    const featuresSync = await featuresHandle.createSyncAccessHandle();
    try {
      featuresSync.truncate(0);
      featuresSync.write(featuresBytes, { at: 0 });
      featuresSync.flush();
    } finally {
      featuresSync.close();
    }
    console.log(
      `[spatial.worker] Wrote ${featuresBytes.byteLength} bytes → OPFS/${OPFS_FEATURES_FILENAME}`
    );
  } catch (err) {
    // OPFS failure is non-fatal – index is still live in memory.
    console.warn('[spatial.worker] OPFS persistence failed (non-fatal):', err);
  }
}

// ---------------------------------------------------------------------------
// Geometry math: closest point on a polyline, Haversine distance
// ---------------------------------------------------------------------------

/**
 * Finds the closest point (and its Haversine distance in metres) on any
 * segment of the given way to the query point (qLng, qLat).
 *
 * The coords buffer is a flat Float64Array: [lng0, lat0, lng1, lat1, …].
 */
function closestPointOnWay(
  qLng: number,
  qLat: number,
  coords: Float64Array
): { point: [number, number]; distanceMeters: number } {
  let bestDist = Infinity;
  let bestPt: [number, number] = [qLng, qLat];
  const segCount = coords.length / 2 - 1;

  for (let i = 0; i < segCount; i++) {
    const ax = coords[i * 2];
    const ay = coords[i * 2 + 1];
    const bx = coords[i * 2 + 2];
    const by = coords[i * 2 + 3];

    const pt = closestPointOnSegment(qLng, qLat, ax, ay, bx, by);
    const d = haversineMeters(qLat, qLng, pt[1], pt[0]);

    if (d < bestDist) {
      bestDist = d;
      bestPt = pt;
    }
  }

  return { point: bestPt, distanceMeters: bestDist };
}

/**
 * Projects the query point Q onto the segment AB, clamped to [0, 1].
 * All coordinates in decimal degrees (WGS-84).
 * Returns the closest point as [lng, lat].
 */
function closestPointOnSegment(
  qx: number, qy: number, // query  (lng, lat)
  ax: number, ay: number, // seg A  (lng, lat)
  bx: number, by: number  // seg B  (lng, lat)
): [number, number] {
  const dx = bx - ax;
  const dy = by - ay;
  const lenSq = dx * dx + dy * dy;

  if (lenSq === 0) return [ax, ay]; // degenerate segment

  // Parameter t along AB
  const t = Math.max(0, Math.min(1, ((qx - ax) * dx + (qy - ay) * dy) / lenSq));

  return [ax + t * dx, ay + t * dy];
}

/**
 * Haversine great-circle distance between two WGS-84 points, in metres.
 *
 * @param lat1 Latitude of point 1 (degrees)
 * @param lon1 Longitude of point 1 (degrees)
 * @param lat2 Latitude of point 2 (degrees)
 * @param lon2 Longitude of point 2 (degrees)
 */
function haversineMeters(
  lat1: number, lon1: number,
  lat2: number, lon2: number
): number {
  const R = 6_371_000; // Earth radius in metres
  const φ1 = (lat1 * Math.PI) / 180;
  const φ2 = (lat2 * Math.PI) / 180;
  const Δφ = ((lat2 - lat1) * Math.PI) / 180;
  const Δλ = ((lon2 - lon1) * Math.PI) / 180;

  const a =
    Math.sin(Δφ / 2) * Math.sin(Δφ / 2) +
    Math.cos(φ1) * Math.cos(φ2) * Math.sin(Δλ / 2) * Math.sin(Δλ / 2);

  return R * 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/** Typed convenience wrapper around self.postMessage. */
function reply<T>(
  id: string,
  type: WorkerResponseMessage['type'],
  payload: T,
  error?: string
): void {
  const msg: WorkerResponseMessage = { id, type, payload, error };
  self.postMessage(msg);
}

/** Promise-based sleep for use inside async retry loops. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ---------------------------------------------------------------------------
// Phase 7: Elevation Profiling & GEOPROCESS
// ---------------------------------------------------------------------------

interface TileCoords {
  tileX: number;
  tileY: number;
  pixelX: number;
  pixelY: number;
}

/**
 * Converts a WGS-84 coordinate [lng, lat] to a Web Mercator tile index (tileX, tileY)
 * and the exact pixel offset (pixelX, pixelY) in a 256x256 tile at a given zoom level.
 */
function lngLatToTile(lng: number, lat: number, zoom: number): TileCoords {
  const N = Math.pow(2, zoom);
  const xFrac = N * (lng + 180) / 360;
  
  // Latitude WGS-84 to Web Mercator projection
  const latRad = (lat * Math.PI) / 180;
  const yFrac = (N / 2) * (1 - Math.log(Math.tan(Math.PI / 4 + latRad / 2)) / Math.PI);
  
  const tileX = Math.floor(xFrac);
  const tileY = Math.floor(yFrac);
  
  // Pixel coordinates inside the 256x256 tile, clamped to [0, 255]
  const pixelX = Math.max(0, Math.min(255, Math.floor((xFrac - tileX) * 256)));
  const pixelY = Math.max(0, Math.min(255, Math.floor((yFrac - tileY) * 256)));
  
  return { tileX, tileY, pixelX, pixelY };
}

const TILE_CACHE = new Map<string, ImageData>();
const MAX_TILE_CACHE_SIZE = 100;

/**
 * Fetches the elevation tile and retrieves its ImageData, utilizing a local cache.
 */
async function getTileImageData(tileX: number, tileY: number): Promise<ImageData | null> {
  const url = `https://s3.amazonaws.com/elevation-tiles-prod/terrarium/14/${tileX}/${tileY}.png`;
  
  if (TILE_CACHE.has(url)) {
    return TILE_CACHE.get(url)!;
  }
  
  try {
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`HTTP error ${response.status}`);
    }
    const blob = await response.blob();
    const imageBitmap = await createImageBitmap(blob);
    
    // Create offscreen canvas and draw the image to extract pixel data
    const canvas = new OffscreenCanvas(256, 256);
    const ctx = canvas.getContext('2d');
    if (!ctx) {
      throw new Error('Failed to get 2D context for OffscreenCanvas');
    }
    ctx.drawImage(imageBitmap, 0, 0);
    const imageData = ctx.getImageData(0, 0, 256, 256);
    
    // Store in cache with LRU eviction
    if (TILE_CACHE.size >= MAX_TILE_CACHE_SIZE) {
      const firstKey = TILE_CACHE.keys().next().value;
      if (firstKey !== undefined) {
        TILE_CACHE.delete(firstKey);
      }
    }
    TILE_CACHE.set(url, imageData);
    return imageData;
  } catch (err) {
    console.warn(`[spatial.worker] Failed to fetch or decode elevation tile ${tileX}/${tileY}:`, err);
    return null;
  }
}

/**
 * Douglas-Peucker polyline simplification algorithm adapted for 2D curve simplification (distance vs elevation).
 * Marks which points to keep based on the perpendicular distance threshold (epsilon).
 */
function simplifyDouglasPeucker(
  d: Float64Array,
  z: Float64Array,
  start: number,
  end: number,
  epsilon: number,
  keep: boolean[]
): void {
  if (end <= start + 1) return;
  
  let maxDist = 0;
  let maxIndex = 0;
  
  const dStart = d[start];
  const zStart = z[start];
  const dEnd = d[end];
  const zEnd = z[end];
  
  const dx = dEnd - dStart;
  const dy = zEnd - zStart;
  const len = Math.sqrt(dx * dx + dy * dy);
  
  for (let i = start + 1; i < end; i++) {
    const dist =
      len === 0
        ? Math.sqrt(
            (d[i] - dStart) * (d[i] - dStart) + (z[i] - zStart) * (z[i] - zStart)
          )
        : Math.abs(dy * d[i] - dx * z[i] + dEnd * zStart - zEnd * dStart) / len;

    if (dist > maxDist) {
      maxDist = dist;
      maxIndex = i;
    }
  }
  
  if (maxDist > epsilon) {
    keep[maxIndex] = true;
    simplifyDouglasPeucker(d, z, start, maxIndex, epsilon, keep);
    simplifyDouglasPeucker(d, z, maxIndex, end, epsilon, keep);
  }
}

/**
 * Smooths elevation data by running Douglas-Peucker on the cumulative distance vs elevation curve,
 * then linearly interpolating the simplified points.
 */
function smoothElevationProfile(
  coords: Float64Array,
  rawElevations: Float64Array,
  epsilon: number = 3.0
): Float64Array {
  const n = rawElevations.length;
  if (n <= 2) {
    return new Float64Array(rawElevations); // Too short to smooth
  }
  
  // 1. Calculate cumulative distances along the route using haversineMeters
  const d = new Float64Array(n);
  d[0] = 0;
  for (let i = 1; i < n; i++) {
    const prevLng = coords[(i - 1) * 2];
    const prevLat = coords[(i - 1) * 2 + 1];
    const currLng = coords[i * 2];
    const currLat = coords[i * 2 + 1];
    d[i] = d[i - 1] + haversineMeters(prevLat, prevLng, currLat, currLng);
  }
  
  // 2. Identify key points to keep
  const keep = new Array<boolean>(n).fill(false);
  keep[0] = true;
  keep[n - 1] = true;
  simplifyDouglasPeucker(d, rawElevations, 0, n - 1, epsilon, keep);
  
  // 3. Interpolate smoothed elevations
  const smoothed = new Float64Array(n);
  let leftIdx = 0;
  
  for (let i = 0; i < n; i++) {
    if (keep[i]) {
      smoothed[i] = rawElevations[i];
      leftIdx = i;
    } else {
      // Find the next kept point to the right
      let rightIdx = i + 1;
      while (rightIdx < n && !keep[rightIdx]) {
        rightIdx++;
      }
      
      const dLeft = d[leftIdx];
      const dRight = d[rightIdx];
      const zLeft = rawElevations[leftIdx];
      const zRight = rawElevations[rightIdx];
      
      if (dRight === dLeft) {
        smoothed[i] = zLeft;
      } else {
        const ratio = (d[i] - dLeft) / (dRight - dLeft);
        smoothed[i] = zLeft + ratio * (zRight - zLeft);
      }
    }
  }
  
  return smoothed;
}

/**
 * Handles the ELEVATION_PROFILE_REQUEST message, sampling Terrarium elevation tiles
 * at zoom 14, smoothing the profile via Douglas-Peucker, and returning a zero-copy response.
 */
async function handleElevationProfile(id: string, payload: ElevationProfileRequestPayload): Promise<void> {
  const { coordinates } = payload;
  if (!coordinates || coordinates.length < 2) {
    throw new Error('Invalid coordinates: must contain at least 1 point');
  }
  
  const n = coordinates.length / 2;
  const rawElevations = new Float64Array(n);
  
  // 1. Group point indices by tile key
  const tileGroups = new Map<string, { tileX: number; tileY: number; points: { idx: number; pixelX: number; pixelY: number }[] }>();
  
  for (let i = 0; i < n; i++) {
    const lng = coordinates[i * 2];
    const lat = coordinates[i * 2 + 1];
    const { tileX, tileY, pixelX, pixelY } = lngLatToTile(lng, lat, 14);
    const key = `${tileX}_${tileY}`;
    
    let group = tileGroups.get(key);
    if (!group) {
      group = { tileX, tileY, points: [] };
      tileGroups.set(key, group);
    }
    group.points.push({ idx: i, pixelX, pixelY });
  }
  
  // 2. Fetch tiles and decode elevation values for each point
  for (const group of tileGroups.values()) {
    const imageData = await getTileImageData(group.tileX, group.tileY);
    
    for (const pt of group.points) {
      if (imageData) {
        const pixelIdx = (pt.pixelY * 256 + pt.pixelX) * 4;
        const r = imageData.data[pixelIdx];
        const g = imageData.data[pixelIdx + 1];
        const b = imageData.data[pixelIdx + 2];
        
        // Terrarium formula: elevation = (R * 256 + G + B / 256) - 32768
        const elevation = (r * 256 + g + b / 256) - 32768;
        rawElevations[pt.idx] = elevation;
      } else {
        // Fallback to 0 if tile data is unavailable
        rawElevations[pt.idx] = 0;
      }
    }
  }
  
  // 3. Smooth the track elevations using Douglas-Peucker
  const smoothed = smoothElevationProfile(coordinates, rawElevations, 3.0);
  
  // 4. Calculate total ascent and descent
  let totalAscent = 0;
  let totalDescent = 0;
  for (let i = 1; i < n; i++) {
    const diff = smoothed[i] - smoothed[i - 1];
    if (diff > 0) {
      totalAscent += diff;
    } else {
      totalDescent += Math.abs(diff);
    }
  }
  
  // 5. Zero-Copy return
  const successPayload: ElevationProfileSuccessPayload = {
    totalAscent,
    totalDescent,
    elevations: smoothed,
  };
  
  const response: WorkerResponseMessage = {
    id,
    type: 'ELEVATION_PROFILE_SUCCESS',
    payload: successPayload,
  };
  
  self.postMessage(response, [smoothed.buffer]);
}
