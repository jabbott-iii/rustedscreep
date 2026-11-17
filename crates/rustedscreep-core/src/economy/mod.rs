//! Economy (SPEC §3.1, §3.2). The RCL1–3 opening (ROADMAP P2.4): generalist workers sized to what
//! each source yields over a worker's round, zero-creep recovery, and the deadlock watchdogs; static
//! miners (P3.1), the link network (P3.2), haulers (P3.3), and the workers' container repairs
//! (P3.5). P3 goes on with upgraders, and replaces the opening.
//!
//! - [`sizing`]: a worker's round between a source and the controller, the workers a source keeps
//!   busy, and a static miner's body.
//! - [`Opening`]: the per-room plan, the spawn requests, each worker's mode and task, and the
//!   intents and moves those tasks need; the workers build the room's sites and repair its
//!   containers (P3.5, [`needs_repair`]).
//! - [`Mining`]: each source's mining tile, its miner's spawn request, transfer, harvest and move,
//!   the sources the workers leave to it, the link tiles, and the link sends.
//! - [`Hauling`]: haulers (P3.3, ADR-025): each mined source's haulers, sized from its yield and
//!   trip, their spawn requests, and their takes at the mining tile, fills and transfers into the
//!   controller container; the opening reads [`HaulView`].
//! - [`Watchdog`]: recovery, the downgrade guard, the spawn wait fallback and stall reports.
//! - [`placement`]: the interim extension sites (ADR-020), source containers (ADR-023), links
//!   (ADR-024) and the controller container (ADR-025) until the P4 base planner.

mod hauling;
mod mining;
mod opening;
pub mod placement;
pub mod sizing;
mod watchdog;

pub use hauling::{
    HaulInput, HaulPlan, HaulSource, HaulStats, HaulTask, HaulView, Hauling, PRIORITY_HAULER,
};
pub use mining::{
    CONTROLLER_LINK_RANGE, LINK_MARGIN, LinkOrder, LinkTotals, MinePlan, MineSource, MineStats,
    Mined, Mining, PRIORITY_MINER, RECEIVER_RANGE, Receiver, link_loss,
};

pub use opening::{
    Opening, PRIORITY_LOW, PRIORITY_NORMAL, PRIORITY_RECOVERY, Plan, RoomStats, SourcePlan, Task,
    below_share, needs_repair,
};
pub use watchdog::{Event, Observation, Watchdog};

#[cfg(test)]
mod tests;
