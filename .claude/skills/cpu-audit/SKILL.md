---
name: cpu-audit
description: Read-only CPU and tick-path robustness audit of a path or of the current diff, run in the hot-path-reviewer subagent so the main context stays clean.
argument-hint: "[path | diff]"
context: fork
agent: hot-path-reviewer
background: false
---

Audit `$ARGUMENTS` for CPU waste and tick-path robustness. If the argument is empty or `diff`, audit `git diff` plus untracked files
under `crates/` and `js/`.

Also read `docs/STATUS.md` for the latest profiler numbers. When those numbers exist, rank findings by measured subsystem cost.
Return the findings table defined in your instructions.
