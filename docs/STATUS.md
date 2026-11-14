# Status

**Current phase:** P0, kickoff (not started). Run `/kickoff`.
**Pinned versions:** not yet pinned (P0)
**Target servers:** not yet chosen (P0)

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
- [ ] Clarifying questions answered (`docs/PROJECT_CONTEXT.md`)
- [ ] Seeded findings re-verified
- [ ] Groups verified: tooling, cpu-memory, pipeline
- [ ] Feature probe results recorded per server
- [ ] ADRs proposed, and accepted by the user

## Measurements (latest)
| Metric | Server | Value | Date | Method |
|---|---|---|---|---|

## Open decisions
See `docs/DECISIONS.md` (ADR-001..008 Proposed; ADR-009 crate names Accepted).

## Blockers
None yet.

## Log (newest first)
- 2026-11-13: Renamed the placeholder crates to `rustedscreep-consts`, `rustedscreep-core` and `rustedscreep-wasm` (ADR-009) across CLAUDE.md, KIT_README.md, `.claude/` and `docs/`. Evidence: `node --check .claude/hooks/guard-purity.mjs` passes; settings.json parses; 8/8 guard-purity block/allow cases pass on the new paths; `grep -rnIE 'game[-_]consts|bot[-_](core|wasm)'` matches only ADR-009's history.
- 2026-11-13: Claude Code kit installed. Next step: `/kickoff`.
