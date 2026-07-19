// SPDX-License-Identifier: Apache-2.0
import { useState } from 'react';
import { useCompilerStore } from '../../store/compilerStore';
import { useMapStore } from '../../store/mapStore';
import { enqueueRegionDownload, COMPILE_MIN_ZOOM, COMPILE_MAX_ZOOM } from '../../services/regionCompiler';

/**
 * P9.C2 — hardcoded test regions for the background-compile pipeline. Bboxes
 * are "west,south,east,north" WGS84, all inside the Innsbruck fixture
 * coverage the native engine's sandbox inputs describe. A drawn bounding-box
 * selector replaces this list in a later chunk.
 */
interface CompileRegion {
  /** Filesystem-safe slug — becomes part of the jobId, which in turn names
   *  the native archive and its OPFS copy (`{jobId}.pmtiles`). */
  slug: string;
  name: string;
  detail: string;
  bbox: string;
  /** Rough compiled-archive estimate shown in the card, purely informative. */
  sizeHint: string;
}

const TEST_REGIONS: CompileRegion[] = [
  {
    slug: 'innsbruck-area',
    name: 'Innsbruck Area',
    detail: 'City + Nordkette · Tyrol, Austria',
    bbox: '11.1,47.1,11.6,47.45',
    sizeHint: '~15 MB',
  },
  {
    slug: 'innsbruck-wide',
    name: 'Innsbruck Wide',
    detail: 'Inn valley + Stubai approaches',
    bbox: '10.9,47.0,11.8,47.5',
    sizeHint: '~40 MB',
  },
  {
    slug: 'patscherkofel',
    name: 'Patscherkofel',
    detail: 'Summit trails south of the Inn',
    bbox: '11.35,47.15,11.55,47.25',
    sizeHint: '~6 MB',
  },
];

type SubmitState = 'idle' | 'submitting' | 'queued' | 'error';

interface RegionPickerProps {
  isOpen: boolean;
  onClose: () => void;
}

/**
 * Bottom-sheet for queuing a background offline compile.
 *
 * Confirming calls MapCompiler.enqueueBackgroundJob() — the OS then owns the
 * job (BGProcessingTask / WorkManager, charging-gated), and the result flows
 * back through the P9.C1 discovery → OPFS ingest → hot-swap pipeline with no
 * further involvement from this component. Because the native PendingJobStore
 * is single-job by design (a second enqueue would overwrite the record), the
 * confirm button hard-disables while `isBackgroundCompiling` reports a
 * queued/running job.
 */
export default function RegionPicker({ isOpen, onClose }: RegionPickerProps) {
  const isBackgroundCompiling = useCompilerStore((s) => s.isBackgroundCompiling);

  const [selectedSlug, setSelectedSlug] = useState<string>(TEST_REGIONS[0].slug);
  const [submitState, setSubmitState] = useState<SubmitState>('idle');
  const [submitError, setSubmitError] = useState<string | null>(null);

  if (!isOpen) return null;

  const selected = TEST_REGIONS.find((r) => r.slug === selectedSlug) ?? TEST_REGIONS[0];
  const busy = isBackgroundCompiling || submitState === 'submitting';

  const handleConfirm = async () => {
    if (busy) return;
    setSubmitState('submitting');
    setSubmitError(null);

    const result = await enqueueRegionDownload(selected.name, selected.bbox);
    if (result.queued) {
      setSubmitState('queued');
    } else {
      setSubmitState('error');
      setSubmitError(result.error ?? 'Unknown enqueue failure.');
    }
  };

  /** P9.C3: hand off to the map's fixed-reticle selection mode — the sheet
   *  closes and RegionSelectorOverlay (MapView) takes over. */
  const handleCustomArea = () => {
    if (busy) return;
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
              <h2 className="text-sm font-bold text-slate-100 tracking-tight">Compile Offline Region</h2>
              <p className="text-[10px] font-mono text-slate-500 uppercase tracking-widest">Background · Runs While Charging</p>
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

        {/* Region list */}
        <div className="flex-1 overflow-y-auto p-6 space-y-3">
          {TEST_REGIONS.map((region) => {
            const isSelected = region.slug === selectedSlug;
            return (
              <button
                key={region.slug}
                onClick={() => setSelectedSlug(region.slug)}
                disabled={busy}
                className={[
                  'w-full text-left p-4 rounded-2xl border transition-all cursor-pointer disabled:opacity-50 disabled:cursor-default',
                  isSelected
                    ? 'bg-emerald-500/10 border-emerald-500/40'
                    : 'bg-slate-950/50 border-slate-800/60 hover:border-slate-700/60',
                ].join(' ')}
              >
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <h4 className={`text-xs font-bold truncate ${isSelected ? 'text-emerald-300' : 'text-slate-200'}`}>
                      {region.name}
                    </h4>
                    <p className="text-[10px] font-mono text-slate-500 mt-0.5 truncate">{region.detail}</p>
                    <p className="text-[9px] font-mono text-slate-600 mt-1">bbox {region.bbox} · z{COMPILE_MIN_ZOOM}–{COMPILE_MAX_ZOOM}</p>
                  </div>
                  <div className="flex flex-col items-end gap-1.5 shrink-0">
                    <span className="text-[10px] font-mono text-slate-500">{region.sizeHint}</span>
                    <span
                      className={[
                        'h-4 w-4 rounded-full border-2 flex items-center justify-center',
                        isSelected ? 'border-emerald-400' : 'border-slate-700',
                      ].join(' ')}
                    >
                      {isSelected && <span className="h-2 w-2 rounded-full bg-emerald-400" />}
                    </span>
                  </div>
                </div>
              </button>
            );
          })}

          {/* Custom area — hands off to the map's fixed-reticle selection mode */}
          <button
            onClick={handleCustomArea}
            disabled={busy}
            className="w-full text-left p-4 rounded-2xl border-2 border-dashed border-slate-700/70 bg-slate-950/30 hover:border-emerald-500/40 hover:bg-emerald-500/5 transition-all cursor-pointer disabled:opacity-50 disabled:cursor-default"
          >
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <h4 className="text-xs font-bold text-slate-200">Custom Area</h4>
                <p className="text-[10px] font-mono text-slate-500 mt-0.5">
                  Frame any area on the map with a selection reticle
                </p>
              </div>
              <svg className="h-5 w-5 text-slate-500 shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M7.5 3.75H6A2.25 2.25 0 003.75 6v1.5M16.5 3.75H18A2.25 2.25 0 0120.25 6v1.5m0 9V18A2.25 2.25 0 0118 20.25h-1.5m-9 0H6A2.25 2.25 0 013.75 18v-1.5M12 12h.008v.008H12V12z" />
              </svg>
            </div>
          </button>
        </div>

        {/* Footer: status + confirm */}
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
            disabled={busy}
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
              <>
                <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
                  <path strokeLinecap="round" strokeLinejoin="round" d="M12 16.5V9.75m0 0l3 3m-3-3l-3 3M6.75 19.5a4.5 4.5 0 01-1.41-8.775 5.25 5.25 0 0110.233-2.33 3 3 0 013.758 3.848A3.752 3.752 0 0118 19.5H6.75z" />
                </svg>
                Compile "{selected.name}" in Background
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
