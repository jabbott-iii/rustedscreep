---
name: kickoff
description: Phase 0 kickoff for the Screeps Rust/WASM bot. Asks the clarifying questions, verifies the toolchain, builds the server feature probe and drafts ADR-001..008. Run once at project start, or again when target servers change.
disable-model-invocation: true
---

# /kickoff: Phase 0 (no bot code yet)

Goal: settle every target, toolchain, and server fact that P1 depends on, before any scaffolding exists.
Read `docs/SPEC.md` §0, §1, §7 and `docs/ROADMAP.md` P0 first.

## 1. Ask the clarifying questions (SPEC §7)
Use AskUserQuestion with two calls of at most four questions each. Every question must make sense without
the others. Offer "I'll type it" where a free answer fits. Questions:
- Call 1:
  - Target servers (multi-select): official MMO, Seasonal, private server, PTR.
  - CPU situation: subscribed/unlocked on the MMO, free tier, private server with a custom limit, or other.
    Do not state CPU numbers. They are verified in step 2.
  - Starting point: fresh respawn, existing colony (ask for GCL and the RCL of each room), or private-server only for now.
  - Late-game scope: pixels and market, market only, pixels only, or neither for now.
- Call 2:
  - Ally/whitelist players (free text). Note that these players are never attacked.
  - Siege-breaker launch frequency: off by default, conservative, moderate, or aggressive. Explain that this only sets the initial
    cooldown config and that telemetry will tune it.
  - Private server: already running (screeps-launcher or other), set one up with Docker, or none.
  - Toolchain policy: stable only (pinned), or nightly allowed if the server needs MVP-only wasm.

Write the answers to `docs/PROJECT_CONTEXT.md`. Unanswered items go under "Assumptions", each with the safest default.

## 2. Verify tooling and platform facts
Run `screeps-verifier` subagents **in parallel**, one per group: `tooling`, `cpu-memory`, and `pipeline`. Use the group definitions in
`../verify/groups.md`. In addition:
- Re-check every row in the VERIFICATION_LOG section "Seeded findings" and supersede each one with a fresh row.
- Find the current versions of `screeps-game-api` (and its features `mmo`, `sim`, and so on), `wasm-bindgen`, wasm-pack and where it is
  now maintained, binaryen/`wasm-opt` and its lowering passes, Rollup, Babel, the `screeps-api` npm package, and screeps-launcher.
- Read the current screeps-starter-rust `js_src/main.js`, `js_tools/deploy.js`, `Cargo.toml` and `.cargo/config.toml` (if any).
  Record which `--target` the starter passes and how the loader instantiates.
Record the results in `docs/VERIFICATION_LOG.md`.

## 3. Build the server feature probe (`js/probe/`)
No deploy is needed for the first pass: the user pastes a console snippet.
- `js/probe/console-snippet.js` is a single expression the user pastes into each target server's in-game console. Use ES5 syntax only.
  It reports:
  - `WebAssembly.validate` results for tiny modules testing each feature: mutable-globals, sign-ext, nontrapping-fptoint,
    bulk-memory, reference-types, multivalue, simd128, exceptions (legacy and exnref), and tail-call. Vendor the byte arrays from
    GoogleChromeLabs `wasm-feature-detect`. Check its license, keep its attribution header, and do not hand-write opcodes.
  - `typeof` checks for `TextDecoder`, `TextEncoder`, `Game.cpu.halt`, `Game.cpu.generatePixel`, `Game.cpu.getHeapStatistics`,
    `RawMemory.segments`, `RawMemory.setActiveSegments`, and `InterShardMemory`. Also `Game.cpu.limit`, `Game.cpu.tickLimit`, `Game.cpu.bucket`,
    `Game.shard`, and heap statistics if the function exists.
  - JS syntax support probed with `new Function(...)` in try/catch: optional chaining, nullish coalescing, class fields, BigInt,
    and async/await. This sets the Babel target.
  - It prints one compact JSON line that the user copies back.
- `js/probe/main.js` is the same probe as a deployable module, for private servers or a spare branch. Never deploy it to an official
  server's active branch, because that replaces the running bot.
Ask the user to run the snippet on each target server and paste the output. Record it per server in VERIFICATION_LOG with the date.
If the console lacks `WebAssembly`, record that and use the private-server deploy path.

## 4. Draft the ADRs in `docs/DECISIONS.md`
Fill in context, options and evidence for each one, and leave the status Proposed:
- ADR-001: build tool
- ADR-002: wasm feature target and Rust toolchain/edition
- ADR-003: trap recovery
- ADR-004: persistence encoding
- ADR-005: constants source
- ADR-006: dependencies
- ADR-007: planner approach (decided at P4 with benchmarks)
- ADR-008: loader language and transpile target
Recommend an option for each, citing the evidence.

## 5. Close P0
- Update `docs/STATUS.md`: answers captured, rows verified, probe results, and ADRs awaiting acceptance.
- Tell the user which ADRs need their decision before P1 starts, then run `/phase-gate P0`.
- Offer a commit message for the docs (`/commit-msg`).
