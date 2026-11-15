//! Economy (SPEC §3.1, §3.2). So far the RCL1–3 opening (ROADMAP P2.4): generalist workers sized
//! to what each source yields over a worker's round, zero-creep recovery, and the deadlock
//! watchdogs. P3 adds static miners, links, haulers and upgraders, and replaces the opening.
//!
//! - [`sizing`]: a worker's round between a source and the controller, and the workers a source
//!   keeps busy.
//! - [`Opening`]: the per-room plan, the spawn requests, each worker's mode and task, and the
//!   intents and moves those tasks need.
//! - [`Watchdog`]: recovery, the downgrade guard, the spawn wait fallback and stall reports.
//! - [`placement`]: the interim extension sites (ADR-020) until the P4 base planner.

mod opening;
pub mod placement;
pub mod sizing;
mod watchdog;

pub use opening::{
    Opening, PRIORITY_LOW, PRIORITY_NORMAL, PRIORITY_RECOVERY, Plan, RoomStats, SourcePlan, Task,
};
pub use watchdog::{Event, Observation, Watchdog};

#[cfg(test)]
mod tests;
