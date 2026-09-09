# FreeHike — Agent Session Bootstrap (MANDATORY, before any mutating step)

<!-- Canonical agent instructions. Tool-specific files (CLAUDE.md, etc.) are
     thin shims that import this file. Edit HERE, never in the shims. -->

This repo is governed by a binding process contract. The full authorities are
imported below and load at session start — they are in your context now; do not
skip them, do not work from this file alone.

@agentic_operating_manual.md
@ARCHITECTURE.md
@TRACKER.md

## Bootstrap sequence (non-negotiable, every session)

1. The manual, the architecture pillars (P1–P9, incl. fenced mechanical-rule
   blocks), and the tracker are imported above. Open `BUG(blocker)` items in
   TRACKER.md are mandatory chunk-planning input: address, re-triage with the
   operator, or explicitly defer in the plan entry.
2. Read the tail (~150 lines) of `freehike-core/LOOPLOG.md` for latest chunk
   state and open follow-ups. Do NOT read the full log by default.
3. Run `git status` before anything mutating.

## LOOPLOG retrieval policy

LOOPLOG is history, not memory. Durable lessons are promoted out of it (see
session close); what remains is narrative. When touching a gnarly subsystem
(OPFS locking, checkpoint/resume, background schedulers, PMTiles finalize,
thermal), grep LOOPLOG for that subsystem's past traps before designing —
targeted retrieval, never ambient full-log carriage.

## Hard rules (redundant with the manual — repeated because fresh sessions
violate these most)

- All work is chunked (`P<phase>.C<n>`), test-first, with named proofs declared
  before implementation. A chunk without a stated proof is not executable.
- HITL gates: new dependencies, edits to `ARCHITECTURE.md`,
  `agentic_operating_manual.md`, or this file, and anything the manual marks
  HITL. Stop and ask; do not proceed on inference.
- Deviations from an approved spec are permitted only when flagged explicitly
  in the report with rationale and an operator veto offered. Silent deviation
  is a process violation even when the deviation is correct.
- If an approved instruction references a file, path, or state that does not
  exist, say so and propose a relocation — never invent a target to satisfy
  the letter of an instruction.
- `git commit` belongs to the operator unless explicitly delegated this session.
- Never hand-edit `TRACKER.md` (generated) or rewrite LOOPLOG history
  (append-only).

## Process facts

- This project runs a two-model process: an implementing agent writes the
  code; a reviewing model from a different vendor audits plans and
  close-reports adversarially; mechanical
  enforcement (tests, CI, janitor) verifies both; the human operator holds all
  gates. Write reports to be checkable, not persuasive: name the test, the
  file, the count — every claim should be verifiable in under a minute.
- Solo-model sessions are for well-specified grunt work only. If a task turns
  out to require novel design judgment mid-session, pause and surface it to
  the operator instead of improvising architecture.

## Session close (from the manual, restated)

- Run `node scripts/tracker-janitor.mjs --fix`; include the regenerated
  `TRACKER.md` in the session's diff.
- Every LOOPLOG kill entry for a tracked item includes `closes D###`/`closes B###`.
- Promotion valve: before closing, ask whether any kill entry contains a lesson
  with permanent applicability. If yes, promote it — pillar amendment (HITL) or
  mechanical `forbidden-pattern` — rather than leaving it stranded in history.
- Append the LOOPLOG entry per manual §1.6.

## Verification quick reference

- Rust L1: `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` +
  `cargo test` (in `freehike-core/`).
- Frontend L1: `tsc -b` + `npx eslint .` + `npm test` (vitest, 43+ suites —
  keep the count monotonically growing; a shrinking suite needs a stated reason).
- Janitor: `node scripts/tracker-janitor.mjs --check` must be clean.
- Cross-compile gate: `cargo ndk -t arm64-v8a build -p ffi` stays green.
- CI (`janitor.yml`: janitor + frontend-tests jobs) is the ground truth; local
  green is a claim until CI confirms it.
- Known blind spot: iOS/Swift is NOT compile-verified in this environment
  (no Xcode) — flag any Swift change as unverified in the LOOPLOG entry and
  tag accumulating debt under D004.
