//! Suspendable slice engine — the Phase 7-shaped execution core.
//!
//! Contract (mirrored across the FFI in `ffi/src/lib.rs`):
//! - `run_slice(job, budget, on_progress)` executes work until either the job
//!   completes (`Finished`), the time budget expires (`Yielded`), or a fatal
//!   error occurs (`Failed`).
//! - State is checkpointed **durably to disk** after every yield via an
//!   atomic write (temp file + fsync + rename). Resume happens by calling
//!   `run_slice` again with the same `JobSpec` — the engine reloads its own
//!   checkpoint. The caller never round-trips state; disk is the single
//!   source of truth, because on iOS the process may be killed between
//!   slices and in-memory state cannot be trusted to survive.
//! - **Minimum forward progress guarantee:** every slice processes at least
//!   one block even if the budget is already exhausted, so a runner passing
//!   a too-small budget degrades to slow progress instead of livelocking.
//!
//! **Pass1Nodes is REAL** (P3.C3): it drives `pbf::run_pass1_slice` over the
//! mmap'd input, resuming from the checkpoint's `pbf_byte_offset` and writing
//! the node index into the per-job redb database (`index_db_path`). The
//! remaining phases (Pass2Ways / Terrain / Finalize) are still simulated
//! block loops — placeholders for Phases 4-6 behind this same contract.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::BBox;

// ---------------------------------------------------------------------------
// Contract types (pure Rust; the ffi crate mirrors these as UniFFI types)
// ---------------------------------------------------------------------------

/// Compilation phases, in execution order. `Terrain` is skipped when the job
/// has no DEM input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Pass1Nodes,
    Pass2Ways,
    Terrain,
    Finalize,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Pass1Nodes => "pass1_nodes",
            Phase::Pass2Ways => "pass2_ways",
            Phase::Terrain => "terrain",
            Phase::Finalize => "finalize",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pass1_nodes" => Some(Phase::Pass1Nodes),
            "pass2_ways" => Some(Phase::Pass2Ways),
            "terrain" => Some(Phase::Terrain),
            "finalize" => Some(Phase::Finalize),
            _ => None,
        }
    }

    /// Human label used in progress callbacks.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Pass1Nodes => "pass1: indexing nodes",
            Phase::Pass2Ways => "pass2: assembling ways",
            Phase::Terrain => "terrain: encoding elevation tiles",
            Phase::Finalize => "finalizing archive",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Validated job description. Constructed from the FFI layer's `CompileJob`.
#[derive(Debug, Clone, PartialEq)]
pub struct JobSpec {
    pub job_id: String,
    pub bbox: BBox,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// Path to the raw .osm.pbf — mmap'd read-only by the real Pass 1.
    pub pbf_path: String,
    /// Optional DEM GeoTIFF; `None` skips the Terrain phase entirely.
    pub dem_path: Option<String>,
    /// Directory owning checkpoints, the redb index, and outputs for this job.
    pub output_dir: String,
}

/// Durable resume state (checkpoint format v2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub job_id: String,
    pub phase: Phase,
    /// Blocks completed *within* `phase` (Pass 1: PBF blocks scanned;
    /// simulated phases: block index).
    pub next_block: u32,
    /// Absolute byte offset into the source PBF — the exact mmap re-entry
    /// point for the real Pass 1 (`pbf::run_pass1_slice` resume contract).
    pub pbf_byte_offset: u64,
    /// Total logical bytes written (node-index bytes + simulated output).
    pub bytes_written: u64,
    /// Blocks completed across ALL phases — feeds `RunSummary::blocks_total`
    /// (per-phase counters reset at phase boundaries; this one never does).
    pub blocks_done: u32,
}

/// Completion report for a finished job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub job_id: String,
    pub blocks_total: u32,
    pub bytes_written: u64,
}

/// Result of one execution slice.
#[derive(Debug, Clone, PartialEq)]
pub enum SliceOutcome {
    /// 100% complete; temporary state (checkpoint + redb index) purged.
    Finished(RunSummary),
    /// Budget expired; durable checkpoint written. Call `run_slice` again
    /// with the same JobSpec to resume.
    Yielded(Checkpoint),
    /// Fatal, non-resumable-as-is error (corrupted state, bad input, I/O).
    Failed(String),
}

// ---------------------------------------------------------------------------
// Phase plan
// ---------------------------------------------------------------------------

// Simulated block counts for the not-yet-real phases (Phases 4-6 replace
// these loops with the way/terrain/archive pipelines).
const PASS2_BLOCKS: u32 = 24;
const TERRAIN_BLOCKS: u32 = 12;
const FINALIZE_BLOCKS: u32 = 2;
/// Simulated cost of one block; stands in for real CPU work.
const BLOCK_WORK: Duration = Duration::from_millis(2);
/// Simulated bytes appended per completed block.
const BLOCK_OUTPUT_BYTES: u64 = 4_096;
/// Logical bytes accounted per indexed node (u64 key + 2×f64 coordinate),
/// so `bytes_written` stays meaningful and deterministic for the real Pass 1.
const NODE_INDEX_BYTES: u64 = 24;

/// Phase order for a job.
fn phase_plan(job: &JobSpec) -> Vec<Phase> {
    let mut plan = vec![Phase::Pass1Nodes, Phase::Pass2Ways];
    if job.dem_path.is_some() {
        plan.push(Phase::Terrain);
    }
    plan.push(Phase::Finalize);
    plan
}

/// Simulated block count for a phase (Pass 1 is real and dynamic → 0).
fn sim_blocks(phase: Phase) -> u32 {
    match phase {
        Phase::Pass1Nodes => 0,
        Phase::Pass2Ways => PASS2_BLOCKS,
        Phase::Terrain => TERRAIN_BLOCKS,
        Phase::Finalize => FINALIZE_BLOCKS,
    }
}

/// One unit of simulated work (Phases 4-6 placeholders).
fn process_sim_block(cp: &mut Checkpoint) {
    std::thread::sleep(BLOCK_WORK);
    cp.bytes_written += BLOCK_OUTPUT_BYTES;
    cp.next_block += 1;
    cp.blocks_done += 1;
}

/// The per-job redb index (Coordinates + Ways tables). Lives beside the
/// checkpoint; purged together with it on finish/cancel.
pub fn index_db_path(output_dir: &str, job_id: &str) -> PathBuf {
    Path::new(output_dir).join(format!("{job_id}.index.redb"))
}

// ---------------------------------------------------------------------------
// Durable checkpoint persistence (std-only; becomes a redb table in Phase 7)
// ---------------------------------------------------------------------------

const CHECKPOINT_VERSION: u32 = 2;

fn checkpoint_path(output_dir: &str, job_id: &str) -> PathBuf {
    Path::new(output_dir).join(format!("{job_id}.checkpoint"))
}

/// Atomic, durable write: temp file → fsync → rename. A crash at any point
/// leaves either the previous checkpoint or the new one — never a torn file.
fn save_checkpoint(output_dir: &str, cp: &Checkpoint) -> Result<(), String> {
    let final_path = checkpoint_path(output_dir, &cp.job_id);
    let tmp_path = final_path.with_extension("checkpoint.tmp");

    let body = format!(
        "version={CHECKPOINT_VERSION}\njob_id={}\nphase={}\nnext_block={}\npbf_byte_offset={}\nbytes_written={}\nblocks_done={}\n",
        cp.job_id, cp.phase, cp.next_block, cp.pbf_byte_offset, cp.bytes_written, cp.blocks_done,
    );

    let mut f = fs::File::create(&tmp_path)
        .map_err(|e| format!("checkpoint write failed ({}): {e}", tmp_path.display()))?;
    f.write_all(body.as_bytes())
        .map_err(|e| format!("checkpoint write failed: {e}"))?;
    f.sync_all()
        .map_err(|e| format!("checkpoint fsync failed: {e}"))?;
    drop(f);

    fs::rename(&tmp_path, &final_path).map_err(|e| format!("checkpoint rename failed: {e}"))?;
    Ok(())
}

/// Loads the checkpoint for a job if one exists. `Ok(None)` = fresh start.
/// Any malformed content is a hard `Err` — a torn or foreign file must never
/// silently restart (and thus duplicate) work.
pub fn load_checkpoint(output_dir: &str, job_id: &str) -> Result<Option<Checkpoint>, String> {
    let path = checkpoint_path(output_dir, job_id);
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("checkpoint unreadable ({}): {e}", path.display())),
    };

    let mut version = None;
    let mut id = None;
    let mut phase = None;
    let mut next_block = None;
    let mut pbf_byte_offset = None;
    let mut bytes_written = None;
    let mut blocks_done = None;

    for line in raw.lines() {
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("corrupted checkpoint: malformed line '{line}'"));
        };
        match k {
            "version" => version = v.parse::<u32>().ok(),
            "job_id" => id = Some(v.to_string()),
            "phase" => phase = Phase::from_str(v),
            "next_block" => next_block = v.parse::<u32>().ok(),
            "pbf_byte_offset" => pbf_byte_offset = v.parse::<u64>().ok(),
            "bytes_written" => bytes_written = v.parse::<u64>().ok(),
            "blocks_done" => blocks_done = v.parse::<u32>().ok(),
            other => return Err(format!("corrupted checkpoint: unknown key '{other}'")),
        }
    }

    match (
        version,
        id,
        phase,
        next_block,
        pbf_byte_offset,
        bytes_written,
        blocks_done,
    ) {
        (
            Some(CHECKPOINT_VERSION),
            Some(id),
            Some(phase),
            Some(nb),
            Some(po),
            Some(bw),
            Some(bd),
        ) => {
            if id != job_id {
                return Err(format!(
                    "corrupted checkpoint: job_id mismatch (file='{id}', requested='{job_id}')"
                ));
            }
            Ok(Some(Checkpoint {
                job_id: id,
                phase,
                next_block: nb,
                pbf_byte_offset: po,
                bytes_written: bw,
                blocks_done: bd,
            }))
        }
        (Some(v), ..) if v != CHECKPOINT_VERSION => {
            Err(format!("corrupted checkpoint: unsupported version {v}"))
        }
        _ => Err("corrupted checkpoint: missing required fields".to_string()),
    }
}

/// Removes all durable state for a job: the checkpoint AND the redb index.
/// Used by Finished (Blueprint step 8: "temporary redb files are purged")
/// and by cancel/purge. Returns true if anything existed and was removed.
pub fn purge_job_state(output_dir: &str, job_id: &str) -> bool {
    let checkpoint_gone = fs::remove_file(checkpoint_path(output_dir, job_id)).is_ok();
    let index_gone = fs::remove_file(index_db_path(output_dir, job_id)).is_ok();
    checkpoint_gone || index_gone
}

// ---------------------------------------------------------------------------
// The slice runner
// ---------------------------------------------------------------------------

/// Executes one budget-bounded slice of a compile job. See module docs for
/// the resume/yield contract.
pub fn run_slice(
    job: &JobSpec,
    budget: Duration,
    on_progress: &mut dyn FnMut(f32, String),
) -> SliceOutcome {
    // Output dir must exist (checkpoints + index live there).
    if let Err(e) = fs::create_dir_all(&job.output_dir) {
        return SliceOutcome::Failed(format!(
            "cannot create output dir '{}': {e}",
            job.output_dir
        ));
    }

    let plan = phase_plan(job);

    // Resume or fresh start. Corrupted state is fatal, never silently reset.
    let mut cp = match load_checkpoint(&job.output_dir, &job.job_id) {
        Ok(Some(cp)) => {
            // A checkpoint for a phase not in this job's plan (e.g. Terrain
            // checkpoint but the job now has no DEM) means the job definition
            // changed under us — refuse rather than guess.
            if !plan.contains(&cp.phase) {
                return SliceOutcome::Failed(format!(
                    "corrupted checkpoint: phase '{}' not in this job's plan (job definition changed?)",
                    cp.phase
                ));
            }
            cp
        }
        Ok(None) => Checkpoint {
            job_id: job.job_id.clone(),
            phase: plan[0],
            next_block: 0,
            pbf_byte_offset: 0,
            bytes_written: 0,
            blocks_done: 0,
        },
        Err(e) => return SliceOutcome::Failed(e),
    };

    let started = Instant::now();
    let n_phases = plan.len() as f32;
    // Minimum-forward-progress bookkeeping: once ANY block has been processed
    // in this slice, budget checks may yield.
    let mut slice_blocks: u32 = 0;

    let phase_index = plan.iter().position(|p| *p == cp.phase).unwrap();
    for (idx, phase) in plan.iter().copied().enumerate().skip(phase_index) {
        cp.phase = phase;
        match phase {
            // ---- REAL Pass 1: mmap → decode → project → redb ------------
            Phase::Pass1Nodes => {
                let pbf = match pbf::PbfMmap::open(Path::new(&job.pbf_path)) {
                    Ok(m) => m,
                    Err(e) => return SliceOutcome::Failed(format!("pass1: {e}")),
                };
                let db = match pbf::open_coord_db(&index_db_path(&job.output_dir, &job.job_id)) {
                    Ok(db) => db,
                    Err(e) => return SliceOutcome::Failed(format!("pass1: {e}")),
                };
                let resume_offset = match usize::try_from(cp.pbf_byte_offset) {
                    Ok(o) => o,
                    Err(_) => {
                        return SliceOutcome::Failed(format!(
                            "corrupted checkpoint: pbf_byte_offset {} exceeds address space",
                            cp.pbf_byte_offset
                        ));
                    }
                };

                let sub = match pbf::run_pass1_slice(&pbf, &db, resume_offset, &mut || {
                    started.elapsed() >= budget
                }) {
                    Ok(s) => s,
                    Err(e) => return SliceOutcome::Failed(format!("pass1: {e}")),
                };

                cp.pbf_byte_offset = sub.next_offset as u64;
                cp.next_block += sub.blocks_scanned;
                cp.blocks_done += sub.blocks_scanned;
                cp.bytes_written += sub.nodes_indexed * NODE_INDEX_BYTES;
                slice_blocks += sub.blocks_scanned;

                let frac = if pbf.is_empty() {
                    1.0
                } else {
                    sub.next_offset as f32 / pbf.len() as f32
                };
                let pct = ((idx as f32 + frac) / n_phases) * 100.0;
                on_progress(
                    pct,
                    format!("{} ({} blocks scanned)", phase.label(), cp.next_block),
                );

                if !sub.finished {
                    return match save_checkpoint(&job.output_dir, &cp) {
                        Ok(()) => SliceOutcome::Yielded(cp),
                        Err(e) => SliceOutcome::Failed(e),
                    };
                }
                cp.next_block = 0; // phase complete; next phase starts fresh
            }
            // ---- Simulated phases (Phases 4-6 placeholders) --------------
            _ => {
                let blocks = sim_blocks(phase);
                while cp.next_block < blocks {
                    // Budget check BEFORE each block, except when this slice
                    // has done nothing yet (no-livelock guarantee).
                    if slice_blocks > 0 && started.elapsed() >= budget {
                        return match save_checkpoint(&job.output_dir, &cp) {
                            Ok(()) => SliceOutcome::Yielded(cp),
                            Err(e) => SliceOutcome::Failed(e),
                        };
                    }
                    process_sim_block(&mut cp);
                    slice_blocks += 1;
                    let frac = cp.next_block as f32 / blocks as f32;
                    let pct = ((idx as f32 + frac) / n_phases) * 100.0;
                    on_progress(
                        pct,
                        format!("{} ({}/{blocks})", phase.label(), cp.next_block),
                    );
                }
                cp.next_block = 0;
            }
        }
    }

    // All phases complete: purge temporary state (checkpoint + index), report.
    purge_job_state(&job.output_dir, &job.job_id);
    SliceOutcome::Finished(RunSummary {
        job_id: job.job_id.clone(),
        blocks_total: cp.blocks_done,
        bytes_written: cp.bytes_written,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Two dense-node groups → fixture layout: 1 OSMHeader + 2 OSMData blocks.
    const G1: &[(i64, i64, i64)] = &[
        (1_000, 472_700_000, 113_900_000),
        (1_005, 472_700_100, 113_900_050),
        (900, -338_650_000, -703_450_000),
    ];
    const G2: &[(i64, i64, i64)] = &[(2_000_000_000, 0, 0), (2_000_000_007, 100, 100)];
    const FIXTURE_PASS1_BLOCKS: u32 = 3;
    const FIXTURE_NODES: u64 = 5;

    fn sim_total(dem: bool) -> u32 {
        PASS2_BLOCKS + if dem { TERRAIN_BLOCKS } else { 0 } + FINALIZE_BLOCKS
    }

    fn expected_bytes(dem: bool) -> u64 {
        FIXTURE_NODES * NODE_INDEX_BYTES + u64::from(sim_total(dem)) * BLOCK_OUTPUT_BYTES
    }

    fn test_job(dir: &Path, dem: bool) -> JobSpec {
        let pbf_path = dir.join("fixture.osm.pbf");
        fs::write(&pbf_path, pbf::fixtures::synthetic_pbf(&[G1, G2])).unwrap();
        JobSpec {
            job_id: "job-alps-1".into(),
            bbox: BBox::parse("11.15,47.05,11.65,47.45").unwrap(),
            min_zoom: 5,
            max_zoom: 14,
            pbf_path: pbf_path.to_string_lossy().into_owned(),
            dem_path: dem.then(|| "unused_dem.tif".into()),
            output_dir: dir.to_string_lossy().into_owned(),
        }
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("freehike-engine-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const BIG: Duration = Duration::from_secs(300);
    const TINY: Duration = Duration::from_millis(5);

    #[test]
    fn large_budget_finishes_real_pass1_and_purges_state() {
        let dir = tmp_dir("finish");
        let job = test_job(&dir, true);
        let mut ticks = 0u32;
        let out = run_slice(&job, BIG, &mut |_, _| ticks += 1);
        match out {
            SliceOutcome::Finished(s) => {
                assert_eq!(s.blocks_total, FIXTURE_PASS1_BLOCKS + sim_total(true));
                assert_eq!(s.bytes_written, expected_bytes(true));
            }
            other => panic!("expected Finished, got {other:?}"),
        }
        // 1 pass1 event (whole file in one sub-slice) + 1 per simulated block.
        assert_eq!(ticks, 1 + sim_total(true));
        assert!(
            load_checkpoint(&job.output_dir, &job.job_id)
                .unwrap()
                .is_none(),
            "checkpoint must be purged"
        );
        assert!(
            !index_db_path(&job.output_dir, &job.job_id).exists(),
            "redb index must be purged on finish"
        );
    }

    #[test]
    fn pass1_indexes_real_nodes_into_redb() {
        let dir = tmp_dir("index");
        let job = test_job(&dir, true);
        // Zero budget → yields immediately after minimum progress, so the
        // index survives between slices for inspection.
        let SliceOutcome::Yielded(_) = run_slice(&job, Duration::ZERO, &mut |_, _| {}) else {
            panic!("expected yield");
        };
        // Drive to the end of pass1 (blocks arrive one per zero-budget slice).
        let cp = loop {
            match run_slice(&job, Duration::ZERO, &mut |_, _| {}) {
                SliceOutcome::Yielded(cp) => {
                    if cp.phase != Phase::Pass1Nodes {
                        break cp;
                    }
                }
                other => panic!("job should still be yielding, got {other:?}"),
            }
        };
        assert_eq!(cp.phase, Phase::Pass2Ways);

        // All five fixture nodes must be durably queryable mid-job.
        let db = pbf::open_coord_db(&index_db_path(&job.output_dir, &job.job_id)).unwrap();
        assert_eq!(pbf::coord_count(&db).unwrap(), FIXTURE_NODES);
        for &(id, lat, lon) in G1.iter().chain(G2) {
            let want = pbf::web_mercator(1e-9 * (100 * lon) as f64, 1e-9 * (100 * lat) as f64);
            assert_eq!(
                pbf::get_coord(&db, id as u64).unwrap(),
                Some(want),
                "node {id}"
            );
        }
        drop(db);

        // And the recorded offset must equal the full file length.
        assert_eq!(
            cp.pbf_byte_offset,
            fs::metadata(&job.pbf_path).unwrap().len(),
            "pass1 must have consumed the whole file"
        );
    }

    #[test]
    fn tiny_budget_yields_with_durable_checkpoint() {
        let dir = tmp_dir("yield");
        let job = test_job(&dir, true);
        let out = run_slice(&job, TINY, &mut |_, _| {});
        match out {
            SliceOutcome::Yielded(cp) => {
                assert!(cp.blocks_done > 0, "must have made progress");
                let on_disk = load_checkpoint(&job.output_dir, &job.job_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(on_disk, cp, "returned checkpoint must match durable state");
            }
            other => panic!("expected Yielded, got {other:?}"),
        }
    }

    #[test]
    fn resume_continues_not_restarts() {
        let dir = tmp_dir("resume");
        let job = test_job(&dir, true);
        let SliceOutcome::Yielded(cp1) = run_slice(&job, TINY, &mut |_, _| {}) else {
            panic!("expected first slice to yield");
        };
        let SliceOutcome::Yielded(cp2) = run_slice(&job, TINY, &mut |_, _| {}) else {
            panic!("expected second slice to yield");
        };
        assert!(
            cp2.blocks_done > cp1.blocks_done,
            "second slice must continue past the first: {cp1:?} -> {cp2:?}"
        );
        assert!(cp2.bytes_written > cp1.bytes_written);
    }

    #[test]
    fn sliced_run_matches_single_run() {
        // Determinism: many tiny slices must produce the same final summary
        // as one big slice — the property behind the kill-resume invariant.
        let dir_a = tmp_dir("det-a");
        let dir_b = tmp_dir("det-b");
        let job_a = test_job(&dir_a, true);
        let job_b = test_job(&dir_b, true);

        let single = match run_slice(&job_a, BIG, &mut |_, _| {}) {
            SliceOutcome::Finished(s) => s,
            other => panic!("expected Finished, got {other:?}"),
        };

        let mut sliced = None;
        for _ in 0..1_000 {
            match run_slice(&job_b, TINY, &mut |_, _| {}) {
                SliceOutcome::Yielded(_) => continue,
                SliceOutcome::Finished(s) => {
                    sliced = Some(s);
                    break;
                }
                SliceOutcome::Failed(e) => panic!("failed mid-slices: {e}"),
            }
        }
        let sliced = sliced.expect("job did not finish within 1000 slices");
        assert_eq!(sliced.blocks_total, single.blocks_total);
        assert_eq!(sliced.bytes_written, single.bytes_written);
    }

    #[test]
    fn zero_budget_still_makes_progress() {
        let dir = tmp_dir("livelock");
        let job = test_job(&dir, true);
        let SliceOutcome::Yielded(cp1) = run_slice(&job, Duration::ZERO, &mut |_, _| {}) else {
            panic!("expected yield");
        };
        assert_eq!(cp1.blocks_done, 1, "exactly the guaranteed minimum block");
        assert_eq!(cp1.phase, Phase::Pass1Nodes);
        let SliceOutcome::Yielded(cp2) = run_slice(&job, Duration::ZERO, &mut |_, _| {}) else {
            panic!("expected yield");
        };
        assert_eq!(cp2.blocks_done, 2, "forward progress under zero budget");
    }

    #[test]
    fn missing_pbf_file_fails() {
        let dir = tmp_dir("nopbf");
        let mut job = test_job(&dir, true);
        job.pbf_path = dir.join("does-not-exist.osm.pbf").display().to_string();
        match run_slice(&job, BIG, &mut |_, _| {}) {
            SliceOutcome::Failed(reason) => {
                assert!(reason.starts_with("pass1:"), "got: {reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn corrupted_pbf_fails_loudly() {
        let dir = tmp_dir("badpbf");
        let job = test_job(&dir, true);
        fs::write(&job.pbf_path, b"<!DOCTYPE html><html>not a pbf</html>").unwrap();
        match run_slice(&job, BIG, &mut |_, _| {}) {
            SliceOutcome::Failed(reason) => {
                assert!(reason.contains("corrupted PBF"), "got: {reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn corrupted_checkpoint_fails() {
        let dir = tmp_dir("corrupt");
        let job = test_job(&dir, true);
        fs::write(
            checkpoint_path(&job.output_dir, &job.job_id),
            "definitely not a checkpoint",
        )
        .unwrap();
        match run_slice(&job, BIG, &mut |_, _| {}) {
            SliceOutcome::Failed(reason) => {
                assert!(reason.contains("corrupted checkpoint"), "got: {reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn phase_transitions_in_order() {
        let dir = tmp_dir("phases");
        let job = test_job(&dir, true);
        let labels = [
            Phase::Pass1Nodes.label(),
            Phase::Pass2Ways.label(),
            Phase::Terrain.label(),
            Phase::Finalize.label(),
        ];
        let mut seen: Vec<&'static str> = Vec::new();
        let out = run_slice(&job, BIG, &mut |_, status| {
            let phase = labels
                .iter()
                .copied()
                .find(|l| status.starts_with(l))
                .expect("status must start with a known phase label");
            if seen.last() != Some(&phase) {
                seen.push(phase);
            }
        });
        assert!(matches!(out, SliceOutcome::Finished(_)));
        assert_eq!(seen, labels.to_vec());
    }

    #[test]
    fn dem_none_skips_terrain_phase() {
        let dir = tmp_dir("nodem");
        let job = test_job(&dir, false);
        let mut saw_terrain = false;
        let out = run_slice(&job, BIG, &mut |_, status| {
            if status.starts_with("terrain") {
                saw_terrain = true;
            }
        });
        match out {
            SliceOutcome::Finished(s) => {
                assert_eq!(s.blocks_total, FIXTURE_PASS1_BLOCKS + sim_total(false));
            }
            other => panic!("expected Finished, got {other:?}"),
        }
        assert!(!saw_terrain);
    }

    #[test]
    fn progress_is_monotonic_across_slices() {
        let dir = tmp_dir("monotonic");
        let job = test_job(&dir, true);
        let mut last = 0.0f32;
        for _ in 0..1_000 {
            let mut ok = true;
            let out = run_slice(&job, TINY, &mut |pct, _| {
                if pct < last {
                    ok = false;
                }
                last = pct;
            });
            assert!(ok, "progress went backwards");
            match out {
                SliceOutcome::Yielded(_) => continue,
                SliceOutcome::Finished(_) => {
                    assert!((last - 100.0).abs() < 0.01, "final pct = {last}");
                    return;
                }
                SliceOutcome::Failed(e) => panic!("{e}"),
            }
        }
        panic!("did not finish");
    }

    /// Integrated engine over the REAL 19.5MB Innsbruck extract, sliced with
    /// a production-shaped 250ms budget — the full FFI execution contract on
    /// real data. Ignored so the L1 ladder stays fixture-independent; run:
    ///   cargo test -p compiler --release -- --ignored --nocapture real_innsbruck
    #[test]
    #[ignore]
    fn real_innsbruck_end_to_end_sliced() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../offline_sandbox/raw_data/innsbruck.osm.pbf");
        let dir = tmp_dir("real-e2e");
        let mut job = test_job(&dir, true);
        job.pbf_path = fixture.to_string_lossy().into_owned();

        let budget = Duration::from_millis(250);
        let mut slices = 0u32;
        let summary = loop {
            match run_slice(&job, budget, &mut |_, _| {}) {
                SliceOutcome::Yielded(_) => slices += 1,
                SliceOutcome::Finished(s) => break s,
                SliceOutcome::Failed(e) => panic!("failed after {slices} slices: {e}"),
            }
            assert!(slices < 10_000, "runaway");
        };

        // 265 real PBF blocks + the simulated placeholder phases.
        assert_eq!(summary.blocks_total, 265 + sim_total(true));
        // 1,900,652 nodes × 24 logical bytes + simulated output bytes.
        assert_eq!(
            summary.bytes_written,
            1_900_652 * NODE_INDEX_BYTES + u64::from(sim_total(true)) * BLOCK_OUTPUT_BYTES
        );
        println!(
            "real end-to-end: {} blocks / {slices} yields",
            summary.blocks_total
        );
    }

    #[test]
    fn invalid_output_dir_fails() {
        let mut job = test_job(&tmp_dir("badout"), true);
        // A path that cannot be created (child of a regular file).
        let blocker = tmp_dir("badout-blocker").join("file");
        fs::write(&blocker, b"x").unwrap();
        job.output_dir = blocker.join("sub").to_string_lossy().into_owned();
        match run_slice(&job, BIG, &mut |_, _| {}) {
            SliceOutcome::Failed(reason) => assert!(reason.contains("output dir"), "got: {reason}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
