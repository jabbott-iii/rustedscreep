---
name: phase-gate
description: Evaluate a roadmap phase's exit criteria with real evidence and record the result in docs/STATUS.md. Use when all items of a phase are ticked, or to see what blocks a phase.
argument-hint: "<phase, e.g. P1>"
disable-model-invocation: true
---

# /phase-gate $ARGUMENTS

1. Read the exit criteria for `$ARGUMENTS` in `docs/ROADMAP.md` and the phase's log in `docs/STATUS.md`.
2. For each criterion, collect evidence in this session. Run the command, read the log file, or ask the user for in-game observations
   that you cannot get yourself. Evidence from an earlier session counts only if STATUS.md recorded the output or numbers and nothing in
   that area has changed since. Check `git status` and `git diff --stat`.
3. Report a table: criterion | evidence (command and key output, or measured number) | pass / fail / not evaluated.
   Any fail or "not evaluated" means the gate is **not passed**.
4. Measured numbers (CPU per tick per subsystem, bucket trend, wasm and bundle size, cold-start cost, throughput) go into
   STATUS.md. Where a comparison against the naive baseline is part of the criteria, record both sides and the method.
5. If the gate passes, tick the phase in STATUS.md, set the next phase as current, and suggest starting a fresh session
   (`/clear`) for the next phase. If it fails, list the blocking items as STATUS checklist entries.
6. Offer a commit message (`/commit-msg`).
