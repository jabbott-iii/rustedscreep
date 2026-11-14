# Architecture decision records

Format for each ADR: number, title, status (Proposed, Accepted, or Superseded by ADR-N), date, context, options, decision, consequences, evidence.
Claude proposes ADRs and the user accepts them. Numbers are never reused.

---

## ADR-001 Build toolchain: Proposed (P0)
- Context: SPEC §1 names wasm-pack (or cargo-screeps). cargo-screeps doesn't work with screeps-game-api ≥0.22, and the rustwasm org is archived (VERIFICATION_LOG S3, S8).
- Options:
  - (a) wasm-pack `build --target <verified>` followed by wasm-opt, which is what the starter uses.
  - (b) `cargo build --target wasm32-unknown-unknown`, then the wasm-bindgen CLI pinned to the crate version, then wasm-opt. This is more explicit and drops a tool dependency.
- Decide by: reproducibility, version-match enforcement, CI simplicity, and output parity with the loader.
- Decision / evidence: _pending_

## ADR-002 Wasm feature target and Rust toolchain/edition: Proposed (P0)
- Context: Rust 1.82 and later emit reference-types and multivalue, and Rust 1.87 and later add bulk-memory (S6). The server's Node may reject these, and the starter documents `--signext-lowering` (S4).
- Options:
  - (a) Stable toolchain pinned to the newest version whose default features the probe accepts.
  - (b) Nightly with `-Ctarget-cpu=mvp` and `-Zbuild-std=panic_abort,std`, plus selected `+features`.
  - (c) Latest stable plus binaryen lowering passes, if they cover everything.
- Edition follows the toolchain (2024 needs a recent stable).
- Decide by: feature probe results (VERIFICATION_LOG §D), build reproducibility, and runtime CPU of the resulting wasm (measure).
- Decision / evidence: _pending_

## ADR-003 Trap and abort recovery: Proposed (P0, measured in P1)
- Context: SPEC §1 asks to re-instantiate on the next tick and never brick the bot. The starter uses `Game.cpu.halt()` after an aborted tick because of wasm-bindgen #3130 (S5).
- Options:
  - (a) `Game.cpu.halt()`. This is simple and proven, but costs a full global reset plus recompile.
  - (b) Discard the instance and the glue module (clear the require cache), then instantiate a fresh one from the cached `WebAssembly.Module`. This is cheaper if it is sound.
  - (c) A hybrid: try (b), and fall back to (a) after repeated failures.
- Decide by: P1 `trap-test` results (time to recover, CPU, correctness over 10 consecutive traps) and whether `halt` is available per server.
- Decision / evidence: _pending_

## ADR-004 Persistence encoding: Proposed (P1)
- Context: RawMemory segments are strings. Persist only data that must survive a reset, in a compact, versioned form, written only when dirty.
- Options: (a) base64 of a binary layout, (b) a denser UTF-16 packing scheme, (c) a hand-rolled varint layout with either of those.
- Decide by: bytes per segment, encode and decode CPU (bench plus in-game), and robustness against corruption (checksum).
- Decision / evidence: _pending_

## ADR-005 Constants source for native tests: Proposed (P0)
- Context: pure crates must test natively, and constants should come from the API (SPEC §0).
- Options:
  - (a) `rustedscreep-consts` re-exports `screeps-game-api` constants and local types, which requires the crate to build and run natively.
  - (b) `rustedscreep-consts` mirrors the values with sources, and the in-game self-check catches drift.
- Decide by: R4 (native build of screeps-game-api).
- Decision / evidence: _pending_

## ADR-006 Dependencies: Proposed (P0)
- Candidates: `rustc-hash` (Fx), `smallvec` or `arrayvec`, `criterion` (dev), `proptest` (dev), `log` and `fern`, and `console_error_panic_hook`.
  Record the version and reason for each, and reject anything that pulls in JS crates or serde in the tick path.
- Decision / evidence: _pending_

## ADR-007 Base planner approach: Proposed (decided in P4)
- Options: (a) stamp or bunker layouts, (b) distance transform with flood fill and min-cut ramparts, (c) a hybrid.
- Decide by: corpus validity, rampart count, path lengths, planner CPU (criterion), and time-sliced in-game cost.
- Decision / evidence: _pending_

## ADR-008 Loader language and transpile target: Proposed (P0)
- Context: SPEC asks for a TypeScript loader. The starter says Babel is required for the older Node on official servers (S3).
- Options: (a) TS → Babel preset-env targeting the probed JS level, then Rollup. (b) TS with `tsc` targeting ES5 or ES2015, then Rollup.
- Decide by: probe syntax results, bundle size, and source-map usefulness.
- Decision / evidence: _pending_

## ADR-009 Crate names: Accepted (2026-11-13)
- Context: the kit shipped with placeholder crate names `game-consts`, `bot-core` and `bot-wasm`. The repository is named `rustedscreep`. CLAUDE.md treats the crate layout as an architecture invariant, and KIT_README says a rename must update CLAUDE.md, the rule `paths` and `guard-purity.mjs` together.
- Options:
  - (a) Prefix all three: `rustedscreep-consts`, `rustedscreep-core`, `rustedscreep-wasm`.
  - (b) As (a), but name the cdylib `rustedscreep`.
  - (c) Rename only the `bot-*` crates and keep `game-consts`.
- Decision: (a), chosen by the user. Each crate's directory matches its name (`crates/rustedscreep-consts`, `crates/rustedscreep-core`, `crates/rustedscreep-wasm`), and Rust paths use `rustedscreep_consts::`, `rustedscreep_core::` and `rustedscreep_wasm::`. Roles, boundaries and purity rules are unchanged.
- Consequences: the wasm-bindgen output files are expected to be named after the cdylib (`rustedscreep_wasm.js`, `rustedscreep_wasm_bg.wasm`). This is an assumption until it is verified against the ADR-001 toolchain in P1. Formula doc comments read `/// Uses: rustedscreep_consts::X`. No crates existed yet, so nothing was migrated.
- Evidence: the user's request on 2026-11-13. Every reference in CLAUDE.md, KIT_README.md, `.claude/` (rules, skills, agents, `guard-purity.mjs`) and `docs/` was renamed. Code-location cells in `VERIFICATION_LOG.md` were repointed without changing any finding. `SPEC.md` names no crates and was not edited.
