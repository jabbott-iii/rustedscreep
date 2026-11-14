//! Constants facade (ADR-005). Besides `rustedscreep-wasm`, this is the only crate that may name the
//! `screeps` crate, and only for constants and pure local types. Values the API does not export go in
//! `extra.rs` with a source URL and a "last verified" date (P1 module 2).

// No-panic rule for non-test code (.claude/rules/core-logic.md; lint names: VERIFICATION_LOG V83).
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
