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
