# freehike-core Loop Log

Append-only audit trail per `agentic_operating_manual.md` §1.6. One entry per chunk.

---

## P0.C1 — Workspace scaffolding + UniFFI walking skeleton

**Status:** IN PROGRESS
**Date:** 2026-07-12
**Goal:** `freehike-core` cargo workspace (crates: `compiler`, `pbf`, `terrain`, `tiles`, `ffi`)
compiles clean; `ffi` exports `compile_chunk(bbox: String) -> String` and a `ProgressCallback`
callback interface via UniFFI proc-macros; release profile `lto = true`, `opt-level = 3`;
`ffi` produces `staticlib` (iOS .a) + `cdylib` (Android .so).

**Files declared:**
- `freehike-core/Cargo.toml`
- `freehike-core/{compiler,pbf,terrain,tiles,ffi}/Cargo.toml`
- `freehike-core/{compiler,pbf,terrain,tiles,ffi}/src/lib.rs`
- `freehike-core/ffi/src/bin/uniffi_bindgen.rs` (feature-gated bindgen CLI for Phase 1)
- `freehike-core/LOOPLOG.md` (this file)
- `.gitignore` (add `freehike-core/target/`)

**Dependencies declared:** `uniffi 0.29` — explicitly approved by operator in the task
directive ("Set up the ffi crate to use uniffi"), satisfying the §1.5 dependency gate in-band.

**Proof tests (named up front):**
- `compiler::tests::{parse_valid_bbox, rejects_wrong_arity, rejects_non_numeric,
  rejects_out_of_range, rejects_inverted_bbox, stub_reports_accepted}`
- `ffi::tests::{compile_chunk_accepts_valid_bbox, compile_chunk_reports_error_on_garbage,
  progress_callback_round_trip, zero_steps_emits_nothing}`
- Ladder: L1 (`cargo test` + `clippy -D warnings` + `fmt --check`), run twice (green-lock).
  L4 cross-target builds are **out of scope** for this chunk (no iOS/Android toolchains
  verified on this machine yet — that is P1 work with its own HITL gate on the FFI surface).

**Plan:**
1. Root workspace `Cargo.toml` — members, shared package keys, `[profile.release]`
   `opt-level=3, lto=true, codegen-units=1`. Deliberately NOT `panic="abort"`: UniFFI converts
   Rust panics to typed foreign errors via unwinding; abort would turn every internal bug into
   a native crash. (Deviation from any future desire for abort noted here.)
2. `compiler` crate: `BBox::parse("w,s,e,n")` with range/ordering validation + `compile_chunk_stub`.
3. `pbf`, `terrain`, `tiles`: documented placeholder crates (Phase 3/6/5 homes respectively).
4. `ffi` crate: `uniffi::setup_scaffolding!`, `#[uniffi::export] compile_chunk`,
   `#[uniffi::export(callback_interface)] trait ProgressCallback`, `emit_test_progress`
   walking-skeleton, `engine_version()`. `crate-type = ["lib","staticlib","cdylib"]`.
5. `cargo check` → `cargo test` → `clippy -D warnings` → `fmt --check`, twice.

**Process deviation (logged per §1.1):** red-phase abbreviated — this is greenfield scaffolding,
so proof tests are authored in the same step as the stubs they test rather than run-red first.
All subsequent chunks on existing code follow strict red→green.

**Known risks:** crate names `pbf`/`tiles`/`compiler` collide with crates.io names — harmless
for path-only workspace deps, but if we ever need the crates.io `pbf`, a rename (HITL) will be
required. `uniffi 0.29` API drift vs. training data — reflection loop will adjust.

**Attempts:**
- A1: full file authoring → `cargo check --workspace` PASS first try (uniffi resolved at 0.29.5).
- A2: `cargo test --workspace` PASS — 12/12 proof tests (7 compiler, 5 ffi).
- A3: `cargo clippy --all-targets -- -D warnings` PASS; `cargo fmt --check` LINT_FAIL
  (2 over-long `write!` lines in compiler/src/lib.rs) → applied `cargo fmt` (mutating step,
  declared files only) → CLEAN.
- A4: green-lock: full L1 ladder run twice consecutively → PASS ×2.
- A5 (extra, host-side artifact proof): `cargo build --release -p ffi` → `libfreehike_ffi.a`
  (ar archive, 18.2MB debug-symbols-stripped-later) + `libfreehike_ffi.dylib` (arm64) produced;
  `nm` confirms `UNIFFI_META_*` symbols for COMPILE_CHUNK / EMIT_TEST_PROGRESS / ENGINE_VERSION /
  PROGRESSCALLBACK. True `.a for aarch64-apple-ios` / `.so for aarch64-linux-android` builds
  remain L4 work (targets/NDK not yet installed) — deferred to P1 with its FFI HITL gate.

**Outcome:** CLOSED. Steps used: 14/25 mutating. Pivots: 0.
Ladder: L1 ✅✅ (green-locked). L2/L3/L4: n/a for this chunk type per matrix.
**Note for P1:** the walking-skeleton FFI surface (`compile_chunk`, `emit_test_progress`,
`engine_version`, `ProgressCallback`) is explicitly provisional; the real surface design is a
HITL review before bindings are generated into the mobile shells.

---

## P1.C1 — Bindings, native shells, MapCompilerPlugin wiring

**Status:** CLOSED
**Date:** 2026-07-12
**Process deviation (logged):** this plan entry was written at chunk close, not before
execution — a §1.1 violation. TaskCreate was done up front; the LOOPLOG plan step was missed
in the multi-part directive. Corrective: session bootstrap checklist now explicitly pairs
TaskCreate+LOOPLOG as one atomic step.

**Operator authorizations consumed (HITL gates):** P0 commit; `cap add ios/android`;
Kotlin/JNA dependency additions; provisional FFI surface wired into shells (surface itself
still flagged provisional). One operator rejection mid-chunk (iOS cross-compile) was
clarified as accidental and re-run on instruction.

**What was done:**
1. Committed P0 baseline as `1d5c4a6` (freehike-core + .gitignore only).
2. `uniffi-bindgen` (library mode, from libfreehike_ffi.dylib) → `ffi/bindings/`
   {freehike.swift, freehikeFFI.h, freehikeFFI.modulemap, uniffi/freehike/freehike.kt}.
3. `npx cap add ios` + `npx cap add android` (Capacitor 8.4.1; iOS shell is SPM-based).
4. iOS: `MapCompilerPlugin.swift` (CAPPlugin+CAPBridgedPlugin; startJob/getEngineVersion/
   emitTestProgress; BridgeForwardingProgress → notifyListeners), `MainViewController.swift`
   (registerPluginInstance), storyboard repointed, bindings copied to App/FreeHikeFFI/,
   pbxproj hand-edited (3 sources, group, bridging header, per-SDK LIBRARY_SEARCH_PATHS,
   -lfreehike_ffi). iOS device+sim staticlibs cross-compiled (SDK-lookup warning only —
   CLT-only machine, archive outputs valid).
5. Android: Kotlin 2.1.20 gradle plugin + JNA 5.17.0@aar deps; binding copied into
   app/src/main/java/uniffi/freehike/; `MapCompilerPlugin.kt` (same 3 methods, single-thread
   executor, System.loadLibrary belt-and-braces in load()); MainActivity registerPlugin.
6. Toolchain bootstrap for validation: openjdk@21 + android-commandlinetools via brew,
   SDK platform-36/build-tools 36 via sdkmanager, local.properties.

**Verification:**
- `plutil -lint` pbxproj: OK. `swiftc -parse` × 3 Swift files: clean (full Xcode build
  deferred — no Xcode on machine, per operator instruction).
- `./gradlew assembleDebug` (JDK 21): **BUILD SUCCESSFUL** (127 tasks). dexdump confirms
  `Lcom/freehike/app/MapCompilerPlugin` + `Luniffi/freehike/*` classes in app-debug.apk.

**Known gaps (deliberate, tracked):**
- No `libfreehike_ffi.so` in jniLibs yet (no NDK/cargo-ndk installed) — runtime FFI calls on
  Android will hit the graceful UnsatisfiedLinkError log path until P1.C2 builds it.
- iOS full compile+link unverified locally (needs full Xcode).
- No JS-side `registerPlugin('MapCompiler')` wiring yet (WebView debug button = next chunk).
- Green-lock (×2) applied to Gradle? Single run only — build determinism for a first
  full-download build is dominated by dependency fetch; second run deferred to next chunk's
  entry check. Logged as a partial green-lock.

---

## P1.C2 — WebView wiring: MapCompiler plugin interface + debug UI

**Status:** IN PROGRESS
**Date:** 2026-07-12
**Goal:** Phase 1 exit criterion plumbing complete on the JS side: typed
`registerPlugin('MapCompiler')` wrapper, `compilationProgress` listener, discrete debug
button firing `startJob` + `emitTestProgress`.

**Files declared:** `src/plugins/MapCompiler.ts` (new), `src/ui/App.tsx` (footer debug UI +
listener effect). No new dependencies.

**Proof:** `tsc -b` clean, `eslint .` clean, live browser check: button renders, click on
web produces the graceful "native shell required" fallback line (full native round-trip
requires the .so — deferred to next session per operator).

**Planned deviation from directive (flagged for operator):** `startJob` typed as
`Promise<{ result: string }>` rather than `Promise<void>` — the native layer resolves the
Rust JSON envelope, and typing it away would hide the round-trip proof. `cancelJob()` is
declared as requested but documented as not-yet-implemented natively (rejects until the
Phase 7 surface lands).

**Attempts:**
- A1: `src/plugins/MapCompiler.ts` + App.tsx listener effect / debug button / footer panel
  authored. `tsc -b` PASS.
- A2: `eslint .` LINT_FAIL — ESLint swept Capacitor-generated artifacts in `android/app/build/`
  (freshly added native shells). Fix: `globalIgnores(['dist','android','ios','freehike-core'])`
  in eslint.config.js (shells have their own toolchains). → PASS.
- A3: live browser check: debug button renders in footer; click → log panel shows
  `→ startJob(11.1,47.1,11.6,47.45)` then `✕ "MapCompiler" plugin is not implemented on web —
  native shell required (web has no Rust core)`. Graceful web fallback confirmed; listener
  attach does not crash on web.

**Outcome:** CLOSED. Steps: 8/25 mutating. Pivots: 0.
**Phase 1 status: code-complete on all three layers** (Rust core ✅ committed · native
plugins ✅ Gradle-verified/swiftc-parsed · JS wiring ✅ tsc/eslint/browser-verified).
The literal exit criterion — "tap in WebView → Rust round-trip → progress event rendered in
JS" on a physical device — remains blocked on two known items: Android `.so` build
(NDK/cargo-ndk, deferred by operator to next session) and an Xcode machine for iOS. The full
path is wired and each segment is independently verified.
