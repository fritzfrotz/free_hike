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
