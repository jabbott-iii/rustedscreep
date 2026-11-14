# Status

**Current phase:** P0, kickoff (in progress; gate not passed). Waiting on probe output and ADR acceptance.
**Pinned versions:** candidates found, not pinned yet (VERIFICATION_LOG V1–V13): screeps-game-api 0.23.5, wasm-bindgen and wasm-bindgen-cli 0.2.130, Rust 1.100.0, binaryen version_133. Pinning happens in P1 (ADR-001 and 006 accepted; ADR-002 waits on the probe).
**Target servers:** official MMO (free tier), Seasonal, private server (none running yet). See `docs/PROJECT_CONTEXT.md`.

## Phases
- [ ] P0 Kickoff and verification
- [ ] P1 Build pipeline, loader, snapshot, scheduler, profiler, persistence, consts self-check
- [ ] P2 Bootstrap and movement core
- [ ] P3 Economy
- [ ] P4 Base planner
- [ ] P5 Global spawn queue
- [ ] P6 Remote mining
- [ ] P7 Defense
- [ ] P8 Expansion
- [ ] P9 Late game
- [ ] P10 Offense core
- [ ] P11 Siege breaker
- [ ] P12 Debug, visuals and final deliverables

## Current phase checklist
- [x] Clarifying questions answered (`docs/PROJECT_CONTEXT.md`; unasked items recorded as assumptions)
- [x] Seeded findings re-verified (S1–S15 superseded by V1–V70)
- [x] Groups verified: tooling, cpu-memory, pipeline (V1–V63, V67–V70; each remaining unverified part states an assumption and a fallback)
- [x] Probe built: `js/probe/console-snippet.js` (five pastes, each within the 1024-character console limit) and `js/probe/main.js`; `node js/probe/check.mjs` passes
- [ ] Feature probe results recorded per server: official MMO, Seasonal (user pastes the five console outputs), private server (needs a server)
- [x] ADR-001..006 and 008 proposed with evidence and recommendations; ADR-007 deferred to P4 (no evidence yet)
- [x] ADR-001, 003, 005 and 006 accepted by the user (2026-11-13)
- [ ] ADR-002 and 008 accepted by the user (they wait on the probe results)

## Measurements (latest)
| Metric | Server | Value | Date | Method |
|---|---|---|---|---|

## Open decisions
See `docs/DECISIONS.md`. Accepted: ADR-001 (build tool), 003 (trap recovery), 005 (constants source), 006 (dependencies), 009 (crate names). Proposed: ADR-002 and 008 (wait on the probe), 004 (P1), 007 (P4).

## Blockers
- No probe output yet for any target server (P0 gate).
- No private server, although it is a target. P1's gate needs one (wasm load, trap-test, cold start).
- Local Node is 18.19.1. screeps-api 2.x (deploy) needs Node 22 or 24, and Babel 8 needs ^22.18 or >=24.11 (V10, V11). Upgrading is the user's call.

## Log (newest first)
- 2026-11-13: The user accepted ADR-001 (wasm-bindgen CLI + pinned wasm-opt), ADR-003 (hybrid recovery, halt first), ADR-005 (re-export the crate's constants) and ADR-006 (dependency list) as recommended ("Accept all four"). VERIFICATION_LOG D6 resolution updated. ADR-002 and 008 still wait on the probe.
- 2026-11-13: P0 gate evaluated: not passed (probe results and ADR acceptance pending). Probe split into five console pastes after a review found the 1024-character console limit (V69). `node js/probe/check.mjs` passes on Node 18.19.1 (unit JSON lengths 777, 745, 713, 864, 724; bytes 10/10 match the references; 5 injected faults each fail it). acorn 8.8.1 parses all five units and main.js as ES5.
- 2026-11-13: Independent review of the kickoff changes: 18 findings confirmed and fixed (console limit, V57/V62/V68 corrections, R2/R9 assumptions, S3 coverage via V70, source URLs, stale pointers); 13 refuted.
- 2026-11-13: P0 verification recorded in VERIFICATION_LOG section F: 70 rows (V1–V70) from 6 researchers plus 6 verbatim skeptic passes. 25 S/R rows marked superseded (only their Confidence cell changed); R2 and R9 stay open with assumptions; 22 open rows in total (R2, R9 and later-phase groups). Key findings: official servers announced Node 24 on 2026-04-01 (V62, inferred until the probe confirms); the engine intent-priority table at a90d26f matches S12 (V44); crate 0.23.5 tests natively (V29).
- 2026-11-13: Kickoff answers captured in PROJECT_CONTEXT (MMO free tier, Seasonal, private; fresh respawn; pixels and market; no allies; conservative breaker; no private server yet; stable-only toolchain).
- 2026-11-13: Renamed the placeholder crates to `rustedscreep-consts`, `rustedscreep-core` and `rustedscreep-wasm` (ADR-009) across CLAUDE.md, KIT_README.md, `.claude/` and `docs/`. Evidence: `node --check .claude/hooks/guard-purity.mjs` passes; settings.json parses; 8/8 guard-purity block/allow cases pass on the new paths; `grep -rnIE 'game[-_]consts|bot[-_](core|wasm)'` matches only ADR-009's history.
- 2026-11-13: Claude Code kit installed. Next step: `/kickoff`.
