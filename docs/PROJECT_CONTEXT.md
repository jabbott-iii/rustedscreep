# Project context

`/kickoff` fills this in from the user's answers. Anything not answered goes under Assumptions, with the safest default.
Answers recorded 2026-11-13 (P0 kickoff).

## Targets
- Servers and shards: official MMO, Seasonal, and a private server. PTR is not a target. Shards for the MMO are not chosen yet (see Assumptions).
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
- Private server (screeps-launcher / Docker / none) and mods: none at the moment, although the private server is a target (see Assumptions and STATUS blockers).
- Toolchain policy (stable pinned, or nightly allowed): stable only, pinned. If a target server rejects the wasm features that current stable emits, the options are an older pinned stable or wasm-opt lowering passes (ADR-002), never nightly.
- Developer OS and Node version (checked 2026-11-13 with `uname -sr`, `node -v`, `rustc -Vv`):
  - Linux 7.2.15 (x86_64).
  - Node v18.19.1 and npm 9.2.0. KIT_README asks for Node 20+, and the starter README installs Node 20 (VERIFICATION_LOG V70). The deploy package screeps-api 2.x needs Node 22.x or 24.x (V11).
  - rustc and cargo 1.99.0 stable (LLVM 23.1.1). Toolchains 1.99.0 and 1.99.1 are also installed.
  - The `wasm32-unknown-unknown` target is not installed for any toolchain. wasm-pack, wasm-bindgen and wasm-opt are not on PATH.

## Assumptions (until the user answers)
- **Private server.** It is a target, but none is running. Assumption: one will be set up before the P1 gate, because that gate (and P2 onward) needs private-server runs. Safest default: plan a screeps-launcher Docker setup in P1. Docker commands always ask first. Until then, P1 can only measure on official servers, and no scripted scenarios can run.
- **MMO shards.** Not chosen. Safest default: pick the shard at respawn time, record it here, and read the CPU limit at runtime rather than compiling it in.
- **Pixels on the free tier.** In scope. The docs state no free-tier restriction, but the MMO backend is closed, so eligibility is unverified (VERIFICATION_LOG V38). The probe's api part reports whether `Game.cpu.generatePixel` exists. Safest default: keep pixel generation behind a runtime capability check and a config flag, and never spend bucket below the configured threshold.
- **Power and power creeps.** Not asked. Assumption: in scope as part of SPEC §3.8 (late game), built in P9, and enabled only once the room economy supports it. Safest default: disabled in config until P9's gate passes.
- **Offense.** Not asked directly. SPEC §4 makes offense mandatory, and the breaker answer is "conservative". Assumption: offense is enabled by default with conservative pacing, inside its own energy and CPU budget (SPEC §4c). It never targets protected rooms (safe mode, novice or respawn areas). With an empty whitelist, no player is exempt.
- **Deploy profile names.** Not given. Assumption: `mmo`, `season` and `private`, matching the server profiles in `config.rs` (ROADMAP P1.10). The user creates `.screeps.yaml`; Claude never reads it.
- **Node version.** The local Node is 18.19.1. The hooks and the probe check run on it, but the deploy tool cannot: screeps-api 2.x needs Node 22.x or 24.x (V11), and Babel 8 needs ^22.18 or >=24.11 (V10). Upgrading Node is the user's call. It blocks `npm run deploy` in P1, not P0.
