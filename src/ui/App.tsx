// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState, useCallback } from 'react';
import MapView from './components/MapView';
import BackgroundHandoffBar from './components/BackgroundHandoffBar';
import RegionPicker from './components/RegionPicker';
import { requestPersistentStorage } from './services/storageGuard';
import { MapCompiler } from '../plugins/MapCompiler';
import { useCompilerStore } from '../store/compilerStore';

/** Great-circle distance between two lng/lat points, in meters (haversine). */
function haversineMeters(a: { lng: number; lat: number }, b: { lng: number; lat: number }): number {
  const R = 6_371_000; // mean Earth radius, meters
  const toRad = (deg: number) => (deg * Math.PI) / 180;
  const dLat = toRad(b.lat - a.lat);
  const dLng = toRad(b.lng - a.lng);
  const lat1 = toRad(a.lat);
  const lat2 = toRad(b.lat);
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(lat1) * Math.cos(lat2) * Math.sin(dLng / 2) ** 2;
  return 2 * R * Math.asin(Math.sqrt(h));
}

/** Formats a whole-second duration as HH:MM:SS. */
function formatElapsed(totalSeconds: number): string {
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = totalSeconds % 60;
  return [h, m, s].map((n) => String(n).padStart(2, '0')).join(':');
}


export default function App() {
  // ── Background worker health (drives the header status pill) ────────────────
  // P-FE.C3: the mapData (OPFS) worker is the only worker left after the
  // sovereignty cleanup; the pill reports its readiness.
  const [workerReady, setWorkerReady] = useState(false);

  // ── Native compiler state (granular selectors — App only re-renders when
  // these specific fields change, not on every compilerStore update) ─────────
  const isCompiling  = useCompilerStore((s) => s.isCompiling);
  const currentPhase = useCompilerStore((s) => s.currentPhase);

  // ── P9.C2: Region Picker sheet ───────────────────────────────────────────
  const [isRegionPickerOpen, setIsRegionPickerOpen] = useState(false);
  const isBackgroundCompiling = useCompilerStore((s) => s.isBackgroundCompiling);

  const [isStorageDurable, setIsStorageDurable] = useState<boolean | null>(null);

  // ── User-facing error banners (surfaced instead of console-only logging) ────
  const [locationPermissionDenied, setLocationPermissionDenied] = useState(false);
  const [mapDataError, setMapDataError] = useState<string | null>(null);

  // ── Phase 1 debug: native MapCompiler round-trip log ────────────────────────
  /** Rolling log (last 8 lines) of the native compile debug round-trip. */
  const [nativeDebugLines, setNativeDebugLines] = useState<string[]>([]);

  // ── Active Trip HUD state ──────────────────────────────────────────────────
  type TripStatus = 'idle' | 'active' | 'paused';
  const [tripStatus, setTripStatus] = useState<TripStatus>('idle');
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const [activeDistanceMeters, setActiveDistanceMeters] = useState(0);
  /** Read inside the GPS position callback, which is only ever attached once. */
  const tripStatusRef = useRef<TripStatus>('idle');
  const lastTripPositionRef = useRef<{ lng: number; lat: number } | null>(null);

  // ── Persistent storage request (durable OPFS) ───────────────────────────────
  useEffect(() => {
    (async () => {
      const status = await requestPersistentStorage();
      setIsStorageDurable(status.isPersistent);
    })();
  }, []);

  // ── Active Trip HUD: elapsed-time ticker ───────────────────────────────────
  useEffect(() => {
    if (tripStatus !== 'active') return;
    const id = window.setInterval(() => {
      setElapsedSeconds((s) => s + 1);
    }, 1_000);
    return () => window.clearInterval(id);
  }, [tripStatus]);

  // Keep the ref in sync so the GPS position callback (attached once, inside
  // MapView) always reads the current trip status without a stale closure.
  useEffect(() => { tripStatusRef.current = tripStatus; }, [tripStatus]);

  // ── Active Trip HUD: live GPS distance accumulation ───────────────────────
  const handlePositionUpdate = useCallback((pos: { lng: number; lat: number; accuracy: number }) => {
    if (tripStatusRef.current === 'active' && lastTripPositionRef.current) {
      const deltaMeters = haversineMeters(lastTripPositionRef.current, pos);
      // Ignore sub-meter jitter from GPS noise so distance doesn't creep while stationary.
      if (deltaMeters > 1) {
        setActiveDistanceMeters((d) => d + deltaMeters);
      }
    }
    lastTripPositionRef.current = { lng: pos.lng, lat: pos.lat };
  }, []);

  const handleStartHike = useCallback(() => {
    lastTripPositionRef.current = null;
    setElapsedSeconds(0);
    setActiveDistanceMeters(0);
    setTripStatus('active');
  }, []);

  const handlePauseHike = useCallback(() => {
    setTripStatus('paused');
  }, []);

  const handleResumeHike = useCallback(() => {
    // Drop the pre-pause fix so the paused gap isn't counted as movement.
    lastTripPositionRef.current = null;
    setTripStatus('active');
  }, []);

  const handleStopHike = useCallback(() => {
    setTripStatus('idle');
    setElapsedSeconds(0);
    setActiveDistanceMeters(0);
    lastTripPositionRef.current = null;
  }, []);

  // ── Phase 1 debug: MapCompiler wiring (WebView → Capacitor → UniFFI → Rust) ─
  const appendNativeDebugLine = useCallback((line: string) => {
    setNativeDebugLines((prev) => [...prev.slice(-7), line]);
  }, []);

  // Attach the progress (per-block) and status (per-slice) listeners once.
  // On the web (no native shell) addListener rejects with "not implemented" —
  // swallowed here, since the whole panel is a native-bridge debug aid.
  useEffect(() => {
    const progressPromise = MapCompiler.addListener('compilationProgress', (event) => {
      appendNativeDebugLine(`◈ ${event.percentage.toFixed(0)}% — ${event.status}`);
      // Low-frequency phase label — fine as Zustand state (one update per
      // processed block, not per byte).
      useCompilerStore.getState().setPhase(event.status);
    }).catch(() => null);

    const statusPromise = MapCompiler.addListener('compilationStatus', (event) => {
      appendNativeDebugLine(`◌ slice ${event.slices}: ${event.state}`);
      if (event.state !== 'yielded') {
        useCompilerStore.getState().setCompiling(false);
      }
      if (event.state === 'finished') {
        // Fire-and-forget: handleJobFinished owns its own error handling
        // (this listener callback is synchronous, so nothing here can await it).
        void useCompilerStore.getState().handleJobFinished(event.jobId);
      }
    }).catch(() => null);

    return () => {
      progressPromise.then((handle) => handle?.remove());
      statusPromise.then((handle) => handle?.remove());
    };
  }, [appendNativeDebugLine]);

  // ── P9.C1: background-job discovery + OPFS handoff orchestration ──────────
  // Two entry paths, one code path: eager discovery on cold boot (a
  // BGProcessingTask / WorkManager run may have finished — or still be
  // queued — while no WebView existed), plus a global 'backgroundCompile'
  // listener for terminal events that land while the app is open. The event
  // is treated purely as a doorbell: discovery re-queries the durable native
  // PendingJobStore record, which stays authoritative until acknowledged,
  // and ingestion is re-entrancy-guarded per jobId, so the two paths racing
  // is harmless.
  useEffect(() => {
    void useCompilerStore.getState().discoverBackgroundJobs();

    const backgroundPromise = MapCompiler.addListener('backgroundCompile', () => {
      void useCompilerStore.getState().discoverBackgroundJobs();
    }).catch(() => null); // web: no native bridge — background compiles are native-only

    return () => {
      backgroundPromise.then((handle) => handle?.remove());
    };
  }, []);

  // Debug button: proves the full Surface v1 loop — a deliberately tiny
  // per-slice budget forces the Rust engine to Yield repeatedly, so one tap
  // exercises checkpoint-write → re-invoke → resume several times before
  // the terminal Finished envelope resolves.
  const handleDebugNativeCompile = useCallback(async () => {
    if (useCompilerStore.getState().isCompiling) return;
    useCompilerStore.getState().setCompiling(true);

    const bbox = '11.1,47.1,11.6,47.45';
    try {
      // Cold-start resume detection: if a durable checkpoint survives (e.g.
      // the OS killed the app mid-compile), surface it before resuming.
      const existing = await MapCompiler.queryJob({ jobId: 'debug-compile' });
      if (existing.found) {
        appendNativeDebugLine(
          `↻ checkpoint found: ${existing.phase} block ${existing.nextBlock} (${existing.bytesWritten} bytes done) — resuming`,
        );
      } else {
        appendNativeDebugLine('∅ no checkpoint — fresh start');
      }

      appendNativeDebugLine(`→ startJob(${bbox}, budgetMs=25)`);
      const result = await MapCompiler.startJob({ bbox, jobId: 'debug-compile', budgetMs: 25 });
      if (result.status === 'finished') {
        appendNativeDebugLine(
          `← finished in ${result.slices} slices — ${result.blocksTotal} blocks, ${result.bytesWritten} bytes`,
        );
      } else {
        appendNativeDebugLine(`← ${result.status}${result.reason ? `: ${result.reason}` : ''} (${result.slices} slices)`);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      appendNativeDebugLine(`✕ ${message} — native shell required (web has no Rust core)`);
    } finally {
      useCompilerStore.getState().setCompiling(false);
    }
  }, [appendNativeDebugLine]);

  // ── User-facing error banner callbacks (memoised) ─────────────────────────
  const handleLocationPermissionDenied = useCallback(() => {
    setLocationPermissionDenied(true);
  }, []);

  const handleMapDataError = useCallback((message: string) => {
    setMapDataError(message);
  }, []);

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
              <strong>Storage warning:</strong> Storage is not persistent. Your offline map data is at risk of being evicted silently if your device runs low on disk space.
            </span>
          </div>
        </div>
      )}

      {locationPermissionDenied && (
        <div className="w-full max-w-6xl mb-6 p-4 rounded-xl bg-amber-500/10 border border-amber-500/30 flex items-center justify-between text-sm text-amber-400">
          <div className="flex items-center gap-2.5">
            <svg className="h-5 w-5 text-amber-400 shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M17.657 16.657L13.414 20.9a1.998 1.998 0 01-2.827 0l-4.244-4.243a8 8 0 1111.314 0z" />
              <path strokeLinecap="round" strokeLinejoin="round" d="M15 11a3 3 0 11-6 0 3 3 0 016 0z" />
            </svg>
            <span>
              <strong>Location access denied:</strong> Your position won't be tracked on the map. Enable location permission for this site in your browser settings to re-center on your hike.
            </span>
          </div>
          <button
            onClick={() => setLocationPermissionDenied(false)}
            className="ml-3 p-1 rounded text-amber-400 hover:text-amber-200 shrink-0 cursor-pointer"
            title="Dismiss"
          >
            <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M6 18L18 6M6 6l12 12" />
            </svg>
          </button>
        </div>
      )}

      {mapDataError && (
        <div className="w-full max-w-6xl mb-6 p-4 rounded-xl bg-rose-500/10 border border-rose-500/30 flex items-center justify-between text-sm text-rose-400">
          <div className="flex items-center gap-2.5">
            <svg className="h-5 w-5 text-rose-400 shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
            </svg>
            <span>
              <strong>Map data unavailable:</strong> {mapDataError}
            </span>
          </div>
          <button
            onClick={() => setMapDataError(null)}
            className="ml-3 p-1 rounded text-rose-400 hover:text-rose-200 shrink-0 cursor-pointer"
            title="Dismiss"
          >
            <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M6 18L18 6M6 6l12 12" />
            </svg>
          </button>
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
          {/* Region Picker trigger (P9.C2) — pulses while the OS owns a queued compile */}
          <button
            onClick={() => setIsRegionPickerOpen(true)}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-full bg-slate-900/60 hover:bg-slate-800/80 border border-slate-800 text-xs text-slate-300 font-semibold cursor-pointer transition-all active:scale-95"
          >
            <span
              className={
                isBackgroundCompiling
                  ? 'h-2 w-2 rounded-full bg-amber-400 animate-pulse'
                  : 'hidden'
              }
            />
            <svg className="h-3.5 w-3.5 text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M12 16.5V9.75m0 0l3 3m-3-3l-3 3M6.75 19.5a4.5 4.5 0 01-1.41-8.775 5.25 5.25 0 0110.233-2.33 3 3 0 013.758 3.848A3.752 3.752 0 0118 19.5H6.75z" />
            </svg>
            {isBackgroundCompiling ? 'Compiling…' : 'New Region'}
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

      {/* ── Background compile → OPFS handoff banner (P9.C1) ──────────────── */}
      <BackgroundHandoffBar />

      {/* ── Map ─────────────────────────────────────────────────────────────── */}
      <section className="w-full max-w-6xl mb-8 relative">
        <MapView
          onMapDataWorkerReady={() => {
            // Drives the header status pill: "connected" means the real
            // OPFS worker is up, not a placeholder heartbeat.
            setWorkerReady(true);
          }}
          onLocationPermissionDenied={handleLocationPermissionDenied}
          onMapDataError={handleMapDataError}
          onPositionUpdate={handlePositionUpdate}
        />
      </section>

      {/* ── Active Trip HUD ────────────────────────────────────────────────── */}
      <section className="w-full max-w-6xl mb-8 bg-slate-900/40 backdrop-blur-md border border-slate-900 rounded-2xl p-6 flex flex-col md:flex-row items-center gap-6">
        <div className="flex flex-col items-center gap-2 md:items-start">
          {tripStatus !== 'idle' && (
            <span className="flex items-center gap-1.5 text-[10px] font-mono uppercase tracking-widest text-slate-500">
              <span className={`h-1.5 w-1.5 rounded-full ${tripStatus === 'active' ? 'bg-emerald-500 animate-pulse' : 'bg-amber-500'}`} />
              {tripStatus === 'active' ? 'Recording' : 'Paused'}
            </span>
          )}

          {tripStatus === 'idle' ? (
            <button
              onClick={handleStartHike}
              className="flex items-center gap-2.5 px-8 py-4 rounded-2xl bg-gradient-to-r from-emerald-500 to-teal-500 text-slate-950 font-bold text-base hover:from-emerald-400 hover:to-teal-400 transition-all active:scale-95 shadow-lg shadow-emerald-500/20 cursor-pointer"
            >
              <svg className="h-5 w-5" viewBox="0 0 24 24" fill="currentColor">
                <path d="M6.5 5.653c0-.856.917-1.398 1.667-.986l11.54 6.347a1.125 1.125 0 010 1.972l-11.54 6.347A1.125 1.125 0 016.5 18.347V5.653z" />
              </svg>
              Start Hike
            </button>
          ) : (
            <div className="flex items-center gap-3">
              {tripStatus === 'active' ? (
                <button
                  onClick={handlePauseHike}
                  className="flex items-center gap-2 px-5 py-3 rounded-xl bg-slate-800 hover:bg-slate-700 border border-slate-700 text-slate-200 font-semibold text-sm transition-all active:scale-95 cursor-pointer"
                >
                  <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
                    <path d="M6.75 5.25a.75.75 0 00-.75.75v12a.75.75 0 00.75.75h2.5a.75.75 0 00.75-.75V6a.75.75 0 00-.75-.75h-2.5zm8 0a.75.75 0 00-.75.75v12c0 .414.336.75.75.75h2.5a.75.75 0 00.75-.75V6a.75.75 0 00-.75-.75h-2.5z" />
                  </svg>
                  Pause
                </button>
              ) : (
                <button
                  onClick={handleResumeHike}
                  className="flex items-center gap-2 px-5 py-3 rounded-xl bg-gradient-to-r from-emerald-500 to-teal-500 text-slate-950 font-bold text-sm hover:from-emerald-400 hover:to-teal-400 transition-all active:scale-95 cursor-pointer"
                >
                  <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
                    <path d="M6.5 5.653c0-.856.917-1.398 1.667-.986l11.54 6.347a1.125 1.125 0 010 1.972l-11.54 6.347A1.125 1.125 0 016.5 18.347V5.653z" />
                  </svg>
                  Resume
                </button>
              )}
              <button
                onClick={handleStopHike}
                className="flex items-center gap-2 px-5 py-3 rounded-xl bg-rose-950/40 hover:bg-rose-600 border border-rose-500/30 hover:border-rose-400 text-rose-300 hover:text-white font-semibold text-sm transition-all active:scale-95 cursor-pointer"
              >
                <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
                  <path d="M6 6.75A.75.75 0 016.75 6h10.5a.75.75 0 01.75.75v10.5a.75.75 0 01-.75.75H6.75a.75.75 0 01-.75-.75V6.75z" />
                </svg>
                Stop
              </button>
            </div>
          )}
        </div>

        <div className="flex-1 grid grid-cols-2 gap-4 w-full">
          <div className="p-4 rounded-xl bg-slate-950/50 border border-slate-900 text-center">
            <span className="text-[10px] uppercase tracking-widest text-slate-500 font-mono">Elapsed Time</span>
            <p className="text-2xl font-bold text-slate-100 font-mono mt-1 tabular-nums">{formatElapsed(elapsedSeconds)}</p>
          </div>
          <div className="p-4 rounded-xl bg-slate-950/50 border border-slate-900 text-center">
            <span className="text-[10px] uppercase tracking-widest text-slate-500 font-mono">Active Distance</span>
            <p className="text-2xl font-bold text-slate-100 font-mono mt-1 tabular-nums">{(activeDistanceMeters / 1000).toFixed(2)} km</p>
          </div>
        </div>
      </section>

      {/* ── Footer ─────────────────────────────────────────────────────────── */}
      <footer className="w-full max-w-6xl text-center border-t border-slate-900 pt-6 mt-8 text-xs text-slate-600">
        <p>© 2026 FreeHike contributors. Built with uncompromised client autonomy.</p>

        {/* Phase 1 debug: native compile bridge round-trip (discrete by design) */}
        <div className="mt-3 flex flex-col items-center gap-2">
          <button
            onClick={handleDebugNativeCompile}
            disabled={isCompiling}
            className="px-2.5 py-1 rounded border border-slate-800 bg-slate-900/40 hover:bg-slate-800/60 text-[10px] font-mono uppercase tracking-widest text-slate-500 hover:text-slate-300 transition-all cursor-pointer disabled:opacity-40 disabled:pointer-events-none"
            title="Fires MapCompiler.startJob + emitTestProgress through the native Rust bridge"
          >
            {isCompiling ? `Compiling… ${currentPhase}` : 'Debug Native Compile'}
          </button>
          {nativeDebugLines.length > 0 && (
            <div className="w-full max-w-xl text-left bg-slate-950/70 border border-slate-900 rounded-lg p-3 font-mono text-[10px] leading-relaxed text-slate-400 space-y-0.5">
              {nativeDebugLines.map((line, i) => (
                <p key={i} className="truncate" title={line}>{line}</p>
              ))}
            </div>
          )}
        </div>
      </footer>

      {/* ── Region Picker Sheet (P9.C2) ─────────────────────────────────────── */}
      <RegionPicker
        isOpen={isRegionPickerOpen}
        onClose={() => setIsRegionPickerOpen(false)}
      />

    </div>
  );
}
