---
name: module
description: Implement one bot module or roadmap item end to end with the project protocol (pre-brief on CPU and caching, verify game rules, pure logic plus tests first, WASM glue second, checks, reviews, post-summary, STATUS update). With no argument it picks the next incomplete item of the MMO core track from docs/STATUS.md and docs/ROADMAP.md.
argument-hint: "[module or roadmap item, e.g. P3.hauling, PS3.dummy-base, SE2.profile]"
---

# /module $ARGUMENTS

## 0. Pick and orient
- No argument: take the first unchecked item of the current MMO core phase in `docs/STATUS.md`. If the previous phase's gate is not passed,
  stop and say so. Optional-track items (PS#, SE#) run only when named, and only after every phase they build on (core or optional) has passed its gate.
- Read the item's section in its track's roadmap (`docs/ROADMAP.md` for P#, `docs/ROADMAP_PRIVATE.md` for PS#, `docs/ROADMAP_SEASONAL.md` for SE#), the related SPEC section, and the nearby code, tests and config.
  Do not assume a helper exists. Search for it.

## 1. Pre-brief (no more than 8 lines, then proceed)
State:
- The CPU and performance trade-offs.
- The cache strategy and its invalidation trigger.
- The CPU tier and the time-slicing plan.
- The data layout.
- The expected JS crossings and intents per tick.
- The main risks.
Stop and ask instead of proceeding only if the item needs a decision that belongs to the user, such as an ADR, a breaking change to a persisted
format, or a new dependency.

## 2. Facts first
List every game rule or number the module depends on. Run `/verify` for any that are not `verified` in the log before writing code that uses them.

## 3. Implement (smallest complete change)
1. Pure logic in `rustedscreep-core`, with unit and property tests whose expectations derive from `rustedscreep-consts`.
2. Config fields for every threshold, mirrored in `docs/TUNING.md`.
3. Glue in `rustedscreep-wasm`: snapshot fields and intent issuance. Add profiler spans for the new subsystem.
4. If the behavior can only be tested in-game, add a native scenario test (`docs/TEST_PLAN.md` §7) and a live MMO check (§8). If it needs a scripted or destructive scenario, add it as a PS3 entry instead (optional track, §3/§5). Never make a core gate depend on it.

## 4. Validate honestly
- Run `/check`. Fix the root cause of each failure. Never relax a lint or test to pass.
- Run `hot-path-reviewer` if the change touches the tick path, and `combat-math-auditor` for defense or offense.
  Apply the high-severity findings and list the rest.

## 5. Post-summary and record
- Done, stubbed (with reason), and test next.
- Files changed. Checks run with their results. Checks not run and why. Assumptions. Residual risks.
- Add one log line to `docs/STATUS.md` with the evidence and tick the item.
- Offer a commit message (`/commit-msg`). Never commit.
