# Roadmap

FreeHike's claim: the phone does the map server's job. Raw OpenStreetMap
data goes in, a browsable map comes out, entirely on-device, with no tile
server to trust. Everything below is ordered by what proves that claim.

## v0.1 — the sovereign map (in progress, target 2026-10-20)

Scope is closed. Done means:

- Install a release APK on Android.
- Download raw region data (PBF, optional GeoTIFF) for one real region.
- Airplane mode on. The phone compiles the region into PMTiles on-device.
- Browse the result: pan, zoom, place search, GPX track overlay.
- No network access after the initial download. Verified, recorded.
- Compile survives process kills, force-stop, reboot and Doze; resume is
  byte-identical to an uninterrupted run.
- Elevation: real terrain compile or explicitly cut. Never simulated.
- README claims are provenance-graded (verified on device / builds,
  unverified / out of scope).

Explicitly out of v0.1: routing of any kind, iOS hardware verification,
Play Store, multiple regions, new features.

## v0.2 candidates (pick one after v0.1 ships)

- **Lazy compilation.** Split the pipeline: ingest and index a region
  once (parse, spatial index, Hilbert sort, elevation); generate tiles
  deterministically on demand as the user pans, with an LRU cache.
  Motivation: index a whole country, render only the valleys you visit.
  Deferred because it reopens the kill-survival matrix.
- **Routing, step 1.** On-device graph build for one region.
- **Routing, step 2.** Point-to-point pathfinding and snapping. This is
  what "basic routing" honestly means; it is not small.

## v0.3 / research

- **Full routing engine on mobile** (Valhalla or equivalent). Heavy port.
  Graph build is deterministic and streamable, so the path is chunking
  and patience. A neural surrogate was considered and rejected (below).
- **ML-assisted cartographic generalization.** A small on-device model
  helping with feature selection, simplification and label decisions at
  low zooms. Boundary: the model chooses what to omit, never invents what
  exists. The deterministic pipeline stays authoritative for facts.
- **ML data enrichment.** On-device inference of missing attributes
  (trail difficulty from geometry and elevation, surface type from
  context, elevation super-resolution). Boundary: everything ML-touched
  carries an *inferred* provenance label and is never rendered
  indistinguishably from mapped fact.
  Rejected sibling, for the record: implicit neural map representations
  (weights-as-map). Per-tile inference still needs the indexed ingest
  stage, so the expensive part survives; on-device training costs more
  than compiling; server-side training kills the thesis; hallucinated
  geometry is a safety failure. Legitimate research topic, wrong tool.

## Someday

- **Distributed map data.** P2P distribution of raw region extracts and
  diffs; devices verify (checksum/signature) and compile locally. The
  on-device compiler is the decoder that makes this workable: distribute
  small, canonical, verifiable artifacts instead of large mutable tile
  blobs. v0.1 is the prerequisite.
- **Diff-based region updates.** Ingest OSM minutely/daily diffs and
  re-render affected tiles locally instead of full re-download and
  recompile. The strongest practical argument for raw-over-prerendered:
  humanitarian edits land hourly; prerendered tiles go stale.
- **Region picker / multi-region.**
- **iOS hardware verification.** A CI compile job keeps the claim honest
  until then.

## How decisions get made

Load-bearing decisions carry a prediction and are scored at checkpoints.
See `docs/decision-ledger.md`. Process contract: `AGENTS.md` and
`agentic_operating_manual.md`.
