// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef } from 'react';
import { useCompilerStore } from '../../store/compilerStore';
import { readHandoffProgress } from '../../services/handoffProgress';

/** Formats a byte count as a fixed-point MB figure for the copy readout. */
function toMB(bytes: number): string {
  return (bytes / (1024 * 1024)).toFixed(1);
}

/**
 * P9.C1 — banner for the background-compile → OPFS handoff pipeline.
 *
 * Re-renders only on coarse stage transitions (a handful per handoff, read
 * from useCompilerStore with granular selectors). Byte-level copy progress
 * NEVER passes through React: opfsMover reports into the handoffProgress ref
 * sink, and a requestAnimationFrame loop here reads that sink and writes CSS
 * `transform` / `textContent` straight onto the DOM nodes — zero VDOM diffs
 * at any update rate, keeping the copy phase at a stable 60 FPS.
 */
export default function BackgroundHandoffBar() {
  const stage = useCompilerStore((s) => s.backgroundProgress.stage);
  const jobId = useCompilerStore((s) => s.backgroundProgress.jobId);
  const error = useCompilerStore((s) => s.backgroundProgress.error);
  const isBackgroundCompiling = useCompilerStore((s) => s.isBackgroundCompiling);

  const barRef = useRef<HTMLDivElement>(null);
  const readoutRef = useRef<HTMLSpanElement>(null);
  const rafIdRef = useRef<number | null>(null);

  // Paint loop — alive only during the copy stage.
  useEffect(() => {
    if (stage !== 'copying') {
      if (barRef.current) {
        barRef.current.style.transform = `scaleX(${stage === 'swapping' || stage === 'done' ? 1 : 0})`;
      }
      return;
    }

    const tick = () => {
      const { bytesWritten, totalBytes } = readHandoffProgress();
      const pct = totalBytes > 0 ? Math.min(bytesWritten / totalBytes, 1) : 0;
      if (barRef.current) {
        barRef.current.style.transform = `scaleX(${pct})`;
      }
      if (readoutRef.current) {
        readoutRef.current.textContent =
          totalBytes > 0
            ? `${toMB(bytesWritten)} / ${toMB(totalBytes)} MB (${Math.round(pct * 100)}%)`
            : `${toMB(bytesWritten)} MB`;
      }
      rafIdRef.current = requestAnimationFrame(tick);
    };
    rafIdRef.current = requestAnimationFrame(tick);

    return () => {
      if (rafIdRef.current !== null) cancelAnimationFrame(rafIdRef.current);
      rafIdRef.current = null;
    };
  }, [stage]);

  const handoffActive = stage !== 'idle';
  if (!handoffActive && !isBackgroundCompiling) return null;

  return (
    <div
      className={[
        'w-full max-w-6xl mb-6 p-4 rounded-xl border text-sm',
        stage === 'error'
          ? 'bg-rose-500/10 border-rose-500/30 text-rose-400'
          : 'bg-teal-500/10 border-teal-500/30 text-teal-300',
      ].join(' ')}
    >
      <div className="flex items-center justify-between gap-3 mb-2">
        <div className="flex items-center gap-2.5 min-w-0">
          <span
            className={[
              'h-2 w-2 rounded-full shrink-0',
              stage === 'error'   ? 'bg-rose-500' :
              stage === 'done'    ? 'bg-emerald-400' :
              handoffActive       ? 'bg-teal-400 animate-pulse' :
                                    'bg-amber-400 animate-pulse',
            ].join(' ')}
          />
          <span className="truncate">
            {stage === 'copying'  && <><strong>Importing compiled region</strong> — streaming {jobId} into offline storage…</>}
            {stage === 'swapping' && <><strong>Activating region</strong> — swapping live map sources…</>}
            {stage === 'done'     && <><strong>Region ready:</strong> {jobId} is now the active offline map.</>}
            {stage === 'error'    && <><strong>Background import failed:</strong> {error}</>}
            {stage === 'idle'     && isBackgroundCompiling && (
              <><strong>Background compile queued</strong> — it will run while the device is charging.</>
            )}
          </span>
        </div>
        {stage === 'copying' && (
          // Written imperatively by the rAF loop — React never re-renders this.
          <span ref={readoutRef} className="font-mono text-xs text-teal-400/80 tabular-nums shrink-0" />
        )}
      </div>

      {(stage === 'copying' || stage === 'swapping' || stage === 'done') && (
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
