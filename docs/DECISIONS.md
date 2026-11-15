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

## ADR-004 Persistence encoding: Accepted (2026-11-14)
- Context: RawMemory segments are strings. Persist only data that must survive a reset, in a compact, versioned form, written only when dirty.
- Options: (a) base64 of a binary layout, (b) a denser UTF-16 packing scheme, (c) a hand-rolled varint layout with either of those.
- Decide by: bytes per segment, encode and decode CPU (bench plus in-game), and robustness against corruption (checksum).
- Evidence (VERIFICATION_LOG sections F and G):
  - RawMemory has 100 segments (ids 0-99), at most 10 active at once, and 100 KB per segment. Requested segments appear on the next tick, and the driver rejects oversize writes at end of tick (V41). Memory itself is capped at 2 MB (V40).
  - The segment and Memory limits are checks on JS string `.length`, so they count UTF-16 code units, not bytes (V40, V41, read verbatim in the driver).
  - Every segment present in `RawMemory.segments` at end of tick is re-saved and counts toward the 10-segment cap. Exceeding the cap throws after the loop and likely loses the tick (V41, inferred; V119), so the segment manager must unload segments it no longer needs.
  - Capacity per segment: base64 carries 76797 bytes in 102400 units (76781 of payload after the 16-byte header); a 15-bit packing carries 192000.
  - The string crossing decides the CPU, not the encoding's density (V123). The wasm-bindgen glue copies ASCII text into wasm in a JS loop and sends everything else through the TextEncoder/TextDecoder polyfill (the MMO has neither, V72), which converts a 15-bit unit to 3 UTF-8 bytes per code point in JS. Measured locally on one full segment (Node 18.19.1, a copy of the glue's loop and the repo's polyfill, the codec compiled to wasm; relative numbers only, the MMO runs a newer Node):

    | Path | Write (encode + CRC-32 + crossing) | Read (crossing + decode + CRC-32) |
    |---|---|---|
    | (a) base64, stock glue | 124 + 196 + 632 µs = 12.7 µs/KiB | 134 + 121 + 196 µs = 6.0 µs/KiB |
    | (b) 15-bit, stock glue | 617 + 429 + 1577 µs = 14.0 µs/KiB | 706 + 755 + 429 µs = 10.1 µs/KiB |
    | (b) 15-bit, inline-JS crossing (a build change) | 617 + 429 + 388 µs = 7.6 µs/KiB | 135 + 755 + 429 µs = 7.0 µs/KiB |

    CRC-32 costs about 2.3 to 2.6 µs/KiB in wasm (Adler-32 0.5, FNV-1a 1.0, both weaker on short records). At the volumes the bot will persist (a few KB per write), the difference between the paths is well under 0.01 CPU per write.
  - Storage fidelity: the driver stores the string as given (V121); the open-source storage path is JSON over UTF-8 (inferred lossless on Node 12 or later); the MMO's storage is closed source, so whether non-ASCII code units survive there is unverified (V121). A bulk UTF-16 crossing needs inline JS (V125).
  - In the sandbox the plain build's self-test (`node js/tools/persist-sim.mjs`, the bundle in a Node vm context with the polyfill) measured a full segment at about 0.33 to 0.6 ms to encode, 0.23 to 0.29 ms to read and 0.46 to 0.7 ms to decode and compare. The JS write took 8 to 13 ms on the first large crossing of a global (the polyfill's decoder is not optimized yet) and 0.8 to 1.9 ms on later runs; the cold read took 2.4 to 3.1 ms (2026-11-14).
- Recommendation: (a) with CRC-32. Through the stock glue it is cheaper than (b) per payload byte in both directions, its ASCII text survives any storage, and it needs no custom JS. (b) is faster only with an inline-JS crossing, which needs a build change, and it is safe only after a live round trip proves the MMO keeps every code unit; neither is worth it at the expected volumes. A tag in the frame names the encoding, so (b) can be added later without breaking stored data.
- Decision: (a) base64 with CRC-32, as recommended. The user chose "base64 + CRC-32 (Recommended)" on 2026-11-14 (P1.9).
- Consequences:
  - Frame (container format 1, `rustedscreep_core::persist::frame`): `"rs1b"` (magic `rs`, version `1`, encoding `b`), then base64 (RFC 4648 section 4 alphabet, no padding, strict decoding) of a 16-byte little-endian header (owner kind u16, schema u16, write tick u32, payload length u32, CRC-32/ISO-HDLC u32 over the first 12 header bytes and the payload) followed by the payload. Every decode failure (empty, foreign, unknown version or encoding, bad base64, short, length or CRC mismatch, another owner's kind or schema) is an error the owner answers by rebuilding from game state.
  - The CRC is the one the loader and tools already use (js/src/crc32.ts, js/tools/lib/crc32.mjs), so a JS tool can check a frame.
  - Payload layouts belong to their owners and are versioned by `schema`; option (c), a shared varint writer, waits for the first owner that needs one.
  - The 15-bit option stays open: a new encoding letter, the inline-JS crossing, and a live round trip of every code unit on the MMO first.
  - The live round trip on the MMO (the P1 gate) is `rustedscreep.persistTest()` (TEST_PLAN §8). It passed on shard3 on 2026-11-14: a full segment (76781 bytes in 102400 units) written and read back intact, at 0.79 CPU to encode, 5.77 to write (cold), 0.31 to read and 1.14 to decode (V144).

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
  - rustedscreep-wasm gets log, fern and console_error_panic_hook. (fern was dropped by ADR-014 on 2026-11-14: rustedscreep-wasm has its own minimal logger.)
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

## ADR-012 Deploy path to the official MMO through the GitHub link: Accepted (2026-11-13; supersedes ADR-011; the wasm's form in `deploy/` amended by ADR-015)
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
  - Until it is measured (V77), assume every push reloads the code: a global reset plus a full wasm compile, which costs bucket even when `deploy/` did not change. Batch pushes, and avoid pushing during a measurement window. Confirmed on 2026-11-14 (V145): a docs-only push reloaded the code, a sync deletes every module missing from `deploy/`, and the code runs within about 10 s of the commit.
  - To verify in P1 with the first checked build (V77): whether `.wasm` arrives byte-for-byte intact, whether modules missing from `deploy/` are deleted, the delay from push to first run, and whether an identical re-sync reloads the code.
  - Fallback if `.wasm` cannot go through the link: the user first unlinks the integration, or points it at a folder that is never pushed. Then `/deploy mmo` uploads through the API without staging (`npm run deploy -- --server mmo`, which needs Node 22 or 24, V11). With the link still active, the next push would overwrite the API upload.
  - Steam client mirror (V74): when the sync changes the server code, an open client offers to replace its local mirror. Decline. In either case, never edit files in the mirror's branch folders, because any local change there uploads that whole branch at once and would overwrite the pushed build. Claude never writes, copies or links anything into the mirror.
  - Every staged build commits a new `.wasm` to the repository. Its size is recorded in STATUS at each stage.
- Evidence: the user's statements on 2026-11-13 (quoted above, and that every push to `main` triggers the sync); V74 (Steam mirror behavior, from the client code); V75 (GitHub integration dialog and warning, from the client code; sync semantics from community reports); V77 (what is still unverified); the repository has no top-level `.js` files and `dist/` is git-ignored (git ls-files, .gitignore).

## ADR-013 When Critical work stops: Accepted (2026-11-14)
- Context: SPEC §3 and CLAUDE.md say Critical (spawning, defense, harvesting) "always runs". The scheduler (ROADMAP P1 module 6) also has to keep the tick below the engine's kill. The kill is a wall-clock timeout at tickLimit + 5 ms, at least 30 ms (VERIFICATION_LOG V105), while the clock the scheduler can read, `Game.cpu.getUsed()`, is isolate CPU time plus 0.2 per intent (V106). Running past tickLimit costs in two ways. A killed tick is an aborted tick, which the loader recovers from with a global reset and a recompile (ADR-003). And while the bucket is below 500 − limit, tickLimit is bucket + limit, so a tick that runs past it without being killed is billed into a negative bucket and the next tick is skipped (V37). There is also an unverified user report of negative `getUsed` readings on the MMO (V106).
- Options:
  - (a) Critical never stops: it runs every unit every tick, whatever the clock says.
  - (b) Critical stops only at the ceiling (tickLimit minus `tick_reserve_pct`), which it shares with the other tiers, and it runs first, so the others get only what it leaves. With a clock that has given `bad_samples_to_break` (2) bad readings in a tick, every tier stops, Critical included, for the rest of that tick.
  - (c) Like (b), but on a broken clock Critical goes on with an estimated clock: the last good reading plus the units run times a conservative cost per unit.
- Decision: (b), as recommended; the user accepted it ("Accept ADR-013", 2026-11-14). P1.6 implements it. Critical runs first and takes what it needs up to the ceiling, which is 90% of tickLimit; at 450 of 500 CPU that is far more than a colony's Critical work needs. It stops only where continuing risks a reset or a skipped tick, which would cost more Critical work (the whole aborted tick and its recovery) than the units it skips. (a) cannot guard against either outcome. (c) keeps Critical running on a clock that has stopped working, with an estimate that is only as good as the last measured cost per unit, so it can still overrun. It is the option to revisit if live runs show bad readings are real and frequent. The other tiers' bucket gates never apply to Critical.
- Consequences: on a tick that reaches the ceiling, Critical jobs that run after it are skipped for that tick. Round-robin cursors start the next tick with the units that were skipped. `rustedscreep.scheduler()` shows budget stops and a broken clock, and TickStats counts tier-order breaches. `bad_samples_to_break` is a config field (TUNING.md), so the threshold can be raised without a code change.
- Evidence: V37, V105 and V106 (driver cf63d8a, engine a90d26f; researcher and skeptic). The P1.6 tests pin the behavior: `the_ceiling_keeps_the_reserve`, `an_empty_bucket_still_runs_critical`, `bad_clock_readings`, `the_break_threshold_is_configurable`, `a_cost_jump_stays_inside_the_reserve`.

## ADR-014 Log backend: fern or a minimal logger: Accepted (2026-11-14)
- Context: SPEC §1 lists `log`/`fern`, and ADR-006 accepted fern 0.7.1 for rustedscreep-wasm. P1.10 added it as specified: one `fern::Dispatch` at level Trace whose only output is `Output::call` to `console.log`, and the level from config through `log::set_max_level` (V126, V127). The bot uses nothing else fern offers (formatting closures, per-module levels, chaining, file, stream or syslog outputs). Wasm size is a cost every global pays: the loader compiles the whole module on each boot (ADR-003), and the boot gate is set from the measured cold start (P1 gate).
- Options:
  - (a) Keep fern, as SPEC and ADR-006 say.
  - (b) Replace fern with a minimal `log::Log` in `logging.rs`: `enabled` returns true and `log` prints `record.args()` with `console.log` (`as_str` first, so a message without arguments does not allocate), installed with `log::set_logger` on a static. Behavior is the same: `log` macros, the same level gate (`log::set_max_level`) and the same lines. `log` stays, and fern leaves Cargo.toml and Cargo.lock.
- Evidence (local, 2026-11-14, `npm run build`, same tree but `logging.rs`): release wasm 170534 bytes with fern and 123394 with (b), so fern costs 47140 bytes, 28% of the module (V126). Before P1.10 the plain build was 89000 bytes (STATUS). The MMO cold-start cost of either size is not measured yet (P1 gate).
- Recommendation: (b), since it removes 47 KB of wasm with no change in behavior. It deviates from SPEC §1's dependency list, which is your call. If accepted: VERIFICATION_LOG section C gets a discrepancy row, and ADR-006's consequences say fern was dropped.
- Decision: (b), as recommended; the user chose it on 2026-11-14 ("Yes, drop fern"), after P1.10 was committed with fern (83f1254).
- Consequences:
  - fern leaves the root `[workspace.dependencies]`, rustedscreep-wasm's Cargo.toml and Cargo.lock. `log` 0.4.35 stays, now with no features at all: `log::set_logger` on a static needs none (V128).
  - rustedscreep-wasm `logging.rs` holds `Console`, a `log::Log` whose `log` prints the message with `console.log`, installed once per instance from the start function; the level is `log::set_max_level` as before, and every log line reads as before.
  - Release wasm of the plain build: 170756 bytes at 83f1254, 123705 after (`npm run build`, 2026-11-14), so 47051 bytes less (V128).
  - VERIFICATION_LOG D8 records the deviation from SPEC §1; ADR-006's consequences note it.

## ADR-015 Wasm delivery through the GitHub link: a base64 text module: Accepted (2026-11-14; amends ADR-012)
- Context: the first live deploy through the link (ADR-012) showed that the sync uploads every file of the linked folder as a text module named after the file: `main.js` became `main`, and `rustedscreep_wasm.wasm` became a text module `rustedscreep_wasm.wasm`. The loader's `require('rustedscreep_wasm')` failed with "Unknown module", and requiring the `.wasm` name makes the engine evaluate the wasm bytes as JavaScript (V136, V137; engine game.js:551-594). No binary file can reach the account intact through the link, and the user keeps the link: "I am leaving it linked because what we make will be live tested with the link" (ADR-012).
- Options:
  - (a) Keep the link and ship the wasm as base64 text in a JavaScript module, `deploy/rustedscreep_wasm.js` = `module.exports = "<base64>";`, which the sync turns into module `rustedscreep_wasm`; the loader decodes the string to bytes before its CRC-32 check and compile.
  - (b) ADR-012's fallback: unlink the integration and upload a `{binary}` module through the API (`npm run deploy -- --server mmo`, which needs Node 22 or 24 and is not written yet).
- Evidence (local, 2026-11-14; the design research, re-measured in the repo's sandbox):
  - Decoding 230513 bytes with a plain-JS decoder (`atob`, `Buffer` and `TextDecoder` absent, as assumed for the MMO, V95, V139): 5.4 to 6.3 ms cold on Node 18.19.1, 1.8 ms on Node 26.13.1. Evaluating the 307373-byte text module: 1.1 to 1.3 ms. End to end in the boot's load step (load + CRC), the smoke sandbox measured 3.1 ms with a binary module and 8.0 ms with the text module on Node 18 (the research: +6.1 ms on Node 18, +3.0 ms on Node 26). The cost is paid once per global boot (the engine caches a module per global); compile, instantiate and the tick are unchanged, because the decoded bytes are the same wasm (its CRC-32 and sha256 equal the build's).
  - Upload budget (V76): the text-module upload is 13 characters longer than the binary one, since a binary module is stored as base64 too (375913 against 375900 of 2097152 characters at this build). Heap: about 0.36 MB more for the global's lifetime (the module source and the string, kept by the module cache).
  - Alternatives measured and rejected: the base64 inside main.js (+0.8 ms to compile main.js on every global, even when the boot is deferred); a separate decode tick (one idle tick more per global reset, to save 3 to 6 ms of bucket); `atob` when present (never probed on the MMO); other encodings (hex twice the size; base85 or UTF-16 schemes add decoder complexity or Unicode hazards through the sync).
- Decision: (a). The user chose "option 1" on 2026-11-14, after asking whether it keeps the benefits of Rust: it does, since only the transport changes; the decoded bytes are the build's wasm, and the loader compares their CRC-32 with the build's (on a mismatch it logs `MISMATCH` and notifies once, then boots anyway, as before ADR-015; refusing to boot on a mismatch would be a separate loader decision).
- Consequences:
  - `deploy/` holds `main.js` and `rustedscreep_wasm.js` (the `.wasm` as one ASCII line of padded standard base64). `npm run stage` builds into its own `target/stage-dist`, checks both files' sha256 against the build and that the text module decodes to the build's `.wasm` before writing anything, copies them and checks the copies, then removes a `rustedscreep_wasm.wasm` an earlier stage left and checks that the folder holds only the staged files. It reports the sha256 and CRC-32 of the decoded bytes; the boot log compares the CRC-32.
  - The loader's `toBytes` accepts a binary module's view or ArrayBuffer (an API upload, a private server, the sandbox) or a base64 string, decoded by `js/src/base64.ts` (strict: a bad length, character or padding is a load incident, never compiled). The CRC-32 check runs over the decoded bytes. The boot line shows the form that ran (`wasm String(base64, N chars)`) and now also reports `typeof atob` and `Buffer` (V139).
  - The build writes `dist/rustedscreep_wasm.js` and runs the smoke test with both forms; the sandbox evaluates the text module in the driver's module wrapper, once per global (V138). `npm run size` counts both upload shapes and checks the larger. The bundle check bans `atob`, `btoa` and `Buffer` in main.js (V95's safe default).
  - The decode runs in the boot's existing load step. If a live `load+crc` figure ever crowds that tick, a separate decode phase can be added then, from the measurement.
  - If the link ever delivers binary files, the loader still accepts them; `deploy/` must then hold only one of the two forms, since both map to one module name.
  - The ADR-012 fallback (b) stays documented; the deploy tool, when written, uploads the binary form.
  - The loader's backoff record (`Memory.rustedscreepLoader`) carries a failed deploy's incident streak into the next build, which then waits up to `backoffMaxTicks` (3200) before booting and keeps the streak after it boots. After a push that follows a failed build, once `rustedscreep.status()` shows the new build ID, run `delete Memory.rustedscreepLoader` in the console (run earlier, the old build rebuilds the streak). Resetting the streak when the build ID changes would remove this step; it changes the persisted record and the backoff across pushes, so it is the user's decision.
  - The `rustedscreep_wasm.wasm` text module the first sync left on the account is not removed by the bot. Whether a later sync deletes it is unverified (V77); otherwise the user deletes it once in the in-game Script editor, since it counts toward the 2 MB code limit.
  - Each staged build commits a 307 KB text file (base64 deltas poorly in git), about 0.3 MB of repository growth per changed wasm.

## ADR-016 The profiler's live check and the CPU floor move to the P2 gate: Accepted (2026-11-14)
- Context: the P1 gate asks for "Empty-bot CPU per tick ... measured on the MMO", which is the profiler build's live check (P1.8, TEST_PLAN §8): two 1000-loop windows of a `profiler` build in segment 99, about 2000 ticks. The P1 bundle has no economy, so the user's RCL1 room E4N59 is never upgraded and is lost about 20000 ticks after its last upgrade (V71), whatever build runs. The profiler build was live from tick 84276781 (V147), and the user asked to move this test past the next phase so as not to lose room control.
- Options:
  - (a) Finish the two windows now (about 2 hours) and keep the P1 gate as written.
  - (b) Move the profiler build's live check and the CPU floor to the P2 gate, measured with the P2 build.
- Decision: (b), the user's decision on 2026-11-14: "move the profiler build test to p2 gate".
- Consequences:
  - The P1 gate no longer includes the empty-bot CPU floor. The rest of it stands: `/check` green, the wasm loads (done, V140), 10 forced traps recover (a `trap-test` build), cold-start cost measured and the boot gate set from it (three samples so far, V146), the persistence round trip natively and once live (done, V144), sizes recorded, and the GitHub-link path verified (done, V145).
  - The P2 gate gains the profiler build's live check: the cold-loop line, a growing window with good readings, and at least two closed windows in segment 99 (matched to the boot's `Game.time`). With P2's jobs running there is no empty bot, so the floor is reported from the profiler's series as the fixed per-tick cost outside the jobs: `start`, the `snapshot` step and the act step's per-tick overhead (each less its own reading, about 0.0024 CPU per reading, V147), from a window after the first (which holds the once-per-global constants self-check). It is recorded beside P2's CPU per tick and the naive baseline.
  - The profiler build now live gives nothing the P1 gate needs; the next deploy replaces it (a `trap-test` build for the P1 gate's recovery test, or a plain build). Its windows, if read before then, are kept as extra P1 data, not as the P2 evidence.
  - TEST_PLAN §8 and STATUS say where the check now belongs.

