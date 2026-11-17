//! Economy (SPEC §3.1, §3.2). The RCL1–3 opening (ROADMAP P2.4): generalist workers sized to what
//! each source yields over a worker's round, zero-creep recovery, and the deadlock watchdogs; and
//! static miners (P3.1). P3 goes on with links, haulers and upgraders, and replaces the opening.
//!
//! - [`sizing`]: a worker's round between a source and the controller, the workers a source keeps
//!   busy, and a static miner's body.
//! - [`Opening`]: the per-room plan, the spawn requests, each worker's mode and task, and the
//!   intents and moves those tasks need.
//! - [`Mining`]: each source's mining tile, its miner's spawn request, harvest and move, and the
//!   sources the workers leave to it.
//! - [`Watchdog`]: recovery, the downgrade guard, the spawn wait fallback and stall reports.
//! - [`placement`]: the interim extension sites (ADR-020) and source containers (ADR-023) until
//!   the P4 base planner.

mod mining;
mod opening;
pub mod placement;
pub mod sizing;
mod watchdog;

pub use mining::{MinePlan, MineSource, MineStats, Mined, Mining, PRIORITY_MINER};

pub use opening::{
    Opening, PRIORITY_LOW, PRIORITY_NORMAL, PRIORITY_RECOVERY, Plan, RoomStats, SourcePlan, Task,
};
pub use watchdog::{Event, Observation, Watchdog};

#[cfg(test)]
mod tests;
