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
//! The block work is currently *simulated* (deterministic counters + a small
//! sleep standing in for CPU work). Phases 3-6 replace the body of
//! `process_block` with the real PBF/redb/terrain pipelines behind this same
//! contract; the checkpoint file becomes a redb table with identical fields.

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
    /// Path to the raw .osm.pbf (unused by the simulated engine; the real
    /// Pass 1/2 mmap this).
    pub pbf_path: String,
    /// Optional DEM GeoTIFF; `None` skips the Terrain phase entirely.
    pub dem_path: Option<String>,
    /// Directory owning checkpoints and outputs for this job.
    pub output_dir: String,
}

/// Durable resume state. Field-compatible with the future redb checkpoint
/// table (Phase 7) — only the storage medium changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub job_id: String,
    pub phase: Phase,
    /// Next block index within `phase` (blocks before it are complete).
    pub next_block: u32,
    /// Simulated for now; the real Pass 1/2 store the mmap read offset here
    /// so a resume re-enters the PBF at the exact block boundary.
    pub pbf_byte_offset: u64,
    /// Total bytes appended to output archives so far.
    pub bytes_written: u64,
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
    /// 100% complete; the checkpoint (temporary state) has been purged.
    Finished(RunSummary),
    /// Budget expired; durable checkpoint written. Call `run_slice` again
    /// with the same JobSpec to resume.
    Yielded(Checkpoint),
    /// Fatal, non-resumable-as-is error (corrupted state, bad input, I/O).
    Failed(String),
}

// ---------------------------------------------------------------------------
// Simulated work model (deterministic; replaced by real pipelines later)
// ---------------------------------------------------------------------------

const PASS1_BLOCKS: u32 = 24;
const PASS2_BLOCKS: u32 = 24;
const TERRAIN_BLOCKS: u32 = 12;
const FINALIZE_BLOCKS: u32 = 2;
/// Simulated cost of one block; stands in for real CPU work.
const BLOCK_WORK: Duration = Duration::from_millis(2);
/// Simulated bytes appended per completed block.
const BLOCK_OUTPUT_BYTES: u64 = 4_096;
/// Simulated PBF bytes consumed per Pass 1/2 block.
const BLOCK_PBF_BYTES: u64 = 8_192;

/// Phase schedule for a job (order + block counts).
fn schedule(job: &JobSpec) -> Vec<(Phase, u32)> {
    let mut plan = vec![
        (Phase::Pass1Nodes, PASS1_BLOCKS),
        (Phase::Pass2Ways, PASS2_BLOCKS),
    ];
    if job.dem_path.is_some() {
        plan.push((Phase::Terrain, TERRAIN_BLOCKS));
    }
    plan.push((Phase::Finalize, FINALIZE_BLOCKS));
    plan
}

fn total_blocks(plan: &[(Phase, u32)]) -> u32 {
    plan.iter().map(|(_, n)| n).sum()
}

/// One unit of work. The real pipelines replace this body; the contract
/// (advance counters, return bytes) stays.
fn process_block(phase: Phase, checkpoint: &mut Checkpoint) {
    std::thread::sleep(BLOCK_WORK);
    checkpoint.bytes_written += BLOCK_OUTPUT_BYTES;
    if matches!(phase, Phase::Pass1Nodes | Phase::Pass2Ways) {
        checkpoint.pbf_byte_offset += BLOCK_PBF_BYTES;
    }
    checkpoint.next_block += 1;
}

// ---------------------------------------------------------------------------
// Durable checkpoint persistence (std-only; becomes a redb table in Phase 7)
// ---------------------------------------------------------------------------

const CHECKPOINT_VERSION: u32 = 1;

fn checkpoint_path(output_dir: &str, job_id: &str) -> PathBuf {
    Path::new(output_dir).join(format!("{job_id}.checkpoint"))
}

/// Atomic, durable write: temp file → fsync → rename. A crash at any point
/// leaves either the previous checkpoint or the new one — never a torn file.
fn save_checkpoint(output_dir: &str, cp: &Checkpoint) -> Result<(), String> {
    let final_path = checkpoint_path(output_dir, &cp.job_id);
    let tmp_path = final_path.with_extension("checkpoint.tmp");

    let body = format!(
        "version={CHECKPOINT_VERSION}\njob_id={}\nphase={}\nnext_block={}\npbf_byte_offset={}\nbytes_written={}\n",
        cp.job_id, cp.phase, cp.next_block, cp.pbf_byte_offset, cp.bytes_written,
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
    ) {
        (Some(CHECKPOINT_VERSION), Some(id), Some(phase), Some(nb), Some(po), Some(bw)) => {
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
            }))
        }
        (Some(v), ..) if v != CHECKPOINT_VERSION => {
            Err(format!("corrupted checkpoint: unsupported version {v}"))
        }
        _ => Err("corrupted checkpoint: missing required fields".to_string()),
    }
}

/// Removes all durable state for a job (checkpoint today; partial outputs
/// too once the real pipelines land). Used by Finished and by cancel/purge.
pub fn purge_job_state(output_dir: &str, job_id: &str) -> bool {
    fs::remove_file(checkpoint_path(output_dir, job_id)).is_ok()
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
    // Output dir must exist (checkpoints live there).
    if let Err(e) = fs::create_dir_all(&job.output_dir) {
        return SliceOutcome::Failed(format!(
            "cannot create output dir '{}': {e}",
            job.output_dir
        ));
    }

    let plan = schedule(job);
    let total = total_blocks(&plan);

    // Resume or fresh start. Corrupted state is fatal, never silently reset.
    let mut cp = match load_checkpoint(&job.output_dir, &job.job_id) {
        Ok(Some(cp)) => {
            // A checkpoint for a phase not in this job's plan (e.g. Terrain
            // checkpoint but the job now has no DEM) means the job definition
            // changed under us — refuse rather than guess.
            if !plan.iter().any(|(p, _)| *p == cp.phase) {
                return SliceOutcome::Failed(format!(
                    "corrupted checkpoint: phase '{}' not in this job's plan (job definition changed?)",
                    cp.phase
                ));
            }
            cp
        }
        Ok(None) => Checkpoint {
            job_id: job.job_id.clone(),
            phase: plan[0].0,
            next_block: 0,
            pbf_byte_offset: 0,
            bytes_written: 0,
        },
        Err(e) => return SliceOutcome::Failed(e),
    };

    let started = Instant::now();
    let mut blocks_done_before: u32 = plan
        .iter()
        .take_while(|(p, _)| *p != cp.phase)
        .map(|(_, n)| n)
        .sum();
    blocks_done_before += cp.next_block;
    let mut done = blocks_done_before;

    // Walk the plan from the checkpointed phase forward.
    let phase_index = plan.iter().position(|(p, _)| *p == cp.phase).unwrap();
    for (phase, blocks) in plan[phase_index..].iter().copied() {
        cp.phase = phase;
        while cp.next_block < blocks {
            // Budget check BEFORE each block except the very first of the
            // slice: minimum forward progress guarantee (no livelock).
            let first_block_of_slice = done == blocks_done_before;
            if !first_block_of_slice && started.elapsed() >= budget {
                return match save_checkpoint(&job.output_dir, &cp) {
                    Ok(()) => SliceOutcome::Yielded(cp),
                    Err(e) => SliceOutcome::Failed(e),
                };
            }

            process_block(phase, &mut cp);
            done += 1;
            let pct = (done as f32 / total as f32) * 100.0;
            on_progress(pct, format!("{} ({done}/{total})", phase.label()));
        }
        cp.next_block = 0; // phase complete; next phase starts at block 0
    }

    // All phases complete: purge temporary state, report.
    purge_job_state(&job.output_dir, &job.job_id);
    SliceOutcome::Finished(RunSummary {
        job_id: job.job_id.clone(),
        blocks_total: total,
        bytes_written: cp.bytes_written,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_job(dir: &Path, dem: bool) -> JobSpec {
        JobSpec {
            job_id: "job-alps-1".into(),
            bbox: BBox::parse("11.15,47.05,11.65,47.45").unwrap(),
            min_zoom: 5,
            max_zoom: 14,
            pbf_path: "unused.osm.pbf".into(),
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
    fn large_budget_finishes_and_purges_checkpoint() {
        let dir = tmp_dir("finish");
        let job = test_job(&dir, true);
        let mut ticks = 0u32;
        let out = run_slice(&job, BIG, &mut |_, _| ticks += 1);
        match out {
            SliceOutcome::Finished(s) => {
                assert_eq!(
                    s.blocks_total,
                    PASS1_BLOCKS + PASS2_BLOCKS + TERRAIN_BLOCKS + FINALIZE_BLOCKS
                );
                assert_eq!(s.bytes_written, s.blocks_total as u64 * BLOCK_OUTPUT_BYTES);
            }
            other => panic!("expected Finished, got {other:?}"),
        }
        assert_eq!(
            ticks,
            PASS1_BLOCKS + PASS2_BLOCKS + TERRAIN_BLOCKS + FINALIZE_BLOCKS
        );
        assert!(
            load_checkpoint(&job.output_dir, &job.job_id)
                .unwrap()
                .is_none(),
            "checkpoint must be purged"
        );
    }

    #[test]
    fn tiny_budget_yields_with_checkpoint_file() {
        let dir = tmp_dir("yield");
        let job = test_job(&dir, true);
        let out = run_slice(&job, TINY, &mut |_, _| {});
        match out {
            SliceOutcome::Yielded(cp) => {
                assert_eq!(cp.phase, Phase::Pass1Nodes);
                assert!(cp.next_block > 0, "must have made progress");
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
        let progressed = cp2.phase != cp1.phase || cp2.next_block > cp1.next_block;
        assert!(
            progressed,
            "second slice must continue past the first: {cp1:?} -> {cp2:?}"
        );
        assert!(cp2.bytes_written > cp1.bytes_written);
    }

    #[test]
    fn sliced_run_matches_single_run() {
        // Determinism: many tiny slices must produce the same final summary
        // as one big slice — this property is what makes the L3 kill-resume
        // torture tests meaningful later.
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
        assert_eq!(cp1.next_block, 1, "exactly the guaranteed minimum block");
        let SliceOutcome::Yielded(cp2) = run_slice(&job, Duration::ZERO, &mut |_, _| {}) else {
            panic!("expected yield");
        };
        assert_eq!(cp2.next_block, 2, "forward progress under zero budget");
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
                assert_eq!(
                    s.blocks_total,
                    PASS1_BLOCKS + PASS2_BLOCKS + FINALIZE_BLOCKS
                );
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
