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
//! - `Failed`    → fatal (corrupted checkpoint, bad input, disk error).
//!
//! Panic safety: UniFFI's generated scaffolding converts Rust panics into
//! foreign-language errors via unwinding, which is why the workspace release
//! profile does NOT set `panic = "abort"`.

use std::time::Duration;

use compiler::engine::{self, JobSpec, SliceOutcome};
use compiler::BBox;

uniffi::setup_scaffolding!("freehike");

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CompilePhase {
    Pass1Nodes,
    Pass2Ways,
    Terrain,
    Finalize,
}

impl From<engine::Phase> for CompilePhase {
    fn from(p: engine::Phase) -> Self {
        match p {
            engine::Phase::Pass1Nodes => CompilePhase::Pass1Nodes,
            engine::Phase::Pass2Ways => CompilePhase::Pass2Ways,
            engine::Phase::Terrain => CompilePhase::Terrain,
            engine::Phase::Finalize => CompilePhase::Finalize,
        }
    }
}

/// Result of one execution slice.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum CompilationStatus {
    /// Compilation for the region is 100% complete; temporary caches purged.
    Finished { summary: CompileSummary },
    /// The time budget expired; durable checkpoint written. Re-invoke
    /// `compile_chunk` with the same CompileJob to resume.
    Yielded { checkpoint: CheckpointState },
    /// A fatal error occurred (e.g. disk full, corrupted payload).
    Failed { reason: String },
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
    if job.job_id.trim().is_empty() {
        return Err("job_id must not be empty".to_string());
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

fn to_status(outcome: SliceOutcome) -> CompilationStatus {
    match outcome {
        SliceOutcome::Finished(s) => CompilationStatus::Finished {
            summary: CompileSummary {
                job_id: s.job_id,
                blocks_total: s.blocks_total,
                bytes_written: s.bytes_written,
            },
        },
        SliceOutcome::Yielded(cp) => CompilationStatus::Yielded {
            checkpoint: CheckpointState {
                job_id: cp.job_id,
                phase: cp.phase.into(),
                next_block: cp.next_block,
                pbf_byte_offset: cp.pbf_byte_offset,
                bytes_written: cp.bytes_written,
            },
        },
        SliceOutcome::Failed(reason) => CompilationStatus::Failed { reason },
    }
}

/// Runs one budget-bounded slice of `job`. See module docs for the
/// Finished / Yielded / Failed contract. Never throws: all failures are
/// values, so foreign call sites need no try/catch ceremony.
#[uniffi::export]
pub fn compile_chunk(
    job: CompileJob,
    budget_ms: u32,
    callback: Box<dyn ProgressCallback>,
) -> CompilationStatus {
    let spec = match to_job_spec(&job) {
        Ok(s) => s,
        Err(reason) => return CompilationStatus::Failed { reason },
    };
    let budget = Duration::from_millis(u64::from(budget_ms));
    let mut on_progress = |pct: f32, status: String| callback.on_progress(pct, status);
    to_status(engine::run_slice(&spec, budget, &mut on_progress))
}

/// Cold-start resume detection: returns the durable checkpoint for a job if
/// one exists (e.g. after the OS killed the process mid-compilation), None
/// if the job has no saved state, or Failed-equivalent None on unreadable
/// state (the next compile_chunk call surfaces the precise error).
#[uniffi::export]
pub fn query_checkpoint(job_id: String, output_dir: String) -> Option<CheckpointState> {
    match engine::load_checkpoint(&output_dir, &job_id) {
        Ok(Some(cp)) => Some(CheckpointState {
            job_id: cp.job_id,
            phase: cp.phase.into(),
            next_block: cp.next_block,
            pbf_byte_offset: cp.pbf_byte_offset,
            bytes_written: cp.bytes_written,
        }),
        _ => None,
    }
}

/// Cancels a job between slices by deleting its durable state. Returns true
/// if state existed and was removed. (In-slice cancellation is not needed:
/// slices are budget-bounded, so the runner simply stops re-invoking.)
#[uniffi::export]
pub fn purge_job(job_id: String, output_dir: String) -> bool {
    engine::purge_job_state(&output_dir, &job_id)
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
                // Fixture: 2 pass1 blocks (header + 1 data) + the simulated
                // pass2/terrain/finalize placeholders (24 + 12 + 2).
                assert_eq!(summary.blocks_total, 2 + 38);
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
                // The tiny fixture's real Pass 1 completes within the budget,
                // so the yield may land in Pass1Nodes OR early Pass2Ways —
                // both are legitimate suspend points.
                assert!(matches!(
                    checkpoint.phase,
                    CompilePhase::Pass1Nodes | CompilePhase::Pass2Ways
                ));
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
            CompilationStatus::Failed { reason } => {
                assert!(reason.contains("invalid bbox"), "got: {reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn failed_on_inverted_zoom_range() {
        let mut job = test_job("badzoom");
        job.min_zoom = 15;
        job.max_zoom = 5;
        match compile_chunk(job, 300_000, Box::new(Recorder(Default::default()))) {
            CompilationStatus::Failed { reason } => {
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
