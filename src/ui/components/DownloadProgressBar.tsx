// SPDX-License-Identifier: Apache-2.0
import { forwardRef, useEffect, useImperativeHandle, useRef } from 'react';
import { useMapStore } from '../../store/mapStore';

/**
 * Imperative sink for byte-level download telemetry. Deliberately exposed as
 * an imperative handle rather than props/state — callers push updates at
 * native-bridge frequency (50-100/sec) without ever triggering a React
 * reconciliation on this component or its ancestors.
 */
export interface DownloadProgressHandle {
  /** Add processed bytes without triggering a re-render. */
  addBytes: (n: number) => void;
  /** Reset the counters ahead of a new fetch; totalBytes may be 0 (unknown). */
  reset: (totalBytes: number) => void;
}

/**
 * Renders the byte-progress fill for the region-download panel.
 *
 * Reads `regionDownloadStatus` from useMapStore (a handful of transitions —
 * cheap to re-render on) but keeps the actual byte counter in a plain ref,
 * mutated directly via the imperative handle and painted through a
 * requestAnimationFrame loop that writes CSS `transform` straight to the DOM
 * node. No state update, no Virtual DOM diff, regardless of update rate.
 */
const DownloadProgressBar = forwardRef<DownloadProgressHandle>((_props, ref) => {
  const status = useMapStore((s) => s.regionDownloadStatus);

  const bytesProcessedRef = useRef(0);
  const totalBytesRef = useRef(1); // avoid divide-by-zero before the first reset()
  const barRef = useRef<HTMLDivElement>(null);
  const rafIdRef = useRef<number | null>(null);

  useImperativeHandle(ref, () => ({
    addBytes: (n: number) => {
      bytesProcessedRef.current += n;
    },
    reset: (totalBytes: number) => {
      bytesProcessedRef.current = 0;
      totalBytesRef.current = Math.max(totalBytes, 1);
    },
  }), []);

  // Paint loop: reads the ref every frame and writes directly to the DOM.
  useEffect(() => {
    const active = status === 'fetching' || status === 'writing';
    if (!active) {
      if (barRef.current) {
        barRef.current.style.transform = `scaleX(${status === 'done' ? 1 : 0})`;
      }
      return;
    }

    const tick = () => {
      const pct = Math.min(bytesProcessedRef.current / totalBytesRef.current, 1);
      if (barRef.current) {
        barRef.current.style.transform = `scaleX(${pct})`;
      }
      rafIdRef.current = requestAnimationFrame(tick);
    };
    rafIdRef.current = requestAnimationFrame(tick);

    return () => {
      if (rafIdRef.current !== null) cancelAnimationFrame(rafIdRef.current);
      rafIdRef.current = null;
    };
  }, [status]);

  // The OPFS write phase has no real chunked feedback from the worker (it
  // reports one bulk MAP_INIT-style success, not per-chunk progress) — a
  // high-frequency simulated advance keeps the bar moving smoothly instead
  // of stalling for the duration of the write.
  useEffect(() => {
    if (status !== 'writing') return;

    const id = window.setInterval(() => {
      const remaining = totalBytesRef.current - bytesProcessedRef.current;
      bytesProcessedRef.current += Math.max(remaining * 0.08, 1);
    }, 10); // 100 Hz, matching the native bridge's expected event cadence

    return () => window.clearInterval(id);
  }, [status]);

  return (
    <div className="h-1.5 w-full rounded-full bg-slate-800/80 overflow-hidden">
      <div
        ref={barRef}
        className="h-full w-full origin-left rounded-full bg-gradient-to-r from-blue-500 to-teal-400 transition-none"
        style={{ transform: 'scaleX(0)' }}
      />
    </div>
  );
});

DownloadProgressBar.displayName = 'DownloadProgressBar';

export default DownloadProgressBar;
