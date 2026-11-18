//! Economy (SPEC §3.1, §3.2). The RCL1–3 opening (ROADMAP P2.4): generalist workers sized to what
//! each source yields over a worker's round, zero-creep recovery, and the deadlock watchdogs; static
//! miners (P3.1), the link network (P3.2), haulers (P3.3), upgraders (P3.4), and the workers'
//! container repairs (P3.5). P3 goes on with the body solver, and replaces the opening.
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
//! - [`Upgrading`]: upgraders (P3.4): WORK-heavy creeps at posts next to the controller container
//!   and controller links, sized to the energy that reaches them less every other creep's body;
//!   the opening reads [`UpgradeView`].
//! - [`Watchdog`]: recovery, the downgrade guard, the spawn wait fallback and stall reports.
//! - [`placement`]: the interim extension sites (ADR-020), source containers (ADR-023), links
//!   (ADR-024) and the controller container (ADR-025) until the P4 base planner.

mod hauling;
mod mining;
mod opening;
pub mod placement;
pub mod sizing;
mod upgrading;
mod watchdog;

pub use hauling::{
    HaulInput, HaulPlan, HaulSource, HaulStats, HaulTask, HaulView, Hauling, PRIORITY_HAULER,
};
pub use mining::{
    CONTROLLER_LINK_RANGE, LIFE_CYCLES, LINK_MARGIN, LinkOrder, LinkTotals, MinePlan, MineSource,
    MineStats, Mined, Mining, PRIORITY_MINER, RECEIVER_RANGE, Receiver, SourceHarvest, link_loss,
    permille,
};

pub use opening::{
    Opening, PRIORITY_LOW, PRIORITY_NORMAL, PRIORITY_RECOVERY, Plan, RoomStats, SourcePlan, Task,
    below_share, needs_repair,
};
pub use upgrading::{
    Crew, PRIORITY_UPGRADER, UpgradePlan, UpgradeStats, UpgradeTask, UpgradeView, Upgrading, Ways,
    delivered, delivering, upkeep,
};
pub use watchdog::{Event, Observation, Watchdog};

#[cfg(test)]
mod tests;
