# FreeHike

**A hiking navigation app with no backend — your phone compiles its own maps.**

FreeHike is a 100% local-first, serverless, offline hiking app for iOS and Android. There is no tile server, no routing API, no user database, and no cloud bill. Instead of downloading pre-built map tiles from a server someone has to pay for, the app downloads *raw open data* — OpenStreetMap extracts and elevation models from public mirrors — and **compiles its own vector basemap and terrain tiles directly on the device**, in native Rust.

> **Status: in active development.** The compiler pipeline (OSM → PMTiles basemap, GeoTIFF → Terrain-RGB) is implemented and validated against real data on desktop. Background compilation, device integration, and UI polish are in progress. Not yet ready for the trail.

## How it works

```
Public mirror ──▶ raw .osm.pbf + DEM GeoTIFF          (validated, resumable downloads)
      │
      ▼
Native Rust compiler (freehike-core)                   dirty RAM budget: < 50 MB
  Pass 1  stream nodes  → on-flash B-tree index (redb)
  Pass 2  stream ways   → geometry + tags
  Pass 3  simplify, clip, bin into z14 tiles
  Finalize MVT encode → gzip → dedup → PMTiles v3      (sequential writes, flash-friendly)
  Terrain  windowed GeoTIFF reads → Terrain-RGB WebP → terrain.pmtiles
      │
      ▼
OPFS (browser storage) ──▶ MapLibre GL renders fully offline
```

The compiler is **kill-safe and resumable**: every pass checkpoints durably to disk, so iOS's ~5-minute background task windows and Android's WorkManager constraints can interrupt a compile at any point and it resumes exactly where it left off — proven with SIGKILL torture tests. It also throttles itself based on device thermal state.

## Features

- **Fully offline** — vector maps, hiking trail rendering (SAC scale T1–T6), terrain, place labels, all from device-local `.pmtiles` archives
- **On-device map compilation** — pick a region, the phone builds the map (while charging, in the background)
- **On-device routing** — Valhalla compiled to WASM, no routing server
- **GPS tracking** — native background geolocation with GPX export
- **User-owned sync** — optional OAuth-PKCE sync to your own Dropbox/Google Drive; no FreeHike accounts, because there is no FreeHike server

## Tech stack

| Layer | Technology |
|---|---|
| Map compiler | Rust (`freehike-core`): mmap + streaming PBF parse, redb, PMTiles v3 writer |
| Native bridge | UniFFI → Swift / Kotlin, exposed via a Capacitor plugin |
| Background execution | iOS `BGProcessingTask` / Android WorkManager + foreground service, thermal governance |
| UI | React + Vite + Zustand, wrapped in Capacitor |
| Rendering | MapLibre GL JS, PMTiles over OPFS byte-range reads |
| Routing | Valhalla WASM (in a Web Worker) |
| Storage | OPFS for archives; all heavy JS in Web Workers |

## Why compile on-device?

Every offline map app either pays for tile hosting or passes that cost to you. FreeHike's constraint is **zero infrastructure cost, forever** — the only way to do that is to make the device the map factory. This is the hardest version of the product: it demands a hard memory ceiling (a 700 MB OSM extract must be processed in under 50 MB of dirty RAM), flash-write discipline (sequential writes only — millions of random writes destroy mobile NAND), and surviving the OS killing the process at any moment. The architecture exists to satisfy those constraints simultaneously; the full rationale lives in [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Building

**Web / desktop dev:**

```bash
npm install
npm run dev        # dev server
npm run build      # production build
```

**Rust core (desktop-first — all pipeline development is validated on host before touching a device):**

```bash
cd freehike-core
cargo test
cargo clippy --all-targets -- -D warnings
```

**Mobile:**

```bash
npm run build
npx cap sync
npx cap open ios       # or: npx cap open android
# Android native lib:
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --release -p ffi
```

## Fixtures

Real-data fixtures (OSM extracts, DEM rasters, compiled PMTiles) are **not
committed** — they are large and fully regenerable. Rebuild them with:

```bash
brew install osmium-tool   # one-time; needed for clipping the Austria extract
bash scripts/compile_sandbox_data.sh
```

The script downloads the full Austria extract from Geofabrik (~800MB, cached
under `offline_sandbox/raw_data/_cache/`), clips it to the Innsbruck/Tyrol
bbox, and compiles the basemap + terrain PMTiles into `public/local/`.
`innsbruck_dem.tif` must be placed in `offline_sandbox/raw_data/` manually
before running (see the script header for the source).

Expected results (known-good as of 2026-07-18; raw-input checksums drift as
OpenStreetMap data updates, so treat sizes as ~±10% sanity bounds):

| File | Size (bytes) | sha256 (known-good) |
|---|---|---|
| `offline_sandbox/raw_data/innsbruck.osm.pbf` | 19,564,802 | `e01039003ec06270bb5daba687008e440ad2763b5baddb6ac581d726a5eded33` |
| `offline_sandbox/raw_data/innsbruck_dem.tif` | 3,554,463 | `1b5d272f887c78932f6b05861cff7884c1109fa445cda093c8a8cf65cd08e710` |
| `public/local/alps_basemap.pmtiles` | 13,247,300 | `60b9fbaea6e65290f66ede8bf7880a479817eff804d4cd9e86be1676f9894063` |
| `public/local/alps_terrain.pmtiles` | 6,073,840 | `4b0a08dddba629c412a3bdc69cf8074aea358d3652ac9a2973398beb3d3bfc5d` |

**HTML-poisoning check:** a silently failed Geofabrik download (e.g. a 302 to
the homepage saved by `curl -L`) produces a few-KB HTML file with a `.pbf`
extension, and the basemap then silently compiles from garbage. If any fixture
is orders of magnitude smaller than the table above, or `head -c 4` shows
`<!DO` / `<htm` instead of binary (the DEM must start with the little-endian
TIFF magic `II*\0`), delete it and re-run the script — it validates downloads,
but files predating it may be poisoned.

## Project structure

```
freehike-core/    Rust workspace: fetcher, PBF pipeline, tile/terrain compilers, FFI
src/              React app: MapLibre views, workers, OPFS plumbing, stores
android/ ios/     Capacitor shells + background schedulers + thermal bridges
ARCHITECTURE.md   The single source of truth for design decisions
```

## Development process

This codebase is built by an AI agent under human oversight, governed by
[`agentic_operating_manual.md`](agentic_operating_manual.md) — test-first chunks, human-in-the-loop
gates for irreversible decisions, and an append-only build log (`freehike-core/LOOPLOG.md`).

Debt/bug tracking and mechanical architecture rules are enforced by the tracker
janitor (see [`docs/tracker_tags.md`](docs/tracker_tags.md)); `TRACKER.md` is
generated, never hand-edited. Enable the pre-commit check once per clone:

```sh
git config core.hooksPath .githooks
```

## Data & attribution

Map data © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors, licensed under ODbL. Elevation data from public open DEM sources. Fonts: Noto Sans (SIL OFL).

## License

[Apache-2.0](LICENSE) — free to use, modify, and distribute.
