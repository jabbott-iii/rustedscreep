//! The decision logic of rustedscreep, a bot for Screeps: World: what every creep, spawn, and tower
//! does each tick (one game turn), in pure, deterministic Rust that never calls the game.
//!
//! Each tick runs snapshot → decide → act. `rustedscreep-wasm` copies the game's objects into a
//! [`snapshot::Snapshot`], this crate decides, and `rustedscreep-wasm` issues the decisions as
//! intents (the commands, such as a move or a harvest, that the game applies once the tick's code
//! has run). With no JS dependency, the crate builds and tests natively, and the same snapshot and
//! state always give the same decisions. Game constants and pure local types come in through
//! `rustedscreep_consts`. The design is in
//! [`docs/ARCHITECTURE.md`](https://github.com/jabbott-iii/rustedscreep/blob/main/docs/ARCHITECTURE.md),
//! and every threshold's default, unit, and safe range in
//! [`docs/TUNING.md`](https://github.com/jabbott-iii/rustedscreep/blob/main/docs/TUNING.md).
//!
//! # Contents
//!
//! - [`scheduler`]: the three CPU tiers (Critical, Standard, and Optional, chosen by the bucket, the
//!   CPU the game banks from earlier ticks), the per-tick budget, and time-sliced jobs.
//! - [`snapshot`]: the tick's tables, one array per field, with the objects named by numeric
//!   handles.
//! - [`intents`]: the act layer's pure half: the engine's table of which intents block which, the
//!   intent buffer, and the outcomes the game returns.
//! - [`movement`]: cost grids, flow fields (every tile's cost to one goal, shared by the creeps going
//!   there), cached detours, traffic, and routes between rooms.
//! - [`spawning`]: the spawn queue, creep bodies and names, and pre-spawn replacement.
//! - [`economy`]: the opening from room controller level (RCL) 1 to 3, static miners, links,
//!   haulers, upgraders, repairs, the body solver, and remote mining.
//! - [`planner`]: the base planner: the room's layout, its checks, and site placement.
//! - [`defense`]: the threat assessment of hostile creeps and the towers' fire and heals.
//! - [`signing`]: the controller signer.
//! - [`expansion`]: scouting for expansion: the atlas of the rooms near the homes, the explorer,
//!   and the observers.
//! - [`persist`]: the binary formats and the manager of the RawMemory segments (strings the game
//!   keeps between ticks) that survive a global reset.
//! - [`config`]: every threshold, the server profiles, and the runtime overrides.
//! - [`consts_check`]: the constants self-check against the server's globals.
//! - [`profiler`]: the profiler's math.

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
pub mod defense;
pub mod economy;
pub mod expansion;
pub mod intents;
pub mod movement;
pub mod persist;
pub mod planner;
pub mod profiler;
pub mod scheduler;
pub mod signing;
pub mod snapshot;
pub mod spawning;

// The README's examples are compiled as doc tests too.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
