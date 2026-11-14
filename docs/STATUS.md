# Status

**Current phase:** P1, build pipeline, loader, snapshot, scheduler, profiler, persistence, consts self-check (in progress: module 1 done). P0 passed 2026-11-13.
**Pinned versions:** Rust 1.100.0 and edition 2024, pinned in `rust-toolchain.toml` and the root `Cargo.toml` (P1.1). Still to pin in P1 (ADR-001 and 006; VERIFICATION_LOG V1–V13): screeps-game-api 0.23.5, wasm-bindgen and wasm-bindgen-cli 0.2.130, binaryen version_133.
**Target servers:** official MMO, shard3 (free tier) on the core track. Optional tracks: private server (`docs/ROADMAP_PRIVATE.md`, none running yet) and Seasonal (`docs/ROADMAP_SEASONAL.md`, after P12). See `docs/PROJECT_CONTEXT.md`.

## Blockers
- The GitHub link stays (the user's decision, ADR-012) and every push to `main` triggers the sync (V77, user-observed). It still points at the repository root, which has no modules, so every push syncs the root. Order (ADR-012): first push a commit containing a checked `deploy/`, then re-point the Folder to `deploy` and press Sync, which overwrites the account's code at once with what `deploy/` holds on GitHub.
- Local Node is 18.19.1. The API tools (`npm run deploy`, `console` and `stats`, all on screeps-api 2.x) need Node 22 or 24 (V11). The MMO push deploy (ADR-012) and the build do not, but P1's live captures use `console` and `stats`. Upgrading is the user's call.

## Open decisions
See `docs/DECISIONS.md`. Accepted: ADR-001 (build tool), 002 (toolchain and wasm features), 003 (trap recovery), 005 (constants source), 006 (dependencies), 008 (loader and transpile target), 009 (crate names), 010 (roadmap tracks), 012 (deploy through the GitHub link; supersedes 011). Proposed: ADR-004 (decided in P1), ADR-007 (decided in P4).

## Phases: MMO core track (`docs/ROADMAP.md`)
- [x] P0 Kickoff and verification (gate passed 2026-11-13)
- [ ] P1 Build pipeline, loader, snapshot, scheduler, profiler, persistence, consts self-check
- [ ] P2 Bootstrap and movement core
- [ ] P3 Economy
- [ ] P4 Base planner
- [ ] P5 Global spawn queue
- [ ] P6 Remote mining
- [ ] P7 Defense
- [ ] P8 Expansion
- [ ] P9 Late game
- [ ] P10 Offense core
- [ ] P11 Siege breaker
- [ ] P12 Debug, visuals and final deliverables

## Optional tracks (never block the core; ADR-010)
Private server (`docs/ROADMAP_PRIVATE.md`):
- [ ] PS1 Private server setup (after P1)
- [ ] PS2 Baselines (after PS1 and P2)
- PS3 Scripted scenarios (each after PS1 and its core phase):
  - [ ] trap-test (P1)
  - [ ] zero-creep recovery (P2)
  - [ ] scripted invader (P6)
  - [ ] scripted attacks (P7)
  - [ ] dummy base (P10)
  - [ ] breaker vs towers (P11)
- [ ] PS4 Automated server tests (after PS1)

Seasonal (`docs/ROADMAP_SEASONAL.md`):
- [ ] SE1 Season verification (after P12)
- [ ] SE2 Season profile and modules (after SE1)

## Current phase checklist (P1, `docs/ROADMAP.md`)
Installs (each asks first): Rust 1.100.0 with rustfmt, clippy and the `wasm32-unknown-unknown` target is installed (2026-11-13, V82). Caveat: rustup's default auto-install lets a plain cargo command install a missing pinned toolchain without asking (fresh clone or after a bump; V82). `RUSTUP_AUTO_INSTALL=0` would stop that, and setting it is the user's call. Still needed for module 3: wasm-bindgen-cli 0.2.130 (cargo install) and binaryen version_133. Deploying also needs Node 22 or 24 (see Blockers).
- [x] 1. Workspace: root `Cargo.toml`, the three crates, `default-members` without rustedscreep-wasm, release profile, `rust-toolchain.toml`, clippy no-panic denies
- [ ] 2. `rustedscreep-consts`: re-exports of the crate's constants and pure local types (ADR-005), `extra.rs`, constant-derived tests
- [ ] 3. Build: cargo build, then wasm-bindgen CLI at the Cargo.lock version, then wasm-opt `-O4` (ADR-001/002); `tsc` to ES2022 plus Rollup (ADR-008); npm scripts including `stage` into the tracked `deploy/` folder (ADR-012); `.example-screeps.yaml`
- [ ] GitHub-link deploy path verified (V77; needs the user to name `mmo` and push): wasm hash matches in game, sentinel shows deletion behavior, delay, identical re-sync (the trigger on `main` is confirmed)
- [ ] 4. Loader: bucket-gated boot, `loop`, trap and abort recovery (ADR-003), `console.error` routing, `trap-test`, TextEncoder/TextDecoder polyfill, boot log of build ID and wasm hash (V77)
- [ ] 5. Snapshot
- [ ] 6. Scheduler
- [ ] 7. Act layer
- [ ] 8. Profiler
- [ ] 9. Persistence (decides ADR-004)
- [ ] 10. Config: `mmo` profile, runtime overrides, TUNING.md and the doc-sync test
- [ ] 11. Constants self-check
- [ ] Gate evidence (ROADMAP P1 gate, all live checks on the MMO): `/check` green; wasm loads; 10 forced traps in a row recover within the ADR-003 bound; cold-start cost measured and the boot gate set from it; empty-bot CPU floor measured; persistence round trip passes natively (with the corrupt-segment test) and once live; sizes recorded
Timing for the P1 live checks: an RCL1 room with no upgrading is lost about 20000 ticks after it is claimed (V71). Run them shortly before P2 is ready, or respawn before the P2 deploy.

## Measurements (latest)
| Metric | Server | Value | Date | Method |
|---|---|---|---|---|
| CPU limit | MMO shard3 | 20 | 2026-11-13 | console probe (V72) |
| tickLimit | MMO shard3 | 500 | 2026-11-13 | console probe (V72) |
| Bucket at probe time | MMO shard3 | 6387 | 2026-11-13 | console probe (V72) |
| Heap size limit | MMO shard3 | 775946240 bytes | 2026-11-13 | console probe, getHeapStatistics (V73) |
| Largest console probe unit | n/a | 864 JSON characters (limit 1024, V69) | 2026-11-13 | `node js/probe/check.mjs` |

## Log (newest first)
- 2026-11-13: P1.1 workspace done. Root `Cargo.toml` (resolver 3, three members, `default-members` without rustedscreep-wasm, `[workspace.package]` edition 2024, rust-version 1.100, release profile `opt-level=3`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip="debuginfo"`), `rust-toolchain.toml` (1.100.0, minimal, rustfmt, clippy, wasm32), three empty crates, the wasm crate a `cdylib`. No dependencies yet. Crate roots deny unwrap_used, expect_used, panic, indexing_slicing, todo, unimplemented and unreachable outside tests (the last three added with the user's OK; core-logic.md updated). `strip="debuginfo"`, not `true`, because "symbols" drops `target_features`, which wasm-bindgen 0.2.130 needs (V81, D7). Evidence: `cargo fmt --all -- --check`, native clippy, wasm32 clippy and `cargo test -p rustedscreep-core -p rustedscreep-consts` (0 tests) all exit 0. `cargo tree` shows no dependencies for core or consts. Probes in a scratch copy: each lint fires exactly once on a planted violation in core (native) and wasm (wasm32), none fires under `cfg(test)`, an unknown lint name fails, and a bare `cargo build` skips the wasm crate. Release wasm sections per strip value: V81. Verification rows V78–V84 (4 researchers, 4 skeptics). /check rows 6–8 (tsc, `npm run build`, sizes) are n/a until modules 3–4. Independent review (3 lenses plus skeptics): 4 findings, 1 confirmed and fixed (the toolchain comment said rustup asks first, but its auto-install does not; V82 and the install caveat above), 3 refuted. V81 wording on when wasm-bindgen warns made precise.
- 2026-11-13: P1 gate evaluated: not passed, because no P1 module exists yet. All 8 criteria fail. Evidence: no `Cargo.toml`, `crates/`, `rust-toolchain.toml`, `package.json`, `dist/` or `deploy/` in the repo; `cargo test -p rustedscreep-core -p rustedscreep-consts` reports "could not find `Cargo.toml`"; `npm run check` exits 1 ("Missing script: check", because npm resolves the prefix to the home folder). Toolchain found: rustc 1.99.0 (pin is 1.100.0), host target only (no `wasm32-unknown-unknown`), no `wasm-bindgen` or `wasm-opt`, Node 18.19.1. Added a gate-evidence checklist entry.
- 2026-11-13: With the user's OK, hardened `.claude/hooks/guard-bash.mjs` (GitHub writes, git plumbing that publishes, quoted, wrapped, nested and eval forms) and added `ask` rules for `npm run stage` and `gh api` to `.claude/settings.json`. Evidence: `node .claude/hooks/test-guard-bash.mjs` passes 60 block and 32 allow cases; `node --check` and the JSON parse pass.
- 2026-11-13: Independent review of the ADR-012 change: 20 findings confirmed and fixed (deploy dry run no longer writes `deploy/`; re-point order and its immediate overwrite stated; API fallback requires unlinking first; V77 test uses the first checked build with a byte hash, a sentinel module and a docs-only push; Steam mirror rule; ADR-012 Evidence field; V74/V75 repointed), 8 refuted. Hardening guard-bash.mjs and settings.json `ask` rules await the user's OK.
- 2026-11-13: The user confirmed that every push to `main` triggers the GitHub sync and that the bot script will deploy from a `deploy/` folder. V77, ADR-012, ROADMAP P1 gate and STATUS updated; the P1 check now covers only `.wasm` handling, deletion and delay.
- 2026-11-13: The user keeps the client's GitHub link for live testing. ADR-012 (deploy through the link, from a tracked `deploy/` folder that `npm run stage` fills with a checked build) accepted and supersedes ADR-011. V77 records the link's unknowns as a P1 check. ROADMAP P1, `/deploy`, CLAUDE.md, PROJECT_CONTEXT and TEST_PLAN updated.
- 2026-11-13: P0 gate passed. Evidence: (1) the user answered both question rounds (PROJECT_CONTEXT); (2) MMO probe recorded (VERIFICATION_LOG section D, V72; raw output in docs/probe-results/); (3) ADR-001, 002, 003, 005, 006 and 008 Accepted (002 and 008 with 011 on "Accept all three"); (4) every tooling row with an unverified part states an assumption and a fallback (only V69 remains, with a safe default; V27 and V62 superseded by V72). `node js/probe/check.mjs` passes; SPEC unchanged. Also aligned ROADMAP P1.3, the js-loader rule and the CLAUDE.md wasm trap with ADR-002/008. Current phase set to P1.
- 2026-11-13: Researched the client's code-sync paths (two researchers plus a reconciler that read the installed client, packageVersion 248): the Steam mirror has a fixed path, is two-way and corrupts `.wasm` on write-back (V74); the repo link is most likely the GitHub integration (V75); MMO code size 2 MB per a 2018 staff post (V76, inferred). ADR-011 (deploy path) proposed: deploy only with the repo's tool.
- 2026-11-13: MMO probe recorded (shard3, Game.time 84257673): all 10 wasm features and all 21 syntax checks through ES2022 accepted, controls correct; TextDecoder/TextEncoder undefined; heap_size_limit 775946240; CPU limit 20, bucket 6387. Raw output in docs/probe-results/; rows V72 and V73. ADR-002 and ADR-008 evidence updated; both are ready for acceptance.
- 2026-11-13: Independent review of the roadmap split: 18 findings confirmed and fixed (PS3 gates require the stated outcomes; PS2/PS3 build on PS1; per-server boot gates in PS1 and SE2; MMO naive-baseline method; `/phase-gate` keeps the core current phase for PS/SE gates; kickoff and deploy skills no longer lean on a private server; season feature may not exist; TEST_PLAN §7–§8), 6 refuted. Added the P1 controller-downgrade warning (V71, verified from engine a90d26f and common 2fb779b).
- 2026-11-13: Split the roadmap into three tracks at the user's request (ADR-010): `docs/ROADMAP.md` is the MMO core (P0–P12) and never depends on the optional `docs/ROADMAP_PRIVATE.md` (PS1–PS4) or `docs/ROADMAP_SEASONAL.md` (SE1–SE2, after P12). The private-server dependencies in ten core phases (P0, P1, P2, P4–P8, P10, P11) now use native scenario tests plus live MMO runs (P0's private-server probe moved to PS1); the scripted and destructive scenarios moved to PS3. The P13 entry is replaced by SE1–SE2. Skills, CLAUDE.md, KIT_README, TEST_PLAN, PROJECT_CONTEXT, ADR-002/004/008 and the probe note updated.
- 2026-11-13: Seasonal moved after the World MMO work at the user's request: new ROADMAP P13 (Seasonal), which starts only after P12's gate. P0 now probes the MMO and the private server only, and P1 builds the `mmo` and `private` profiles. PROJECT_CONTEXT, ADR-002/008 wording and the VERIFICATION_LOG probe note updated.
- 2026-11-13: The user accepted ADR-001 (wasm-bindgen CLI + pinned wasm-opt), ADR-003 (hybrid recovery, halt first), ADR-005 (re-export the crate's constants) and ADR-006 (dependency list) as recommended ("Accept all four"). VERIFICATION_LOG D6 resolution updated. ADR-002 and 008 still wait on the probe.
- 2026-11-13: P0 gate evaluated: not passed (probe results and ADR acceptance pending). Probe split into five console pastes after a review found the 1024-character console limit (V69). `node js/probe/check.mjs` passes on Node 18.19.1 (unit JSON lengths 777, 745, 713, 864, 724; bytes 10/10 match the references; 5 injected faults each fail it). acorn 8.8.1 parses all five units and main.js as ES5.
- 2026-11-13: Independent review of the kickoff changes: 18 findings confirmed and fixed (console limit, V57/V62/V68 corrections, R2/R9 assumptions, S3 coverage via V70, source URLs, stale pointers); 13 refuted.
- 2026-11-13: P0 verification recorded in VERIFICATION_LOG section F: 70 rows (V1–V70) from 6 researchers plus 6 verbatim skeptic passes. 25 S/R rows marked superseded (only their Confidence cell changed); R2 and R9 stay open with assumptions; 22 open rows in total (R2, R9 and later-phase groups). Key findings: official servers announced Node 24 on 2026-04-01 (V62, inferred until the probe confirms); the engine intent-priority table at a90d26f matches S12 (V44); crate 0.23.5 tests natively (V29).
- 2026-11-13: Kickoff answers captured in PROJECT_CONTEXT (MMO free tier, Seasonal, private; fresh respawn; pixels and market; no allies; conservative breaker; no private server yet; stable-only toolchain).
- 2026-11-13: Renamed the placeholder crates to `rustedscreep-consts`, `rustedscreep-core` and `rustedscreep-wasm` (ADR-009) across CLAUDE.md, KIT_README.md, `.claude/` and `docs/`. Evidence: `node --check .claude/hooks/guard-purity.mjs` passes; settings.json parses; 8/8 guard-purity block/allow cases pass on the new paths; `grep -rnIE 'game[-_]consts|bot[-_](core|wasm)'` matches only ADR-009's history.
- 2026-11-13: Claude Code kit installed. Next step: `/kickoff`.
