// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from 'react';
import { useCompilerStore } from '../../store/compilerStore';
import { useMapStore } from '../../store/mapStore';
import {
  COMPILE_MAX_ZOOM,
  COMPILE_MIN_ZOOM,
  DEMO_SOURCE_ID,
  ensureInputs,
  enqueueRegionDownload,
  verifiedInputPath,
} from '../../services/regionCompiler';
import FetchProgressBar from './FetchProgressBar';

/**
 * P-SOV.C2b — the one v0.1 demo region (DL-003): Portugal via Geofabrik,
 * demo area Lisbon / Sintra / Arrábida. The raw extract is fetched natively
 * (step 1) and only a verified file can be compiled (step 2). Bbox is
 * "west,south,east,north" WGS84.
 */
const DEMO_REGION = {
  sourceId: DEMO_SOURCE_ID,
  name: 'Portugal',
  detail: 'Lisbon · Sintra · Arrábida — Geofabrik daily extract',
  bbox: '-9.55,38.38,-8.75,38.95',
  sizeHint: '~403 MiB download',
};

type SubmitState = 'idle' | 'submitting' | 'queued' | 'error';

interface RegionPickerProps {
  isOpen: boolean;
  onClose: () => void;
}

/**
 * Bottom-sheet for the two-step region flow.
 *
 * Step 1 (Download) drives MapCompiler.fetchInputs through
 * regionCompiler.ensureInputs — resumable across kills, cancellable between
 * slices. Step 2 (Compile) calls MapCompiler.enqueueBackgroundJob with the
 * SOURCE ID; the native layer resolves the verified file and refuses
 * anything else (D6). The native PendingJobStore is single-job, so the
 * compile button hard-disables while a job is queued.
 */
export default function RegionPicker({ isOpen, onClose }: RegionPickerProps) {
  const isBackgroundCompiling = useCompilerStore((s) => s.isBackgroundCompiling);
  const fetchStage = useCompilerStore((s) => s.fetchStage);

  const [submitState, setSubmitState] = useState<SubmitState>('idle');
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [fetchError, setFetchError] = useState<string | null>(null);

  // On open, discover an already-verified extract (previous session) so the
  // sheet opens on step 2. Native-only: the web build's rejection is ignored.
  useEffect(() => {
    if (!isOpen || fetchStage !== 'idle') return;
    let cancelled = false;
    void verifiedInputPath(DEMO_REGION.sourceId)
      .then((path) => {
        if (!cancelled && path) {
          useCompilerStore.getState().setFetchStage('ready', DEMO_REGION.sourceId);
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [isOpen, fetchStage]);

  if (!isOpen) return null;

  const inputsReady = fetchStage === 'ready';
  const fetching = fetchStage === 'fetching';
  const busy = isBackgroundCompiling || submitState === 'submitting' || fetching;

  const handleDownload = async () => {
    if (fetching || inputsReady) return;
    setFetchError(null);
    const result = await ensureInputs(DEMO_REGION.sourceId);
    if (!result.ready && !result.cancelled) {
      setFetchError(result.error ?? 'Unknown download failure.');
    }
  };

  const handleConfirm = async () => {
    if (busy || !inputsReady) return;
    setSubmitState('submitting');
    setSubmitError(null);
    const result = await enqueueRegionDownload(DEMO_REGION.sourceId, DEMO_REGION.name, DEMO_REGION.bbox);
    if (result.queued) {
      setSubmitState('queued');
    } else {
      setSubmitState('error');
      setSubmitError(result.error ?? 'Unknown enqueue failure.');
    }
  };

  /** P9.C3: hand off to the map's fixed-reticle selection mode — the sheet
   *  closes and RegionSelectorOverlay (MapView) takes over. The overlay's
   *  enqueue goes through the same D6 gate, so it needs step 1 done too. */
  const handleCustomArea = () => {
    if (busy || !inputsReady) return;
    useMapStore.getState().setSelectingRegion(true);
    onClose();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-end sm:items-center justify-center pointer-events-none">
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-slate-950/40 backdrop-blur-sm pointer-events-auto transition-opacity"
        onClick={onClose}
      />

      {/* Bottom sheet (centers on ≥sm screens) */}
      <div className="relative w-full sm:max-w-lg bg-slate-900/95 backdrop-blur-xl border-t sm:border border-slate-800 sm:rounded-2xl pointer-events-auto shadow-2xl flex flex-col z-10 max-h-[85vh]">
        {/* Header */}
        <div className="flex items-center justify-between p-6 border-b border-slate-800/80">
          <div className="flex items-center gap-2.5">
            <div className="h-8 w-8 rounded-lg bg-gradient-to-tr from-emerald-600 to-teal-500 flex items-center justify-center shadow-lg shadow-emerald-500/10">
              <svg className="h-4.5 w-4.5 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M12 16.5V9.75m0 0l3 3m-3-3l-3 3M6.75 19.5a4.5 4.5 0 01-1.41-8.775 5.25 5.25 0 0110.233-2.33 3 3 0 013.758 3.848A3.752 3.752 0 0118 19.5H6.75z" />
              </svg>
            </div>
            <div>
              <h2 className="text-sm font-bold text-slate-100 tracking-tight">Offline Region</h2>
              <p className="text-[10px] font-mono text-slate-500 uppercase tracking-widest">1 · Download raw data &nbsp; 2 · Compile while charging</p>
            </div>
          </div>

          <button
            onClick={onClose}
            className="p-1.5 rounded-lg border border-slate-800 bg-slate-950/60 hover:bg-slate-800 text-slate-400 hover:text-slate-200 transition-all cursor-pointer"
            title="Close"
          >
            <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M6 18L18 6M6 6l12 12" />
            </svg>
          </button>
        </div>

        {/* Region + step 1 */}
        <div className="flex-1 overflow-y-auto p-6 space-y-3">
          <div className="w-full text-left p-4 rounded-2xl border bg-emerald-500/10 border-emerald-500/40">
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <h4 className="text-xs font-bold truncate text-emerald-300">{DEMO_REGION.name}</h4>
                <p className="text-[10px] font-mono text-slate-500 mt-0.5 truncate">{DEMO_REGION.detail}</p>
                <p className="text-[9px] font-mono text-slate-600 mt-1">bbox {DEMO_REGION.bbox} · z{COMPILE_MIN_ZOOM}–{COMPILE_MAX_ZOOM}</p>
              </div>
              <span className="text-[10px] font-mono text-slate-500 shrink-0">{DEMO_REGION.sizeHint}</span>
            </div>
          </div>

          <button
            onClick={handleDownload}
            disabled={fetching || inputsReady}
            className="w-full flex items-center justify-center gap-2.5 px-6 py-3 rounded-xl border border-teal-500/40 bg-teal-500/10 text-teal-200 font-semibold text-sm hover:bg-teal-500/20 transition-all cursor-pointer disabled:opacity-40 disabled:pointer-events-none"
          >
            {inputsReady ? '1 · Region data on device' : fetching ? '1 · Downloading…' : '1 · Download region data'}
          </button>

          <FetchProgressBar />

          {fetchError && (
            <div className="flex items-center gap-2.5 p-3 rounded-xl bg-rose-500/10 border border-rose-500/30 text-xs text-rose-300">
              <span className="h-2 w-2 rounded-full bg-rose-500 shrink-0" />
              <span><strong>Couldn't download:</strong> {fetchError}</span>
            </div>
          )}

          {/* Custom area — hands off to the map's fixed-reticle selection mode */}
          <button
            onClick={handleCustomArea}
            disabled={busy || !inputsReady}
            className="w-full text-left p-4 rounded-2xl border-2 border-dashed border-slate-700/70 bg-slate-950/30 hover:border-emerald-500/40 hover:bg-emerald-500/5 transition-all cursor-pointer disabled:opacity-50 disabled:cursor-default"
          >
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <h4 className="text-xs font-bold text-slate-200">Custom Area</h4>
                <p className="text-[10px] font-mono text-slate-500 mt-0.5">
                  Frame any area inside the downloaded extract with a selection reticle
                </p>
              </div>
              <svg className="h-5 w-5 text-slate-500 shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M7.5 3.75H6A2.25 2.25 0 003.75 6v1.5M16.5 3.75H18A2.25 2.25 0 0120.25 6v1.5m0 9V18A2.25 2.25 0 0118 20.25h-1.5m-9 0H6A2.25 2.25 0 013.75 18v-1.5M12 12h.008v.008H12V12z" />
              </svg>
            </div>
          </button>
        </div>

        {/* Footer: status + step 2 */}
        <div className="p-6 border-t border-slate-800/80 space-y-3">
          {isBackgroundCompiling && (
            <div className="flex items-center gap-2.5 p-3 rounded-xl bg-amber-500/10 border border-amber-500/30 text-xs text-amber-300">
              <span className="h-2 w-2 rounded-full bg-amber-400 animate-pulse shrink-0" />
              <span>
                <strong>A background compile is already queued.</strong> The OS runs it while the
                device charges; the map updates automatically when it lands. One region at a time.
              </span>
            </div>
          )}

          {submitState === 'queued' && !isBackgroundCompiling && (
            <div className="flex items-center gap-2.5 p-3 rounded-xl bg-emerald-500/10 border border-emerald-500/30 text-xs text-emerald-300">
              <span className="h-2 w-2 rounded-full bg-emerald-400 shrink-0" />
              <span><strong>Queued.</strong> The compile is now managed by the OS scheduler.</span>
            </div>
          )}

          {submitState === 'error' && submitError && (
            <div className="flex items-center gap-2.5 p-3 rounded-xl bg-rose-500/10 border border-rose-500/30 text-xs text-rose-300">
              <span className="h-2 w-2 rounded-full bg-rose-500 shrink-0" />
              <span><strong>Couldn't queue the compile:</strong> {submitError}</span>
            </div>
          )}

          <button
            onClick={handleConfirm}
            disabled={busy || !inputsReady}
            className="w-full flex items-center justify-center gap-2.5 px-6 py-3.5 rounded-xl bg-gradient-to-r from-emerald-500 to-teal-500 text-slate-950 font-bold text-sm hover:from-emerald-400 hover:to-teal-400 transition-all active:scale-[0.98] shadow-lg shadow-emerald-500/20 cursor-pointer disabled:opacity-40 disabled:pointer-events-none"
          >
            {isBackgroundCompiling ? (
              'Background Task Active'
            ) : submitState === 'submitting' ? (
              <>
                <span className="h-4 w-4 rounded-full border-2 border-slate-950/20 border-t-slate-950 animate-spin" />
                Queuing…
              </>
            ) : (
              <>2 · Compile "{DEMO_REGION.name}" in Background</>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
