// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from 'react';
import type maplibregl from 'maplibre-gl';
import { useMapStore } from '../../store/mapStore';
import { useCompilerStore } from '../../store/compilerStore';
import { enqueueRegionDownload } from '../../services/regionCompiler';

/** Reticle footprint as fractions of the map container — MUST stay in sync
 *  with the rendered box below (width/height style) since the geographic
 *  bounds are derived from these same fractions, not from DOM measurement. */
const RETICLE_W = 0.7;
const RETICLE_H = 0.6;

/** Mean Earth radius (m) for the km-readout haversine. */
const R_EARTH = 6_371_000;

interface ReticleBounds {
  /** "west,south,east,north" — the enqueueRegionDownload contract. */
  bbox: string;
  widthKm: number;
  heightKm: number;
}

/**
 * Geographic bounds of the fixed reticle under the CURRENT camera. All four
 * corners are unprojected and min/maxed rather than just SW/NE: under
 * residual pitch the screen rectangle maps to a trapezoid, and min/max
 * yields its bounding box instead of a skewed pair.
 */
function computeReticleBounds(map: maplibregl.Map): ReticleBounds {
  const container = map.getContainer();
  const w = container.clientWidth;
  const h = container.clientHeight;
  const halfW = (w * RETICLE_W) / 2;
  const halfH = (h * RETICLE_H) / 2;
  const cx = w / 2;
  const cy = h / 2;

  const corners = [
    map.unproject([cx - halfW, cy - halfH]),
    map.unproject([cx + halfW, cy - halfH]),
    map.unproject([cx - halfW, cy + halfH]),
    map.unproject([cx + halfW, cy + halfH]),
  ];

  const lons = corners.map((c) => c.lng);
  const lats = corners.map((c) => c.lat);
  const minLon = Math.max(-180, Math.min(...lons));
  const maxLon = Math.min(180, Math.max(...lons));
  const minLat = Math.max(-85, Math.min(...lats));
  const maxLat = Math.min(85, Math.max(...lats));

  const toRad = (deg: number) => (deg * Math.PI) / 180;
  const midLat = toRad((minLat + maxLat) / 2);
  const widthKm = (R_EARTH * Math.cos(midLat) * toRad(maxLon - minLon)) / 1000;
  const heightKm = (R_EARTH * toRad(maxLat - minLat)) / 1000;

  return {
    bbox: [minLon, minLat, maxLon, maxLat].map((v) => v.toFixed(4)).join(','),
    widthKm,
    heightKm,
  };
}

/**
 * P9.C3 — custom-region selection mode: a FIXED center-screen reticle the
 * user pans/zooms the map beneath (the mobile-friendly alternative to
 * drag-drawing a box). Mounted by MapView while mapStore.isSelectingRegion
 * is true; MapView hides its own control chrome for the duration so the
 * pointer-events-none mask can't leak clicks into dimmed buttons.
 *
 * Performance contract: this component renders on mount and on the handful
 * of coarse state changes (error, isBackgroundCompiling). The per-frame
 * bounds readout during panning is written straight to DOM textContent from
 * the map's 'move' listener — no React state, no VDOM work at 60 FPS.
 */
export default function RegionSelectorOverlay({ map }: { map: maplibregl.Map }) {
  const isBackgroundCompiling = useCompilerStore((s) => s.isBackgroundCompiling);

  const [isQueuing, setIsQueuing] = useState(false);
  const [queueError, setQueueError] = useState<string | null>(null);

  const bboxReadoutRef = useRef<HTMLSpanElement>(null);
  const sizeReadoutRef = useRef<HTMLSpanElement>(null);

  // Flatten the camera for the duration of the selection: the bbox is the
  // reticle's bounding box, and at pitch 45 that box balloons far beyond
  // what the user visually framed. easeTo keeps it smooth; the user can
  // re-pitch afterwards (we deliberately don't restore it).
  useEffect(() => {
    if (map.getPitch() > 0) {
      map.easeTo({ pitch: 0, duration: 400 });
    }
  }, [map]);

  // Live readout: direct-DOM writes from the 'move' stream (fires per
  // rendered frame while panning/zooming — exactly what must never touch
  // React). Also runs once immediately for the initial camera.
  useEffect(() => {
    const paint = () => {
      const { bbox, widthKm, heightKm } = computeReticleBounds(map);
      if (bboxReadoutRef.current) {
        bboxReadoutRef.current.textContent = bbox;
      }
      if (sizeReadoutRef.current) {
        sizeReadoutRef.current.textContent = `${widthKm.toFixed(1)} × ${heightKm.toFixed(1)} km`;
      }
    };
    paint();
    map.on('move', paint);
    return () => {
      map.off('move', paint);
    };
  }, [map]);

  // Esc = cancel, mirroring the sheet-close affordance.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') useMapStore.getState().setSelectingRegion(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const handleCancel = () => {
    useMapStore.getState().setSelectingRegion(false);
  };

  const handleConfirm = async () => {
    if (isQueuing || isBackgroundCompiling) return;
    setIsQueuing(true);
    setQueueError(null);

    // Bounds are computed at CONFIRM time from the live camera — the
    // readout refs are display-only and never read back.
    const { bbox } = computeReticleBounds(map);
    const result = await enqueueRegionDownload('Custom Region', bbox);

    if (result.queued) {
      useMapStore.getState().setSelectingRegion(false);
    } else {
      setQueueError(result.error ?? 'Unknown enqueue failure.');
      setIsQueuing(false);
    }
  };

  return (
    // The mask itself must not eat pointer events — panning/zooming the map
    // BENEATH the reticle is the whole interaction. Only the labelled
    // controls re-enable pointer events on themselves.
    <div className="absolute inset-0 z-30 pointer-events-none flex items-center justify-center">
      {/* Vignette outside the reticle */}
      <div className="absolute inset-0 bg-slate-950/50" />

      {/* Fixed reticle — fractions mirrored by computeReticleBounds */}
      <div className="relative" style={{ width: `${RETICLE_W * 100}%`, height: `${RETICLE_H * 100}%` }}>
        <div className="absolute inset-0 border-2 border-dashed border-emerald-400/70 rounded-lg" />
        <div className="absolute inset-0 bg-emerald-400/5 rounded-lg" />
        {(['tl', 'tr', 'bl', 'br'] as const).map((c) => (
          <span
            key={c}
            className={[
              'absolute h-4 w-4 border-emerald-400',
              c === 'tl' ? 'top-0 left-0 border-t-2 border-l-2 rounded-tl-sm' : '',
              c === 'tr' ? 'top-0 right-0 border-t-2 border-r-2 rounded-tr-sm' : '',
              c === 'bl' ? 'bottom-0 left-0 border-b-2 border-l-2 rounded-bl-sm' : '',
              c === 'br' ? 'bottom-0 right-0 border-b-2 border-r-2 rounded-br-sm' : '',
            ].join(' ')}
          />
        ))}
      </div>

      {/* Top hint + cancel */}
      <div className="absolute top-4 left-4 right-4 flex items-start justify-between gap-3">
        <div className="px-3 py-2 rounded-xl bg-slate-950/80 border border-emerald-500/30 text-[11px] font-mono text-emerald-300 tracking-wide">
          Pan &amp; zoom to frame the area to compile
        </div>
        <button
          onClick={handleCancel}
          className="pointer-events-auto p-2 rounded-xl border border-slate-700 bg-slate-950/80 hover:bg-slate-800 text-slate-300 hover:text-slate-100 transition-all cursor-pointer"
          title="Cancel selection (Esc)"
        >
          <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
            <path strokeLinecap="round" strokeLinejoin="round" d="M6 18L18 6M6 6l12 12" />
          </svg>
        </button>
      </div>

      {/* Bottom cluster: live readout + error + FAB */}
      <div className="absolute bottom-4 left-1/2 -translate-x-1/2 flex flex-col items-center gap-2.5 w-[min(92%,26rem)]">
        <div className="px-3 py-1.5 rounded-xl bg-slate-950/80 border border-slate-800 font-mono text-[10px] text-slate-400 flex items-center gap-3 max-w-full">
          {/* Written imperatively from the map 'move' listener — React never
              re-renders these during panning. */}
          <span ref={sizeReadoutRef} className="text-emerald-300 font-bold shrink-0 tabular-nums" />
          <span ref={bboxReadoutRef} className="truncate tabular-nums" />
        </div>

        {queueError && (
          <div className="pointer-events-auto px-3 py-2 rounded-xl bg-rose-500/10 border border-rose-500/30 text-[11px] text-rose-300 text-center">
            <strong>Couldn't queue the compile:</strong> {queueError}
          </div>
        )}

        {isBackgroundCompiling && (
          <div className="px-3 py-2 rounded-xl bg-amber-500/10 border border-amber-500/30 text-[11px] text-amber-300 text-center">
            A background compile is already queued — one region at a time.
          </div>
        )}

        <button
          onClick={handleConfirm}
          disabled={isQueuing || isBackgroundCompiling}
          className="pointer-events-auto flex items-center gap-2.5 px-7 py-3.5 rounded-2xl bg-gradient-to-r from-emerald-500 to-teal-500 text-slate-950 font-bold text-sm hover:from-emerald-400 hover:to-teal-400 transition-all active:scale-95 shadow-lg shadow-emerald-500/30 cursor-pointer disabled:opacity-40 disabled:pointer-events-none"
        >
          {isQueuing ? (
            <>
              <span className="h-4 w-4 rounded-full border-2 border-slate-950/20 border-t-slate-950 animate-spin" />
              Queuing…
            </>
          ) : (
            <>
              <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M12 16.5V9.75m0 0l3 3m-3-3l-3 3M6.75 19.5a4.5 4.5 0 01-1.41-8.775 5.25 5.25 0 0110.233-2.33 3 3 0 013.758 3.848A3.752 3.752 0 0118 19.5H6.75z" />
              </svg>
              Download This Area
            </>
          )}
        </button>
      </div>
    </div>
  );
}
