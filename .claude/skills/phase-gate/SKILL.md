---
name: phase-gate
description: Evaluate a roadmap phase's exit criteria with real evidence and record the result in docs/STATUS.md. Use when all items of a phase are ticked, or to see what blocks a phase.
argument-hint: "<phase, e.g. P1, PS1, SE1>"
disable-model-invocation: true
---

# /phase-gate $ARGUMENTS

1. Read the exit criteria for `$ARGUMENTS` in its track's roadmap (`docs/ROADMAP.md` for P#, `docs/ROADMAP_PRIVATE.md` for PS#,
   `docs/ROADMAP_SEASONAL.md` for SE#) and the phase's log in `docs/STATUS.md`. For an optional-track phase, first confirm that every phase it
   builds on (core or optional) has passed its gate.
2. For each criterion, collect evidence in this session. Run the command, read the log file, or ask the user for in-game observations
   that you cannot get yourself. Evidence from an earlier session counts only if STATUS.md recorded the output or numbers and nothing in
   that area has changed since. Check `git status` and `git diff --stat`.
3. Report a table: criterion | evidence (command and key output, or measured number) | pass / fail / not evaluated.
   Any fail or "not evaluated" means the gate is **not passed**.
4. Measured numbers (CPU per tick, or game turn, per subsystem; bucket trend, the game's store of unused CPU over time; wasm
   (WebAssembly) and bundle size; cold-start cost; throughput) go into STATUS.md. Where a comparison against the naive baseline (the
   same bot with every cache and optimization toggled off) is part of the criteria, record both sides and the method.
5. If the gate passes, tick the phase in STATUS.md, and in its track's roadmap tick the gate's criteria and add the pass date to its
   **Gate:** line. For a core phase (P#), set the next core phase as current and suggest starting a
   fresh session (`/clear`) for it. For an optional-track phase (PS#, SE#), tick only its entry under "Optional tracks" (for PS3,
   whose items are scripted scenarios, the scenario's own entry, and in the roadmap the scenario's box, since PS3's gate criteria have
   none) and leave the core's current phase and checklist unchanged. If the gate fails, list the blocking items as
   STATUS checklist entries (core) or under the optional phase's entry (PS#, SE#).
6. Offer a commit message (`/commit-msg`).
