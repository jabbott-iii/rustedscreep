---
name: verify
description: Verify Screeps game rules, constants, API signatures or tool behavior against sources of truth, then record them in docs/VERIFICATION_LOG.md, rustedscreep-consts, and constant-derived tests. Use before writing code that depends on any game number or engine behavior not already verified for the pinned version.
argument-hint: "[group or item, e.g. combat | boosts | creep.fatigue | tooling]"
---

# /verify $ARGUMENTS

## 1. Scope
- Map `$ARGUMENTS` to items using `groups.md` in this skill's directory. If no argument is given, take every row in
  `docs/VERIFICATION_LOG.md` that is marked `unverified` or `VERIFY` and is needed by the current STATUS item.
- Drop items that already have a `verified` row for the pinned `screeps-game-api` version and the current target servers.

## 2. Research (parallel, outside this context)
- Spawn one `screeps-verifier` subagent per independent group, all in the same turn. Give each one the exact item list,
  the pinned crate version (from `Cargo.lock`, or `Cargo.toml` before P1, the phase that set up the build), and the target
  servers from `docs/PROJECT_CONTEXT.md`.
- Ask for the verifier's standard output format (rows, discrepancies, suggested tests). Do not ask for raw page text.

## 3. Reconcile and record
- If sources conflict, the higher-priority source wins (CLAUDE.md source order). Record both, and note which won and why.
- Add the rows to `docs/VERIFICATION_LOG.md`. Supersede rows. Never delete them.
- Constants:
  - Where an API constant exists, re-export it from `crates/rustedscreep-consts` under its API name.
  - Otherwise add it to `crates/rustedscreep-consts/src/extra.rs` with a doc comment giving the source URL, the version it applies to, and `last verified: YYYY-MM-DD`.
  - Server-specific values, such as the CPU limit, are not compile-time constants. Read them at runtime and record them in PROJECT_CONTEXT.
- Add a self-check entry so the in-game init compares this constant with the server's JS global, where one exists.
- Add or adjust tests that derive expected values from the constants.
- If a finding contradicts `docs/SPEC.md`, add it to "Discrepancies with SPEC" and tell the user in your reply.

## 4. Report
Give a short table of items with their outcome (verified, inferred, or unverified, plus the assumption), the files changed, and the tests added.
Do not claim the tests pass unless you ran them in this session.
