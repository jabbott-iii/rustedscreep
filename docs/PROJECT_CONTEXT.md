# Project context

`/kickoff` fills this in from the user's answers. Anything not answered goes under Assumptions, with the safest default.
Answers recorded 2026-11-13 (P0 kickoff, the core roadmap's first phase: verification before any bot code).

## Targets
- Servers and shards: the official MMO (the massively multiplayer online server, the game's persistent shared world), shard3 (one of the MMO's shards, separate worlds that each get their own CPU limit), on the core track (`docs/ROADMAP.md`). The probe (a check of the WebAssembly, or wasm, features and JavaScript syntax the server accepts) ran there, and `Game.cpu.shardLimits` lists only shard3 at 20 CPU (VERIFICATION_LOG V72). Two optional tracks build on it (user, 2026-11-13; ADR-010, the architecture decision record, or ADR, on the roadmap tracks): a private server (`docs/ROADMAP_PRIVATE.md`) and Seasonal, an official server whose seasons each change the rules (`docs/ROADMAP_SEASONAL.md`), which starts only after the core is complete (P12, its last phase). PTR (the public test realm) is not a target. The probe settled the MMO shard as shard3 (see Assumptions).
- Deploy profile names in `.screeps.yaml` (names only, never credentials): not given yet (see Assumptions).
- CPU situation (subscription or unlock state; the verified limit per server goes in VERIFICATION_LOG): free tier (no subscription or CPU unlock) on the MMO.
- Starting point (fresh, or existing with GCL, the global control level, and per-room RCL, the room controller level): fresh respawn (a new start in a new room).

## Scope
- Pixels (a resource generated from the bucket, the game's reserve of unused CPU, at 10000 CPU each): in scope.
- Market: in scope, with sanity limits (price bands, volume caps, credit floor; ROADMAP P9, the late-game phase).
- Power (a late-game resource) and power creeps (hero units tied to the player's account): not asked (see Assumptions).
- Offense enabled by default: not asked directly (see Assumptions).
- Siege-breaker (a single heavily armored unit, or creep, that dismantles enemy structures) initial frequency (maps to config `breaker.cooldown_ticks`): conservative. The tick value (a tick is one game turn) is set in P11 (the siege-breaker phase) and documented in `docs/TUNING.md`; sortie telemetry tunes it afterwards.

## Diplomacy
- Ally and whitelist players (never attacked): none for now. The whitelist starts empty and is set through config.
- Shared-intel protocol (for example simpleAllies), if any: none.

## Environment
- Private server (screeps-launcher / Docker / none) and mods: none at the moment. It is the optional private-server track (PS1 sets it up when the user wants it).
- Game client and code sync (2026-11-13): the Steam client (packageVersion 248) keeps a two-way local mirror at `~/.config/Screeps/scripts/screeps.com/` (VERIFICATION_LOG V74). The user linked the repository in the client on 2026-11-13, most likely through Account → GitHub (V75), and keeps the link for live testing (ADR-012, which makes the GitHub link the MMO deploy path). Every push to `main` triggers the sync (user-observed), so a push deploys. The user has since pointed the link's folder at the tracked `deploy/` folder instead of the root (V136).
- Toolchain policy (stable pinned, or nightly allowed): stable only, pinned. If a target server rejects the wasm features that current stable emits, the options are an older pinned stable or wasm-opt lowering passes (rewrites of newer wasm features into older instructions; ADR-002), never nightly.
- Developer OS and Node version (checked 2026-11-13 with `uname -sr`, `node -v`, `rustc -Vv`):
  - Linux 7.2.15 (x86_64).
  - Node v18.19.1 and npm 9.2.0. KIT_README asked for Node 20+ then (it now asks for 18.19 or later, as `package.json` does), and the starter README installs Node 20 (VERIFICATION_LOG V70). The deploy package screeps-api 2.x needs Node 22.x or 24.x (V11); no tool uses it since ADR-021 (which dropped the API tools).
  - rustc and cargo 1.99.0 stable (LLVM 23.1.1). Toolchains 1.99.0 and 1.99.1 are also installed.
  - The `wasm32-unknown-unknown` target is not installed for any toolchain. wasm-pack, wasm-bindgen, and wasm-opt are not on PATH.
  - Since then P1 pinned Rust 1.100.0 with the wasm32 target (`rust-toolchain.toml`, ADR-002) and installed the tools the build uses:
    the wasm-bindgen CLI 0.2.130 (V90) and binaryen version_133's wasm-opt in the git-ignored `.tools/` (V91).

## Assumptions (until the user answers)
- **Private server.** Optional (ADR-010). No core gate (a phase's exit criteria on the core track) needs one. If the user wants the private track, PS1 sets up screeps-launcher pinned to v1.17.0. Docker commands always ask first.
- **MMO shard.** Answered by the probe: shard3 (V72). The CPU limit is still read at runtime, never compiled in.
- **Pixels on the free tier.** In scope. The docs state no free-tier restriction, but the MMO backend is closed, so eligibility is unverified (VERIFICATION_LOG V38). The probe's api part reports whether `Game.cpu.generatePixel` exists. Safest default: keep pixel generation behind a runtime capability check and a config flag, and never spend bucket below the configured threshold.
- **Power and power creeps.** Not asked. Assumption: in scope as part of SPEC §3.8 (late game), built in P9, and enabled only once the room economy supports it. Safest default: disabled in config until P9's gate passes.
- **Offense.** Not asked directly. SPEC §4 makes offense mandatory, and the breaker answer is "conservative". Assumption: offense is enabled by default with conservative pacing, inside its own energy and CPU budget (SPEC §4c). It never targets protected rooms (safe mode, novice, or respawn areas; the last two shield new and restarting players). With an empty whitelist, no player is exempt.
- **Deploy profile names.** Not given. Assumption: `mmo` for the core, matching the server profile in `config.rs` (ROADMAP P1.10, the config module). A `private` profile is added in PS1 and a `season` profile in SE2. No tool reads `.screeps.yaml` (ADR-021); Claude never reads it.
- **Node version.** The local Node is 18.19.1. The build, the stage, `npm run check`, the hooks, and the probe check run on it. screeps-api 2.x needs Node 22.x or 24.x (V11) and Babel 8 needs ^22.18 or >=24.11 (V10), but neither is used (ADR-008, the loader compiled by `tsc` without Babel; ADR-021), so nothing in the core track needs an upgrade (the optional PS4 mockup spike may: V61 and V63 record the Node versions the private server and the mockup need).
