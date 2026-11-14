# Tuning guide (SPEC §6.5)

This guide mirrors `crates/rustedscreep-core/src/config.rs` field for field, and a doc-sync test fails when the two diverge.
Each row gives the default, the unit, the rationale (with a measurement or source), the safe range (overrides are clamped to it),
and the server profile, if the value differs by profile.

| Field | Default | Unit | Rationale / evidence | Safe range | Profile overrides |
|---|---|---|---|---|---|

Groups to cover as they land:
- Bucket tiers and boot gate (P1).
- Cache TTLs and toggles (P1 onward).
- Spawn lead time (P5).
- Remote profitability cut-offs (P6).
- Defense and safe-mode triggers (P7).
- Expansion limits (P8).
- Market sanity limits and pixel threshold (P9).
- Offense budgets and abort thresholds (P10).
- Breaker cooldown, caps, gates and the "no regrets" threshold (P11).
