# FreeHike Agentic Operating Manual

**Scope:** All agent-driven work on the `freehike-core` Rust workspace (the zero-cost
edge-compute compiler defined in
`research/On-Device Map Compilation - Feasibility, Architecture, and Implementation Plan.md`).
This manual is the binding process contract between the human operator and the build agent
(Claude). Where this manual conflicts with an ad-hoc instruction in chat, the chat instruction
wins for that turn, but the deviation must be logged (§1.6).

**Authority levels:**
- **AUTONOMOUS** — the agent may do this freely inside a loop.
- **HITL** — Human-in-the-Loop gate: the agent must stop, present the decision, and wait for
  explicit approval in chat before proceeding.
- **FORBIDDEN** — never done by the agent; the human does it themselves if needed.

**Modification of this manual is itself a HITL gate.**

---

## Part 1 — Build Looping Protocol (Plan → Execute → Verify)

### 1.0 The unit of work: the Chunk

All work is decomposed into **Chunks** — the smallest independently verifiable increment
(typically one module, one algorithm, or one seam). Every Chunk carries:

| Field | Meaning |
|---|---|
| **ID** | `P<phase>.C<n>` referencing the implementation plan (e.g., `P3.C2` = Phase 3, chunk 2) |
| **Goal** | One sentence, stated as an observable outcome |
| **Files** | Explicit list of files the chunk may create/modify |
| **Proof** | The named tests that demonstrate the goal (written *before* implementation) |
| **Ladder level** | Which verification levels (§2) must pass to close the chunk |
| **Step budget** | Mutating-step allowance (§1.4) |

A chunk that cannot state its Proof up front is not ready to be executed — it goes back to
planning.

### 1.1 PLAN phase (before any code)

1. **Read first.** Read every file in the declared Files list plus its direct dependents. Never
   edit a file unread in the current session.
2. **Record the plan** before the first mutating step:
   - Create/claim a task in the task tracker (`TaskCreate`) named with the Chunk ID.
   - Append a plan entry to `freehike-core/LOOPLOG.md` (§1.6): numbered steps, each mapped to a
     concrete, observable check ("step 3: `test_mercator_innsbruck` fails with
     `assertion failed` — proves test exercises the right path").
   - Declare any **new dependencies** — adding a crate is a **HITL gate** (§1.5).
3. **Test-first mandate.** The plan must name the failing tests to be written before the
   implementation code. Red → green → refactor, in that order, every chunk.

### 1.2 EXECUTE phase — tool constraints

**Allowed (AUTONOMOUS):**
- `Read` / `Write` / `Edit` / `Grep` / `Glob` **within the repo**, restricted to the chunk's
  declared Files list plus test/fixture directories.
- `Bash`, restricted to this command whitelist:
  - `cargo check`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
    `cargo test …`, `cargo build …` (incl. `--target` cross-builds), `cargo fetch`
  - `cargo ndk …` (Android), `xcodebuild -create-xcframework …` (iOS packaging)
  - repo scripts under `freehike-core/scripts/` (mem gate, kill/resume harness, golden diff)
  - read-only git: `git status`, `git diff`, `git log`
  - read-only inspection: `nm`, `otool -L`, `file`, `shasum`, `ls`, `wc`
- One `cargo fetch` per chunk to resolve dependencies **already approved in the plan**.

**HITL (stop and ask):**
- `git commit` / `git push` (never bundled with other work; always shown as a diff-stat first)
- Adding/upgrading/removing any dependency
- Deleting any file, or modifying anything under `offline_sandbox/raw_data/` or
  `freehike-core/tests/golden/`
- Any edit to the `ffi` crate's public surface (see §1.5)
- Touching files outside the declared Files list (requires a logged plan revision, and if the
  overflow is > 2 files, a HITL check-in)

**FORBIDDEN:**
- Network access other than `cargo fetch` of plan-approved crates
- `--force` flags of any kind; history rewriting; branch deletion
- Weakening an assertion, widening a tolerance, or editing a golden manifest **to make a failing
  test pass** (a legitimate threshold change is a HITL gate with rationale)
- Marking a chunk complete with any failing, skipped, or `#[ignore]`d proof test

### 1.3 REFLECT phase — self-correction rules

After every `cargo` invocation, classify the outcome: `PASS`, `COMPILE_FAIL`, `TEST_FAIL`,
`LINT_FAIL`, or `FLAKY` (same command, different results). Then:

1. **Same-error rule:** if the *same error signature* (same error code + same location) survives
   two consecutive fix attempts, stop patching. Write a **Pivot entry** in the loop log: what was
   tried, why it failed, and a *different* approach. Resume with the revised plan.
2. **Two-strike rule:** if an *approach* (not just an edit) fails twice — i.e., two Pivots on the
   same step — the third attempt must use a structurally different design, and the loop log must
   say what was learned from the failures.
3. **Three-pivot escalation:** three Pivots on one chunk → hard stop, escalate to human with the
   verbatim failing output, the attempted approaches, and a recommendation.
4. **Flakiness is a defect:** a `FLAKY` classification is never retried-until-green; the source
   of nondeterminism becomes the chunk's new first priority (determinism is load-bearing for the
   entire L3 resume-test strategy).

### 1.4 Stopping conditions

- **Step budget:** a *mutating step* = one `Write`/`Edit` or one state-changing `Bash` command
  (reads are free). Default budget **25** mutating steps per chunk; the agent may self-extend
  once to **40** with a logged justification. Exceeding 40 → hard stop, escalate.
- **Retry cap:** max **6** consecutive fix attempts against a single failing test, regardless of
  budget remaining.
- **Green-lock:** a chunk closes only when its full ladder (§2) passes **twice consecutively**
  (guards against order-dependent or flaky greens).
- **Session bootstrap:** at the start of every session, the agent re-reads this manual's Part 1,
  runs `TaskList`, reads the tail of `LOOPLOG.md`, reads the ARCHITECTURE.md pillars and
  `TRACKER.md`, and runs `git status` before doing anything mutating. Open `BUG(blocker)`
  items in TRACKER.md are mandatory chunk-planning input: address, re-triage with the
  operator, or explicitly defer them in the plan entry.
- **Session close:** run `node scripts/tracker-janitor.mjs --fix` and commit the regenerated
  `TRACKER.md` with the session's work. Every LOOPLOG kill entry for a tracked item MUST
  include `closes D###`/`closes B###` — the janitor flags "resolved but not buried" otherwise.

### 1.5 Mandatory HITL gates (recap)

| Gate | Why |
|---|---|
| FFI public surface (any `#[uniffi::export]` item, UDL change, callback trait signature) | Breaking this boundary breaks Swift + Kotlin + JS simultaneously; the human signs off on every surface diff before it is "final" |
| Dependency add/change | Supply-chain and binary-size budget |
| Golden manifest regeneration | Otherwise the agent can silently redefine "correct" |
| Threshold changes (50MB memory gate, tolerances) | Same reason |
| `git commit` / push | Human owns history |
| File deletion / fixture modification | Irreversible |

### 1.6 The Loop Log

`freehike-core/LOOPLOG.md` — append-only. One entry per chunk: plan, per-attempt outcomes,
Pivots, final ladder results, steps consumed. This is the agent's persistent memory across
context windows and the human's audit trail. Deviations authorized ad-hoc in chat are logged
here too.

---

## Part 2 — In-Depth Testing Protocol (the Verification Ladder)

A chunk's success criteria are **exclusively** these tests. "It looks right" is not a state this
protocol recognizes.

### Level 1 — Rust unit tests (pure logic)

- **Command:** `cargo test --workspace --lib && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
- **Targets:** Web Mercator projection (assert against precomputed pairs — e.g., Innsbruck
  `11.3908, 47.2757` → known EPSG:3857 meters), Ramer-Douglas-Peucker (known polyline → expected
  vertices at each ε), Sutherland-Hodgman (segment crossing a tile edge → exact parametric
  intersection vertex), ZigZag/varint codecs, Hilbert/tile-ID math, checkpoint serialization.
- **Rules:** no file/network I/O in L1; inline fixtures ≤ 1KB; codecs get property-based
  round-trip tests (`proptest`); every bug found at any level earns a permanent L1 regression
  test before the fix lands.
- **Bar:** 100% pass, clippy clean at `-D warnings`, fmt clean. No exceptions.

### Level 2 — Golden fixture integration tests

- **Command:** `cargo test --test golden` (CLI invoked against real fixtures)
- **Fixtures (already in repo, never modified — HITL to touch):**
  - `offline_sandbox/raw_data/innsbruck.osm.pbf` — 19,564,802 bytes; 1,900,652 nodes; 198,405
    ways; 3,648 relations; 29,558 path/footway/track ways; 5,231 `sac_scale` ways
  - `offline_sandbox/raw_data/innsbruck_dem.tif` — 1800×1260 px, WGS84 bounds
    11.0999–11.5999°E / 47.1001–47.4501°N
- **Assertions:**
  1. PMTiles header: bbox within fixture bounds, expected zoom range, correct tile type.
  2. Tile-count and per-layer feature-count deltas vs. the committed golden manifest
     (`freehike-core/tests/golden/*.json`) within declared tolerance.
  3. Spot-decode: the z12 tile covering Innsbruck center contains known named features.
  4. **Determinism:** two consecutive runs produce byte-identical archives (SHA-256 equality).
     This is a *hard* requirement — the entire L3 resume strategy depends on it.
- **Render gate (for format changes only):** flipping the encoder default (e.g., MVT → MLT)
  additionally requires the headless-MapLibre render harness to pass on golden tiles.

### Level 3 — Memory & state validation

**3a. Memory footprint gate** — `freehike-core/scripts/mem_gate.sh`
- Runs the CLI compile of the full Austria PBF
  (`offline_sandbox/raw_data/_cache/austria-latest.osm.pbf`, ~767MB) while sampling the process
  every 500ms.
- **Metric honesty:** the 50MB budget is *dirty anonymous memory*, not total RSS. On macOS dev
  machines, total RSS includes clean mmap pages that Jetsam ignores — the script must read the
  dirty figure (`footprint(1)` / `vmmap --summary`), and on Linux CI, `RssAnon` from
  `/proc/<pid>/status`. Gating on plain RSS would fail spuriously the moment we mmap the PBF.
- **Bar:** peak dirty-anon < **50MB** (fail-hard), plus an in-process allocator peak counter as a
  second opinion asserting peak heap < 40MB. Device-true numbers (Instruments / Perfetto) are a
  Phase 8 exit criterion, not a per-chunk gate.

**3b. Idempotent checkpoint / kill-resume torture** — `freehike-core/scripts/kill_resume_test.sh`
- Loop: launch CLI → `kill -9` after a random 0.5–10s → relaunch → repeat until `Finished`.
- **Assertions:** final archive SHA-256 equals the uninterrupted run's SHA-256; redb opens clean
  after every kill (no recovery errors); no work re-done beyond the last un-checkpointed chunk
  (verified via chunk-ID logging); no partial tile records ever precede a checkpoint commit.
- **Bar:** 25 random-kill cycles green per chunk during development; **100 cycles** green as the
  Phase 7 exit criterion.

### Level 4 — FFI / cross-platform compilation gates

- **Commands (all must pass):**
  - `cargo build --release --target aarch64-apple-ios -p ffi`
  - `cargo build --release --target aarch64-apple-ios-sim -p ffi`
  - `cargo ndk -t arm64-v8a build --release -p ffi`
  - `uniffi-bindgen` generation, then compile-check the *generated* bindings
    (`swiftc -typecheck`, `kotlinc`) — generated code that doesn't compile is an L4 failure even
    if the Rust builds.
  - xcframework assembly script completes.
- **Static checks:** `nm` confirms expected `uniffi_*` symbols; clippy config denies `unwrap()`
  / `expect()` in the `ffi` crate; every FFI entry point wraps the core in `catch_unwind` (a
  Rust panic must surface as a typed error across the bridge, never a native crash).
- **Budget:** warn at > 15MB per-arch release binary; investigate before proceeding.
- **Rule:** no module is marked "done" — regardless of L1–L3 status — until L4 is green **and**
  the FFI surface diff has passed its HITL gate (§1.5).

### Ladder application matrix

| Chunk type | L1 | L2 | L3a | L3b | L4 |
|---|---|---|---|---|---|
| Pure geometry / codec | ● | | | | |
| PBF parser / redb indexing | ● | ● | ● | | |
| Transform + encode pipeline | ● | ● | ● | | |
| Checkpointing / state machine | ● | ● | ● | ● | |
| Terrain (GeoTIFF/contours) | ● | ● | ● | | |
| FFI crate / bridge | ● | ● | | | ● |
| **Phase exit** | ● | ● | ● | ● | ● |

### Reporting format (end of every loop)

```
CHUNK P<x>.C<y> — <status: CLOSED | ESCALATED | IN PROGRESS>
Steps: <used>/<budget>   Pivots: <n>
Ladder: L1 ✅  L2 ✅  L3a ✅  L3b —  L4 —   (× 2 consecutive: yes/no)
Diff: <files changed, +/- lines>
Open risks / notes: <anything the human should know>
Next: <proposed next chunk>
```

Escalations always include verbatim failing output — never a paraphrase.
