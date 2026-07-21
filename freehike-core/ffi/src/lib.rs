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
