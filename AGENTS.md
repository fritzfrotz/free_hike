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
- Anything written to a tracked file (LOOPLOG, reports, code, docs)
  names a device or machine by model and OS version only — never
  serials, UDIDs, hostnames, login names, home-folder paths, email
  addresses or local network addresses. The janitor's privacy pass
  checks staged content at pre-commit and the tree in CI for the
  common shapes; the rule holds whether or not the check catches it.
  A false positive is fixed by tightening the pattern, never by an
  exemption.
- On-device runs: raw captures and device detail (log lines,
  device-side paths, process and job identifiers, device log
  timestamps) stay in device-logs/, which is git-ignored and never
  committed. The LOOPLOG entry for a device run is a summary: date,
  device model and OS version, build identity (the main commit hash;
  for a pre-merge build, the chunk ID plus the APK size and checksum,
  never a wip hash), what was done, pass/fail per item, findings in
  plain words.

## Operator interaction (v2.1 — see manual Part 3)

- Default is DECISION mode: before implementing, present options,
  trade-offs, a recommendation and why, then STOP and wait for the
  operator's call. "just do it" delegates one task; mode resets after.
- For load-bearing choices (load-bearing as defined in manual §2
  Reporting format): steelman the losing option and state what
  would have to be true for the recommendation to be wrong.
- Every close-report leads with the risk-ranked review summary
  (manual Part 2 reporting format, v2.1 amendment). The operator reviews
  it against the diff at a depth they choose and will challenge at
  least one thing per review. Answer the challenge; do not comply
  reflexively. If the challenge is wrong, say so and defend the code.
- Honesty: never open by agreeing; disagree before doing work; concede
  to arguments, never to pressure; "I don't know" is a valid answer.
  Full contract in manual Part 3.

## Handoff and budget (added 2026-09-23)

- At AGENT-CLOSED, stage the chunk and stop. The operator commits it to
  a `wip/<chunk>` branch and pushes, so a review chat can clone it. Main
  is untouched. The operator deletes the wip branch once the real commit
  lands on main.
- Do not start implementing chunk N+1 while chunk N is uncommitted
  (dirty tree). Stop and say so. "go ahead anyway" from the operator
  overrides it for that chunk.
- A step budget is the operator's leash. Reaching it means stop and ask;
  changing it is an HITL gate, same tier as manual edits.
- Close-reports list risks and ask for the veto. They never propose the
  review challenge or "questions I would ask in your place."

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
