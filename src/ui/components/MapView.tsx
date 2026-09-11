// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState, useCallback } from 'react';
import maplibregl from 'maplibre-gl';
import type { WorkerRequestMessage, WorkerResponseMessage, MapInitSuccessPayload } from '../../shared/types';
import 'maplibre-gl/dist/maplibre-gl.css';
import mlcontour from 'maplibre-contour';
import type { Feature as GeoJSONFeature } from 'geojson';
import { startTracking, stopTracking, type TrackerHandle } from '../services/locationTracker';
import { useMapStore } from '../../store/mapStore';
import { decideRegionBoot, regionFilesToVerify } from '../../services/regionBootPolicy';
import { registerPMTilesSource, type TelemetryData } from '../../services/pmtilesRegistry';
import RegionSelectorOverlay from './RegionSelectorOverlay';

export type { TelemetryData };

// ---------------------------------------------------------------------------
// PMTiles protocol + source registry
//
// P9.C4: registration moved OUT of this component module entirely — see
// services/pmtilesRegistry.ts. The old module-scoped `getGlobalProtocol()`
// singleton did not survive HMR module duplication (MapView.tsx and its
// ?t=-stamped twin could both be live, each with its own registry, and
// maplibre kept whichever handler registered last) — the root cause of the
// flaky "map never fires 'load'" boots. The registry now lives on
// globalThis, registers with maplibre once at import time, and fails LOUD
// on a key miss instead of silently falling back to HTTP.
// ---------------------------------------------------------------------------
// mapData worker teardown gate
//
// React 18 StrictMode double-invokes the boot effect in dev (mount → cleanup
// → remount). Each mount spawns its own mapData worker and asks it to
// createSyncAccessHandle() the same OPFS files. Worker.terminate() kills the
// old worker's thread immediately but does NOT guarantee its OPFS handle
// locks are released synchronously with that call — the browser's release of
// an implicitly-abandoned handle can lag terminate() by a tick or more. If
// the new mount's worker tries to open the same files before that lag
// clears, createSyncAccessHandle() throws "Access Handles cannot be created
// if there is another open Access Handle...".
//
// Fix: an explicit handle.close() (which the worker's MAP_CLOSE handler
// performs) *is* synchronous and guaranteed. So cleanup asks the outgoing
// worker to MAP_CLOSE, awaits its acknowledgment, *then* terminates it — and
// chains that whole sequence into this module-level promise. The next
// mount's MAP_INIT send awaits this same promise before firing, so it always
// runs strictly after the previous worker's handles are actually closed.
// ---------------------------------------------------------------------------

let pendingWorkerTeardown: Promise<void> = Promise.resolve();

/**
 * Asks a mapData worker to close its open OPFS SyncAccessHandles and waits
 * for its acknowledgment (or a timeout, in case the worker is already wedged
 * / unresponsive) before resolving. Callers should terminate() the worker
 * only after this resolves.
 */
function closeWorkerHandles(worker: Worker, timeoutMs = 1500): Promise<void> {
  return new Promise((resolve) => {
    const closeId = `close-${Math.random().toString(36).substring(2, 9)}`;
    let settled = false;

    const finish = () => {
      if (settled) return;
      settled = true;
      worker.removeEventListener('message', onMessage);
      clearTimeout(timer);
      resolve();
    };

    const onMessage = (event: MessageEvent<WorkerResponseMessage>) => {
      if (event.data.id !== closeId) return;
      finish();
    };

    worker.addEventListener('message', onMessage);
    const timer = setTimeout(finish, timeoutMs);

    try {
      worker.postMessage({ id: closeId, type: 'MAP_CLOSE', payload: null } satisfies WorkerRequestMessage);
    } catch {
      // Worker already terminated/unresponsive — nothing to wait for.
      finish();
    }
  });
}

// ---------------------------------------------------------------------------
// Blank DEM tile placeholder (for contour-generation neighbors that fall
// outside our clipped terrain coverage — see demSource.manager.getTile below)
// ---------------------------------------------------------------------------

let blankDemTileBlob: Blob | null = null;
/**
 * A flat 0m-elevation tile encoded in Mapbox terrain-RGB (base=-10000,
 * interval=0.1 → R=1,G=134,B=160). maplibre-contour fetches a 3x3 neighbor
 * grid around every tile it generates contours for; at the edge of our small
 * clipped region several of those neighbors don't exist in the archive.
 * Returning a flat placeholder (instead of throwing) lets contour generation
 * for the in-bounds tiles proceed instead of the whole neighbor fetch
 * rejecting on one missing edge tile.
 */
async function getBlankDemTile(): Promise<Blob> {
  if (blankDemTileBlob) return blankDemTileBlob;
  const canvas = new OffscreenCanvas(2, 2);
  const ctx = canvas.getContext('2d')!;
  ctx.fillStyle = 'rgb(1,134,160)';
  ctx.fillRect(0, 0, 2, 2);
  blankDemTileBlob = await canvas.convertToBlob({ type: 'image/png' });
  return blankDemTileBlob;
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
// OPFS existence probe (P9.C2 cold-boot replay guard)
// ---------------------------------------------------------------------------

/**
 * True if `filename` exists at the OPFS root with at least one byte. Used to
 * validate a persisted region before binding the map to it — OPFS can be
 * evicted independently of localStorage (non-durable storage, user "clear
 * site data"), and binding to a missing file would silently render an empty
 * map: the worker's handle open uses `{ create: true }` and would fabricate
 * a zero-byte archive. Deliberately opens NO SyncAccessHandle (worker-owned
 * locks) — getFile() is a lock-free snapshot read.
 */
async function opfsFileHasBytes(filename: string): Promise<boolean> {
  try {
    const root = await navigator.storage.getDirectory();
    const fileHandle = await root.getFileHandle(filename); // throws if absent
    const file = await fileHandle.getFile();
    return file.size > 0;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

/**
 * Imperative handle for swapping the active offline region without unmounting
 * the map.  Handed to the parent via onRegionSwitcherReady.
 *
 * One producer feeds this today: compilerStore's handleJobFinished (native
 * Rust compile → opfsMover → OPFS) calls useMapStore's setActiveRegion, and
 * the effect below drives loadOfflineRegion() from there. The producer never
 * needs to know this switcher exists.
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
  /** Called once with the mapData Worker instance (drives App's worker-status pill). */
  onMapDataWorkerReady?: (worker: Worker) => void;
  /** Called when the GPS watcher reports the user denied location permission. */
  onLocationPermissionDenied?: () => void;
  /** Called with a human-readable message when offline map data fails to provision. */
  onMapDataError?: (message: string) => void;
  /** Called with every live GPS fix — drives the Active Trip HUD's distance tracking. */
  onPositionUpdate?: (pos: { lng: number; lat: number; accuracy: number }) => void;
  /**
   * Called once after the map finishes loading with an imperative switcher
   * object.  The parent can store it in a ref and call
   * switcher.loadOfflineRegion() whenever the user selects a new region.
   * Note: MapView also self-wires this internally via useMapStore's
   * activeRegion — this callback remains for external consumers only.
   */
  onRegionSwitcherReady?: (switcher: OfflineRegionSwitcher) => void;
}

export default function MapView({
  onMapDataWorkerReady,
  onLocationPermissionDenied,
  onMapDataError,
  onPositionUpdate,
  onRegionSwitcherReady,
}: MapViewProps) {
  const mapContainerRef  = useRef<HTMLDivElement>(null);
  const mapRef           = useRef<maplibregl.Map | null>(null);
  const regionSwitcherRef = useRef<OfflineRegionSwitcher | null>(null);
  /** Filenames the live MapLibre sources are currently bound to — lets
   *  loadOfflineRegion() skip re-registering a source that hasn't changed
   *  (re-registering an already-active PMTiles source duplicates its worker
   *  reader and corrupts in-flight tile transfers). */
  const activeFilesRef = useRef<{ basemap: string; terrain: string }>({ basemap: '', terrain: '' });
  const mapDataWorkerRef = useRef<Worker | null>(null);   // OPFS / PMTiles worker

  // Map bootstrap state
  const [initStatus,     setInitStatus]     = useState<'idle' | 'initializing' | 'ready' | 'error'>('idle');
  /** Render-safe mirror of mapRef — set once when the map instance is
   *  created, so JSX (e.g. RegionSelectorOverlay's prop) never has to read
   *  a ref during render. */
  const [liveMap, setLiveMap] = useState<maplibregl.Map | null>(null);
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

  // ---------------------------------------------------------------------------
  // Phase 9: Live GPS location tracking
  // ---------------------------------------------------------------------------

  // Whether the map camera should auto-follow the GPS position. Lives in
  // useMapStore rather than local state — the long-lived GPS watcher closure
  // below reads it via useMapStore.getState() (no ref-mirror needed; that's
  // exactly the point of a store that lives outside the component tree).
  const isTrackingCamera = useMapStore((s) => s.isTrackingCamera);
  const setTrackingCamera = useMapStore((s) => s.setTrackingCamera);
  /** Stores the active tracker handle so we can stop it on unmount.
   *  TrackerHandle is a tagged union — kind:'native' holds a string watcher
   *  ID from the Capacitor plugin; kind:'web' holds a number from the
   *  Web Geolocation API.  stopTracking() selects the right path. */
  const gpsWatchIdRef = useRef<TrackerHandle | null>(null);
  /** Latest known GPS fix — [lng, lat] + accuracy metres. */
  const userLocationRef   = useRef<{ lng: number; lat: number; accuracy: number } | null>(null);

  /** The offline region bound to the live MapLibre sources — hot-swapped via
   *  loadOfflineRegion() below whenever this changes, without a style reload. */
  const activeRegion = useMapStore((s) => s.activeRegion);

  // ── P9.C3: custom-region selection mode ────────────────────────────────
  // Entered by RegionPicker (App), exited by RegionSelectorOverlay or Esc.
  // While active, ALL of MapView's control chrome unmounts: the overlay's
  // vignette is pointer-events-none (panning beneath the reticle is the
  // interaction), so any chrome left mounted would still catch clicks
  // through the dim — invisible-but-clickable buttons.
  const isSelectingRegion = useMapStore((s) => s.isSelectingRegion);
  const showChrome = initStatus === 'ready' && !isSelectingRegion;

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
    activeFilesRef.current = { basemap: DEFAULT_BASEMAP, terrain: DEFAULT_TERRAIN };

    let map: maplibregl.Map | null = null;

    // Helper: bind an OPFS filename to THIS mount's worker in the global
    // registry. Last write wins by key, so re-registering (StrictMode
    // remount, hot-swap) atomically replaces any stale instance bound to a
    // torn-down worker.
    const registerSource = (filename: string, onTelemetry?: (d: TelemetryData) => void) =>
      registerPMTilesSource(filename, mapDataWorker, onTelemetry);

    const initId = Math.random().toString(36).substring(2, 9);

    const handleInitMessage = (event: MessageEvent<WorkerResponseMessage>) => {
      const response = event.data;
      if (response.id !== initId) return;
      mapDataWorker.removeEventListener('message', handleInitMessage);

      if (response.type === 'MAP_INIT_SUCCESS') {
        if (!active) return;
        const { size: sizeBytes, provisionFailures } = response.payload as MapInitSuccessPayload;
        setFileSize(sizeBytes);
        setStatusMessage(`OPFS storage bound. Database: ${(sizeBytes / 1024 / 1024).toFixed(2)} MB`);

        if (provisionFailures.length > 0) {
          onMapDataError?.(
            `Couldn't load offline map data for: ${provisionFailures.join(', ')}. ` +
            'The map may be missing terrain, hillshading, or trail layers.',
          );
        }

        // Register the two default sources (basemap + terrain) upfront.
        registerSource(DEFAULT_BASEMAP, (tData) => {
          setTelemetry(prev => ({
            ...tData,
            totalBytes: tData.totalBytes > 0 ? tData.totalBytes : prev.totalBytes,
          }));
        });
        const terrainPMTiles = registerSource(DEFAULT_TERRAIN);

        if (mapContainerRef.current) {
          setStatusMessage('Mounting map container canvas…');

          // BUG(B008): style asset gaps, sprite URL rejected as relative by MapLibre and a contour-label fontstack is not in the vendored glyph set (local glyph fallback renders digits) — severity: minor — repro: boot dev app, console shows sprite error + glyph range warnings
          map = new maplibregl.Map({
            container: mapContainerRef.current,
            // ── Phase Block-1: High-contrast 3D alpine style ─────────────
            // External style JSON bundles sources (basemap-local + terrain-local),
            // the terrain block (exaggeration 1.3), hillshading, contours,
            // dynamic trail rendering, and peak labels.
            style: '/styles/high_contrast_outdoor_style.json',
            center: HIKE_LOCATIONS[0].coords,
            zoom: HIKE_LOCATIONS[0].zoom,
            pitch: 45,
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
          setLiveMap(activeMap);
          activeMap.on('load', () => {
            // ── Memory management: WebGL texture-cache OOM guards ────────
            // Clamp each source cache to 25 tiles max to prevent GPU VRAM
            // exhaustion on memory-constrained mobile devices.
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            (activeMap as any).style?.sourceCaches?.['basemap-local']?.setMaxTiles(25);
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            (activeMap as any).style?.sourceCaches?.['terrain-local']?.setMaxTiles(25);

            // Enable 3D terrain mesh mapping using the offline raster DEM source
            activeMap.setTerrain({
              source: 'terrain-local',
              exaggeration: 1.3,
            });

            // ── Build the imperative region-switcher ──────────────────────
            // loadOfflineRegion() hot-swaps both tile sources without a full
            // style reload. Stored in a ref so the activeRegion effect below
            // can invoke it directly; also handed to onRegionSwitcherReady
            // for any external caller that still wants it.
            {
              const switcher: OfflineRegionSwitcher = {
                async loadOfflineRegion(basemapFile: string, terrainFile: string) {
                  const worker = mapDataWorkerRef.current;
                  if (!worker) throw new Error('[loadOfflineRegion] mapData worker not ready.');

                  // Only touch sources whose filename actually changed —
                  // re-registering an already-active PMTiles source spins up
                  // a second WorkerPMTilesSource/PMTiles pair for the same
                  // file, which corrupts in-flight tile transfers on the
                  // pre-existing source (surfaces as MapLibre "ArrayBuffer
                  // already detached" postMessage errors).
                  const basemapChanged = basemapFile !== activeFilesRef.current.basemap;
                  const terrainChanged = terrainFile !== activeFilesRef.current.terrain;
                  if (!basemapChanged && !terrainChanged) return;

                  const changedFilenames = [
                    ...(basemapChanged ? [basemapFile] : []),
                    ...(terrainChanged ? [terrainFile] : []),
                  ];

                  // 1. Ask the worker to open SyncAccessHandles for the new files.
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
                      payload: { filenames: changedFilenames },
                    } satisfies WorkerRequestMessage);
                  });

                  // 2. Register new WorkerPMTilesSource + PMTiles instances
                  //    only for the files that changed.
                  if (basemapChanged) registerSource(basemapFile);
                  if (terrainChanged) registerSource(terrainFile);

                  // 3. Swap the live MapLibre sources to the new pmtiles:// URLs.
                  //    setUrl() updates the source in-place — no layer teardown.
                  const bSrc = activeMap.getSource('basemap-local') as (maplibregl.RasterTileSource & { setUrl?: (url: string) => void }) | undefined;
                  const tSrc = activeMap.getSource('terrain-local') as (maplibregl.RasterTileSource & { setUrl?: (url: string) => void }) | undefined;

                  if (basemapChanged && bSrc?.setUrl) bSrc.setUrl(`pmtiles://local/${basemapFile}`);
                  if (terrainChanged && tSrc?.setUrl) tSrc.setUrl(`pmtiles://local/${terrainFile}`);

                  activeFilesRef.current = { basemap: basemapFile, terrain: terrainFile };

                  console.log(
                    `[MapView] Offline region swapped → basemap: ${basemapFile}, terrain: ${terrainFile}`,
                  );
                },
              };
              regionSwitcherRef.current = switcher;
              onRegionSwitcherReady?.(switcher);

              // ── P9.C2: cold-boot replay of the persisted region ────────
              // The activeRegion effect below fires on mount, BEFORE this
              // 'load' handler builds the switcher, and bails — a region
              // rehydrated from localStorage would otherwise never re-bind.
              // Replay it here exactly once, after validating its OPFS files
              // still hold bytes (localStorage and OPFS can diverge: cleared
              // site data, storage eviction). A missing file drops the
              // persisted binding entirely so the map stays on the defaults
              // it just booted with instead of binding to an empty archive.
              {
                const { activeRegion: persisted, clearActiveRegion } = useMapStore.getState();
                if (persisted) {
                  void (async () => {
                    // Decision logic lives in services/regionBootPolicy
                    // (extracted P-FE.C1, behavior-identical); the probe and
                    // the MapLibre call stay here.
                    const toVerify = regionFilesToVerify(persisted, {
                      basemapFile: DEFAULT_BASEMAP,
                      terrainFile: DEFAULT_TERRAIN,
                    });
                    const exists = await Promise.all(toVerify.map(opfsFileHasBytes));
                    const decision = decideRegionBoot(toVerify, exists);
                    if (decision.action === 'clear') {
                      console.warn(
                        `[MapView] Persisted region "${persisted.regionLabel}" dropped — missing OPFS file(s): ${decision.missing.join(', ')}`,
                      );
                      clearActiveRegion();
                      return;
                    }
                    if (decision.action === 'noop') return; // persisted region IS the default binding
                    try {
                      await switcher.loadOfflineRegion(persisted.basemapFile, persisted.terrainFile);
                    } catch (err) {
                      console.error('[MapView] Cold-boot region replay failed:', err);
                    }
                  })();
                }
              }
            }

            // Instantiate demSource for dynamic contours, sourcing raw DEM
            // tiles directly from the already-provisioned offline
            // 'terrain-local' PMTiles instance instead of a remote network
            // endpoint — contours must generate with zero connectivity, in
            // line with the rest of the offline-first pipeline.
            const LOCAL_TERRAIN_DEM_URL = 'local-terrain-dem://{z}/{x}/{y}';
            const demSource = new mlcontour.DemSource({
              url: LOCAL_TERRAIN_DEM_URL,
              encoding: 'mapbox',
              maxzoom: 12,
              worker: false,
            });
            demSource.manager = new mlcontour.LocalDemManager({
              demUrlPattern: LOCAL_TERRAIN_DEM_URL,
              cacheSize: 100,
              encoding: 'mapbox',
              maxzoom: 12,
              timeoutMs: 10_000,
              getTile: async (url: string) => {
                const [, z, x, y] = /\/\/(\d+)\/(\d+)\/(\d+)/.exec(url) || [];
                const tile = await terrainPMTiles.getZxy(Number(z), Number(x), Number(y));
                if (!tile) {
                  // Outside our clipped terrain coverage — return a flat
                  // placeholder rather than throwing (see getBlankDemTile).
                  return { data: await getBlankDemTile() };
                }
                return { data: new Blob([tile.data]) };
              },
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
            const emptyFC = { type: 'FeatureCollection' as const, features: [] as GeoJSONFeature[] };
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
          // Surface the underlying Error message/stack — logging the raw event
          // object prints "[object Object]" and hides the actual failure.
          activeMap.on('error', (e) => console.error('[MapLibre]', e.error?.message ?? e.error ?? e));
        }
      } else {
        setInitStatus('error');
        setStatusMessage(`Initialization failed: ${response.error ?? 'Unknown error'}`);
      }
    };

    mapDataWorker.addEventListener('message', handleInitMessage);
    setStatusMessage('Syncing binary file buffers to OPFS…');
    // Wait for the previous mount's worker (if any) to fully close its OPFS
    // handles before asking this worker to open the same files — see the
    // pendingWorkerTeardown comment above. On a normal first mount this
    // promise is already resolved, so the send fires on the next microtask
    // with no perceptible delay.
    // Wait for the previous mount's worker (if any) to fully close its OPFS
    // handles before asking this worker to open the same files — see the
    // pendingWorkerTeardown comment above. On a normal first mount this
    // promise is already resolved, so the send fires on the next microtask
    // with no perceptible delay.
    pendingWorkerTeardown.then(() => {
      if (!active) return; // unmounted again before this mount's init could fire
      // Send the two default filenames so the worker opens SyncAccessHandles
      // for both OPFS files during init.  Subsequent MAP_READ_BYTES requests
      // will find the handles ready without an extra round-trip.
      mapDataWorker.postMessage({
        id:      initId,
        type:    'MAP_INIT',
        payload: { filenames: [DEFAULT_BASEMAP, DEFAULT_TERRAIN] },
      } satisfies WorkerRequestMessage);
    });

    return () => {
      active = false;
      mapDataWorker.removeEventListener('message', handleInitMessage);

      // Deliberately NOT clearing the protocol registry here (P9.C4): stale
      // entries are inert (their worker is terminated, and the map that
      // could request through them is removed below), and the next mount
      // re-registers every key it uses before creating its map. Clearing
      // raced the async boot for zero benefit — and with the registry's
      // fail-loud guard, an unexpected empty registry would now error every
      // tile request instead of degrading silently.

      // Explicitly remove map instances to free map canvas/WebGL context
      map?.remove();
      mapRef.current?.remove();
      mapRef.current = null;

      // Ask the worker to close its OPFS handles and wait for confirmation
      // before terminating it — terminate() alone doesn't guarantee the
      // underlying file locks are released before the next mount's worker
      // tries to open the same files. The next mount's MAP_INIT send (above)
      // chains off this same promise, so it can never race this close.
      pendingWorkerTeardown = closeWorkerHandles(mapDataWorker).then(() => {
        mapDataWorker.terminate();
      });
    };
  // onMapDataWorkerReady, onRegionSwitcherReady, and onMapDataError are
  // intentionally omitted: the mapData worker and MapLibre map are
  // instantiated exactly once on mount; re-running on prop changes would
  // spawn duplicate workers/map instances and re-trigger MAP_INIT.
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ---------------------------------------------------------------------------
  // Hot-swap the active PMTiles source whenever useMapStore's activeRegion
  // changes. This is the only thing that re-touches the WebGL sources.
  // loadOfflineRegion() swaps sources in place; the map instance itself is
  // never torn down or remounted.
  //
  // compilerStore's handleJobFinished copies the Rust engine's archive into
  // OPFS via opfsMover, then calls setActiveRegion with the copied filename.
  // ---------------------------------------------------------------------------

  useEffect(() => {
    if (!activeRegion) return;
    const switcher = regionSwitcherRef.current;
    if (!switcher) return;
    switcher.loadOfflineRegion(activeRegion.basemapFile, activeRegion.terrainFile).catch((err) => {
      console.error('[MapView] Failed to hot-swap offline region:', err);
    });
  }, [activeRegion]);

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
        onPositionUpdate?.({ lng, lat, accuracy });

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

        // Auto-follow camera only when tracking is active. Read straight off
        // the store (outside the React tree) — no ref-mirror needed since
        // useMapStore.getState() always returns the latest value.
        if (useMapStore.getState().isTrackingCamera) {
          map.flyTo({ center: [lng, lat], zoom: 15, essential: true });
        }
      },
      // onError
      (err) => {
        console.warn('[GPS] Tracker error:', err.message);
        if (err.message.toLowerCase().includes('denied')) {
          onLocationPermissionDenied?.();
        }
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
  // isTrackingCamera is read via useMapStore.getState() to avoid stale
  // closures; re-registering the watcher on every tracking toggle would
  // create duplicate watchers and drain battery.
  }, [onLocationPermissionDenied, onPositionUpdate]);

  // Dragstart → disengage camera tracking so the user can freely pan.
  useEffect(() => {
    if (initStatus !== 'ready') return;
    const map = mapRef.current;
    if (!map) return;
    const onDragStart = () => setTrackingCamera(false);
    map.on('dragstart', onDragStart);
    return () => { map.off('dragstart', onDragStart); };
  }, [initStatus, setTrackingCamera]);

  /** Snap camera to current GPS fix and engage auto-follow. */
  const handleCenterOnMe = useCallback(() => {
    const map = mapRef.current;
    if (!map) return;
    const loc = userLocationRef.current;
    if (loc) {
      map.flyTo({ center: [loc.lng, loc.lat], zoom: 15, essential: true });
    }
    setTrackingCamera(true);
  }, [setTrackingCamera]);

  // ---------------------------------------------------------------------------
  // Location selector handler
  // ---------------------------------------------------------------------------

  const handleLocationSelect = (locationName: string) => {
    setSelectedHike(locationName);

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
      {showChrome && (
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

      {/* ── Bottom-left: Thread telemetry HUD ────────────────────────────── */}
      {showChrome && (
        <div className="absolute bottom-4 left-4 z-20 max-w-[268px] pointer-events-auto">
          <div className="backdrop-blur-md bg-slate-950/75 border border-slate-800 rounded-2xl p-4 shadow-xl space-y-4 text-slate-200">

            <div className="flex items-center justify-between border-b border-slate-800 pb-2">
              <span className="text-[10px] uppercase font-mono tracking-widest text-emerald-400">Thread Telemetry</span>
              <span className="px-1.5 py-0.5 rounded bg-slate-800 text-slate-400 text-[9px] font-mono">RPC</span>
            </div>

            <div className="grid grid-cols-2 gap-2 text-xs font-mono">
              <div className="p-2 rounded-xl bg-slate-900/40 border border-slate-800">
                <span className="text-[9px] text-slate-500 uppercase block leading-none">Database</span>
                <span className="text-slate-300 font-bold block mt-1">alps_basemap.pmtiles</span>
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
          </div>
        </div>
      )}

      {/* ── Custom-region selection overlay (P9.C3) ───────────────────────── */}
      {initStatus === 'ready' && isSelectingRegion && liveMap && (
        <RegionSelectorOverlay map={liveMap} />
      )}

      {/* ── Bottom-right: Center on Me button ──────────────────────────────── */}
      {showChrome && (
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

    </div>
  );
}
