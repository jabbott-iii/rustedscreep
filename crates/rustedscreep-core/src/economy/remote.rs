//! Remote mining's profit model (ROADMAP P6.1, SPEC §3.5): what mining a remote room (a room the
//! bot mines without owning it) earns and costs, in energy and in CPU, over one creep life
//! (`CREEP_LIFE_TIME` ticks, the unit [`yield_per_life`] counts in), for each way of running it
//! ([`RemotePlan`]):
//!
//! - **With or without a reserver.** A reserver is a creep with CLAIM parts that reserves the
//!   room's controller. While the room is reserved its sources refill to `SOURCE_ENERGY_CAPACITY`
//!   instead of `SOURCE_ENERGY_NEUTRAL_CAPACITY` (V184), twice as much at the pinned values.
//! - **With or without roads** along the ways between the home and the sources. A road makes a
//!   step cost one fatigue a heavy part instead of two on plain and ten on swamp (V54), so the
//!   haulers need fewer MOVE parts, but the road costs energy to build and to keep up.
//!
//! The model prices the creeps the economy's own solvers size: the miner's MOVE parts
//! ([`miner_body_work`]), the haulers ([`hauler_plan_with`]) and the reserver
//! ([`reserver_body`]). The remote manager that spawns them (P6.2) uses the same functions, so
//! the model counts the bodies the bot would spawn.
//!
//! # What it counts, per creep life
//!
//! - **Energy in:** each source's yield at the plan's capacity ([`yield_per_life`]).
//! - **Energy out:** every creep's body, the repairs of the mining tiles' containers and of the
//!   roads (`REPAIR_COST` a hit, V222), and the one-time builds (a container not standing yet, the
//!   road tiles not built yet) spread over [`RemoteConfig::horizon_ticks`].
//! - **CPU:** every billed intent at `INTENT_CPU_COST` (V34: only an action that returns OK is
//!   billed, so a tired creep's move, refused game side with `ERR_TIRED`, costs nothing, V108),
//!   and [`RemoteConfig::creep_ucpu`] for each creep alive each tick (the bot's own snapshot,
//!   decision and act-step work).
//! - **Spawn time:** `CREEP_SPAWN_TIME` a part, for every life (V170).
//!
//! # What it assumes
//!
//! - **Posts are held without a gap.** A post is a place a creep stays at: a mining tile, or the
//!   tile by the controller. Pre-spawn replacement (P5.2, `SpawnConfig::replace`) orders each
//!   creep's successor so it arrives as the old one dies, so a post needs `need / (life − walk)`
//!   lives over `need` ticks there: each creep works its life less its walk out.
//! - **The miner repairs its container.** It carries one CARRY part for the repairs' energy and
//!   enough WORK parts to harvest the source empty and repair what the container loses in each
//!   regeneration window ([`remote_miner_work`]); a `repair` blocks that tick's `harvest` (V44,
//!   V45), so the two share the window's ticks.
//! - **The haulers repair the roads.** With roads each hauler carries one WORK part and repairs
//!   the road under it as it walks (`repair` and `move` run in the same tick, V45, V46), from the
//!   load it carries. Each repair restores at most `REPAIR_POWER` hits, so the repair intents are
//!   counted at one a `REPAIR_POWER` hits.
//! - **Each way is walked whole.** A hauler steps every tile of its source's way loaded and again
//!   empty, a miner once a life. The walks are [`walk_ticks`], lower bounds (as everywhere in
//!   [`crate::economy::sizing`]).
//! - **Each road tile belongs to one way.** A tile two ways share is counted in one of them
//!   ([`RemoteSource::road`]), so its decay is counted once; the other way's creeps' wear on it is
//!   not counted (a lower bound on wear). A road wears down faster under traffic: each step onto
//!   it brings its next decay step `ROAD_WEAROUT` ticks a body part closer (V54).
//! - **A reserver comes when the reservation can take its whole work.** A reserve that would take
//!   the reservation past `CONTROLLER_RESERVE_MAX` ticks still returns OK and is billed (V263),
//!   but the engine drops it (V264), so a reserver whose work does not fit wastes its life. The
//!   manager (P6.2) is
//!   expected to time its reservers so the reservation never caps; [`reserver_body`] picks bodies
//!   whose whole stay fits under the cap.
//!
//! Not counted: the builders' intents and bodies for the one-time builds (P6.2 decides who
//! builds), a reserver's wear on the roads, defense (P7), and hostile action.

use core::cmp::Ordering;

use rustedscreep_consts::{
    CARRY_CAPACITY, CONSTRUCTION_COST_ROAD_SWAMP_RATIO, CONSTRUCTION_COST_ROAD_WALL_RATIO,
    CONTAINER_DECAY, CONTAINER_DECAY_TIME, CONTROLLER_RESERVE, CONTROLLER_RESERVE_MAX,
    CREEP_CLAIM_LIFE_TIME, CREEP_LIFE_TIME, ENERGY_REGEN_TIME, HARVEST_POWER, INTENT_CPU_COST,
    MAX_CREEP_SIZE, Part, REPAIR_POWER, ROAD_DECAY_AMOUNT, ROAD_DECAY_TIME, ROAD_WEAROUT,
    SOURCE_ENERGY_CAPACITY, SOURCE_ENERGY_NEUTRAL_CAPACITY, StructureType,
};

use super::sizing::{
    Trip, hauler_plan_with, hits_per_energy, miner_body_work, miner_work, walk_ticks,
    yield_per_life,
};
use crate::config::RemoteConfig;
use crate::spawning::Body;

/// The model's window: one creep life (`CREEP_LIFE_TIME`, V170). Unit: ticks.
const LIFE: u64 = CREEP_LIFE_TIME as u64;

/// Millionths, for the CPU figures (µCPU).
const MICRO: u64 = 1_000_000;

/// The road tiles of a way, by the terrain under them: a road costs `CONSTRUCTION_COST[road]` on
/// plain and `CONSTRUCTION_COST_ROAD_SWAMP_RATIO` or `CONSTRUCTION_COST_ROAD_WALL_RATIO` times that
/// on swamp or on a wall (V186, V235), and each decay step takes `ROAD_DECAY_AMOUNT` hits times the
/// same ratio (V242). Unit: tiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoadTiles {
    pub plain: u32,
    pub swamp: u32,
    pub wall: u32,
}

impl RoadTiles {
    /// No road tile.
    pub const NONE: Self = Self {
        plain: 0,
        swamp: 0,
        wall: 0,
    };

    /// All the tiles. Unit: tiles.
    pub fn tiles(self) -> u32 {
        self.plain
            .saturating_add(self.swamp)
            .saturating_add(self.wall)
    }

    /// The tiles weighted by their terrain's ratio: what they cost to build and lose a decay step
    /// against one road on plain. Unit: plain-road tiles.
    ///
    /// Uses: `CONSTRUCTION_COST_ROAD_SWAMP_RATIO`, `CONSTRUCTION_COST_ROAD_WALL_RATIO`.
    pub fn weight(self) -> u64 {
        u64::from(self.plain)
            .saturating_add(
                u64::from(self.swamp).saturating_mul(u64::from(CONSTRUCTION_COST_ROAD_SWAMP_RATIO)),
            )
            .saturating_add(
                u64::from(self.wall).saturating_mul(u64::from(CONSTRUCTION_COST_ROAD_WALL_RATIO)),
            )
    }

    /// What building them costs: `CONSTRUCTION_COST[road]` a plain-road tile ([`Self::weight`]).
    /// Unit: energy.
    ///
    /// Uses: `CONSTRUCTION_COST` (`StructureType::construction_cost`), [`Self::weight`].
    pub fn build(self) -> u64 {
        let road = StructureType::Road.construction_cost().unwrap_or(0);
        self.weight().saturating_mul(u64::from(road))
    }
}

/// One way between the home and a post in a remote room, measured twice on movement's cost grid
/// ([`Trip`]): as the ground is (`bare`), and with the way's planned road tiles at the road's cost
/// (`paved`). The miner's and the haulers' way ends at the mining tile; the reserver's next to
/// the controller. The home end is the room's first spawn, where the creeps are born and the
/// haulers unload (a storage beside it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteWay {
    pub bare: Trip,
    pub paved: Trip,
}

impl RemoteWay {
    /// The way a plan's creeps walk.
    pub const fn trip(self, roads: bool) -> Trip {
        if roads { self.paved } else { self.bare }
    }
}

/// One source of a remote room, as the model sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteSource {
    /// Between the home and its mining tile.
    pub way: RemoteWay,
    /// The road tiles this way keeps with roads (its decay and its traffic's wear): every tile of
    /// the way that gets a road, less those another source's way already counts, and less the
    /// home room's own planned roads (the home's upkeep).
    pub road: RoadTiles,
    /// Of [`Self::road`], the tiles with no road yet: the roads plan builds them once.
    pub road_new: RoadTiles,
    /// A container stands on its mining tile: no container to build.
    pub container: bool,
}

/// A remote room, as the model sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteRoom<'a> {
    pub sources: &'a [RemoteSource],
    /// Between the home and a tile next to the controller (a reserve needs range 1). `None` for a
    /// room with no controller to reserve: no reserver plan.
    pub controller: Option<RemoteWay>,
}

/// The room that spawns the remote's creeps and takes its energy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteHome {
    /// What its spawns and extensions hold at most: the largest body it spawns. Unit: energy.
    pub energy: u32,
    /// What one transfer into its store takes at most (a storage's free space: one transfer a
    /// load; [`crate::economy::sizing::haul_round`]'s `unload`). Unit: energy.
    pub unload: u32,
    /// `HaulingConfig::margin_pct`: the haulers' spare carrying. Unit: percent.
    pub margin_pct: u32,
}

/// A way to run a remote room: whether a reserver keeps its controller reserved, and whether its
/// ways get roads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemotePlan {
    pub reserve: bool,
    pub roads: bool,
}

impl RemotePlan {
    /// Every plan, in the order [`best_plan`] breaks its last ties by.
    pub const ALL: [Self; 4] = [
        Self {
            reserve: true,
            roads: true,
        },
        Self {
            reserve: true,
            roads: false,
        },
        Self {
            reserve: false,
            roads: true,
        },
        Self {
            reserve: false,
            roads: false,
        },
    ];

    /// Each source's capacity under this plan: `SOURCE_ENERGY_CAPACITY` reserved, else
    /// `SOURCE_ENERGY_NEUTRAL_CAPACITY` (V184). Unit: energy.
    ///
    /// Uses: `SOURCE_ENERGY_CAPACITY`, `SOURCE_ENERGY_NEUTRAL_CAPACITY`.
    pub const fn capacity(self) -> u32 {
        if self.reserve {
            SOURCE_ENERGY_CAPACITY
        } else {
            SOURCE_ENERGY_NEUTRAL_CAPACITY
        }
    }
}

/// What a remote room earns and costs under one plan, over one creep life (`CREEP_LIFE_TIME`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteProfit {
    pub plan: RemotePlan,
    /// What its sources yield. Unit: energy.
    pub gross: u64,
    /// The bodies of its miners, haulers and reservers. Unit: energy.
    pub bodies: u64,
    /// The repairs of its containers and roads. Unit: energy.
    pub upkeep: u64,
    /// The one-time builds (containers and roads not standing yet), whole. Unit: energy.
    pub build: u64,
    /// The builds' share of one creep life over [`RemoteConfig::horizon_ticks`]. Unit: energy.
    pub amortized: u64,
    /// `gross − bodies − upkeep − amortized`: what reaches the home's store, less what the remote
    /// took from it. Unit: energy.
    pub net: i64,
    /// The billed intents of its creeps. Unit: intents.
    pub intents: u64,
    /// The ticks its creeps are alive, summed (the creeps alive on average × `CREEP_LIFE_TIME`).
    /// Unit: creep-ticks.
    pub creep_ticks: u64,
    /// Its CPU: `intents` at `INTENT_CPU_COST` and `creep_ticks` at
    /// [`RemoteConfig::creep_ucpu`]. Unit: µCPU.
    pub cpu_ucpu: u64,
    /// The home's spawn time it takes. Unit: ticks.
    pub spawn_ticks: u64,
    /// Its haulers, all sources together. Unit: creeps.
    pub haulers: u32,
}

impl RemoteProfit {
    /// The energy it nets per CPU it costs; `None` with no CPU. Unit: energy per CPU.
    pub fn per_cpu(&self) -> Option<i64> {
        if self.cpu_ucpu == 0 {
            return None;
        }
        let v = i128::from(self.net).saturating_mul(i128::from(MICRO)) / i128::from(self.cpu_ucpu);
        Some(i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX }))
    }

    /// What the home spends on it: `bodies + upkeep + amortized`. Unit: energy.
    pub fn spent(&self) -> u64 {
        self.bodies
            .saturating_add(self.upkeep)
            .saturating_add(self.amortized)
    }

    /// The energy it nets per energy the home spends on it ([`Self::spent`]); `None` when it
    /// spends nothing. Unit: permille.
    pub fn per_energy_permille(&self) -> Option<i64> {
        let spent = self.spent();
        if spent == 0 {
            return None;
        }
        let v = i128::from(self.net).saturating_mul(1000) / i128::from(spent);
        Some(i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX }))
    }

    /// The share of one spawn's time it takes. Unit: permille.
    pub fn spawn_permille(&self) -> u64 {
        self.spawn_ticks.saturating_mul(1000) / LIFE.max(1)
    }
}

/// `x × need / stay` rounded up: `x` per life of a post held `need` ticks by creeps that each stay
/// `stay` ticks there (at least 1).
fn share(x: u64, need: u64, stay: u64) -> u64 {
    let v = u128::from(x)
        .saturating_mul(u128::from(need))
        .div_ceil(u128::from(stay.max(1)));
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// What one billed intent costs: `INTENT_CPU_COST` (V34) in millionths, rounded (the crate's value
/// is an `f64`). Unit: µCPU.
///
/// Uses: `INTENT_CPU_COST`.
pub fn intent_ucpu() -> u64 {
    let v = (INTENT_CPU_COST * MICRO as f64).round();
    if v.is_finite() && v >= 0.0 {
        v.min(u64::MAX as f64) as u64
    } else {
        0
    }
}

/// Hits a container in a remote room loses over `ticks`: `CONTAINER_DECAY` every
/// `CONTAINER_DECAY_TIME − 1` ticks (V214: the schedule of a room whose controller has no level,
/// a reserved or a neutral one), rounded up. Unit: hits.
///
/// Uses: `CONTAINER_DECAY`, `CONTAINER_DECAY_TIME`.
pub fn remote_container_hits(ticks: u64) -> u64 {
    ticks
        .saturating_mul(u64::from(CONTAINER_DECAY))
        .div_ceil(u64::from(CONTAINER_DECAY_TIME.saturating_sub(1).max(1)))
}

/// Ticks `work` WORK parts take to harvest `capacity`: `HARVEST_POWER` each a tick (V183).
///
/// Uses: `HARVEST_POWER`.
fn harvest_ticks(capacity: u32, work: u32) -> u64 {
    u64::from(capacity).div_ceil(u64::from(work.saturating_mul(HARVEST_POWER).max(1)))
}

/// Ticks `work` WORK parts take to repair `hits`: `REPAIR_POWER` each a tick (V222).
///
/// Uses: `REPAIR_POWER`.
fn repair_ticks(hits: u64, work: u32) -> u64 {
    hits.div_ceil(u64::from(work.saturating_mul(REPAIR_POWER).max(1)))
}

/// WORK parts a remote miner takes (P6.1): the fewest, from [`miner_work`] up, that harvest a
/// source of `capacity` empty and repair what its container loses in one regeneration window
/// ([`remote_container_hits`] of `ENERGY_REGEN_TIME`) within that window's `ENERGY_REGEN_TIME`
/// ticks, since a tick's `repair` blocks its `harvest` (V44, V45). Room is left for one CARRY
/// and one MOVE part. `None` for an empty capacity, or when no count fits `MAX_CREEP_SIZE`.
///
/// Uses: `ENERGY_REGEN_TIME`, `HARVEST_POWER`, `REPAIR_POWER`, `MAX_CREEP_SIZE`, [`miner_work`],
/// [`remote_container_hits`].
pub fn remote_miner_work(capacity: u32) -> Option<u32> {
    if capacity == 0 {
        return None;
    }
    let hits = remote_container_hits(u64::from(ENERGY_REGEN_TIME));
    (miner_work(capacity)..=MAX_CREEP_SIZE.saturating_sub(2)).find(|&w| {
        harvest_ticks(capacity, w).saturating_add(repair_ticks(hits, w))
            <= u64::from(ENERGY_REGEN_TIME)
    })
}

/// A reserver's body for a controller `trip` from the home, in a room that holds at most `energy`
/// (P6.1): CLAIM parts, then MOVE parts. Each tick next to the controller a reserve adds
/// `CONTROLLER_RESERVE` ticks a CLAIM part to the reservation, which loses one a tick, so one CLAIM
/// part holds it steady and more grow it (V264, V265). A reserve needs range 1 (V263). A reserver
/// lives `CREEP_CLAIM_LIFE_TIME` (V170) and stays its life less its walk there ([`walk_ticks`],
/// every CLAIM part weighing, V54, V266). Among the bodies that cost at most `energy`,
/// hold at most `MAX_CREEP_SIZE` parts, and whose whole stay fits under the cap (`(claim ×
/// CONTROLLER_RESERVE − 1) × stay ≤ CONTROLLER_RESERVE_MAX`: a reserve past it is billed and
/// dropped), it takes the least energy for a tick of reservation (`cost / (claim × stay)`), then
/// the fewest intents for one (`(stay + tiles) / (claim × stay)`: a reserve each tick there and a
/// move each tile out), then the fewest parts. MOVE parts past the ones that bring the walk down
/// to `trip.tiles` save nothing. `None` when one CLAIM and one MOVE cost more than `energy`, or no
/// body stays at all.
///
/// Uses: `BODYPART_COST` (`Part::cost`), `CREEP_CLAIM_LIFE_TIME`, `CONTROLLER_RESERVE`,
/// `CONTROLLER_RESERVE_MAX`, `MAX_CREEP_SIZE`, [`walk_ticks`].
pub fn reserver_body(trip: Trip, energy: u32) -> Option<Body> {
    let claim_cost = Part::Claim.cost();
    let move_cost = Part::Move.cost();
    let mut best: Option<Reserver> = None;
    for claim in 1..MAX_CREEP_SIZE {
        let fixed = claim_cost.saturating_mul(claim);
        if fixed.saturating_add(move_cost) > energy {
            break;
        }
        for moves in 1..=MAX_CREEP_SIZE.saturating_sub(claim) {
            let cost = fixed.saturating_add(move_cost.saturating_mul(moves));
            if cost > energy {
                break;
            }
            let walk = walk_ticks(trip, claim, moves)?;
            let Some(stay) = CREEP_CLAIM_LIFE_TIME.checked_sub(walk).filter(|&s| s > 0) else {
                continue;
            };
            let growth = claim.saturating_mul(CONTROLLER_RESERVE).saturating_sub(1);
            if u64::from(growth) * u64::from(stay) <= u64::from(CONTROLLER_RESERVE_MAX) {
                let r = Reserver {
                    cost,
                    claim,
                    moves,
                    stay,
                };
                if best.is_none_or(|b| r.better(&b, trip.tiles)) {
                    best = Some(r);
                }
            }
            if walk <= trip.tiles {
                break;
            }
        }
    }
    let r = best?;
    let mut parts = [Part::Move; MAX_CREEP_SIZE as usize];
    let claim = usize::try_from(r.claim).ok()?;
    let len = claim.checked_add(usize::try_from(r.moves).ok()?)?;
    for part in parts.iter_mut().take(claim) {
        *part = Part::Claim;
    }
    Body::new(parts.get(..len)?)
}

/// A reserver body [`reserver_body`] weighs.
#[derive(Clone, Copy)]
struct Reserver {
    cost: u32,
    claim: u32,
    moves: u32,
    stay: u32,
}

impl Reserver {
    /// Reservation ticks one life adds: `claim × stay` (times `CONTROLLER_RESERVE`, the same for
    /// every body).
    fn adds(&self) -> u128 {
        u128::from(self.claim) * u128::from(self.stay)
    }

    /// Less energy a reservation tick, then fewer intents a reservation tick, then fewer parts.
    fn better(&self, other: &Self, tiles: u32) -> bool {
        let energy =
            (u128::from(self.cost) * other.adds()).cmp(&(u128::from(other.cost) * self.adds()));
        let intents = (u128::from(self.stay.saturating_add(tiles)) * other.adds())
            .cmp(&(u128::from(other.stay.saturating_add(tiles)) * self.adds()));
        let parts = (self.claim + self.moves).cmp(&(other.claim + other.moves));
        energy.then(intents).then(parts) == Ordering::Less
    }
}

/// What a plan's creeps add up to over one creep life.
#[derive(Default)]
struct Ledger {
    bodies: u64,
    upkeep: u64,
    intents: u64,
    creep_ticks: u64,
    spawn_ticks: u64,
}

impl Ledger {
    /// A post held `need` ticks a life by creeps of `body` that each walk `tiles` tiles out (a
    /// billed move each) and stay `stay` ticks there: `need / stay` lives, each its body's cost,
    /// spawn time and life.
    fn post(&mut self, body: &Body, tiles: u32, need: u64, stay: u64) {
        self.bodies = self
            .bodies
            .saturating_add(share(u64::from(body.cost()), need, stay));
        self.spawn_ticks =
            self.spawn_ticks
                .saturating_add(share(u64::from(body.spawn_time()), need, stay));
        self.creep_ticks =
            self.creep_ticks
                .saturating_add(share(u64::from(body.life()), need, stay));
        self.intents = self
            .intents
            .saturating_add(share(u64::from(tiles), need, stay));
    }

    /// `n` creeps of `body`, each alive one creep life.
    fn crew(&mut self, body: &Body, n: u32) {
        let n = u64::from(n);
        self.bodies = self
            .bodies
            .saturating_add(n.saturating_mul(u64::from(body.cost())));
        self.spawn_ticks = self
            .spawn_ticks
            .saturating_add(n.saturating_mul(u64::from(body.spawn_time())));
        self.creep_ticks = self
            .creep_ticks
            .saturating_add(n.saturating_mul(u64::from(body.life())));
    }
}

fn parts(body: &Body, part: Part) -> u32 {
    u32::try_from(body.count(part)).unwrap_or(u32::MAX)
}

fn len(body: &Body) -> u64 {
    u64::try_from(body.len()).unwrap_or(u64::MAX)
}

/// What `room` earns and costs under `plan` over one creep life ([`RemoteProfit`]; the module
/// docs list what it counts and assumes), with creeps sized for `home`. Per source: the yield at
/// [`RemotePlan::capacity`]; a miner of [`remote_miner_work`] WORK parts, one CARRY and the MOVE
/// parts [`miner_body_work`] solves, at its post every tick (its harvests, `ceil(capacity / (work
/// × HARVEST_POWER))` a regeneration window, and its container's repairs,
/// [`remote_container_hits`] over the life at `REPAIR_POWER` a WORK part); the haulers
/// [`hauler_plan_with`] sizes for the yield (one WORK part each with roads), each load a withdraw,
/// `ceil(load / unload)` transfers and a move a tile both ways; and with roads, the road's decay
/// over the life and its wear, `ROAD_WEAROUT` a body part a step of the haulers' and the miners'
/// walks, `ROAD_DECAY_AMOUNT` a decay step a plain-road tile every `ROAD_DECAY_TIME` ticks
/// (V242; an upper bound: wear that brings a step forward past the tick is lost when the step sets
/// the next one). With a reserver, [`reserver_body`]'s creeps at the controller for `ceil(LIFE /
/// (claim × CONTROLLER_RESERVE))` ticks, a reserve each tick there. A container not standing
/// costs `CONSTRUCTION_COST[container]` once and the roads plan's new tiles
/// [`RoadTiles::build`], both spread over [`RemoteConfig::horizon_ticks`]. `None` when a creep
/// the plan needs does not fit `home.energy` or would walk its whole life, or for a reserver plan
/// in a room without a controller.
///
/// Uses: `CREEP_LIFE_TIME`, `CARRY_CAPACITY`, `HARVEST_POWER`, `REPAIR_POWER`,
/// `ENERGY_REGEN_TIME`, `ROAD_WEAROUT`, `ROAD_DECAY_AMOUNT`, `ROAD_DECAY_TIME`,
/// `CONTROLLER_RESERVE`, `CONSTRUCTION_COST`, `INTENT_CPU_COST`, `REPAIR_COST` (through
/// `hits_per_energy`), [`yield_per_life`], [`remote_miner_work`], [`miner_body_work`],
/// [`hauler_plan_with`], [`reserver_body`], [`walk_ticks`].
pub fn remote_profit(
    room: &RemoteRoom<'_>,
    plan: RemotePlan,
    home: RemoteHome,
    cfg: &RemoteConfig,
) -> Option<RemoteProfit> {
    let capacity = plan.capacity();
    let per_hit = u64::from(hits_per_energy());
    let mut ledger = Ledger::default();
    let (mut gross, mut build, mut haulers) = (0u64, 0u64, 0u32);
    for s in room.sources {
        let trip = s.way.trip(plan.roads);
        let yielded = yield_per_life(capacity);
        gross = gross.saturating_add(yielded);
        let crew = source_crew(s, plan, home)?;

        // The miner: at its post every tick of the life.
        let work = crew.work;
        let miner = crew.miner;
        let stay = LIFE
            .checked_sub(u64::from(crew.miner_walk))
            .filter(|&s| s > 0)?;
        ledger.post(&miner, trip.tiles, LIFE, stay);
        let harvests = share(
            harvest_ticks(capacity, work),
            LIFE,
            u64::from(ENERGY_REGEN_TIME),
        );
        let hits = remote_container_hits(LIFE);
        ledger.intents = ledger
            .intents
            .saturating_add(harvests)
            .saturating_add(repair_ticks(hits, work));
        let repaired = hits.div_ceil(per_hit.max(1));
        ledger.upkeep = ledger.upkeep.saturating_add(repaired);
        if !s.container {
            let container = StructureType::Container.construction_cost().unwrap_or(0);
            build = build.saturating_add(u64::from(container));
        }

        // The haulers: what reaches the container, in whole loads.
        let roads = plan.roads && s.road.tiles() > 0;
        let repair_work = u32::from(roads);
        let (hauler, n) = (crew.hauler, crew.haulers);
        ledger.crew(&hauler, n);
        haulers = haulers.saturating_add(n);
        let load = u64::from(parts(&hauler, Part::Carry)).saturating_mul(u64::from(CARRY_CAPACITY));
        let loads = yielded.saturating_sub(repaired).div_ceil(load.max(1));
        let fills = load.div_ceil(u64::from(home.unload.max(1))).max(1);
        let per_load = 1u64
            .saturating_add(fills)
            .saturating_add(u64::from(trip.tiles).saturating_mul(2));
        ledger.intents = ledger
            .intents
            .saturating_add(loads.saturating_mul(per_load));

        // The roads: their decay over the life, brought forward by the traffic's steps.
        if roads {
            let steps = loads
                .saturating_mul(2)
                .saturating_mul(len(&hauler))
                .saturating_add(share(len(&miner), LIFE, stay));
            let wear = steps.saturating_mul(u64::from(ROAD_WEAROUT));
            let road_hits = u64::from(ROAD_DECAY_AMOUNT)
                .saturating_mul(s.road.weight())
                .saturating_mul(LIFE.saturating_add(wear))
                .div_ceil(u64::from(ROAD_DECAY_TIME.max(1)));
            ledger.upkeep = ledger
                .upkeep
                .saturating_add(road_hits.div_ceil(per_hit.max(1)));
            ledger.intents = ledger
                .intents
                .saturating_add(repair_ticks(road_hits, repair_work));
            build = build.saturating_add(s.road_new.build());
        }
    }

    // The reserver: CLAIM parts at the controller for the ticks that hold the reservation.
    if plan.reserve {
        let trip = room.controller?.trip(plan.roads);
        let (reserver, walk) = reserver_crew(room, plan, home)?;
        let claim = parts(&reserver, Part::Claim);
        let stay = u64::from(CREEP_CLAIM_LIFE_TIME)
            .checked_sub(u64::from(walk))
            .filter(|&s| s > 0)?;
        let need = LIFE.div_ceil(u64::from(claim.saturating_mul(CONTROLLER_RESERVE).max(1)));
        ledger.post(&reserver, trip.tiles, need, stay);
        ledger.intents = ledger.intents.saturating_add(need);
    }

    let amortized = share(build, LIFE, u64::from(cfg.horizon_ticks));
    let spent = ledger
        .bodies
        .saturating_add(ledger.upkeep)
        .saturating_add(amortized);
    let net = i128::from(gross) - i128::from(spent);
    let cpu_ucpu = ledger
        .intents
        .saturating_mul(intent_ucpu())
        .saturating_add(ledger.creep_ticks.saturating_mul(u64::from(cfg.creep_ucpu)));
    Some(RemoteProfit {
        plan,
        gross,
        bodies: ledger.bodies,
        upkeep: ledger.upkeep,
        build,
        amortized,
        net: i64::try_from(net).unwrap_or(if net < 0 { i64::MIN } else { i64::MAX }),
        intents: ledger.intents,
        creep_ticks: ledger.creep_ticks,
        cpu_ucpu,
        spawn_ticks: ledger.spawn_ticks,
        haulers,
    })
}

/// The creeps one source of a remote room needs under a plan ([`remote_crew`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceCrew {
    /// The miner's WORK parts ([`remote_miner_work`]). Unit: parts.
    pub work: u32,
    /// The miner: `work` WORK parts, one CARRY part and the MOVE parts [`miner_body_work`] solves.
    pub miner: Body,
    /// Its walk out from the home ([`walk_ticks`], a lower bound). Unit: ticks.
    pub miner_walk: u32,
    /// The haulers' body ([`hauler_plan_with`], one WORK part on a way with roads).
    pub hauler: Body,
    /// Haulers the source keeps. Unit: creeps.
    pub haulers: u32,
    /// A hauler's walk one way, loaded ([`walk_ticks`]). Unit: ticks.
    pub hauler_walk: u32,
}

/// The creeps a remote room needs under a plan: what [`remote_profit`] prices, so the manager
/// (P6.2) spawns the bodies the model counted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCrew {
    pub plan: RemotePlan,
    /// One per source, in [`RemoteRoom::sources`]' order.
    pub sources: Vec<SourceCrew>,
    /// The reserver's body ([`reserver_body`]) and its walk to the controller, with a reserver plan.
    pub reserver: Option<(Body, u32)>,
}

/// The creeps of one source under `plan`, sized for `home`; `None` as in [`remote_profit`].
///
/// Uses: [`remote_miner_work`], [`miner_body_work`], [`hauler_plan_with`], [`walk_ticks`],
/// `CREEP_LIFE_TIME`.
fn source_crew(s: &RemoteSource, plan: RemotePlan, home: RemoteHome) -> Option<SourceCrew> {
    let capacity = plan.capacity();
    let trip = s.way.trip(plan.roads);
    let work = remote_miner_work(capacity)?;
    let miner = miner_body_work(work, 1, capacity, trip, home.energy)?;
    let miner_walk = walk_ticks(trip, work, parts(&miner, Part::Move))?;
    LIFE.checked_sub(u64::from(miner_walk)).filter(|&s| s > 0)?;
    let repair_work = u32::from(plan.roads && s.road.tiles() > 0);
    let (hauler, haulers) = hauler_plan_with(
        capacity,
        trip,
        home.energy,
        home.margin_pct,
        home.unload,
        repair_work,
    )?;
    let weight = parts(&hauler, Part::Carry).saturating_add(parts(&hauler, Part::Work));
    let hauler_walk = walk_ticks(trip, weight, parts(&hauler, Part::Move))?;
    Some(SourceCrew {
        work,
        miner,
        miner_walk,
        hauler,
        haulers,
        hauler_walk,
    })
}

/// The reserver's body and walk under a reserver plan, `None` without a controller or a body.
///
/// Uses: [`reserver_body`], [`walk_ticks`].
fn reserver_crew(room: &RemoteRoom<'_>, plan: RemotePlan, home: RemoteHome) -> Option<(Body, u32)> {
    let trip = room.controller?.trip(plan.roads);
    let reserver = reserver_body(trip, home.energy)?;
    let walk = walk_ticks(
        trip,
        parts(&reserver, Part::Claim),
        parts(&reserver, Part::Move),
    )?;
    Some((reserver, walk))
}

/// The creeps `room` needs under `plan` ([`RemoteCrew`]), or `None` where [`remote_profit`] has no
/// figure for the plan.
pub fn remote_crew(
    room: &RemoteRoom<'_>,
    plan: RemotePlan,
    home: RemoteHome,
) -> Option<RemoteCrew> {
    let sources = room
        .sources
        .iter()
        .map(|s| source_crew(s, plan, home))
        .collect::<Option<Vec<_>>>()?;
    let reserver = if plan.reserve {
        Some(reserver_crew(room, plan, home)?)
    } else {
        None
    };
    Some(RemoteCrew {
        plan,
        sources,
        reserver,
    })
}

/// The plan to run `room` by, with what it earns and costs ([`remote_profit`]), or `None` when no
/// plan nets more energy than it takes: of the plans in [`RemotePlan::ALL`] that net more than 0,
/// those within [`RemoteConfig::slack_pct`] of the best plan's gross yield of the best net (the
/// model's walks are lower bounds, so nets that close are within its error), the one with the
/// least CPU, then the most net, then the fewest spawn ticks, then the first in
/// [`RemotePlan::ALL`]. Net energy comes first: a plan that nets clearly less is never taken for
/// its CPU.
pub fn best_plan(
    room: &RemoteRoom<'_>,
    home: RemoteHome,
    cfg: &RemoteConfig,
) -> Option<RemoteProfit> {
    let all =
        RemotePlan::ALL.map(|plan| remote_profit(room, plan, home, cfg).filter(|p| p.net > 0));
    let top = all
        .iter()
        .flatten()
        .fold(None::<&RemoteProfit>, |best, p| {
            if best.is_none_or(|b| p.net > b.net) {
                Some(p)
            } else {
                best
            }
        })?;
    let slack = top.gross.saturating_mul(u64::from(cfg.slack_pct)) / 100;
    let floor = top
        .net
        .saturating_sub(i64::try_from(slack).unwrap_or(i64::MAX));
    all.iter()
        .flatten()
        .enumerate()
        .filter(|(_, p)| p.net >= floor)
        .min_by_key(|&(i, p)| (p.cpu_ucpu, core::cmp::Reverse(p.net), p.spawn_ticks, i))
        .map(|(_, p)| *p)
}
