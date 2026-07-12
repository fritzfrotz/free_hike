import { useEffect, useState } from 'react';
import type { SavedRoute } from '../../shared/db';
import { getAllRoutes } from '../../shared/db';

interface SavedRoutesPanelProps {
  isOpen: boolean;
  onClose: () => void;
  onLoadRoute: (route: SavedRoute) => void;
  onDeleteRoute: (id: number) => Promise<void>;
  // We allow trigger refetches by passing a key or refresh trigger
  refreshKey: number;
}

export default function SavedRoutesPanel({
  isOpen,
  onClose,
  onLoadRoute,
  onDeleteRoute,
  refreshKey,
}: SavedRoutesPanelProps) {
  const [routes, setRoutes] = useState<SavedRoute[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!isOpen) return;

    let cancelled = false;

    // Defer the initial setState out of the effect's synchronous body — calling
    // setState directly inside an effect can trigger cascading renders.
    queueMicrotask(() => {
      if (!cancelled) setLoading(true);
    });

    getAllRoutes()
      .then((data) => {
        if (cancelled) return;
        setRoutes(data.sort((a, b) => b.timestamp - a.timestamp));
      })
      .catch((err) => console.error('Failed to get saved routes:', err))
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [isOpen, refreshKey]);

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex justify-end pointer-events-none">
      {/* Backdrop (closes panel when clicked) */}
      <div
        className="absolute inset-0 bg-slate-950/40 backdrop-blur-sm pointer-events-auto transition-opacity"
        onClick={onClose}
      />

      {/* Drawer Container */}
      <div className="relative w-full max-w-md h-full bg-slate-900/95 backdrop-blur-xl border-l border-slate-800 pointer-events-auto shadow-2xl flex flex-col z-10 animate-slide-in">
        {/* Header */}
        <div className="flex items-center justify-between p-6 border-b border-slate-800/80">
          <div className="flex items-center gap-2.5">
            <div className="h-8 w-8 rounded-lg bg-gradient-to-tr from-blue-600 to-indigo-500 flex items-center justify-center shadow-lg shadow-blue-500/10">
              <svg className="h-4.5 w-4.5 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round" d="M9 20l-5.447-2.724A1 1 0 013 16.382V5.618a1 1 0 011.447-.894L9 7m0 13l6-3m-6 3V7m6 10l4.553 2.276A1 1 0 0021 18.382V7.618a1 1 0 00-.553-.894L15 4m0 13V4m0 0L9 7" />
              </svg>
            </div>
            <div>
              <h2 className="text-sm font-bold text-slate-100 tracking-tight">My Hikes</h2>
              <p className="text-[10px] font-mono text-slate-500 uppercase tracking-widest">Saved Routes · Offline Index</p>
            </div>
          </div>

          <button
            onClick={onClose}
            className="p-1.5 rounded-lg border border-slate-800 bg-slate-950/60 hover:bg-slate-800 text-slate-400 hover:text-slate-200 transition-all cursor-pointer"
            title="Close Panel"
          >
            <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M6 18L18 6M6 6l12 12" />
            </svg>
          </button>
        </div>

        {/* Content list */}
        <div className="flex-1 overflow-y-auto p-6 space-y-4">
          {loading ? (
            <div className="h-full flex flex-col items-center justify-center py-12 space-y-3">
              <div className="h-8 w-8 rounded-full border-2 border-blue-500/20 border-t-blue-500 animate-spin" />
              <p className="text-xs font-mono text-slate-500">Loading saved hikes...</p>
            </div>
          ) : routes.length === 0 ? (
            <div className="h-full flex flex-col items-center justify-center text-center py-16 space-y-3">
              <div className="h-12 w-12 rounded-2xl bg-slate-950/40 border border-slate-800/80 flex items-center justify-center text-slate-700">
                <svg className="h-6 w-6" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
                  <path strokeLinecap="round" strokeLinejoin="round" d="M9 20l-5.447-2.724A1 1 0 013 16.382V5.618a1 1 0 011.447-.894L9 7m0 13l6-3m-6 3V7m6 10l4.553 2.276A1 1 0 0021 18.382V7.618a1 1 0 00-.553-.894L15 4m0 13V4m0 0L9 7" />
                </svg>
              </div>
              <p className="text-slate-400 text-xs font-semibold">No saved hikes found</p>
              <p className="text-slate-600 text-[11px] font-mono leading-relaxed max-w-[200px]">
                Create a route on the map and click "Save Hike" inside the elevation profile to store it.
              </p>
            </div>
          ) : (
            routes.map((route) => {
              const dateStr = new Date(route.timestamp).toLocaleDateString(undefined, {
                day: '2-digit',
                month: 'short',
                year: 'numeric',
                hour: '2-digit',
                minute: '2-digit',
              });

              return (
                <div
                  key={route.id}
                  className="p-4 rounded-2xl bg-slate-950/50 border border-slate-800/60 hover:border-slate-700/60 transition-all flex flex-col gap-3"
                >
                  <div className="flex justify-between items-start gap-2">
                    <div className="min-w-0">
                      <h4 className="text-xs font-bold text-slate-200 truncate" title={route.title}>
                        {route.title}
                      </h4>
                      <p className="text-[10px] font-mono text-slate-500 mt-0.5">{dateStr}</p>
                    </div>
                    <div className="flex gap-2">
                      {/* Load button */}
                      <button
                        onClick={() => {
                          onLoadRoute(route);
                          onClose();
                        }}
                        className="px-2.5 py-1 rounded bg-blue-600/20 hover:bg-blue-600 border border-blue-500/30 hover:border-blue-400 text-blue-300 hover:text-white text-[10px] font-bold font-mono transition-all cursor-pointer"
                      >
                        Load
                      </button>
                      {/* Delete button */}
                      <button
                        onClick={async () => {
                          if (route.id !== undefined) {
                            await onDeleteRoute(route.id);
                          }
                        }}
                        className="p-1 rounded bg-rose-950/30 hover:bg-rose-600 border border-rose-500/20 hover:border-rose-400 text-rose-400 hover:text-white transition-all cursor-pointer"
                        title="Delete Route"
                      >
                        <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                          <path strokeLinecap="round" strokeLinejoin="round" d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" />
                        </svg>
                      </button>
                    </div>
                  </div>

                  {/* Tiny summary metrics */}
                  <div className="grid grid-cols-3 gap-2 pt-1 border-t border-slate-900 text-center font-mono text-[9px]">
                    <div>
                      <span className="text-slate-600 block">ASCENT</span>
                      <span className="text-emerald-400 font-bold mt-0.5 block">{Math.round(route.totalAscent)} m</span>
                    </div>
                    <div>
                      <span className="text-slate-600 block">DESCENT</span>
                      <span className="text-slate-400 font-bold mt-0.5 block">{Math.round(route.totalDescent)} m</span>
                    </div>
                    <div>
                      <span className="text-slate-600 block">POINTS</span>
                      <span className="text-blue-400 font-bold mt-0.5 block">{route.elevations.length}</span>
                    </div>
                  </div>
                </div>
              );
            })
          )}
        </div>
      </div>
    </div>
  );
}
