# Tuning guide (SPEC §6.5)

This guide mirrors `crates/rustedscreep-core/src/config.rs` field for field, and a doc-sync test fails when the two diverge.
Each row gives the default, the unit, the rationale (with a measurement or source), the safe range (overrides are clamped to it),
and the server profile, if the value differs by profile.

| Field | Default | Unit | Rationale / evidence | Safe range | Profile overrides |
|---|---|---|---|---|---|
| `LoaderConfig::boot_bucket_min` | 1500 | CPU | The starter's gate (VERIFICATION_LOG V17), kept until the P1 cold-start measurement on the MMO sets it. In the sandbox a boot costs about 3 ms of CPU-like time (V99), far below it; the MMO number decides. | 0 to 10000 (`CPU_BUCKET_MAX`; the config test checks it) | none |
| `LoaderConfig::boot_split_ticks` | true | flag | Compile on one tick and instantiate on the next, so one tick never pays for both as the wasm grows. Costs one tick of boot latency (sandbox: booted in 1 tick, V99). | true or false | none |
| `LoaderConfig::recovery_mode` | halt | enum | ADR-003: `Game.cpu.halt()` (a) is the proven default. Re-instantiation (b) recovered 10 of 10 traps in the sandbox in 2 ticks against 3 for halt (V99), but stays off until the MMO trap-test measures it. Without `Game.cpu.halt`, halt mode falls back to (b). | halt or reinstantiate | none |
| `LoaderConfig::backoff_window_ticks` | 1000 | ticks | An incident continues the streak if it comes less than this plus the delay then in force after the previous one; otherwise a new streak starts. Long enough to see a crash loop, short enough to forget a one-off trap. Starting value; revisit after live incidents. | 100 to 20000 | none |
| `LoaderConfig::backoff_incidents` | 3 | count | One trap or aborted tick recovers at once; the third consecutive incident marks a loop, so the next boot waits. Also caps re-instantiations per window in mode (b). Trap-test traps do not count. | 1 to 20 (the loader keeps the last 20 incidents) | none |
| `LoaderConfig::backoff_base_ticks` | 100 | ticks | First delay once backoff starts, doubling per further consecutive incident (a persistent trap in the sandbox waits 100, 200, 400, ... 3200 ticks; V99). An idle bot refills about 20 CPU per tick at the free-tier limit (V37), so 100 ticks restore about 2000 CPU, above the boot gate. | 1 to `backoff_max_ticks` (the config test checks it) | none |
| `LoaderConfig::backoff_max_ticks` | 3200 | ticks | 100 × 2^5: caps the wait so the bot always retries without human action. | `backoff_base_ticks` to 20000 | none |
| `LoaderConfig::notify_group_minutes` | 60 | minutes | `groupInterval` for `Game.notify`, which batches messages into that window (V96); hourly keeps a crash loop from flooding email. | 0 to 1440 (the driver clamps to it, V96) | none |
| `LoaderConfig::trap_test_max` | 20 | count | Most traps one `rustedscreep.trapTest(n)` may schedule. The P1 gate needs 10 (the config test checks at least 10); the cap stops a mistyped command. | 10 to 100 | none |

Groups to cover as they land:
- Bucket tiers (P1). The loader's boot gate, recovery and backoff values are above (P1.4); they live in config.rs's `LOADER_MMO` and are injected into the bundle at build time, so they cannot be overridden at runtime.
- Cache TTLs and toggles (P1 onward).
- Spawn lead time (P5).
- Remote profitability cut-offs (P6).
- Defense and safe-mode triggers (P7).
- Expansion limits (P8).
- Market sanity limits and pixel threshold (P9).
- Offense budgets and abort thresholds (P10).
- Breaker cooldown, caps, gates and the "no regrets" threshold (P11).
