# Architecture decision records

Format for each ADR: number, title, status (Proposed, Accepted, or Superseded by ADR-N), date, context, options, decision, consequences, evidence.
Claude proposes ADRs and the user accepts them. Numbers are never reused.

---

## ADR-001 Build toolchain: Accepted (2026-11-13)
- Context: SPEC §1 names wasm-pack (or cargo-screeps). cargo-screeps doesn't work with screeps-game-api ≥0.22, and the rustwasm org is archived (VERIFICATION_LOG S3, S8).
- Options:
  - (a) wasm-pack `build --target <verified>` followed by wasm-opt, which is what the starter uses.
  - (b) `cargo build --target wasm32-unknown-unknown`, then the wasm-bindgen CLI pinned to the crate version, then wasm-opt. This is more explicit and drops a tool dependency.
- Decide by: reproducibility, version-match enforcement, CI simplicity, and output parity with the loader.
- Evidence (VERIFICATION_LOG section F):
  - cargo-screeps 0.5.2 (2024-01-15) is the latest release, and the starter README says screeps-game-api 0.22+ is incompatible with it (V12). Option "cargo-screeps" is out (D1).
  - wasm-pack now lives at github.com/wasm-bindgen/wasm-pack, latest v0.15.1, 2026-10-30 (V6).
  - The starter runs `wasm-pack build --target web --release`, and wasm-pack runs wasm-opt `-O4` from Cargo.toml metadata. The starter does not pin the wasm-opt version (V15, V16).
  - The wasm-bindgen CLI must match the crate's schema version. Pinning the CLI to the exact Cargo.lock version is the safe rule (V4). wasm-bindgen 0.2.130 and wasm-bindgen-cli 0.2.130 are current.
  - binaryen version_133 has the lowering passes ADR-002 may need (V7, V8).
  - Both options produce the same `--target web` glue, which exposes `initSync({ module })` (V18).
- Recommendation: (b). The build script runs `cargo build --release --target wasm32-unknown-unknown -p rustedscreep-wasm`, then `wasm-bindgen --target web` at the exact Cargo.lock version (the script fails if the installed CLI differs), then a pinned binaryen `wasm-opt` with the ADR-002 flags. This pins every tool that touches the binary, and ADR-002 may need wasm-opt flags beyond what wasm-pack's metadata hook makes easy to audit. Cost: two tools to install (`cargo install wasm-bindgen-cli --version <lock>` and a binaryen release), and both installs ask first.
- Decision: (b), as recommended; the user accepted it on 2026-11-13 ("Accept all four", covering ADR-001, 003, 005 and 006 as recommended).
- Consequences: the P1 build script runs `cargo build --release --target wasm32-unknown-unknown -p rustedscreep-wasm`, then `wasm-bindgen --target web`, failing if the installed CLI differs from the Cargo.lock version, then a pinned binaryen `wasm-opt` with the ADR-002 flags. wasm-pack is not used. This is the deviation from SPEC §1 that D6 records. Installing the wasm32 target, wasm-bindgen-cli and binaryen needs the user's approval (rustup and cargo install always ask).

## ADR-002 Wasm feature target and Rust toolchain/edition: Accepted (2026-11-13)
- Context: Rust 1.82 and later emit reference-types and multivalue, and Rust 1.87 and later add bulk-memory (S6). The server's Node may reject these, and the starter documents `--signext-lowering` (S4).
- Options:
  - (a) Stable toolchain pinned to the newest version whose default features the probe accepts.
  - (b) Nightly with `-Ctarget-cpu=mvp` and `-Zbuild-std=panic_abort,std`, plus selected `+features`.
  - (c) Latest stable plus binaryen lowering passes, if they cover everything.
- Edition follows the toolchain (2024 needs a recent stable).
- Decide by: feature probe results (VERIFICATION_LOG §D), build reproducibility, and runtime CPU of the resulting wasm (measure).
- Constraint from the user (PROJECT_CONTEXT): stable only, pinned. Option (b) is therefore out.
- Evidence (VERIFICATION_LOG section F):
  - Rust stable is 1.100.0 (V13). Its wasm32-unknown-unknown default features are multivalue, mutable-globals, reference-types and sign-ext, plus bulk-memory and nontrapping-fptoint since 1.87 (V22). Turning them off for std needs nightly build-std (V23), which the user's policy rules out.
  - wasm-opt (binaryen version_133) can lower sign-ext, bulk-memory (memory.copy/fill) and nontrapping-fptoint. It has no multivalue or reference-types lowering pass (V8). A caveat: rustc's reference-types default may only change the call_indirect table-index encoding, so `--disable-reference-types` may still work on a real binary. Test this in P1.
  - The official servers have run Node 24 since 2026-04-01, per Screeps' own Steam announcement, which says the private server binaries were updated too (V62). It is an official changelog (priority 4), so the log marks it `inferred` until the probe confirms it per server. Node 24 supports all six Rust default features. By webassembly.org's table, Node added the last of them, reference-types, in 17.2 (V27). The starter dropped `--signext-lowering` and the mvp build on 2026-04-03 for that reason (V16). The probe confirms this on the MMO. The optional tracks probe the private server (PS1) and Seasonal (SE1).
  - The private server (screeps-launcher) installs Node 24 by default (V61).
  - Edition 2024 needs Rust 1.85+ (V25).
  - Local sanity check: Node 18.19.1 (V8 10.2) already accepts all six default features (js/probe/check.mjs).
  - Probe on the official MMO, shard3, 2026-11-13 (V72): all six default features accepted, plus simd128, legacy EH, exnref and tail-call, with correct controls. The recommendation's condition is met on the core target, so no lowering pass or `--disable-*` flag is needed there.
- Recommendation: (a) with today's stable. Pin Rust 1.100.0 (or the stable current when P1 starts) in `rust-toolchain.toml`, use edition 2024, keep the default target features, and run wasm-opt `-O4` only, provided the probe accepts mutable-globals, sign-ext, nontrapping-fptoint, bulk-memory, reference-types and multivalue on the official MMO (core track). The optional tracks re-check it on the private server (PS1) and Seasonal (SE1). Fallbacks, in order: if a server rejects only sign-ext, bulk-memory or nontrapping-fptoint, add the matching wasm-opt lowering pass (c). If it rejects reference-types or multivalue, test `--disable-*` on a real binary. If that fails, pin the newest older stable whose defaults pass (edition 2021 below 1.85). On that path criterion (MSRV 1.86) and proptest (MSRV 1.88) need older versions (ADR-006).
- Decision: (a), as recommended; the user accepted it on 2026-11-13 ("Accept all three", covering ADR-002, 008 and 011 as recommended).
- Consequences: `rust-toolchain.toml` pins Rust 1.100.0 (the local machine has 1.99, so P1 installs 1.100.0 and the wasm32 target, and rustup asks first). Every crate uses edition 2024. The wasm build keeps the default target features, and wasm-opt runs `-O4` with no lowering pass. The private (PS1) and Seasonal (SE1) tracks re-run the probe and apply the fallback chain only if their server rejects a feature.

## ADR-003 Trap and abort recovery: Accepted (2026-11-13; (b) adopted only after P1 measurement)
- Context: SPEC §1 asks to re-instantiate on the next tick and never brick the bot. The starter uses `Game.cpu.halt()` after an aborted tick because of wasm-bindgen #3130 (S5).
- Options:
  - (a) `Game.cpu.halt()`. This is simple and proven, but costs a full global reset plus recompile.
  - (b) Discard the instance and the glue module (clear the require cache), then instantiate a fresh one from the cached `WebAssembly.Module`. This is cheaper if it is sound.
  - (c) A hybrid: try (b), and fall back to (a) after repeated failures.
- Decide by: P1 `trap-test` results (time to recover, CPU, correctness over 10 consecutive traps) and whether `halt` is available per server.
- Evidence (VERIFICATION_LOG section F):
  - The starter loader runs require, `new WebAssembly.Module` and `initSync` in one tick once the bucket gate (1500) passes. If `running` is still set at the start of a tick, it calls `Game.cpu.halt()`. Its catch block logs but does not reset `running`, so any thrown trap also leads to a halt on the next tick (V17).
  - `halt()` resets the runtime and wipes the heap. The isolate's heap limit is about 256 MB plus terrain data on the private server (driver 5.3.0, V43). The MMO limit is unverified, and the probe's env part reports `heap_size_limit`.
  - Unwinding on wasm needs nightly and build-std (V26), which the user's stable-only policy rules out, so panics abort and recovery has to live in JS (D3).
  - `Game.cpu.halt` exists on the official MMO (V72). The MMO heap limit is about 740 MiB (V73).
  - Open question for P1: whether the `--target web` glue can be initialized a second time. That means reading the generated glue for the pinned wasm-bindgen to see how `initSync` treats an already-set instance, and whether the glue must be re-evaluated as a separate module.
  - Answered in P1.4 (2026-11-14, V97): `initSync` cannot re-initialize, because it returns early once an instance exists. wasm-bindgen 0.2.130's `--experimental-reset-state-function` exports `__wbg_reset_state()`, which builds a new instance from the module the glue holds and reruns the start function. The build passes the flag, so (b) is available in every build.
  - P1.4 sandbox evidence (V99; node:vm following the open-source runtime, V96): 10 forced traps in a row recovered with (a) in 3 ticks each (11 globals) and with (b) in 2 ticks each (1 global, no halt). Both are correct there; the MMO trap-test still has to measure cost and correctness before (b) can become the default. The halting tick's Memory is discarded (V96), so the loader records each incident on the trap tick and halts on the next one.
- Recommendation: (c), the hybrid. Ship (a) as the P1 baseline because it is proven. Prototype (b) behind a config flag and adopt it only if `trap-test` shows it is correct over 10 consecutive traps and cheaper than a halt plus recompile. Back off after repeated failures.
- Decision: (c), the hybrid, as recommended; the user accepted it on 2026-11-13 ("Accept all four", covering ADR-001, 003, 005 and 006 as recommended).
- Consequences: the P1 loader ships the halt-based recovery (a) first. Re-instantiating from the cached Module (b) goes behind a config flag, off by default, and becomes the default only if `trap-test` shows it correct over 10 consecutive traps and cheaper than a halt plus recompile. The measured numbers and the final default are recorded here in P1. Backoff and one notification per incident apply in both modes.

## ADR-004 Persistence encoding: Proposed (P1)
- Context: RawMemory segments are strings. Persist only data that must survive a reset, in a compact, versioned form, written only when dirty.
- Options: (a) base64 of a binary layout, (b) a denser UTF-16 packing scheme, (c) a hand-rolled varint layout with either of those.
- Decide by: bytes per segment, encode and decode CPU (bench plus in-game), and robustness against corruption (checksum).
- Evidence (VERIFICATION_LOG section F):
  - RawMemory has 100 segments (ids 0-99), at most 10 active at once, and 100 KB per segment. Requested segments appear on the next tick, and the driver rejects oversize writes at end of tick (V41). Memory itself is capped at 2 MB (V40).
  - The segment and Memory limits are checks on JS string `.length`, so they count UTF-16 code units, not bytes (V40, V41, read verbatim in the driver).
  - Every segment present in `RawMemory.segments` at end of tick is re-saved and counts toward the 10-segment cap. Exceeding the cap throws after the loop and likely loses the tick (V41, inferred), so the segment manager must unload segments it no longer needs.
- Recommendation: decide in P1 by benchmark, as planned. Because the limit counts code units, (b) a dense UTF-16 packing (for example 15 bits per code unit, which avoids lone surrogates) holds about 2.5 times what base64 holds per segment (6 bits per code unit). Lean towards (b) with a versioned header and checksum, provided a live round-trip test on the MMO shows the storage path preserves every code unit. Otherwise use (a).
- Decision: _pending (P1)_

## ADR-005 Constants source for native tests: Accepted (2026-11-13)
- Context: pure crates must test natively, and constants should come from the API (SPEC §0).
- Options:
  - (a) `rustedscreep-consts` re-exports `screeps-game-api` constants and local types, which requires the crate to build and run natively.
  - (b) `rustedscreep-consts` mirrors the values with sources, and the in-game self-check catches drift.
- Decide by: R4 (native build of screeps-game-api).
- Evidence (VERIFICATION_LOG section F):
  - screeps-game-api's own CI runs `cargo clippy --all-features` and `cargo test --all-features` on native Ubuntu. lib.rs has no `target_arch = "wasm32"` gate. The only cfg gate is `feature = "mmo"` (V29).
  - In `constants`, five files are free of JS bindings. find.rs, look.rs, small_enums.rs and types.rs use `#[wasm_bindgen]` (look.rs has a crate-private extern block), but their public items are plain Rust values and enums that compile natively. `local::RoomName`, `Position` and `LocalCostMatrix` are pure logic, apart from conversions from JS types and Position's JS-wrapping `game_methods` (V30). wasm-bindgen is a plain dependency and compiles natively. Calling a JS-backed function natively would fail at runtime (inferred).
  - The crate already exports most CPU and memory limits (`INTENT_CPU_COST`, `CPU_BUCKET_MAX`, `CPU_TICK_LIMIT_MAX`, `PIXEL_CPU_COST`, `MEMORY_SIZE_LIMIT`, `MEMORY_SEGMENT_SIZE_LIMIT`, `MEMORY_SEGMENT_ACTIVE_LIMIT`, `INTER_SHARD_MEMORY_SIZE_LIMIT`; V34-V42), so `extra.rs` needs fewer hand-written values.
  - The purity hook already blocks `screeps::` in rustedscreep-core and JS-backed screeps modules in rustedscreep-consts.
- Recommendation: (a). rustedscreep-consts re-exports `screeps::constants` and the pure `screeps::local` types, and a native `cargo test -p rustedscreep-consts` smoke test (for example `RoomName` parsing plus a constant) proves it in P1. Values the API does not export go in `extra.rs` with their sources. (b) stays the fallback if a future crate version stops building natively.
- Decision: (a), as recommended; the user accepted it on 2026-11-13 ("Accept all four", covering ADR-001, 003, 005 and 006 as recommended).
- Consequences: rustedscreep-consts depends on screeps-game-api and re-exports `screeps::constants` and the pure `screeps::local` types. A native `cargo test -p rustedscreep-consts` smoke test in P1 proves the native build. `extra.rs` holds only values the crate does not export, with sources and dates. Native code never calls JS-backed functions, and the purity hook enforces the module boundary. If a future crate version stops building natively, this ADR is superseded by (b).

## ADR-006 Dependencies: Accepted (2026-11-13)
- Candidates: `rustc-hash` (Fx), `smallvec` or `arrayvec`, `criterion` (dev), `proptest` (dev), `log` and `fern`, and `console_error_panic_hook`.
  Record the version and reason for each, and reject anything that pulls in JS crates or serde in the tick path.
- Evidence (crates.io API, read 2026-11-13; VERIFICATION_LOG V68):

  | Crate | Latest | Published | MSRV | Default normal deps | Goes in | Use |
  |---|---|---|---|---|---|---|
  | rustc-hash | 2.1.3 | 2026-07-02 | 1.77 | none (rand optional) | core | FxHash maps and sets |
  | arrayvec | 0.7.8 | 2026-07-02 | 1.51 | none (serde optional) | core | fixed-capacity buffers. screeps-game-api already depends on it (V28) |
  | smallvec | 1.16.5 | 2026-11-06 | none set | none (serde optional) | core, only if arrayvec doesn't fit | spill-to-heap small vectors |
  | criterion | 0.8.2 | 2026-02-04 | 1.86 | n/a (dev) | core (dev) | benches |
  | proptest | 1.12.0 | 2026-10-10 | 1.88 | n/a (dev) | core (dev) | property tests |
  | log | 0.4.35 | 2026-10-30 | 1.71 | none (serde optional) | all | logging facade |
  | fern | 0.7.1 | 2024-12-15 | 1.60 | log | wasm | log backend |
  | console_error_panic_hook | 0.1.7 | 2021-10-11 | none set | cfg-if, wasm-bindgen | wasm only | panic messages to console.error |

  - screeps-game-api 0.23.5 itself depends on serde, serde_json and serde-wasm-bindgen (V28). Serde is therefore in the tree regardless. The "no serde in the tick path" rule governs our own code, not the dependency tree.
  - Every MSRV is at most 1.88, so Rust 1.100 (ADR-002) builds them all. If ADR-002 falls back to an older stable, criterion (1.86) and proptest (1.88) need older versions.
- Recommendation: accept rustc-hash, arrayvec, log and fern, plus criterion and proptest as dev-dependencies of rustedscreep-core only, and console_error_panic_hook in rustedscreep-wasm only. Add smallvec only when a measured case needs spill-to-heap. Enable no optional features. Pin exact versions in Cargo.lock.
- Decision: as recommended; the user accepted it on 2026-11-13 ("Accept all four", covering ADR-001, 003, 005 and 006 as recommended).
- Consequences:
  - rustedscreep-core gets rustc-hash, arrayvec and log, plus criterion and proptest as dev-dependencies only.
  - rustedscreep-wasm gets log, fern and console_error_panic_hook.
  - smallvec waits for a measured need.
  - No optional features are enabled, and versions are pinned in Cargo.lock in P1. `cargo add` always asks first.
  - If ADR-002 falls back to an older stable, criterion and proptest move to versions within that MSRV.

## ADR-007 Base planner approach: Proposed (decided in P4)
- Context: SPEC §3.3 asks for an automatic base planner (stamp/bunker or distance transform) with incremental, cached placement. ROADMAP P4 decides between the candidates by benchmark on a terrain corpus.
- Options: (a) stamp or bunker layouts, (b) distance transform with flood fill and min-cut ramparts, (c) a hybrid.
- Decide by: corpus validity, rampart count, path lengths, planner CPU (criterion), and time-sliced in-game cost.
- Evidence: none yet. It needs the P4 terrain corpus and the criterion benches. No recommendation before P4, because a recommendation now would not rest on measurement.
- Decision: _pending (P4)_

## ADR-008 Loader language and transpile target: Accepted (2026-11-13)
- Context: SPEC asks for a TypeScript loader. The starter says Babel is required for the older Node on official servers (S3).
- Options: (a) TS → Babel preset-env targeting the probed JS level, then Rollup. (b) TS with `tsc` targeting ES5 or ES2015, then Rollup.
- Decide by: probe syntax results, bundle size, and source-map usefulness.
- Evidence (VERIFICATION_LOG section F):
  - The official servers run Node 24 since 2026-04-01 (V62, an official announcement, `inferred` until the probe confirms it), and the starter now sets Babel preset-env `targets: { node: 24 }` (V21). The probe measures the real JS syntax level on each server (21 syntax checks, ES2015 to ES2022).
  - Babel 8 (@babel/core 8.1.2, @babel/preset-env 8.0.7) is ESM-only and needs Node ^22.18.0 or >=24.11.0. The Babel 7 line (7.29.x) is still published (V10).
  - The `screeps-api` npm package 2.1.0, used for deploy, needs Node 22.x or 24.x (V11). Rollup 4.68.3 needs Node >=18 (V9).
  - The local Node is 18.19.1 (PROJECT_CONTEXT), so neither Babel 8 nor screeps-api 2.x runs locally as installed.
  - Probe on the official MMO, shard3, 2026-11-13 (V72): all 21 syntax checks through ES2022 compile, and `TextDecoder` and `TextEncoder` are undefined.
  - Neither Babel preset-env without core-js nor tsc adds runtime polyfills. Both only lower syntax. The one runtime gap the starter fills is a TextEncoder/TextDecoder polyfill, and the probe checks for both.
- Recommendation: (b), TypeScript compiled by `tsc` (or Rollup's TypeScript plugin) with `target` set to the highest ES level the probe accepts on the official MMO (the optional tracks re-check the private server in PS1 and Seasonal in SE1), then Rollup. If the probe shows ES2022 or newer everywhere, Babel adds nothing and is one less toolchain with a Node 22+ requirement. On the MMO that target is ES2022 (V72), and the loader must ship the smallest TextEncoder/TextDecoder polyfill the wasm-bindgen glue needs. Use (a) only if a server needs a syntax level that tsc handles badly. Separately, the local Node must move to 22 or 24 for the deploy tool. That upgrade is the user's call.
- Decision: (b), as recommended; the user accepted it on 2026-11-13 ("Accept all three", covering ADR-002, 008 and 011 as recommended).
- Consequences: the loader is TypeScript compiled with `target: ES2022` and bundled by Rollup into `dist/main.js`, with no Babel. The bundle ships the smallest TextEncoder/TextDecoder polyfill the wasm-bindgen glue needs. The TypeScript and Rollup plugin versions are verified in P1 before pinning. The API tools (`npm run deploy`, `console` and `stats`, all on screeps-api 2.x) need Node 22 or 24 locally (V11). The MMO push deploy (ADR-012) does not. The PS1 and SE1 probes re-check the ES level on their servers.

## ADR-009 Crate names: Accepted (2026-11-13)
- Context: the kit shipped with placeholder crate names `game-consts`, `bot-core` and `bot-wasm`. The repository is named `rustedscreep`. CLAUDE.md treats the crate layout as an architecture invariant, and KIT_README says a rename must update CLAUDE.md, the rule `paths` and `guard-purity.mjs` together.
- Options:
  - (a) Prefix all three: `rustedscreep-consts`, `rustedscreep-core`, `rustedscreep-wasm`.
  - (b) As (a), but name the cdylib `rustedscreep`.
  - (c) Rename only the `bot-*` crates and keep `game-consts`.
- Decision: (a), chosen by the user. Each crate's directory matches its name (`crates/rustedscreep-consts`, `crates/rustedscreep-core`, `crates/rustedscreep-wasm`), and Rust paths use `rustedscreep_consts::`, `rustedscreep_core::` and `rustedscreep_wasm::`. Roles, boundaries and purity rules are unchanged.
- Consequences: the wasm-bindgen output files are expected to be named after the cdylib (`rustedscreep_wasm.js`, `rustedscreep_wasm_bg.wasm`). This is an assumption until it is verified against the ADR-001 toolchain in P1. Formula doc comments read `/// Uses: rustedscreep_consts::X`. No crates existed yet, so nothing was migrated.
- Evidence: the user's request on 2026-11-13. Every reference in CLAUDE.md, KIT_README.md, `.claude/` (rules, skills, agents, `guard-purity.mjs`) and `docs/` was renamed. Code-location cells in `VERIFICATION_LOG.md` were repointed without changing any finding. `SPEC.md` names no crates and was not edited.

## ADR-010 Roadmap tracks: Accepted (2026-11-13)
- Context: the kit shipped one roadmap whose gates leaned on a private server: wasm load, trap test, bootstrap, scripted invaders and attacks, a dummy base and the breaker scenario. The user has no private server, and asked for the private server and Seasonal work to be "totally optional and built off the MMO core track". SPEC §6.4 asks the test plan to include screeps-launcher/private-server instructions and offense tests against a scripted dummy base (including a siege-breaker scenario with towers).
- Options:
  - (a) Keep one roadmap with private-server evidence in the core gates (the kit default).
  - (b) Three roadmaps: an MMO core track (`docs/ROADMAP.md`, P0–P12) that never depends on the others, plus optional private-server (`docs/ROADMAP_PRIVATE.md`, PS1–PS4) and Seasonal (`docs/ROADMAP_SEASONAL.md`, SE1–SE2) tracks built on it.
- Decision: (b), the user's request on 2026-11-13: "make the private server portion an entirely option separate track from the main road map. Do the same for the seasonal changes." The Seasonal track starts only after P12, per the user's earlier instruction the same day.
- Consequences:
  - Core gates use native scenario tests in `rustedscreep-core` plus live MMO runs. Scripted and destructive in-game scenarios (killing the colony's creeps, scripted attackers, dummy bases) move to PS3.
  - Live offense evidence for P10 and P11 needs a target the user approves, or the user's recorded decision to skip live offense.
  - Naive-baseline comparisons on the MMO run only in short, bucket-gated windows (20 CPU free tier). Long comparisons and the starter-bot reference move to PS2.
  - Server profiles: `mmo` in P1, `private` in PS1, `season` in SE2. The profile table lets optional tracks add profiles as new rows (and, for Seasonal, feature-gated modules) without changing `mmo` behavior or any core gate. The SE2 gate checks this.
  - Skills and CLAUDE.md point at the right roadmap by phase ID: P# (core), PS# (private) and SE# (Seasonal).
  - Deviation from SPEC §6.4: the screeps-launcher instructions (PS1) and the scripted dummy-base offense tests, including the breaker scenario with towers (PS3), are delivered only if the private track runs. The core TEST_PLAN covers unit, native scenario and live MMO evidence and links to the optional sections.
- Evidence: the user's requests on 2026-11-13. The audit of core gates that depended on a private server, recorded in STATUS on 2026-11-13.

## ADR-011 Deploy path to the official MMO: Superseded by ADR-012 (2026-11-13)
- Context: code can reach the MMO three ways: the repo's Node deploy tool (`npm run deploy`, POST /api/user/code through screeps-api, built in P1), the client's GitHub integration (Account → GitHub, VERIFICATION_LOG V75), and the Steam client's local-folder mirror (V74). Project rule (CLAUDE.md): deploy to an official server only when the user names it in the current message. On 2026-11-13 the user linked the repository root in the client and asked whether a subfolder would be better.
- Options:
  - (a) Deploy only with the repo's tool, to a named branch, through `/deploy mmo` (check, dry run, then upload). No GitHub link and no repo folder in the client mirror.
  - (b) GitHub integration on a committed deploy folder (for example built `main.js` and `.wasm` committed to the repo).
  - (c) Point the client mirror at the build output (for example a symlink from the mirror's branch folder to `dist/`).
- Evidence: (b) deploys on every push the integration picks up, which bypasses `/check`, the dry run and the explicit-ask rule. Its `.wasm` handling and deletion behavior are unknown, and it needs build artifacts committed. The repo's top level has no `.js` files and `dist/` is git-ignored, so neither the root nor `dist/` works with it as-is (V75). (c) is two-way, uploads on every rebuild with no debounce, deletes server modules a folder lacks, and corrupts any `.wasm` it writes back (V74). (a) uploads binaries correctly as `{binary: base64}` (V19) and goes through `/deploy`.
- Recommendation: (a). The user unlinks the GitHub integration (Reset in Account → GitHub) and keeps the Steam mirror as a plain folder that the repo never links into. During an API deploy with the client open, decline "replace local scripts" and never choose "Always replace without asking".
- Decision: (a), as recommended; the user accepted it on 2026-11-13 ("Accept all three", covering ADR-002, 008 and 011 as recommended).
- Consequences: `/deploy mmo` (check, dry run, then upload to a named branch) is the only way code reaches the MMO. Unlinking the GitHub integration is the user's action; until the user confirms it, STATUS keeps it as a blocker, because a push could overwrite the live `default` branch.

## ADR-012 Deploy path to the official MMO through the GitHub link: Accepted (2026-11-13; supersedes ADR-011)
- Context: ADR-011 chose the repo's Node tool and asked the user to unlink the client's GitHub integration. The user decided to keep the link: "I am leaving it linked because what we make will be live tested with the link". Every push to `main` triggers the sync (the user's observation, VERIFICATION_LOG V77). Community reports say the sync copies the top-level files of the linked folder into the account's `default` branch (V75, inferred). The repository root has no top-level `.js` files and `dist/` is git-ignored, so a root link cannot deliver the bot. The user pushed commit 665d204 with the root linked, so that push synced the root.
- Options:
  - (a) Keep the root link and commit the built files at the repository root, next to the docs.
  - (b) Point the link at a dedicated tracked folder, `deploy/`, that holds only a staged build (`main.js` and `rustedscreep_wasm.wasm`).
  - (c) Unlink and deploy with the repo's tool only (ADR-011). The user declined this.
- Decision: deploy through the GitHub link (the user's decision, quoted above), with (b) as the folder layout, which the user confirmed: "we plan to use a deploy folder for the bot script". The user re-points the link's Folder from the root to `deploy` (user action, in the order given below).
- Consequences:
  - Any write that reaches GitHub `main` is a live MMO deploy. Claude never commits or pushes (project rule). guard-bash.mjs (hardened 2026-11-13 with the user's OK) blocks git history and publish commands, GitHub writes (`gh api` with a write method or fields, `gh pr merge`, `gh repo sync`, `gh workflow run`), and quoted, wrapped, nested-shell and `eval` forms. `node .claude/hooks/test-guard-bash.mjs` checks 60 block and 32 allow cases. settings.json asks before `npm run stage` and any `gh api`. It is a guard, not a sandbox.
  - `deploy/` is tracked (not git-ignored) and holds only a staged build that passed `/check`. `npm run stage` (P1) runs the release build and copies `main.js` and the `.wasm` into `deploy/`. It reports sizes, a byte hash of the `.wasm` and a build ID, and with `--out <dir>` it writes elsewhere for tests and dry runs. `/deploy mmo` runs `/check` and then the stage step, and Claude runs it only when the user names `mmo` in the current message.
  - Order for the first deploy: (1) push a commit that contains a checked `deploy/`; that push still syncs the root. (2) Then re-point the link to `deploy` and press Sync. Sync overwrites the account's code at once with what `deploy/` holds on GitHub (V75 warns "All your scripts in Screeps account will be overwritten"). Re-pointing before `deploy/` exists on GitHub would sync an empty or missing folder.
  - Until it is measured (V77), assume every push reloads the code: a global reset plus a full wasm compile, which costs bucket even when `deploy/` did not change. Batch pushes, and avoid pushing during a measurement window.
  - To verify in P1 with the first checked build (V77): whether `.wasm` arrives byte-for-byte intact, whether modules missing from `deploy/` are deleted, the delay from push to first run, and whether an identical re-sync reloads the code.
  - Fallback if `.wasm` cannot go through the link: the user first unlinks the integration, or points it at a folder that is never pushed. Then `/deploy mmo` uploads through the API without staging (`npm run deploy -- --server mmo`, which needs Node 22 or 24, V11). With the link still active, the next push would overwrite the API upload.
  - Steam client mirror (V74): when the sync changes the server code, an open client offers to replace its local mirror. Decline. In either case, never edit files in the mirror's branch folders, because any local change there uploads that whole branch at once and would overwrite the pushed build. Claude never writes, copies or links anything into the mirror.
  - Every staged build commits a new `.wasm` to the repository. Its size is recorded in STATUS at each stage.
- Evidence: the user's statements on 2026-11-13 (quoted above, and that every push to `main` triggers the sync); V74 (Steam mirror behavior, from the client code); V75 (GitHub integration dialog and warning, from the client code; sync semantics from community reports); V77 (what is still unverified); the repository has no top-level `.js` files and `dist/` is git-ignored (git ls-files, .gitignore).
