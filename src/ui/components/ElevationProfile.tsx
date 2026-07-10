import { useRef, useCallback, useState } from 'react';
import type { ElevationProfileSuccessPayload } from '../../shared/types';

interface ElevationProfileProps {
  data: ElevationProfileSuccessPayload;
  /** Called with the nearest point index when the user hovers, or null on leave. */
  onHoverIndex?: (index: number | null) => void;
  /** Called when the user confirms saving the hike. */
  onSaveHike?: (title: string) => Promise<void>;
}

// ---------------------------------------------------------------------------
// Constants — fixed SVG coordinate system
// ---------------------------------------------------------------------------

const VB_W   = 1000; // viewBox width
const VB_H   = 200;  // viewBox height
const PAD_X  = 12;   // left / right horizontal padding inside viewBox
const PAD_Y  = 18;   // top / bottom vertical padding inside viewBox

const PLOT_W = VB_W - PAD_X * 2; // 976
const PLOT_H = VB_H - PAD_Y * 2; // 164

// ---------------------------------------------------------------------------
// Core: build SVG path string from Float64Array — single pass, no temp arrays
// ---------------------------------------------------------------------------

function buildSvgPath(elevations: Float64Array): { path: string; minElev: number; maxElev: number } {
  const n = elevations.length;
  if (n === 0) return { path: '', minElev: 0, maxElev: 0 };

  // ── Pass 1: find min / max ─────────────────────────────────────────────
  let minElev = elevations[0];
  let maxElev = elevations[0];
  for (let i = 1; i < n; i++) {
    const e = elevations[i];
    if (e < minElev) minElev = e;
    if (e > maxElev) maxElev = e;
  }

  const range = maxElev - minElev;
  // Guard against flat terrain: give it a 5 m artificial range so we still render a line.
  const effectiveRange = range < 5 ? 5 : range;
  const midOffset = range < 5 ? 2.5 : 0;

  // ── Pass 2: build the path string ────────────────────────────────────────
  // y is inverted: higher elevation → lower SVG y value (closer to top).
  let d = '';

  for (let i = 0; i < n; i++) {
    const x = PAD_X + (i / (n - 1)) * PLOT_W;
    const normalised = (elevations[i] - (minElev - midOffset)) / effectiveRange;
    const y = PAD_Y + PLOT_H - normalised * PLOT_H;

    if (i === 0) {
      d += `M ${x.toFixed(2)} ${y.toFixed(2)}`;
    } else {
      d += ` L ${x.toFixed(2)} ${y.toFixed(2)}`;
    }
  }

  // Close the filled area: drop straight to baseline, go back to origin, close.
  const firstX = PAD_X.toFixed(2);
  const lastX  = (PAD_X + PLOT_W).toFixed(2);
  const baseY  = (PAD_Y + PLOT_H).toFixed(2);
  const areaPath = `${d} L ${lastX} ${baseY} L ${firstX} ${baseY} Z`;

  return { path: areaPath, minElev, maxElev };
}

// ---------------------------------------------------------------------------
// Helpers: format metres
// ---------------------------------------------------------------------------

function fmtElev(m: number): string {
  return `${Math.round(m).toLocaleString()} m`;
}

function fmtDelta(m: number): string {
  return `${m < 1 ? '0' : Math.round(m).toLocaleString()} m`;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export default function ElevationProfile({ data, onHoverIndex, onSaveHike }: ElevationProfileProps) {
  const { elevations, totalAscent, totalDescent } = data;
  const svgRef = useRef<SVGSVGElement>(null);

  // ── Save Hike UI state ───────────────────────────────────────────────────
  const [isSaving, setIsSaving]   = useState(false);
  const [title, setTitle]         = useState('');
  const [isSaved, setIsSaved]     = useState(false);
  const [isLoading, setIsLoading] = useState(false);

  // ── Hover: map pointer x → nearest elevation index ───────────────────────
  const handleMouseMove = useCallback(
    (e: React.MouseEvent<SVGSVGElement>) => {
      if (!onHoverIndex || !svgRef.current) return;
      const rect = svgRef.current.getBoundingClientRect();
      const relX = e.clientX - rect.left;
      // Map client pixel → [0, PLOT_W] → index
      const plotFraction = (relX - (rect.width * PAD_X) / VB_W) / (rect.width * PLOT_W / VB_W);
      const clamped = Math.max(0, Math.min(1, plotFraction));
      const idx = Math.round(clamped * (elevations.length - 1));
      onHoverIndex(idx);
    },
    [onHoverIndex, elevations.length],
  );

  const handleMouseLeave = useCallback(() => {
    onHoverIndex?.(null);
  }, [onHoverIndex]);

  // ── Build path ───────────────────────────────────────────────────────────
  const { path, minElev, maxElev } = buildSvgPath(elevations);

  const gradientId  = 'elev-fill-gradient';
  const clipId      = 'elev-clip';

  // ── Ascent / descent colour thresholds ───────────────────────────────────
  const ascentColour  = totalAscent  > 500 ? '#f97316' : '#34d399'; // amber if steep
  const descentColour = totalDescent > 500 ? '#fb923c' : '#94a3b8';

  return (
    <div
      className={[
        // slide-up tray — absolute, bottom of the map container, full width
        'absolute bottom-0 left-0 right-0 z-30',
        'pointer-events-auto',
        // glassmorphism card
        'bg-slate-950/80 backdrop-blur-xl',
        'border-t border-slate-700/50',
        'rounded-b-3xl',
        // subtle entrance animation
        'animate-slide-up',
      ].join(' ')}
    >
      {/* ── Stats bar ────────────────────────────────────────────────────── */}
      <div className="flex items-center justify-between px-5 pt-3 pb-1.5 gap-4">
        {/* Label */}
        <div className="flex items-center gap-2">
          <span className="h-1.5 w-1.5 rounded-full bg-blue-400 animate-pulse" />
          <span className="text-[10px] uppercase font-mono tracking-widest text-slate-400">
            Elevation Profile
          </span>
        </div>

        {/* Stat pills */}
        <div className="flex items-center gap-3 flex-wrap justify-end">
          {/* Range */}
          <div className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-slate-900/70 border border-slate-800">
            <span className="text-[9px] uppercase font-mono text-slate-500 tracking-wide">Range</span>
            <span className="text-xs font-bold font-mono text-slate-200">
              {fmtElev(minElev)} – {fmtElev(maxElev)}
            </span>
          </div>

          {/* Ascent */}
          <div className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-slate-900/70 border border-slate-800">
            <svg className="h-3 w-3" style={{ color: ascentColour }} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M5 10l7-7m0 0l7 7m-7-7v18" />
            </svg>
            <span className="text-xs font-bold font-mono" style={{ color: ascentColour }}>
              {fmtDelta(totalAscent)}
            </span>
          </div>

          {/* Descent */}
          <div className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-slate-900/70 border border-slate-800">
            <svg className="h-3 w-3" style={{ color: descentColour }} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M19 14l-7 7m0 0l-7-7m7 7V3" />
            </svg>
            <span className="text-xs font-bold font-mono" style={{ color: descentColour }}>
              {fmtDelta(totalDescent)}
            </span>
          </div>

          {/* Save Hike action */}
          {onSaveHike && (
            <div className="flex items-center gap-1.5 pl-2 border-l border-slate-800/80">
              {isSaved ? (
                <span className="inline-flex items-center gap-1 px-2.5 py-1 rounded-lg bg-emerald-950/60 border border-emerald-500/30 text-emerald-400 text-xs font-mono">
                  <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
                    <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                  </svg>
                  Saved!
                </span>
              ) : isSaving ? (
                <form
                  onSubmit={async (e) => {
                    e.preventDefault();
                    if (!title.trim()) return;
                    setIsLoading(true);
                    try {
                      await onSaveHike(title.trim());
                      setIsSaved(true);
                      setIsSaving(false);
                      setTimeout(() => setIsSaved(false), 3000);
                    } catch (err) {
                      console.error('Failed to save route:', err);
                    } finally {
                      setIsLoading(false);
                    }
                  }}
                  className="flex items-center gap-1.5"
                >
                  <input
                    type="text"
                    value={title}
                    onChange={(e) => setTitle(e.target.value)}
                    placeholder="Hike title..."
                    disabled={isLoading}
                    autoFocus
                    className="px-2 py-0.5 rounded bg-slate-900 border border-slate-700 text-xs text-slate-200 focus:outline-none focus:border-blue-500 max-w-[120px]"
                  />
                  <button
                    type="submit"
                    disabled={isLoading || !title.trim()}
                    className="p-1 rounded bg-blue-600 hover:bg-blue-500 text-white disabled:opacity-40 cursor-pointer"
                    title="Confirm Save"
                  >
                    <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
                      <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                    </svg>
                  </button>
                  <button
                    type="button"
                    disabled={isLoading}
                    onClick={() => setIsSaving(false)}
                    className="p-1 rounded bg-slate-800 hover:bg-slate-700 text-slate-400 cursor-pointer"
                    title="Cancel"
                  >
                    <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                      <path strokeLinecap="round" strokeLinejoin="round" d="M6 18L18 6M6 6l12 12" />
                    </svg>
                  </button>
                </form>
              ) : (
                <button
                  type="button"
                  onClick={() => {
                    setTitle('');
                    setIsSaving(true);
                  }}
                  className="flex items-center gap-1 px-2.5 py-1 rounded-lg bg-blue-600/80 hover:bg-blue-500 text-white text-xs font-bold font-mono transition-all active:scale-95 cursor-pointer shadow-lg shadow-blue-500/10 border border-blue-500/30"
                >
                  <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                    <path strokeLinecap="round" strokeLinejoin="round" d="M8 7H5a2 2 0 00-2 2v9a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-3m-1 4l-3 3m0 0l-3-3m3 3V4" />
                  </svg>
                  Save Hike
                </button>
              )}
            </div>
          )}
        </div>

      </div>

      {/* ── SVG Chart ────────────────────────────────────────────────────── */}
      <svg
        ref={svgRef}
        viewBox={`0 0 ${VB_W} ${VB_H}`}
        preserveAspectRatio="none"
        className="w-full h-[88px] block cursor-crosshair"
        onMouseMove={handleMouseMove}
        onMouseLeave={handleMouseLeave}
        aria-label="Elevation profile chart"
        role="img"
      >
        <defs>
          {/* Vertical gradient: vibrant blue → transparent */}
          <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%"   stopColor="#3b82f6" stopOpacity="0.55" />
            <stop offset="60%"  stopColor="#1d4ed8" stopOpacity="0.20" />
            <stop offset="100%" stopColor="#1e3a5f" stopOpacity="0.04" />
          </linearGradient>
          {/* Clip path so fill never bleeds outside plot area */}
          <clipPath id={clipId}>
            <rect x={PAD_X} y={PAD_Y} width={PLOT_W} height={PLOT_H} />
          </clipPath>
        </defs>

        {/* Baseline grid lines (3 horizontal bands) */}
        {[0.25, 0.5, 0.75].map((frac) => {
          const y = PAD_Y + frac * PLOT_H;
          return (
            <line
              key={frac}
              x1={PAD_X}
              y1={y}
              x2={PAD_X + PLOT_W}
              y2={y}
              stroke="#1e293b"
              strokeWidth="0.8"
            />
          );
        })}

        {/* Filled area */}
        {path && (
          <path
            d={path}
            fill={`url(#${gradientId})`}
            clipPath={`url(#${clipId})`}
          />
        )}

        {/* Terrain stroke — sharp, high-contrast line on top of fill */}
        {path && (
          <path
            // Stroke path = only the top portion (area path minus the closing lines).
            // Re-derive: strip " L lastX baseY L firstX baseY Z" from the end.
            d={(() => {
              const closingIdx = path.lastIndexOf(' L ', path.lastIndexOf(' L ') - 1);
              return path.slice(0, closingIdx);
            })()}
            fill="none"
            stroke="#60a5fa"         // blue-400 — sharp terrain line
            strokeWidth="2"
            strokeLinejoin="round"
            strokeLinecap="round"
            clipPath={`url(#${clipId})`}
          />
        )}

        {/* Subtle glow duplicate at lower opacity */}
        {path && (
          <path
            d={(() => {
              const closingIdx = path.lastIndexOf(' L ', path.lastIndexOf(' L ') - 1);
              return path.slice(0, closingIdx);
            })()}
            fill="none"
            stroke="#93c5fd"        // blue-300 — glow
            strokeWidth="5"
            strokeLinejoin="round"
            strokeLinecap="round"
            strokeOpacity="0.18"
            clipPath={`url(#${clipId})`}
          />
        )}
      </svg>
    </div>
  );
}
