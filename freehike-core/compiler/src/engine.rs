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
//! **Pass1Nodes, Pass2Ways, Pass3Tiles and Finalize are REAL**
//! (P3.C3/C4, P4.C2, P5.C1): the passes drive `pbf::run_pass{1,2,3}_slice`
//! over the mmap'd input and the per-job redb index (`index_db_path`),
//! each resuming from its own durable cursor (`pbf_byte_offset` /
//! `pass2_byte_offset` / `pass3_last_way_id`); Finalize drives
//! `tiles::run_finalize_encode_slice` (cursor `pass5_last_tile`) plus one
//! idempotent `tiles::assemble_archive` block, producing
//! `{job_id}.pmtiles` at `archive_path` BEFORE the index purge. Only
//! Terrain remains a simulated block loop — the Phase 6 placeholder
//! behind this same contract.

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
    Pass3Tiles,
    Terrain,
    Finalize,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Pass1Nodes => "pass1_nodes",
            Phase::Pass2Ways => "pass2_ways",
            Phase::Pass3Tiles => "pass3_tiles",
            Phase::Terrain => "terrain",
            Phase::Finalize => "finalize",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pass1_nodes" => Some(Phase::Pass1Nodes),
            "pass2_ways" => Some(Phase::Pass2Ways),
            "pass3_tiles" => Some(Phase::Pass3Tiles),
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
            Phase::Pass3Tiles => "pass3: binning ways into tiles",
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

/// Durable resume state (checkpoint format v5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub job_id: String,
    pub phase: Phase,
    /// Blocks completed *within* `phase` (real passes 1/2: PBF blocks
    /// scanned; pass 3: ways binned; simulated phases: block index).
    pub next_block: u32,
    /// Absolute byte offset into the source PBF — the exact mmap re-entry
    /// point for the real Pass 1 (`pbf::run_pass1_slice` resume contract).
    pub pbf_byte_offset: u64,
    /// Pass 2's own cursor over the same file (it re-walks the framing
    /// through the way-bearing view, starting from 0) — independent of
    /// Pass 1's offset so each pass resumes exactly where IT stopped.
    pub pass2_byte_offset: u64,
    /// Pass 3's cursor: the last way ID fully binned into TileFeatures
    /// (0 = none yet). Not a byte offset — Pass 3 iterates the WAYS table,
    /// not the file.
    pub pass3_last_way_id: u64,
    /// Finalize's encode cursor: the PMTiles tile ID of the last tile
    /// fully encoded into the temporary data file (0 = none yet — no real
    /// tile at our zooms has ID 0). Maps bijectively back to the
    /// TileFeatures scan position on resume.
    pub pass5_last_tile: u64,
    /// Total logical bytes written (node/way-index bytes + simulated output).
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

// Simulated block count for the one not-yet-real phase (Phase 6 replaces
// this loop with the terrain pipeline).
const TERRAIN_BLOCKS: u32 = 12;
/// Simulated cost of one block; stands in for real CPU work.
const BLOCK_WORK: Duration = Duration::from_millis(2);
/// Simulated bytes appended per completed block.
const BLOCK_OUTPUT_BYTES: u64 = 4_096;
/// Logical bytes accounted per indexed node (u64 key + 2×f64 coordinate),
/// so `bytes_written` stays meaningful and deterministic for the real Pass 1.
const NODE_INDEX_BYTES: u64 = 24;
/// Logical bytes accounted per indexed way (key + amortized ref bytes) —
/// deterministic accounting for the real Pass 2, same role as
/// [`NODE_INDEX_BYTES`].
const WAY_INDEX_BYTES: u64 = 32;
/// Logical bytes accounted per binned tile feature (composite key +
/// amortized clipped-segment bytes) — deterministic accounting for the real
/// Pass 3, same role as [`NODE_INDEX_BYTES`].
const TILE_FEATURE_BYTES: u64 = 64;

/// Phase order for a job.
fn phase_plan(job: &JobSpec) -> Vec<Phase> {
    let mut plan = vec![Phase::Pass1Nodes, Phase::Pass2Ways, Phase::Pass3Tiles];
    if job.dem_path.is_some() {
        plan.push(Phase::Terrain);
    }
    plan.push(Phase::Finalize);
    plan
}

/// Simulated block count for a phase (the real phases are dynamic → 0).
fn sim_blocks(phase: Phase) -> u32 {
    match phase {
        Phase::Pass1Nodes | Phase::Pass2Ways | Phase::Pass3Tiles | Phase::Finalize => 0,
        Phase::Terrain => TERRAIN_BLOCKS,
    }
}

/// One unit of simulated work (the Terrain placeholder).
fn process_sim_block(cp: &mut Checkpoint) {
    std::thread::sleep(BLOCK_WORK);
    cp.bytes_written += BLOCK_OUTPUT_BYTES;
    cp.next_block += 1;
    cp.blocks_done += 1;
}

/// The per-job redb index (Coordinates + Ways + TileFeatures + Finalize
/// bookkeeping tables). Lives beside the checkpoint; purged together with
/// it on finish/cancel.
pub fn index_db_path(output_dir: &str, job_id: &str) -> PathBuf {
    Path::new(output_dir).join(format!("{job_id}.index.redb"))
}

/// The job's compiled output — the one artifact that SURVIVES the purge.
pub fn archive_path(output_dir: &str, job_id: &str) -> PathBuf {
    Path::new(output_dir).join(format!("{job_id}.pmtiles"))
}

/// Finalize's temporary payload store (encode-stage appends); purged with
/// the rest of the job state.
fn tile_data_tmp_path(output_dir: &str, job_id: &str) -> PathBuf {
    Path::new(output_dir).join(format!("{job_id}.tiledata.tmp"))
}

// ---------------------------------------------------------------------------
// Durable checkpoint persistence (std-only; becomes a redb table in Phase 7)
// ---------------------------------------------------------------------------

// v5: added pass5_last_tile (P5.C1). Any format change bumps the version —
// that discipline is what keeps kill-resume honest; older versions are
// rejected as corrupt rather than guessed at (no shipped users yet).
const CHECKPOINT_VERSION: u32 = 5;

fn checkpoint_path(output_dir: &str, job_id: &str) -> PathBuf {
    Path::new(output_dir).join(format!("{job_id}.checkpoint"))
}

/// Atomic, durable write: temp file → fsync → rename. A crash at any point
/// leaves either the previous checkpoint or the new one — never a torn file.
fn save_checkpoint(output_dir: &str, cp: &Checkpoint) -> Result<(), String> {
    let final_path = checkpoint_path(output_dir, &cp.job_id);
    let tmp_path = final_path.with_extension("checkpoint.tmp");

    let body = format!(
        "version={CHECKPOINT_VERSION}\njob_id={}\nphase={}\nnext_block={}\npbf_byte_offset={}\npass2_byte_offset={}\npass3_last_way_id={}\npass5_last_tile={}\nbytes_written={}\nblocks_done={}\n",
        cp.job_id,
        cp.phase,
        cp.next_block,
        cp.pbf_byte_offset,
        cp.pass2_byte_offset,
        cp.pass3_last_way_id,
        cp.pass5_last_tile,
        cp.bytes_written,
        cp.blocks_done,
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
    let mut pass2_byte_offset = None;
    let mut pass3_last_way_id = None;
    let mut pass5_last_tile = None;
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
            "pass2_byte_offset" => pass2_byte_offset = v.parse::<u64>().ok(),
            "pass3_last_way_id" => pass3_last_way_id = v.parse::<u64>().ok(),
            "pass5_last_tile" => pass5_last_tile = v.parse::<u64>().ok(),
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
        pass2_byte_offset,
        pass3_last_way_id,
        pass5_last_tile,
        bytes_written,
        blocks_done,
    ) {
        (
            Some(CHECKPOINT_VERSION),
            Some(id),
            Some(phase),
            Some(nb),
            Some(po),
            Some(p2o),
            Some(p3w),
            Some(p5t),
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
                pass2_byte_offset: p2o,
                pass3_last_way_id: p3w,
                pass5_last_tile: p5t,
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

/// Removes all durable TEMPORARY state for a job: the checkpoint, the redb
/// index, Finalize's tile-data scratch file, and any half-written archive
/// temp. The finished `.pmtiles` archive is deliberately NOT touched —
/// it is the job's product, not its state. Used by Finished (Blueprint
/// step 8: "temporary redb files are purged") and by cancel/purge.
/// Returns true if anything existed and was removed.
pub fn purge_job_state(output_dir: &str, job_id: &str) -> bool {
    let checkpoint_gone = fs::remove_file(checkpoint_path(output_dir, job_id)).is_ok();
    let index_gone = fs::remove_file(index_db_path(output_dir, job_id)).is_ok();
    let tiledata_gone = fs::remove_file(tile_data_tmp_path(output_dir, job_id)).is_ok();
    let archive_tmp_gone =
        fs::remove_file(archive_path(output_dir, job_id).with_extension("pmtiles.tmp")).is_ok();
    checkpoint_gone || index_gone || tiledata_gone || archive_tmp_gone
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
            pass2_byte_offset: 0,
            pass3_last_way_id: 0,
            pass5_last_tile: 0,
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
            // ---- REAL Pass 2: re-walk framing → filter → ways → redb -----
            Phase::Pass2Ways => {
                let pbf = match pbf::PbfMmap::open(Path::new(&job.pbf_path)) {
                    Ok(m) => m,
                    Err(e) => return SliceOutcome::Failed(format!("pass2: {e}")),
                };
                let db = match pbf::open_coord_db(&index_db_path(&job.output_dir, &job.job_id)) {
                    Ok(db) => db,
                    Err(e) => return SliceOutcome::Failed(format!("pass2: {e}")),
                };
                let resume_offset = match usize::try_from(cp.pass2_byte_offset) {
                    Ok(o) => o,
                    Err(_) => {
                        return SliceOutcome::Failed(format!(
                            "corrupted checkpoint: pass2_byte_offset {} exceeds address space",
                            cp.pass2_byte_offset
                        ));
                    }
                };

                let sub = match pbf::run_pass2_slice(&pbf, &db, resume_offset, &mut || {
                    started.elapsed() >= budget
                }) {
                    Ok(s) => s,
                    Err(e) => return SliceOutcome::Failed(format!("pass2: {e}")),
                };

                cp.pass2_byte_offset = sub.next_offset as u64;
                cp.next_block += sub.blocks_scanned;
                cp.blocks_done += sub.blocks_scanned;
                cp.bytes_written += sub.ways_indexed * WAY_INDEX_BYTES;
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
                cp.next_block = 0;
            }
            // ---- REAL Pass 3: WAYS → simplify → clip → TileFeatures ------
            Phase::Pass3Tiles => {
                let db = match pbf::open_coord_db(&index_db_path(&job.output_dir, &job.job_id)) {
                    Ok(db) => db,
                    Err(e) => return SliceOutcome::Failed(format!("pass3: {e}")),
                };

                let sub = match pbf::run_pass3_slice(&db, cp.pass3_last_way_id, &mut || {
                    started.elapsed() >= budget
                }) {
                    Ok(s) => s,
                    Err(e) => return SliceOutcome::Failed(format!("pass3: {e}")),
                };

                cp.pass3_last_way_id = sub.last_way_id;
                cp.next_block += sub.ways_binned;
                cp.blocks_done += sub.ways_binned;
                cp.bytes_written += sub.features_written * TILE_FEATURE_BYTES;
                slice_blocks += sub.ways_binned;

                // Progress denominator: total indexed ways (Pass 2 is
                // complete by the time this phase runs, so it's stable).
                let total_ways = match pbf::way_count(&db) {
                    Ok(n) => n,
                    Err(e) => return SliceOutcome::Failed(format!("pass3: {e}")),
                };
                let frac = if total_ways == 0 {
                    1.0
                } else {
                    cp.next_block as f32 / total_ways as f32
                };
                let pct = ((idx as f32 + frac) / n_phases) * 100.0;
                on_progress(
                    pct,
                    format!("{} ({}/{total_ways} ways)", phase.label(), cp.next_block),
                );

                if !sub.finished {
                    return match save_checkpoint(&job.output_dir, &cp) {
                        Ok(()) => SliceOutcome::Yielded(cp),
                        Err(e) => SliceOutcome::Failed(e),
                    };
                }
                cp.next_block = 0;
            }
            // ---- REAL Finalize: TileFeatures → MVT → PMTiles v3 ----------
            Phase::Finalize => {
                let db = match pbf::open_coord_db(&index_db_path(&job.output_dir, &job.job_id)) {
                    Ok(db) => db,
                    Err(e) => return SliceOutcome::Failed(format!("finalize: {e}")),
                };
                let data_path = tile_data_tmp_path(&job.output_dir, &job.job_id);
                let total_rows = match tiles::tile_feature_row_count(&db) {
                    Ok(n) => n,
                    Err(e) => return SliceOutcome::Failed(format!("finalize: {e}")),
                };

                // Stage 1: budget-yieldable MVT encode of every tile.
                let sub = match tiles::run_finalize_encode_slice(
                    &db,
                    &data_path,
                    cp.pass5_last_tile,
                    &mut || started.elapsed() >= budget,
                ) {
                    Ok(s) => s,
                    Err(e) => return SliceOutcome::Failed(format!("finalize: {e}")),
                };

                cp.pass5_last_tile = sub.last_tile_id;
                cp.next_block += sub.features_drained;
                cp.blocks_done += sub.tiles_encoded;
                cp.bytes_written += sub.payload_bytes_written;
                slice_blocks += sub.tiles_encoded;

                // Encode spans the first 90% of this phase's progress; the
                // assembly block is the final 10%.
                let frac = if total_rows == 0 {
                    0.9
                } else {
                    0.9 * (cp.next_block as f32 / total_rows as f32)
                };
                let pct = ((idx as f32 + frac) / n_phases) * 100.0;
                on_progress(
                    pct,
                    format!(
                        "{} ({}/{total_rows} features packed)",
                        phase.label(),
                        cp.next_block
                    ),
                );

                if !sub.finished {
                    return match save_checkpoint(&job.output_dir, &cp) {
                        Ok(()) => SliceOutcome::Yielded(cp),
                        Err(e) => SliceOutcome::Failed(e),
                    };
                }
                // Encode complete. Assembly is one atomic, idempotent block;
                // if this slice already spent its budget, yield first and
                // let the next slice own it (min-progress covers a fresh
                // slice whose budget is already zero).
                if slice_blocks > 0 && started.elapsed() >= budget {
                    return match save_checkpoint(&job.output_dir, &cp) {
                        Ok(()) => SliceOutcome::Yielded(cp),
                        Err(e) => SliceOutcome::Failed(e),
                    };
                }

                // Stage 2: assemble the archive at its final destination —
                // BEFORE the loop exit purges the index this data lives in.
                let bounds = (job.bbox.west, job.bbox.south, job.bbox.east, job.bbox.north);
                let out_path = archive_path(&job.output_dir, &job.job_id);
                let info = match tiles::assemble_archive(&db, &data_path, &out_path, bounds) {
                    Ok(i) => i,
                    Err(e) => return SliceOutcome::Failed(format!("finalize: {e}")),
                };

                // Encode already accounted the data section (payload
                // appends); assembly adds the header + directories +
                // metadata, so a clean run's total equals the archive size.
                cp.bytes_written += info.archive_bytes - info.tile_data_bytes;
                cp.blocks_done += 1;
                slice_blocks += 1;
                let pct = ((idx as f32 + 1.0) / n_phases) * 100.0;
                on_progress(
                    pct,
                    format!(
                        "{} ({} tiles, {} bytes)",
                        phase.label(),
                        info.tile_entries,
                        info.archive_bytes
                    ),
                );
                cp.next_block = 0;
            }
            // ---- Simulated phase (Phase 6 placeholder) -------------------
            Phase::Terrain => {
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

    use pbf::fixtures::FixtureWay;

    // Fixture layout: 1 OSMHeader + 2 dense-node OSMData + 2 way OSMData = 5
    // blocks, walked once by each real pass.
    const G1: &[(i64, i64, i64)] = &[
        (1_000, 472_700_000, 113_900_000),
        (1_005, 472_700_100, 113_900_050),
        (900, -338_650_000, -703_450_000),
    ];
    const G2: &[(i64, i64, i64)] = &[(2_000_000_000, 0, 0), (2_000_000_007, 100, 100)];
    /// Relevant block: way 500 kept (highway); 501 tag-filtered (building).
    /// Way 500's refs are deliberately LOCAL (the two Innsbruck nodes, ~1m
    /// apart) so its Pass-3 binning is exactly one z14 tile — a way spanning
    /// hemispheres would bin thousands of tiles and make the byte accounting
    /// assertions unverifiable by hand. Cross-hemisphere assembly stays
    /// covered by the pbf crate's own tests.
    const W1: &[FixtureWay<'static>] = &[
        (500, b"highway", b"path", &[1_000, 1_005]),
        (501, b"building", b"yes", &[1_000, 900]),
    ];
    /// Irrelevant block: rejected whole by the StringTable pre-filter.
    const W2: &[FixtureWay<'static>] = &[(600, b"created_by", b"JOSM", &[1_000, 900])];
    const FIXTURE_BLOCKS: u32 = 5; // scanned once per real pass
    const FIXTURE_NODES: u64 = 5;
    const FIXTURE_WAYS: u64 = 1;
    /// Way 500 fits inside one z14 tile, clear of the buffer zone.
    const FIXTURE_TILE_FEATURES: u64 = 1;

    /// Finalize's dynamic block count for the fixture: one encoded tile
    /// (the single binned feature's tile) + the assembly block.
    const FIXTURE_FINALIZE_BLOCKS: u32 = FIXTURE_TILE_FEATURES as u32 + 1;

    fn sim_total(dem: bool) -> u32 {
        if dem {
            TERRAIN_BLOCKS
        } else {
            0
        }
    }

    fn expected_blocks(dem: bool) -> u32 {
        FIXTURE_BLOCKS * 2 + FIXTURE_WAYS as u32 + sim_total(dem) + FIXTURE_FINALIZE_BLOCKS
    }

    /// Static part of the byte accounting. A finished job's total is this
    /// plus the real archive size (encode counts the data section, assembly
    /// counts header + directories + metadata) — callers measure that from
    /// disk, since gzip output length isn't worth hand-deriving.
    fn expected_bytes_before_finalize(dem: bool) -> u64 {
        FIXTURE_NODES * NODE_INDEX_BYTES
            + FIXTURE_WAYS * WAY_INDEX_BYTES
            + FIXTURE_TILE_FEATURES * TILE_FEATURE_BYTES
            + u64::from(sim_total(dem)) * BLOCK_OUTPUT_BYTES
    }

    fn test_job(dir: &Path, dem: bool) -> JobSpec {
        let pbf_path = dir.join("fixture.osm.pbf");
        fs::write(
            &pbf_path,
            pbf::fixtures::synthetic_pbf_with_ways(&[G1, G2], &[W1, W2]),
        )
        .unwrap();
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
                assert_eq!(s.blocks_total, expected_blocks(true));
                let archive_len = fs::metadata(archive_path(&job.output_dir, &job.job_id))
                    .unwrap()
                    .len();
                assert_eq!(
                    s.bytes_written,
                    expected_bytes_before_finalize(true) + archive_len,
                    "a clean run's finalize accounting must equal the archive size"
                );
            }
            other => panic!("expected Finished, got {other:?}"),
        }
        // 1 event per real-pass sub-slice (whole file/table each) + 1 per
        // sim block + 2 finalize events (encode slice + assembly).
        assert_eq!(ticks, 3 + sim_total(true) + 2);
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
    fn pass2_indexes_ways_and_geometry_assembles_mid_job() {
        let dir = tmp_dir("pass2");
        let job = test_job(&dir, true);
        // Zero-budget slices: drive block-by-block until Pass 2 completes
        // (checkpoint phase advances past Pass2Ways).
        let cp = loop {
            match run_slice(&job, Duration::ZERO, &mut |_, _| {}) {
                SliceOutcome::Yielded(cp) => {
                    if !matches!(cp.phase, Phase::Pass1Nodes | Phase::Pass2Ways) {
                        break cp;
                    }
                }
                other => panic!("job should still be yielding, got {other:?}"),
            }
        };
        assert_eq!(cp.phase, Phase::Pass3Tiles);
        assert_eq!(
            cp.pass2_byte_offset,
            fs::metadata(&job.pbf_path).unwrap().len(),
            "pass2 must have consumed the whole file through its own cursor"
        );

        // The join across both tables, mid-job, from durable state alone.
        let db = pbf::open_coord_db(&index_db_path(&job.output_dir, &job.job_id)).unwrap();
        assert_eq!(
            pbf::get_way_refs(&db, 500).unwrap(),
            Some(vec![1_000, 1_005])
        );
        assert_eq!(pbf::get_way_refs(&db, 501).unwrap(), None, "tag-filtered");
        assert_eq!(pbf::get_way_refs(&db, 600).unwrap(), None, "prefiltered");

        let line = pbf::assemble_way_geometry(&db, 500).unwrap().unwrap();
        let want: Vec<(f64, f64)> = [1_000i64, 1_005]
            .iter()
            .map(|id| {
                let &(_, lat, lon) = G1.iter().find(|(nid, _, _)| nid == id).unwrap();
                pbf::web_mercator(1e-9 * (100 * lon) as f64, 1e-9 * (100 * lat) as f64)
            })
            .collect();
        assert_eq!(line, want, "geometry join must survive the engine path");
    }

    #[test]
    fn pass3_bins_tiles_mid_job() {
        let dir = tmp_dir("pass3");
        let job = test_job(&dir, true);
        // Drive zero-budget slices until Pass 3 completes.
        let cp = loop {
            match run_slice(&job, Duration::ZERO, &mut |_, _| {}) {
                SliceOutcome::Yielded(cp) => {
                    if !matches!(
                        cp.phase,
                        Phase::Pass1Nodes | Phase::Pass2Ways | Phase::Pass3Tiles
                    ) {
                        break cp;
                    }
                }
                other => panic!("job should still be yielding, got {other:?}"),
            }
        };
        assert_eq!(cp.phase, Phase::Terrain);
        assert_eq!(
            cp.pass3_last_way_id, 500,
            "cursor must land on the last (only) renderable way"
        );

        // The binned feature, mid-job, from durable state alone: way 500 in
        // the single z14 tile containing the two Innsbruck nodes.
        let db = pbf::open_coord_db(&index_db_path(&job.output_dir, &job.job_id)).unwrap();
        let want_line = pbf::assemble_way_geometry(&db, 500).unwrap().unwrap();
        let (tx, ty) = geom::mercator_to_tile(want_line[0].0, want_line[0].1, pbf::BASE_TILE_ZOOM);
        let feats = pbf::get_tile_features(&db, pbf::BASE_TILE_ZOOM, tx, ty).unwrap();
        // The 2-vertex way passes through RDP unchanged and lies fully
        // inside the tile, so the stored segments are the geometry verbatim —
        // now carrying its layer/class tag metadata end to end (P5.C2).
        assert_eq!(
            feats,
            vec![pbf::tile::TileFeature {
                way_id: 500,
                layer: 0,
                class: b"path".to_vec(),
                segments: vec![want_line],
            }]
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
            Phase::Pass3Tiles.label(),
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
                assert_eq!(s.blocks_total, expected_blocks(false));
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

        // 265 real PBF blocks walked by passes 1 AND 2, one Pass-3 block
        // per indexed way, one Finalize block per encoded tile + 1 assembly
        // block, + the simulated Terrain placeholder. Ways and tiles are no
        // longer separable from the block count alone, so plausibility-gate
        // against the REAL archive the run must now produce.
        let archive = fs::read(archive_path(&job.output_dir, &job.job_id)).unwrap();
        assert_eq!(&archive[0..7], b"PMTiles");
        assert_eq!(archive[7], 3);
        let entries = u64::from_le_bytes(archive[80..88].try_into().unwrap());
        let data_off = u64::from_le_bytes(archive[56..64].try_into().unwrap());
        let data_len = u64::from_le_bytes(archive[64..72].try_into().unwrap());
        assert_eq!(data_off + data_len, archive.len() as u64);

        // dynamic = ways + tiles_encoded. The Innsbruck fixture holds
        // 29,558 highway paths alone (research md), and every archive entry
        // implies an encoded tile — both floors must hold together. The
        // extract's 97,619 features concentrate ~96 per tile (dense city
        // core + valley corridors), so the DISTINCT-tile count is in the
        // ~1,000 range, not the feature range.
        let dynamic = u64::from(summary.blocks_total) - 265 * 2 - u64::from(sim_total(true)) - 1;
        assert!(
            (500..20_000).contains(&entries),
            "archive tile count outside the plausible band: {entries}"
        );
        assert!(
            dynamic > 29_000 + entries,
            "implausibly few renderable ways: dynamic={dynamic}, entries={entries}"
        );
        assert!(summary.bytes_written > archive.len() as u64);
        println!(
            "real end-to-end: {} blocks / {entries} archive tiles / {} archive bytes / {slices} yields",
            summary.blocks_total,
            archive.len()
        );
    }

    /// The P5.C1 contract line: the archive is written to its final
    /// destination BEFORE the purge deletes the index it was built from,
    /// and it is the ONLY artifact that survives.
    #[test]
    fn finalize_writes_archive_before_purge() {
        let dir = tmp_dir("archive");
        let job = test_job(&dir, false);
        let out = run_slice(&job, BIG, &mut |_, _| {});
        assert!(matches!(out, SliceOutcome::Finished(_)), "got {out:?}");

        let archive = archive_path(&job.output_dir, &job.job_id);
        let bytes = fs::read(&archive).expect("archive must exist after finish");
        assert_eq!(&bytes[0..7], b"PMTiles", "magic");
        assert_eq!(bytes[7], 3, "spec version");
        assert_eq!(
            u64::from_le_bytes(bytes[80..88].try_into().unwrap()),
            FIXTURE_TILE_FEATURES,
            "directory must hold exactly the fixture's single tile"
        );
        let data_off = u64::from_le_bytes(bytes[56..64].try_into().unwrap());
        let data_len = u64::from_le_bytes(bytes[64..72].try_into().unwrap());
        assert_eq!(
            data_off + data_len,
            bytes.len() as u64,
            "tile data section must end exactly at EOF"
        );

        // Everything temporary is gone; the archive remains.
        assert!(load_checkpoint(&job.output_dir, &job.job_id)
            .unwrap()
            .is_none());
        assert!(!index_db_path(&job.output_dir, &job.job_id).exists());
        assert!(!tile_data_tmp_path(&job.output_dir, &job.job_id).exists());
        assert!(archive.exists());
    }

    /// Finalize must yield mid-phase with a durable `pass5_last_tile`
    /// cursor and still converge to a valid archive — the kill-resume
    /// contract extended to the fifth phase.
    #[test]
    fn finalize_yields_mid_phase_with_durable_tile_cursor() {
        let dir = tmp_dir("p5cursor");
        let job = test_job(&dir, false);
        let mut saw_finalize_cursor = false;
        for _ in 0..1_000 {
            match run_slice(&job, Duration::ZERO, &mut |_, _| {}) {
                SliceOutcome::Yielded(cp) => {
                    if cp.phase == Phase::Finalize && cp.pass5_last_tile > 0 {
                        let disk = load_checkpoint(&job.output_dir, &job.job_id)
                            .unwrap()
                            .unwrap();
                        assert_eq!(
                            disk.pass5_last_tile, cp.pass5_last_tile,
                            "the tile cursor must be durable, not just returned"
                        );
                        saw_finalize_cursor = true;
                    }
                }
                SliceOutcome::Finished(_) => {
                    assert!(
                        saw_finalize_cursor,
                        "zero-budget run never exposed a finalize cursor"
                    );
                    assert!(archive_path(&job.output_dir, &job.job_id).exists());
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
