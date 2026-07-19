// SPDX-License-Identifier: Apache-2.0
//! FFI-boundary tests for the P8.C1 thermal contract, in their own process
//! (integration binary) so the global thermal flag can never race the ffi
//! crate's unit tests. Tests serialize through `guard()`, which restores
//! Nominal on drop.

use std::sync::{Mutex, MutexGuard};

use freehike_ffi::{
    compile_chunk, set_thermal_state, thermal_state, CompilationStatus, CompileJob,
    ProgressCallback, ThermalState,
};

static SERIAL: Mutex<()> = Mutex::new(());

struct StateGuard(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Drop for StateGuard {
    fn drop(&mut self) {
        set_thermal_state(ThermalState::Nominal);
    }
}

fn guard() -> StateGuard {
    let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    set_thermal_state(ThermalState::Nominal);
    StateGuard(g)
}

struct Sink;
impl ProgressCallback for Sink {
    fn on_progress(&self, _percentage: f32, _status: String) {}
}

fn test_job(tag: &str) -> CompileJob {
    let dir = std::env::temp_dir().join(format!(
        "freehike-ffi-thermal-test-{tag}-{}",
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
fn thermal_state_round_trips_across_the_boundary() {
    let _g = guard();
    assert_eq!(thermal_state(), ThermalState::Nominal, "cold-start default");
    for s in [
        ThermalState::Fair,
        ThermalState::Serious,
        ThermalState::Critical,
        ThermalState::Nominal,
    ] {
        set_thermal_state(s);
        assert_eq!(thermal_state(), s);
    }
}

/// The full contract end-to-end through the FFI types: a Critical report
/// turns a would-finish budget into an immediate durable yield, and a
/// recovery report lets the SAME job resume to completion — no new
/// surface, no state round-tripping, just re-invocation.
#[test]
fn critical_yields_compile_chunk_and_recovery_resumes_it() {
    let _g = guard();
    let job = test_job("e2e");

    set_thermal_state(ThermalState::Critical);
    match compile_chunk(job.clone(), 300_000, Box::new(Sink)) {
        CompilationStatus::Yielded { checkpoint } => {
            assert_eq!(checkpoint.job_id, job.job_id);
        }
        other => panic!("Critical must yield even under a huge budget, got {other:?}"),
    }

    set_thermal_state(ThermalState::Nominal);
    let mut slices = 0u32;
    loop {
        match compile_chunk(job.clone(), 300_000, Box::new(Sink)) {
            CompilationStatus::Finished { summary } => {
                assert!(summary.blocks_total > 0);
                break;
            }
            CompilationStatus::Yielded { .. } => {
                slices += 1;
                assert!(slices < 1_000, "runaway resume loop");
            }
            CompilationStatus::FailedFatal { reason }
            | CompilationStatus::FailedTransient { reason } => panic!("resume failed: {reason}"),
        }
    }
}
