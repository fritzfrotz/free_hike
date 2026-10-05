// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef } from 'react';
import { useCompilerStore } from '../../store/compilerStore';
import { readFetchProgress } from '../../services/fetchProgress';
import { cancelInputsFetch } from '../../services/regionCompiler';

/**
 * P-SOV.C2b — progress for the native raw-input download.
 *
 * Re-renders only on the coarse fetch stage. The percentage and Rust's
 * status line NEVER pass through React: the 'fetchProgress' listener writes
 * the fetchProgress ref sink and a requestAnimationFrame loop here paints
 * `transform` / `textContent` straight onto the DOM (ARCHITECTURE.md P8).
 */
export default function FetchProgressBar() {
  const stage = useCompilerStore((s) => s.fetchStage);
  const error = useCompilerStore((s) => s.fetchError);
  const sourceId = useCompilerStore((s) => s.activeSourceId);

  const barRef = useRef<HTMLDivElement>(null);
  const readoutRef = useRef<HTMLSpanElement>(null);
  const rafIdRef = useRef<number | null>(null);

  useEffect(() => {
    if (stage !== 'fetching') {
      if (barRef.current) barRef.current.style.transform = `scaleX(${stage === 'ready' ? 1 : 0})`;
      return;
    }
    const tick = () => {
      const { percentage, status } = readFetchProgress();
      if (barRef.current) barRef.current.style.transform = `scaleX(${Math.min(percentage, 100) / 100})`;
      if (readoutRef.current) readoutRef.current.textContent = status || 'Connecting to the mirror…';
      rafIdRef.current = requestAnimationFrame(tick);
    };
    rafIdRef.current = requestAnimationFrame(tick);
    return () => {
      if (rafIdRef.current !== null) cancelAnimationFrame(rafIdRef.current);
      rafIdRef.current = null;
    };
  }, [stage]);

  if (stage === 'idle') return null;

  return (
    <div
      className={[
        'p-3 rounded-xl border text-xs',
        stage === 'error'
          ? 'bg-rose-500/10 border-rose-500/30 text-rose-300'
          : 'bg-teal-500/10 border-teal-500/30 text-teal-300',
      ].join(' ')}
    >
      <div className="flex items-center justify-between gap-3 mb-2">
        <span className="truncate">
          {stage === 'fetching' && <><strong>Downloading region data</strong> — {sourceId}</>}
          {stage === 'ready' && <><strong>Region data verified</strong> — {sourceId} is on the device.</>}
          {stage === 'error' && <><strong>Download failed:</strong> {error}</>}
        </span>
        {stage === 'fetching' && (
          <button
            onClick={() => void cancelInputsFetch()}
            className="shrink-0 px-2 py-1 rounded-lg border border-slate-700 text-slate-300 hover:bg-slate-800 cursor-pointer"
            title="Stop the download (it resumes from where it stopped)"
          >
            Pause
          </button>
        )}
      </div>
      {stage === 'fetching' && (
        // Written imperatively by the rAF loop — React never re-renders this.
        <span ref={readoutRef} className="block font-mono text-[10px] text-teal-400/80 tabular-nums mb-2" />
      )}
      {(stage === 'fetching' || stage === 'ready') && (
        <div className="h-1.5 w-full rounded-full bg-slate-800/80 overflow-hidden">
          <div
            ref={barRef}
            className="h-full w-full origin-left rounded-full bg-gradient-to-r from-teal-500 to-emerald-400 transition-none"
            style={{ transform: 'scaleX(0)' }}
          />
        </div>
      )}
    </div>
  );
}
