import { useEffect, useRef, useState, useCallback } from 'react';
import maplibregl from 'maplibre-gl';
import { Protocol, PMTiles } from 'pmtiles';
import type { Source, RangeResponse } from 'pmtiles';
import type { WorkerRequestMessage, WorkerResponseMessage } from '../../shared/types';
import 'maplibre-gl/dist/maplibre-gl.css';
import mlcontour from 'maplibre-contour';
import { startTracking, stopTracking, type TrackerHandle } from '../services/locationTracker';

// ---------------------------------------------------------------------------
// Telemetry (OPFS byte-range reads from the mapData worker)
// ---------------------------------------------------------------------------

export interface TelemetryData {
  activeRequests: number;
  lastFetchTime: number;
  lastFetchSize: number;
  totalBytes: number;
}

// ---------------------------------------------------------------------------
// Spatial worker proximity HUD state
// ---------------------------------------------------------------------------

interface NearestTrail {
  found: boolean;
  name: string;
  highway: string;
  distanceMeters: number;
}

// ---------------------------------------------------------------------------
// Scan state machine
// ---------------------------------------------------------------------------

type ScanStatus = 'idle' | 'scanning' | 'indexed' | 'error';

// ---------------------------------------------------------------------------
// WorkerPMTilesSource — bridges PMTiles byte-range calls to the mapData worker
// ---------------------------------------------------------------------------

class WorkerPMTilesSource implements Source {
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

  /** PMTiles protocol uses the key to de-duplicate sources in its registry. */
  getKey() { return this.filename; }

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
// Global PMTiles protocol singleton (survives HMR)
// ---------------------------------------------------------------------------

let globalProtocol: Protocol | null = null;
function getGlobalProtocol(): Protocol {
  if (!globalProtocol) {
    globalProtocol = new Protocol();
    maplibregl.addProtocol('pmtiles', globalProtocol.tile);
  }
  return globalProtocol;
}

// ---------------------------------------------------------------------------
// Static hiking location presets
// ---------------------------------------------------------------------------

interface HikeLocation {
  name: string;
  region: string;
  coords: [number, number];
  zoom: number;
}

const HIKE_LOCATIONS: HikeLocation[] = [
  // All three points are within the alps_basemap.pmtiles / alps_terrain.pmtiles
  // boundary box (approx. z5-z14, Innsbruck/Alps region).
  { name: 'Innsbruck Center', region: 'Tyrol, Austria', coords: [11.3908, 47.2757], zoom: 12 },
  { name: 'Nordkette Range',  region: 'Alps, Austria',  coords: [11.3794, 47.3061], zoom: 13 },
  { name: 'Patscherkofel',    region: 'Alps, Austria',  coords: [11.4619, 47.2086], zoom: 13 },
];

// ---------------------------------------------------------------------------
// MapLibre source/layer IDs for the OSM trail overlay
// ---------------------------------------------------------------------------

const TRAIL_SOURCE_ID = 'osm-trails';
const TRAIL_LAYER_ID  = 'osm-trails-layer';

// ---------------------------------------------------------------------------
// Throttle helper (ref-based, no external dep)
// ---------------------------------------------------------------------------

/** Returns true if enough time has elapsed since the last call. */
function makeThrottle(intervalMs: number) {
  let lastCall = 0;
  return () => {
    const now = performance.now();
    if (now - lastCall >= intervalMs) {
      lastCall = now;
      return true;
    }
    return false;
  };
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

/**
 * Imperative handle for swapping the active offline region without unmounting
 * the map.  Handed to the parent via onRegionSwitcherReady.
 */
export interface OfflineRegionSwitcher {
  /**
   * Swap both map tile sources to new OPFS files.
   *
   * @param basemapFile  Filename in OPFS for the new vector basemap (e.g. 'active_map.pmtiles').
   * @param terrainFile  Filename in OPFS for the new terrain raster (e.g. 'alps_terrain.pmtiles').
   *
   * The method:
   *  1. Sends LOAD_OFFLINE_REGION to the worker so it opens fresh SyncAccessHandles.
   *  2. Creates new WorkerPMTilesSource + PMTiles instances for both files.
   *  3. Registers them with the global Protocol so pmtiles:// URLs resolve.
   *  4. Calls setUrl() on the live MapLibre sources — no full style reload.
   */
  loadOfflineRegion(basemapFile: string, terrainFile: string): Promise<void>;
}

export interface MapViewProps {
  routingWorker: Worker | null;
  calculatedRoute: {
    coordinatesBuffer: ArrayBuffer;
    distanceMeters: number;
  } | null;
  clearRoute: () => void;
  /** Hovered elevation chart index — drives the crosshair on the map. */
  hoveredElevIndex?: number | null;
  /** Called once with the spatial Worker instance so App can post to it. */
  onSpatialWorkerReady?: (worker: Worker) => void;
  /** Called once with the mapData Worker instance so App can send DOWNLOAD_REGION_REQUEST. */
  onMapDataWorkerReady?: (worker: Worker) => void;
  /** Called when the user confirms a region download; receives the current bounds. */
  onRegionDownload?: (bounds: maplibregl.LngLatBounds) => void;
  /** Current state of an in-progress download (drives the confirm panel). */
  downloadStatus?: 'idle' | 'fetching' | 'writing' | 'done' | 'error';
  /** Progress label to show inside the panel while fetching/writing. */
  downloadProgressLabel?: string;
  /**
   * Called once after the map finishes loading with an imperative switcher
   * object.  The parent can store it in a ref and call
   * switcher.loadOfflineRegion() whenever the user selects a new region.
   */
  onRegionSwitcherReady?: (switcher: OfflineRegionSwitcher) => void;
}

export default function MapView({
  routingWorker,
  calculatedRoute,
  clearRoute,
  hoveredElevIndex = null,
  onSpatialWorkerReady,
  onMapDataWorkerReady,
  onRegionDownload,
  downloadStatus        = 'idle',
  downloadProgressLabel = '',
  onRegionSwitcherReady,
}: MapViewProps) {
  const mapContainerRef  = useRef<HTMLDivElement>(null);
  const mapRef           = useRef<maplibregl.Map | null>(null);
  const mapDataWorkerRef = useRef<Worker | null>(null);   // OPFS / PMTiles worker
  const spatialWorkerRef = useRef<Worker | null>(null);   // Spatial index worker
  const throttleRef      = useRef(makeThrottle(120));     // 120 ms proximity throttle
  const pendingNearestId = useRef<string | null>(null);   // de-duplicate in-flight queries
  /** Flat route coordinates mirror — populated whenever calculatedRoute changes. */
  const routeCoordsRef   = useRef<[number, number][]>([]);

  // Map bootstrap state
  const [initStatus,     setInitStatus]     = useState<'idle' | 'initializing' | 'ready' | 'error'>('idle');
  const [statusMessage,  setStatusMessage]  = useState('Booting offline mapping engines...');
  const [fileSize,       setFileSize]       = useState<number | null>(null);
  const [selectedHike,   setSelectedHike]   = useState(HIKE_LOCATIONS[0].name);

  // Tile-read telemetry
  const [telemetry, setTelemetry] = useState<TelemetryData>({
    activeRequests: 0,
    lastFetchTime: 0,
    lastFetchSize: 0,
    totalBytes: 0,
  });

  // Spatial scan state
  const [scanStatus,    setScanStatus]    = useState<ScanStatus>('idle');
  const [trailCount,    setTrailCount]    = useState(0);
  const [indexKb,       setIndexKb]       = useState(0);
  const [scanError,     setScanError]     = useState<string | null>(null);

  // Proximity HUD
  const [nearest,       setNearest]       = useState<NearestTrail | null>(null);

  // ---------------------------------------------------------------------------
  // Phase 9: Live GPS location tracking
  // ---------------------------------------------------------------------------

  /** Whether the map camera should auto-follow the GPS position. */
  const [isTrackingCamera, setIsTrackingCamera] = useState(false);
  /** Ref mirror of isTrackingCamera so the long-lived GPS watcher closure
   *  always reads the current value without needing to re-register. */
  const isTrackingCameraRef = useRef(false);
  /** Stores the active tracker handle so we can stop it on unmount.
   *  TrackerHandle is a tagged union — kind:'native' holds a string watcher
   *  ID from the Capacitor plugin; kind:'web' holds a number from the
   *  Web Geolocation API.  stopTracking() selects the right path. */
  const gpsWatchIdRef = useRef<TrackerHandle | null>(null);
  /** Latest known GPS fix — [lng, lat] + accuracy metres. */
  const userLocationRef   = useRef<{ lng: number; lat: number; accuracy: number } | null>(null);

  // ---------------------------------------------------------------------------
  // Phase 10: Download mode UI state
  // ---------------------------------------------------------------------------

  /** Whether the download zone overlay and confirm panel are visible. */
  const [isDownloadMode, setIsDownloadMode] = useState(false);

  // Start and end coordinates for routing clicks
  const [startPt, setStartPt] = useState<[number, number] | null>(null);
  const [endPt, setEndPt] = useState<[number, number] | null>(null);

  // ---------------------------------------------------------------------------
  // Boot: mapData worker + MapLibre map
  // ---------------------------------------------------------------------------

  useEffect(() => {
    let active = true;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setInitStatus('initializing');
    setStatusMessage('Spawning map data worker…');

    const mapDataWorker = new Worker(
      new URL('../../workers/mapData.worker.ts', import.meta.url),
      { type: 'module' },
    );
    mapDataWorkerRef.current = mapDataWorker;
    onMapDataWorkerReady?.(mapDataWorker);

    // The two OPFS filenames that back the style's pmtiles:// sources.
    // These names match the URL fragments in high_contrast_outdoor_style.json:
    //   pmtiles://local/alps_basemap.pmtiles  → basemap-local MapLibre source
    //   pmtiles://local/alps_terrain.pmtiles  → terrain-local MapLibre source
    const DEFAULT_BASEMAP  = 'alps_basemap.pmtiles';
    const DEFAULT_TERRAIN  = 'alps_terrain.pmtiles';

    const protocol = getGlobalProtocol();
    let   map: maplibregl.Map | null = null;

    // Helper: create a WorkerPMTilesSource + PMTiles pair and register it
    // with the global protocol.  Safe to call multiple times for the same
    // filename — PMTiles.getKey() de-duplicates within the protocol registry.
    const registerSource = (
      filename: string,
      onTelemetry?: (d: TelemetryData) => void,
    ): PMTiles => {
      const src      = new WorkerPMTilesSource(filename, mapDataWorker, onTelemetry);
      const instance = new PMTiles(src);
      protocol.add(instance);
      return instance;
    };

    const initId = Math.random().toString(36).substring(2, 9);

    const handleInitMessage = (event: MessageEvent<WorkerResponseMessage>) => {
      const response = event.data;
      if (response.id !== initId) return;
      mapDataWorker.removeEventListener('message', handleInitMessage);

      if (response.type === 'MAP_INIT_SUCCESS') {
        if (!active) return;
        const sizeBytes = response.payload.size as number;
        setFileSize(sizeBytes);
        setStatusMessage(`OPFS storage bound. Database: ${(sizeBytes / 1024 / 1024).toFixed(2)} MB`);

        // Register the two default sources (basemap + terrain) upfront.
        registerSource(DEFAULT_BASEMAP, (tData) => {
          setTelemetry(prev => ({
            ...tData,
            totalBytes: tData.totalBytes > 0 ? tData.totalBytes : prev.totalBytes,
          }));
        });
        registerSource(DEFAULT_TERRAIN);

        if (mapContainerRef.current) {
          setStatusMessage('Mounting map container canvas…');

          map = new maplibregl.Map({
            container: mapContainerRef.current,
            // ── Phase Block-1: High-contrast 3D alpine style ─────────────
            // External style JSON bundles sources (basemap-local + terrain-local),
            // the terrain block (exaggeration 1.3), hillshading, contours,
            // dynamic trail rendering, and peak labels.
            style: '/styles/high_contrast_outdoor_style.json',
            center: HIKE_LOCATIONS[0].coords,
            zoom: HIKE_LOCATIONS[0].zoom,
            maxZoom: 18,
            // Prevent zooming out past the tile extent of our offline
            // alps_basemap.pmtiles / alps_terrain.pmtiles files (z5-z14).
            // z4 gives just enough overview to orient without hitting empty space.
            minZoom: 4,
            // GPU fill-rate guardrail: prevents horizon overdraw that exhausts
            // mobile GPU bandwidth at high pitch angles.
            maxPitch: 60,
          });

          const activeMap = map;
          mapRef.current = activeMap;
          activeMap.on('load', () => {
            // ── Memory management: WebGL texture-cache OOM guards ────────
            // Clamp each source cache to 25 tiles max to prevent GPU VRAM
            // exhaustion on memory-constrained mobile devices.
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            (activeMap as any).style?.sourceCaches?.['basemap-local']?.setMaxTiles(25);
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            (activeMap as any).style?.sourceCaches?.['terrain-local']?.setMaxTiles(25);

            // ── Expose imperative region-switcher to the parent ───────────
            // loadOfflineRegion() lets App (or a future download manager UI)
            // hot-swap both tile sources without a full style reload.
            if (onRegionSwitcherReady) {
              const switcher: OfflineRegionSwitcher = {
                async loadOfflineRegion(basemapFile: string, terrainFile: string) {
                  const worker = mapDataWorkerRef.current;
                  if (!worker) throw new Error('[loadOfflineRegion] mapData worker not ready.');

                  // 1. Ask the worker to open SyncAccessHandles for both new files.
                  const loadId  = Math.random().toString(36).substring(2, 9);
                  await new Promise<void>((resolve, reject) => {
                    const onMsg = (ev: MessageEvent<WorkerResponseMessage>) => {
                      if (ev.data.id !== loadId) return;
                      worker.removeEventListener('message', onMsg);
                      if (ev.data.type === 'LOAD_OFFLINE_REGION_SUCCESS') resolve();
                      else reject(new Error(ev.data.error ?? 'LOAD_OFFLINE_REGION failed'));
                    };
                    worker.addEventListener('message', onMsg);
                    worker.postMessage({
                      id:      loadId,
                      type:    'LOAD_OFFLINE_REGION',
                      payload: { filenames: [basemapFile, terrainFile] },
                    } satisfies WorkerRequestMessage);
                  });

                  // 2. Register new WorkerPMTilesSource + PMTiles instances.
                  registerSource(basemapFile);
                  registerSource(terrainFile);

                  // 3. Swap the live MapLibre sources to the new pmtiles:// URLs.
                  //    setUrl() updates the source in-place — no layer teardown.
                  const bSrc = activeMap.getSource('basemap-local') as (maplibregl.RasterTileSource & { setUrl?: (url: string) => void }) | undefined;
                  const tSrc = activeMap.getSource('terrain-local') as (maplibregl.RasterTileSource & { setUrl?: (url: string) => void }) | undefined;

                  if (bSrc?.setUrl) bSrc.setUrl(`pmtiles://local/${basemapFile}`);
                  if (tSrc?.setUrl) tSrc.setUrl(`pmtiles://local/${terrainFile}`);

                  console.log(
                    `[MapView] Offline region swapped → basemap: ${basemapFile}, terrain: ${terrainFile}`,
                  );
                },
              };
              onRegionSwitcherReady(switcher);
            }

            // Instantiate and setup demSource for dynamic contours
            const demSource = new mlcontour.DemSource({
              url: 'https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png',
              encoding: 'terrarium',
              maxzoom: 15,
              worker: true,
            });
            demSource.setupMaplibre(maplibregl);

            // Add the contour vector source
            activeMap.addSource('contour-source', {
              type: 'vector',
              tiles: [
                demSource.contourProtocolUrl({
                  multiplier: 1,
                  thresholds: {
                    9: [20, 100],
                  },
                }),
              ],
              maxzoom: 15,
            });

            // Add contour lines layer
            activeMap.addLayer({
              id: 'contour-lines-layer',
              type: 'line',
              source: 'contour-source',
              'source-layer': 'contours',
              layout: {
                'line-join': 'round',
                'line-cap': 'round',
              },
              paint: {
                'line-color': [
                  'case',
                  ['>=', ['get', 'level'], 1],
                  'rgba(148, 163, 184, 0.35)', // major lines: slate-400 at 35% opacity
                  'rgba(148, 163, 184, 0.15)', // minor lines: slate-400 at 15% opacity
                ],
                'line-width': [
                  'case',
                  ['>=', ['get', 'level'], 1],
                  1.2, // major line thickness
                  0.6, // minor line thickness
                ],
              },
            });

            // Add contour labels layer
            activeMap.addLayer({
              id: 'contour-labels-layer',
              type: 'symbol',
              source: 'contour-source',
              'source-layer': 'contours',
              filter: ['>=', ['get', 'level'], 1], // only label major lines
              layout: {
                'symbol-placement': 'line',
                'text-field': ['concat', ['to-string', ['get', 'ele']], 'm'],
                'text-size': 9,
                'text-max-angle': 45,
                'text-pitch-alignment': 'viewport',
                'text-rotation-alignment': 'map',
                'text-keep-upright': true,
              },
              paint: {
                'text-color': 'rgba(148, 163, 184, 0.7)',
                'text-halo-color': '#020617', // match slate-950 background
                'text-halo-width': 1,
              },
            });

            // ── Phase 9: user-location source + layers ──────────────────────
            // Registered once on load; data is updated dynamically by the GPS
            // watcher via source.setData() — no layer teardown ever needed.
            const emptyFC = { type: 'FeatureCollection' as const, features: [] as any[] };
            activeMap.addSource('user-location-source', {
              type: 'geojson',
              data: emptyFC,
            });

            // Accuracy halo — translucent blue fill polygon (circle approximation)
            activeMap.addLayer({
              id:     'user-location-accuracy',
              type:   'circle',
              source: 'user-location-source',
              filter: ['==', ['get', 'type'], 'accuracy'],
              paint: {
                // radius driven by accuracy metres; scale with zoom via
                // a style expression so it stays geographically sized.
                'circle-radius': [
                  'interpolate', ['exponential', 2], ['zoom'],
                  0,  0,
                  20, [
                    '/',
                    ['*', ['get', 'accuracyMeters'], ['cos', ['*', ['/', ['get', 'lat'], 180], Math.PI]]],
                    0.075,  // metres-per-pixel at zoom 0 (web mercator constant)
                  ],
                ],
                'circle-color':   '#3b82f6',
                'circle-opacity': 0.15,
                'circle-stroke-color':   '#3b82f6',
                'circle-stroke-width':   1,
                'circle-stroke-opacity': 0.35,
              },
            });

            // Core blue dot
            activeMap.addLayer({
              id:     'user-location-dot',
              type:   'circle',
              source: 'user-location-source',
              filter: ['==', ['get', 'type'], 'dot'],
              paint: {
                'circle-radius':       6,
                'circle-color':        '#2563eb',
                'circle-stroke-color': '#ffffff',
                'circle-stroke-width': 2.5,
              },
            });

            setInitStatus('ready');
          });
          activeMap.on('error', (e) => console.error('[MapLibre]', e));
        }
      } else {
        setInitStatus('error');
        setStatusMessage(`Initialization failed: ${response.error ?? 'Unknown error'}`);
      }
    };

    mapDataWorker.addEventListener('message', handleInitMessage);
    setStatusMessage('Syncing binary file buffers to OPFS…');
    // Send the two default filenames so the worker opens SyncAccessHandles
    // for both OPFS files during init.  Subsequent MAP_READ_BYTES requests
    // will find the handles ready without an extra round-trip.
    mapDataWorker.postMessage({
      id:      initId,
      type:    'MAP_INIT',
      payload: { filenames: [DEFAULT_BASEMAP, DEFAULT_TERRAIN] },
    } satisfies WorkerRequestMessage);

    return () => {
      active = false;
      try {
        mapDataWorker.postMessage({
          id:      'close-id',
          type:    'MAP_CLOSE',
          payload: null,
        } satisfies WorkerRequestMessage);
      } catch (err) {
        // Ignored if worker is already terminated/unresponsive
      }

      // Clear the global protocol tiles registry to release any references/handles
      // to offline files on map unmount
      try {
        getGlobalProtocol().tiles.clear();
      } catch (err) {
        console.warn('[MapView] Failed to clear global protocol cache:', err);
      }

      // Explicitly remove map instances to free map canvas/WebGL context
      map?.remove();
      mapRef.current?.remove();
      mapRef.current = null;

      mapDataWorker.terminate();
    };
  }, []);

  // ---------------------------------------------------------------------------
  // Boot: spatial worker + its global message dispatcher
  // ---------------------------------------------------------------------------

  useEffect(() => {
    const spatialWorker = new Worker(
      new URL('../../workers/spatial.worker.ts', import.meta.url),
      { type: 'module' },
    );
    spatialWorkerRef.current = spatialWorker;
    // Hand the worker reference back to App so it can post ELEVATION_PROFILE_REQUEST
    onSpatialWorkerReady?.(spatialWorker);

    const handleSpatialMessage = (event: MessageEvent<WorkerResponseMessage>) => {
      const { type, payload, error } = event.data;

      // ── TRAILS_INDEX_COMPLIANCE ──────────────────────────────────────────
      if (type === 'TRAILS_INDEX_COMPLIANCE') {
        const featureCount = payload.featureCount as number;
        const indexBytes   = payload.indexBytes   as number;
        const geojson      = payload.geojson      as string | undefined;

        setTrailCount(featureCount);
        setIndexKb(Math.round(indexBytes / 1024));
        setScanStatus('indexed');
        setScanError(null);

        // Inject the GeoJSON FeatureCollection into the MapLibre source.
        const map = mapRef.current;
        if (map && geojson) {
          const parsed = JSON.parse(geojson) as { type: 'FeatureCollection'; features: any[] };

          if (map.getSource(TRAIL_SOURCE_ID)) {
            // Source already exists from a previous scan — just update data.
            (map.getSource(TRAIL_SOURCE_ID) as maplibregl.GeoJSONSource).setData(parsed);
          } else {
            // First scan: register source + layer.
            map.addSource(TRAIL_SOURCE_ID, {
              type: 'geojson',
              data: parsed,
            });

            map.addLayer({
              id:     TRAIL_LAYER_ID,
              type:   'line',
              source: TRAIL_SOURCE_ID,
              layout: {
                'line-join': 'round',
                'line-cap':  'round',
              },
              paint: {
                // Glowing emerald trail lines
                'line-color':   '#10b981',
                'line-width':   [
                  'interpolate', ['linear'], ['zoom'],
                  3,  1.5,
                  8,  2.5,
                  14, 4,
                ],
                'line-opacity': 0.9,
                'line-blur':    0.4,
              },
            });

            // Second pass: add a wider, low-opacity glow layer beneath.
            map.addLayer(
              {
                id:     `${TRAIL_LAYER_ID}-glow`,
                type:   'line',
                source: TRAIL_SOURCE_ID,
                layout: {
                  'line-join': 'round',
                  'line-cap':  'round',
                },
                paint: {
                  'line-color':   '#34d399',
                  'line-width':   [
                    'interpolate', ['linear'], ['zoom'],
                    3,  4,
                    8,  8,
                    14, 14,
                  ],
                  'line-opacity': 0.15,
                  'line-blur':    6,
                },
              },
              TRAIL_LAYER_ID, // insert glow *below* the sharp line
            );
          }
        }
        return;
      }

      // ── TRAILS_NEAREST_RESPONSE ──────────────────────────────────────────
      if (type === 'TRAILS_NEAREST_RESPONSE') {
        pendingNearestId.current = null;
        if (payload.found) {
          setNearest({
            found:          true,
            name:           payload.name          as string,
            highway:        payload.highway        as string,
            distanceMeters: payload.distanceMeters as number,
          });
        } else {
          setNearest(null);
        }
        return;
      }

      // ── ERROR ────────────────────────────────────────────────────────────
      if (type === 'ERROR') {
        console.error('[spatial.worker] RPC error:', error);
        setScanStatus('error');
        setScanError(error ?? 'Spatial worker error');
      }
    };

    spatialWorker.addEventListener('message', handleSpatialMessage);

    return () => {
      spatialWorker.removeEventListener('message', handleSpatialMessage);
      spatialWorker.terminate();
    };
  // onSpatialWorkerReady is intentionally omitted: the spatial worker is
  // instantiated exactly once on mount; re-running on prop changes would
  // spawn duplicate workers and leak message listeners.
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ---------------------------------------------------------------------------
  // MapLibre mousemove → throttled TRAILS_QUERY_NEAREST
  // ---------------------------------------------------------------------------

  useEffect(() => {
    if (initStatus !== 'ready') return;
    const map = mapRef.current;
    if (!map) return;

    const handleMouseMove = (e: maplibregl.MapMouseEvent) => {
      if (!throttleRef.current()) return;
      const spatialWorker = spatialWorkerRef.current;
      if (!spatialWorker) return;

      // Drop any previous in-flight nearest query — result would be stale.
      const msgId = Math.random().toString(36).substring(2, 9);
      pendingNearestId.current = msgId;

      const req: WorkerRequestMessage = {
        id:      msgId,
        type:    'TRAILS_QUERY_NEAREST',
        payload: { lng: e.lngLat.lng, lat: e.lngLat.lat },
      };
      spatialWorker.postMessage(req);
    };

    map.on('mousemove', handleMouseMove);
    return () => { map.off('mousemove', handleMouseMove); };
  }, [initStatus]);

  // ---------------------------------------------------------------------------
  // "Scan Viewport" button handler
  // ---------------------------------------------------------------------------

  const handleScanViewport = useCallback(() => {
    const map = mapRef.current;
    const spatialWorker = spatialWorkerRef.current;
    if (!map || !spatialWorker || scanStatus === 'scanning') return;

    const bounds = map.getBounds();
    const bbox: [number, number, number, number] = [
      bounds.getSouth(),
      bounds.getWest(),
      bounds.getNorth(),
      bounds.getEast(),
    ];

    setScanStatus('scanning');
    setScanError(null);
    setNearest(null);

    const msgId = Math.random().toString(36).substring(2, 9);
    const req: WorkerRequestMessage = {
      id:      msgId,
      type:    'TRAILS_FETCH_BOUNDS',
      payload: { bbox },
    };

    console.log(
      '%c[MapView] Dispatching TRAILS_FETCH_BOUNDS',
      'color:#10b981;font-weight:bold;',
      { bbox },
    );

    spatialWorker.postMessage(req);
  }, [scanStatus]);

  // Synchronise clicked start/end points to the MapLibre circles layer
  const updateMarkersLayer = useCallback((start: [number, number] | null, end: [number, number] | null) => {
    const map = mapRef.current;
    if (!map) return;

    const features = [];
    if (start) {
      features.push({
        type: 'Feature' as const,
        properties: { role: 'start' },
        geometry: { type: 'Point' as const, coordinates: start }
      });
    }
    if (end) {
      features.push({
        type: 'Feature' as const,
        properties: { role: 'end' },
        geometry: { type: 'Point' as const, coordinates: end }
      });
    }

    const data = { type: 'FeatureCollection' as const, features };
    const source = map.getSource('markers-source') as maplibregl.GeoJSONSource | undefined;
    if (source) {
      source.setData(data);
    } else {
      map.addSource('markers-source', { type: 'geojson', data });
      map.addLayer({
        id: 'markers-layer',
        type: 'circle',
        source: 'markers-source',
        paint: {
          'circle-radius': 7,
          'circle-color': [
            'case',
            ['==', ['get', 'role'], 'start'], '#10b981', // green for start
            '#ef4444' // red for end
          ],
          'circle-stroke-width': 2,
          'circle-stroke-color': '#020617'
        }
      });
    }
  }, []);

  // MapLibre click → handles start/end coordinates and triggers routing requests
  useEffect(() => {
    if (initStatus !== 'ready') return;
    const map = mapRef.current;
    if (!map) return;

    const handleMapClick = (e: maplibregl.MapMouseEvent) => {
      const clicked: [number, number] = [e.lngLat.lng, e.lngLat.lat];

      if (!startPt || (startPt && endPt)) {
        // Clear existing route and markers
        setStartPt(clicked);
        setEndPt(null);
        clearRoute();
        updateMarkersLayer(clicked, null);
      } else {
        // Set end point and post request to routing worker
        setEndPt(clicked);
        updateMarkersLayer(startPt, clicked);

        if (routingWorker) {
          const requestId = Math.random().toString(36).substring(2, 9);
          routingWorker.postMessage({
            id: requestId,
            type: 'ROUTE_CALCULATE_REQUEST',
            payload: {
              startX: startPt[0],
              startY: startPt[1],
              endX: clicked[0],
              endY: clicked[1],
              costingModel: 'pedestrian',
            },
          } satisfies WorkerRequestMessage);
        }
      }
    };

    map.on('click', handleMapClick);
    return () => {
      map.off('click', handleMapClick);
    };
  }, [initStatus, startPt, endPt, routingWorker, clearRoute, updateMarkersLayer]);

  // Reconstruct GeoJSON and draw route line from binary buffer coordinate updates
  useEffect(() => {
    const map = mapRef.current;
    if (!map || initStatus !== 'ready') return;

    if (!calculatedRoute) {
      const source = map.getSource('route-source') as maplibregl.GeoJSONSource | undefined;
      if (source) {
        source.setData({ type: 'FeatureCollection', features: [] });
      }
      return;
    }

    const flatCoords = new Float64Array(calculatedRoute.coordinatesBuffer);
    const coords: [number, number][] = [];
    for (let i = 0; i < flatCoords.length; i += 2) {
      coords.push([flatCoords[i], flatCoords[i + 1]]);
    }
    // Stash for the crosshair effect below.
    routeCoordsRef.current = coords;

    const geojson = {
      type: 'FeatureCollection' as const,
      features: [
        {
          type: 'Feature' as const,
          properties: {},
          geometry: {
            type: 'LineString' as const,
            coordinates: coords,
          },
        },
      ],
    };

    const source = map.getSource('route-source') as maplibregl.GeoJSONSource | undefined;
    if (source) {
      source.setData(geojson);
    } else {
      map.addSource('route-source', { type: 'geojson', data: geojson });
      map.addLayer({
        id: 'route-layer',
        type: 'line',
        source: 'route-source',
        layout: {
          'line-join': 'round',
          'line-cap': 'round',
        },
        paint: {
          'line-color': '#3b82f6', // blue
          'line-width': 4,
          'line-opacity': 0.85,
        },
      });
    }

    // Automatically zoom/pan to fit the newly loaded or calculated route
    if (coords.length > 0) {
      const bounds = new maplibregl.LngLatBounds();
      for (const coord of coords) {
        bounds.extend(coord);
      }
      map.fitBounds(bounds, { padding: 80, maxZoom: 15, duration: 1000 });
    }
  }, [calculatedRoute, initStatus]);

  // ---------------------------------------------------------------------------
  // Crosshair pulse marker — driven by hoveredElevIndex from ElevationProfile
  // ---------------------------------------------------------------------------

  useEffect(() => {
    const map = mapRef.current;
    if (!map || initStatus !== 'ready') return;

    const CROSSHAIR_SOURCE = 'crosshair-source';
    const CROSSHAIR_LAYER  = 'crosshair-layer';
    const CROSSHAIR_PULSE  = 'crosshair-pulse';

    const coord =
      hoveredElevIndex !== null && routeCoordsRef.current.length > 0
        ? routeCoordsRef.current[Math.min(hoveredElevIndex, routeCoordsRef.current.length - 1)]
        : null;

    const data = coord
      ? { type: 'FeatureCollection' as const, features: [{
          type: 'Feature' as const,
          properties: {},
          geometry: { type: 'Point' as const, coordinates: coord },
        }] }
      : { type: 'FeatureCollection' as const, features: [] };

    const source = map.getSource(CROSSHAIR_SOURCE) as maplibregl.GeoJSONSource | undefined;
    if (source) {
      source.setData(data);
    } else {
      map.addSource(CROSSHAIR_SOURCE, { type: 'geojson', data });

      // Outer pulse ring
      map.addLayer({
        id: CROSSHAIR_PULSE,
        type: 'circle',
        source: CROSSHAIR_SOURCE,
        paint: {
          'circle-radius': 14,
          'circle-color': '#60a5fa',   // blue-400
          'circle-opacity': 0.25,
          'circle-stroke-color': '#3b82f6',
          'circle-stroke-width': 1.5,
          'circle-stroke-opacity': 0.55,
        },
      });

      // Inner sharp dot
      map.addLayer({
        id: CROSSHAIR_LAYER,
        type: 'circle',
        source: CROSSHAIR_SOURCE,
        paint: {
          'circle-radius': 5,
          'circle-color': '#ffffff',
          'circle-stroke-color': '#3b82f6',
          'circle-stroke-width': 2.5,
        },
      });
    }
  }, [hoveredElevIndex, initStatus]);

  // ---------------------------------------------------------------------------
  // Phase 9: GPS tracking — updates blue dot and auto-follows camera
  // ---------------------------------------------------------------------------
  //
  // Runtime selection is delegated to locationTracker.ts:
  //   • iOS / Android (Capacitor) → BackgroundGeolocation.addWatcher()
  //     Continues delivering fixes even when the screen is locked or the app
  //     is backgrounded, via the OS native background location API.
  //   • Desktop browser / PWA    → navigator.geolocation.watchPosition()
  //     Standard Web API, adequate for development and desktop use.

  useEffect(() => {
    let cancelled = false;

    startTracking(
      // onPosition — identical MapLibre update logic as the old watchPosition cb
      ({ lng, lat, accuracy }) => {
        if (cancelled) return;

        userLocationRef.current = { lng, lat, accuracy };

        const map = mapRef.current;
        if (!map) return;

        const src = map.getSource('user-location-source') as maplibregl.GeoJSONSource | undefined;
        if (!src) return;

        // Two features share the same source:
        //  • 'dot'      — the precise GPS point (blue circle layer)
        //  • 'accuracy' — the uncertainty halo (driven by circle-radius expression)
        src.setData({
          type: 'FeatureCollection',
          features: [
            {
              type:       'Feature',
              properties: { type: 'dot' },
              geometry:   { type: 'Point', coordinates: [lng, lat] },
            },
            {
              type:       'Feature',
              properties: { type: 'accuracy', accuracyMeters: accuracy, lat },
              geometry:   { type: 'Point', coordinates: [lng, lat] },
            },
          ],
        });

        // Auto-follow camera only when tracking is active.
        if (isTrackingCameraRef.current) {
          map.flyTo({ center: [lng, lat], zoom: 15, essential: true });
        }
      },
      // onError
      (err) => {
        console.warn('[GPS] Tracker error:', err.message);
      }
    ).then((handle) => {
      if (cancelled) {
        // Component unmounted before the async init resolved — stop immediately.
        stopTracking(handle);
        return;
      }
      gpsWatchIdRef.current = handle;
    }).catch((err) => {
      console.error('[GPS] Failed to start tracker:', err);
    });

    return () => {
      cancelled = true;
      stopTracking(gpsWatchIdRef.current);
      gpsWatchIdRef.current = null;
    };
  // isTrackingCamera is read via isTrackingCameraRef to avoid stale closures;
  // re-registering the watcher on every tracking toggle would create duplicate
  // watchers and drain battery.
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Keep the ref in sync with state so the watcher closure stays current.
  useEffect(() => { isTrackingCameraRef.current = isTrackingCamera; }, [isTrackingCamera]);

  // Dragstart → disengage camera tracking so the user can freely pan.
  useEffect(() => {
    if (initStatus !== 'ready') return;
    const map = mapRef.current;
    if (!map) return;
    const onDragStart = () => setIsTrackingCamera(false);
    map.on('dragstart', onDragStart);
    return () => { map.off('dragstart', onDragStart); };
  }, [initStatus]);

  /** Snap camera to current GPS fix and engage auto-follow. */
  const handleCenterOnMe = useCallback(() => {
    const map = mapRef.current;
    if (!map) return;
    const loc = userLocationRef.current;
    if (loc) {
      map.flyTo({ center: [loc.lng, loc.lat], zoom: 15, essential: true });
    }
    setIsTrackingCamera(true);
  }, []);

  // ---------------------------------------------------------------------------
  // Location selector handler
  // ---------------------------------------------------------------------------

  const handleLocationSelect = (locationName: string) => {
    setSelectedHike(locationName);
    setStartPt(null);
    setEndPt(null);
    clearRoute();
    updateMarkersLayer(null, null);

    const loc = HIKE_LOCATIONS.find(l => l.name === locationName);
    if (loc && mapRef.current) {
      mapRef.current.flyTo({
        center: loc.coords,
        zoom:   loc.zoom,
        speed:  1.2,
        curve:  1.4,
        essential: true,
      });
    }
  };

  // ---------------------------------------------------------------------------
  // Derived UI labels
  // ---------------------------------------------------------------------------

  const scanLabel: Record<ScanStatus, string> = {
    idle:    'Scan Viewport for Trails',
    scanning:'Querying Overpass API…',
    indexed: `${trailCount} trails indexed`,
    error:   'Retry Scan',
  };

  const highwayLabel = (raw: string) => {
    const map: Record<string, string> = {
      path:    'Hiking Path',
      footway: 'Footway',
      track:   'Track',
      hiking:  'Hiking Route',
    };
    return map[raw] ?? raw;
  };

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  return (
    <div className="relative w-full h-[600px] rounded-3xl overflow-hidden border border-slate-800 bg-slate-950 shadow-2xl">

      {/* ── Map canvas mount ─────────────────────────────────────────────── */}
      <div ref={mapContainerRef} className="absolute inset-0 w-full h-full" />

      {/* ── Ambient radial glow ──────────────────────────────────────────── */}
      <div className="absolute inset-0 pointer-events-none bg-[radial-gradient(ellipse_at_center,rgba(16,185,129,0.04),transparent_65%)]" />

      {/* ── Boot / error overlay ─────────────────────────────────────────── */}
      {initStatus !== 'ready' && (
        <div className="absolute inset-0 flex flex-col items-center justify-center bg-slate-950/90 backdrop-blur-md z-30 p-8 text-center">
          <div className="relative mb-6">
            <div className="h-16 w-16 rounded-full border-2 border-emerald-500/20 border-t-emerald-400 animate-spin" />
            <div className="absolute inset-0 flex items-center justify-center">
              <svg className="h-6 w-6 text-emerald-400 animate-pulse" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M9 20l-5.447-2.724A1 1 0 013 16.382V5.618a1 1 0 011.447-.894L9 7m0 13l6-3m-6 3V7m6 10l4.553 2.276A1 1 0 0021 18.382V7.618a1 1 0 00-.553-.894L15 4m0 13V4m0 0L9 7" />
              </svg>
            </div>
          </div>
          <h3 className="text-lg font-bold text-slate-100 tracking-wide">FreeHike Storage Bridge</h3>
          <p className="text-xs text-slate-400 font-mono mt-2 px-6 py-2 rounded-lg bg-slate-900/60 border border-slate-800 max-w-md break-all">
            {statusMessage}
          </p>
          {initStatus === 'error' && (
            <button
              onClick={() => window.location.reload()}
              className="mt-6 px-4 py-2 rounded-xl bg-rose-500/20 border border-rose-500/30 text-rose-300 font-mono text-xs hover:bg-rose-500/30 transition-all cursor-pointer"
            >
              Retry Storage Bind
            </button>
          )}
        </div>
      )}

      {/* ── Top HUD bar ──────────────────────────────────────────────────── */}
      {initStatus === 'ready' && (
        <div className="absolute top-4 left-4 right-4 z-20 flex flex-wrap items-center justify-between gap-3 pointer-events-none">

          {/* Location selector */}
          <div className="pointer-events-auto backdrop-blur-md bg-slate-950/70 border border-slate-800 rounded-2xl p-2.5 flex items-center space-x-3 shadow-lg max-w-sm">
            <span className="text-[10px] uppercase font-mono tracking-widest text-slate-500">Region</span>
            <div className="h-4 w-px bg-slate-800" />
            <select
              value={selectedHike}
              onChange={(e) => handleLocationSelect(e.target.value)}
              className="bg-transparent text-slate-200 font-semibold text-xs focus:outline-none cursor-pointer pr-4"
            >
              {HIKE_LOCATIONS.map(loc => (
                <option key={loc.name} value={loc.name} className="bg-slate-950 text-slate-200">
                  {loc.name} ({loc.region})
                </option>
              ))}
            </select>
          </div>

          {/* OPFS mode pill */}
          <div className="pointer-events-auto backdrop-blur-md bg-slate-950/70 border border-slate-800 rounded-2xl px-4 py-3 flex items-center space-x-2.5 shadow-lg text-xs font-mono">
            <span className="h-2 w-2 rounded-full bg-emerald-500 animate-pulse" />
            <span className="text-[10px] text-slate-400 uppercase tracking-widest">Offline-First · OPFS</span>
          </div>
        </div>
      )}

      {/* ── "Scan Viewport for Trails" floating CTA ──────────────────────── */}
      {initStatus === 'ready' && (
        <div className="absolute top-20 left-1/2 -translate-x-1/2 z-20 pointer-events-auto">
          <button
            id="scan-viewport-btn"
            onClick={handleScanViewport}
            disabled={scanStatus === 'scanning'}
            className={[
              // glassmorphism base
              'group relative flex items-center gap-2.5 px-5 py-3 rounded-2xl',
              'backdrop-blur-xl border shadow-xl',
              'font-semibold text-sm tracking-wide transition-all duration-300',
              'active:scale-95 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-400',
              // idle / indexed
              scanStatus !== 'scanning' && scanStatus !== 'error'
                ? 'bg-emerald-950/60 border-emerald-500/40 text-emerald-300 hover:bg-emerald-900/70 hover:border-emerald-400/60 hover:shadow-emerald-500/20 cursor-pointer'
                : '',
              // scanning
              scanStatus === 'scanning'
                ? 'bg-slate-900/70 border-slate-700/40 text-slate-400 cursor-not-allowed'
                : '',
              // error
              scanStatus === 'error'
                ? 'bg-rose-950/60 border-rose-500/40 text-rose-300 hover:bg-rose-900/70 cursor-pointer'
                : '',
            ].join(' ')}
          >
            {/* Subtle pulse ring when scanning */}
            {scanStatus === 'scanning' && (
              <span className="absolute inset-0 rounded-2xl border border-emerald-500/30 animate-ping opacity-40" />
            )}

            {/* Icon */}
            {scanStatus === 'scanning' ? (
              <svg className="h-4 w-4 animate-spin" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
              </svg>
            ) : scanStatus === 'indexed' ? (
              <svg className="h-4 w-4 text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M9 20l-5.447-2.724A1 1 0 013 16.382V5.618a1 1 0 011.447-.894L9 7m0 13l6-3m-6 3V7m6 10l4.553 2.276A1 1 0 0021 18.382V7.618a1 1 0 00-.553-.894L15 4m0 13V4m0 0L9 7" />
              </svg>
            ) : scanStatus === 'error' ? (
              <svg className="h-4 w-4 text-rose-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
              </svg>
            ) : (
              <svg className="h-4 w-4 text-emerald-400 group-hover:scale-110 transition-transform" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
              </svg>
            )}

            <span>{scanLabel[scanStatus]}</span>

            {/* Indexed badge */}
            {scanStatus === 'indexed' && indexKb > 0 && (
              <span className="ml-1 px-1.5 py-0.5 rounded-md bg-emerald-500/20 text-emerald-300 text-[10px] font-mono border border-emerald-500/30">
                {indexKb} KB index
              </span>
            )}
          </button>

          {/* Overpass error sub-label */}
          {scanStatus === 'error' && scanError && (
            <p className="mt-1.5 text-center text-[10px] font-mono text-rose-400/80 max-w-[280px] mx-auto">
              {scanError}
            </p>
          )}
        </div>
      )}

      {/* ── Bottom-left: Thread telemetry HUD ────────────────────────────── */}
      {initStatus === 'ready' && (
        <div className="absolute bottom-4 left-4 z-20 max-w-[268px] pointer-events-auto">
          <div className="backdrop-blur-md bg-slate-950/75 border border-slate-800 rounded-2xl p-4 shadow-xl space-y-4 text-slate-200">

            <div className="flex items-center justify-between border-b border-slate-800 pb-2">
              <span className="text-[10px] uppercase font-mono tracking-widest text-emerald-400">Thread Telemetry</span>
              <span className="px-1.5 py-0.5 rounded bg-slate-800 text-slate-400 text-[9px] font-mono">RPC</span>
            </div>

            <div className="grid grid-cols-2 gap-2 text-xs font-mono">
              <div className="p-2 rounded-xl bg-slate-900/40 border border-slate-800">
                <span className="text-[9px] text-slate-500 uppercase block leading-none">Database</span>
                <span className="text-slate-300 font-bold block mt-1">hike.pmtiles</span>
              </div>
              <div className="p-2 rounded-xl bg-slate-900/40 border border-slate-800">
                <span className="text-[9px] text-slate-500 uppercase block leading-none">Local Size</span>
                <span className="text-slate-300 font-bold block mt-1">
                  {fileSize ? `${(fileSize / 1024).toFixed(0)} KB` : '…'}
                </span>
              </div>
            </div>

            <div className="space-y-2.5 text-xs font-mono">
              <div className="flex justify-between items-center">
                <span className="text-slate-500">Active Thread Calls</span>
                <span className={`font-bold ${telemetry.activeRequests > 0 ? 'text-teal-400' : 'text-slate-300'}`}>
                  {telemetry.activeRequests}
                </span>
              </div>
              <div className="flex justify-between items-center">
                <span className="text-slate-500">Last Range Latency</span>
                <span className="text-slate-300 font-bold">
                  {telemetry.lastFetchTime > 0 ? `${telemetry.lastFetchTime.toFixed(1)} ms` : '0.0 ms'}
                </span>
              </div>
              <div className="flex justify-between items-center">
                <span className="text-slate-500">Last Range Size</span>
                <span className="text-slate-300 font-bold">
                  {telemetry.lastFetchSize > 0 ? `${(telemetry.lastFetchSize / 1024).toFixed(1)} KB` : '0.0 KB'}
                </span>
              </div>
              <div className="flex justify-between items-center border-t border-slate-800 pt-2">
                <span className="text-slate-500">Total Read Vol</span>
                <span className="text-emerald-400 font-bold">
                  {telemetry.totalBytes > 0 ? `${(telemetry.totalBytes / 1024).toFixed(1)} KB` : '0.0 KB'}
                </span>
              </div>
            </div>

            {/* Spatial index stats — appear once indexed */}
            {scanStatus === 'indexed' && (
              <div className="border-t border-slate-800 pt-3 space-y-2 text-xs font-mono">
                <div className="flex items-center justify-between">
                  <span className="text-[10px] uppercase tracking-widest text-teal-400">Spatial Index</span>
                  <span className="text-[9px] text-slate-500">Flatbush R-Tree</span>
                </div>
                <div className="flex justify-between items-center">
                  <span className="text-slate-500">Indexed Ways</span>
                  <span className="text-teal-300 font-bold">{trailCount.toLocaleString()}</span>
                </div>
                <div className="flex justify-between items-center">
                  <span className="text-slate-500">Index Size</span>
                  <span className="text-teal-300 font-bold">{indexKb} KB</span>
                </div>
              </div>
            )}
          </div>
        </div>
      )}

      {/* ── Download zone overlay (visible only in download mode) ─────────── */}
      {initStatus === 'ready' && isDownloadMode && (
        <div className="absolute inset-0 z-25 pointer-events-none flex items-center justify-center">
          {/* Dark vignette outside the selection box */}
          <div className="absolute inset-0 bg-slate-950/50" />

          {/* Selection box — centred 70% × 60% of the container */}
          <div
            className="relative"
            style={{ width: '70%', height: '60%' }}
          >
            {/* Dashed border */}
            <div className="absolute inset-0 border-2 border-dashed border-blue-400/70 rounded-lg" />
            {/* Cut-out: make the inside lighter than the vignette */}
            <div className="absolute inset-0 bg-blue-400/5 rounded-lg" />

            {/* Corner tick marks */}
            {(['tl','tr','bl','br'] as const).map(c => (
              <span
                key={c}
                className={[
                  'absolute h-4 w-4 border-blue-400',
                  c === 'tl' ? 'top-0 left-0 border-t-2 border-l-2 rounded-tl-sm' : '',
                  c === 'tr' ? 'top-0 right-0 border-t-2 border-r-2 rounded-tr-sm' : '',
                  c === 'bl' ? 'bottom-0 left-0 border-b-2 border-l-2 rounded-bl-sm' : '',
                  c === 'br' ? 'bottom-0 right-0 border-b-2 border-r-2 rounded-br-sm' : '',
                ].join(' ')}
              />
            ))}

            {/* Centre label */}
            <div className="absolute inset-0 flex items-center justify-center">
              <span className="px-2.5 py-1 rounded-lg bg-slate-950/80 border border-blue-500/30 text-blue-300 text-[11px] font-mono tracking-widest uppercase">
                Download Zone
              </span>
            </div>
          </div>
        </div>
      )}

      {/* ── Download control: floating button / confirm panel ───────────── */}
      {initStatus === 'ready' && (
        <div className="absolute top-20 right-4 z-30 pointer-events-auto flex flex-col items-end gap-2">

          {!isDownloadMode ? (
            /* ─ Idle: single "Download Map Area" pill button ─ */
            <button
              id="download-map-area-btn"
              onClick={() => setIsDownloadMode(true)}
              className={[
                'flex items-center gap-2 px-4 py-2.5 rounded-2xl',
                'backdrop-blur-xl bg-slate-950/70 border border-slate-700/50',
                'text-slate-300 text-xs font-semibold tracking-wide',
                'hover:bg-slate-900/80 hover:border-blue-500/50 hover:text-blue-300',
                'shadow-lg transition-all duration-200 active:scale-95',
                'focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-400 cursor-pointer',
              ].join(' ')}
            >
              <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round"
                  d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4" />
              </svg>
              Download Map Area
            </button>
          ) : (
            /* ─ Active: confirm / cancel panel ─ */
            <div className="backdrop-blur-xl bg-slate-950/85 border border-blue-500/30 rounded-2xl p-4 shadow-2xl shadow-blue-500/10 w-64 space-y-3">

              {/* Header */}
              <div className="flex items-center gap-2">
                <div className="h-7 w-7 rounded-lg bg-blue-600/20 border border-blue-500/30 flex items-center justify-center flex-shrink-0">
                  <svg className="h-4 w-4 text-blue-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                    <path strokeLinecap="round" strokeLinejoin="round"
                      d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4" />
                  </svg>
                </div>
                <div>
                  <p className="text-xs font-bold text-slate-100">Download viewport area?</p>
                  <p className="text-[10px] font-mono text-slate-500">Saved to OPFS — works offline</p>
                </div>
              </div>

              {/* Payload info */}
              <div className="grid grid-cols-2 gap-1.5">
                {[
                  { label: 'Map tiles',   value: 'active_map.pmtiles' },
                  { label: 'Routing',     value: 'active_routing.tar' },
                ].map(({ label, value }) => (
                  <div key={label} className="px-2.5 py-1.5 rounded-lg bg-slate-900/60 border border-slate-800">
                    <p className="text-[9px] text-slate-500 uppercase tracking-widest font-mono">{label}</p>
                    <p className="text-[10px] text-slate-300 font-mono truncate mt-0.5">{value}</p>
                  </div>
                ))}
              </div>

              {/* Estimated size note */}
              <p className="text-[10px] text-slate-500 font-mono text-center">
                Est. size: <span className="text-blue-400 font-bold">~8 – 25 MB</span> depending on zoom extent
              </p>

              {/* Progress display (visible when fetch/write is in progress) */}
              {(downloadStatus === 'fetching' || downloadStatus === 'writing') && (
                <div className="flex items-center gap-2 px-3 py-2 rounded-xl bg-blue-950/50 border border-blue-500/25">
                  <svg className="h-3.5 w-3.5 text-blue-400 animate-spin flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
                    <path strokeLinecap="round" strokeLinejoin="round" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
                  </svg>
                  <span className="text-[10px] text-blue-300 font-mono truncate">
                    {downloadProgressLabel || (downloadStatus === 'fetching' ? 'Fetching files…' : 'Writing to OPFS…')}
                  </span>
                </div>
              )}

              {/* Success flash */}
              {downloadStatus === 'done' && (
                <div className="flex items-center gap-2 px-3 py-2 rounded-xl bg-emerald-950/50 border border-emerald-500/25">
                  <svg className="h-3.5 w-3.5 text-emerald-400 flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
                    <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                  </svg>
                  <span className="text-[10px] text-emerald-300 font-mono">Region saved to OPFS</span>
                </div>
              )}

              {/* Error flash */}
              {downloadStatus === 'error' && (
                <div className="flex items-center gap-2 px-3 py-2 rounded-xl bg-rose-950/50 border border-rose-500/25">
                  <svg className="h-3.5 w-3.5 text-rose-400 flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                    <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v2m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                  </svg>
                  <span className="text-[10px] text-rose-300 font-mono">Download failed — check console</span>
                </div>
              )}

              {/* Action buttons */}
              <div className="flex gap-2 pt-1">
                <button
                  id="download-confirm-btn"
                  disabled={downloadStatus === 'fetching' || downloadStatus === 'writing'}
                  onClick={() => {
                    const map = mapRef.current;
                    if (!map) return;
                    onRegionDownload?.(map.getBounds());
                  }}
                  className={[
                    'flex-1 flex items-center justify-center gap-1.5 py-2.5 rounded-xl',
                    'text-xs font-bold tracking-wide transition-all active:scale-95',
                    'focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-400',
                    'disabled:opacity-40 disabled:pointer-events-none cursor-pointer',
                    downloadStatus === 'done'
                      ? 'bg-emerald-600/70 border border-emerald-500/40 text-white'
                      : 'bg-blue-600/80 border border-blue-500/40 text-white hover:bg-blue-500/90',
                  ].join(' ')}
                >
                  {downloadStatus === 'fetching' || downloadStatus === 'writing' ? (
                    <svg className="h-3.5 w-3.5 animate-spin" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                      <path strokeLinecap="round" strokeLinejoin="round" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
                    </svg>
                  ) : downloadStatus === 'done' ? (
                    <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
                      <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                    </svg>
                  ) : (
                    <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                      <path strokeLinecap="round" strokeLinejoin="round" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4" />
                    </svg>
                  )}
                  {downloadStatus === 'done' ? 'Saved!' : 'Confirm'}
                </button>

                <button
                  id="download-cancel-btn"
                  disabled={downloadStatus === 'fetching' || downloadStatus === 'writing'}
                  onClick={() => setIsDownloadMode(false)}
                  className="flex-1 py-2.5 rounded-xl text-xs font-semibold text-slate-400 border border-slate-700/60 bg-slate-900/50 hover:bg-slate-800/60 hover:text-slate-200 transition-all active:scale-95 disabled:opacity-40 disabled:pointer-events-none cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-slate-400"
                >
                  Cancel
                </button>
              </div>
            </div>
          )}
        </div>
      )}

      {/* ── Bottom-right: Center on Me button ──────────────────────────────── */}
      {initStatus === 'ready' && (
        <div className="absolute bottom-[270px] right-4 z-20 pointer-events-auto">
          <button
            id="center-on-me-btn"
            onClick={handleCenterOnMe}
            title={isTrackingCamera ? 'Camera tracking active' : 'Center on my location'}
            className={[
              'group flex items-center justify-center h-11 w-11 rounded-2xl',
              'backdrop-blur-xl border shadow-xl transition-all duration-200',
              'active:scale-90 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-400',
              isTrackingCamera
                ? 'bg-blue-600/80 border-blue-400/60 shadow-blue-500/30 cursor-default'
                : 'bg-slate-950/70 border-slate-700/50 hover:bg-slate-900/80 hover:border-blue-500/50 cursor-pointer',
            ].join(' ')}
          >
            {/* Crosshair / location icon */}
            <svg
              className={`h-5 w-5 transition-colors ${
                isTrackingCamera ? 'text-white' : 'text-slate-400 group-hover:text-blue-400'
              }`}
              fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}
            >
              <circle cx="12" cy="12" r="3" fill="currentColor" stroke="none" />
              <path strokeLinecap="round" strokeLinejoin="round"
                d="M12 2v3m0 14v3M2 12h3m14 0h3" />
              <circle cx="12" cy="12" r="7" strokeOpacity="0.4" />
            </svg>

            {/* Pulsing ring when tracking */}
            {isTrackingCamera && (
              <span className="absolute inset-0 rounded-2xl border border-blue-400/60 animate-ping opacity-50" />
            )}
          </button>
        </div>
      )}

      {/* ── Bottom-right: Live proximity HUD ─────────────────────────────── */}
      {initStatus === 'ready' && (
        <div className="absolute bottom-4 right-4 z-20 w-[220px] pointer-events-none">
          <div className={[
            'backdrop-blur-md border rounded-2xl p-4 shadow-xl transition-all duration-300',
            nearest
              ? 'bg-emerald-950/70 border-emerald-500/30'
              : 'bg-slate-950/70 border-slate-800',
          ].join(' ')}>

            {/* Header */}
            <div className="flex items-center justify-between mb-3">
              <span className="text-[10px] uppercase font-mono tracking-widest text-emerald-400">
                Nearest Trail
              </span>
              <span className={[
                'h-2 w-2 rounded-full',
                nearest     ? 'bg-emerald-400 animate-pulse' : 'bg-slate-700',
              ].join(' ')} />
            </div>

            {nearest ? (
              <div className="space-y-2">
                {/* Trail name */}
                <p className="text-sm font-bold text-slate-100 leading-snug break-words">
                  {nearest.name}
                </p>

                {/* Type pill */}
                <span className="inline-flex items-center px-2 py-0.5 rounded-md bg-teal-500/15 border border-teal-500/25 text-teal-300 text-[10px] font-mono">
                  {highwayLabel(nearest.highway)}
                </span>

                {/* Distance */}
                <div className="flex items-end gap-1 pt-1">
                  <span className={[
                    'text-2xl font-black tabular-nums leading-none',
                    nearest.distanceMeters < 500
                      ? 'text-emerald-400'
                      : nearest.distanceMeters < 2000
                        ? 'text-teal-400'
                        : 'text-slate-300',
                  ].join(' ')}>
                    {nearest.distanceMeters >= 1000
                      ? `${(nearest.distanceMeters / 1000).toFixed(1)}`
                      : nearest.distanceMeters.toString()}
                  </span>
                  <span className="text-xs font-mono text-slate-400 mb-0.5">
                    {nearest.distanceMeters >= 1000 ? 'km' : 'm'}&nbsp;away
                  </span>
                </div>
              </div>
            ) : (
              <div className="space-y-1.5">
                <p className="text-xs text-slate-500 font-mono leading-relaxed">
                  {scanStatus === 'idle' || scanStatus === 'error'
                    ? 'Scan the viewport first to\nbuild the spatial index.'
                    : scanStatus === 'scanning'
                      ? 'Building Flatbush R-Tree…'
                      : 'Move cursor over the map.'}
                </p>
                {(scanStatus === 'idle' || scanStatus === 'error') && (
                  <div className="flex items-center gap-1.5 text-[10px] font-mono text-slate-600">
                    <svg className="h-3 w-3" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                      <path strokeLinecap="round" strokeLinejoin="round" d="M5 10l7-7m0 0l7 7m-7-7v18" />
                    </svg>
                    Use the button above
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      )}

    </div>
  );
}
