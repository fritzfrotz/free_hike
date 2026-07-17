//! `thermal` — Phase 8 thermal governance (ARCHITECTURE.md P9).
//!
//! Mobile SoCs terminate background processes that keep P-cores pinned while
//! the chassis heats up. This module makes the compiler a cooperative
//! citizen: the native shells report the OS thermal level through the FFI
//! (`set_thermal_state`, any thread, any time), and every compilation loop
//! *actively listens* to that level while it runs:
//!
//! - **Nominal** — full speed: whole governed pool, unscaled slice budgets.
//! - **Fair** — shed opportunistic parallelism (half the pool width) but
//!   keep the duty cycle; a warming device finishes sooner by narrowing,
//!   not by stalling.
//! - **Serious** — the spec's voluntary downshift point: single worker, a
//!   cooling pause before every unit of work, and the slice budget honored
//!   at 50% — slices end early, so the process spends more wall-clock time
//!   idle between scheduler invocations.
//! - **Critical** — stop generating heat NOW: [`SliceGovernor::should_yield`]
//!   returns true unconditionally, so the engine checkpoints (durable and
//!   kill-safe anyway) and returns `Yielded` to the scheduler. The
//!   minimum-forward-progress guarantee still applies (one block per
//!   invocation), so even a runner that keeps re-invoking degrades to a
//!   trickle, never a livelock — but the intended runner behavior is to
//!   stop re-invoking until the OS reports recovery.
//!
//! The state lives in one global [`AtomicU8`]. Global is deliberate:
//! thermal pressure is a property of the DEVICE, not of a job, and the FFI
//! setter must work from any foreign thread without handles or locks.
//! `Relaxed` ordering suffices — the flag publishes no associated data;
//! readers only need *a recent* value, and every loop re-reads it at each
//! throttle point.
//!
//! ## Rayon under governance
//!
//! A rayon pool's width is fixed at construction — the global pool can be
//! initialized exactly once and no pool can be resized mid-flight. So the
//! governor never tries to change the pool; it changes ADMISSION:
//!
//! - One custom pool ([`pool_width`] threads), built once, capped below the
//!   core count so OS UI/audio threads keep headroom even at Nominal.
//! - [`for_each_governed`] feeds that pool in bounded WAVES. Before each
//!   wave it re-reads the thermal state and admits only
//!   [`effective_parallelism`] workers; a downshift mid-batch takes effect
//!   at the next wave boundary (at most [`WAVE_FACTOR`] items per worker
//!   away). At width 1 it bypasses rayon entirely and runs on the calling
//!   thread with the cooling pause between items.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Device thermal pressure as reported by the native shell. Mirrored 1:1
/// by the FFI crate's UniFFI enum (iOS `ProcessInfo.ThermalState` maps
/// directly; Android `PowerManager` thermal statuses collapse onto it).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThermalState {
    Nominal = 0,
    Fair = 1,
    Serious = 2,
    Critical = 3,
}

impl ThermalState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => ThermalState::Nominal,
            1 => ThermalState::Fair,
            2 => ThermalState::Serious,
            // Unreachable through the typed setter; fail COOL if it ever
            // happens — Critical still makes minimum forward progress.
            _ => ThermalState::Critical,
        }
    }

    /// Throttle policy for this state (see module docs for the rationale).
    pub fn policy(self) -> Policy {
        match self {
            ThermalState::Nominal | ThermalState::Fair => Policy {
                budget_scale: 1.0,
                cooling_pause: Duration::ZERO,
            },
            ThermalState::Serious => Policy {
                budget_scale: 0.5,
                cooling_pause: Duration::from_millis(25),
            },
            ThermalState::Critical => Policy {
                budget_scale: 0.0,
                cooling_pause: Duration::from_millis(100),
            },
        }
    }
}

/// How hard the compiler may work under a given thermal state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    /// Fraction of the caller's slice budget that is honored; 0.0 means
    /// "yield immediately" (Critical).
    pub budget_scale: f32,
    /// Sleep injected before each unit of work so the SoC gets idle cycles
    /// to shed heat mid-slice.
    pub cooling_pause: Duration,
}

static STATE: AtomicU8 = AtomicU8::new(ThermalState::Nominal as u8);

/// Publishes the OS-reported thermal level. Called from any foreign thread
/// via the FFI; takes effect at every running loop's next throttle point.
pub fn set_state(state: ThermalState) {
    STATE.store(state as u8, Ordering::Relaxed);
}

/// The most recently published thermal level (Nominal until the shell
/// reports otherwise).
pub fn current() -> ThermalState {
    ThermalState::from_u8(STATE.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------------------
// Slice throttling (the sequential passes)
// ---------------------------------------------------------------------------

/// Per-slice throttle: owns the slice clock and folds the LIVE thermal
/// policy into every deadline check. Replaces the engine's raw
/// `started.elapsed() >= budget` closures, so a state change lands at the
/// very next block boundary of a slice that is already running.
pub struct SliceGovernor {
    started: Instant,
    budget: Duration,
}

impl SliceGovernor {
    pub fn new(budget: Duration) -> Self {
        Self {
            started: Instant::now(),
            budget,
        }
    }

    /// Deadline check for the pass drivers, called before each block.
    ///
    /// - Critical → true immediately, no pause: the fastest path off the
    ///   CPU is through the checkpoint-and-yield.
    /// - Serious → sleep the cooling pause FIRST (it burns slice wall-clock,
    ///   shrinking the duty cycle on top of the 50% budget scale), then
    ///   check against the scaled budget.
    /// - Nominal/Fair → one atomic load + clock read; behavior identical to
    ///   the pre-governance engine.
    pub fn should_yield(&self) -> bool {
        let policy = current().policy();
        if policy.budget_scale <= 0.0 {
            return true;
        }
        if !policy.cooling_pause.is_zero() {
            std::thread::sleep(policy.cooling_pause);
        }
        self.started.elapsed() >= self.budget.mul_f32(policy.budget_scale)
    }
}

// ---------------------------------------------------------------------------
// Governed parallelism (the rayon side)
// ---------------------------------------------------------------------------

/// Fixed width of the governed pool: logical cores − 2, floor 1 — the
/// spec's "P-cores − 2–3" margin. Portable Rust cannot distinguish P- from
/// E-cores, so the margin comes off the logical count; the shells can
/// refine this through a dedicated FFI knob if device profiling demands it.
pub fn pool_width() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(2).max(1))
}

/// Workers a parallel section may use RIGHT NOW under the current state.
pub fn effective_parallelism() -> usize {
    match current() {
        ThermalState::Nominal => pool_width(),
        ThermalState::Fair => (pool_width() / 2).max(1),
        ThermalState::Serious | ThermalState::Critical => 1,
    }
}

fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(pool_width())
            .thread_name(|i| format!("freehike-worker-{i}"))
            .build()
            .expect("pool construction with a fixed thread count cannot fail")
    })
}

/// Items each admitted worker may claim before the wave ends and the
/// thermal state is re-read: small enough that a downshift lands within a
/// few work units, large enough to amortize the scope setup.
const WAVE_FACTOR: usize = 4;

/// Runs `work` over every item at a parallel width and duty cycle that
/// track the live thermal state (see module docs). Always completes the
/// whole batch — callers size batches to their slice budget and let
/// [`SliceGovernor`] decide when to stop submitting.
pub fn for_each_governed<T, F>(items: &[T], work: F)
where
    T: Sync,
    F: Fn(&T) + Sync,
{
    let next = AtomicUsize::new(0);
    while next.load(Ordering::Relaxed) < items.len() {
        let width = effective_parallelism();
        if width <= 1 {
            let pause = current().policy().cooling_pause;
            let Some(i) = claim(&next, items.len()) else {
                break;
            };
            work(&items[i]);
            if !pause.is_zero() {
                std::thread::sleep(pause);
            }
        } else {
            let wave_end = (next.load(Ordering::Relaxed) + width * WAVE_FACTOR).min(items.len());
            pool().scope(|scope| {
                for _ in 0..width {
                    scope.spawn(|_| {
                        while let Some(i) = claim(&next, wave_end) {
                            work(&items[i]);
                        }
                    });
                }
            });
        }
    }
}

/// Claims the next index strictly below `limit`. CAS loop rather than
/// `fetch_add`: an unconditional increment could run past the wave
/// boundary and consume indexes the next wave would then silently skip.
fn claim(next: &AtomicUsize, limit: usize) -> Option<usize> {
    let mut cur = next.load(Ordering::Relaxed);
    while cur < limit {
        match next.compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Some(cur),
            Err(actual) => cur = actual,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests (pure/table-level only — everything touching the GLOBAL state lives
// in tests/thermal_governance.rs, a separate process, so it can never race
// the engine unit tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_table_matches_the_spec() {
        for s in [ThermalState::Nominal, ThermalState::Fair] {
            assert_eq!(s.policy().budget_scale, 1.0);
            assert!(s.policy().cooling_pause.is_zero());
        }
        let serious = ThermalState::Serious.policy();
        assert_eq!(serious.budget_scale, 0.5);
        assert!(!serious.cooling_pause.is_zero());
        let critical = ThermalState::Critical.policy();
        assert_eq!(critical.budget_scale, 0.0);
    }

    #[test]
    fn severity_is_ordered() {
        assert!(ThermalState::Nominal < ThermalState::Fair);
        assert!(ThermalState::Fair < ThermalState::Serious);
        assert!(ThermalState::Serious < ThermalState::Critical);
    }

    #[test]
    fn unknown_state_bytes_fail_cool() {
        assert_eq!(ThermalState::from_u8(200), ThermalState::Critical);
    }

    #[test]
    fn pool_width_leaves_core_headroom() {
        let width = pool_width();
        assert!(width >= 1);
        if let Ok(n) = std::thread::available_parallelism() {
            assert!(width <= n.get().saturating_sub(2).max(1));
        }
    }

    #[test]
    fn claim_never_crosses_the_limit() {
        let next = AtomicUsize::new(0);
        assert_eq!(claim(&next, 2), Some(0));
        assert_eq!(claim(&next, 2), Some(1));
        assert_eq!(claim(&next, 2), None);
        assert_eq!(next.load(Ordering::Relaxed), 2, "no overshoot past limit");
    }
}
