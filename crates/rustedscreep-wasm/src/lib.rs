//! WASM boundary: snapshot (JS → Rust), act (Rust → intents), persistence (RawMemory), the profiler
//! and the exported `loop`. The only crate that uses `wasm-bindgen`, `js-sys` or game-object APIs.

// No-panic rule for non-test code (.claude/rules/wasm-bot.md; lint names: VERIFICATION_LOG V83).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::todo,
        clippy::unimplemented,
        clippy::unreachable
    )
)]
