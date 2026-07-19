# FreeHike — Definitive Architecture & Master Implementation Plan

> **THIS IS THE SINGLE SOURCE OF TRUTH.** (Consolidated 2026-07-17)
>
> This document supersedes all prior architectural references, which are retained for
> historical/citation purposes only:
> - `research/Geospatial App Architecture Research.pdf` (June 2026 — app-level local-first spec; its full text previously lived at the repo root as `architecture.md`)
> - `research/Client-Side Map Compilation Feasibility.pdf` (July 2026)
> - `research/On-Device Map Compiler Blueprint.pdf` (July 2026)
> - `research/On-Device Map Compilation - Feasibility, Architecture, and Implementation Plan.md` (July 2026 — synthesis + original master plan)
> - `research/Offline Map App Architecture & Build Plan.pdf` (July 2026 — frontend build plan)
> - `implementation_plan_phase3.md` (June 2026 — Overpass/Flatbush frontend phase, shipped)
>
> Process rules (chunking, HITL gates, verification ladder) live in
> `agentic_operating_manual.md`. The append-only build history lives in
> `freehike-core/LOOPLOG.md`. Neither is superseded by this document.

---

## 1. The Paradigm: Zero-Cost Edge Compute

FreeHike is a 100% local-first, serverless hiking-navigation app. There is no backend:
no tile server, no routing API, no user database. Two architectural eras compose it:

1. **The local-first app shell (June 2026).** A Capacitor-wrapped React + MapLibre GL JS
   WebView. Map rendering from PMTiles via byte-range reads, OPFS binary storage,
   Flatbush spatial indexing, Valhalla-WASM on-device routing, OAuth-PKCE user-owned
   cloud sync. All heavy JS work in Web Workers; zero-copy `ArrayBuffer` transfer.
2. **The edge-compute compiler pivot (July 2026, `freehike-core`).** Instead of
   downloading pre-compiled tiles from a CDN we pay for, the user's device downloads
   *raw* open data (OSM `.osm.pbf` + DEM GeoTIFF from public mirrors) and **compiles its
   own `basemap.pmtiles` / `terrain.pmtiles` locally** in native Rust, bridged into the
   WebView via UniFFI + Capacitor plugins.

The pivot is justified by the zero-cost constraint, not UX: a CDN download beats a local
compile whenever a server exists. Ours doesn't, by design. This is the hardest possible
version of the product and it works **only** if every pillar below is respected
simultaneously.

## 2. Core Architectural Pillars (non-negotiable)

**P1 — Native Rust, never WASM, for the compiler.** WebView WASM is capped (4GB wasm32;
engine-capped wasm64), SharedArrayBuffer/COOP/COEP is fragile on Capacitor scheme
handlers, and a 700MB PBF parse inside a WebView is mathematically guaranteed to breach
the Jetsam/LMKD ceiling (~1.5–2GB for WebViews, far less for background tasks). The
compiler is pure Rust compiled for `aarch64-apple-ios` / `aarch64-linux-android`, bridged
with Mozilla UniFFI (proc-macros; generated Swift/Kotlin), exposed to JS through a
Capacitor `MapCompilerPlugin`. **Bulk data never crosses the JS bridge** — a bbox goes
down; progress events come up.

**P2 — mmap + two-pass stream parsing, never an in-memory graph.** The PBF is mmap'd
read-only: clean, file-backed, OS-evictable pages that do not count against the
process's dirty-memory limit. Pass 1 streams DenseNodes (delta/zigzag decode, StringTable
block prefilter, Web-Mercator projection) into a **redb** B-tree on flash
(`NodeID → (x,y)`). Pass 2 re-streams Ways (per-pass prost views of the same bytes; way
blocks that fail the StringTable relevance probe are skipped without deserializing),
joins refs against redb, and materializes geometry **one way at a time**, dropped
immediately after use.

**P3 — Hard memory budget: dirty RSS(anon) < 50MB.** Project constants
`RAM_CEILING_BYTES = 50MB` and `REDB_CACHE_BYTES = 32MB` (headroom for decode buffers,
in-flight batches, FFI overhead) are enforced by **compile-time asserts** — a budget
violation is a build failure. Batched redb commits (default 10,000 rows per write txn,
never per-node) keep both fsync count and B-tree churn coarse. Blob/inflate caps
(BlobHeader < 64KiB, payload ≤ 16MiB, zlib-bomb-proof exact-size inflate) bound the
largest transient allocation.

**P4 — Idempotent, budget-yielding, kill-safe state machine.** The public FFI contract
(Surface v1) is `compile_chunk(job, budget_ms, callback) → Finished | Yielded | Failed`.
Durable checkpoints (currently **v6**: `spec_hash`, `phase`, `pbf_byte_offset`,
`pass2_byte_offset`, `pass3_last_way_id`, `pass5_last_tile`, `blocks_done`,
`bytes_written`) are written fsync+atomic-rename, always *behind* durable data — a
checkpoint never runs ahead of what's on disk. `spec_hash` fingerprints the job
parameters (bbox, zooms, input paths): a checkpoint whose fingerprint doesn't match the
incoming spec (jobId reused for a different region) is never resumed — the engine purges
the stale state loudly and restarts fresh, never bricking the job. Resume is by job identity: the foreign layer can't feed state back, only
re-invoke; disk is the sole carrier. Proven on-device: SIGKILL mid-compile loses
nothing, resume is exact and non-duplicating. Any checkpoint format change bumps the
version and old checkpoints are refused loudly (never guessed at). This contract is what
survives iOS's ~295s `BGProcessingTask` guillotine and Android's 6h/24h foreground-service
cap (Phase 8 wires the schedulers).

```
rule-id: P4a
forbidden-pattern: ^panic\s*=\s*"abort"
paths: freehike-core/**
# UniFFI converts Rust panics into typed foreign errors by UNWINDING;
# panic="abort" would turn every internal bug into a native crash.
# Line-anchored so prose/comments discussing the setting don't trip it.
```

**P5 — Sequential PMTiles v3 output, never SQLite.** Millions of tiny random writes
destroy mobile NAND (write amplification), drain battery, and trigger thermal
throttling. Tiles are encoded (MVT extent 4096, ZigZag command integers, gzip), FNV-1a64
deduplicated (byte-verified on hash hit), appended sequentially to a tmp data file, then
assembled Hilbert-ordered (`clustered=1`) with exact-offset 127-byte header + varint root
directory, atomic tmp+rename. MLT is a possible future encode stage behind a format flag
(R4) — MVT+gzip ships first and is what's implemented.

```
rule-id: P5a
forbidden-pattern: ^\s*\S*sqlite\S*\s*=|^name = "[^"]*sqlite
paths: freehike-core/**
# First alternative: direct dependency lines in Cargo.toml. Second:
# resolved-graph entries in Cargo.lock, so TRANSITIVE sqlite deps are
# caught too. Limitation: Cargo table syntax ([dependencies.rusqlite])
# is not matched by the first alternative — the Cargo.lock check is the
# safety net that still catches the resolve.
```

**P6 — Hostile-mirror ingestion.** Every download is validated before it is trusted:
resumable Range requests, magic-byte checks (PBF `OSMHeader` blob; TIFF `II*\0`/`MM\0*`),
Content-Length sanity. This permanently encodes the Geofabrik-HTML-redirect lesson (a
302-to-homepage saved as a "successful" `.pbf` poisoned the pipeline for weeks). TLS is
pure-Rust rustls — zero OpenSSL in the mobile cross-compile.

```
rule-id: P6a
forbidden-pattern: ^\s*(native-tls|openssl)\S*\s*=|^name = "(native-tls|openssl)
paths: freehike-core/**
# First alternative: direct dependency lines in Cargo.toml. Second:
# resolved-graph entries in Cargo.lock (transitive OpenSSL sneaking in
# via a feature flag fails CI). Limitation: Cargo table syntax
# ([dependencies.openssl]) is not matched by the first alternative —
# the Cargo.lock check is the safety net.
```

**P7 — The OPFS seam.** The WebView renders through
`WorkerPMTilesSource → mapData.worker → OPFS SyncAccessHandle` synchronous byte-range
reads (the default Capacitor file protocols are disqualified: WKURLSchemeHandler drops
Range headers on iOS; Capacitor's Android file server overflows 32-bit byte offsets past
2.14GB). Natively-compiled archives land in the app sandbox, which is *not* OPFS —
finished archives are **stream-copied into OPFS post-compile** (option (a); a native
byte-range read path is the fallback if storage pressure demands).

**P8 — Frontend performance discipline.** High-frequency compile telemetry never touches
React state: Capacitor listeners mutate `useRef` sinks polled by a
`requestAnimationFrame` loop that writes DOM/CSS directly. Global state is **Zustand**
(slice subscriptions, mutable from outside the component tree), not React Context. The
MapLibre canvas stays permanently mounted (CSS visibility toggling) to preserve the WebGL
context. Styling is fully offline: vendored glyphs (Noto Sans SDF `.pbf` ranges under
`public/`), local sprites, `pmtiles://` sources; theme switching via
`map.setPaintProperty()`, never `setStyle()` teardown.

```
rule-id: P8a
forbidden-pattern: \.setStyle\(
paths: src/**
```

```
rule-id: P8b
forbidden-pattern: createContext\(
paths: src/**
# Global state is Zustand, not React Context (P8).
```

**P9 — Thermal & background governance (Phase 8).** Rayon pool capped to P-cores − 2–3;
poll `ProcessInfo.thermalState` / Android `THERMAL_STATUS` and voluntarily downshift at
`.serious`. Honest UX on iOS: "will compile while charging" — never a fake ETA.

## 3. Memory-Constraint Summary

| Constraint | Value | Enforcement |
|---|---|---|
| Dirty heap ceiling (Rust core) | **< 50MB RSS:anon** | `const` assert; L2 test evidence (75MB max RSS *total incl. clean mmap pages* for full Innsbruck run) |
| redb page cache | **32MB** | `Builder::set_cache_size`, `const` assert `< RAM_CEILING` |
| mmap'd PBF pages | unlimited (clean/evictable) | read-only map; excluded from dirty budget by OS accounting |
| redb commit granularity | 10,000 rows/txn | `insert_coords_batched` / `insert_ways_batched` |
| Largest transient alloc | ≤ 16MiB (blob inflate) | scanner caps + exact-size zlib limit |
| Geometry residency | one way at a time | assemble → simplify → clip → drop |
| WebView JS heap | ~1.5–2GB Jetsam ceiling | no bulk data over the bridge; workers + transferables |
| WASM (routing only) | 512MB Valhalla cap + OOM recovery loop | existing app shell |

## 4. Pipeline Dataflow (implemented through Phase 5)

```
mirror ──fetcher (Range+magic bytes)──▶ raw .osm.pbf / .tif (app sandbox)
.osm.pbf ──mmap──▶ Pass 1: nodes ──▶ redb COORDINATES (id → WebMercator x,y)
          └─────▶ Pass 2: ways  ──▶ redb WAYS (delta+zigzag+LEB128 refs)
                                   + WAY_TAGS (layer, class, sac_scale, name)
Pass 3: per way → assemble → RDP simplify (ε per zoom) → Liang-Barsky clip per
        z14 tile (+64/4096 buffer; Amanatides-Woo grid traversal)
        ──▶ redb TILE_FEATURES v4 (z,x,y,way_id → layer|class|sac|name|segments)
Finalize: per tile → MVT (4 named layers: highway/waterway/natural/landuse;
          attrs class + sac_scale [highway only] + name) → gzip → FNV-1a64 dedup
          → Hilbert-clustered PMTiles v3 (atomic rename, index purged after)
                                   ──▶ {job_id}.pmtiles ──copy──▶ OPFS ──▶ MapLibre
Terrain (Phase 6, NEXT): .tif ──windowed GeoTIFF reads──▶ Terrain-RGB WebP tiles
          (mapbox encoding: base −10000, interval 0.1; massif params, z5–12)
          ──▶ terrain.pmtiles;  contours stay runtime-generated (maplibre-contour)
          unless baking wins the Phase-6 trade study
```

Everything runs under the P4 budget-yield contract; every pass has its own durable
cursor. Real-data fingerprint (19.5MB Innsbruck extract, 250ms budget, release):
1,900,652 nodes / 91.5k ways / 97,619 tile features / 1,030 tiles / 3.4MB archive /
~3.6s, validated by the app's own `pmtiles` JS reader down to UTF-8 labels
(»…straße«) and all six SAC grades on the MVT wire.

## 5. Master Implementation Plan & Status

Guiding rule: **desktop-first** — every core phase is validated as host-side Rust
against the Innsbruck fixtures before touching a device.

| Phase | Scope | Status |
|---|---|---|
| 0 | Cargo workspace scaffold + UniFFI walking skeleton | ✅ CLOSED (P0.C1) |
| 1 | UniFFI bridge, native shells, WebView wiring, Android e2e | ✅ CLOSED (P1.C1–C3) |
| 2 | Hostile-mirror fetcher (+ Phase-7 state machine pulled forward: Surface v1, checkpoints, kill-resume torture proof) | ✅ CLOSED (P2.C0–C4) |
| 3 | Pass 1: mmap → DenseNodes → redb, StringTable prefilter | ✅ CLOSED (P3.C1–C4) |
| 4 | Pass 2 geometry + RDP + Liang-Barsky clip + z14 tile binning | ✅ CLOSED (P4.C0–C2) |
| 5 | MVT encode + PMTiles v3 assembly + tag pipeline (class, sac_scale, name) + frontend style/glyphs/labels | ✅ **CLOSED & SEALED** (P5.C1–C4 + commits `cbb0e05`, `01fed9f`, `d3481de`, `555207e`) |
| **6** | **Terrain pipeline: windowed GeoTIFF → Terrain-RGB WebP → terrain.pmtiles; contour bake-vs-runtime decision** | ⏳ **IN PROGRESS** — P6.C1–C4 closed: windowed reads → Terrain-RGB WebP → WebMercator bilinear pyramid → terrain.pmtiles (62 tiles z5–12, JS-reader-validated) under the Surface-v1 budget-yield cursor (kill-safe, byte-identical resume); remaining: frontend wiring, contour study |
| 7 | Idempotent state machine | ✅ done early (inside Phase 2); torture-cycle expansion optional |
| 8 | Background schedulers (BGProcessingTask / WorkManager FGS) + thermal governance | ⏳ **IN PROGRESS** — P8.C1 closed: FFI `ThermalState` + atomic governance core (`SliceGovernor` slice throttling, capped rayon pool with wave admission). P8.C2 closed: iOS shell (`ThermalStateBridge` observer + initial poll, `BGProcessingTask` `com.freehike.compiler.sync` external-power window, 2s-slice loop with expiration-flag graceful stop, durable `PendingJobStore` + JS-side OPFS import seam); device smoke carried on the Xcode-machine follow-up. P8.C3 closed: Android shell (`ThermalStateBridge` PowerManager listener + initial poll, charging-constrained WorkManager `CoroutineWorker` promoted to dataSync FGS, `Result.retry()` backoff as thermal-Critical cooldown, SharedPreferences `PendingJobStore`; APK + merged-manifest verified). P8.C4 closed: MainActivity → Kotlin, eager `libfreehike_ffi` JNI init + boot-time thermal arm (dex-verified). Remaining tail: device smokes (both platforms), JS `backgroundCompile` listener + OPFS import (Phase 9) |
| 9 | Product integration: region picker → compile → OPFS copy → hot-swap | ⏳ **IN PROGRESS** — P9.C1 closed: background-job handoff orchestration (Zustand `isBackgroundCompiling`/`backgroundProgress`/`pendingHandoffJobs`, cold-boot `queryBackgroundJob` discovery + `backgroundCompile` doorbell listener, stream-copy sandbox archive → OPFS via opfsMover, acknowledge-after-durable-close, `setActiveRegion` hot-swap; byte progress bypasses React via ref sink + rAF direct-DOM paint in `BackgroundHandoffBar`). Note: archives ingest as `{jobId}.pmtiles`, not over the live `alps_basemap.pmtiles` — the worker's persistent SyncAccessHandle holds an exclusive OPFS lock on the bound file, and same-name swaps are no-ops in `loadOfflineRegion` by design. P9.C2 closed: activeRegion persistence (zustand/persist over localStorage, partialized to the region binding; MapView cold-boot replay in the 'load' handler validates OPFS files before binding, drops evicted regions via `clearActiveRegion`) + `RegionPicker.tsx` sheet (hardcoded Innsbruck test regions → `enqueueBackgroundJob` z5–14; confirm hard-disabled while the single-job native record is pending). P9.C3 closed: fixed-reticle custom region selector (`RegionSelectorOverlay.tsx`: pointer-transparent mask + chrome unmount, pitch→0 ease, 4-corner unproject min/max bounds, direct-DOM bounds readout; shared `enqueueRegionDownload` in `regionCompiler.ts` unifies picker presets + custom FAB). P9.C4 closed: pmtiles protocol registry root-cause fix (`services/pmtilesRegistry.ts` — getKey MUST be scheme-stripped `local/<file>` to match pmtiles v4 lookups; the old full-URL key never matched, so ALL tile traffic silently fell back to HTTP `public/local/` twins and OPFS never served MapLibre; globalThis singleton + addProtocol once at module eval + fail-loud miss guard; verified 5/5 reload boots serving 100% from OPFS, zero /local/ HTTP). Remaining: device smokes (must include a `{jobId}.pmtiles` hot-swap render — structurally broken pre-C4, never exercised) |
| 10 | Hardening & release: flash-write telemetry, mirror etiquette, store review | ◻ pending |

**Carried follow-ups:** tracked as inline `DEBT(D###)`/`BUG(B###)` tags in code —
see the generated `TRACKER.md` (authority model + tag spec: `docs/tracker_tags.md`).

## 6. Risk Register (condensed; full prose in the deprecated research docs)

- **R1 iOS background windows** (High) — opportunistic ~295s slices; honest queue-UX; P4 is the mitigation.
- **R2 Flash write amplification** (High) — P5 sequential writes, coarse commits, aggressive intermediate purge, permanent region cache.
- **R3 Thermal throttling** (High) — P9 governance.
- **R4 MLT maturity** (Medium) — ship MVT+gzip; MLT behind a flag gated on a golden render test.
- **R5 Mirror fragility/etiquette** (Medium) — P6 validation; bounded mirror list; budget for a dumb static mirror.
- **R6 JSI-vs-Capacitor** (resolved) — Capacitor + UniFFI stands; no bulk data crosses the bridge, so JSI's advantage is moot. Do not relitigate.
- **R7 OPFS seam** (Medium) — option (a) stream-copy; revisit under storage pressure.
- **R8 App Store review** (Low-Med) — honest `BGProcessingTask` use; foreground-compile fallback story.
- **Storage preflight** — transient footprint ~2.5–3.5GB Alps-scale; check before starting.

## 7. Fixtures & Verification Assets

| Fixture | Size | Role |
|---|---|---|
| `offline_sandbox/raw_data/innsbruck.osm.pbf` | 19.5MB | Vector-pipeline golden input (1.9M nodes, 265 blocks) |
| `offline_sandbox/raw_data/innsbruck_dem.tif` | 3.55MB | **Phase 6 input** — verified `II*\0` little-endian GDAL GeoTIFF, present and parseable |
| Synthetic PBF builders (`pbf::fixtures`) | — | Deterministic L1 coverage, feature-gated out of production builds |

Verification ladder per `agentic_operating_manual.md`: L1 = workspace tests + clippy
`-D warnings` + fmt, green-locked ×2; L2 = ignored real-data tests; L4 = aarch64
cross-compiles (`cargo ndk -t arm64-v8a build -p ffi` must stay clean).

## 8. Generated UniFFI Bindings — Sync Rule

The UniFFI-generated bindings exist in **two places** and are vendored, not
built on the fly:

1. `freehike-core/ffi/bindings/` — canonical generator output
   (`freehike.swift`, `freehikeFFI.h`, `freehikeFFI.modulemap`, `uniffi/`).
2. The native OS trees that actually compile them:
   - iOS: `ios/App/App/FreeHikeFFI/` (Swift + header + modulemap)
   - Android: `android/app/src/main/java/uniffi/freehike/freehike.kt`

Any change to the FFI surface (`ffi` crate) MUST regenerate the bindings
(`cargo run -p ffi --features cli --bin uniffi-bindgen`) and re-vendor them
into **both** OS trees in the same commit. A drifted copy fails at runtime,
not at build time (JNA/modulemap symbol lookup), so treat "bindings differ
between locations" as a broken build. These generated files are exempt from
SPDX headers (`scripts/add_spdx_headers.sh` skips them).
