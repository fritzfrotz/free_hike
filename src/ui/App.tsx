import React, { useEffect, useRef, useState, useCallback } from 'react';
import type {
  WorkerRequestMessage,
  WorkerResponseMessage,
  SyncProvider,
  SyncConnectionStatus,
  SyncMetadata,
  SyncManifestRecord,
  CachedTrailFeature,
  RouteCalculateSuccessPayload,
  ElevationProfileRequestPayload,
  ElevationProfileSuccessPayload,
  DownloadRegionRequestPayload,
  DownloadRegionSuccessPayload,
} from '../shared/types';
import MapView from './components/MapView';
import CloudSyncPanel from './components/CloudSyncPanel';
import ElevationProfile from './components/ElevationProfile';
import maplibregl from 'maplibre-gl';
import { retrieveAndClearState } from './services/cryptoPKCE';
import {
  buildGoogleAuthUrl,
  exchangeGoogleCode,
  getGoogleUserInfo,
  syncToGoogle,
  disconnectGoogle,
  loadGoogleTokenRecord,
} from './services/sync/GoogleDriveSync';
import {
  buildDropboxAuthUrl,
  exchangeDropboxCode,
  getDropboxUserInfo,
  syncToDropbox,
  disconnectDropbox,
  loadDropboxTokenRecord,
} from './services/sync/DropboxSync';
import { saveSyncMetadata, loadSyncMetadata, clearSyncMetadata } from './services/syncDB';
import { featuresToGpx } from './services/gpxSerializer';
import SavedRoutesPanel from './components/SavedRoutesPanel';
import { saveRoute, deleteRoute } from '../shared/db';
import type { SavedRoute } from '../shared/db';
import { requestPersistentStorage } from './services/storageGuard';


export default function App() {
  // ── Dummy worker state (existing Phase 1 scaffold) ──────────────────────────
  const [messages, setMessages] = useState<Array<{ sender: 'client' | 'worker'; text: string; timestamp: Date }>>([]);
  const [inputText, setInputText]   = useState('Explore the trails!');
  const [workerReady, setWorkerReady] = useState(false);
  const workerRef = useRef<Worker | null>(null);

  // ── Cloud sync state ─────────────────────────────────────────────────────────
  const [syncProvider, setSyncProvider] = useState<SyncProvider>('none');
  const [syncStatus,   setSyncStatus]   = useState<SyncConnectionStatus>('disconnected');
  const [syncMetadata, setSyncMetadata] = useState<SyncMetadata | null>(null);
  const [syncEmail,    setSyncEmail]    = useState<string | null>(null);

  // ── Offline Routing state (Phase 5) ──────────────────────────────────────────
  const [calculatedRoute, setCalculatedRoute] = useState<{
    coordinatesBuffer: ArrayBuffer;
    distanceMeters: number;
  } | null>(null);
  const [routingWorker, setRoutingWorker] = useState<Worker | null>(null);

  // ── Elevation profile state (Phase 7/8) ───────────────────────────────────────
  const [elevationProfileData, setElevationProfileData] = useState<ElevationProfileSuccessPayload | null>(null);
  /** Index of the coordinate point the user is hovering over in ElevationProfile. */
  const [hoveredElevIndex, setHoveredElevIndex] = useState<number | null>(null);
  /** Ref to the spatial worker so we can post to it from the routing handler. */
  const spatialWorkerRef = useRef<Worker | null>(null);

  // ── Phase 10: Offline Download Manager state ───────────────────────────────
  type DownloadStatus = 'idle' | 'fetching' | 'writing' | 'done' | 'error';
  const [downloadStatus,        setDownloadStatus]        = useState<DownloadStatus>('idle');
  const [downloadProgressLabel, setDownloadProgressLabel] = useState('');
  /** Ref to the mapData worker — needed to send DOWNLOAD_REGION_REQUEST. */
  const mapDataWorkerRef = useRef<Worker | null>(null);

  // ── Phase 11: Route State Management state ───────────────────────────────
  const [isSavedRoutesOpen, setIsSavedRoutesOpen] = useState(false);
  const [savedRoutesRefreshKey, setSavedRoutesRefreshKey] = useState(0);
  const [isStorageDurable, setIsStorageDurable] = useState<boolean | null>(null);

  // ── Effect 1: OAuth callback interception + existing token restoration ───────
  //
  // Strategy: Startup URL interception on window.location.search.
  // When Google/Dropbox redirect back, the URL carries ?code=...&state=...
  // We read these params, validate the state nonce against sessionStorage,
  // perform the token exchange, then clean the address bar via history.replaceState.
  //
  // If no callback params are found, we check localStorage for an existing
  // token and restore the connection state from IDB sync metadata.
  useEffect(() => {
    const params        = new URLSearchParams(window.location.search);
    const code          = params.get('code');
    const returnedState = params.get('state');

    if (code && returnedState) {
      // Validate CSRF state nonce — retrieveAndClearState() removes it from
      // sessionStorage so it cannot be replayed.
      const storedState = retrieveAndClearState();
      if (storedState !== returnedState) {
        console.error('[OAuth] State nonce mismatch — discarding callback.');
        return;
      }

      // Clean the authorization code from the address bar immediately.
      history.replaceState({}, '', window.location.pathname);

      const isGoogle  = returnedState.startsWith('g_');
      const isDropbox = returnedState.startsWith('dbx_');

      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSyncStatus('connecting');

      if (isGoogle) {
        setSyncProvider('google');
        (async () => {
          try {
            const record = await exchangeGoogleCode(code);
            const info   = await getGoogleUserInfo(record.accessToken);
            setSyncEmail(info.email);
            setSyncStatus('connected');
            const manifest = await loadSyncMetadata();
            if (manifest) setSyncMetadata(manifest.metadata);
          } catch (err) {
            console.error('[OAuth] Google exchange failed:', err);
            setSyncStatus('error');
          }
        })();
        return;
      }

      if (isDropbox) {
        setSyncProvider('dropbox');
        (async () => {
          try {
            const record = await exchangeDropboxCode(code);
            const info   = await getDropboxUserInfo(record.accessToken);
            setSyncEmail(info.email);
            setSyncStatus('connected');
            const manifest = await loadSyncMetadata();
            if (manifest) setSyncMetadata(manifest.metadata);
          } catch (err) {
            console.error('[OAuth] Dropbox exchange failed:', err);
            setSyncStatus('error');
          }
        })();
        return;
      }

      // Unknown state prefix — discard silently.
      return;
    }

    // ── No callback code: attempt to restore an existing connection ───────────
    const googleRecord  = loadGoogleTokenRecord();
    const dropboxRecord = loadDropboxTokenRecord();

    if (googleRecord) {
      setSyncProvider('google');
      setSyncStatus('connected');
      // Restore email + metadata from IDB; token refresh happens lazily on sync.
      (async () => {
        try {
          const manifest = await loadSyncMetadata();
          if (manifest) {
            setSyncMetadata(manifest.metadata);
            if (manifest.metadata.accountEmail) setSyncEmail(manifest.metadata.accountEmail);
          }
        } catch { /* non-critical */ }
      })();
      return;
    }

    if (dropboxRecord) {
      setSyncProvider('dropbox');
      setSyncStatus('connected');
      (async () => {
        try {
          const manifest = await loadSyncMetadata();
          if (manifest) {
            setSyncMetadata(manifest.metadata);
            if (manifest.metadata.accountEmail) setSyncEmail(manifest.metadata.accountEmail);
          }
        } catch { /* non-critical */ }
      })();
    }

    // Check and request persistent storage (durable storage)
    (async () => {
      const status = await requestPersistentStorage();
      setIsStorageDurable(status.isPersistent);
    })();
  }, []);

  // ── Effect 2: Dummy background worker (Phase 1 scaffold) ────────────────────
  useEffect(() => {
    const worker = new Worker(
      new URL('../workers/dummy.worker.ts', import.meta.url),
      { type: 'module' },
    );
    workerRef.current = worker;

    const handleMessage = (event: MessageEvent<WorkerResponseMessage>) => {
      const response = event.data;
      console.log('%c[Main Thread] Worker response:', 'color:#0d9488;font-weight:bold;', response);

      if (response.type === 'PONG') {
        setMessages(prev => [...prev, {
          sender:    'worker',
          text:      response.payload.message as string,
          timestamp: new Date(response.payload.timestamp as number),
        }]);
      } else if (response.type === 'ERROR') {
        setMessages(prev => [...prev, {
          sender:    'worker',
          text:      `Error: ${response.error ?? 'Unknown error'}`,
          timestamp: new Date(),
        }]);
      }
    };

    worker.addEventListener('message', handleMessage);
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setWorkerReady(true);
    console.log('%c[Main Thread] Web Worker initialized.', 'color:#0284c7;font-weight:bold;');

    return () => {
      worker.removeEventListener('message', handleMessage);
      worker.terminate();
    };
  }, []);

  // ── Offline Routing Worker lifecycle (Phase 5) ─────────────────────────────
  useEffect(() => {
    const worker = new Worker(
      new URL('../workers/routing.worker.ts', import.meta.url),
      { type: 'module' },
    );
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setRoutingWorker(worker);

    const handleMessage = (event: MessageEvent<WorkerResponseMessage>) => {
      const { type, payload } = event.data;
      if (type === 'ROUTE_CALCULATE_SUCCESS') {
        const { coordinatesBuffer, distanceMeters } = payload as RouteCalculateSuccessPayload;
        // Save route for rendering on the map (needs a clone — we're transferring below).
        // coordinatesBuffer is detached after transfer, so clone it first for the map.
        const mapBuffer = coordinatesBuffer.slice(0);
        setCalculatedRoute({ coordinatesBuffer: mapBuffer, distanceMeters });

        // ── Phase 8: Chain → spatial worker for elevation profiling ──────────
        // Build a Float64Array view over the original buffer and transfer it
        // zero-copy to the spatial worker.
        const spatialWorker = spatialWorkerRef.current;
        if (spatialWorker && coordinatesBuffer.byteLength >= 16) {
          const coordsForElevation = new Float64Array(coordinatesBuffer);
          const elevReqId = Math.random().toString(36).substring(2, 9);
          const elevReq: WorkerRequestMessage = {
            id: elevReqId,
            type: 'ELEVATION_PROFILE_REQUEST',
            payload: { coordinates: coordsForElevation } satisfies ElevationProfileRequestPayload,
          };
          spatialWorker.postMessage(elevReq, [coordsForElevation.buffer]);
          console.log(
            '%c[Main Thread] Dispatched ELEVATION_PROFILE_REQUEST',
            'color:#818cf8;font-weight:bold;',
            { points: coordsForElevation.length / 2 },
          );
        }
      }
    };

    worker.addEventListener('message', handleMessage);
    console.log('%c[Main Thread] Routing Worker initialized.', 'color:#3b82f6;font-weight:bold;');

    return () => {
      worker.removeEventListener('message', handleMessage);
      worker.terminate();
    };
  }, []);

  // ── Elevation hover callback (memoised) ───────────────────────────────────
  const handleElevHover = useCallback((index: number | null) => {
    setHoveredElevIndex(index);
  }, []);

  // ── Phase 10: Region download orchestrator ────────────────────────────────
  //
  // Flow:
  //   1. Fetch hike.pmtiles + test_graph.tar from our public sandbox URLs
  //      (in production these would come from a CDN parametrised by region bbox).
  //   2. Read both responses as ArrayBuffers — bypassing the browser cache.
  //   3. Transfer them zero-copy to mapData.worker via DOWNLOAD_REGION_REQUEST.
  //   4. Listen for DOWNLOAD_REGION_SUCCESS or DOWNLOAD_REGION_ERROR.
  //   5. Advance the download state machine accordingly.
  const handleRegionDownload = useCallback(async (_bounds: maplibregl.LngLatBounds) => {
    const worker = mapDataWorkerRef.current;
    if (!worker) {
      console.error('[Download] mapData worker not available.');
      return;
    }
    if (downloadStatus === 'fetching' || downloadStatus === 'writing') return;

    setDownloadStatus('fetching');
    setDownloadProgressLabel('Fetching hike.pmtiles…');

    try {
      // ── Step 1: Fetch both files ───────────────────────────────────────
      // In a production build these URLs would be parametrised from the bbox;
      // here we always fetch our Andorra sandbox files.
      const PMTILES_URL = '/hike.pmtiles';       // served by Vite from /public
      const ROUTING_URL = '/test_graph.tar';     // served by Vite from /public

      const pmRes = await fetch(PMTILES_URL, { cache: 'no-store' });
      if (!pmRes.ok) throw new Error(`PMTiles fetch failed: ${pmRes.statusText}`);
      const pmtilesBuffer = await pmRes.arrayBuffer();

      setDownloadProgressLabel('Fetching test_graph.tar…');
      let routingBuffer = new ArrayBuffer(0);
      try {
        const tarRes = await fetch(ROUTING_URL, { cache: 'no-store' });
        if (tarRes.ok) routingBuffer = await tarRes.arrayBuffer();
      } catch {
        // Routing tar is optional — continue with empty buffer if absent.
        console.warn('[Download] test_graph.tar unavailable; routing skipped.');
      }

      // ── Step 2: Hand buffers to the worker (zero-copy transfer) ──────────
      setDownloadStatus('writing');
      setDownloadProgressLabel('Writing to OPFS…');

      const reqId = Math.random().toString(36).substring(2, 9);
      const req: WorkerRequestMessage = {
        id:      reqId,
        type:    'DOWNLOAD_REGION_REQUEST',
        payload: {
          pmtilesBuffer,
          routingBuffer,
          regionLabel: 'Andorra',
        } satisfies DownloadRegionRequestPayload,
      };

      // ── Step 3: One-shot response listener ────────────────────────────
      const onWorkerMessage = (event: MessageEvent<WorkerResponseMessage>) => {
        const { id, type, payload, error } = event.data;
        if (id !== reqId) return;
        worker.removeEventListener('message', onWorkerMessage);

        if (type === 'DOWNLOAD_REGION_SUCCESS') {
          const result = payload as DownloadRegionSuccessPayload;
          setDownloadStatus('done');
          setDownloadProgressLabel('');
          console.log(
            '%c[Download] DOWNLOAD_REGION_SUCCESS',
            'color:#10b981;font-weight:bold;',
            result,
          );
          // Auto-reset to idle after 3 s so the panel can be re-used.
          setTimeout(() => setDownloadStatus('idle'), 3_000);
        } else {
          throw new Error(error ?? 'DOWNLOAD_REGION_ERROR from worker');
        }
      };

      worker.addEventListener('message', onWorkerMessage);
      // Transfer both ArrayBuffers zero-copy — main thread relinquishes ownership.
      worker.postMessage(req, [pmtilesBuffer, routingBuffer]);

    } catch (err) {
      console.error('[Download] Region download failed:', err);
      setDownloadStatus('error');
      setDownloadProgressLabel('');
      // Auto-reset to idle after 4 s.
      setTimeout(() => setDownloadStatus('idle'), 4_000);
    }
  }, [downloadStatus]);

  // ── Phase 11: Route State Management actions ──────────────────────────────
  const handleSaveHike = useCallback(async (title: string) => {
    if (!calculatedRoute || !elevationProfileData) {
      throw new Error('No active route or elevation profile to save.');
    }

    // Reconstruct flat coordinate array from coordinatesBuffer
    const coordsArray = new Float64Array(calculatedRoute.coordinatesBuffer.slice(0));

    const routeData: SavedRoute = {
      title,
      timestamp: Date.now(),
      coordinates: coordsArray,
      totalAscent: elevationProfileData.totalAscent,
      totalDescent: elevationProfileData.totalDescent,
      elevations: elevationProfileData.elevations,
    };

    await saveRoute(routeData);
    setSavedRoutesRefreshKey(prev => prev + 1);
  }, [calculatedRoute, elevationProfileData]);

  const handleLoadRoute = useCallback((route: SavedRoute) => {
    // Zero-copy transfer slice so we can construct a fresh ArrayBuffer
    const coordsBuffer = route.coordinates.buffer.slice(0) as ArrayBuffer;

    setCalculatedRoute({
      coordinatesBuffer: coordsBuffer,
      distanceMeters: 0,
    });

    setElevationProfileData({
      totalAscent: route.totalAscent,
      totalDescent: route.totalDescent,
      elevations: route.elevations,
    });
  }, []);

  const handleDeleteRoute = useCallback(async (id: number) => {
    await deleteRoute(id);
    setSavedRoutesRefreshKey(prev => prev + 1);
  }, []);

  // ── Spatial worker elevation response handler (Phase 8) ───────────────────
  // spatialWorkerRef is populated via onSpatialWorkerReady from MapView.
  // We use a stable ref-listener pattern: attach once when the ref is set.
  const spatialListenerAttached = useRef(false);
  const handleSpatialElevation = useCallback(
    (event: MessageEvent<WorkerResponseMessage>) => {
      const { type, payload } = event.data;
      if (type === 'ELEVATION_PROFILE_SUCCESS') {
        const profile = payload as ElevationProfileSuccessPayload;
        setElevationProfileData(profile);
        console.log(
          '%c[Main Thread] ELEVATION_PROFILE_SUCCESS',
          'color:#34d399;font-weight:bold;',
          { ascent: profile.totalAscent.toFixed(0), descent: profile.totalDescent.toFixed(0) },
        );
      }
    },
    [],
  );


  // ── Ping handler (Phase 1 scaffold) ─────────────────────────────────────────
  const sendPing = (e: React.FormEvent) => {
    e.preventDefault();
    if (!workerRef.current || !inputText.trim()) return;

    const id: string = Math.random().toString(36).substring(2, 9);
    const request: WorkerRequestMessage = { id, type: 'PING', payload: { message: inputText } };

    console.log('%c[Main Thread] Sending ping:', 'color:#4f46e5;font-weight:bold;', request);
    setMessages(prev => [...prev, { sender: 'client', text: inputText, timestamp: new Date() }]);
    workerRef.current.postMessage(request);
    setInputText('');
  };

  // ── Cloud sync handlers ──────────────────────────────────────────────────────

  const handleConnectGoogle = async () => {
    try {
      const url = await buildGoogleAuthUrl();
      window.location.href = url;
    } catch (err) {
      console.error('[Auth] Failed to build Google auth URL:', err);
      setSyncStatus('error');
    }
  };

  const handleConnectDropbox = async () => {
    try {
      const url = await buildDropboxAuthUrl();
      window.location.href = url;
    } catch (err) {
      console.error('[Auth] Failed to build Dropbox auth URL:', err);
      setSyncStatus('error');
    }
  };

  const handleDisconnect = () => {
    if (syncProvider === 'google')  disconnectGoogle();
    if (syncProvider === 'dropbox') disconnectDropbox();
    clearSyncMetadata().catch(console.error);
    setSyncProvider('none');
    setSyncStatus('disconnected');
    setSyncMetadata(null);
    setSyncEmail(null);
  };

  const handleSyncNow = async () => {
    if (syncStatus !== 'connected') return;
    setSyncStatus('syncing');

    try {
      // 1. Read the spatial index feature cache from OPFS (FlatGeobuf format).
      let features: CachedTrailFeature[] = [];
      try {
        const root       = await navigator.storage.getDirectory();
        const fileHandle = await root.getFileHandle('trails_features.fgb');
        const file       = await fileHandle.getFile();
        const buffer     = await file.arrayBuffer();
        const uint8Array = new Uint8Array(buffer);
        const { geojson: fgbGeojson } = await import('flatgeobuf');
        for await (const feature of fgbGeojson.deserialize(uint8Array)) {
          const properties = (feature.properties || {}) as Record<string, any>;
          const geometry = feature.geometry;
          if (geometry && geometry.type === 'LineString') {
            const coordsFlat: number[] = [];
            for (const pt of geometry.coordinates) {
              coordsFlat.push(pt[0], pt[1]);
            }
            // Compute bounding box
            let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
            for (const pt of geometry.coordinates) {
              if (pt[0] < minX) minX = pt[0];
              if (pt[1] < minY) minY = pt[1];
              if (pt[0] > maxX) maxX = pt[0];
              if (pt[1] > maxY) maxY = pt[1];
            }
            features.push({
              id: Number(properties.id || 0),
              name: String(properties.name || 'Unnamed Trail'),
              highway: String(properties.highway || 'path'),
              coords: coordsFlat,
              minX,
              minY,
              maxX,
              maxY,
            });
          }
        }
      } catch (err) {
        console.warn('Failed to read or deserialize trails_features.fgb:', err);
        // trails_features.fgb is absent if no Overpass scan has run yet.
        // Proceed with empty GPX — the sync still validates the pipeline.
      }

      // 2. Serialise to GPX 1.1 and build metadata manifest.
      const gpxContent = featuresToGpx(features);
      const metaJson   = JSON.stringify({
        syncedAt:     new Date().toISOString(),
        featureCount: features.length,
        provider:     syncProvider,
        appVersion:   '3.0.0-phase4',
      }, null, 2);

      // 3. Upload to the connected provider.
      let uploadResult: { filesUploaded: number; totalBytes: number };
      if (syncProvider === 'google') {
        uploadResult = await syncToGoogle(gpxContent, metaJson);
      } else if (syncProvider === 'dropbox') {
        uploadResult = await syncToDropbox(gpxContent, metaJson);
      } else {
        throw new Error('No provider connected.');
      }

      // 4. Persist the outcome to IndexedDB.
      const newMetadata: SyncMetadata = {
        provider:     syncProvider,
        accountEmail: syncEmail ?? undefined,
        lastSynced:   new Date().toISOString(),
        lastFileSize: uploadResult.totalBytes,
        filesUploaded: uploadResult.filesUploaded,
      };

      const tokenRecord =
        syncProvider === 'google'  ? loadGoogleTokenRecord()  :
        syncProvider === 'dropbox' ? loadDropboxTokenRecord()  : null;

      if (tokenRecord) {
        const manifest: SyncManifestRecord = {
          id:          'sync_manifest',
          metadata:    newMetadata,
          tokenRecord,
        };
        await saveSyncMetadata(manifest);
      }

      setSyncMetadata(newMetadata);
      setSyncStatus('connected');

      console.log(
        '%c[Sync] Upload complete.',
        'color:#10b981;font-weight:bold;',
        uploadResult,
      );
    } catch (err) {
      console.error('[Sync] Upload failed:', err);
      setSyncStatus('error');
      // Revert to 'connected' after 3 s so the user can retry.
      setTimeout(() => setSyncStatus('connected'), 3_000);
    }
  };

  // ── Render ───────────────────────────────────────────────────────────────────

  return (
    <div className="min-h-screen bg-slate-950 text-slate-100 flex flex-col items-center justify-between p-6 md:p-12 font-sans selection:bg-emerald-500/30 selection:text-emerald-300">

      {isStorageDurable === false && (
        <div className="w-full max-w-6xl mb-6 p-4 rounded-xl bg-amber-500/10 border border-amber-500/30 flex items-center justify-between text-sm text-amber-400">
          <div className="flex items-center gap-2.5">
            <svg className="h-5 w-5 text-amber-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
            </svg>
            <span>
              <strong>Storage warning:</strong> Storage is not persistent. Your offline map data and route caches are at risk of being evicted silently if your device runs low on disk space.
            </span>
          </div>
        </div>
      )}

      {/* ── Header ─────────────────────────────────────────────────────────── */}
      <header className="w-full max-w-6xl flex items-center justify-between border-b border-slate-900 pb-6 mb-8">
        <div className="flex items-center space-x-3">
          <div className="h-10 w-10 rounded-xl bg-gradient-to-tr from-emerald-500 to-teal-400 flex items-center justify-center shadow-lg shadow-emerald-500/20">
            <svg className="h-6 w-6 text-slate-950" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M9 20l-5.447-2.724A1 1 0 013 16.382V5.618a1 1 0 011.447-.894L9 7m0 13l6-3m-6 3V7m6 10l4.553 2.276A1 1 0 0021 18.382V7.618a1 1 0 00-.553-.894L15 4m0 13V4m0 0L9 7" />
            </svg>
          </div>
          <div>
            <h1 className="text-2xl font-bold tracking-tight bg-gradient-to-r from-emerald-400 via-teal-300 to-cyan-400 bg-clip-text text-transparent">
              FreeHike
            </h1>
            <p className="text-xs text-slate-500 font-mono tracking-widest uppercase">Local-First Geospatial Engine</p>
          </div>
        </div>

        <div className="flex items-center gap-4">
          {/* Sync status indicator */}
          {syncStatus !== 'disconnected' && (
            <div className="flex items-center gap-2 px-3 py-1.5 rounded-full bg-slate-900/60 border border-slate-800">
              <span className={[
                'h-2 w-2 rounded-full',
                syncStatus === 'connected' ? 'bg-indigo-400 animate-pulse' :
                syncStatus === 'syncing'   ? 'bg-teal-400 animate-ping'    :
                syncStatus === 'error'     ? 'bg-rose-500'                  :
                                             'bg-amber-400 animate-pulse',
              ].join(' ')} />
              <span className="text-[10px] font-mono text-slate-400 uppercase tracking-wide">
                {syncStatus === 'connected' ? `${syncProvider === 'google' ? 'Drive' : 'Dropbox'} linked` :
                 syncStatus === 'syncing'   ? 'Syncing…' :
                 syncStatus === 'error'     ? 'Sync error' : 'Connecting…'}
              </span>
            </div>
          )}

          {/* My Hikes HUD Trigger */}
          <button
            onClick={() => setIsSavedRoutesOpen(true)}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-full bg-slate-900/60 hover:bg-slate-800/80 border border-slate-800 text-xs text-slate-300 font-semibold cursor-pointer transition-all active:scale-95"
          >
            <svg className="h-3.5 w-3.5 text-blue-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M5 5a2 2 0 012-2h10a2 2 0 012 2v16l-7-3.5L5 21V5z" />
            </svg>
            My Hikes
          </button>

          {/* Worker status indicator */}
          <div className="flex items-center space-x-2">
            <span className={`h-2.5 w-2.5 rounded-full ${workerReady ? 'bg-emerald-500 animate-pulse' : 'bg-rose-500'}`} />
            <span className="text-xs font-mono text-slate-400 uppercase tracking-wide">
              {workerReady ? 'Worker Connected' : 'Worker Offline'}
            </span>
          </div>
        </div>
      </header>

      {/* ── Map + Elevation Panel ──────────────────────────────────────────── */}
      <section className="w-full max-w-6xl mb-8 relative">
        <MapView
          routingWorker={routingWorker}
          calculatedRoute={calculatedRoute}
          clearRoute={() => {
            setCalculatedRoute(null);
            setElevationProfileData(null);
            setHoveredElevIndex(null);
          }}
          hoveredElevIndex={hoveredElevIndex}
          onSpatialWorkerReady={(worker) => {
            spatialWorkerRef.current = worker;
            // Attach the elevation response listener exactly once.
            if (!spatialListenerAttached.current) {
              worker.addEventListener('message', handleSpatialElevation);
              spatialListenerAttached.current = true;
            }
          }}
          onMapDataWorkerReady={(worker) => {
            mapDataWorkerRef.current = worker;
          }}
          onRegionDownload={handleRegionDownload}
          downloadStatus={downloadStatus}
          downloadProgressLabel={downloadProgressLabel}
        />
        {elevationProfileData && (
          <ElevationProfile
            data={elevationProfileData}
            onHoverIndex={handleElevHover}
            onSaveHike={handleSaveHike}
          />
        )}
      </section>

      {/* ── Cloud Sync Panel ────────────────────────────────────────────────── */}
      <CloudSyncPanel
        syncProvider={syncProvider}
        syncStatus={syncStatus}
        syncMetadata={syncMetadata}
        syncEmail={syncEmail}
        onConnectGoogle={handleConnectGoogle}
        onConnectDropbox={handleConnectDropbox}
        onDisconnect={handleDisconnect}
        onSyncNow={handleSyncNow}
      />

      {/* ── Main Body ──────────────────────────────────────────────────────── */}
      <main className="w-full max-w-6xl flex-grow flex flex-col md:flex-row gap-8 items-stretch justify-center my-4">

        {/* Left: Architecture overview */}
        <section className="flex-1 bg-slate-900/40 backdrop-blur-md border border-slate-900 rounded-2xl p-6 flex flex-col justify-between">
          <div className="space-y-6">
            <div>
              <span className="px-2 py-1 rounded-md bg-emerald-500/10 text-emerald-400 text-xs font-mono border border-emerald-500/20">
                Main Thread Isolation
              </span>
              <h2 className="text-xl font-semibold mt-3 text-slate-100">Zero-Overhead Processing</h2>
              <p className="text-sm text-slate-400 mt-2 leading-relaxed">
                Spatial index builders, DEM contourizers, and Valhalla routing queries run in isolated
                Web Workers — keeping the UI fluid at 60 FPS while Phase 4 OAuth flows execute entirely
                on the main DOM thread without any server involvement.
              </p>
            </div>

            <div className="border-t border-slate-900 pt-6 space-y-4">
              {[
                { n: 1, title: 'Main Thread UI',            body: 'Renders the vector map, drives OAuth flows, and orchestrates the sync pipeline.' },
                { n: 2, title: 'Shared Transferables',      body: 'Zero-copy ArrayBuffer transfers between threads; structured cloning avoided.' },
                { n: 3, title: 'Geospatial Worker Pool',    body: 'Flatbush indexing, Overpass ingestion, DEM decoding — never blocking the UI.' },
              ].map(({ n, title, body }) => (
                <div key={n} className="flex items-start space-x-3">
                  <div className="mt-1 h-5 w-5 rounded bg-slate-800 flex items-center justify-center text-xs font-mono text-slate-300">{n}</div>
                  <div>
                    <h4 className="text-sm font-semibold text-slate-200">{title}</h4>
                    <p className="text-xs text-slate-400 mt-0.5">{body}</p>
                  </div>
                </div>
              ))}
            </div>
          </div>

          <div className="mt-8 p-4 rounded-xl bg-slate-950/50 border border-slate-900 flex items-center justify-between text-xs font-mono text-slate-400">
            <span>TypeScript Targets:</span>
            <span className="text-teal-400 font-semibold">WebWorker · DOM · isolated</span>
          </div>
        </section>

        {/* Right: RPC Activity Stream */}
        <section className="flex-1 bg-slate-900/60 border border-slate-900 rounded-2xl p-6 flex flex-col">
          <h3 className="text-sm font-semibold uppercase tracking-wider text-slate-400 mb-4 flex items-center justify-between">
            <span>Thread Activity Stream</span>
            <span className="font-mono text-xs text-slate-500">RPC Layer</span>
          </h3>

          <div className="flex-grow bg-slate-950/80 border border-slate-900 rounded-xl p-4 overflow-y-auto font-mono text-xs min-h-[300px] max-h-[400px] space-y-4 shadow-inner">
            {messages.length === 0 ? (
              <div className="h-full flex items-center justify-center text-slate-600 italic text-center p-4">
                No active RPC transfers. Send a ping to dispatch a serialized message across the thread boundary.
              </div>
            ) : (
              messages.map((msg, index) => (
                <div
                  key={index}
                  className={`flex flex-col space-y-1 ${msg.sender === 'client' ? 'items-end' : 'items-start'}`}
                >
                  <div className="flex items-center space-x-2 text-[10px] text-slate-500">
                    <span>{msg.sender === 'client' ? 'Main Thread ➔ Worker' : 'Worker ➔ Main Thread'}</span>
                    <span>•</span>
                    <span>{msg.timestamp.toLocaleTimeString()}</span>
                  </div>
                  <div className={`max-w-[85%] rounded-lg p-2.5 leading-normal ${
                    msg.sender === 'client'
                      ? 'bg-indigo-600/20 text-indigo-200 border border-indigo-500/30'
                      : 'bg-emerald-600/20 text-emerald-200 border border-emerald-500/30'
                  }`}>
                    {msg.text}
                  </div>
                </div>
              ))
            )}
          </div>

          <form onSubmit={sendPing} className="mt-4 flex space-x-2">
            <input
              type="text"
              value={inputText}
              onChange={(e) => setInputText(e.target.value)}
              placeholder="Enter message for background thread..."
              className="flex-grow bg-slate-950 border border-slate-800 rounded-xl px-4 py-2.5 text-sm focus:outline-none focus:border-teal-500 text-slate-200 transition-colors"
            />
            <button
              type="submit"
              disabled={!workerReady}
              className="px-4 py-2.5 rounded-xl bg-gradient-to-r from-emerald-500 to-teal-500 text-slate-950 font-semibold text-sm hover:from-emerald-400 hover:to-teal-400 transition-all active:scale-95 disabled:opacity-50 disabled:pointer-events-none shadow-md shadow-emerald-500/10 cursor-pointer"
            >
              Send
            </button>
          </form>
        </section>

      </main>

      {/* ── Footer ─────────────────────────────────────────────────────────── */}
      <footer className="w-full max-w-6xl text-center border-t border-slate-900 pt-6 mt-8 text-xs text-slate-600">
        <p>© 2026 Antigravity. Built with uncompromised client autonomy.</p>
      </footer>

      {/* ── Saved Routes Drawer Panel ───────────────────────────────────────── */}
      <SavedRoutesPanel
        isOpen={isSavedRoutesOpen}
        onClose={() => setIsSavedRoutesOpen(false)}
        onLoadRoute={handleLoadRoute}
        onDeleteRoute={handleDeleteRoute}
        refreshKey={savedRoutesRefreshKey}
      />

    </div>
  );
}
