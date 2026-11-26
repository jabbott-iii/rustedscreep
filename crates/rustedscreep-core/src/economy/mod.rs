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
//! - [`remote_profit`]: remote mining's profit model (P6.1): what a room the bot mines without
//!   owning it earns and costs per creep life, in energy and in CPU, with or without a reserver
//!   and roads, and the plan to run it by ([`best_plan`]).
//! - [`Minerals`]: mineral mining (P9.1, ADR-044): each room's mineral miner and haulers, from
//!   its extractor, the container next to its mineral, and its terminal.
//! - [`Labs`]: lab reactions (P9.2, ADR-045): each room's lab roles, its job (one step of a
//!   target compound's chain), its reactions, and its lab hauler.
//! - [`Factories`]: the factory (P9.3, ADR-046): each room's job (the surplus mineral it
//!   compresses into its bar), its batches, its terminal's energy from the haulers, and its
//!   factory hauler.
//! - [`Terminals`]: terminal balancing (P9.4, ADR-047): the owned rooms' terminals send each
//!   other the base minerals up to a keep, paying the fees from energy the haulers bring.
//! - [`Remotes`]: remote mining's manager and creeps (P6.2, ADR-029): each home's candidate rooms,
//!   what the bot saw of them, their ways and prices, the rooms it mines, and the scouts,
//!   reservers, remote miners and remote haulers.

mod factory;
mod hauling;
mod labs;
mod minerals;
mod mining;
mod opening;
pub mod placement;
mod remote;
mod remotes;
pub mod sizing;
mod terminals;
mod upgrading;
mod watchdog;

pub use factory::{
    Factories, FactoryEvent, FactoryRoom, FactoryStats, Member as FactoryMember,
    PRIORITY_FACTORY_HAULER, ProduceOrder, Recipe as FactoryRecipe, Stop as FactoryStop,
    Task as FactoryTask, Why as FactoryWhy, life_energy as factory_life_energy,
    pace as factory_pace, pick as factory_pick, recipes as factory_recipes,
};
pub use hauling::{
    HaulInput, HaulPlan, HaulSource, HaulStats, HaulTask, HaulView, Hauling, PRIORITY_HAULER,
};
pub use labs::{
    Job as LabJob, LabEvent, LabRoom, LabStats, Labs, Member as LabMember, PRIORITY_LAB_HAULER,
    Phase as LabPhase, ReactionOrder, Step as LabStep, TARGETS as LAB_TARGETS, Task as LabTask,
    Why as LabWhy, goals as lab_goals, hauler_body as lab_hauler_body, pick as lab_pick,
    product_of as lab_product, source_pair as lab_source_pair,
};
pub use minerals::{
    HARVEST_PERIOD, Member as MineralMember, MineralEvent, MineralPlan, MineralRoom, MineralStats,
    Minerals, PRIORITY_MINERAL_HAULER, PRIORITY_MINERAL_MINER, Seen as MineralSeen,
    Why as MineralWhy, gate as mineral_gate, hauler_body as mineral_hauler_body,
    haulers_wanted as mineral_haulers_wanted, life_yield as mineral_life_yield,
    miner_body as mineral_miner_body, rate_capacity as mineral_rate, why as mineral_why,
};
pub use mining::{
    CONTROLLER_LINK_RANGE, ContainerNeed, LIFE_CYCLES, LINK_MARGIN, LinkNeed, LinkOrder,
    LinkTotals, MinePlan, MineSource, MineStats, Mined, Mining, PRIORITY_MINER, RECEIVER_RANGE,
    Receiver, SourceHarvest, link_loss, permille,
};

pub use remote::{
    RemoteCrew, RemoteHome, RemotePlan, RemoteProfit, RemoteRoom, RemoteSource, RemoteWay,
    RoadTiles, SourceCrew, best_plan, intent_ucpu, remote_container_hits, remote_crew,
    remote_miner_work, remote_profit, reserver_body,
};
pub use remotes::{
    Abandon, AbandonReason, Audit, Intel, PRIORITY_REMOTE_HAULER, PRIORITY_REMOTE_MINER,
    PRIORITY_RESERVER, PRIORITY_SCOUT, Remote, RemoteEvent, RemoteSite, RemoteStats, RemoteWays,
    Remotes, SourceWay, Threat, WaySearch, candidates, is_guest, is_remote_room,
};

pub use opening::{
    Fix, Opening, PRIORITY_LOW, PRIORITY_NORMAL, PRIORITY_RECOVERY, Plan, RoomStats, SourcePlan,
    Task, below_pct, below_share, needs_repair, rampart_goal, worker_capacity,
};
pub use terminals::{
    Giving as TerminalGiving, PlanScratch as TerminalPlanScratch, Send as TerminalSend, SendOrder,
    TerminalRoom, TerminalStats, Terminals, Wait as TerminalWait,
    affordable as terminal_affordable, fee as terminal_fee, plan as terminal_plan,
    range as terminal_range,
};
pub use upgrading::{
    Crew, PRIORITY_UPGRADER, UpgradePlan, UpgradeStats, UpgradeTask, UpgradeView, Upgrading, Ways,
    delivered, delivering, upkeep,
};
pub use watchdog::{Event, Observation, Watchdog};

#[cfg(test)]
mod factory_tests;
#[cfg(test)]
mod labs_tests;
#[cfg(test)]
mod minerals_tests;
#[cfg(test)]
mod remote_tests;
// Its two-room world is also the remote defenders' (P7.6, `defense::remote`).
#[cfg(test)]
pub(crate) mod remotes_tests;
#[cfg(test)]
mod terminals_tests;
#[cfg(test)]
mod tests;
