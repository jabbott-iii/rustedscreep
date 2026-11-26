---
name: combat-verify
description: Audit defense and offense math (tower falloff, heal, TOUGH and boosts, damage order, intent conflicts, breaker solver, survival sim, formations) against verified constants and engine source, in the combat-math-auditor subagent.
argument-hint: "[path | diff]"
context: fork
agent: combat-math-auditor
background: false
---

Audit the combat math in `$ARGUMENTS`. If the argument is empty or `diff`, audit `git diff` limited to
`crates/rustedscreep-core/src/{defense,offense,combat,intents}/**`.

Use `docs/VERIFICATION_LOG.md` rows in the `combat`, `boosts`, `pipeline`, and `safemode` groups as ground truth. Flag any reliance on rows that are
not `verified`. Return the findings table and the missing-tests list defined in your instructions.
