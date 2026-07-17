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

**Outcome:** CLOSED. tsc/eslint clean; web-fallback path verified live in browser
(startJob line + graceful UNIMPLEMENTED error rendered in the debug panel). Committed as
`62a4eff` (native) + `29388d7` (ui/docs) on operator instruction.

---

## P1.C3 — Android .so + end-to-end emulator verification

**Status:** IN PROGRESS
**Date:** 2026-07-13
**Goal:** Phase 1 exit criterion made literal on Android: libfreehike_ffi.so built for
arm64-v8a into jniLibs, app boots on an emulator, Debug Native Compile tap produces a
Rust round-trip + progress events observable in logcat/UI.

**Files declared:** `android/app/src/main/jniLibs/arm64-v8a/libfreehike_ffi.so` (build
artifact — gitignore decision at close), `dist/` (vite rebuild), LOOPLOG.

**Toolchain installs authorized by directive:** cargo-ndk, Android NDK (via sdkmanager —
not present at ~/Library/Android/sdk/ndk despite directive's assumption), rustup android
targets ×4, emulator package + arm64-v8a system image + AVD (none exist on this machine).

**Proof:**
1. `file`/`llvm-nm` on the .so: ELF aarch64 with uniffi_* exports.
2. Logcat at app boot: MapCompilerPlugin load() logs "libfreehike_ffi.so loaded
   (freehike-core 0.1.0)" — native-layer FFI round-trip proof.
3. adb input tap on Debug Native Compile → UI log panel shows Rust JSON envelope +
   5 progress ticks; logcat shows Capacitor plugin calls + notifyListeners events.

**Plan:** rustup targets → cargo install cargo-ndk → sdkmanager ndk/emulator/system-image
→ avdmanager create → cargo ndk build into jniLibs → npm run build → cap sync android →
boot emulator (background) → install+launch → logcat/screenshot/tap verification.

**Attempts:**
- A1: rustup android targets ×4 — OK. cargo-ndk 4.1.2 installed — OK.
- A2: NDK 27.2.12479018 via sdkmanager — OK (directive assumed NDK present; it was not).
- A3: `cargo ndk -t arm64-v8a … -p ffi` → jniLibs/arm64-v8a/libfreehike_ffi.so (527KB ELF
  aarch64; llvm-nm confirms uniffi_* exports) — OK, first try.
- A4: avdmanager (brew path) → "Package path is not valid" — PIVOT: installed
  `cmdline-tools;latest` INTO the sdk root; its avdmanager created the AVD cleanly
  (pixel_7, android-36 google_apis arm64-v8a).
- A5: npm run build (dist was stale — predated debug button) + cap sync — OK.
  Emulator booted in ~25s. gradle installDebug — OK.
- A6: BOOT PROOF in logcat: `nativeloader: Load …libfreehike_ffi.so… : ok` then
  `MapCompilerPlugin: libfreehike_ffi.so loaded (freehike-core 0.1.0)` — the version
  string is a live JNA→Rust engineVersion() round-trip at plugin load.
- A7: first button tap missed (page settled between screenshot and tap; landed on a link
  that opened Chrome) — PIVOT: re-front app, screenshot immediately before tap, no
  intervening gestures.
- A8: TAP PROOF: logcat shows `To native: MapCompiler.startJob {"bbox":"11.1,47.1,11.6,47.45"}`
  → then `emitTestProgress {"steps":5}` (only reachable in JS after startJob resolved) →
  5× `Notifying listeners for event compilationProgress`. UI screenshot shows the panel:
  startJob line, Rust envelope `{"status":"accepted","engine":"freehike-core 0.1.0",…`,
  and ticks 20/40/60/80% (100% below fold; logcat shows all 5).

**Outcome:** CLOSED. Pivots: 2. **Phase 1 exit criterion met literally on Android:**
tap in WebView → Rust round-trip → progress events rendered in JS.
jniLibs/ gitignored (regenerable build artifact; command documented in .gitignore).
Emulator shut down cleanly. iOS device-side demo still pending a full-Xcode machine.
Uncommitted: .gitignore line + dist rebuild + this log — awaiting operator.

---

## P2.C0 — Production FFI contract (suspendable state machine surface, v1)

**Status:** IN PROGRESS
**Date:** 2026-07-13
**HITL gate:** FFI public surface redesign — **operator-initiated and specified in the
directive**, satisfying §1.5 in-band. Surface v1 = CompileJob / CompilationStatus /
CheckpointState / CompilePhase / compile_chunk(job, budget_ms, callback).

**Goal:** the Phase 7-shaped contract, implemented today over a simulated-but-real slice
engine: budget-bounded execution, durable atomic checkpoints, resume-by-job-identity,
minimum-forward-progress guarantee. Real PBF/terrain pipelines (Phases 3-6) later replace
the simulated block work behind the same contract.

**Files declared:** `freehike-core/compiler/src/lib.rs` (module split),
`freehike-core/compiler/src/engine.rs` (new — slice engine + checkpoint persistence),
`freehike-core/ffi/src/lib.rs` (new surface), `freehike-core/ffi/bindings/*` (regen),
LOOPLOG. No new dependencies (checkpoint file is std-only; redb replaces it in Phase 3/7
behind the same engine API).

**Proof tests (named up front):**
- compiler::engine: `large_budget_finishes_and_purges_checkpoint`,
  `tiny_budget_yields_with_checkpoint_file`, `resume_continues_not_restarts`,
  `sliced_run_matches_single_run` (determinism), `zero_budget_still_makes_progress`
  (livelock guard), `corrupted_checkpoint_fails`, `invalid_output_dir_fails`,
  `phase_transitions_in_order`, `progress_is_monotonic_across_slices`,
  `dem_none_skips_terrain_phase`.
- ffi: `compile_chunk_finishes_with_large_budget`, `compile_chunk_yields_with_tiny_budget`,
  `yielded_checkpoint_round_trips_via_query`, `failed_on_garbage_bbox`,
  `callback_receives_phase_labels`.
- Ladder: L1 ×2 (green-lock) + bindings regen compiles (uniffi-bindgen runs clean).

**Design decisions to flag to operator (deviations/extensions):**
1. `Finished` carries a `CompileSummary` record (blocks, bytes, duration) — spec said bare
   Finished; the summary feeds the completion UI + L2 assertions. Vetoable.
2. Resume is by job identity: the runner calls compile_chunk again with the same
   CompileJob; the engine reloads its own durable checkpoint. CheckpointState returned in
   `Yielded` is informational (UI/telemetry), never fed back — the foreign layer cannot
   corrupt resume state, and iOS may kill the process between slices anyway, so disk is
   the only trustworthy carrier.
3. Added `query_checkpoint(job_id, output_dir)` + `purge_job(job_id, output_dir)` — the
   runner needs cold-start resume detection and the JS surface already declares cancelJob.
4. `emit_test_progress` + `engine_version` retained (device smoke path).
5. Known downstream break (next chunk, not this one): Kotlin/Swift plugins + TS interface
   still target the walking-skeleton surface. Repo stays runtime-consistent because
   jniLibs/.so + embedded Kotlin binding are the OLD pair; regenerated bindings land in
   ffi/bindings/ only. P2.C1 adapts the three shells + re-verifies on emulator.

**Attempts:**
- A1: engine.rs (slice engine + atomic checkpoint persistence + 10 tests) + module split
  + new ffi/src/lib.rs (5 records/enums, 4 exports, 7 tests). One self-caught test bug
  fixed pre-compile (phase-label extraction assumed a colon in every label).
- A2: `cargo test --workspace` → 24/24 PASS first compile. `cargo fmt` applied.
- A3: green-lock ×2: tests + clippy -D warnings + fmt all clean, twice.
- A4: bindings regenerated from new dylib — Swift: `CompileJob` struct /
  `CompilationStatus` enum / `compileChunk(job:budgetMs:callback:)` /
  `queryCheckpoint` / `purgeJob`; Kotlin: data class / sealed class / same functions.
  (ktlint-not-installed warning is cosmetic, as in P1.C1.)

**Outcome:** CLOSED. Steps ~11/25. Pivots: 0.
Ladder: L1 ✅✅ (green-locked) · bindings-regen ✅ · full L4 cross-targets deferred to
P2.C1 (shell adaptation chunk, where the .so is rebuilt anyway).
Surface v1 + deviations APPROVED by operator 2026-07-13.

---

## P2.C1 — Shell realignment to Surface v1 + emulator re-verification

**Status:** IN PROGRESS
**Date:** 2026-07-13
**Budget:** 40 (declared up front: three shells + device verification)
**Goal:** Kotlin/Swift/TS plugins consume the v1 bindings; native layers run the
budget-yield loop (re-invoke compile_chunk while Yielded, honor cancel between slices);
end-to-end emulator proof shows multi-slice yield → resume → Finished.

**Files declared:** android/.../uniffi/freehike/freehike.kt (binding copy),
android/.../MapCompilerPlugin.kt, ios/App/App/FreeHikeFFI/* (binding copies),
ios/App/App/MapCompilerPlugin.swift, src/plugins/MapCompiler.ts, src/ui/App.tsx (debug
handler), jniLibs .so rebuild (gitignored), LOOPLOG.

**Proof:**
1. tsc -b + eslint clean; swiftc -parse clean; gradle assembleDebug clean.
2. Emulator logcat: N>1 "slice yielded" lines for one startJob call (budget-yield loop
   working), full progress stream, final Finished envelope resolved to JS.
3. UI screenshot: debug panel shows slice yields + finished summary.

**Design notes:** startJob(bbox, budgetMs?) drives the loop natively on the plugin's
single-thread lane; JS debug button passes budgetMs=25 to force multiple yields of the
~124ms simulated job. cancelJob sets an atomic flag checked between slices → purge_job →
resolves pending startJob with status "cancelled". query_checkpoint exposure through the
plugins deferred (logged) — not needed for this chunk's proof.

**Attempts:**
- A1: P2.C0 committed as `3753347` (operator-instructed).
- A2: bindings copied to both shells; MapCompilerPlugin.kt + .swift rewritten with the
  budget-yield loop + cancel; MapCompiler.ts v1 interface; App.tsx debug handler updated
  (budgetMs=25, per-slice status listener).
- A3: gates first try: tsc CLEAN, eslint CLEAN, swiftc -parse ×2 CLEAN. .so rebuilt for
  v1 (checksums now match embedded binding), npm build + cap sync, gradle installDebug
  BUILD SUCCESSFUL, plugin load line OK on emulator.
- A4: tap targeting: two misses (WebView re-render resets scroll between screenshot and
  tap — the map-init spinner re-render is the culprit; filed as UI nit). PIVOT →
  uiautomator bounds lookup + atomic tap; one shell-arithmetic bug in bounds parsing
  fixed ("][ " collapsed by tr).
- A5: EVIDENCE (one tap, budgetMs=25): logcat — 62 compilationProgress events (= exactly
  blocks_total), 16 compilationStatus events (15 yielded + 1 finished);
  `slice 1 yielded: phase=PASS1_NODES block=6` → `slice 2 ... block=11` (durable
  checkpoint resume between invocations, on device); terminal
  `job debug-compile finished in 16 slices: 62 blocks, 253952 bytes`.
  UI accessibility dump — "◌ slice 15: yielded", "◌ slice 16: finished",
  "← finished in 16 slices — 62 blocks, 253952 bytes" rendered in the React panel.

**Outcome:** CLOSED. Pivots: 1 (tap targeting). Steps ~30/40.
Downstream break from P2.C0 fully resolved: all three shells consume Surface v1; the
budget-yield loop and Yielded-state handling verified end-to-end on the emulator.
Deferred: iOS full build (needs Xcode machine), query_checkpoint plugin exposure,
scroll-reset UI nit during map init. Committed as `7de3bb7` on operator instruction.

---

## P2.C2 — Kill-resume torture test (process-death invariant)

**Status:** IN PROGRESS
**Date:** 2026-07-13
**Goal:** prove the design invariant: SIGKILL mid-job loses nothing. Sequence: start
multi-slice job → `am force-stop` mid-run → relaunch → queryJob via JS bridge shows the
durable checkpoint → startJob same jobId resumes from it → Finished summary shows 62
blocks / 253,952 bytes with run-2 progress events < 62 (no duplication).

**Files declared:** MapCompilerPlugin.kt/.swift (queryJob method + logcat evidence line),
src/plugins/MapCompiler.ts (queryJob), src/ui/App.tsx (handler queries before start),
LOOPLOG. No FFI change (query_checkpoint already in Surface v1 → no .so rebuild).

**Proof:** logcat chain across two process lifetimes + UI panel dump.

**Attempts:**
- A1: queryJob exposed on Kotlin + Swift (+ TS interface + App.tsx cold-start query
  before startJob). tsc/eslint/swiftc/gradle all clean; .so unchanged (query_checkpoint
  already in Surface v1).
- A2-A4: repeated UI-tap attempts to trigger + kill mid-job FAILED. Two compounding
  problems diagnosed: (a) the map-mount re-render resets WebView scroll to top (the nit
  flagged in P2.C1), so coordinate taps landed on empty space; (b) the simulated job at
  BLOCK_WORK=2ms finishes in <0.5s — faster than a shell-timed `am force-stop` can land,
  so every kill hit an already-Finished-and-purged job ("No such file" = correct purge).
- A5 PIVOT (tap targeting → CDP): abandoned coordinate taps. Enabled via the debug
  build's WebView devtools socket (`webview_devtools_remote_<pid>`), `adb forward`, and a
  hand-rolled CDP websocket driver (scratchpad/cdp.py) to call
  `Capacitor.Plugins.MapCompiler.*` directly in the page context — deterministic, no taps.
- A6 PIVOT (job too fast → instrumentation): bumped BLOCK_WORK 2ms→200ms (≈12s job) to
  open a wide, reliable kill window. Rebuilt .so. **Reverted to 2ms + rebuilt + retested
  after the proof; engine.rs diff vs commit is empty.**

**EVIDENCE (deterministic, two process lifetimes):**
- RUN 1: pre-start `queryJob` → {found:false}. Fired startJob(jobId=torture-1). After 2s,
  `am force-stop` (PID confirmed DEAD). Last slice logged:
  `slice 4 yielded: phase=PASS1_NODES block=8`. **Surviving on-disk checkpoint**
  (`run-as cat files/map_jobs/torture-1.checkpoint`): version=1 / pass1_nodes /
  next_block=8 / pbf_byte_offset=65536 / bytes_written=32768 — byte-exact match to the
  last slice, i.e. the fsync+atomic-rename write survived SIGKILL.
- RUN 2 (brand-new PID 5438): `queryJob` via JS bridge →
  {found:true, phase:PASS1_NODES, nextBlock:8, bytesWritten:32768}. Resume startJob(same
  jobId) → status:finished, blocksTotal:62, bytesWritten:253952 (identical to a clean
  run), **progressEvents:54**, first="pass1: indexing nodes (9/62)", last="finalizing
  archive (62/62)". **54 (run 2) + 8 (run 1 pre-kill) = 62, zero duplication; resume began
  at block 9, never re-ran 0-7.**

**Outcome:** CLOSED. Pivots: 2 (tap→CDP, speed→instrumentation). Invariant PROVEN on
device: SIGKILL mid-compile loses nothing; resume is exact and non-duplicating.
Instrumentation reverted. Emulator down. Uncommitted (P2.C2 diff): 4 shell/UI files +
this log. New UI nit corroborated (scroll-reset on map mount) — worth fixing, tracked.

---

## P2.C3 — Hostile-mirror native fetcher (Phase 2 scaffolding)

**Status:** IN PROGRESS
**Date:** 2026-07-13
**Goal:** new `fetcher` crate: reqwest+rustls resumable Range downloads +
magic-byte validation, encoding the Geofabrik-HTML-redirect lesson into Rust. Clean
workspace compile + L1 tests. Wiring into the compile pipeline is a later chunk.

**Files declared:** freehike-core/fetcher/Cargo.toml (new), fetcher/src/lib.rs (new),
freehike-core/Cargo.toml (add member + workspace dep), LOOPLOG.

**Dependencies (directive-approved, satisfies §1.5 in-band):** reqwest (default-features
off, features: rustls-tls, stream) — NO native-tls, per directive's OpenSSL-avoidance
requirement. Plus tokio (rt + macros for the async client), futures-util (stream). All
pure-Rust TLS → clean aarch64-android/ios cross-compile.

**Proof tests (named up front, no network in L1):**
- magic-byte unit tests: `pbf_osmheader_accepted`, `pbf_html_redirect_rejected`,
  `tiff_little_endian_accepted`, `tiff_big_endian_accepted`, `tiff_garbage_rejected`,
  `empty_payload_rejected`, `truncated_header_rejected`.
- Range/resume logic tested via a `ResumePlan` pure helper (existing-bytes → Range header
  + whether to append): `fresh_download_no_range`, `partial_resumes_from_offset`,
  `complete_file_skips_download`.
- Full network download is an integration test gated behind `--ignored` (no live
  Geofabrik hit in CI); L1 covers the validation + resume math deterministically.

**Design:** `Validator` enum (OsmPbf | Tiff) with `validate(&[u8]) -> Result<(),FetchError>`
reading only the leading bytes. PBF check: parse the 4-byte BE BlobHeader length, bounds-
check it, then confirm the HeaderBlock blobtype string is "OSMHeader" (the exact anti-
HTML-redirect assertion). TIFF: first 4 bytes ∈ {II*\0, MM\0*}. Download flow: stat local
partial → HTTP Range: bytes=N- → validate first bytes of the assembled head before
trusting → stream to disk with append. FetchError is a plain enum (UniFFI-ready later).

**Attempts:**
- A1: grounded validators on the REAL fixtures (xxd): innsbruck.osm.pbf head =
  `00 00 00 0d 0a 09 "OSMHeader"`; innsbruck_dem.tif head = `49 49 2a 00` (II*\0). PBF
  test vector is byte-for-byte from the fixture.
- A2: fetcher/Cargo.toml (reqwest default-features=off + rustls-tls,stream; tokio;
  futures-util) + workspace member/dep. `cargo build -p fetcher` clean first try.
- A3: `cargo test -p fetcher` → 13/13 unit tests pass, live_download correctly `ignored`.
- A4: green-lock ×2 (workspace tests + clippy -D warnings + fmt) — all clean.
- A5 (cross-compile, the point of rustls): `cargo ndk -t arm64-v8a build -p fetcher`
  → CLEAN (libfetcher.rlib built). `cargo tree` confirms **zero openssl / native-tls**;
  pure rustls 0.23 + ring 0.17. iOS (`aarch64-apple-ios`) FAILED — but the error is
  `ring` build script: `xcrun: SDK "iphoneos" cannot be located`, i.e. the SAME
  no-full-Xcode env limitation blocking the rest of the iOS story, NOT an OpenSSL/rustls
  problem. Resolves on any Xcode machine; the dependency choice is validated by the clean
  Android build + openssl-free tree.

**Outcome:** CLOSED. Pivots: 0. Fetcher scaffolded, compiles clean (host + Android),
13 L1 tests green, hostile-mirror + resume invariants covered deterministically. Not yet
wired into the compile pipeline (later chunk). iOS cross-build deferred to Xcode machine.
Uncommitted (P2.C3): new fetcher crate + workspace manifest + LOOPLOG.

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

---

## P2.C4 — Phase 2 hygiene sweep (pre-Phase-3 foundation check)

**Status:** CLOSED
**Date:** 2026-07-13
**Goal:** strict workspace hygiene before Phase 3 (out-of-core indexing): fmt, clippy,
debug-instrumentation audit, dependency audit, clean `cargo check`, commit.

**Findings & actions:**
- `cargo fmt --all`: zero diffs — tree already normalized.
- `cargo clippy --workspace --all-targets` (fresh, after `cargo clean -p` of all six
  members, and again with `-D warnings`): **zero warnings**. Nothing to fix.
- Debug-instrumentation audit of engine.rs + fetcher: the P2.C2 torture-test bump
  (BLOCK_WORK 2ms→200ms) was already reverted in-session (see P2.C2 A6); engine.rs
  carries only the canonical 2ms simulated-work constant, which is the documented
  simulation model, not instrumentation. fetcher has no sleeps/mocks; its live network
  test stays `#[ignore]`d. No mock budgets anywhere (grep-verified across all crates
  and the three shells).
- Dependency audit: fetcher's `[dev-dependencies]` tokio was dangling (duplicated the
  main dep + an unused `test-util` feature). Split correctly: lib deps = `fs`,`io-util`
  only (the lib never owns a runtime); dev deps = `rt-multi-thread`,`macros` for
  `#[tokio::test]`. All other manifests clean; workspace.dependencies entries for
  pbf/terrain/tiles/fetcher retained as inert version pins for Phases 3-6.
- Bonus catch: root `.gitignore` had two entries fused into one line
  (`android/app/build/offline_sandbox/gdal/`), so the vendored 15MB
  `offline_sandbox/gdal/` checkout was NOT ignored despite commit `aad9f0d`'s intent.
  Split into `android/app/build/` + `offline_sandbox/gdal/`; `git check-ignore` now
  confirms both.

**Verification:** `cargo check --workspace --all-targets` clean;
`cargo test --workspace` 37/37 pass (17 compiler + 13 fetcher + 7 ffi, 1 ignored
live-network test); clippy 0 warnings after the manifest change.

**Outcome:** CLOSED. Pivots: 0. Foundation verified flawless for Phase 3. Commit also
carries the previously-uncommitted P2.C2 shell diff (queryJob) and P2.C3 fetcher crate
per operator instruction to stage all changes.

---

## P3.C1 — Out-of-core indexing engine (mmap reader + bounded-cache redb index)

**Status:** CLOSED
**Date:** 2026-07-13
**Goal:** Phase 3 opener in the `pbf` crate: zero-copy mmap PBF reader + redb
coordinate index with a hard-capped page cache (50MB RAM ceiling), chunked batch
insertion, unit-tested create/batch/read.

**Files declared:** pbf/src/lib.rs (rewritten from Phase-0 stub), pbf/Cargo.toml,
freehike-core/Cargo.toml (workspace deps), Cargo.lock, LOOPLOG.

**Dependencies (named in the Phase 3 directive, satisfies §1.5 in-band):**
memmap2 0.9 (zero-copy read-only maps) + redb 4.1 (pure-Rust embedded B-tree,
`Builder::set_cache_size` is the ceiling enforcement point). redb 4.x API drift
from 2.x handled: `begin_read` now lives on the `ReadableDatabase` trait.

**Design:**
- `PbfMmap`: read-only `memmap2::Mmap` wrapper. One documented `unsafe` block
  (map creation) with the file-immutability invariant stated (fetcher validates
  then never mutates). Empty files rejected pre-map. `slice(offset,len)` is
  overflow- and bounds-checked for hostile BlobHeader length fields. Mapped pages
  are clean/file-backed → OS-evictable cache, NOT heap; they don't count against
  the ceiling (rationale in module docs).
- RAM budget: `RAM_CEILING_BYTES=50MB` (project constant) with
  `REDB_CACHE_BYTES=32MB` — deliberately less than the ceiling; the remainder is
  headroom for decode buffers, in-flight batch, FFI/shell overhead. Both
  invariants enforced at COMPILE TIME via `const _: () = assert!(...)` (a budget
  violation is now a build failure, stronger than the unit test it replaced
  after clippy's assertions-on-constants lint flagged the runtime version).
- Table: `TableDefinition<u64,(f64,f64)>` named "Coordinates" — node ID →
  Web Mercator meters. `web_mercator(lon,lat)` helper clamps lat to ±85.0511°
  so polar garbage can never inject ±inf into the index.
- `insert_coords_batched(&Database, iter, batch_size)`: one write txn (one
  fsync) per chunk, default 10,000 rows. `&Database` is Sync; redb serializes
  writers → thread-safe by construction, callers interleave at chunk
  boundaries. Committed chunks survive an error; only the in-flight chunk rolls
  back (checkpoint-style resume semantics). Trailing empty txn aborts (no
  phantom table creation on empty input).

**Proof tests (11):** mmap zero-copy equality vs file bytes, bounds-checked
slice (past-EOF / offset-overflow / hostile-length → None), empty-file
rejected, missing-file → Io; 25k-node batch insert at default chunk (2 full +
1 short commit) with spot-reads across chunk boundaries + absent-key None;
short-final-chunk and exact-multiple chunking; empty iterator no-op (no table
side effect); zero batch_size rejected; reopen persistence; two-thread
concurrent batched inserts (disjoint ranges, 20k rows, all readable);
Web Mercator known values (origin, antimeridian x=20037508.34, Innsbruck band,
pole clamp finite).

**Verification:** `cargo test -p pbf` 11/11 → workspace 48/48 green; fmt clean;
`clippy --workspace --all-targets -D warnings` clean (one iteration: const
assertions replaced the constant-value test); `cargo check --workspace
--all-targets` clean; **Android cross-compile clean** (`cargo ndk -t arm64-v8a
build -p pbf` — redb is pure Rust, memmap2/libc fine on aarch64-android).

**Outcome:** CLOSED. Pivots: 0. The out-of-core substrate is in place; next
chunk wires real PBF PrimitiveBlock decoding (Pass 1) through `PbfMmap` into
the Coordinates index under the same budget.

---

## P3.C2 — PBF block decoder + Pass 1 node extraction (suspendable)

**Status:** CLOSED
**Date:** 2026-07-13
**Goal:** decode `.osm.pbf` directly from `PbfMmap` and run Pass 1: block
scanning, zlib blob decompression, DenseNodes delta-decoding, Web Mercator
projection, batched redb insertion — suspendable at block boundaries for the
Phase 2 FFI budget loop.

**Files declared:** pbf/src/proto.rs (new), pbf/src/scan.rs (new),
pbf/src/lib.rs (module wiring), pbf/Cargo.toml, workspace Cargo.toml + lock,
LOOPLOG.

**Dependencies (named in directive):** prost 0.14 + miniz_oxide 0.9. Design
decision: prost message structs are HAND-DERIVED (proto.rs) rather than
prost-build-generated — no protoc binary or build.rs in the aarch64 cross-
compile path, for a wire format frozen since 2011. Declared subset only
(BlobHeader/Blob/PrimitiiveBlock/PrimitiveGroup/Node/DenseNodes); prost skips
undeclared tags (ways, relations, denseinfo, keys_vals) for free.

**Design:**
- `BlockScanner`: cursor over `[u32 BE len][BlobHeader][Blob]` framing. Every
  length field bounds-checked against the map (checked adds; hostile lengths
  → typed `corrupted PBF at byte N` errors, never a panic/OOB). Caps:
  BlobHeader < 64KiB (spec MUST), blob + decompressed payload ≤ 16MiB (spec
  SHOULD, enforced because the inflate buffer is Pass 1's largest transient
  allocation and must fit in the ceiling headroom). zlib-bomb-proof:
  `decompress_to_vec_zlib_with_limit(raw_size)` + exact-size verification.
  raw + zlib_data encodings supported; lzma/bzip2/lz4/zstd rejected loudly;
  unknown blob TYPES skipped per spec (Header/Data/Skipped enum).
- Delta decoding: DenseNodes parallel arrays (id/lat/lon each sint64
  delta-coded, first relative to 0); running accumulators; parallel-array
  length mismatch and negative node IDs are hard errors. Plain (non-dense)
  Node messages also handled (silent drop would surface as Pass-2 lookup
  failures far from the cause). Full spec formula incl. granularity (default
  100) and lat/lon offsets → degrees → `web_mercator` → redb.
- `run_pass1_slice(pbf, db, resume_offset, should_yield)`: the suspend
  contract. Yield checked at block boundaries after ≥1 block (engine's
  min-progress rule); nodes buffered and flushed through
  `insert_coords_batched` at DEFAULT_BATCH_SIZE, with a final flush BEFORE
  the offset is reported — the checkpointed offset never runs ahead of
  durable data; crash between commit and checkpoint is harmless (upserts).

**Proof tests (7 new L1 + 1 ignored L2):** full-run over synthetic 2-block
file (backward ID deltas, southern/western hemisphere, antimeridian,
pole-clamp) with exact coordinate read-back; yield-every-block slicing (4
slices, per-slice sums == distinct count → no re-scan duplication, offsets
monotonic, last-block flush proven); plain nodes + granularity=1000 +
offsets formula; unknown block type skipped; hostile framing table
(HTML-as-PBF, truncated prefix/blob, zero header len) all rejected with
nothing committed; unsupported encoding + raw_size lie rejected; mismatched
dense arrays rejected; resume-past-EOF rejected / resume-at-EOF = finished.

**L2 EVIDENCE (real data):** `pass1_real_innsbruck_extract` (ignored, like
fetcher's live test) over the real 19.5MB innsbruck.osm.pbf fixture:
**1,900,652 nodes / 265 blocks / 67 forced-yield slices in ~1.9s (release,
host)**, per-slice sums == final coord_count (zero duplication), final
offset == file length. `/usr/bin/time -l`: 75MB max RSS TOTAL for the test
process — upper bound including the 19.5MB mmap'd clean pages (evictable;
counted in RSS while resident) + test harness; heap-side budget holds.
Precise on-device RSS measurement belongs to the Phase 3 torture chunk.

**Verification:** fmt clean; clippy --workspace --all-targets -D warnings
clean (2 iterations: EOF-slice min-progress assertion refined, double-
comparison lint); workspace 56/56 green (2 ignored L2s); Android
cross-compile clean (`cargo ndk -t arm64-v8a build -p pbf`).

**Outcome:** CLOSED. Pivots: 0. Pass 1 is real end-to-end: mmap → framing →
inflate → delta-decode → project → bounded-cache redb, suspendable at block
boundaries under the Phase 2 contract. Next: wire into
`compiler::engine::Phase::Pass1Nodes` (replace simulated `process_block`),
then Pass 2 (ways).

---

## P3.C2b — Alignment audit vs /research architecture specs

**Status:** CLOSED
**Date:** 2026-07-13
**Trigger:** operator instruction to verify the P3.C2 implementation against
research/ (md master plan + Blueprint/Feasibility PDFs) — audit ran post-hoc
since P3.C2 was already implemented when the instruction arrived.

**Alignment matrix (Blueprint "Two-Pass Parsing" + md Phase 3):**
- mmap read-only, clean pages invisible to Jetsam → PbfMmap, and the md's
  "dirty RSS:anon < 50MB" phrasing CONFIRMS the ceiling is dirty-anon heap,
  matching P3.C2's RSS-accounting rationale. ✓
- `[u32 BE][BlobHeader][Blob]` framing, zlib payloads → BlockScanner. ✓
  (Blueprint mentions lz4 as possible encoding; we reject non-raw/zlib with a
  typed error — acceptable-loud posture, revisit only if a real mirror ships
  lz4.)
- ZigZag delta decoding of DenseNodes → extract_node_coords. ✓
- NodeID → coords in redb, "coarse batched commits, never per node" (R2) →
  insert_coords_batched @ 10k/commit (coarser than one PBF block). ✓
- redb keyed by 64-bit Node ID storing **WebMercator(x,y)** per the md
  diagram + P3.C1 directive. (Blueprint prose says "Longitude and Latitude";
  md supersedes — projecting once in Pass 1 beats projecting per way-vertex
  in Pass 2. Discrepancy noted, resolved in favor of md/directive.)
- Parser library: Blueprint suggests osmpbf/rosm_pbf_reader ("such as");
  operator directive specified prost + hand framing → hand-derived proto.rs,
  no protoc in cross-compile path. Directive supersedes suggestion. ✓
- Two-pass architecture, Pass 2 redb lookups, materialize-then-drop → shape
  already anticipated (get_coord, scanner re-walk). Pass 2 is Phase 4 work. ✓
- Desktop-first guiding rule → real innsbruck.osm.pbf integration test. ✓

**Gap found & closed: StringTable semantic pre-filter** (Blueprint "Semantic
Filtering"; md Phase 3 line). Implemented `RELEVANT_TAG_KEYS`
(highway/sac_scale/waterway/natural/ele) + `stringtable_has_relevant_keys()`
with exact-match (not substring) semantics + unit test (3 cases).
**SPEC BUG flagged, not implemented:** the Blueprint states the filter lets
Pass 1 skip non-relevant blocks entirely. Applied literally to node indexing
this is a correctness bug: way vertices are overwhelmingly UNTAGGED dense
nodes, so node blocks rarely contain highway/sac_scale in their StringTable —
gating Pass 1 on it would skip ~all node blocks, hollow out the coordinate
index, and break Pass-2 reconstruction. The filter is therefore implemented
as a **Pass-2 gate** (way/relation blocks), with the rationale documented on
the function and at the Pass-1 call site. Pass 1 achieves the Blueprint's
intended CPU saving structurally: prost skips undeclared way/relation/tag
fields at the wire level without deserializing them.

**Still open from md Phase 3 (future chunks, unchanged):** RSS:anon
instrumentation as a CI gate; Austria-scale (767MB) on-device index run; iOS
increased-memory entitlements.

**Verification:** fmt + clippy -D warnings clean; pbf 20/20 (+1 ignored L2);
real-extract test re-run green post-change.

**Outcome:** CLOSED. Implementation aligned with research/ specs; one spec
correctness bug documented and routed to Pass 2 instead of blindly applied.

---

## P3.C3 — Pipeline integration (real Pass 1 in the engine) + Pass 2 schema

**Status:** CLOSED
**Date:** 2026-07-13
**Goal:** replace the simulated Pass1Nodes loop in compiler/engine.rs with the
real `pbf::run_pass1_slice` driver (resume via durable `pbf_byte_offset`);
declare the Pass-2 `Ways` redb schema with compact node-ref values; coordinate
lookup helper.

**Files declared:** compiler/src/engine.rs (integration rewrite), compiler
lib.rs docs + Cargo.toml, pbf/src/lib.rs (Ways table + varint codec +
helpers), pbf/src/fixtures.rs (new, shared synthetic-PBF builders),
pbf/src/scan.rs (tests refactored onto fixtures), pbf/Cargo.toml (`fixtures`
feature), ffi/src/lib.rs (tests + doc), ffi/Cargo.toml, LOOPLOG.

**Design:**
- Engine integration: Pass1Nodes now mmaps `job.pbf_path`, opens the per-job
  index (`{job_id}.index.redb`, bounded-cache), and calls `run_pass1_slice`
  with `should_yield = elapsed >= budget`, resuming from the checkpoint's
  `pbf_byte_offset` — the offset maps back through Yielded exactly as the
  directive specifies. Pass2/Terrain/Finalize stay simulated placeholders.
- Checkpoint format v2: added cumulative `blocks_done` (feeds
  RunSummary::blocks_total now that Pass 1's block count is dynamic).
  INTERNAL only — the FFI CheckpointState record is UNCHANGED (no Surface v1
  HITL gate triggered); v1 checkpoints are rejected as unsupported-version
  (no shipped users; corrupt-state posture preserved).
- Progress model: per-phase fraction (pass1 = byte offset / file length; sim
  = block/blocks) mapped to overall pct = (phase_idx + frac)/n_phases.
  Monotonicity property preserved (test kept green).
- Purge now removes checkpoint AND index db on finish/cancel (Blueprint step
  8, "temporary redb files are purged"); index deliberately SURVIVES yields —
  it is the resume substrate.
- `Ways` table: `TableDefinition<u64, &[u8]>` — values are delta+zigzag+
  LEB128 varint node-ref sequences (the PBF wire format's own integer coding;
  ~1 byte/node for consecutive IDs vs 8 raw — proven by test). No in-memory
  coordinate vectors on the write path (50MB posture). encode/decode reject
  IDs outside the OSM sint64 domain, truncated varints, >64-bit varints, and
  delta overflow. `insert_ways_batched` mirrors the coords batching contract;
  `get_way_refs` decodes on read. Coordinate lookup = existing `get_coord`
  (P3.C1) — cited rather than duplicated.
- Fixtures: synthetic-PBF builders extracted to `pbf::fixtures`
  (cfg(any(test, feature = "fixtures"))); compiler/ffi consume via
  dev-dependency feature — never in production builds; scan.rs test
  duplication removed.

**Proof:**
- compiler 20/20 (+1 ignored L2): real-nodes-into-redb mid-job verification
  (all 5 fixture nodes durably queryable between zero-budget slices, offset ==
  file length at pass1 end), kill-resume determinism (sliced == single run),
  zero-budget min-progress, missing/corrupted PBF → typed Failed, phase order,
  monotonic progress, checkpoint v2 roundtrip, purge removes index db.
- pbf 23/23: way-refs roundtrip (incl. non-monotonic, i64::MAX boundary,
  compactness <1.01 byte/node), garbage rejection, Ways+Coordinates coexist,
  chunked commits.
- ffi 7/7 against real (synthetic) PBF through compile_chunk.
- **L2 real-data, integrated:** `real_innsbruck_end_to_end_sliced` (ignored):
  full engine over the real 19.5MB extract at a production-shaped 250ms
  budget → **303 blocks (265 real + 38 sim) / 8 yields / ~2.3s**, byte
  accounting exact for 1,900,652 nodes through the integrated path.
- fmt + clippy -D warnings clean; workspace 63/63 (3 ignored L2s);
  `cargo ndk -t arm64-v8a build -p ffi` CLEAN — the full shipping chain
  (ffi→compiler→pbf: redb/prost/memmap2/miniz) cross-compiles.

**Outcome:** CLOSED. Pivots: 0. The engine's Pass 1 is production-real under
the Phase-2 budget-yield contract; Pass 2 storage schema ready. Next: Pass 2
way extraction (StringTable-filtered) into `Ways`, then geometry
reconstruction via `get_coord` joins.

---

## P3.C4 — Pass 2 driver (way extraction) + geometry assembly + engine wiring

**Status:** CLOSED
**Date:** 2026-07-13
**Goal:** suspendable Pass 2 scanner with the StringTable pre-filter live on
its own cursor, tag-filtered way extraction into `Ways`, a two-table geometry
join (`assemble_way_geometry`), and real Pass2Ways in the engine with an
independent `pass2_byte_offset` checkpoint field.

**Files declared:** pbf/src/proto.rs (Way/WayBlock/WayGroup/StringTableProbe),
pbf/src/scan.rs (next_raw refactor + run_pass2_slice + extract_relevant_ways),
pbf/src/lib.rs (assemble_way_geometry + re-exports), pbf/src/fixtures.rs
(way_block / synthetic_pbf_with_ways), compiler/src/engine.rs (Pass2 arm +
checkpoint v3), ffi tests, LOOPLOG.

**Design:**
- **Per-pass prost views of the same wire bytes** — the load-bearing idea:
  scanner refactored to a raw framing step (`next_raw`) + typed decode per
  pass. Pass 1's PrimitiveBlock still never deserializes ways; Pass 2's
  `next_way_block` decodes a `StringTableProbe` (tag 1 ONLY) first and, when
  the pre-filter rejects, returns `Irrelevant` WITHOUT ever materializing Way
  messages — the Blueprint's "skip without deserializing entities", exactly.
  Only surviving blocks get the `WayBlock` decode (which itself wire-skips
  the dense-node arrays).
- Way extraction: keep iff any tag key resolves (StringTable) to
  RELEVANT_TAG_KEYS; refs delta-decoded (sint64 accumulator, checked);
  degenerate ways (<2 refs) dropped; corrupt inputs (key idx OOB, negative
  way id/ref, delta overflow) are typed errors. `Way.id` is plain int64 on
  the wire (NOT zigzag, unlike node ids) — encoded in proto.rs comments.
- `run_pass2_slice`: identical yield/min-progress/flush-before-offset
  contract as Pass 1; own cursor from 0; `blocks_prefiltered` exposed for
  telemetry. Ways batched through `insert_ways_batched` (LEB128 values).
- `assemble_way_geometry(db, way_id)`: WAYS→refs→COORDINATES join producing
  one transient Web-Mercator linestring (materialize-one-way-at-a-time, per
  Blueprint). Missing nodes are SKIPPED, not fatal — clipped extracts
  legitimately reference nodes outside the bbox; <2 resolved vertices → None.
- Engine: real Pass2Ways arm mirrors Pass 1; checkpoint **v3** adds
  `pass2_byte_offset` (independent cursor). Version bumped 2→3 (any format
  change bumps — the discipline that keeps kill-resume honest); FFI
  CheckpointState again UNCHANGED (internal field only, no Surface v1 gate).
  bytes_written accounting: +32 logical bytes/way (WAY_INDEX_BYTES).

**Proof:**
- pbf 28/28: pass2 filter matrix (kept/tag-filtered/block-prefiltered, with
  the node block itself counted prefiltered via its empty stringtable),
  yield-every-block resume with zero duplication, geometry join order+
  projection exactness, missing-node trio (mid-way skip / all-missing → None
  / single-vertex → None), corrupt-way rejection trio.
- compiler 21/21: new mid-job engine test drives zero-budget slices through
  BOTH real passes, then proves `get_way_refs`/`assemble_way_geometry` from
  durable state alone; checkpoint v3 roundtrip; determinism (sliced==single)
  now covers two real passes.
- ffi 7/7 (yield-phase assertion generalized — real passes finish fast).
- **L2 real-data:** integrated engine over the real 19.5MB Innsbruck extract,
  250ms budget: **544 blocks (265×2 passes + 14 sim) / 81,395 renderable
  ways / 8 yields / ~2.3s** — vs the fixture's known 29,558 highway paths,
  81k is plausible with waterway/natural/ele included.
- fmt + clippy -D warnings clean; `cargo check --workspace --all-targets`
  clean; workspace 69/69 (3 ignored L2s); `cargo ndk -t arm64-v8a build -p
  ffi` CLEAN.

**Outcome:** CLOSED. Pivots: 0 (one transient tool outage mid-chunk, no
impact). Both passes of the two-pass architecture are now real end-to-end
under the budget-yield contract; geometry reconstruction works from durable
state. Next (Phase 4 proper): RDP simplification + Sutherland-Hodgman
clipping over assembled linestrings, then tile binning.

---

## P4.C0 — Phase 4 scaffold: `geom` crate (handoff to geometry agent)

**Status:** CLOSED (scaffold only — implementation halted for handoff)
**Date:** 2026-07-13
**Goal:** empty-but-contractual `geom` crate so the geometry agent can
implement RDP simplification and Sutherland-Hodgman clipping against fixed
signatures.

**Done:**
- New workspace member `geom` (+ workspace.dependencies registry entry).
- **Dependency decision: NO `geo`/`geo-types` — raw `(f64, f64)` tuples.**
  Rationale in lib.rs: the pipeline already speaks `Vec<(f64,f64)>`
  (`assemble_way_geometry` output), both algorithms are ~100 lines of
  dependency-free math, and the aarch64 mobile cross-compile wants the
  smallest possible tree.
- `simplify_rdp(&[(f64,f64)], epsilon) -> Vec<(f64,f64)>` and
  `clip_to_bounds(&[(f64,f64)], (f64,f64,f64,f64)) -> Vec<(f64,f64)>`
  declared exactly as directed, bodies `todo!()`, with full implementer
  contracts in doc comments (units = Web-Mercator meters; RDP must preserve
  endpoints, pass through <3-vertex inputs, and be stack-safe; clip must
  insert boundary-intersection vertices).
- **FLAG for the operator/geometry agent:** `clip_to_bounds`'s directed
  single-`Vec` return cannot faithfully represent a linestring that exits
  and re-enters the tile box (multiple disjoint segments). Documented in the
  signature's doc comment: resolve the return shape (likely
  `Vec<Vec<(f64,f64)>>`) BEFORE wiring into tile binning; never silently
  bridge disjoint segments.

**Verification:** `cargo check --workspace --all-targets` clean; clippy
-D warnings clean; workspace tests all green (stubs are `todo!()` and
uncalled). P3.C4 committed as `7e143e6` per operator instruction.

**Outcome:** CLOSED. Halted here per operator instruction — implementation
of the two functions belongs to the geometry agent.

---

## P4.C1 — `geom`: RDP simplification + Liang-Barsky clip implementation

**Status:** CLOSED
**Date:** 2026-07-14
**Goal:** `simplify_rdp` and `clip_to_bounds` fully implemented, dependency-free,
stack-safe, with exhaustive unit coverage; `clip_to_bounds`'s return-shape flag
from P4.C0 resolved.

**Files declared:** `geom/src/lib.rs` (implementation + tests), `LOOPLOG.md`.

**Design:**
- **RDP — iterative, explicit heap stack.** `Vec<(usize, usize)>` of
  `(start, end)` index ranges stands in for the call stack; a `keep: Vec<bool>`
  accumulates survivors, filtered into the output at the end. No native
  recursion at any point — verified with a 20k-vertex zigzag fixture
  (`rdp_handles_large_input_without_recursing`) where nearly every split
  survives, forcing the deepest/widest stack the algorithm can produce.
  Perpendicular distance uses the standard cross-product-over-segment-length
  form, degenerating to Euclidean point distance when start==end coincide
  (zero-length chord).
- **Clip return-shape flag (P4.C0) resolved: `Vec<Vec<(f64,f64)>>`,** per
  operator direction. Implemented as per-segment **Liang-Barsky** parametric
  clipping (not Sutherland-Hodgman — that algorithm is for closed convex
  polygons and has no notion of "disjoint output pieces"; an open linestring
  that exits and re-enters the box needs exactly the per-segment
  entry/exit-run tracking Liang-Barsky gives for free via its `t0/t1`
  parametrization). Each segment clips independently against the 4
  half-planes; runs are chained across segments by comparing the current
  segment's entry point against the last point pushed, using plain `==` —
  safe because un-clipped endpoints are returned bit-identical to the input
  (`t<=0`/`t>=1` short-circuit in `at()`, never reconstructed via
  interpolation), so a shared vertex between two adjacent in-bounds segments
  is the same f64 bits in both calls.
- **Degenerate-run filter:** tangential single-point corner touches clip to
  two bit-identical points, which would pass a naive `len() >= 2` check
  without representing a real line. `flush()` additionally requires at least
  one pair of adjacent points to differ before emitting a run.

**Proof (geom 14/14, all new):**
- RDP: empty/1-point/2-point passthrough, collinear removal, sharp-corner
  preservation, epsilon-boundary wiggle (kept vs. dropped at two epsilons),
  endpoint invariant under an aggressive epsilon, 20k-vertex stack-safety
  smoke test.
- Clip: fully inside (unmodified single run), fully outside (empty),
  single intersection (one inserted boundary vertex), exit-and-reentry (two
  disjoint runs, exact boundary coordinates), corner intersection (diagonal
  clipped exactly to both corners), tangential corner touch (degenerate,
  correctly dropped), short-input (<2 vertices) passthrough-to-empty.

**Verification:** `cargo fmt --check` clean; `cargo clippy -p geom
--all-targets -- -D warnings` and `cargo clippy --workspace --all-targets --
-D warnings` clean; `cargo check --workspace --all-targets` clean;
`cargo test --workspace` 83/83 passing (69 pre-existing + 14 new, 2 ignored
L2s unaffected); `cargo build -p geom --target aarch64-apple-ios` and
`cargo ndk -t arm64-v8a build -p geom` both clean (mobile cross-compile
confirmed directly on the new code — `ffi` doesn't depend on `geom` yet, so
the `ffi` cross-build doesn't exercise it).

**Outcome:** CLOSED. Both Phase 4 geometry primitives are real. Not yet
wired into `ffi`/tile binning — that integration (and the corresponding
`geom` entry in `ffi`'s `Cargo.toml`) is a separate chunk. Pivots: 1 — an
initial unit test for the tangential-corner-touch case used a line that
doesn't actually pass through the box corner; caught by hand-verifying the
Liang-Barsky arithmetic before trusting the test, which also surfaced the
real degenerate-run bug in `flush()` fixed above.

(P4.C1 committed as `e390784` per operator approval.)

---

## P4.C2 — Pass 3: tile binning (geom → engine integration)

**Status:** CLOSED
**Date:** 2026-07-14
**Goal:** real Pass 3 end-to-end: iterate WAYS, assemble → RDP-simplify →
clip per z14 tile → store disjoint segments in a `TileFeatures` index, under
the same budget-yield/kill-resume contract as Passes 1-2.

**Files declared:** geom/src/lib.rs (tile grid math), pbf/Cargo.toml (+geom
dep, operator-directed), pbf/src/tile.rs (NEW: table, ser/de, driver),
pbf/src/lib.rs (module + way_count + varint visibility),
compiler/src/engine.rs (Pass3Tiles arm + checkpoint v4),
compiler/Cargo.toml (+geom dev-dep), ffi/src/lib.rs (CompilePhase),
LOOPLOG.

**Design:**
- **`TileFeatures` schema (pbf):** `(u8 z, u32 x, u32 y, u64 way_id)` →
  bytes, exactly as directed. Way ID in the composite KEY keeps writes
  append-shaped (no read-modify-write of shared per-tile blobs); a tile's
  features come back via key-range scan. Value format:
  `varint(n_segments) [varint(n_vertices) (f64 LE x, f64 LE y)...]...` —
  full-precision Web Mercator; quantization belongs to the MVT stage.
  Decode is corruption-typed (truncation, hostile counts, trailing bytes).
- **Tile math (geom, dependency-free):** `MERCATOR_HALF_WORLD_M`,
  `tile_extent_m` / `meters_per_pixel` / `epsilon_for_zoom` (half a display
  pixel — ~2.4km at z5, ~4.8m at z14, the Blueprint's per-zoom scaling),
  `mercator_to_tile` / `tile_bounds` (XYZ y-down, clamped at world edges),
  and `tiles_crossed_by_segment` (Amanatides-Woo grid traversal).
- **DEVIATION (logged per §0):** the directive said way *bbox* → tile range.
  A way-bbox scan is O(bbox area): one extract-clipped diagonal way
  (Innsbruck→Valparaíso was literally in our own engine fixture) spans
  ~3,700×4,000 z14 tiles → 15M clip calls — a self-DoS on device. Implemented
  per-SEGMENT grid traversal instead: O(tiles actually crossed), identical
  output, immune to hostile/clipped geometry. Candidate tiles are dilated by
  one ring so ways grazing a neighbour's *buffer* zone still contribute
  (buffer << tile, so one ring is always sufficient).
- **Clip buffer:** each tile clips to bounds + `64/4096` of extent (MVT
  buffer convention) — Blueprint's "bbox plus a minor rendering buffer",
  so strokes have join geometry across tile seams.
- **Driver (`run_pass3_slice`):** cursor = last fully-binned way ID
  (`pass3_last_way_id`, 0 = fresh; OSM way IDs ≥ 1). Same
  yield/min-progress/flush-before-cursor contract as Passes 1-2; re-binning
  a way after a crash rewrites identical `(z,x,y,way)` keys — idempotent.
  Geometry is materialized one way at a time via `assemble_way_geometry`
  and dropped immediately (50MB posture); simplification runs ONCE per way
  (epsilon is zoom-fixed, so per-tile simplify would be identical output for
  3× the work). Unassemblable ways (refs outside the extract) count as
  progress and are skipped.
- **Engine:** `Phase::Pass3Tiles` between Pass2Ways and Terrain; checkpoint
  **v4** (+`pass3_last_way_id`); progress denominator = `pbf::way_count`;
  accounting +64 logical bytes/feature (`TILE_FEATURE_BYTES`).
- **SURFACE v1 CHANGE (flag):** `ffi::CompilePhase` mirrors `engine::Phase`
  exhaustively, so `Pass3Tiles` had to be appended — new Swift/Kotlin enum
  case, existing cases unchanged. Made under the operator's P4.C2
  integration directive; no other FFI surface change (CheckpointState
  unchanged — the new field stays internal).
- **Fixture change:** engine-test way 500 refs became local
  ([1000, 1005], both Innsbruck) — the old cross-hemisphere way would bin
  thousands of tiles and make byte-accounting assertions unverifiable.
  Cross-hemisphere assembly coverage remains in the pbf crate's own tests.

**Proof:**
- geom 18/18 (+4): tile corners/quadrants/clamping, bounds↔tile roundtrip,
  epsilon-vs-zoom scaling, traversal (single-tile / 3-tile crossing /
  100×50 diagonal visits ~151 tiles not 5,151 / zero-length degenerate).
- pbf 36/36 (+8): ser/de roundtrip + 4-way corruption rejection; **the
  required integration test** (`way_crossing_tile_boundary_splits_into_both_tiles`:
  one way straddling x=0 at z14 → exactly 2 features, each clipped to its
  buffered box, unclipped endpoints bit-exact, boundary vertices at ±buffer);
  fully-inside way binned once unclipped; yield-every-way resume with zero
  duplication (3 ways / 4 slices / 3 features); unassemblable-way skip;
  empty/absent WAYS table; sub-epsilon wiggle simplified before storage.
- compiler 22/22 (+1): `pass3_bins_tiles_mid_job` proves the binned feature
  from durable state alone mid-job; determinism (sliced==single),
  monotonic progress, phase order, and checkpoint-roundtrip tests all now
  cover the third real pass; ffi 7/7.
- **L2 real-data:** integrated engine over the real 19.5MB Innsbruck
  extract, 250ms budget, release: **81,395 ways binned / 97,619 tile
  features / 11 yields / 3.18s total across all three real passes** —
  ~1.2 features per way is exactly the local-ways-mostly-fit-one-tile
  shape expected at z14, with boundary-crossers contributing the rest.
- fmt clean; clippy --workspace --all-targets -D warnings clean (one
  type_complexity resolved via a named `TileFeature` alias); workspace
  96/96; `cargo ndk -t arm64-v8a build -p ffi` CLEAN. `cargo build
  --target aarch64-apple-ios` compiles every crate; the final `ffi` cdylib
  LINK step fails on this machine with or without P4.C2 (CommandLineTools
  only, no iPhoneOS SDK) — pre-existing environment gap, verified by
  stash-and-rebuild of the committed tree, not a code regression.

**Outcome:** CLOSED. All three real passes now run under the budget-yield
contract; TileFeatures is ready for the Phase 5/6 MVT encode + PMTiles
stages (note: the index, TileFeatures included, is still purged on job
finish — Finalize must drain it into the archive before that purge once
Phase 6 lands). Pivots: 1 — the way-bbox→tiles blowup above, resolved by
grid traversal.

**Operator review:** APPROVED — grid-traversal deviation, serialization
format, and MVT-buffer math all accepted. Committed with this entry per
operator instruction. **The Rust backend is FROZEN at this commit per
operator directive** — no further core chunks; work halts here.

---

## P5.C1 — Out-of-core Finalize: MVT encode + PMTiles v3 assembly

**Status:** IN PROGRESS
**Date:** 2026-07-16
**Operator context:** core freeze (P4.C2) lifted 2026-07-16 for Phase 5. Directive
approves MVT/gzip dependency additions in-band (§1.5): `flate2` chosen (pure-Rust
miniz_oxide backend — same inflate library Pass 1 already ships, gains a correct
gzip wrapper + CRC32); MVT wire format via HAND-DERIVED prost structs in `tiles`
(house pattern from pbf/src/proto.rs — no protoc, no new tree); PMTiles v3 writer
hand-rolled (127-byte header + varint directories; the `pmtiles` crate is
async/reader-shaped, wrong fit for a sequential mobile writer).

**Goal:** replace the simulated `Finalize` arm with a real two-stage driver:
(1) ENCODE — drain `TileFeatures` per tile → MVT (extent 4096, tile-local
quantization matching Pass 3's 64/4096 buffer) → gzip → dedup → append to a tmp
data file, yieldable per tile with cursor `pass5_last_tile` (checkpoint v5);
(2) ASSEMBLE — Hilbert-ordered clustered data section + root directory +
gzip'd metadata + exact-offset header, atomic tmp+rename to `{job_id}.pmtiles`,
written BEFORE the index purge.

**Files declared:** freehike-core/Cargo.toml (+flate2), tiles/Cargo.toml,
tiles/src/{lib,hilbert,mvt,pmtiles,finalize}.rs (crate becomes real),
compiler/Cargo.toml (+tiles), compiler/src/engine.rs (Finalize arm, checkpoint
v5, purge of tiledata tmp), ffi/src/lib.rs (test expectation only — NO surface
change), LOOPLOG.

**Durability design (the load-bearing part):**
- Encode slice = one read txn over TILE_FEATURES + ONE write txn holding
  `FinalizeTileEntries` (tile_id → (offset,len) in tmp data file) and
  `FinalizePayloadHashes` (FNV-1a64 → (offset,len), byte-verified on hit so a
  hash collision can never alias a wrong tile). Payload bytes fsync'd BEFORE
  the entry txn commits; cursor reported after both — checkpoint never runs
  ahead of durable data. Torn tail from a crash mid-append is truncated on
  resume to the entries' high-water mark. Re-encoding after a stale checkpoint
  is idempotent (identical payload → dedup hit → identical entry row).
- Assembly is a single idempotent block (atomic tmp+rename); if killed between
  rename and purge, the resume re-assembles identical bytes. Yield honored
  BETWEEN encode-exhaustion and assembly so a spent budget never overruns.
- Data section re-ordered at assembly into ascending-tile_id (Hilbert) order →
  `clustered=1` per spec; dedup'd entries point back at first occurrence.
- KNOWN SIMPLIFICATIONS (logged): root-directory-only layout (spec-valid; leaf
  splitting when entry counts demand it is a follow-up chunk); run_length
  coalescing not implemented (all run_length=1); single MVT layer "features"
  with way_id as feature id and NO attributes — TileFeatures stores geometry
  only; persisting the matched tag class through Pass 2/3 for styled layers is
  flagged as required follow-up work.

**Proof tests (named up front):**
- tiles::hilbert: z0/z1/z2 ids match the PMTiles reference values (1..4, 5, 7),
  zoom base offsets, exhaustive xy2d↔d2xy roundtrip z≤5 + spot z14.
- tiles::mvt: exact MoveTo/LineTo/zigzag command stream for a known geometry,
  tile-local scaling, buffered vertices beyond extent survive, degenerate
  segments dropped, all-degenerate tile → None, payload prost-decodes.
- tiles::pmtiles: header is byte-exact 127 with fields at spec offsets,
  directory varint roundtrip incl. offset-0 continuation encoding.
- tiles::finalize: **archive_from_dummy_rows_validates_header** (the directive's
  integration test: dummy TILE_FEATURES rows → encode → assemble → header magic/
  version/offset arithmetic + root-dir readback + gunzip + prost decode + dedup
  contents<entries), yield-every-tile resume with zero duplication, torn-tail
  truncation, empty-table → valid empty archive.
- compiler::engine: suite updated to checkpoint v5 + real finalize accounting;
  new finalize_writes_archive_before_purge (archive exists + magic bytes after
  Finished; checkpoint/index/tmp all gone) and mid-encode yield exposing
  pass5_last_tile > 0; determinism (sliced==single) now covers real Finalize.
- ffi: blocks_total expectation updated (finalize = tiles + 1 assembly block).
- Ladder: L1 ×2 green-lock + `cargo ndk -t arm64-v8a build -p ffi`.

**Attempts:**
- A1: `flate2` (default-features off, `rust_backend` only) added to workspace;
  tree audit confirms pure miniz_oxide underneath — zero C in the mobile path.
  `tiles` crate rebuilt from Phase-0 stub into hilbert/mvt/pmtiles/finalize
  modules; compiler gains the `tiles` dep; `Phase::Finalize` arm made real;
  checkpoint v4→v5 (+`pass5_last_tile`); purge extended to the tiledata tmp +
  half-written archive tmp (final `.pmtiles` explicitly survives).
- A2: first `cargo test -p tiles`: Hilbert rotate panicked on debug-build
  subtract overflow — the classic algorithm's `s-1-x` reflection relies on
  wrapping when unprocessed high bits are present. FIX: mask processed bits
  (`x &= s-1`) before rotating; equivalent modulo the bits ever read again.
  Spec-example IDs (z0/z1/z2 published values), exhaustive z≤5 roundtrip, and
  curve-adjacency tests all green after.
- A3: one test-side assertion bug self-caught: asserted the encode cursor
  monotonic in HILBERT id space; it is monotonic in (z,x,y) SCAN order (the
  ids deliberately jump around the row-major scan; resume maps id→(z,x,y)
  bijectively, so the contract was never at risk). Assertion fixed to the
  real invariant.
- A4: clippy -D warnings: type_complexity (encode_tile_mvt signature → reuse
  `pbf::tile::TileFeature`) + map_identity (test) — both fixed; fmt applied.
- A5: workspace 118/118 green (24 compiler / 20 tiles / 36 pbf / 18 geom /
  7 ffi / 13 fetcher; 3 ignored L2s). Green-lock ×2 (tests + clippy + fmt).
- A6 **L2 real-data:** integrated engine over the real 19.5MB Innsbruck
  extract, 250ms budget, release: **82,957 blocks / 1,019 archive tiles /
  2,419,611-byte basemap.pmtiles / 12 yields / 3.37s** all five phases.
  Block algebra cross-checks exactly: 530 (passes 1+2) + 81,395 ways + 12
  terrain-sim + 1,019 tiles + 1 assembly = 82,957. One plausibility floor in
  the L2 test was MINE not the code's: guessed >10k distinct tiles, but
  97,619 features concentrate ~96/tile → 1,019 tiles (~6,000km² at z14) —
  floor corrected to a defensible band.
- A7 **Independent reference-reader proof (beyond the declared ladder):**
  the archive validated with the app's own `pmtiles` JS package (4.4.1,
  the exact reader the client uses): header parses (v3, MVT, clustered,
  bounds exact, 1019/1019/1019 counts), metadata JSON parses with
  vector_layers, and `getZxy(14, 8710, 5744)` (Innsbruck centre) returns a
  56,897-byte de-gzipped tile opening with the MVT `layers` field tag
  (0x1A). Writer proven against the reference parser, not just our own.
- A8: `cargo ndk -t arm64-v8a build -p ffi` CLEAN — full shipping chain
  (ffi→compiler→tiles→pbf/geom + flate2) cross-compiles.

**Outcome:** CLOSED. Pivots: 1 (Hilbert debug-overflow fix; A3/A6 were
test-side corrections). The pipeline is now END-TO-END REAL for a flat
basemap: PBF → nodes → ways → tile binning → MVT+gzip → PMTiles v3 at
`archive_path`, all under the budget-yield/kill-resume contract. Terrain
remains the only simulated phase (Phase 6, deferred per operator).
FFI surface UNCHANGED (no HITL gate triggered).
**Follow-ups logged:** tag/class persistence through Pass 2/3 for styled MVT
layers (currently single anonymous "features" layer); leaf-directory split
for large entry counts; run_length coalescing; client wiring of the produced
archive (Phase 5 exit criterion — render in the FreeHike app — is the NEXT
chunk's proof).

**Operator review:** APPROVED — hand-rolled prost structs and FNV-1a64 dedup
accepted as aligned with the zero-dependency, mobile-first constraints.
Committed with this entry per operator instruction. **The Rust backend is
LOCKED at this commit per operator directive** — Phase 5 compiler work
complete; next work is client integration of the produced `.pmtiles`
archive (the Phase 5 exit criterion: render in the FreeHike app).

---

## P5.C2 — Tag persistence and MVT layering

**Status:** IN PROGRESS
**Date:** 2026-07-16
**Operator context:** P5.C1 lock lifted by directive for this chunk. Goal: styled
MVT output — persist rendering-relevant OSM tags from Pass 2 extraction through
to per-layer MVT encoding, within the 50MB out-of-core posture.

**Design (locked before code):**
- **Layer taxonomy** (pbf, shared): `LAYER_KEYS = [highway, waterway, natural,
  landuse]` in match-priority order. A way's LAYER is the first of these keys it
  carries; its CLASS is that tag's value (e.g. highway=path → layer "highway",
  class "path"). Open-ended class strings are stored verbatim (UTF-8 validated
  at extraction — OSM stringtables are UTF-8 by spec; invalid is corruption).
- **SEMANTICS TIGHTENING (deviation, logged):** Pass 2's keep-filter becomes
  "way carries a LAYER key" — `sac_scale`/`ele` alone no longer keep a way.
  Rationale: a way with sac_scale but no highway cannot be styled into any
  layer, and un-layered geometry can no longer exist in the new TileFeatures
  format. `RELEVANT_TAG_KEYS` (the block-level StringTable prefilter) KEEPS
  sac_scale/ele and gains landuse — the prefilter stays conservative
  (block-level relevance), the way-level filter becomes exact (layer-level).
- **Schema:** new `WayTags` table `way_id → (u8 layer, &[u8] class)` written in
  the SAME chunked write txns as `Ways` (one crash-consistency domain).
  `insert_ways_batched` item becomes `IndexedWay {id, layer, class, refs}`.
  Proto `Way` gains `vals` (tag 3) — keys/vals parallel arrays; length mismatch
  is a typed corruption error.
- **TileFeatures value format v2:** `u8 layer | varint(class_len) class |
  varint(n_segments) [segments…]` — Pass 3 denormalizes tags into every row it
  writes (class ≈ bytes-per-feature cost, disk not RAM), so Finalize's drain
  stays a single-table scan. Decode is corruption-typed incl. layer ≥ 4.
  `get_tile_features` returns a named `TileFeature` struct (way_id, layer,
  class, segments) — the tuple outgrew itself.
- **Pass 3:** joins `WayTags` per way; a WAYS row without a WayTags row is a
  HARD error (only reachable by resuming a pre-P5.C2 index against new code —
  refuse rather than guess, same posture as checkpoint version bumps).
- **MVT encoder:** groups a tile's features by layer index (BTreeMap →
  deterministic layer order), emits one named MVT Layer per group with
  `keys=["class"]`, `values` = first-seen-deduped class strings, and per-feature
  `tags=[0, value_idx]`. Archive metadata `vector_layers` lists the four layers
  with `fields: {"class":"String"}`.
- **Accounting unchanged:** WAY_INDEX_BYTES (32) and TILE_FEATURE_BYTES (64)
  stay — they were already amortized estimates; docs updated to mention tags.
- **Fixtures:** `FixtureWay` becomes `(id, key, value, refs)` (compiler/ffi
  test suites updated in the same sweep).

**Proof tests (named up front):**
- pbf::scan: keep/drop matrix rewritten for layer semantics (highway kept +
  layer/class stored; sac_scale-only DROPPED — the new tightened rule; landuse
  kept; building dropped; prefilter counts unchanged); keys/vals length
  mismatch and val-index-out-of-StringTable rejected; layer priority (way with
  natural+highway → highway).
- pbf::tile: v2 roundtrip incl. layer/class; garbage rejection extended (bad
  layer byte, truncated class); binning tests assert layer/class survive the
  clip into every affected tile.
- tiles::mvt: two-layer tile → two named Layer messages in deterministic
  order; class value dedup within a layer (two features share one Value,
  both tags = [0, idx]); keys==["class"] everywhere.
- tiles::finalize: integration test now seeds two layers + asserts per-layer
  readback through the root directory (prost decode → layer names, tags,
  values) and metadata vector_layers naming all four layers.
- compiler::engine: fixture ways carry values; pass3_bins_tiles_mid_job
  asserts the TileFeature struct incl. layer/class; end-to-end totals
  unchanged (accounting constants untouched).
- Ladder: L1 ×2 green-lock + `cargo ndk -t arm64-v8a build -p ffi` + L2
  real-Innsbruck rerun (expect same way/tile counts — the keep-filter change
  only affects sac_scale/ele-only ways, which are rare anomalies).

**Attempts:**
- A1: proto `Way` gains `vals` (tag 3, parallel to keys — mismatch is typed
  corruption). `LAYER_KEYS`/`layer_name` taxonomy + `IndexedWay` +
  `WAY_TAGS` table in pbf; `insert_ways_batched` writes refs + tags in ONE
  txn per chunk; `get_way_tags` point lookup. `extract_relevant_ways`
  resolves highest-priority layer key + UTF-8-validated class value.
- A2: TileFeatures value format v2 (layer byte + varint-length class +
  segments); decode rejects bad layer / truncated class; `TileFeature`
  struct replaces the outgrown tuple; Pass 3 joins WAY_TAGS per way and
  HARD-fails on a refs-without-tags row (pre-P5.C2 index resumed against
  new code — refuse, don't guess).
- A3: MVT encoder groups by layer index (BTreeMap → deterministic layer
  order), one named Layer per group, keys=["class"], first-seen-deduped
  value pools, per-feature tags=[0,v]. `LAYER_NAME` const retired in favor
  of `CLASS_KEY`. Archive metadata declares all four vector_layers with
  fields {"class":"String"} unconditionally (styles reference statically).
- A4: full test sweep across pbf/tiles/compiler fixtures (`FixtureWay` →
  4-tuple). Workspace 122/122 first complete run; two clippy nits (unused
  mut, test type_complexity) fixed; green-lock ×2; `cargo ndk -t arm64-v8a
  build -p ffi` CLEAN.
- A5 **L2 real-data:** Innsbruck extract, 250ms budget, release: **93,085
  blocks / 1,030 archive tiles / 3,206,159-byte archive / 13 yields /
  3.66s**. Deltas vs P5.C1 are the expected signature of the filter change:
  ways ~81.4k → ~91.5k (landuse now kept), archive 2.4MB → 3.2MB (tag
  attributes + landuse geometry), tiles 1,019 → 1,030.
- A6 **Reference-reader proof:** the app's own `pmtiles` JS package parses
  the new archive's metadata into the four named vector_layers (class:
  String each) and the Innsbruck-centre z14 tile (74,223 bytes decompressed)
  carries `highway`/`waterway`/`natural`/`landuse`/`class`/`path` strings
  on the MVT wire — the styled-layer contract MapLibre needs is live
  end-to-end.

**Outcome:** CLOSED. Pivots: 0. Tags now persist raw-PBF → WayTags →
TileFeatures v2 → per-layer MVT with class attributes, all within the
out-of-core posture (tags are bytes-on-disk denormalization, never an
in-memory join table). Way-level keep-filter deliberately tightened to
layer keys (logged deviation, §above). FFI surface UNCHANGED; checkpoint
format UNCHANGED (v5 — no new cursor state; the schema change lives inside
the purged-per-job index).
**Follow-ups:** client style referencing the new source-layers
(high_contrast_outdoor_style.json still targets the old anonymous layer);
sac_scale as a per-feature attribute on highway features (dropped entirely
today); leaf directories + run-length coalescing still pending from P5.C1.
Uncommitted (P5.C2 diff): pbf (proto/lib/scan/tile/fixtures), tiles
(mvt/finalize), compiler test fixtures, LOOPLOG — awaiting operator.

---

## P5.C3 — Basemap enrichment: sac_scale as a second feature attribute

**Status:** IN PROGRESS
**Date:** 2026-07-16
**Operator context:** core lock lifted by directive. P5.C2 deliberately dropped
`sac_scale` at the way-level keep-filter; the frontend now needs it back to
color-code trail difficulty. This chunk threads it through as an OPTIONAL
second attribute — the keep-filter itself is unchanged (a sac_scale-only way
still has no layer and stays dropped).

**Design (locked before code):**
- **Schema:** [`WAY_TAGS`] value widens `(u8, &[u8])` → `(u8, &[u8], &[u8])`
  — layer, class, sac_scale (EMPTY SLICE = absent; OSM sac_scale values are
  never empty, so the sentinel is unambiguous). One table, one lookup, one
  crash-consistency domain with WAYS, ~1 byte/way for the overwhelmingly-empty
  case — chosen over a sparse secondary table because Pass 3 already pays a
  per-way point lookup and a second one buys nothing but code. A pre-P5.C3
  index resumed against new code fails loudly via redb's table-type-mismatch
  error (same refuse-don't-guess posture as the P5.C2 no-tag-record check).
- **Extraction scope:** sac_scale is resolved ONLY for layer-0 (highway) ways
  (directive scope; also keeps the archive metadata exactly honest — no other
  layer can ever carry the field). Values stored verbatim + UTF-8-validated,
  same no-whitelist policy as `class` (unknown grades fall through the style's
  match default).
- **TileFeatures format v3:** `u8 layer | varint class | varint sac_scale |
  varint n_segments …` — the same length-prefixed slot, empty = absent.
  Decode corruption-typed as before (truncated sac slot is a new reject case).
- **MVT encoder, multi-attribute:** per-layer key pool grows lazily —
  `keys[0]="class"` at layer creation, `keys[1]="sac_scale"` appended on the
  first sac-bearing feature (layers without any stay exactly `["class"]`,
  byte-identical to P5.C2 output). The per-layer VALUE pool is shared across
  keys (valid MVT; a class string and a sac string that coincide share one
  entry). Feature tags: `[0,c]` or `[0,c,1,s]`.
- **Metadata:** the highway vector_layer declares
  `fields:{"class":"String","sac_scale":"String"}`; the other three stay
  class-only.
- **Unchanged:** keep-filter, block prefilter (sac_scale already in
  RELEVANT_TAG_KEYS), checkpoint format (no new cursor state), FFI surface,
  engine accounting constants.

**Proof tests (named up front):**
- pbf::scan: hand-built WayBlock — highway+sac_scale extracts both; sac on a
  non-highway way is ignored; sac value index OOB / non-UTF-8 rejected.
- pbf::lib: ways-roundtrip covers the widened triple incl. empty-sac.
- pbf::tile: v3 roundtrip (with/without sac), truncated-sac rejection,
  binning carries sac into every clipped tile.
- tiles::mvt: sac-bearing highway feature → keys ["class","sac_scale"] +
  tags [0,c,1,s]; mixed layer (one feature with, one without) keeps a single
  key pool with per-feature tag arity; sac-free layers byte-stable at
  ["class"]; class/sac value-pool sharing.
- tiles::finalize: integration seeds sac on the dedup pair (identical sac →
  payloads still dedup) + asserts highway keys/tags and metadata fields
  through the assembled archive readback.
- compiler::engine: fixture TileFeature gains the empty-sac field (fixture
  ways carry no sac — single-tag fixture builder unchanged by design; the
  two-tag wire path is proven at the scan unit level and on real data).
- Ladder: L1 ×2 green-lock + `cargo ndk -t arm64-v8a build -p ffi` + L2
  real-Innsbruck rerun (way counts must be UNCHANGED vs P5.C2 — the filter
  didn't move; archive grows only by sac strings) + reference-reader scan
  for sac_scale reaching the MVT wire.

**Attempts:**
- A1: schema + extraction: WAY_TAGS widened to `(u8, &[u8], &[u8])`,
  `IndexedWay.sac_scale`, `WayTagRecord` alias; extract_relevant_ways notes
  sac_scale's value index in the same key sweep and resolves it (shared
  bounds+UTF-8 resolver with class) only when the way lands on layer 0 —
  a grade on a waterway/natural/landuse way is nonsense data, ignored.
- A2: TileFeatures v3 (second length-prefixed slot), TileFeature.sac_scale,
  bin_way/run_pass3_slice denormalization — all mechanical, per plan.
- A3: MVT encoder: per-layer lazy key pool (["class"] → +"sac_scale" on
  first graded feature), value pool shared across keys, tags [0,c] or
  [0,c,1,s]. Metadata declares sac_scale on highway ONLY. One clippy
  type_complexity on the widened get_way_tags return → named WayTagRecord.
- A4: workspace 127/127 green first complete run (24 compiler / 13 fetcher /
  7 ffi / 18 geom / 40 pbf / 25 tiles), green-lock ×2, fmt/clippy clean,
  `cargo ndk -t arm64-v8a build -p ffi` CLEAN. Self-caught: two stray noop
  lines left in a test edit, removed before compile.
- A5 **L2 real-data — the predicted fingerprint, exactly:** 93,085 blocks /
  1,030 archive tiles — BOTH IDENTICAL to P5.C2 (keep-filter untouched,
  way counts must not move, and they didn't); archive 3,206,159 →
  3,242,090 bytes (+35,931 = precisely the grade strings + key/tag
  entries); 13 yields / 3.67s.
- A6 **Reference-reader proof:** app's own `pmtiles` JS — highway
  vector_layer declares {"class","sac_scale"}, no other layer does; 199 of
  221 Innsbruck-region z14 tiles carry the sac_scale key on the
  decompressed MVT wire; ALL SIX canonical SAC grades observed (hiking →
  difficult_alpine_hiking, the full T1-T6 scale — exactly what a Tirol
  extract should yield).

**Outcome:** CLOSED. Pivots: 0. sac_scale persists raw-PBF → WayTags →
TileFeatures v3 → highway MVT features as a second attribute, with
grade-free layers byte-stable at their P5.C2 shape. Keep-filter, block
prefilter, checkpoint format (v5), FFI surface, and engine accounting all
UNCHANGED. The P5.C2 follow-up "sac_scale as a per-feature attribute" is
retired.
**Follow-ups (carried):** client style can now color trails on
`["get","sac_scale"]` (frontend chunk); leaf-directory splitting +
run-length coalescing; Terrain (Phase 6) deferred.
Uncommitted (P5.C3 diff): pbf (lib/scan/tile), tiles (mvt/finalize),
compiler test fixture, LOOPLOG — awaiting operator.


---

## P5.C4 — Basemap enrichment: `name` as a third feature attribute

**Status:** IN PROGRESS
**Date:** 2026-07-16
**Operator context:** approved continuation of the tagging pipeline. Goal:
persist OSM `name` end-to-end for text labels, on ALL FOUR layers (unlike
sac_scale's highway-only scope), same 50MB out-of-core posture.

**Design (locked before code):**
- **Schema:** [`WAY_TAGS`] widens again → `(u8 layer, class, sac_scale,
  name)`, empty slice = absent (unnamed ways cost ~1 byte). Same loud
  table-type-mismatch failure for a pre-P5.C4 index resumed on new code.
  WAYS ONLY: this pipeline has no POI-node extraction (peaks etc. are
  node-tagged; a node-POI layer is a future chunk, logged), so the
  directive's node-table clause is N/A by construction.
- **TileFeatures format v4:** third length-prefixed slot
  `layer | class | sac_scale | name | segments`; truncated/hostile name
  lengths are typed rejects like the other slots.
- **Extraction:** the existing single key sweep also notes `name`'s value
  index; resolved for EVERY kept way regardless of layer, through the same
  shared bounds+UTF-8 resolver (names are where non-ASCII actually lives —
  Tirol's ß/ä/ö/ü — so the UTF-8 validation finally earns its keep).
- **MVT encoder — the real refactor of this chunk:** with TWO lazy keys
  (sac_scale, name) the P5.C3 "keys.len()==1 → push at index 1" special
  case is wrong (key indices depend on which optional attribute a layer
  sees first). Replaced by a per-layer KEY pool mirroring the value pool:
  `HashMap<(layer, key), idx>` + lazy append to `Layer.keys`, with "class"
  pooled first by construction. Key order per layer = first-seen order —
  deterministic given deterministic feature order (way order), which the
  payload dedup + determinism proofs already rely on. Attribute-free
  layers stay byte-stable at `["class"]`.
- **Metadata:** all four vector_layers declare `"name":"String"` (per the
  directive); highway keeps `sac_scale` additionally.
- **Unchanged:** keep-filter (`name` alone keeps nothing — it's not a
  layer key), block prefilter (name-only blocks stay prefiltered: a named
  way with no layer key is dropped anyway), checkpoint v5, FFI surface,
  engine accounting.

**Proof tests (named up front):**
- pbf::scan: named highway (class+sac+name all extracted), named waterway
  (name on a NON-highway layer — the scope difference vs sac), unnamed way
  (empty sentinel); name value-index OOB and non-UTF-8 name rejected.
- pbf::lib: roundtrip covers the widened 4-slot record.
- pbf::tile: v4 roundtrip incl. name/UTF-8 umlauts, truncated-name reject,
  boundary-split binning carries name into both tiles.
- tiles::mvt: named non-highway feature → keys [class,name], tags
  [0,c,1,n]; **the key-index regression test**: one layer that sees a
  named-only feature FIRST and a graded-only feature SECOND must yield
  keys [class,name,sac_scale] with tags [0,c,1,n] and [0,c,2,s]
  respectively; attribute-free layers stay ["class"]; a fully-attributed
  feature carries [0,c,1,s,2,n]-shaped tags per its layer's pool order.
- tiles::finalize: seeded dedup pair gains identical names (payloads still
  dedup); waterway named; archive readback asserts per-layer keys/tags;
  metadata declares "name":"String" exactly 4 times (all layers) and
  sac_scale still exactly once.
- compiler::engine: fixture TileFeature literal gains the empty name.
- Ladder: L1 ×2 green-lock + cargo ndk + L2 real-Innsbruck (way/tile
  counts must AGAIN be unchanged; archive grows by name strings — the
  largest enrichment yet, street names are common) + reference-reader
  proof that real UTF-8 names (»…straße«) reach the MVT wire.

**Attempts:**
- A1: schema + extraction: WAY_TAGS → `(layer, class, sac_scale, name)`
  (named `RawWayTagsValue` alias — clippy type_complexity — with the
  standard redb 'static-schema-marker note), IndexedWay.name,
  4-slot WayTagRecord; the single key sweep now notes `name`'s value index
  and resolves it for EVERY layer through the shared bounds+UTF-8 resolver.
- A2: TileFeatures v4 (third length-prefixed slot), TileFeature.name,
  bin_way/pass3 denormalization (+ #[allow(too_many_arguments)] — the
  signature mirrors the v4 slot order verbatim). New decode rejects:
  hostile/truncated name lengths.
- A3 **the refactor:** encode_tile_mvt's P5.C3 "keys.len()==1" special case
  is WRONG with two lazy keys (indices depend on which attribute a layer
  sees first) — replaced with a per-layer KEY pool mirroring the value
  pool; "class" pools first for every feature → index 0 by construction.
  Metadata: name declared on ALL FOUR vector_layers (directive), sac_scale
  still highway-only.
- A4: workspace 131/131 first complete run (42 pbf / 27 tiles / 24
  compiler / 18 geom / 13 fetcher / 7 ffi); green-lock ×2; fmt/clippy
  clean; `cargo ndk -t arm64-v8a build -p ffi` CLEAN. The new
  `lazy_key_indices_follow_first_seen_order` test pins the refactor's
  exact regression risk (named-first layer → keys [class,name,sac_scale],
  per-feature tags re-indexed accordingly).
- A5 **L2 real-data — the fingerprint holds a third time:** 93,085 blocks /
  1,030 tiles, both IDENTICAL to P5.C2/C3 (keep-filter untouched); archive
  3,242,090 → 3,400,009 bytes (+157,919 of label text — the largest
  enrichment yet, as street names should be); 13 yields / 3.61s.
- A6 **Reference-reader proof:** all four vector_layers declare
  "name":"String" (highway: class+sac_scale+name); 220 of 221
  Innsbruck-region z14 tiles carry the name key; real UTF-8 labels on the
  decompressed wire — »straße« (ß), »Höhe« (ö), Inn, gasse, weg.

**Outcome:** CLOSED. Pivots: 0. `name` persists raw-PBF → WayTags →
TileFeatures v4 → MVT features on every layer; attribute-free layers stay
byte-stable at ["class"]. Keep-filter, block prefilter, checkpoint v5, FFI
surface, engine accounting all UNCHANGED. The Phase 5 tagging pipeline is
COMPLETE: class + sac_scale + name.
**Follow-ups (carried):** frontend labeling (style has no symbol/text
layers yet AND no glyphs assets populated — both needed before names
render); node-POI extraction (peak names are node-tagged, out of this
ways-only pipeline); leaf directories + run-length coalescing; Terrain
(Phase 6) deferred.
Uncommitted (P5.C4 diff): pbf (lib/scan/tile), tiles (mvt/finalize),
compiler test fixture, LOOPLOG — awaiting operator.
*(Post-close note: committed as `cbb0e05` (sac_scale) / `01fed9f` (name) on
operator instruction; see P5.SEAL below.)*

---

## P5.SEAL — Phase 5 closure: vector basemap complete, verified, sealed

**Status:** CLOSED
**Date:** 2026-07-17
**Operator directive:** workspace audit, doc consolidation, Phase 6 prep.

**Phase 5 is COMPLETE, VERIFIED, and SEALED.** The full vector basemap path is
production-real end-to-end: raw `.osm.pbf` → Pass 1 nodes → Pass 2 ways+tags →
Pass 3 tile binning → per-layer MVT (class + sac_scale + name attributes) →
gzip+dedup → Hilbert-clustered PMTiles v3 — all under the budget-yield /
kill-resume contract, validated against the reference `pmtiles` JS reader down
to UTF-8 labels and all six SAC grades on the wire. The frontend side of the
exit criterion is equally done: offline style renders the four source-layers
with T1–T6 trail difficulty colors, natural/forest fills, and Noto Sans labels
from vendored offline glyphs — **visually verified live in the app.**
Commits sealing the phase: `cbb0e05` (sac_scale pipeline), `01fed9f` (name
pipeline + MVT key-pool refactor), `d3481de` (offline style: T1–T6 colors,
forests, labels), `555207e` (vendored Noto Sans glyphs).

**DOCUMENTATION CONSOLIDATION (operator directive, this session):**
The root **`ARCHITECTURE.md` is now the single, definitive source of truth**
for the master implementation plan, memory constraints, and core architectural
pillars. **All older research files are hereby DEPRECATED** in its favor —
retained only as historical/citation references:
- `research/Geospatial App Architecture Research.pdf` (and its former root
  text export `architecture.md`)
- `research/Client-Side Map Compilation Feasibility.pdf`
- `research/On-Device Map Compiler Blueprint.pdf`
- `research/On-Device Map Compilation - Feasibility, Architecture, and
  Implementation Plan.md`
- `research/Offline Map App Architecture & Build Plan.pdf`
- `implementation_plan_phase3.md`
`agentic_operating_manual.md` (process contract) and this LOOPLOG (append-only
history) remain in force alongside it.

**Hygiene sweep (pre-Phase-6 foundation check):**
- `cargo fmt --all`: zero diffs; `cargo fmt --check` clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: **zero warnings**
  on a forced full recheck (sources touched to invalidate cache). Nothing
  lingering from the sac_scale/name iteration.
- `cargo test --workspace`: **131/131 green** (24 compiler / 13 fetcher /
  7 ffi / 18 geom / 42 pbf / 27 tiles; 3 ignored L2 real-data tests).

**Phase 6 fixture verification:** `offline_sandbox/raw_data/innsbruck_dem.tif`
PRESENT — 3,554,463 bytes, magic bytes `49 49 2a 00` (`II*\0`, little-endian
TIFF, GDAL-produced with striped layout). Ready to be parsed.

**Outcome:** CLOSED. Workspace green, docs consolidated, fixture verified.
Phase 5 sealed; no further changes to the vector pipeline outside its carried
follow-ups (leaf directories, run-length coalescing, node-POI layer).

---

## PHASE 6 — TERRAIN PIPELINE (OPEN)

**Opened:** 2026-07-17
**Scope (per ARCHITECTURE.md §5, Phase 6):** replace the simulated `Terrain`
engine phase with a real driver in the `terrain` crate:
- Windowed GeoTIFF reads of `innsbruck_dem.tif` (never whole-raster — the P3
  50MB posture applies unchanged).
- Terrain-RGB encoding matching what the client already consumes (`mapbox`
  encoding: base −10000, interval 0.1, the massif parameters, z5–12), WebP
  tiles → `terrain.pmtiles` via the existing Phase-5 PMTiles writer.
- Suspendable under the Surface-v1 budget-yield contract (new checkpoint
  cursor ⇒ checkpoint version bump per house rule).
- Decision point carried from the plan: bake Marching-Squares contours into
  the basemap vs. keep runtime maplibre-contour generation.
**Fixture:** verified present and valid (see P5.SEAL).
**Exit criterion:** compiled terrain archive drives the existing 3D
terrain/hillshade/contours indistinguishably from the massif-built one.

*(Chunks P6.C1… follow below as work begins.)*

---

## P6.C1 — Windowed GeoTIFF reads + Terrain-RGB WebP encoding

**Status:** CLOSED
**Date:** 2026-07-17
**Operator context:** Phase-6 dependency additions in-band: `tiff` 0.11
(default-features off, `lzw` only — weezl core) and `image` 0.25
(default-features off, `webp` only — pure-Rust image-webp). Both cross-compile
clean (no GDAL, no C codecs). The `geotiff` wrapper crate was evaluated and
REJECTED: its sole read path (`GeoTiff::read` → `read_image()`) decodes the
full raster, violating the windowed-read/50MB posture; we sit directly on the
`tiff` decoder it wraps, using `read_chunk` per internal chunk.

**Goal:** `terrain` crate becomes real — windowed DEM reads → Terrain-RGB
(mapbox: base −10000, interval 0.1) → lossless 256×256 WebP tiles.

**Files:** freehike-core/Cargo.toml (+tiff, +image), terrain/Cargo.toml
(deps + `terrain-tile` dev bin), terrain/src/{lib,reader,rgb,webp,main}.rs.

**Design:**
- `reader::WindowedDemReader` — `Decoder::read_chunk` seeks each chunk's byte
  range (TileOffsets/TileByteCounts) and inflates only it; works on tiled and
  striped layouts; GDAL NoData (ASCII tag 42113) resolved to NaN in f64 so
  integer sentinels (−32768) match exactly. Peak heap per window on the
  fixture: 128KB i16 chunk + 256KB f32 + 192KB RGB ≪ 50MB, raster-size
  independent. The fixture is natively tiled 256×256 (LZW, predictor 1) — the
  P5.SEAL note "striped layout" was a magic-bytes-only guess; tiffinfo and the
  decoded chunk grid (8×5) settle it as tiled.
- `rgb::elevation_to_rgb` — exact task equation; scaled value **rounded** (not
  truncated) so f32 representation noise can't slip a 0.1m step, clamped to
  24 bits; NoData encodes as 0m (Terrain-RGB convention). Inverse
  `rgb_to_elevation` shipped for verification. Edge windows pad to 256×256
  with the 0m pixel (pyramid assembly is a later chunk's concern).
- `webp::encode_rgb_lossless` — lossless is load-bearing: lossy quantisation
  would corrupt the low-order 0.1m byte.
- `terrain-tile` dev CLI (bin target — never enters the ffi cross-compile):
  `terrain-tile <dem.tif> <out.webp> [col] [row]`.

**Verification:**
- L1: 8 new tests (synthetic in-memory TIFF reader round-trip incl. NoData +
  out-of-range windows; known Terrain-RGB encodings incl. sea level
  [1,134,160], floor, Everest; 0.05m round-trip sweep; clamp; edge padding;
  WebP byte-exact round-trip). Workspace 139/139 green; fmt clean; clippy
  `-D warnings` clean.
- L2 (`real_innsbruck_dem_window_to_webp`, ignored): fixture parses as
  1800×1260 / 256×256 chunks / 8×5 grid / nodata −32768; window (1,1) decodes
  998.0–2364.0m (Inn valley → Nordkette flank, plausible); full pipeline WebP
  decoded back pixel-for-pixel to source elevations within the 0.1m step.
- CLI run: `offline_sandbox/output/terrain_rgb_1_1.webp` — 67,056 bytes,
  independently validated (`file`: RIFF Web/P; `sips`: 256×256).
- L4: `cargo check --target aarch64-linux-android -p terrain --lib` clean;
  `cargo ndk -t arm64-v8a build -p ffi` stays clean.

**Outcome:** CLOSED. Next chunks: z5–12 tile pyramid (reprojection/resampling
onto XYZ tiles), `terrain.pmtiles` assembly via the Phase-5 writer, Surface-v1
budget-yield cursor (checkpoint version bump), contour bake-vs-runtime study.

---

## P6.C2 — WebMercator reprojection & bilinear resampling (tile pyramid core)

**Status:** CLOSED
**Date:** 2026-07-17
**Operator context:** no new dependencies — pure math on top of the P6.C1
reader/encoder. Continues the desktop-first ladder.

**Goal:** map a WebMercator `(z,x,y)` onto the DEM and emit the full
per-tile pipeline: reprojection → bilinear resampling → Terrain-RGB →
lossless WebP, yielding `RenderedTile { coord, webp }`.

**Files:** terrain/src/{mercator,sample,pyramid}.rs (new),
reader.rs (+GeoTransform), rgb.rs (+grid_to_terrain_rgb), lib.rs (modules +
L2 test), main.rs (z/x/y CLI mode).

**Design:**
- `reader::GeoTransform` — parsed from ModelTiepointTag (33922) +
  ModelPixelScaleTag (33550); general tiepoint form (i,j)→(X,Y) handled,
  ModelTransformationTag (rotated rasters) declared out of scope. PixelIsArea
  (GeoKey 1025=1, GDAL default, fixture-confirmed): integer sample coords are
  pixel CENTERS, hence the ±0.5 in both directions of the transform.
  Fixture: origin (11.099861, 47.450139), scale 0.000278° (~1 arcsec).
- `mercator` — slippy-map math in EPSG:4326 degrees (the DEM's model space).
  Load-bearing subtlety: latitude is NOT linear across a tile — per-row lats
  come from the Mercator inverse `atan(sinh(π(1−2yn)))`, never from
  interpolating corner latitudes. Innsbruck z12 tile = 12/2177/1436
  (cross-checked against the standard formula and asserted in L1).
- `sample::DemSampler` — bilinear needs 4 pixel-center neighbours that
  routinely straddle chunk boundaries, so a bounded LRU of decoded chunks
  (default 16 → ≤4MB f32) sits over the windowed reader. Memory scales with
  the working set, never the raster; 50MB posture intact. NaN-aware
  weighting: NoData/off-raster neighbours drop out and the finite ones
  renormalise; all-NaN → NaN (→ 0m fill in Terrain-RGB).
- `pyramid::render_tile` — 256×256 pixel-center sampling (one Mercator lat
  per row, linear lons per column) → `rgb::grid_to_terrain_rgb` →
  `webp::encode_rgb_lossless`. Bilinear covers both mismatch directions: z12
  slightly oversamples the ~21–31m DEM smoothly (no terracing); low zooms
  (z5–8) downsample with acceptable aliasing — a proper reduction filter is
  logged as a later trade study if overview renders look noisy.

**Verification:**
- L1: 15 new tests (23 total, fixture-independent via the shared
  `test_dem::build` synthetic-GeoTIFF builder now carrying geo tags):
  mercator (z0 world bounds, known Innsbruck z12 tile, in-bounds monotonic
  pixel centers, pole/dateline clamps), reader (transform parse + round-trip,
  missing-tags → None), sampler (exact pixel-center hits, bilinear midpoints,
  NoData drop-out, off-raster NaN, ungeoreferenced rejection), pyramid (the
  task's required test — known z12 tile over an Innsbruck-shaped synthetic
  DEM renders a valid decodable WebP with every pixel on the elevation ramp;
  z5 mostly-outside tile fills 0m but still samples the covered corner;
  anti-terracing: an oversampled row keeps >50 distinct levels). Workspace
  **154/154 green**; fmt clean; clippy `-D warnings` clean.
- L2 (`real_innsbruck_z12_tile_renders_plausibly`, ignored): real-DEM render
  of 12/2177/1436 — all 65,536 pixels finite alpine relief (no NoData fill,
  tile fully inside coverage), decoded elevations agree with direct bilinear
  sampler queries to the 0.1m encoding step at spot-checked pixels.
- CLI: `terrain-tile <dem> <out> z/x/y` renders 12/2177/1436 (79,080 bytes,
  real relief) and 5/17/11 (524 bytes, mostly-fill overview) — both validated
  externally (`file`: RIFF Web/P; `sips`: 256×256).
- L4: aarch64-linux-android `cargo check -p terrain --lib` clean;
  `cargo ndk -t arm64-v8a build -p ffi` stays clean.

**Outcome:** CLOSED. Next chunks: pyramid enumeration (which (z,x,y) sets
cover the DEM extent per zoom), `terrain.pmtiles` assembly via the Phase-5
writer, Surface-v1 budget-yield cursor (checkpoint version bump), contour
bake-vs-runtime study.

---

## P6.C3 — Pyramid enumeration & terrain.pmtiles assembly

**Status:** CLOSED
**Date:** 2026-07-17
**Operator context:** no new external dependencies — `terrain` gains the
in-workspace `tiles` dep (Phase-5 PMTiles byte-level writer + Hilbert IDs),
plus `flate2` as dev-only (archive-shape tests gunzip internal sections).
One surgical, behavior-preserving edit to the sealed vector path:
`tiles::pmtiles::Header` gains `tile_type`/`tile_compression` fields (the
values were hardcoded MVT/gzip at header bytes 98–99); `finalize.rs` pins its
existing values, golden header test byte-identical. New consts
`COMPRESSION_NONE`, `TILE_TYPE_WEBP`.

**Goal:** enumerate every (z,x,y) intersecting the DEM extent for z5–12,
render each through the P6.C2 pipeline, and pack a spec-valid
`terrain.pmtiles` through the shared writer.

**Files:** tiles/src/pmtiles.rs (+2 header fields, +2 consts),
tiles/src/finalize.rs (pin MVT/gzip), terrain/src/archive.rs (new),
mercator.rs (+TileRange/tile_range_for_bounds), reader.rs (+geo_bounds),
lib.rs (+Io error, L2 test), main.rs (.pmtiles CLI mode),
terrain/Cargo.toml.

**Design:**
- `mercator::tile_range_for_bounds` — bbox → inclusive tile rectangle per
  zoom; max-side edges exclusive (a box ending exactly on a tile boundary
  does not pull in the zero-width neighbour), degenerate boxes yield one
  tile, latitudes clamp to the Mercator limit.
- `archive::tile_id_range_sorted` — whole-pyramid enumeration sorted by
  Hilbert tile ID; ascending IDs order z5 before z12 for free (zoom-prefixed
  ID space). Tiles are RENDERED in that order, so payloads stream
  append-only into the data section (`clustered=1`, no shuffle pass); peak
  memory = directory entries + one tile in flight + the sampler's 4MB chunk
  cache.
- Payloads are NOT gzipped (WebP is already entropy-coded): header declares
  `tile_compression=none`, `tile_type=webp`; internal compression stays
  gzip. No payload dedup (n_tile_contents = n_entries) — the 0m-fill
  overview tiles are the only dedup candidates and the win is ~1KB; logged
  alongside the P5 dedup/run-length follow-ups.
- Metadata JSON: `format:"webp"`, `type:"baselayer"`, `encoding:"mapbox"`
  (what maplibre raster-dem reads to pick the decode equation), zoom range,
  bounds. Atomic write: data temp file → header+dirs+metadata+stream-copy →
  fsync → rename.
- **Precision correction en route:** the fixture's pixel scale is exactly
  1 arcsec (0.0002777…78°), not the 0.000278 tiffinfo prints rounded. True
  bounds: lon 11.099861…11.599861, lat 47.100139…47.450139. First test
  expectation (68 tiles) was derived from the rounded scale and WRONG; the
  exact bounds give **62 tiles** (2+2+2+2+2+4+12+36 for z5…z12), now pinned
  in both L1 (mercator, from constants) and L2 (real tags end-to-end).

**Verification:**
- L1: 4 new tests, 27 total. Enumeration math per the task: world bounds at
  z0/z3, fixture bounds consistent across z5–12 (corners inside range, every
  enumerated tile intersects the box, known z12 extent 2174–2179 ×
  1433–1438, 62-tile pyramid total), exact-boundary exclusivity + degenerate
  boxes, pole/dateline clamps. Archive shape: L1 builds a z5–7 archive over
  the synthetic Innsbruck-shaped DEM and parses it back (magic, spec 3,
  webp/none declaration, e7 bounds, strictly-ascending IDs, gapless
  append-only offsets, every payload RIFF/WEBP, IDs exactly the sorted
  Hilbert enumeration, metadata keys). Workspace **158/158 green**; fmt
  clean; clippy `-D warnings` clean.
- L2 (`real_innsbruck_full_pyramid_assembles`, ignored): full z5–12 archive
  from the real DEM — 62 tiles, header spec-shaped, and the archived z12
  Innsbruck tile is byte-identical to a direct P6.C2 render.
- CLI: `terrain-tile <dem> terrain.pmtiles` → 62 tiles, 4,143,672-byte
  archive in ~4.3s release.
- **Independent reader proof (the P5 validation pattern):** the app's own
  `pmtiles` JS reader (4.4.1) opens the archive — spec 3, tileType 4,
  tileCompression 1, z5–12, 62 addressed tiles, clustered, e7 bounds exact,
  metadata JSON intact; `getZxy` returns RIFF/WEBP payloads for z12/z8/z5
  probes (12/2177/1436 = the byte-identical 79,080-byte C2 render) and
  `undefined` outside the bbox.
- L4: aarch64-linux-android `cargo check -p terrain --lib` clean (tiles dep
  cross-compiles — already in the ffi tree via compiler); `cargo ndk -t
  arm64-v8a build -p ffi` stays clean.

**Outcome:** CLOSED. The Phase-6 artifact exists end-to-end. Next chunks:
Surface-v1 budget-yield cursor around the render loop (checkpoint version
bump), frontend wiring (raster-dem source over the OPFS archive), contour
bake-vs-runtime study, optional overview-quality trade study (bilinear
aliasing at z5–8).

---

## P6.C4 — Surface-v1 budget-yield cursor for terrain assembly

**Status:** CLOSED
**Date:** 2026-07-17
**Operator context:** no new dependencies, no engine changes. The cursor is
TERRAIN-LOCAL: its own `TERRAIN_CHECKPOINT_VERSION = 1` counter (bump on any
field or recovery-contract change, house rule), file-based rather than redb —
the engine's v5 checkpoint is untouched, and wiring terrain into the
engine's phase table is deferred to product integration (P9), where the
version-bump rule will apply to whichever side owns the combined cursor.

**Goal:** wrap the P6.C3 render loop in the kill-safe budget-yield contract:
`run_archive_slice(sampler, out, minz, maxz, budget)` →
`SliceOutcome::{Yielded(TerrainCheckpoint), Finished(ArchiveReport)}`,
mirroring the engine's shape.

**Files:** terrain/src/archive.rs (slice machinery), lib.rs (+`Corrupt`
error, L2 sliced test), main.rs (optional `budget_ms` CLI arg).

**Design (the load-bearing parts):**
- `TerrainCheckpoint` = cursor (`last_tile_id` Hilbert ID, `tiles_written`,
  `bytes_written` high-water mark) + identity (zoom range, DEM bounds as
  exact f64 BIT PATTERNS — resume must be against the very enumeration the
  checkpoint was cut from; print-precision comparison would be a lie).
  Serialized as engine-style `key=value` text, written tmp→fsync→rename.
- Durability order per slice: payload bytes flushed+fsynced BEFORE the
  checkpoint that references them commits (P5 house rule — the cursor never
  runs ahead of durable data). Budget checked after each render+write; at
  least one tile of progress per slice regardless of budget; a spent budget
  yields BETWEEN render exhaustion and assembly (P5 encode/assemble split),
  so assembly always starts a slice fresh.
- Resume: identity-validate checkpoint (zoom/bounds/cursor-vs-enumeration),
  truncate the data temp to `bytes_written` (torn-tail discard), then
  REBUILD the directory by walking the RIFF-delimited payloads up to the
  high-water mark — WebP is self-describing (bytes 4..8 = chunk size), so
  the checkpoint stays fixed-size regardless of pyramid scale (no
  per-tile length table to grow at Alps scale).
- Malformed/foreign/mismatched checkpoint state is a HARD `Corrupt` error,
  never a silent restart (silent restarts mask bugs and can interleave
  archives from different parameter sets).
- **Kill-safety fix found in review:** finish-path purge order. Assembly
  originally deleted the data temp before the caller deleted the
  checkpoint; a crash between the two left checkpoint-present/data-missing
  — which resume (correctly) refuses — bricking a pipeline whose contract
  is "safe to re-run after a crash at ANY point". Order is now archive
  rename → checkpoint purge → data purge: every crash window either
  resumes+reassembles idempotently or falls back to a clean fresh start.
- `build_terrain_archive` is now a deadline-free call through the same
  slice path (monolithic == sliced by construction), and it resumes a
  killed sliced run if a checkpoint exists.

**Verification:**
- L1 (4 slice tests, 31 total, workspace **162/162 green**, fmt + clippy
  `-D warnings` clean):
  - `zero_budget_slices_resume_to_byte_identical_archive` — the
    task-required proof: 0ms budget → exactly one tile per slice (6 yields
    for the z5–7 synthetic pyramid), fresh sampler per slice (process-death
    simulation), a torn tail scribbled past the high-water mark after yield
    2, final archive BYTE-IDENTICAL to the uninterrupted run, all temp
    state purged.
  - `finish_crash_window_reassembles_idempotently` — the fixed purge-order
    window: archive renamed + both temp files still present → re-entry
    reassembles identical bytes and completes cleanup.
  - `resume_rejects_foreign_or_mismatched_checkpoints` — zoom-range
    mismatch, missing data file, torn checkpoint → hard `Corrupt`.
  - `monolithic_build_resumes_a_killed_sliced_run` — plain build picks up a
    3-tile checkpoint and lands byte-identical to mono.
- L2 (`real_innsbruck_sliced_run_matches_monolithic`, ignored): full z5–12
  real-DEM pyramid at 50ms budget, fresh sampler per slice — 5 yields,
  byte-identical to monolithic. All 4 L2 tests green.
- CLI: `terrain-tile <dem> <out>.pmtiles 5 12 250` → yield at 45/62 tiles
  (2.8MB durable), resume, finish; output byte-identical (`cmp`) to a fresh
  monolithic run. 4,143,672 bytes, same as P6.C3.
- L4: aarch64-linux-android `cargo check -p terrain --lib` clean; ndk ffi
  build clean.

**Outcome:** CLOSED. Remaining P6: frontend wiring (raster-dem source over
the OPFS archive), contour bake-vs-runtime study, optional overview-quality
trade study. Engine-table integration of the terrain phase lands with P9.

## PHASE 8 — BACKGROUND SCHEDULERS & THERMAL GOVERNANCE (OPEN)

## P8.C1 — Thermal governance: FFI ThermalState + governed throttling core

**Status:** CLOSED
**Date:** 2026-07-17
**Operator context:** operator-directed Surface v1 ADDITION (new enum + two
exported fns; no existing record/enum/callback touched) and one new
dependency, `rayon = "1"` (pure Rust, workspace dep) — both explicitly
directed in the Phase 8 kickoff task, so the §1.5 HITL gates are
operator-signed by construction. Swift/Kotlin shells deliberately NOT
written yet (P8.C2+).

**Goal:** make the compiler survive mobile thermal policy: the shells
report `ProcessInfo.thermalState` / `PowerManager` thermal status through
the FFI, and every compilation loop actively listens and voluntarily
throttles before the SoC kills the process.

**Files:** compiler/src/thermal.rs (new), compiler/src/engine.rs (deadline
checks re-routed), compiler/src/lib.rs (+mod), ffi/src/lib.rs (surface
addition), compiler/tests/thermal_governance.rs (new),
ffi/tests/thermal.rs (new), both Cargo.toml.

**Design (the load-bearing parts):**
- `ThermalState { Nominal, Fair, Serious, Critical }` in a single global
  `AtomicU8` (`Relaxed` — advisory flag, no payload to publish; thermal
  pressure is a DEVICE property, so global is correct and the FFI setter
  is one lock-free store callable from any foreign thread mid-compile).
  Unknown bytes decode as Critical (fail COOL — still makes minimum
  progress, never bricks).
- Policy table: Nominal/Fair = full budget, no pauses; Serious = budget
  honored at 50% + 25ms cooling pause before every block; Critical =
  budget scale 0 (yield NOW, no pause on the exit path).
- `SliceGovernor` replaces the engine's raw `started.elapsed() >= budget`
  closures at ALL SIX deadline sites (passes 1/2/3, finalize encode,
  finalize assembly gate, terrain sim). State re-read at every block
  boundary → a mid-slice downshift lands at the next block. Under
  Critical the existing minimum-forward-progress guarantee degrades a
  still-invoking runner to one block per slice (no livelock); the durable
  checkpoint machinery is untouched — thermal yield IS a normal yield.
- Rayon: the GLOBAL pool is never initialized (one-shot init hazard
  avoided entirely). One custom `ThreadPool` (`pool_width()` = logical
  cores − 2, floor 1 — the spec's "P-cores − 2–3"; P/E distinction is not
  portable, shells can refine later), built lazily via `OnceLock`.
  Governance is ADMISSION, not resizing: `for_each_governed` feeds the
  pool in bounded waves (`WAVE_FACTOR = 4` items/worker), re-reading
  `effective_parallelism()` (Nominal=full, Fair=half, Serious/Critical=1)
  before each wave; width 1 bypasses rayon and runs caller-thread with
  the cooling pause between items. Index claims are CAS-bounded
  (`claim()`) so a wave boundary can never swallow items. This is the
  execution substrate for the upcoming parallel encode stages; the
  sequential engine passes throttle via `SliceGovernor` today.

**Verification:**
- L1: 5 new pure unit tests in thermal.rs (policy table, severity order,
  fail-cool decode, pool headroom, claim boundary). Global-state behavior
  isolated in `compiler/tests/thermal_governance.rs` (own process, mutex
  + Nominal-reset guard): FFI-shaped roundtrip, Critical → immediate
  yield with no sleep on the exit path, Serious → half budget honored +
  pause injected, engine trickles exactly 1 block/slice under Critical
  then resumes to Finished on recovery, executor exactly-once over 200
  items, strict serialism under Serious (peak concurrency == 1),
  mid-batch downshift completes, effective-width table.
- L3-shaped FFI boundary: `ffi/tests/thermal.rs` (own process): enum
  roundtrip across the boundary; end-to-end Critical forces
  `compile_chunk` (300s budget) to `Yielded`, Nominal resumes the same
  job to `Finished`.
- Workspace: **178/178 green ×2 consecutive** (162 prior + 16 new), fmt
  clean, clippy `-D warnings` clean.
- L4: `cargo check -p ffi --lib --target aarch64-linux-android` clean
  (rayon cross-compiles pure-Rust).

**Outcome:** CLOSED. Remaining Phase 8: Swift `BGProcessingTask` +
`thermalStateDidChangeNotification` observer, Kotlin WorkManager FGS +
`OnThermalStatusChangedListener` (P8.C2/C3); first parallel consumer of
`for_each_governed` lands with the terrain engine-table integration (P9).

## P8.C2 — iOS shell: BGProcessingTask scheduler + thermal observer

**Status:** CLOSED (build-verification carried — see Verification)
**Date:** 2026-07-17
**Operator context:** operator-directed. No Rust changes; the Surface v1
bindings were REGENERATED (uniffi-bindgen library mode, Swift + Kotlin)
because the vendored `ios/App/App/FreeHikeFFI/freehike.swift` predated
P8.C1 and had no `ThermalState` — the Kotlin copy in `ffi/bindings/` is
refreshed alongside for P8.C3. All new Swift lives INSIDE the two existing
files (MapCompilerPlugin.swift, AppDelegate.swift): this env is CLT-only
(no iphoneos SDK), so adding new files would mean unverifiable
project.pbxproj surgery.

**Goal:** drive the P8.C1 thermal contract from iOS: observe
`ProcessInfo.thermalState` → `set_thermal_state()`, and run the compile
loop inside `BGProcessingTask` windows with graceful expiration.

**Files:** ios/App/App/MapCompilerPlugin.swift (+~330 lines),
AppDelegate.swift, Info.plist, regenerated bindings (ios vendored copy +
ffi/bindings).

**Design (the load-bearing parts):**
- `ThermalStateBridge`: 1:1 map (`.nominal→.nominal` … `.critical→
  .critical`, `@unknown default → .critical` — fail COOL, mirroring the
  core's unknown-byte rule). Started in `didFinishLaunching`; pushes the
  CURRENT state immediately (notifications only cover changes), and
  `handle(task:)` pushes again at window start — a BGTask can wake a fresh
  process on an already-hot device. Observer queue `nil` (posting thread)
  is safe: the FFI setter is one lock-free atomic store.
- `BackgroundCompileScheduler` (`com.freehike.compiler.sync`):
  registration before `didFinishLaunching` returns (hard iOS requirement);
  `requiresExternalPower = true`, `requiresNetworkConnectivity = false`
  (raw PBF/DEM already fetched; honest "compiles while charging" UX).
  Submission is idempotent (same-id replaces) and re-issued from
  `applicationDidEnterBackground`; submit failure (Simulator, BG refresh
  off) just logs — the job stays runnable via foreground `startJob`, same
  checkpoint.
- Execution loop: 2000ms slices via `compileChunk`. Expiration handler
  only raises an NSLock flag (house idiom, same as cancelJob) — the
  graceful stop IS not starting another slice: the in-flight slice ends
  through the engine's own fsync+rename checkpoint path, so there is no
  native-side state to save inside the ~5s grace. `Yielded` → continue,
  UNLESS `thermal_state() == .critical`: re-invoking would defeat the
  throttle (engine yields after its 1-block minimum every call), so the
  window is handed back and re-requested for after cooldown.
- `Finished` → the archive is already at its final sandbox path;
  native code CANNOT write WKWebView's OPFS (P7 seam), so the "copy to
  OPFS" is delegated: durable `PendingJobStore` record flips to
  `finished`, `backgroundCompile` event fires if a WebView is alive, and
  the JS layer stream-copies into OPFS on resume via new plugin methods
  `enqueueBackgroundJob` / `queryBackgroundJob` / `acknowledgeBackgroundJob`
  (ack of a still-pending job is refused — that's cancel+purge territory).
  `Failed` → marked, NOT rescheduled (fatal per Surface v1; retrying
  overnight burns battery/flash).
- `PendingJobStore`: single-job JSON (atomic write) beside the engine
  state — BGProcessingTask fires in a fresh process, so the job spec must
  be durable; a queue is Phase 9 product territory.
- Info.plist: `UIBackgroundModes = [processing]`,
  `BGTaskSchedulerPermittedIdentifiers = [com.freehike.compiler.sync]`.

**Verification:**
- Bindings regen clean; vendored copy now exposes
  `ThermalState`/`setThermalState`/`thermalState` (diff-checked against
  generated source; enum cases and record fields match all call sites).
- `swiftc -parse` clean on both edited files; `plutil -lint` OK.
- L3b/L4 (device build, real BGTask window via
  `_simulateLaunchForTaskWithIdentifier`, SIGKILL-mid-window resume) is
  CARRIED on the existing "iOS full build/link needs an Xcode machine"
  follow-up — this CLT-only env cannot link UIKit/Capacitor. The chunk's
  logic surface is otherwise fully exercised by the P8.C1 Rust tests
  (Critical→Yielded, resume-to-Finished) that this shell merely drives.

**Outcome:** CLOSED with carried device-verification. Remaining Phase 8:
Android WorkManager FGS + OnThermalStatusChangedListener (P8.C3), device
smoke of the BGTask window when an Xcode machine is available.
