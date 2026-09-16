// SPDX-License-Identifier: Apache-2.0
//! `ffi` — the UniFFI boundary crate (Layer 3 of the tri-layer bridge).
//!
//! **Surface v1 — the production suspendable-state-machine contract.**
//! Everything exported here is consumed by generated Swift/Kotlin bindings
//! and wrapped by the Capacitor `MapCompilerPlugin`. Per the operating
//! manual, any change to this surface is a HITL gate — this revision was
//! operator-directed (see LOOPLOG P2.C0).
//!
//! ## The execution contract
//!
//! `compile_chunk(job, budget_ms, callback)` runs **one slice** of a compile
//! job and always returns within roughly `budget_ms` (plus at most one block
//! of overrun — the minimum-forward-progress guarantee prevents livelock
//! when the budget is smaller than a single unit of work).
//!
//! - `Finished`  → job 100% complete; temporary state purged.
//! - `Yielded`   → budget expired; a durable checkpoint (fsync + atomic
//!   rename) is already on disk. The returned `CheckpointState` is
//!   informational for UI/telemetry — resume happens by calling
//!   `compile_chunk` again with the **same `CompileJob`**; the engine reloads
//!   its own checkpoint. The foreign layer never round-trips state, so it
//!   can neither corrupt it nor lose it when iOS kills the process.
//! - `FailedFatal`     → non-retryable (corrupted checkpoint/index, bad
//!   input, non-clearing I/O like EACCES).
//! - `FailedTransient` → the environment refused the slice (advisory slice
//!   lock held by another runner, ENOSPC, EIO); durable state untouched —
//!   back off and retry.
//!
//! Panic safety: UniFFI's generated scaffolding converts Rust panics into
//! foreign-language errors via unwinding, which is why the workspace release
//! profile does NOT set `panic = "abort"`.

use std::time::Duration;

use compiler::engine::{self, JobSpec, SliceOutcome};
use compiler::{thermal, BBox};
use log::{error, info, warn};

uniffi::setup_scaffolding!("freehike");

/// Binds the `log` facade to logcat (tag "freehike-core") once per process
/// on Android, so every jobId-tagged lifecycle line from the core crates is
/// greppable next to the Kotlin layer's own logs. Called at the entry of
/// every exported function — the .so has no other guaranteed init hook.
/// On non-Android targets this is a no-op: hosts (tests, CLIs) bind their
/// own backend if they want the output.
fn ensure_logging() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        #[cfg(target_os = "android")]
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("freehike-core"),
        );
    });
}

// ---------------------------------------------------------------------------
// Records (plain data across the boundary — no references, no lifetimes)
// ---------------------------------------------------------------------------

/// Description of a compile job. Send the *same* record for every slice of
/// the same job — `job_id` + `output_dir` are the resume identity.
#[derive(Debug, Clone, uniffi::Record)]
pub struct CompileJob {
    /// Caller-chosen unique ID (e.g. a UUID). Checkpoints are keyed by it.
    pub job_id: String,
    /// "west,south,east,north" in WGS84 degrees (validated on every call).
    pub bbox: String,
    /// Minimum zoom level to generate (inclusive).
    pub min_zoom: u8,
    /// Maximum zoom level to generate (inclusive).
    pub max_zoom: u8,
    /// Absolute path to the raw .osm.pbf extract on device storage.
    pub pbf_path: String,
    /// Absolute path to the DEM GeoTIFF; None skips the Terrain phase.
    pub dem_path: Option<String>,
    /// Directory owning this job's checkpoints and output archives.
    pub output_dir: String,
}

/// Where a yielded job stopped. Informational: display it, log it, but never
/// feed it back — the engine owns the durable copy.
#[derive(Debug, Clone, uniffi::Record)]
pub struct CheckpointState {
    pub job_id: String,
    /// Processing phase the job will resume in.
    pub phase: CompilePhase,
    /// Next block index within the phase.
    pub next_block: u32,
    /// Byte offset into the source PBF — the real Pass 1's exact mmap
    /// re-entry point (block-boundary aligned).
    pub pbf_byte_offset: u64,
    /// Total bytes appended to output archives so far.
    pub bytes_written: u64,
}

/// Completion report for a finished job.
#[derive(Debug, Clone, uniffi::Record)]
pub struct CompileSummary {
    pub job_id: String,
    pub blocks_total: u32,
    pub bytes_written: u64,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Compilation phases, in execution order.
///
/// P4.C2 surface note: `Pass3Tiles` was appended when the real tile-binning
/// pass landed — a Surface v1 addition made under the operator's Phase-4
/// integration directive (adds a Swift/Kotlin enum case; existing cases and
/// their ordinals are unchanged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CompilePhase {
    Pass1Nodes,
    Pass2Ways,
    Pass3Tiles,
    Terrain,
    Finalize,
}

impl From<engine::Phase> for CompilePhase {
    fn from(p: engine::Phase) -> Self {
        match p {
            engine::Phase::Pass1Nodes => CompilePhase::Pass1Nodes,
            engine::Phase::Pass2Ways => CompilePhase::Pass2Ways,
            engine::Phase::Pass3Tiles => CompilePhase::Pass3Tiles,
            engine::Phase::Terrain => CompilePhase::Terrain,
            engine::Phase::Finalize => CompilePhase::Finalize,
        }
    }
}

/// Result of one execution slice.
///
/// Surface v1 revision (operator-directed hardening pass): the former
/// `Failed` case is split by retryability so the shells can route failures
/// to the right WorkManager/BGTask policy instead of guessing from strings.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum CompilationStatus {
    /// Compilation for the region is 100% complete; temporary caches purged.
    Finished { summary: CompileSummary },
    /// The time budget expired; durable checkpoint written. Re-invoke
    /// `compile_chunk` with the same CompileJob to resume.
    Yielded { checkpoint: CheckpointState },
    /// A fatal error occurred (corrupted checkpoint, corrupted index, bad
    /// input, non-clearing I/O like EACCES). Runners must NOT retry — the
    /// same inputs fail the same way.
    FailedFatal { reason: String },
    /// The environment refused this slice: another runner holds the job's
    /// slice lock, or an I/O operation hit a condition that can clear
    /// (ENOSPC — disk full; EIO — transient device error). Durable state
    /// is untouched; back off and retry later.
    FailedTransient { reason: String },
}

/// Device thermal pressure, reported by the native shells so the compiler
/// can throttle itself before the OS terminates the process (P8.C1).
///
/// Suggested platform mapping (the shells own this; Rust never polls):
/// - iOS `ProcessInfo.ThermalState`: `.nominal`/`.fair`/`.serious`/
///   `.critical` map 1:1.
/// - Android `PowerManager` thermal status: `NONE` → Nominal, `LIGHT` →
///   Fair, `MODERATE` → Serious, `SEVERE` and above → Critical.
///
/// Effect inside the compiler: Nominal/Fair run at full duty cycle (Fair
/// additionally halves parallel-section width); Serious halves the honored
/// slice budget and injects cooling pauses between blocks; Critical makes
/// the very next block boundary checkpoint and return `Yielded`, so the
/// runner can go idle until the OS reports recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ThermalState {
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl From<ThermalState> for thermal::ThermalState {
    fn from(s: ThermalState) -> Self {
        match s {
            ThermalState::Nominal => thermal::ThermalState::Nominal,
            ThermalState::Fair => thermal::ThermalState::Fair,
            ThermalState::Serious => thermal::ThermalState::Serious,
            ThermalState::Critical => thermal::ThermalState::Critical,
        }
    }
}

impl From<thermal::ThermalState> for ThermalState {
    fn from(s: thermal::ThermalState) -> Self {
        match s {
            thermal::ThermalState::Nominal => ThermalState::Nominal,
            thermal::ThermalState::Fair => ThermalState::Fair,
            thermal::ThermalState::Serious => ThermalState::Serious,
            thermal::ThermalState::Critical => ThermalState::Critical,
        }
    }
}

// ---------------------------------------------------------------------------
// Callback interface (implemented on the foreign side)
// ---------------------------------------------------------------------------

/// Progress events emitted from the Rust core to the native (Swift/Kotlin)
/// layer, forwarded to the WebView as Capacitor `compilationProgress` events.
#[uniffi::export(callback_interface)]
pub trait ProgressCallback: Send + Sync {
    /// `percentage` is 0.0-100.0 across the whole job (not the slice);
    /// `status` is a human-readable phase label, e.g.
    /// "pass1: indexing nodes (12/62)".
    fn on_progress(&self, percentage: f32, status: String);
}

// ---------------------------------------------------------------------------
// Core interface
// ---------------------------------------------------------------------------

fn to_job_spec(job: &CompileJob) -> Result<JobSpec, String> {
    let bbox = BBox::parse(&job.bbox).map_err(|e| format!("invalid bbox: {e}"))?;
    // job_id names on-disk files (checkpoint/index/archive) via
    // `output_dir.join(format!("{job_id}.pmtiles"))` in the engine. A `/`,
    // `..`, or absolute path there would traverse out of the sandbox (or,
    // with a leading `/`, replace output_dir entirely). This is the single
    // choke point every platform and both the foreground and background
    // paths cross, so the filesystem-safe-charset invariant is enforced here.
    // Validate the raw value that becomes the path component (not a trimmed
    // copy): the charset below already forbids whitespace, so leading/trailing
    // spaces are rejected rather than silently smuggled into the filename.
    let id = &job.job_id;
    if id.is_empty() {
        return Err("job_id must not be empty".to_string());
    }
    if id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(format!(
            "invalid job_id {id:?}: only [A-Za-z0-9_-] allowed, max 128 chars"
        ));
    }
    if job.min_zoom > job.max_zoom {
        return Err(format!(
            "invalid zoom range: min_zoom {} > max_zoom {}",
            job.min_zoom, job.max_zoom
        ));
    }
    Ok(JobSpec {
        job_id: job.job_id.clone(),
        bbox,
        min_zoom: job.min_zoom,
        max_zoom: job.max_zoom,
        pbf_path: job.pbf_path.clone(),
        dem_path: job.dem_path.clone(),
        output_dir: job.output_dir.clone(),
    })
}

fn to_status(job_id: &str, outcome: SliceOutcome) -> CompilationStatus {
    match outcome {
        SliceOutcome::Finished(s) => {
            info!(
                "FFI compile_chunk({job_id}) -> Finished ({} blocks, {} bytes)",
                s.blocks_total, s.bytes_written
            );
            CompilationStatus::Finished {
                summary: CompileSummary {
                    job_id: s.job_id,
                    blocks_total: s.blocks_total,
                    bytes_written: s.bytes_written,
                },
            }
        }
        SliceOutcome::Yielded(cp) => {
            info!(
                "FFI compile_chunk({job_id}) -> Yielded (phase={}, next_block={}, bytes_written={})",
                cp.phase, cp.next_block, cp.bytes_written
            );
            CompilationStatus::Yielded {
                checkpoint: CheckpointState {
                    job_id: cp.job_id,
                    phase: cp.phase.into(),
                    next_block: cp.next_block,
                    pbf_byte_offset: cp.pbf_byte_offset,
                    bytes_written: cp.bytes_written,
                },
            }
        }
        SliceOutcome::FailedFatal(reason) => {
            error!("FFI compile_chunk({job_id}) -> FailedFatal: {reason}");
            CompilationStatus::FailedFatal { reason }
        }
        SliceOutcome::FailedTransient(reason) => {
            warn!("FFI compile_chunk({job_id}) -> FailedTransient: {reason}");
            CompilationStatus::FailedTransient { reason }
        }
    }
}

/// Runs one budget-bounded slice of `job`. See module docs for the
/// Finished / Yielded / FailedFatal / FailedTransient contract. Never
/// throws: all failures are values, so foreign call sites need no
/// try/catch ceremony.
#[uniffi::export]
pub fn compile_chunk(
    job: CompileJob,
    budget_ms: u32,
    callback: Box<dyn ProgressCallback>,
) -> CompilationStatus {
    ensure_logging();
    info!(
        "FFI compile_chunk({}) entered (budget_ms={budget_ms})",
        job.job_id
    );
    let spec = match to_job_spec(&job) {
        Ok(s) => s,
        Err(reason) => {
            error!(
                "FFI compile_chunk({}) rejected at spec validation: {reason}",
                job.job_id
            );
            return CompilationStatus::FailedFatal { reason };
        }
    };
    let budget = Duration::from_millis(u64::from(budget_ms));
    let mut on_progress = |pct: f32, status: String| callback.on_progress(pct, status);
    to_status(
        &job.job_id,
        engine::run_slice(&spec, budget, &mut on_progress),
    )
}

/// Cold-start resume detection: returns the durable checkpoint for a job if
/// one exists (e.g. after the OS killed the process mid-compilation), None
/// if the job has no saved state, or Failed-equivalent None on unreadable
/// state (the next compile_chunk call surfaces the precise error).
#[uniffi::export]
pub fn query_checkpoint(job_id: String, output_dir: String) -> Option<CheckpointState> {
    ensure_logging();
    match engine::load_checkpoint(&output_dir, &job_id) {
        Ok(Some(cp)) => {
            info!(
                "FFI query_checkpoint({job_id}) -> found (phase={}, next_block={})",
                cp.phase, cp.next_block
            );
            Some(CheckpointState {
                job_id: cp.job_id,
                phase: cp.phase.into(),
                next_block: cp.next_block,
                pbf_byte_offset: cp.pbf_byte_offset,
                bytes_written: cp.bytes_written,
            })
        }
        Ok(None) => {
            info!("FFI query_checkpoint({job_id}) -> none (fresh start)");
            None
        }
        Err(e) => {
            warn!("FFI query_checkpoint({job_id}) -> unreadable state ({e}); reporting none");
            None
        }
    }
}

/// Cancels a job between slices by deleting its durable state. Returns true
/// if state existed and was removed. (In-slice cancellation is not needed:
/// slices are budget-bounded, so the runner simply stops re-invoking.)
#[uniffi::export]
pub fn purge_job(job_id: String, output_dir: String) -> bool {
    ensure_logging();
    info!("FFI purge_job({job_id}) requested");
    engine::purge_job_state(&output_dir, &job_id)
}

/// Publishes the OS-reported thermal level to the compiler core. Callable
/// from ANY foreign thread at any time — including while `compile_chunk`
/// is running on another thread; the write is a single atomic store and
/// running loops pick it up at their next block boundary. The shells
/// should call this from their thermal-notification observers
/// (`thermalStateDidChangeNotification` / `OnThermalStatusChangedListener`)
/// and once at scheduler-window start (notifications don't fire for a
/// state that was already elevated when the process woke).
#[uniffi::export]
pub fn set_thermal_state(state: ThermalState) {
    ensure_logging();
    info!("FFI set_thermal_state({state:?})");
    thermal::set_state(state.into());
}

/// The thermal level the compiler is currently governed by (Nominal until
/// a shell reports otherwise). For smoke tests and telemetry/UI.
#[uniffi::export]
pub fn thermal_state() -> ThermalState {
    thermal::current().into()
}

/// Version string for plugin smoke tests ("is the Rust core actually loaded?").
#[uniffi::export]
pub fn engine_version() -> String {
    format!("freehike-core {}", env!("CARGO_PKG_VERSION"))
}

/// Debug walking-skeleton retained from Phase 1: emits `steps` synthetic
/// progress ticks through the callback and returns how many were sent.
#[uniffi::export]
pub fn emit_test_progress(callback: Box<dyn ProgressCallback>, steps: u32) -> u32 {
    if steps == 0 {
        return 0;
    }
    for i in 1..=steps {
        let percentage = (i as f32 / steps as f32) * 100.0;
        callback.on_progress(percentage, format!("walking-skeleton step {i}/{steps}"));
    }
    steps
}

// ---------------------------------------------------------------------------
// Raw-input fetching (P-SOV.C2a) — Surface v1 addition, HITL-reviewed
// ---------------------------------------------------------------------------
//
// `fetch_chunk(source_id, dest_dir, budget_ms, callback)` is the download
// twin of `compile_chunk`: one budget-bounded slice, durable state on disk
// only (a sidecar + the data file's own length), resume by re-invoking with
// the same arguments. The foreign layer never names a host — it passes a
// source id from `list_sources()`; the URL table lives in the fetcher crate
// (ARCHITECTURE.md P10a keeps absolute URLs out of the WebView).

/// Payload family of a source, mirrored from the fetcher crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FetchKind {
    OsmPbf,
    Tiff,
}

impl From<fetcher::PayloadKind> for FetchKind {
    fn from(k: fetcher::PayloadKind) -> Self {
        match k {
            fetcher::PayloadKind::OsmPbf => FetchKind::OsmPbf,
            fetcher::PayloadKind::Tiff => FetchKind::Tiff,
        }
    }
}

/// One fetchable raw-extract origin (display data for the UI; the id is
/// what goes back into `fetch_chunk`).
#[derive(Debug, Clone, uniffi::Record)]
pub struct FetchSource {
    pub id: String,
    pub label: String,
    /// Entry URL, for display/provenance only — never fetched by the WebView.
    pub url: String,
    pub kind: FetchKind,
}

/// Durable fetch state for a source in a destination directory.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FetchState {
    pub source_id: String,
    /// Final URL after redirect resolution; empty until resolved.
    pub pinned_url: String,
    /// Absolute path of the data file (`<dest_dir>/<pinned basename>`);
    /// empty until resolved. This is the `pbf_path` a `CompileJob` takes.
    pub path: String,
    pub bytes_have: u64,
    pub bytes_total: u64,
    /// True only after magic-byte + MD5 verification: the enqueue gate.
    pub verified: bool,
    /// Restart-clean events so far (entity rotated, Range ignored…).
    pub restarts: u32,
}

/// Result of one fetch slice.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FetchStatus {
    /// Complete and verified; `state.path` is safe to compile from.
    Finished { state: FetchState },
    /// Budget expired; `state.bytes_have` bytes are durable. Re-invoke.
    Yielded { state: FetchState },
    /// Non-retryable (bad payload, checksum mismatch, refused URL, restart
    /// cap, unreadable sidecar). `purge_fetch` clears the state.
    FailedFatal { reason: String },
    /// Network/disk refused the slice; durable state untouched. Retry later.
    FailedTransient { reason: String },
}

/// The source table, in declaration order.
#[uniffi::export]
pub fn list_sources() -> Vec<FetchSource> {
    ensure_logging();
    fetcher::sources::sources()
        .into_iter()
        .map(|s| FetchSource {
            id: s.id,
            label: s.label,
            url: s.url,
            kind: s.kind.into(),
        })
        .collect()
}

fn to_fetch_state(s: fetcher::FetchState) -> FetchState {
    FetchState {
        source_id: s.source_id,
        pinned_url: s.pinned_url,
        path: s.path.to_string_lossy().into_owned(),
        bytes_have: s.have,
        bytes_total: s.total,
        verified: s.verified,
        restarts: s.restarts,
    }
}

/// The full sidecar view after a slice. The engine wrote the sidecar an
/// instant ago, so failing to read it back means the durable state is
/// inconsistent — reported as an error, never fabricated (same rule as
/// `query_checkpoint`: unreadable state is not guessed at).
fn state_after(source_id: &str, dest_dir: &str) -> Result<FetchState, String> {
    match fetcher::query_fetch(source_id, std::path::Path::new(dest_dir)) {
        Ok(Some(s)) => Ok(to_fetch_state(s)),
        Ok(None) => Err(format!(
            "fetch sidecar for {source_id} vanished after the slice"
        )),
        Err(e) => Err(format!(
            "fetch sidecar for {source_id} unreadable after the slice: {e}"
        )),
    }
}

/// Runs one fetch slice for an already-resolved `Source`. Plain (non-
/// exported) entry point so tests can point the engine at a loopback
/// mirror; `fetch_chunk` is the exported wrapper that adds the id lookup.
pub fn fetch_chunk_for_source(
    source: &fetcher::sources::Source,
    dest_dir: &str,
    budget_ms: u32,
    callback: &dyn ProgressCallback,
) -> FetchStatus {
    if dest_dir.is_empty() {
        return FetchStatus::FailedFatal {
            reason: "dest_dir must not be empty".to_string(),
        };
    }
    let budget = Duration::from_millis(u64::from(budget_ms));
    let mut on_progress = |pct: f32, status: String| callback.on_progress(pct, status);
    let outcome = fetcher::fetch_slice_blocking(
        source,
        std::path::Path::new(dest_dir),
        budget,
        &mut on_progress,
    );
    match outcome {
        fetcher::FetchOutcome::Finished { path, bytes } => {
            info!(
                "FFI fetch_chunk({}) -> Finished ({} bytes at {})",
                source.id,
                bytes,
                path.display()
            );
            match state_after(&source.id, dest_dir) {
                Ok(state) => FetchStatus::Finished { state },
                Err(reason) => {
                    error!("FFI fetch_chunk({}) -> FailedFatal: {reason}", source.id);
                    FetchStatus::FailedFatal { reason }
                }
            }
        }
        fetcher::FetchOutcome::Yielded { have, total, .. } => {
            info!(
                "FFI fetch_chunk({}) -> Yielded ({have}/{total} bytes)",
                source.id
            );
            match state_after(&source.id, dest_dir) {
                Ok(state) => FetchStatus::Yielded { state },
                Err(reason) => {
                    error!("FFI fetch_chunk({}) -> FailedFatal: {reason}", source.id);
                    FetchStatus::FailedFatal { reason }
                }
            }
        }
        fetcher::FetchOutcome::FailedFatal(reason) => {
            error!("FFI fetch_chunk({}) -> FailedFatal: {reason}", source.id);
            FetchStatus::FailedFatal { reason }
        }
        fetcher::FetchOutcome::FailedTransient(reason) => {
            warn!(
                "FFI fetch_chunk({}) -> FailedTransient: {reason}",
                source.id
            );
            FetchStatus::FailedTransient { reason }
        }
    }
}

/// Runs one budget-bounded fetch slice of `source_id` into `dest_dir`.
/// Never throws: all failures are values. Resume by calling again with the
/// same arguments; the engine reloads its own sidecar and the data file's
/// length. See `FetchStatus`.
#[uniffi::export]
pub fn fetch_chunk(
    source_id: String,
    dest_dir: String,
    budget_ms: u32,
    callback: Box<dyn ProgressCallback>,
) -> FetchStatus {
    ensure_logging();
    info!("FFI fetch_chunk({source_id}) entered (budget_ms={budget_ms})");
    let Some(source) = fetcher::sources::find_source(&source_id) else {
        error!("FFI fetch_chunk({source_id}) rejected: unknown source");
        return FetchStatus::FailedFatal {
            reason: format!("unknown source id {source_id:?}"),
        };
    };
    fetch_chunk_for_source(&source, &dest_dir, budget_ms, callback.as_ref())
}

/// Durable fetch state for `source_id` in `dest_dir`, or None if nothing
/// was ever resolved there. `verified == true` is the compile-enqueue gate.
/// Unreadable state reports None (the next `fetch_chunk` surfaces the
/// precise error), mirroring `query_checkpoint`.
#[uniffi::export]
pub fn query_fetch(source_id: String, dest_dir: String) -> Option<FetchState> {
    ensure_logging();
    match fetcher::query_fetch(&source_id, std::path::Path::new(&dest_dir)) {
        Ok(Some(s)) => {
            info!(
                "FFI query_fetch({source_id}) -> found ({}/{} bytes, verified={})",
                s.have, s.total, s.verified
            );
            Some(to_fetch_state(s))
        }
        Ok(None) => {
            info!("FFI query_fetch({source_id}) -> none");
            None
        }
        Err(e) => {
            warn!("FFI query_fetch({source_id}) -> unreadable state ({e}); reporting none");
            None
        }
    }
}

/// Removes the sidecar and the data file (partial or complete) for
/// `source_id` in `dest_dir`. Returns true if anything existed.
#[uniffi::export]
pub fn purge_fetch(source_id: String, dest_dir: String) -> bool {
    ensure_logging();
    info!("FFI purge_fetch({source_id}) requested");
    fetcher::purge_fetch(&source_id, std::path::Path::new(&dest_dir))
}

#[cfg(test)]
mod fetch_tests {
    use super::*;
    use fetcher::sources::{Md5Spec, Source};
    use fetcher::testing::{md5_hex, range_response, LoopbackServer, Request, Response};
    use std::sync::{Arc, Mutex};

    struct Sink(Arc<Mutex<Vec<(f32, String)>>>);
    impl ProgressCallback for Sink {
        fn on_progress(&self, percentage: f32, status: String) {
            self.0.lock().unwrap().push((percentage, status));
        }
    }

    const PBF_HEAD: &[u8] = &[
        0x00, 0x00, 0x00, 0x0d, 0x0a, 0x09, b'O', b'S', b'M', b'H', b'e', b'a', b'd', b'e', b'r',
        0x18, 0x4b, 0x10, 0x3f, 0x1a,
    ];

    fn body(len: usize) -> Vec<u8> {
        let mut out = PBF_HEAD.to_vec();
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        while out.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.push((x & 0xff) as u8);
        }
        out
    }

    fn mirror(data: Vec<u8>) -> LoopbackServer {
        LoopbackServer::start(move |req: &Request| {
            if req.path == "/pt-latest.osm.pbf" {
                Response::redirect("/pt-260910.osm.pbf")
            } else if req.path == "/pt-260910.osm.pbf" {
                range_response(&data, req, "\"ffi-etag\"")
            } else if req.path == "/pt-260910.osm.pbf.md5" {
                Response::ok(
                    format!("{}  pt-260910.osm.pbf\n", md5_hex(&data)).into_bytes(),
                    "text/plain",
                )
            } else {
                Response::not_found()
            }
        })
    }

    fn scratch(tag: &str) -> String {
        let dir =
            std::env::temp_dir().join(format!("freehike-ffi-fetch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.to_string_lossy().into_owned()
    }

    fn loopback_source(server: &LoopbackServer) -> Source {
        Source {
            id: "loopback-pt".into(),
            label: "loopback".into(),
            url: server.url("/pt-latest.osm.pbf"),
            kind: fetcher::PayloadKind::OsmPbf,
            md5: Md5Spec::Derived,
        }
    }

    #[test]
    fn fetch_chunk_rejects_unknown_source() {
        let sink = Sink(Default::default());
        match fetch_chunk(
            "no-such-source".into(),
            scratch("unknown"),
            1_000,
            Box::new(sink),
        ) {
            FetchStatus::FailedFatal { reason } => {
                assert!(reason.contains("unknown source"), "got: {reason}")
            }
            other => panic!("expected FailedFatal, got {other:?}"),
        }
    }

    #[test]
    fn fetch_chunk_rejects_empty_dest_dir() {
        let sink = Sink(Default::default());
        match fetch_chunk(
            "geofabrik-portugal".into(),
            String::new(),
            1_000,
            Box::new(sink),
        ) {
            FetchStatus::FailedFatal { reason } => {
                assert!(reason.contains("dest_dir"), "got: {reason}")
            }
            other => panic!("expected FailedFatal, got {other:?}"),
        }
    }

    #[test]
    fn list_sources_matches_table() {
        let ffi = list_sources();
        let table = fetcher::sources::sources();
        assert_eq!(ffi.len(), table.len());
        for (a, b) in ffi.iter().zip(table.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.url, b.url);
            assert_eq!(a.kind, b.kind.into());
        }
        assert!(ffi.iter().any(|s| s.id == "geofabrik-portugal"));
        assert!(ffi.iter().any(|s| s.id == "osmfr-lisbon"));
    }

    #[test]
    fn fetch_chunk_finishes_via_loopback() {
        let data = body(250_000);
        let server = mirror(data.clone());
        let src = loopback_source(&server);
        let dir = scratch("finish");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Sink(Arc::clone(&seen));

        let mut status = None;
        for _ in 0..5 {
            match fetch_chunk_for_source(&src, &dir, 10_000, &sink) {
                FetchStatus::Yielded { .. } => continue,
                terminal => {
                    status = Some(terminal);
                    break;
                }
            }
        }
        let Some(FetchStatus::Finished { state }) = status else {
            panic!("expected Finished, got {status:?}")
        };
        assert!(state.verified);
        assert_eq!(state.bytes_have, data.len() as u64);
        assert_eq!(state.bytes_total, data.len() as u64);
        assert!(state.pinned_url.ends_with("/pt-260910.osm.pbf"));
        assert!(state.path.ends_with("/pt-260910.osm.pbf"));
        assert_eq!(std::fs::read(&state.path).unwrap(), data);
        let seen = seen.lock().unwrap();
        assert!(seen.iter().any(|(_, s)| s.starts_with("fetch: resolved")));
        assert!(seen.iter().any(|(_, s)| s.starts_with("fetch: verified")));
    }

    #[test]
    fn query_and_purge_fetch_roundtrip() {
        let data = body(120_000);
        let server = mirror(data);
        let src = loopback_source(&server);
        let dir = scratch("roundtrip");

        assert!(query_fetch(src.id.clone(), dir.clone()).is_none());
        let sink = Sink(Default::default());
        let mut done = false;
        for _ in 0..5 {
            if let FetchStatus::Finished { .. } = fetch_chunk_for_source(&src, &dir, 10_000, &sink)
            {
                done = true;
                break;
            }
        }
        assert!(done);

        let q = query_fetch(src.id.clone(), dir.clone()).expect("state after finish");
        assert!(q.verified);
        assert_eq!(q.restarts, 0);
        assert!(std::path::Path::new(&q.path).is_file());

        assert!(purge_fetch(src.id.clone(), dir.clone()));
        assert!(query_fetch(src.id.clone(), dir.clone()).is_none());
        assert!(!std::path::Path::new(&q.path).exists());
        assert!(!purge_fetch(src.id, dir));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Recorder(Arc<Mutex<Vec<(f32, String)>>>);
    impl ProgressCallback for Recorder {
        fn on_progress(&self, percentage: f32, status: String) {
            self.0.lock().unwrap().push((percentage, status));
        }
    }

    fn test_job(tag: &str) -> CompileJob {
        let dir =
            std::env::temp_dir().join(format!("freehike-ffi-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Real (synthetic) PBF: the integrated Pass 1 mmaps and decodes it.
        let pbf_path = dir.join("fixture.osm.pbf");
        std::fs::write(
            &pbf_path,
            pbf::fixtures::synthetic_pbf(&[&[
                (1, 472_700_000, 113_900_000),
                (2, 472_700_100, 113_900_050),
            ]]),
        )
        .unwrap();
        CompileJob {
            job_id: format!("job-{tag}"),
            bbox: "11.15,47.05,11.65,47.45".into(),
            min_zoom: 5,
            max_zoom: 14,
            pbf_path: pbf_path.to_string_lossy().into_owned(),
            dem_path: Some("unused_dem.tif".into()),
            output_dir: dir.to_string_lossy().into_owned(),
        }
    }

    #[test]
    fn compile_chunk_finishes_with_large_budget() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let status = compile_chunk(
            test_job("finish"),
            300_000,
            Box::new(Recorder(Arc::clone(&seen))),
        );
        match status {
            CompilationStatus::Finished { summary } => {
                // Fixture: 2 blocks (header + 1 node data) walked by each
                // real pass + simulated terrain (12) + real finalize on a
                // nodes-only extract (0 ways → 0 tiles + 1 assembly block).
                assert_eq!(summary.blocks_total, 2 * 2 + 12 + 1);
                assert!(summary.bytes_written > 0);
            }
            other => panic!("expected Finished, got {other:?}"),
        }
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty());
        assert!((seen.last().unwrap().0 - 100.0).abs() < 0.01);
    }

    #[test]
    fn compile_chunk_yields_with_tiny_budget() {
        let job = test_job("yield");
        let status = compile_chunk(job.clone(), 4, Box::new(Recorder(Default::default())));
        match status {
            CompilationStatus::Yielded { checkpoint } => {
                // The tiny fixture's real passes complete within the budget,
                // so the yield may land anywhere before Finalize completes —
                // any phase is a legitimate suspend point; what matters is
                // that a durable checkpoint exists for this job.
                assert_eq!(checkpoint.job_id, job.job_id);
            }
            other => panic!("expected Yielded, got {other:?}"),
        }
    }

    #[test]
    fn yielded_checkpoint_round_trips_via_query() {
        let job = test_job("query");
        let CompilationStatus::Yielded { checkpoint } =
            compile_chunk(job.clone(), 4, Box::new(Recorder(Default::default())))
        else {
            panic!("expected Yielded");
        };
        let queried = query_checkpoint(job.job_id.clone(), job.output_dir.clone())
            .expect("durable checkpoint must be queryable");
        assert_eq!(queried.next_block, checkpoint.next_block);
        assert_eq!(queried.bytes_written, checkpoint.bytes_written);

        // purge = cancel between slices
        assert!(purge_job(job.job_id.clone(), job.output_dir.clone()));
        assert!(query_checkpoint(job.job_id, job.output_dir).is_none());
    }

    #[test]
    fn failed_on_garbage_bbox() {
        let mut job = test_job("badbbox");
        job.bbox = "the alps".into();
        match compile_chunk(job, 300_000, Box::new(Recorder(Default::default()))) {
            CompilationStatus::FailedFatal { reason } => {
                assert!(reason.contains("invalid bbox"), "got: {reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn failed_on_traversal_job_id() {
        // A job_id that would traverse out of output_dir (or, with a leading
        // '/', replace it) must be rejected before any path is built.
        for evil in [
            "../../../../etc/passwd",
            "/tmp/evil",
            "a/b",
            "sub\\dir",
            "has space",
            "",
            "   ",
        ] {
            let mut job = test_job("traversal");
            job.job_id = evil.to_string();
            match compile_chunk(job, 300_000, Box::new(Recorder(Default::default()))) {
                CompilationStatus::FailedFatal { reason } => {
                    assert!(
                        reason.contains("job_id"),
                        "job_id {evil:?} rejected for the wrong reason: {reason}"
                    )
                }
                other => panic!("job_id {evil:?} must be rejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn failed_on_inverted_zoom_range() {
        let mut job = test_job("badzoom");
        job.min_zoom = 15;
        job.max_zoom = 5;
        match compile_chunk(job, 300_000, Box::new(Recorder(Default::default()))) {
            CompilationStatus::FailedFatal { reason } => {
                assert!(reason.contains("zoom"), "got: {reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn callback_receives_phase_labels() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        compile_chunk(
            test_job("labels"),
            300_000,
            Box::new(Recorder(Arc::clone(&seen))),
        );
        let seen = seen.lock().unwrap();
        assert!(seen.iter().any(|(_, s)| s.starts_with("pass1")));
        assert!(seen.iter().any(|(_, s)| s.starts_with("pass2")));
        assert!(seen.iter().any(|(_, s)| s.starts_with("terrain")));
    }

    #[test]
    fn zero_steps_emits_nothing() {
        struct Panicker;
        impl ProgressCallback for Panicker {
            fn on_progress(&self, _p: f32, _s: String) {
                panic!("must not be called for steps=0");
            }
        }
        assert_eq!(emit_test_progress(Box::new(Panicker), 0), 0);
    }
}
