# Project context

`/kickoff` fills this in from the user's answers. Anything not answered goes under Assumptions, with the safest default.
Answers recorded 2026-11-13 (P0 kickoff).

## Targets
- Servers and shards: the official MMO, shard3, on the core track (`docs/ROADMAP.md`). The probe ran there, and `Game.cpu.shardLimits` lists only shard3 at 20 CPU (VERIFICATION_LOG V72). Two optional tracks build on it (user, 2026-11-13; ADR-010): a private server (`docs/ROADMAP_PRIVATE.md`) and Seasonal (`docs/ROADMAP_SEASONAL.md`), which starts only after the core is complete (P12). PTR is not a target. Shards for the MMO are not chosen yet (see Assumptions).
- Deploy profile names in `.screeps.yaml` (names only, never credentials): not given yet (see Assumptions).
- CPU situation (subscription or unlock state; the verified limit per server goes in VERIFICATION_LOG): free tier on the MMO.
- Starting point (fresh, or existing with GCL and per-room RCL): fresh respawn.

## Scope
- Pixels: in scope.
- Market: in scope, with sanity limits (price bands, volume caps, credit floor; ROADMAP P9).
- Power and power creeps: not asked (see Assumptions).
- Offense enabled by default: not asked directly (see Assumptions).
- Siege-breaker initial frequency (maps to config `breaker.cooldown_ticks`): conservative. The tick value is set in P11 and documented in `docs/TUNING.md`; sortie telemetry tunes it afterwards.

## Diplomacy
- Ally and whitelist players (never attacked): none for now. The whitelist starts empty and is set through config.
- Shared-intel protocol (for example simpleAllies), if any: none.

## Environment
- Private server (screeps-launcher / Docker / none) and mods: none at the moment. It is the optional private-server track (PS1 sets it up when the user wants it).
- Game client and code sync (2026-11-13): the Steam client (packageVersion 248) keeps a two-way local mirror at `~/.config/Screeps/scripts/screeps.com/` (VERIFICATION_LOG V74). The user linked the repository in the client on 2026-11-13, most likely through Account → GitHub (V75), and keeps the link for live testing (ADR-012). Every push to `main` triggers the sync (user-observed), so a push deploys. The user plans to point the link at the tracked `deploy/` folder instead of the root.
- Toolchain policy (stable pinned, or nightly allowed): stable only, pinned. If a target server rejects the wasm features that current stable emits, the options are an older pinned stable or wasm-opt lowering passes (ADR-002), never nightly.
- Developer OS and Node version (checked 2026-11-13 with `uname -sr`, `node -v`, `rustc -Vv`):
  - Linux 7.2.15 (x86_64).
  - Node v18.19.1 and npm 9.2.0. KIT_README asks for Node 20+, and the starter README installs Node 20 (VERIFICATION_LOG V70). The deploy package screeps-api 2.x needs Node 22.x or 24.x (V11).
  - rustc and cargo 1.99.0 stable (LLVM 23.1.1). Toolchains 1.99.0 and 1.99.1 are also installed.
  - The `wasm32-unknown-unknown` target is not installed for any toolchain. wasm-pack, wasm-bindgen and wasm-opt are not on PATH.

## Assumptions (until the user answers)
- **Private server.** Optional (ADR-010). No core gate needs one. If the user wants the private track, PS1 sets up screeps-launcher pinned to v1.17.0. Docker commands always ask first.
- **MMO shard.** Answered by the probe: shard3 (V72). The CPU limit is still read at runtime, never compiled in.
- **Pixels on the free tier.** In scope. The docs state no free-tier restriction, but the MMO backend is closed, so eligibility is unverified (VERIFICATION_LOG V38). The probe's api part reports whether `Game.cpu.generatePixel` exists. Safest default: keep pixel generation behind a runtime capability check and a config flag, and never spend bucket below the configured threshold.
- **Power and power creeps.** Not asked. Assumption: in scope as part of SPEC §3.8 (late game), built in P9, and enabled only once the room economy supports it. Safest default: disabled in config until P9's gate passes.
- **Offense.** Not asked directly. SPEC §4 makes offense mandatory, and the breaker answer is "conservative". Assumption: offense is enabled by default with conservative pacing, inside its own energy and CPU budget (SPEC §4c). It never targets protected rooms (safe mode, novice or respawn areas). With an empty whitelist, no player is exempt.
- **Deploy profile names.** Not given. Assumption: `mmo` for the core, matching the server profile in `config.rs` (ROADMAP P1.10). A `private` profile is added in PS1 and a `season` profile in SE2. The user creates `.screeps.yaml`; Claude never reads it.
- **Node version.** The local Node is 18.19.1. The hooks and the probe check run on it, but the deploy tool cannot: screeps-api 2.x needs Node 22.x or 24.x (V11), and Babel 8 needs ^22.18 or >=24.11 (V10). Upgrading Node is the user's call. It blocks `npm run deploy` (a P2 item since ADR-017), not P0 or P1.
