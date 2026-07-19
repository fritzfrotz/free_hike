// SPDX-License-Identifier: Apache-2.0
//! Thermal-governance behavior over the GLOBAL thermal flag (P8.C1).
//!
//! Lives in its own integration binary so mutations of the process-wide
//! `AtomicU8` can never race the engine's unit tests (each integration
//! test file is a separate process). WITHIN this binary the harness still
//! runs tests on parallel threads, so every test serializes through
//! [`guard`], which also resets the state to Nominal on drop — even on
//! panic — so one failing test cannot poison the next.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use compiler::engine::{run_slice, JobSpec, SliceOutcome};
use compiler::thermal::{self, SliceGovernor, ThermalState};
use compiler::BBox;

static SERIAL: Mutex<()> = Mutex::new(());

struct StateGuard(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Drop for StateGuard {
    fn drop(&mut self) {
        thermal::set_state(ThermalState::Nominal);
    }
}

fn guard() -> StateGuard {
    // A panicking test poisons the mutex but leaves the () state fine.
    let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    thermal::set_state(ThermalState::Nominal);
    StateGuard(g)
}

// ---------------------------------------------------------------------------
// FFI-shaped state plumbing
// ---------------------------------------------------------------------------

#[test]
fn set_and_read_roundtrip_all_states() {
    let _g = guard();
    for s in [
        ThermalState::Fair,
        ThermalState::Serious,
        ThermalState::Critical,
        ThermalState::Nominal,
    ] {
        thermal::set_state(s);
        assert_eq!(thermal::current(), s);
    }
}

#[test]
fn default_state_is_nominal() {
    let _g = guard();
    assert_eq!(thermal::current(), ThermalState::Nominal);
}

// ---------------------------------------------------------------------------
// SliceGovernor
// ---------------------------------------------------------------------------

#[test]
fn governor_yields_immediately_under_critical() {
    let _g = guard();
    let gov = SliceGovernor::new(Duration::from_secs(300));
    assert!(!gov.should_yield(), "fresh slice under Nominal must run");
    thermal::set_state(ThermalState::Critical);
    let asked = Instant::now();
    assert!(gov.should_yield(), "Critical must force an immediate yield");
    assert!(
        asked.elapsed() < Duration::from_millis(50),
        "the Critical path must not sleep on its way to the checkpoint"
    );
}

#[test]
fn governor_halves_the_honored_budget_under_serious() {
    let _g = guard();
    // 1.2s elapsed against a 2s budget: within the Nominal budget but past
    // the Serious-scaled one (1s). Generous margins keep this deterministic
    // on a loaded machine.
    let gov = SliceGovernor::new(Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(1_200));
    assert!(!gov.should_yield(), "Nominal honors the full budget");
    thermal::set_state(ThermalState::Serious);
    let asked = Instant::now();
    assert!(
        gov.should_yield(),
        "Serious must honor only half the budget"
    );
    assert!(
        asked.elapsed() >= ThermalState::Serious.policy().cooling_pause,
        "Serious checks must inject the cooling pause"
    );
}

// ---------------------------------------------------------------------------
// Engine integration: the state machine under thermal pressure
// ---------------------------------------------------------------------------

fn test_job(tag: &str) -> JobSpec {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "freehike-thermal-test-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pbf_path = dir.join("fixture.osm.pbf");
    std::fs::write(
        &pbf_path,
        pbf::fixtures::synthetic_pbf(&[&[
            (1, 472_700_000, 113_900_000),
            (2, 472_700_100, 113_900_050),
        ]]),
    )
    .unwrap();
    JobSpec {
        job_id: format!("job-{tag}"),
        bbox: BBox::parse("11.15,47.05,11.65,47.45").unwrap(),
        min_zoom: 5,
        max_zoom: 14,
        pbf_path: pbf_path.to_string_lossy().into_owned(),
        dem_path: Some("unused_dem.tif".into()),
        output_dir: dir.to_string_lossy().into_owned(),
    }
}

#[test]
fn critical_forces_minimum_progress_yields_then_recovery_finishes() {
    let _g = guard();
    let job = test_job("critical");
    thermal::set_state(ThermalState::Critical);

    // A budget that would normally finish the whole fixture in one slice
    // must instead checkpoint after the guaranteed minimum block.
    let SliceOutcome::Yielded(cp1) = run_slice(&job, Duration::from_secs(300), &mut |_, _| {})
    else {
        panic!("Critical must force a yield even under a huge budget");
    };
    assert_eq!(cp1.blocks_done, 1, "exactly the minimum-progress block");

    // Still Critical: the trickle continues (no livelock), one block per
    // invocation — the degradation contract for a runner that keeps going.
    let SliceOutcome::Yielded(cp2) = run_slice(&job, Duration::from_secs(300), &mut |_, _| {})
    else {
        panic!("expected a second trickle yield");
    };
    assert_eq!(cp2.blocks_done, 2);

    // Cooled down: the same job resumes from its checkpoint and completes.
    thermal::set_state(ThermalState::Nominal);
    match run_slice(&job, Duration::from_secs(300), &mut |_, _| {}) {
        SliceOutcome::Finished(s) => assert!(s.blocks_total > cp2.blocks_done),
        other => panic!("expected recovery to finish the job, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Governed parallel executor
// ---------------------------------------------------------------------------

/// Tracks the peak number of concurrent `work` invocations.
struct ConcurrencyProbe {
    active: AtomicUsize,
    peak: AtomicUsize,
    ran: AtomicUsize,
}

impl ConcurrencyProbe {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            ran: AtomicUsize::new(0),
        }
    }

    fn enter(&self) {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
    }

    fn exit(&self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.ran.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn executor_runs_every_item_exactly_once_under_nominal() {
    let _g = guard();
    let items: Vec<usize> = (0..200).collect();
    let hits: Vec<AtomicUsize> = items.iter().map(|_| AtomicUsize::new(0)).collect();
    thermal::for_each_governed(&items, |&i| {
        hits[i].fetch_add(1, Ordering::SeqCst);
    });
    for (i, h) in hits.iter().enumerate() {
        assert_eq!(h.load(Ordering::SeqCst), 1, "item {i}");
    }
}

#[test]
fn executor_is_strictly_serial_under_serious() {
    let _g = guard();
    thermal::set_state(ThermalState::Serious);
    let items: Vec<usize> = (0..8).collect();
    let probe = ConcurrencyProbe::new();
    thermal::for_each_governed(&items, |_| {
        probe.enter();
        std::thread::sleep(Duration::from_millis(2));
        probe.exit();
    });
    assert_eq!(probe.ran.load(Ordering::SeqCst), items.len());
    assert_eq!(
        probe.peak.load(Ordering::SeqCst),
        1,
        "Serious must admit exactly one worker"
    );
}

#[test]
fn executor_downshifts_mid_batch_and_still_completes() {
    let _g = guard();
    let items: Vec<usize> = (0..32).collect();
    let probe = ConcurrencyProbe::new();
    thermal::for_each_governed(&items, |_| {
        probe.enter();
        // Simulate the shell reporting pressure partway through the batch;
        // the executor must narrow at the next wave boundary and finish.
        if probe.ran.load(Ordering::SeqCst) == 10 {
            thermal::set_state(ThermalState::Serious);
        }
        probe.exit();
    });
    assert_eq!(probe.ran.load(Ordering::SeqCst), items.len());
}

#[test]
fn effective_parallelism_tracks_the_state() {
    let _g = guard();
    let width = thermal::pool_width();
    assert_eq!(thermal::effective_parallelism(), width);
    thermal::set_state(ThermalState::Fair);
    assert_eq!(thermal::effective_parallelism(), (width / 2).max(1));
    thermal::set_state(ThermalState::Serious);
    assert_eq!(thermal::effective_parallelism(), 1);
    thermal::set_state(ThermalState::Critical);
    assert_eq!(thermal::effective_parallelism(), 1);
}
