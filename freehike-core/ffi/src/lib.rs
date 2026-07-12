//! `ffi` — the UniFFI boundary crate (Layer 3 of the tri-layer bridge).
//!
//! Everything exported here is consumed by generated Swift/Kotlin bindings and
//! wrapped by the Capacitor `MapCompilerPlugin`. Per the operating manual,
//! **any change to this crate's public surface is a HITL gate** — the Swift,
//! Kotlin, and JS layers all break together if this drifts.
//!
//! Panic safety: UniFFI's generated scaffolding wraps every exported function
//! and converts Rust panics into foreign-language errors via unwinding, which
//! is why the workspace release profile does NOT set `panic = "abort"`.

use compiler::BBox;

uniffi::setup_scaffolding!("freehike");

/// Progress events emitted from the Rust core to the native (Swift/Kotlin)
/// layer, which forwards them to the WebView as Capacitor
/// `compilationProgress` events. Implemented on the foreign side.
#[uniffi::export(callback_interface)]
pub trait ProgressCallback: Send + Sync {
    /// `percentage` is 0.0-100.0; `status` is a human-readable phase label
    /// (e.g. "pass1: indexing nodes").
    fn on_progress(&self, percentage: f32, status: String);
}

/// Walking-skeleton compile entry point.
///
/// Accepts a `"west,south,east,north"` WGS84 bbox string and returns a JSON
/// status envelope. Phase 7 replaces the body with the real chunked,
/// checkpointed state machine (`compile_chunk(budget) -> Finished | Yielded`);
/// the signature here is deliberately primitive (String -> String) so the
/// bridge plumbing can be proven end-to-end before the real surface is
/// designed and HITL-reviewed.
#[uniffi::export]
pub fn compile_chunk(bbox: String) -> String {
    match BBox::parse(&bbox) {
        Ok(b) => compiler::compile_chunk_stub(&b),
        Err(e) => format!(r#"{{"status":"error","message":"{e}"}}"#),
    }
}

/// Proves the foreign-callback path crosses the bridge: emits `steps`
/// synthetic progress ticks through the callback and returns how many were
/// sent. Wired to a hidden debug button in the WebView during Phase 1.
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

/// Version string for plugin smoke tests ("is the Rust core actually loaded?").
#[uniffi::export]
pub fn engine_version() -> String {
    format!("freehike-core {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn compile_chunk_accepts_valid_bbox() {
        let out = compile_chunk("11.15,47.05,11.65,47.45".to_string());
        assert!(out.contains(r#""status":"accepted""#), "got: {out}");
    }

    #[test]
    fn compile_chunk_reports_error_on_garbage() {
        let out = compile_chunk("the alps".to_string());
        assert!(out.contains(r#""status":"error""#), "got: {out}");
    }

    /// Rust-side implementation of the foreign trait, capturing every call.
    struct Recorder {
        calls: Mutex<Vec<(f32, String)>>,
    }

    impl ProgressCallback for Recorder {
        fn on_progress(&self, percentage: f32, status: String) {
            self.calls.lock().unwrap().push((percentage, status));
        }
    }

    #[test]
    fn progress_callback_round_trip() {
        // Ownership moves into the callee (mirroring the foreign-handle
        // semantics of the generated bindings), so this test asserts on the
        // returned count; the shared-Arc variant below inspects the payloads.
        let recorder = Box::new(Recorder {
            calls: Mutex::new(Vec::new()),
        });
        let sent = emit_test_progress(recorder, 4);
        assert_eq!(sent, 4);
    }

    #[test]
    fn progress_callback_receives_monotonic_percentages() {
        use std::sync::Arc;

        struct SharedRecorder(Arc<Mutex<Vec<f32>>>);
        impl ProgressCallback for SharedRecorder {
            fn on_progress(&self, percentage: f32, _status: String) {
                self.0.lock().unwrap().push(percentage);
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let cb = Box::new(SharedRecorder(Arc::clone(&seen)));
        emit_test_progress(cb, 5);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 5);
        assert!(
            seen.windows(2).all(|w| w[0] < w[1]),
            "not monotonic: {seen:?}"
        );
        assert!((seen.last().unwrap() - 100.0).abs() < f32::EPSILON);
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
