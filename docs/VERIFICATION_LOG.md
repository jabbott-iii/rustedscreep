# Verification log

This log is the record of every game rule, constant, API signature and tool behavior the bot depends on (SPEC §0).
Rules: append or supersede rows and never delete them. Use the API constant where one exists. Everything else goes in
`crates/game-consts/src/extra.rs` with its source and a "last verified" date.

**Confidence levels**
- `verified`: read directly in a priority 1–3 source (crate docs or source for the pinned version, docs.screeps.com, or engine source) that applies to the version or server in use.
- `inferred`: derived from verified facts, read in a priority 4–5 source, read through a summarizing tool rather than verbatim, or read for a different version.
- `unverified`: not found yet. The row states the assumption and the safe default.

Columns: # | Group | Parameter | Finding | Applies to | Source | Date | Confidence | Constant / code location

---

## A. Seeded findings (researched 2026-11-13 in chat, before the repo existed; P0 must re-check and supersede each one)

| # | Group | Parameter | Finding | Applies to | Source | Date | Confidence | Constant / code location |
|---|---|---|---|---|---|---|---|---|
| S1 | tooling | screeps-game-api current line | The current line is 0.23.x. One docs.rs listing shows 0.23.1, and a 0.23.3 source page exists. Pin the exact version from crates.io. | crate | https://docs.rs/crate/screeps-game-api/latest ; https://docs.rs/crate/screeps-game-api/0.23.3/source/src/lib.rs | 2026-11-13 | inferred | Cargo.toml |
| S2 | tooling | Starter template Cargo.toml | Dependencies: `screeps-game-api = "0.23"`, `wasm-bindgen = "0.2"`, `js-sys = "0.3"`, `log`, `fern = "0.6"`, `web-sys` (console). Release profile: `panic="abort"`, `opt-level=3`, `lto=true`. wasm-pack metadata: `wasm-opt = ["-O4"]`. Features: `sim` and `mmo` map to `screeps-game-api/sim` and `/mmo`. | screeps-starter-rust main | https://raw.githubusercontent.com/rustyscreeps/screeps-starter-rust/main/Cargo.toml | 2026-11-13 | verified | root Cargo.toml / bot-wasm |
| S3 | tooling | Starter build/deploy pipeline | screeps-game-api 0.22+ is "no longer compatible" with cargo-screeps. The pipeline is wasm-pack + wasm-opt, Rollup + Babel (Babel "required" for the older Node on Screeps servers), and the `screeps-api` npm package via `npm run deploy -- --server <name> [--dryrun]`. `.screeps.yaml` must stay out of git, and its password must be a string. Node 16–22 tested locally; Node 20 recommended. | screeps-starter-rust main | https://github.com/rustyscreeps/screeps-starter-rust (README) | 2026-11-13 | verified | ADR-001, ADR-008 |
| S4 | tooling | `--signext-lowering` | The README names wasm-opt `--signext-lowering` as the fix for an "Invalid opcode" load error, but the main-branch Cargo.toml shows only `-O4`. Settle this with the feature probe. | starter main | README vs Cargo.toml (S2/S3) | 2026-11-13 | inferred | ADR-002 |
| S5 | tooling | Starter loader pattern | Boot is deferred while the bucket is below `BUCKET_BOOT_THRESHOLD` (1500). The loader then runs `require(<module>)` to get the bytes, `new WebAssembly.Module(bytes)`, then `initSync({ module })` across ticks, frees the bytes and deletes the require cache, and swaps in the loop. A `running` flag left set means the previous tick aborted, so the loader calls `Game.cpu.halt()` (wasm-bindgen issue #3130). try/catch logs only the message, since the panic hook prints the stack. `console.error` is routed to `Game.notify`. | starter main `js_src/main.js` | https://raw.githubusercontent.com/rustyscreeps/screeps-starter-rust/main/js_src/main.js | 2026-11-13 | inferred (read through a summarizing fetch; re-read raw) | js/src/main.ts, ADR-003 |
| S6 | tooling | Rust wasm32 default features | Rust 1.82 (LLVM 19) enables `multivalue` and `reference-types` by default on wasm32-unknown-unknown. Rust 1.87 (LLVM 20) adds `bulk-memory` and `nontrapping-fptoint`. Opting out takes `-Ctarget-cpu=mvp` plus nightly `-Zbuild-std=panic_abort,std`. `wasm32v1-none` (Rust 1.84+) is MVP + mutable-globals and `no_std` only. | rustc | https://blog.rust-lang.org/2024/09/24/webassembly-targets-change-in-default-target-features/ ; https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html ; https://doc.rust-lang.org/rustc/platform-support/wasm32v1-none.html | 2026-11-13 | verified | ADR-002, rust-toolchain.toml |
| S7 | tooling | Panic strategy on wasm | Unwinding on wasm needs nightly and build-std (`RUSTFLAGS='-Cpanic=unwind' cargo +nightly build -Zbuild-std`), and the engine must support exception handling. Whether the Screeps server supports exceptions is unknown (probe). Working assumption: abort only, with recovery in JS. | rustc; server unknown | https://doc.rust-lang.org/stable/rustc/platform-support/wasm32-unknown-unknown.html | 2026-11-13 | verified (rustc) / unverified (server) | ADR-003 |
| S8 | tooling | Rust/WASM tool maintenance | The rustwasm GitHub org was archived (Sept 2025). wasm-bindgen moved to a new `wasm-bindgen` org with new maintainers, and wasm-pack moved to its long-time maintainer. Find the current wasm-pack repo and release in P0. | ecosystem | https://blog.rust-lang.org/inside-rust/2025/07/21/sunsetting-the-rustwasm-github-org ; https://users.rust-lang.org/t/future-of-rust-wasm/133089 | 2026-11-13 | verified (plan) / inferred (wasm-pack's new home) | ADR-001 |
| S9 | cpu-memory | `Memory` access cost | screeps-game-api's `Memory` wrapper runs `JSON.parse` on first access each tick and `JSON.stringify` at tick end. | screeps-game-api 0.23.3 | https://docs.rs/crate/screeps-game-api/0.23.3/source/src/lib.rs | 2026-11-13 | verified | bot-wasm (keep the tick path off Memory) |
| S10 | cpu-memory | Intent CPU cost | The crate docs call game actions relatively expensive, at about 0.2 ms. The official per-intent CPU cost on docs.screeps.com has not been read yet. | screeps-game-api 0.23.3 docs | same as S9 | 2026-11-13 | inferred | extra.rs (if no constant) |
| S11 | pipeline | Simultaneous actions (docs) | Within a dependency pipeline, "only the most right one" executes. Healing a healthy creep or repairing an undamaged structure returns OK and still blocks the methods to its left. Call order doesn't matter, and for the same method the last call wins. Simultaneous CARRY-using methods each see start-of-tick energy. `transfer` runs at most once per tick. The diagram image has not been machine-read. | docs (current) | https://docs.screeps.com/simultaneous-actions.html | 2026-11-13 | verified (text only) | bot-core::intents |
| S12 | pipeline | Engine intent priorities | `heal` is never blocked. `rangedHeal` is blocked by heal. `attackController` by rangedHeal and heal. `dismantle` by attackController, rangedHeal and heal. `repair` by dismantle plus those. `build` by repair plus those. `attack` by build, repair, dismantle, attackController, rangedHeal and heal. `harvest` by attack plus everything above. `rangedAttack` by rangedMassAttack, build, repair and rangedHeal. `rangedMassAttack` by build, repair and rangedHeal. move, transfer, withdraw, pickup and drop are outside the table. | engine master | https://github.com/screeps/engine/blob/master/src/processor/intents/creeps/intents.js | 2026-11-13 | inferred (read through a summarizing fetch; re-read raw and record the commit SHA) | bot-core::intents |
| S13 | tooling | Private server | screepers/screeps-launcher is available as a Docker image (`screepers/screeps-launcher`) and configured with `config.yml`. Initialize with `screeps-launcher cli` and then `system.resetAllData()`. The community wiki recommends it. | launcher main | https://github.com/ags131/screeps-launcher ; https://hub.docker.com/r/screepers/screeps-launcher/ ; https://wiki.screepspl.us/Private_Server_Installation | 2026-11-13 | verified | docs/TEST_PLAN.md |
| S14 | tooling | Automated server tests | screeps-server-mockup (npm `@brisberg/screeps-server-mockup`, based on screepers/screeps-server-test) exists for scripted tests. Its maintenance status is unchecked. | npm | https://www.npmjs.com/package/@brisberg/screeps-server-mockup | 2026-11-13 | inferred | docs/TEST_PLAN.md |
| S15 | kit | Claude Code mechanics used by this kit | Skills live in `.claude/skills/<name>/SKILL.md` with `context: fork` and `agent`. Subagents live in `.claude/agents` (`tools` and `model`). Path-scoped rules use the `paths` frontmatter in `.claude/rules`. CLAUDE.md should stay under 200 lines. A PreToolUse hook blocks by exiting with 2. SessionStart hooks support the `compact` matcher. Permission syntax looks like `Bash(npm run test *)`. `attribution` replaces the deprecated `includeCoAuthoredBy`. | Claude Code docs | https://code.claude.com/docs/en/skills.md ; /sub-agents.md ; /memory.md ; /hooks-guide.md ; /settings.md | 2026-11-13 | verified (the effect of an empty string in `attribution` not read) | .claude/ |

## B. Required parameters (SPEC §0; group definitions in `.claude/skills/verify/groups.md`)

| # | Group | Parameter | Finding | Applies to | Source | Date | Confidence | Constant / code location |
|---|---|---|---|---|---|---|---|---|
| R1 | tooling | Pinned versions: screeps-game-api (+features), wasm-bindgen crate/CLI, js-sys, wasm-pack, binaryen, Rollup, Babel, screeps-api npm, Rust toolchain/edition | | | | | unverified | Cargo.lock, package-lock |
| R2 | tooling | Wasm features and JS syntax accepted per target server (probe) | | per server | | | unverified | ADR-002, ADR-008 |
| R3 | tooling | `--target` and instantiate pattern expected by the bindings | | | | | unverified | ADR-001 |
| R4 | tooling | Whether screeps-game-api builds and tests natively (constants and local types) | | | | | unverified | ADR-005 |
| R5 | tooling | Code upload size limit and binary-module upload rules | | | | | unverified | ADR-001 |
| R6 | cpu-memory | CPU limit formula per server/shard/subscription, plus tickLimit and bucket max and refill | | | | | unverified | runtime + TUNING |
| R7 | cpu-memory | `generatePixel` cost and requirements; crate feature name | | | | | unverified | |
| R8 | cpu-memory | Official CPU cost per state-changing intent | | | | | unverified | extra.rs |
| R9 | cpu-memory | WASM compile + instantiate cost on a global reset (measured per server) | | | | | unverified | TUNING (boot gate) |
| R10 | cpu-memory | Memory size limit; RawMemory segment count, size, active limit and activation latency; heap limits and getHeapStatistics | | | | | unverified | |
| R11 | creep | Lifetime (including the CLAIM exception), max body size, part costs, spawn time per part | | | | | unverified | |
| R12 | creep | Fatigue per terrain and road, MOVE reduction, empty vs loaded CARRY, carry capacity (including boosts) | | | | | unverified | |
| R13 | economy | Source capacity per room type and regen time, harvest per WORK | | | | | unverified | |
| R14 | economy | Controller downgrade timers, upgrade per WORK, any max-RCL cap | | | | | unverified | |
| R15 | economy | Structure counts per RCL; build/repair power; decay of roads, containers and ramparts | | | | | unverified | |
| R16 | economy | Link loss and cooldown, terminal send cost and cooldown, factory costs and cooldowns | | | | | unverified | |
| R17 | combat | Tower attack/heal/repair power, optimal and falloff range, falloff formula | | | | | unverified | |
| R18 | combat | Heal and rangedHeal power; attack, ranged, mass-ranged by range, and dismantle power | | | | | unverified | |
| R19 | combat | Damage order across body parts, TOUGH boost reduction, how multiple sources combine within a tick (engine) | | | | | unverified | |
| R20 | combat | Rampart and wall max hits per RCL | | | | | unverified | |
| R21 | boosts | Compound list, part/action and multiplier per tier (dismantle, fatigue, carry, attack, ranged, heal, TOUGH, harvest, build/repair, upgrade); boost cost; unboost | | | | | unverified | |
| R22 | boosts | Lab reaction chains, reaction times, cooldowns and range | | | | | unverified | |
| R23 | pipeline | Engine intent-priority table at a pinned commit (supersedes S12) | | | | | unverified | bot-core::intents |
| R24 | pipeline | Movement semantics: swap, pull, fatigue, collision resolution | | | | | unverified | |
| R25 | safemode | Duration, cooldown, charges, what it blocks (dismantle and attacks on structures) | | | | | unverified | |
| R26 | safemode | Novice/respawn area rules and other attack restrictions | | | | | unverified | |
| R27 | vision | Observer range and when `observeRoom` results appear; structure visibility | | | | | unverified | |
| R28 | expansion | GCL to room-count rule; claim/reserve mechanics and timers | | | | | unverified | |
| R29 | expansion | Shard travel, portals and InterShardMemory limits | | | | | unverified | |
| R30 | market-power | Order fees and price rules, deal energy cost, terminal cooldown | | | | | unverified | |
| R31 | market-power | Power processing; power-creep levels, abilities and cooldowns | | | | | unverified | |
| R32 | pathfinding | PathFinder limits (maxOps, maxRooms, maxCost), CostMatrix semantics, flee, roomCallback | | | | | unverified | |

## C. Discrepancies with SPEC

| # | SPEC ref | SPEC says | Finding | Resolution |
|---|---|---|---|---|
| D1 | §1 tech stack | "`wasm-pack build --target web` (or `cargo-screeps`)" | cargo-screeps doesn't work with screeps-game-api ≥0.22 (S3) | Drop cargo-screeps (ADR-001). Pending re-verification. |
| D2 | §1 TS glue 3 | "re-instantiate on the next tick" | The starter halts the VM with `Game.cpu.halt()` after an aborted tick because of wasm-bindgen #3130 (S5) | ADR-003 decides by measurement |
| D3 | §1 release profile | `panic="abort"` (confirm compatibility) | Compatible only because recovery lives in JS. Unwinding needs nightly and engine support (S7) | Keep abort unless the probe and ADR say otherwise |
| D4 | §5 code quality | "Per-room and per-creep error isolation" | Under abort, `catch_unwind` can't isolate. Isolation has to come from `Result` boundaries | Recorded as an architecture invariant |
| D5 | §4b VERIFY | Can dismantle combine with other actions? | Dismantle is blocked by heal, rangedHeal and attackController. It can run alongside move and rangedAttack (S12, pending R23) | Breaker never heals while dismantling |
| D6 | §0 tooling | wasm-pack as the build tool | The rustwasm org is archived and wasm-pack has a new maintainer (S8) | ADR-001 compares wasm-pack with the wasm-bindgen CLI plus wasm-opt |

## D. Feature probe results (per server)

| Server | Date | Wasm features accepted | JS syntax | TextDecoder | halt / pixel / heap | CPU limit / tickLimit | Raw output file |
|---|---|---|---|---|---|---|---|

## E. Remaining unverified items (closing list; maintained from P12)
