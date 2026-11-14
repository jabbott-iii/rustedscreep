# Claude Code kit: Screeps Rust/WASM bot

This kit turns your spec (`docs/SPEC.md`, verbatim) into a phase-gated Claude Code workflow with guardrails.

## Install
1. Copy the contents of this folder into the root of a new, empty repository, then run `git init`.
2. Prerequisites: Node 20+ on your PATH (the hooks are Node scripts, and the build needs Node anyway), plus Rust through rustup.
   Claude Code installs nothing on its own. `cargo install`, `npm install`, `rustup` and `docker` always ask first.
3. Create `.screeps.yaml` yourself from `.example-screeps.yaml`, which P1 adds. Claude is blocked from reading or writing it.
4. Start `claude` in the repo root and trust the folder. The allow rules in `.claude/settings.json` only apply after you trust it.
   Run `/status` to confirm the project settings loaded, and `/context` to see which CLAUDE.md and rules files loaded.

## Drive it
| Step | Command |
|---|---|
| Phase 0: questions, verification, feature probe, ADR drafts | `/kickoff` |
| Accept or amend ADRs | edit `docs/DECISIONS.md`, or tell Claude |
| Build the next item with the full protocol | `/module` (or `/module P3.hauling`) |
| Close a phase with evidence | `/phase-gate P1` |
| Verify game rules before coding against them | `/verify combat` (Claude also runs it on its own) |
| Full validation gate | `/check` |
| CPU / robustness review (forked subagent) | `/cpu-audit diff` |
| Combat math review (forked subagent) | `/combat-verify diff` |
| Deploy: `mmo` stages `deploy/` for your push through the GitHub link (ADR-012), and `--dryrun` stages to a scratch folder only. Official servers only when you name them | `/deploy mmo --dryrun` |
| Commit message to copy (no trailers, never commits) | `/commit-msg` |

Use one phase per session. After `/phase-gate` passes, run `/clear`. The SessionStart hook re-injects `docs/STATUS.md` at startup,
on resume, and after compaction, so long work keeps its place.

## What's inside
- `CLAUDE.md`: mission, working agreement, source-of-truth rule, architecture invariants, commands, workflow, and known traps. It is kept under 200 lines.
- `.claude/rules/`: path-scoped rules that load only when Claude touches matching files: `core-logic`, `wasm-bot`, `js-loader`, `combat`, and `docs`.
- `.claude/skills/`: the slash commands above. `verify/groups.md` lists every §0 parameter group.
- `.claude/agents/`:
  - `screeps-verifier` (Sonnet, read-only plus web) handles research, and several run in parallel.
  - `hot-path-reviewer`, `combat-math-auditor` and `wasm-build-doctor` are read-only reviewers.
- `.claude/hooks/`:
  - `guard-bash.mjs` blocks git history and publish operations, work-discarding git commands, package publishing, GitHub writes, and credential access. It also catches compound, `git -C`, quoted, wrapped, nested-shell and `eval` forms (`node .claude/hooks/test-guard-bash.mjs` tests it).
  - `guard-purity.mjs` keeps `rustedscreep-core` and `rustedscreep-consts` free of JS bindings.
  - `session-context.mjs` re-injects status.
- `.claude/settings.json`: allow, ask and deny rules, the hooks, and empty commit/PR attribution.
- `docs/`: SPEC (verbatim), ROADMAP (MMO core track P0–P12 with exit gates), ROADMAP_PRIVATE and ROADMAP_SEASONAL (optional tracks, ADR-010), STATUS, VERIFICATION_LOG (seeded with this kit's research and the full §0 checklist),
  DECISIONS (the kit shipped ADR-001..008 as proposed; the project adds its own ADRs), PROJECT_CONTEXT, and skeletons for ARCHITECTURE, TEST_PLAN and TUNING.

## Customize
- Personal overrides go in `.claude/settings.local.json` and `CLAUDE.local.md`. Both are gitignored.
- Change crate names or layout through an ADR, then update CLAUDE.md, the `paths` in the rules, and `guard-purity.mjs` together.
- To audit the instruction files for conflicts, run `/doctor prompt-audit`.
