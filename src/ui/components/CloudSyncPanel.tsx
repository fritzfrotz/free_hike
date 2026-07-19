// SPDX-License-Identifier: Apache-2.0
/**
 * CloudSyncPanel.tsx
 *
 * Glassmorphic two-column Cloud Sync interface panel.
 *
 * Left column:  Provider connect / disconnect controls.
 * Right column: Live sync telemetry metrics + "Sync Data Now" trigger.
 *
 * The component is entirely presentational — all async logic lives in App.tsx.
 * It receives callbacks and read-only state; it never imports provider modules
 * directly.
 */

import type { SyncConnectionStatus, SyncMetadata, SyncProvider } from '../../shared/types';

// ─── Props ────────────────────────────────────────────────────────────────────

interface CloudSyncPanelProps {
  syncProvider:     SyncProvider;
  syncStatus:       SyncConnectionStatus;
  syncMetadata:     SyncMetadata | null;
  syncEmail:        string | null;
  onConnectGoogle:  () => Promise<void>;
  onConnectDropbox: () => Promise<void>;
  onDisconnect:     () => void;
  onSyncNow:        () => Promise<void>;
}

// ─── Small sub-components ─────────────────────────────────────────────────────

function StatusPill({ status }: { status: SyncConnectionStatus }) {
  const cfg: Record<SyncConnectionStatus, { dot: string; label: string; bg: string }> = {
    disconnected: { dot: 'bg-slate-600',              label: 'Disconnected', bg: 'bg-slate-800/60 border-slate-700/40 text-slate-400' },
    connecting:   { dot: 'bg-amber-400 animate-pulse', label: 'Connecting…', bg: 'bg-amber-950/50 border-amber-500/30 text-amber-300' },
    connected:    { dot: 'bg-emerald-400 animate-pulse',label: 'Connected',  bg: 'bg-emerald-950/50 border-emerald-500/30 text-emerald-300' },
    syncing:      { dot: 'bg-teal-400 animate-ping',   label: 'Syncing…',   bg: 'bg-teal-950/50 border-teal-500/30 text-teal-300' },
    error:        { dot: 'bg-rose-500',                label: 'Error',      bg: 'bg-rose-950/50 border-rose-500/30 text-rose-300' },
  };
  const { dot, label, bg } = cfg[status];
  return (
    <span className={`inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full border text-[10px] font-mono uppercase tracking-widest ${bg}`}>
      <span className={`h-1.5 w-1.5 rounded-full ${dot}`} />
      {label}
    </span>
  );
}

function MetricRow({ label, value, accent = false }: { label: string; value: string; accent?: boolean }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-slate-500 text-xs font-mono">{label}</span>
      <span className={`text-xs font-bold font-mono ${accent ? 'text-emerald-400' : 'text-slate-300'}`}>{value}</span>
    </div>
  );
}

function ProviderButton({
  id,
  label,
  sublabel,
  active,
  disabled,
  onClick,
  children,
}: {
  id: string;
  label: string;
  sublabel: string;
  active: boolean;
  disabled: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      id={id}
      onClick={onClick}
      disabled={disabled}
      className={[
        'w-full flex items-center gap-3 px-4 py-3 rounded-xl border text-left',
        'transition-all duration-200 active:scale-[0.98]',
        'disabled:opacity-40 disabled:pointer-events-none focus:outline-none',
        'focus-visible:ring-2 focus-visible:ring-emerald-400',
        active
          ? 'bg-emerald-950/60 border-emerald-500/40 shadow-emerald-500/10 shadow-md cursor-default'
          : 'bg-slate-900/50 border-slate-800/60 hover:bg-slate-800/50 hover:border-slate-700 cursor-pointer',
      ].join(' ')}
    >
      <span className="h-9 w-9 rounded-lg bg-slate-800 flex items-center justify-center flex-shrink-0">
        {children}
      </span>
      <div className="min-w-0 flex-1">
        <p className={`text-sm font-semibold leading-none mb-0.5 ${active ? 'text-emerald-300' : 'text-slate-200'}`}>
          {label}
        </p>
        <p className="text-[10px] font-mono text-slate-500 truncate">{sublabel}</p>
      </div>
      {active && (
        <svg className="h-4 w-4 text-emerald-400 flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2.5}>
          <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
        </svg>
      )}
    </button>
  );
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

function formatTimestamp(iso: string): string {
  try {
    return new Date(iso).toLocaleString(undefined, {
      day:    '2-digit',
      month:  'short',
      year:   'numeric',
      hour:   '2-digit',
      minute: '2-digit',
    });
  } catch {
    return iso;
  }
}

function providerLabel(p: SyncProvider): string {
  if (p === 'google')  return 'Google Drive';
  if (p === 'dropbox') return 'Dropbox';
  return '—';
}

// ─── Main component ───────────────────────────────────────────────────────────

export default function CloudSyncPanel({
  syncProvider,
  syncStatus,
  syncMetadata,
  syncEmail,
  onConnectGoogle,
  onConnectDropbox,
  onDisconnect,
  onSyncNow,
}: CloudSyncPanelProps) {
  const isConnected   = syncStatus === 'connected' || syncStatus === 'syncing';
  const isBusy        = syncStatus === 'connecting' || syncStatus === 'syncing';
  const providerNone  = syncProvider === 'none';

  return (
    <section
      aria-label="Cloud Data Sovereignty"
      className="w-full max-w-6xl mb-8"
    >
      {/* Panel card */}
      <div className="backdrop-blur-md bg-slate-900/40 border border-slate-800 rounded-3xl overflow-hidden shadow-2xl">

        {/* ── Panel header ─────────────────────────────────────────────────── */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-slate-800/70">
          <div className="flex items-center gap-3">
            {/* Cloud icon */}
            <div className="h-9 w-9 rounded-xl bg-gradient-to-tr from-indigo-500 to-violet-400 flex items-center justify-center shadow-lg shadow-indigo-500/20">
              <svg className="h-5 w-5 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                <path strokeLinecap="round" strokeLinejoin="round"
                  d="M3 15a4 4 0 004 4h9a5 5 0 10-.1-9.999 5.002 5.002 0 10-9.78 2.096A4.001 4.001 0 003 15z" />
              </svg>
            </div>
            <div>
              <h2 className="text-sm font-bold text-slate-100 tracking-tight">Cloud Data Sovereignty</h2>
              <p className="text-[10px] font-mono text-slate-500 uppercase tracking-widest">Zero-Server · PKCE OAuth 2.0</p>
            </div>
          </div>
          <StatusPill status={syncStatus} />
        </div>

        {/* ── Two-column body ───────────────────────────────────────────────── */}
        <div className="grid grid-cols-1 md:grid-cols-2 divide-y md:divide-y-0 md:divide-x divide-slate-800/60">

          {/* ── Left: Provider connect controls ───────────────────────────── */}
          <div className="p-6 space-y-4">
            <p className="text-[10px] uppercase font-mono tracking-widest text-slate-500 mb-3">
              Connect Provider
            </p>

            {/* Google Drive button */}
            <ProviderButton
              id="connect-google-btn"
              label="Google Drive"
              sublabel={syncProvider === 'google' && syncEmail ? syncEmail : 'drive.file scope · OAuth 2.0 PKCE'}
              active={syncProvider === 'google' && isConnected}
              disabled={isBusy || (!providerNone && syncProvider !== 'google')}
              onClick={() => {
                if (syncProvider === 'google' && isConnected) return;
                onConnectGoogle().catch(console.error);
              }}
            >
              {/* Google colourful G icon */}
              <svg viewBox="0 0 24 24" className="h-5 w-5" fill="none">
                <path d="M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92c-.26 1.37-1.04 2.53-2.21 3.31v2.77h3.57c2.08-1.92 3.28-4.74 3.28-8.09z" fill="#4285F4"/>
                <path d="M12 23c2.97 0 5.46-.98 7.28-2.66l-3.57-2.77c-.98.66-2.23 1.06-3.71 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84C3.99 20.53 7.7 23 12 23z" fill="#34A853"/>
                <path d="M5.84 14.09c-.22-.66-.35-1.36-.35-2.09s.13-1.43.35-2.09V7.07H2.18C1.43 8.55 1 10.22 1 12s.43 3.45 1.18 4.93l3.66-2.84z" fill="#FBBC05"/>
                <path d="M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15C17.45 2.09 14.97 1 12 1 7.7 1 3.99 3.47 2.18 7.07l3.66 2.84c.87-2.6 3.3-4.53 6.16-4.53z" fill="#EA4335"/>
              </svg>
            </ProviderButton>

            {/* Dropbox button */}
            <ProviderButton
              id="connect-dropbox-btn"
              label="Dropbox"
              sublabel={syncProvider === 'dropbox' && syncEmail ? syncEmail : '/Apps/FreeHike/ · Offline tokens'}
              active={syncProvider === 'dropbox' && isConnected}
              disabled={isBusy || (!providerNone && syncProvider !== 'dropbox')}
              onClick={() => {
                if (syncProvider === 'dropbox' && isConnected) return;
                onConnectDropbox().catch(console.error);
              }}
            >
              {/* Dropbox icon */}
              <svg viewBox="0 0 24 24" className="h-5 w-5" fill="#0061FF">
                <path d="M6 2L0 6l6 4-6 4 6 4 6-4-6-4 6-4L6 2zm12 0l-6 4 6 4-6 4 6 4 6-4-6-4 6-4-6-4zM6 18l6 4 6-4-6-4-6 4z"/>
              </svg>
            </ProviderButton>

            {/* Disconnect control (shown only when connected) */}
            {isConnected && (
              <button
                id="disconnect-sync-btn"
                onClick={onDisconnect}
                disabled={isBusy}
                className="w-full mt-2 flex items-center justify-center gap-2 px-4 py-2.5 rounded-xl
                           border border-rose-500/25 bg-rose-950/30 text-rose-400
                           text-xs font-mono hover:bg-rose-950/50 hover:border-rose-500/40
                           transition-all active:scale-95 disabled:opacity-40 disabled:pointer-events-none
                           cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-rose-400"
              >
                <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                  <path strokeLinecap="round" strokeLinejoin="round"
                    d="M17 16l4-4m0 0l-4-4m4 4H7m6 4v1a3 3 0 01-3 3H6a3 3 0 01-3-3V7a3 3 0 013-3h4a3 3 0 013 3v1" />
                </svg>
                Disconnect {providerLabel(syncProvider)}
              </button>
            )}

            {/* Security note */}
            <div className="mt-4 p-3 rounded-xl bg-slate-950/40 border border-slate-800/50">
              <p className="text-[10px] text-slate-600 font-mono leading-relaxed">
                Tokens are stored in <span className="text-slate-500">localStorage</span> scoped to this origin.
                Your files are written exclusively to <span className="text-slate-500">/Apps/FreeHike/</span> — we never read your other data.
              </p>
            </div>
          </div>

          {/* ── Right: Telemetry + Sync Now ───────────────────────────────── */}
          <div className="p-6 flex flex-col gap-4">
            <p className="text-[10px] uppercase font-mono tracking-widest text-slate-500">
              Sync Telemetry
            </p>

            {isConnected ? (
              <>
                {/* Metrics grid */}
                <div className="bg-slate-950/40 border border-slate-800/60 rounded-xl p-4 space-y-3">
                  <MetricRow
                    label="Provider"
                    value={providerLabel(syncProvider)}
                    accent
                  />
                  <MetricRow
                    label="Account"
                    value={syncEmail ?? '—'}
                  />
                  {syncMetadata ? (
                    <>
                      <div className="border-t border-slate-800/50 my-1" />
                      <MetricRow
                        label="Last Sync"
                        value={formatTimestamp(syncMetadata.lastSynced)}
                      />
                      <MetricRow
                        label="Files Uploaded"
                        value={`${syncMetadata.filesUploaded} file${syncMetadata.filesUploaded !== 1 ? 's' : ''}`}
                      />
                      <MetricRow
                        label="Volume"
                        value={formatBytes(syncMetadata.lastFileSize)}
                        accent
                      />
                    </>
                  ) : (
                    <>
                      <div className="border-t border-slate-800/50 my-1" />
                      <p className="text-[10px] font-mono text-slate-600 text-center py-2">
                        No sync recorded yet — run your first sync below.
                      </p>
                    </>
                  )}
                </div>

                {/* Sync Now button */}
                <button
                  id="sync-now-btn"
                  onClick={() => onSyncNow().catch(console.error)}
                  disabled={syncStatus === 'syncing'}
                  className={[
                    'relative w-full flex items-center justify-center gap-2.5 px-5 py-3.5',
                    'rounded-xl font-semibold text-sm tracking-wide',
                    'border transition-all duration-300 active:scale-[0.98]',
                    'focus:outline-none focus-visible:ring-2 focus-visible:ring-indigo-400',
                    'disabled:opacity-50 disabled:pointer-events-none cursor-pointer',
                    syncStatus === 'syncing'
                      ? 'bg-indigo-950/60 border-indigo-500/40 text-indigo-300'
                      : 'bg-gradient-to-r from-indigo-600/80 to-violet-600/80 border-indigo-500/40 text-white hover:from-indigo-500/90 hover:to-violet-500/90 shadow-lg shadow-indigo-500/15',
                  ].join(' ')}
                >
                  {syncStatus === 'syncing' ? (
                    <>
                      <svg className="h-4 w-4 animate-spin" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                        <path strokeLinecap="round" strokeLinejoin="round" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
                      </svg>
                      Uploading to {providerLabel(syncProvider)}…
                    </>
                  ) : (
                    <>
                      <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                        <path strokeLinecap="round" strokeLinejoin="round" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-8l-4-4m0 0L8 8m4-4v12" />
                      </svg>
                      Sync Data Now
                    </>
                  )}
                </button>

                {/* Payload legend */}
                <div className="grid grid-cols-2 gap-2">
                  {['trails_cache.gpx', 'sync_metadata.json'].map(f => (
                    <div key={f} className="flex items-center gap-1.5 px-3 py-2 rounded-lg bg-slate-900/50 border border-slate-800/50">
                      <svg className="h-3 w-3 text-indigo-400 flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                        <path strokeLinecap="round" strokeLinejoin="round" d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z" />
                      </svg>
                      <span className="text-[10px] font-mono text-slate-500 truncate">{f}</span>
                    </div>
                  ))}
                </div>
              </>
            ) : (
              /* Disconnected placeholder */
              <div className="flex-1 flex flex-col items-center justify-center py-10 space-y-3 text-center">
                <div className="h-14 w-14 rounded-2xl bg-slate-900/60 border border-slate-800 flex items-center justify-center">
                  <svg className="h-7 w-7 text-slate-700" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
                    <path strokeLinecap="round" strokeLinejoin="round"
                      d="M3 15a4 4 0 004 4h9a5 5 0 10-.1-9.999 5.002 5.002 0 10-9.78 2.096A4.001 4.001 0 003 15z" />
                  </svg>
                </div>
                <p className="text-slate-500 text-sm font-semibold">No sync configured</p>
                <p className="text-slate-600 text-xs font-mono leading-relaxed max-w-[220px]">
                  Connect Google Drive or Dropbox on the left to enable automatic zero-server trail backup.
                </p>

                {syncStatus === 'error' && (
                  <span className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-rose-950/50 border border-rose-500/30 text-rose-300 text-xs font-mono">
                    <svg className="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                      <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v2m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                    </svg>
                    Authentication error — please retry
                  </span>
                )}
              </div>
            )}
          </div>

        </div>
      </div>
    </section>
  );
}
