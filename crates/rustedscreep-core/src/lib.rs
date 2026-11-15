//! Pure, deterministic decision logic: scheduler, planner, economy, spawning, logistics, defense,
//! offense, expansion, movement, market policy, profiling math and util. No JS dependencies; game
//! constants and pure local types come in through `rustedscreep_consts`.

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

pub mod config;
pub mod consts_check;
pub mod intents;
pub mod movement;
pub mod persist;
pub mod profiler;
pub mod scheduler;
pub mod snapshot;
