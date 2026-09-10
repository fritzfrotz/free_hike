# Decision Ledger

Technical, load-bearing decisions. Format: optimizing-for / traded-away /
premises / prediction / wrong-if / checkpoint / verdict. Reversible
decisions don't get nodes; one-way doors do. Predictions are scored at
checkpoints. Reversals are recorded before patching.
Prefix convention: DL-### = decision ledger, D### = tracker debt.

## DL-001 · Methodology v2.1 (2026-09-09)
- Optimizing for: decision ownership, review integrity, defensible architecture.
- Traded away: review overhead on load-bearing diffs.
- Premise: v0.1 scope small enough to absorb it.
- Prediction (author, primary): ~80% slowdown; ~10 defects caught by review by 2026-09-22.
- Prediction (Claude, second opinion): <=10% slowdown; >=1 real defect caught by review that v1 would have shipped.
- Wrong if: ship date at risk from review overhead, or reviews rubber-stamped.
- Checkpoint: 2026-09-22. Verdict: ___

## DL-002 · Map-first re-scope for v0.1 (2026-08-21)
- Optimizing for: credibility of the core claim (the phone compiles its own map).
- Traded away: a routing demo in v0.1. Routing gated behind ship.
- Premise: half-baked map + half-baked routing loses to one solid map.
- Prediction: v0.1 ships on time with zero routing code in the demo path; test_graph.tar server path removed rather than fixed.
- Wrong if: the demo feels incomplete to a stranger without routing.
- Checkpoint: 2026-10-20.

## DL-003 · Portugal demo region from a public extract mirror (2026-09-10)
- Optimizing for: a compile long enough that kill-survival and airplane mode mean something.
- Traded away: the trivially fast Innsbruck-fixture demo; ingestion now depends on a third-party mirror's uptime and layout.
- Demo region: Portugal (Geofabrik extract, `europe/portugal-latest.osm.pbf`). Demo area: Lisbon / Sintra / Arrábida. Innsbruck test fixtures unchanged. (Amended 2026-09-10: originally Tirol via download.openstreetmap.fr; the Range/MD5/state-file verification recorded for that mirror does not carry over and must be re-run against Geofabrik before the first fetch.)
- Premise: the Geofabrik Portugal extract is served with Range support and a published checksum, and is small enough to fit the on-device storage preflight while large enough to outlast a single foreground session.
- Prediction (operator): breaks at scale — Android kills the compile before finish.
- Prediction (Claude): Pass 1 at Portugal scale stays under the 50 MB dirty-heap posture (mmap + redb are out-of-core by design); the first on-device break is the terrain phase integration, not memory.
- Wrong if: Pass 1 at Portugal scale breaks the 50 MB posture on the Samsung.
- Fallback if wrong: pre-clipped Lisbon–Sintra–Setúbal box on a static host, same fetch mechanism, one URL change (Option C of the 2026-09-10 sovereignty audit).
- Checkpoint: first Portugal-scale on-device run. Verdict: ___

## Backlog (to backfill in finish-line Phase 3)
Valhalla over GraphHopper · redb · FlatGeobuf · on-device compile over server tiles · 50MB dirty-heap ceiling (out-of-core) · checkpointed resume · OPFS · PMTiles output · terrain real-or-cut
