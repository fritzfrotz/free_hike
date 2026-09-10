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

## Backlog (to backfill in finish-line Phase 3)
Valhalla over GraphHopper · redb · FlatGeobuf · on-device compile over server tiles · 512MB cap · checkpointed resume · OPFS · PMTiles output · terrain real-or-cut
