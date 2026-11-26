//! Lab reactions (ROADMAP P9.2, ADR-045): each owned room with an own terminal and at least three
//! own labs makes, one job at a time, the next step of the first target compound whose terminal
//! stock is below its goal (the user's four choices: a target list with stock goals, labs built
//! once reagents are on hand, a lab hauler, compounds only with no boosting).
//!
//! - **The reaction** (V403 to V407). An output lab's `runReaction(lab1, lab2)` takes
//!   `LAB_REACTION_AMOUNT` (5) of a mineral from each of two source labs within
//!   `LAB_REACTION_RANGE` (2) of it and makes as much of their product, whose `REACTION_TIME`
//!   becomes the output's cooldown. Only the output's cooldown, owner and level are checked, so
//!   every output in range of both sources can react on the same tick, each while the sources
//!   still hold 5 (the processor counts them down in order, V405). Reactions use no energy (V407).
//! - **The source labs** ([`source_pair`]): of a room's labs, the pair with the most other labs in
//!   range of both, which the planner's lab stamp makes its two middle labs once all 10 stand
//!   (V240). The other labs in range of both are the outputs; the rest stay unused. The roles are
//!   chosen again when the room's set of labs changes, keeping the current pair on a tie.
//! - **The job** ([`pick`]): for the first target in [`TARGETS`] order whose stock is at least
//!   `labs.batch_min` below its goal, the deepest step of its chain
//!   (`ResourceType::reaction_components`, V409) whose two reagents the terminal holds: a step runs
//!   when both reagents hold at least the least of its shortfall and `labs.batch_min`, else its
//!   first short reagent's own step is looked for, down to the base minerals. A target that needs a
//!   base mineral the terminal lacks is passed over. A job makes at most `LAB_MINERAL_CAPACITY`, a
//!   source lab's load, so one empty output always has room for the whole product (V410, V411).
//! - **Its phases** ([`Phase`]). While it runs, the lab hauler loads each source lab with its
//!   reagent from the terminal up to the job's amount, and every output reacts when its cooldown
//!   reads 0. Once a source holds less than `LAB_REACTION_AMOUNT` and no more can be loaded, the
//!   job empties: the hauler takes every lab's mineral back to the terminal, and with all labs
//!   empty the next job is picked. A lab holding another mineral than its role's is emptied first
//!   (a lab holds one mineral at a time, V411), as is an output that cannot take another reaction.
//! - **The lab hauler** ([`Role::LabHauler`]): CARRY and MOVE parts ([`hauler_body`]), one a room,
//!   requested while the room's labs have work ([`LabRoom::has_work`]) and the Standard tier runs,
//!   at [`PRIORITY_LAB_HAULER`], below the mineral creeps. It withdraws one mineral at a time (from
//!   a lab with no amount, so all it can hold, V416; from the terminal with the load's amount) and
//!   transfers it with no amount (V415): a reagent to the source lab that still needs it, anything
//!   else to the terminal.
//! - **Building the labs** ([`Labs::wants_labs`]): a room wants labs (`planner.labs` places them
//!   only there, `planner::build::Gates::with_labs`) once an own lab stands in it, or once its
//!   terminal holds enough of both reagents for a target's next step ([`pick`] finds one).
//! - **The upgraders' budget** ([`Labs::upkeeps`]): the lab hauler's body over a creep life while
//!   the room's labs have work, which `economy::Upgrading::set_labs` leaves out of their budget.
//!
//! The reads (the snapshot's `LabSites`): a room is read on the tick after one that asks for it
//! ([`Labs::reads`]): every tick the Standard tier runs while its lab hauler is out of the spawn
//! and has work (a task at the last read, or a load), on the tick its first output's cooldown
//! ends ([`LabRoom::reaction_due`]), and otherwise once `labs.read_ticks` have passed since its
//! last read. Between reads nothing acts.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Labs::begin`] after the snapshot: the rooms (an own terminal or lab, or a lab hauler's
//!    home), this tick's read of each with its lab roles, and each room's lab haulers.
//! 2. The Critical `labs` job runs [`Labs::plan_room`] per room, before the spawn plan: the job's
//!    phase or a new job, whether the room wants labs, and the hauler's request.
//! 3. The Standard `labs work` job runs [`Labs::work_room`] per room read this tick: the
//!    reactions ([`Labs::orders`], which the act step issues as `runReaction`) and the hauler's
//!    move, withdraw and transfer.
//! 4. [`Labs::reads`]: the rooms the next tick's snapshot reads.
//!
//! Everything is kept in the heap. After a global reset a room's first read adopts a job already
//! loaded in its source labs (two reagents of one reaction, each at least `LAB_REACTION_AMOUNT`),
//! with no more loading; labs holding anything else are emptied before a new job. Nothing is read
//! from or written to `Memory`.

use std::cmp::Reverse;

use rustedscreep_consts::{
    CARRY_CAPACITY, LAB_MINERAL_CAPACITY, LAB_REACTION_AMOUNT, LAB_REACTION_RANGE, MAX_CREEP_SIZE,
    MOVE_COST_ROAD, MOVE_POWER, Part, Position, ResourceType, RoomName, StructureType,
};

use crate::config::{LabsConfig, MovementConfig};
use crate::intents::{CreepIntent, IntentBuffer, Outcome, Target};
use crate::movement::{MoveTo, Movement, Tile};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{
    CreepHandle, CreepStore, LAB_RESOURCES, LabRead, LabState, Snapshot, StructureHandle,
    lab_resource,
};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner};

/// The spawn priority of a lab hauler: below the mineral miner (65) and hauler (66), so a room
/// mines its own mineral before it reacts imported ones.
pub const PRIORITY_LAB_HAULER: u8 = 64;

/// How hard the lab hauler's moves push past other creeps: as the mineral haulers'.
const HAULER_MOVE_PRIORITY: u8 = 2;

/// Withdrawing and transferring reach 1 tile (`isNearTo`, V399, V400).
const NEAR: u8 = 1;

/// The most reaction steps [`pick`] follows down one chain: a tier-3 compound is 5 reactions
/// above its deepest base mineral (XGHO2 from GHO2, GO, G, UL and U), so 8 is never reached; it
/// only bounds the search.
const MAX_DEPTH: u32 = 8;

/// The labs' target compounds, worked in this order, each with its goal in `labs.goal_*`
/// ([`goals`]): the siege breaker's three boosts (SPEC §4b: TOUGH, dismantle, MOVE), then heal,
/// ranged attack, attack and upgrade (`BOOSTS`, V131).
pub const TARGETS: [ResourceType; 7] = [
    ResourceType::CatalyzedGhodiumAlkalide,
    ResourceType::CatalyzedZynthiumAcid,
    ResourceType::CatalyzedZynthiumAlkalide,
    ResourceType::CatalyzedLemergiumAlkalide,
    ResourceType::CatalyzedKeaniumAlkalide,
    ResourceType::CatalyzedUtriumAcid,
    ResourceType::CatalyzedGhodiumAcid,
];

/// Each of [`TARGETS`]' goals, in its order. Unit: units.
pub const fn goals(cfg: &LabsConfig) -> [u32; TARGETS.len()] {
    [
        cfg.goal_xgho2,
        cfg.goal_xzh2o,
        cfg.goal_xzho2,
        cfg.goal_xlho2,
        cfg.goal_xkho2,
        cfg.goal_xuh2o,
        cfg.goal_xgh2o,
    ]
}

/// A terminal's stock, by index in [`LAB_RESOURCES`]. Unit: units.
pub type Stock = [u32; LAB_RESOURCES.len()];

fn stock_of(stock: &Stock, kind: ResourceType) -> u32 {
    lab_resource(kind)
        .and_then(|i| stock.get(i).copied())
        .unwrap_or(0)
}

/// The product of `a` and `b`: the compound whose two reagents they are, in either order
/// (`REACTIONS` lists both, V404); `None` when they make nothing.
///
/// Uses: `REACTIONS` (`ResourceType::reaction_components`, V409).
pub fn product_of(a: ResourceType, b: ResourceType) -> Option<ResourceType> {
    LAB_RESOURCES.iter().copied().find(|p| {
        p.reaction_components()
            .is_some_and(|[x, y]| (x == a && y == b) || (x == b && y == a))
    })
}

/// One reaction step the labs can run: its product, its two reagents, and how much of each it
/// loads (and so of the product it makes). Unit of the amount: units, a whole number of
/// `LAB_REACTION_AMOUNT`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub product: ResourceType,
    pub reagents: [ResourceType; 2],
    pub amount: u32,
}

/// The next job for a terminal holding `stock` (module docs): for the first of [`TARGETS`] whose
/// stock is at least `labs.batch_min` below its goal (a goal of 0 is no target), the deepest step
/// of its chain the terminal holds both reagents of; `None` when no target needs one or every one
/// that does lacks a base mineral.
///
/// Uses: `LAB_REACTION_AMOUNT`, `LAB_MINERAL_CAPACITY`, `REACTIONS`
/// (`ResourceType::reaction_components`, V409).
pub fn pick(stock: &Stock, cfg: &LabsConfig) -> Option<Step> {
    let min = cfg.batch_min.max(LAB_REACTION_AMOUNT);
    TARGETS.iter().zip(goals(cfg)).find_map(|(&target, goal)| {
        let have = stock_of(stock, target);
        if goal == 0 || have.saturating_add(min) > goal {
            return None;
        }
        step_for(stock, target, goal.saturating_sub(have), min, 0)
    })
}

/// The step that brings `want` more of `product` closest (module docs, [`pick`]): `product`'s own
/// reaction when the terminal holds at least `min(want, min)` of both reagents, making the least
/// of `want`, both stocks and `LAB_MINERAL_CAPACITY`, rounded down to a whole number of
/// `LAB_REACTION_AMOUNT`s; else, when both reagents can be had ([`can_have`]), the step for its
/// first short reagent, for what `product` still lacks of it. `None` for a base mineral (it has no
/// reaction), for a product one of whose reagents cannot be had, or past [`MAX_DEPTH`].
fn step_for(stock: &Stock, product: ResourceType, want: u32, min: u32, depth: u32) -> Option<Step> {
    if depth > MAX_DEPTH {
        return None;
    }
    let reagents = product.reaction_components()?;
    let want = want
        .min(LAB_MINERAL_CAPACITY)
        .next_multiple_of(LAB_REACTION_AMOUNT);
    let floor = want.min(min);
    let [a, b] = reagents.map(|r| stock_of(stock, r));
    if a >= floor && b >= floor {
        let amount = want.min(a).min(b).min(LAB_MINERAL_CAPACITY);
        let amount = amount - amount % LAB_REACTION_AMOUNT;
        return (amount >= LAB_REACTION_AMOUNT).then_some(Step {
            product,
            reagents,
            amount,
        });
    }
    let ok = |r: ResourceType, held: u32| {
        held >= floor
            || can_have(
                stock,
                r,
                want.saturating_sub(held),
                min,
                depth.saturating_add(1),
            )
    };
    if !(ok(reagents[0], a) && ok(reagents[1], b)) {
        return None;
    }
    let (short, held) = if a < floor {
        (reagents[0], a)
    } else {
        (reagents[1], b)
    };
    step_for(
        stock,
        short,
        want.saturating_sub(held),
        min,
        depth.saturating_add(1),
    )
}

/// Whether `want` more of `product` can be had from `stock` through reactions: its reagents are
/// held (at least `min(want, min)` each, as [`step_for`] asks) or can be had in turn, down to the
/// base minerals, which must be held. So a target whose chain needs a base mineral the terminal
/// lacks gets no step, and no intermediate is made for it.
fn can_have(stock: &Stock, product: ResourceType, want: u32, min: u32, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let Some(reagents) = product.reaction_components() else {
        return false;
    };
    let want = want
        .min(LAB_MINERAL_CAPACITY)
        .next_multiple_of(LAB_REACTION_AMOUNT);
    let floor = want.min(min);
    reagents.iter().all(|&r| {
        let held = stock_of(stock, r);
        held >= floor
            || can_have(
                stock,
                r,
                want.saturating_sub(held),
                min,
                depth.saturating_add(1),
            )
    })
}

/// The lab hauler's body for a room that holds at most `energy`: CARRY parts and a MOVE part for
/// every `MOVE_POWER / MOVE_COST_ROAD` of them (rounded up), so it walks roads loaded at a tile a
/// tick (each loaded CARRY part adds `MOVE_COST_ROAD` fatigue a tile, each MOVE part removes
/// `MOVE_POWER` a tick, V54, V55); the most CARRY parts up to `max_carry` that fit `energy` and
/// `MAX_CREEP_SIZE`. `None` when not even one CARRY and one MOVE part fit.
///
/// Uses: `BODYPART_COST` (`Part::cost`), `MAX_CREEP_SIZE`, `MOVE_POWER`, `MOVE_COST_ROAD`.
pub fn hauler_body(energy: u32, max_carry: u32) -> Option<Body> {
    let per_move = (MOVE_POWER / MOVE_COST_ROAD.max(1)).max(1);
    let (carry, moves) = (1..=max_carry.min(MAX_CREEP_SIZE))
        .rev()
        .find_map(|carry| {
            let moves = carry.div_ceil(per_move);
            let cost = carry
                .saturating_mul(Part::Carry.cost())
                .saturating_add(moves.saturating_mul(Part::Move.cost()));
            (carry.saturating_add(moves) <= MAX_CREEP_SIZE && cost <= energy)
                .then_some((carry, moves))
        })?;
    let mut parts = [Part::Move; MAX_CREEP_SIZE as usize];
    let carry = usize::try_from(carry).ok()?;
    let len = carry.checked_add(usize::try_from(moves).ok()?)?;
    for p in parts.iter_mut().take(carry) {
        *p = Part::Carry;
    }
    Body::new(parts.get(..len)?)
}

/// The two source labs among `labs` (their tiles), as indices into it (module docs): the pair
/// with the most other labs within `LAB_REACTION_RANGE` of both (V240, V403), ties to `current`
/// when it is one of the tied pairs, then to the least sum of the ranges from both to those labs,
/// then to the lowest indices. `None` when no pair has another lab in range of both (fewer than 3
/// labs, or none placed together).
///
/// Uses: `LAB_REACTION_RANGE`.
pub fn source_pair(labs: &[Tile], current: Option<[Tile; 2]>) -> Option<[usize; 2]> {
    let near = |a: Tile, b: Tile| u32::from(a.range(b)) <= LAB_REACTION_RANGE;
    /// How a pair ranks: the labs in range of both, whether it is the current pair, the least
    /// sum of ranges.
    type Rank = (usize, bool, Reverse<u32>);
    let mut best: Option<(Rank, [usize; 2])> = None;
    for (i, &a) in labs.iter().enumerate() {
        for (j, &b) in labs.iter().enumerate().skip(i.saturating_add(1)) {
            let mut count = 0usize;
            let mut sum = 0u32;
            for (k, &c) in labs.iter().enumerate() {
                if k != i && k != j && near(a, c) && near(b, c) {
                    count += 1;
                    sum = sum
                        .saturating_add(u32::from(a.range(c)))
                        .saturating_add(u32::from(b.range(c)));
                }
            }
            if count == 0 {
                continue;
            }
            let kept = current.is_some_and(|[x, y]| (x == a && y == b) || (x == b && y == a));
            let key = (count, kept, Reverse(sum));
            if best.as_ref().is_none_or(|(k, _)| key > *k) {
                best = Some((key, [i, j]));
            }
        }
    }
    best.map(|(_, pair)| pair)
}

/// A job's phase (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The source labs are loaded and the outputs react.
    Run,
    /// Every lab's mineral goes back to the terminal.
    Empty,
}

/// The room's job (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job {
    pub product: ResourceType,
    pub reagents: [ResourceType; 2],
    /// How much of each reagent the job loads. Unit: units.
    pub amount: u32,
    /// How much of each reagent went into its source lab so far (the transfers ordered). Unit:
    /// units.
    pub loaded: [u32; 2],
    /// Reactions ordered for it. Unit: reactions.
    pub reactions: u32,
    pub phase: Phase,
}

/// What a room's labs do, from its last read (module docs). The order is the order of the
/// checks, so a room shows the first that holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Why {
    /// `labs.enabled` is off.
    Off,
    /// Not read yet in this global.
    #[default]
    Unread,
    NoTerminal,
    /// No own lab stands (the room may want them built, [`LabRoom::wants`]).
    NoLabs,
    /// No pair of labs has another lab in range of both ([`source_pair`]).
    FewLabs,
    /// A job runs.
    Reacting,
    /// A job's labs are being emptied.
    Emptying,
    /// No job: labs hold minerals that are emptied before one starts.
    Clearing,
    /// No job: every target is at its goal or needs a base mineral the terminal lacks.
    Idle,
}

impl Why {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Unread => "not read yet",
            Self::NoTerminal => "no terminal",
            Self::NoLabs => "no labs",
            Self::FewLabs => "too few labs",
            Self::Reacting => "reacting",
            Self::Emptying => "emptying",
            Self::Clearing => "clearing",
            Self::Idle => "idle",
        }
    }
}

/// A lab hauler of this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub handle: CreepHandle,
    pub serial: u64,
    pub row: usize,
    pub at: Position,
    pub spawning: bool,
    pub store: CreepStore,
}

/// One task of an empty lab hauler ([`LabRoom::task`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    /// Withdraw all of lab `0`'s mineral (an index into [`LabRoom::labs`]).
    Unload(usize),
    /// Withdraw this much of source `0`'s reagent from the terminal (0 or 1).
    Load(usize, u32),
}

/// One room's counters for the console.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LabStats {
    pub reads: u32,
    pub requests: u32,
    pub jobs: u32,
    pub reactions: u32,
    pub refused: u32,
    pub withdraws: u32,
    pub transfers: u32,
}

/// One `StructureLab.runReaction(lab1, lab2)` call for the act step (V403, V408).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReactionOrder {
    pub room: RoomName,
    pub lab: StructureHandle,
    /// The output's and the two sources' rows in this tick's structures (their JS objects).
    pub lab_row: u32,
    pub source_rows: [u32; 2],
    pub product: ResourceType,
}

/// A change in a room's jobs, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabEvent {
    /// A job started: its step.
    Started { room: RoomName, step: Step },
    /// A job's labs were emptied: its product and the reactions ordered for it.
    Done {
        room: RoomName,
        product: ResourceType,
        reactions: u32,
    },
}

/// One room the module tracks.
#[derive(Clone, Debug)]
pub struct LabRoom {
    pub name: RoomName,
    /// Whether this tick read the room.
    pub read: bool,
    /// The last read: its terminal and stock.
    pub seen: Option<LabRead>,
    pub seen_time: u32,
    /// The last read's labs, by tile index.
    pub labs: Vec<LabState>,
    /// The source labs (indices into [`Self::labs`]), and the outputs.
    pub sources: Option<[usize; 2]>,
    pub outputs: Vec<usize>,
    /// The tiles the roles were chosen for, and the chosen sources' tiles.
    role_tiles: Vec<Tile>,
    source_tiles: Option<[Tile; 2]>,
    pub job: Option<Job>,
    pub why: Why,
    /// Whether the room wants labs built ([`Labs::wants_labs`]).
    pub wants: bool,
    /// The room's energy capacity at the last plan. Unit: energy.
    pub capacity: u32,
    pub haulers: Vec<Member>,
    /// Whether the hauler had work at the last work run (a task or a load).
    pub busy: bool,
    /// The tick the first output's cooldown ends, while a job runs.
    pub next_react: Option<u32>,
    /// No reaction until this tick (after a refused one).
    pub held_until: u32,
    /// A job already loaded may be adopted (until the room's first read is planned).
    fresh: bool,
    seen_tick: u32,
    pub stats: LabStats,
}

/// The lab module's state for one global.
#[derive(Default)]
pub struct Labs {
    time: u32,
    rooms: Vec<LabRoom>,
    events: Vec<LabEvent>,
    orders: Vec<ReactionOrder>,
    reads: Vec<RoomName>,
    wanted: Vec<RoomName>,
    upkeeps: Vec<(RoomName, u64)>,
    /// Whether `labs.enabled` was on at `begin`.
    enabled: bool,
}

impl Labs {
    /// Starts the tick (module docs, step 1).
    pub fn begin(&mut self, snap: &Snapshot, cfg: &LabsConfig) {
        self.time = snap.tick.time;
        self.enabled = cfg.enabled;
        self.events.clear();
        self.orders.clear();
        self.upkeeps.clear();
        let now = self.time;
        let st = &snap.structures;
        for (&kind, pos) in st.kinds().iter().zip(st.positions()) {
            if matches!(kind, StructureType::Terminal | StructureType::Lab) {
                self.track(pos.room_name(), now);
            }
        }
        for room in &mut self.rooms {
            room.haulers.clear();
            room.read = false;
        }
        let creeps = &snap.creeps;
        for (row, (id, (&mine, &h))) in creeps
            .identities()
            .iter()
            .zip(creeps.my().iter().zip(creeps.handles()))
            .enumerate()
        {
            let Some(id) = id.filter(|id| id.role == Role::LabHauler && mine) else {
                continue;
            };
            let Some(&at) = creeps.positions().get(row) else {
                continue;
            };
            let spawning = creeps.spawning().get(row).copied().unwrap_or(true);
            let store = creeps.stores().get(row).copied().unwrap_or_default();
            let ri = self.track(id.home, now);
            if let Some(room) = self.rooms.get_mut(ri) {
                room.haulers.push(Member {
                    handle: h,
                    serial: id.serial,
                    row,
                    at,
                    spawning,
                    store,
                });
            }
        }
        // A room dropped (no terminal, lab or hauler) costs the upgraders nothing more.
        for r in &self.rooms {
            if r.seen_tick != now {
                self.upkeeps.push((r.name, 0));
            }
        }
        self.rooms.retain(|r| r.seen_tick == now);
        for room in &mut self.rooms {
            room.haulers.sort_by_key(|m| m.serial);
            if let Some(read) = snap.labs.get(room.name) {
                room.read = true;
                room.seen = Some(*read);
                room.seen_time = now;
                room.stats.reads = room.stats.reads.saturating_add(1);
                room.labs.clear();
                room.labs.extend(snap.labs.labs(room.name).copied());
                room.labs.sort_by_key(|l| Tile::of(l.at).index());
                // Here, not in the plan: a budget-cut Critical job must not leave the work with
                // roles indexing the last read's labs.
                room.update_roles();
            }
        }
    }

    /// The index of `room`, added when missing; marks it tracked this tick.
    fn track(&mut self, room: RoomName, now: u32) -> usize {
        let key = room.packed_repr();
        match self
            .rooms
            .binary_search_by_key(&key, |r| r.name.packed_repr())
        {
            Ok(i) => {
                if let Some(r) = self.rooms.get_mut(i) {
                    r.seen_tick = now;
                }
                i
            }
            Err(i) => {
                self.rooms.insert(
                    i,
                    LabRoom {
                        name: room,
                        read: false,
                        seen: None,
                        seen_time: 0,
                        labs: Vec::new(),
                        sources: None,
                        outputs: Vec::new(),
                        role_tiles: Vec::new(),
                        source_tiles: None,
                        job: None,
                        why: Why::default(),
                        wants: false,
                        capacity: 0,
                        haulers: Vec::new(),
                        busy: false,
                        next_react: None,
                        held_until: 0,
                        fresh: true,
                        seen_tick: now,
                        stats: LabStats::default(),
                    },
                );
                i
            }
        }
    }

    /// The rooms, by name.
    pub fn rooms(&self) -> &[LabRoom] {
        &self.rooms
    }

    /// The Critical job's units: one a room.
    pub fn units(&self) -> u32 {
        u32::try_from(self.rooms.len()).unwrap_or(u32::MAX)
    }

    /// This tick's job starts and ends.
    pub fn events(&self) -> &[LabEvent] {
        &self.events
    }

    /// This tick's reactions, for the act step.
    pub fn orders(&self) -> &[ReactionOrder] {
        &self.orders
    }

    /// Each room's lab hauler body over a creep life while its labs have work, 0 while they have
    /// none, from this tick's plan runs and the rooms dropped at `begin`, for
    /// `economy::Upgrading::set_labs`.
    pub fn upkeeps(&self) -> &[(RoomName, u64)] {
        &self.upkeeps
    }

    /// The rooms that want labs built (module docs), sorted by packed name, from the last plan
    /// runs: `economy::placement::Placement::set_lab_rooms` takes them.
    pub fn wants_labs(&mut self) -> &[RoomName] {
        self.wanted.clear();
        self.wanted
            .extend(self.rooms.iter().filter(|r| r.wants).map(|r| r.name));
        &self.wanted
    }

    /// The Critical half of unit `unit`'s tick (module docs, step 2): on a read tick the job's
    /// phase (or a new job, or an adopted one after a reset) and whether the room wants labs; the
    /// upkeep; and the hauler's request while `standard` (the Standard tier runs this tick).
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        spawner: &mut Spawner,
        cfg: &LabsConfig,
        standard: bool,
    ) -> Result<(), Fault> {
        let ri = usize::try_from(unit).map_err(|_| Fault::new("unit out of range", unit))?;
        let r = self
            .rooms
            .get_mut(ri)
            .ok_or(Fault::new("no room for the unit", unit))?;
        let name = r.name;
        if let Some(row) = snap.rooms.row_of(name)
            && let Some(e) = snap.rooms.energy().get(row)
        {
            r.capacity = e.capacity;
        }
        if r.read {
            if let Some(e) = r.advance(cfg) {
                self.events.push(e);
            }
            // Whether the room wants labs changes only with what a read sees.
            r.wants = cfg.enabled
                && (!r.labs.is_empty()
                    || r.seen
                        .is_some_and(|s| s.terminal.is_some() && pick(&s.stock, cfg).is_some()));
        }
        if !cfg.enabled {
            r.wants = false;
        }
        r.why = r.why_now(cfg);
        let work = cfg.enabled && r.has_work();
        let body = work
            .then(|| hauler_body(r.capacity, cfg.hauler_carry))
            .flatten();
        let upkeep = body.map_or(0, |b| u64::from(b.cost()));
        self.upkeeps.push((name, upkeep));
        if standard
            && work
            && r.haulers.is_empty()
            && let Some(body) = body
            && spawner
                .request(SpawnRequest {
                    home: name,
                    role: Role::LabHauler,
                    remote: None,
                    body: BodySpec::Exact(body),
                    budget: EnergyBudget::Capacity,
                    priority: PRIORITY_LAB_HAULER,
                })
                .is_some()
        {
            r.stats.requests = r.stats.requests.saturating_add(1);
        }
        Ok(())
    }

    /// Whether the Standard work has a room to work: one read this tick with a hauler out of the
    /// spawn or a job running.
    pub fn has_work(&self) -> bool {
        self.rooms.iter().any(|r| {
            r.read
                && (r.haulers.iter().any(|m| !m.spawning)
                    || r.job.is_some_and(|j| j.phase == Phase::Run))
        })
    }

    /// The Standard half of unit `unit`'s tick (module docs, step 3), in a room read this tick:
    /// the reactions while a job runs and `labs.enabled` is on, then the oldest hauler's move,
    /// withdraw or transfer. Call the units in order, one a room ([`Self::units`]).
    #[allow(clippy::too_many_arguments)]
    pub fn work_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &LabsConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let now = self.time;
        let ri = usize::try_from(unit).map_err(|_| Fault::new("unit out of range", unit))?;
        let r = self
            .rooms
            .get_mut(ri)
            .ok_or(Fault::new("no room for the unit", unit))?;
        if !r.read {
            return Ok(());
        }
        if cfg.enabled {
            r.react(now, &mut self.orders, commit);
        }
        let Some(h) = r.haulers.first().copied().filter(|m| !m.spawning) else {
            r.busy = false;
            return Ok(());
        };
        let Some(seen) = r.seen else {
            r.busy = false;
            return Ok(());
        };
        let Some(terminal) = seen.terminal else {
            r.busy = false;
            return Ok(());
        };
        let store = h.store;
        let here = Tile::of(h.at);
        let near =
            |at: Position| at.room_name() == h.at.room_name() && here.range(Tile::of(at)) <= NEAR;
        // A load first: energy (never taken, but delivered if held) or its one mineral.
        if store.energy > 0 || store.other > 0 {
            // A load it cannot deliver (no type to name, or a full terminal) keeps no read going
            // (the hot-path review, 2026-11-26).
            r.busy = r.deliverable(&store);
            let (kind, amount) = if store.energy > 0 {
                (ResourceType::Energy, store.energy)
            } else {
                match store.other_kind {
                    Some(k) => (k, store.other),
                    // A type no lab handles: nothing to name it by.
                    None => return Ok(()),
                }
            };
            let dest = if cfg.enabled { r.delivery(kind) } else { None };
            let (target, at, to_lab) = match dest {
                Some(i) => match r.source_lab(i) {
                    Some(l) => (l.handle, l.at, Some((i, l))),
                    None => (terminal.handle, terminal.at, None),
                },
                None => (terminal.handle, terminal.at, None),
            };
            if !near(at) {
                go(snap, movement, h.handle, at, mcfg, commit);
                return Ok(());
            }
            if to_lab.is_none() && terminal.free == 0 {
                return Ok(());
            }
            if buffer
                .push(
                    snap,
                    h.handle,
                    CreepIntent::Transfer {
                        target: Target::Structure(target),
                        resource: kind,
                        amount: None,
                    },
                    commit,
                )
                .is_ok()
            {
                r.stats.transfers = r.stats.transfers.saturating_add(1);
                if let Some((i, l)) = to_lab
                    && let Some(job) = r.job.as_mut()
                    && let Some(loaded) = job.loaded.get_mut(i)
                {
                    let fits = LAB_MINERAL_CAPACITY.saturating_sub(l.amount);
                    *loaded = loaded.saturating_add(amount.min(fits));
                }
            }
            return Ok(());
        }
        let task = if cfg.enabled {
            r.task(store.free_all())
        } else {
            None
        };
        r.busy = task.is_some();
        let Some(task) = task else {
            return Ok(());
        };
        let (target, at, resource, amount) = match task {
            Task::Unload(li) => {
                let Some(l) = r.labs.get(li) else {
                    return Ok(());
                };
                let Some(kind) = l.kind else {
                    return Ok(());
                };
                (l.handle, l.at, kind, None)
            }
            Task::Load(i, amount) => {
                let Some(&kind) = r.job.as_ref().and_then(|j| j.reagents.get(i)) else {
                    return Ok(());
                };
                (terminal.handle, terminal.at, kind, Some(amount))
            }
        };
        if !near(at) {
            go(snap, movement, h.handle, at, mcfg, commit);
            return Ok(());
        }
        if buffer
            .push(
                snap,
                h.handle,
                CreepIntent::Withdraw {
                    target,
                    resource,
                    amount,
                },
                commit,
            )
            .is_ok()
        {
            r.stats.withdraws = r.stats.withdraws.saturating_add(1);
        }
        Ok(())
    }

    /// Records the act step's result for order `index` ([`Self::orders`]): a call that was not OK
    /// holds the room's reactions for `labs.read_ticks` (module docs). Returns whether it was
    /// held.
    pub fn record_reaction(&mut self, index: usize, outcome: Outcome, cfg: &LabsConfig) -> bool {
        if outcome == Outcome::Ok {
            return false;
        }
        let Some(order) = self.orders.get(index).copied() else {
            return false;
        };
        let until = self.time.saturating_add(cfg.read_ticks.max(1));
        match self.rooms.iter_mut().find(|r| r.name == order.room) {
            Some(r) => {
                r.stats.refused = r.stats.refused.saturating_add(1);
                let held = r.held_until <= self.time;
                r.held_until = until;
                r.next_react = Some(until);
                held
            }
            None => false,
        }
    }

    /// The rooms the next tick's snapshot reads (module docs), sorted by packed name: while
    /// `standard` (the Standard tier ran this tick, so the hauler's work and the reactions could
    /// run), those whose hauler is out of the spawn with work ([`LabRoom::can_act`]) or whose
    /// reaction is due; and, while `labs.enabled` is on, those whose `labs.read_ticks` read is
    /// due.
    pub fn reads(&mut self, cfg: &LabsConfig, standard: bool) -> &[RoomName] {
        let next = self.time.saturating_add(1);
        self.reads.clear();
        for r in &self.rooms {
            let cadence =
                r.seen.is_none() || next >= r.seen_time.saturating_add(cfg.read_ticks.max(1));
            // A reaction is ordered only in the Standard work, so its deadline asks for a read only
            // while that tier runs (the hot-path review, 2026-11-26: off it, the deadline stayed due
            // and the room was read every tick for nothing).
            let react = cfg.enabled && r.next_react.is_some_and(|t| next >= t);
            if (standard && (r.can_act(cfg.enabled) || react)) || (cfg.enabled && cadence) {
                self.reads.push(r.name);
            }
        }
        &self.reads
    }
}

impl LabRoom {
    /// Chooses the lab roles again when the read's labs stand on other tiles than the roles were
    /// chosen for ([`source_pair`]). A job keeps its source labs while both stand, so a level's
    /// new labs change the roles only once it ends (the labs on it become outputs when in range
    /// of both); a job whose source lab is gone is emptied.
    fn update_roles(&mut self) {
        let same = self.role_tiles.len() == self.labs.len()
            && self
                .role_tiles
                .iter()
                .zip(&self.labs)
                .all(|(&t, l)| t == Tile::of(l.at));
        if same {
            return;
        }
        self.role_tiles.clear();
        self.role_tiles
            .extend(self.labs.iter().map(|l| Tile::of(l.at)));
        let kept = self
            .source_tiles
            .and_then(|t| self.index_pair(t))
            .filter(|_| self.job.is_some());
        match kept {
            Some(pair) => self.set_roles(Some(pair)),
            None => {
                self.set_roles(source_pair(&self.role_tiles, self.source_tiles));
                if let Some(job) = self.job.as_mut() {
                    job.phase = Phase::Empty;
                }
            }
        }
    }

    /// The indices of the labs standing on `tiles`, while both stand.
    fn index_pair(&self, tiles: [Tile; 2]) -> Option<[usize; 2]> {
        let at = |t: Tile| self.role_tiles.iter().position(|&x| x == t);
        Some([at(tiles[0])?, at(tiles[1])?])
    }

    /// Makes `pair` (indices into [`Self::labs`]) the source labs and every other lab in range of
    /// both an output; `None` leaves the room with no roles.
    fn set_roles(&mut self, pair: Option<[usize; 2]>) {
        let tiles =
            pair.and_then(|[i, j]| Some([*self.role_tiles.get(i)?, *self.role_tiles.get(j)?]));
        self.outputs.clear();
        if let (Some([i, j]), Some([a, b])) = (pair, tiles) {
            let near = |x: Tile, y: Tile| u32::from(x.range(y)) <= LAB_REACTION_RANGE;
            self.outputs.extend(
                self.role_tiles
                    .iter()
                    .enumerate()
                    .filter(|&(k, &c)| k != i && k != j && near(a, c) && near(b, c))
                    .map(|(k, _)| k),
            );
        }
        self.sources = pair.filter(|_| tiles.is_some());
        self.source_tiles = tiles;
    }

    /// Source lab `i` (0 or 1) of the current roles.
    pub fn source_lab(&self, i: usize) -> Option<LabState> {
        let li = *self.sources?.get(i)?;
        self.labs.get(li).copied()
    }

    /// Source `i`'s stock of the job's reagent `i` (0 when it holds another mineral or none).
    fn source_amount(&self, i: usize) -> u32 {
        let (Some(job), Some(l)) = (self.job, self.source_lab(i)) else {
            return 0;
        };
        match (l.kind, job.reagents.get(i)) {
            (Some(k), Some(&r)) if k == r => l.amount,
            _ => 0,
        }
    }

    /// How much more of reagent `i` its source lab takes now while the job runs: none once the
    /// job's amount is loaded, the terminal holds none, or the lab holds another mineral (V411).
    /// Unit: units.
    fn load_room(&self, i: usize) -> u32 {
        let (Some(job), Some(l), Some(seen)) = (self.job, self.source_lab(i), self.seen) else {
            return 0;
        };
        let (Some(&reagent), Some(&loaded)) = (job.reagents.get(i), job.loaded.get(i)) else {
            return 0;
        };
        if job.phase != Phase::Run || l.kind.is_some_and(|k| k != reagent) {
            return 0;
        }
        job.amount
            .saturating_sub(loaded)
            .min(seen.stock_of(reagent))
            .min(LAB_MINERAL_CAPACITY.saturating_sub(l.amount))
    }

    /// Whether a lab hauler of the room carries some of `kind`.
    fn carrying(&self, kind: ResourceType) -> bool {
        self.haulers
            .iter()
            .any(|h| h.store.other > 0 && h.store.other_kind == Some(kind))
    }

    /// The job's next phase from this tick's read, a new job, or an adopted one (module docs).
    /// Returns the event it makes, if any.
    fn advance(&mut self, cfg: &LabsConfig) -> Option<LabEvent> {
        let fresh = std::mem::replace(&mut self.fresh, false);
        match self.job {
            Some(job) if job.phase == Phase::Run => {
                // A reagent the hauler carries is still on its way (the terminal may hold none of it
                // then, after a trip that took the whole stock).
                let stops = (0..2).any(|i| {
                    self.source_amount(i) < LAB_REACTION_AMOUNT
                        && self.load_room(i) == 0
                        && !job.reagents.get(i).is_some_and(|&r| self.carrying(r))
                });
                if stops && let Some(j) = self.job.as_mut() {
                    j.phase = Phase::Empty;
                    self.next_react = None;
                }
                None
            }
            Some(job) => {
                if self.labs.iter().all(|l| l.kind.is_none() || l.amount == 0) {
                    self.job = None;
                    // The roles are chosen again at the next read (a level's new labs).
                    self.role_tiles.clear();
                    return Some(LabEvent::Done {
                        room: self.name,
                        product: job.product,
                        reactions: job.reactions,
                    });
                }
                None
            }
            None => {
                if fresh && let Some((pair, job)) = self.adoptable() {
                    self.set_roles(Some(pair));
                    self.job = Some(job);
                    return None;
                }
                let holding = self.labs.iter().any(|l| l.kind.is_some() && l.amount > 0);
                // A load still in a hauler is not in the terminal's stock yet: wait for it.
                let loaded = self
                    .haulers
                    .iter()
                    .any(|h| h.store.other > 0 || h.store.energy > 0);
                let ready = cfg.enabled
                    && !holding
                    && !loaded
                    && self.sources.is_some()
                    && !self.outputs.is_empty()
                    && self.seen.is_some_and(|s| s.terminal.is_some());
                if !ready {
                    return None;
                }
                let step = pick(&self.seen?.stock, cfg)?;
                self.job = Some(Job {
                    product: step.product,
                    reagents: step.reagents,
                    amount: step.amount,
                    loaded: [0, 0],
                    reactions: 0,
                    phase: Phase::Run,
                });
                self.stats.jobs = self.stats.jobs.saturating_add(1);
                Some(LabEvent::Started {
                    room: self.name,
                    step,
                })
            }
        }
    }

    /// The job two labs already hold after a reset, and those labs as the sources: the first pair
    /// (in lab order) whose minerals react, each at least `LAB_REACTION_AMOUNT`, with another lab
    /// in range of both to react them in; nothing more is loaded. The pair need not be the one
    /// [`source_pair`] would choose (with three labs every pair ties), so a job loaded before the
    /// reset goes on in the same labs.
    ///
    /// Uses: `LAB_REACTION_AMOUNT`, `LAB_REACTION_RANGE`, `REACTIONS` ([`product_of`]).
    fn adoptable(&self) -> Option<([usize; 2], Job)> {
        let near = |x: Tile, y: Tile| u32::from(x.range(y)) <= LAB_REACTION_RANGE;
        for (i, a) in self.labs.iter().enumerate() {
            for (j, b) in self.labs.iter().enumerate().skip(i.saturating_add(1)) {
                let (Some(ka), Some(kb)) = (a.kind, b.kind) else {
                    continue;
                };
                if a.amount < LAB_REACTION_AMOUNT || b.amount < LAB_REACTION_AMOUNT {
                    continue;
                }
                let Some(product) = product_of(ka, kb) else {
                    continue;
                };
                let (ta, tb) = (Tile::of(a.at), Tile::of(b.at));
                let output = self.labs.iter().enumerate().any(|(k, c)| {
                    let t = Tile::of(c.at);
                    k != i && k != j && near(ta, t) && near(tb, t)
                });
                if !output {
                    continue;
                }
                let amount = a.amount.max(b.amount);
                return Some((
                    [i, j],
                    Job {
                        product,
                        reagents: [ka, kb],
                        amount,
                        loaded: [amount, amount],
                        reactions: 0,
                        phase: Phase::Run,
                    },
                ));
            }
        }
        None
    }

    /// What the room's labs do now (module docs).
    fn why_now(&self, cfg: &LabsConfig) -> Why {
        if !cfg.enabled {
            return Why::Off;
        }
        let Some(seen) = self.seen else {
            return Why::Unread;
        };
        if seen.terminal.is_none() {
            return Why::NoTerminal;
        }
        if self.labs.is_empty() {
            return Why::NoLabs;
        }
        if self.sources.is_none() || self.outputs.is_empty() {
            return Why::FewLabs;
        }
        match self.job.map(|j| j.phase) {
            Some(Phase::Run) => Why::Reacting,
            Some(Phase::Empty) => Why::Emptying,
            None if self.labs.iter().any(|l| l.kind.is_some() && l.amount > 0) => Why::Clearing,
            None => Why::Idle,
        }
    }

    /// Whether lab `li` holds a mineral its role should not (module docs): any while no job runs
    /// or the job empties; another than its reagent in a source, than the product in an output;
    /// any in a lab of no role.
    fn wrong(&self, li: usize) -> bool {
        let Some(l) = self.labs.get(li) else {
            return false;
        };
        let Some(kind) = l.kind.filter(|_| l.amount > 0) else {
            return false;
        };
        let Some(job) = self.job.filter(|j| j.phase == Phase::Run) else {
            return true;
        };
        match self.sources {
            Some([a, _]) if a == li => kind != job.reagents[0],
            Some([_, b]) if b == li => kind != job.reagents[1],
            _ if self.outputs.contains(&li) => kind != job.product,
            _ => true,
        }
    }

    /// The task of an empty hauler with `free` room (module docs), in order: a lab holding a
    /// mineral its role should not, an output that cannot take another reaction, then the source
    /// with less loaded that can take more. `None` with no terminal, no room in the hauler, or
    /// nothing to do.
    pub fn task(&self, free: u32) -> Option<Task> {
        let terminal = self.seen.and_then(|s| s.terminal)?;
        if free == 0 {
            return None;
        }
        if terminal.free > 0 {
            if let Some(li) = (0..self.labs.len()).find(|&li| self.wrong(li)) {
                return Some(Task::Unload(li));
            }
            let full = self.outputs.iter().copied().find(|&li| {
                self.labs.get(li).is_some_and(|l| {
                    l.kind.is_some() && l.amount > LAB_MINERAL_CAPACITY - LAB_REACTION_AMOUNT
                })
            });
            if let Some(li) = full {
                return Some(Task::Unload(li));
            }
        }
        let job = self.job?;
        (0..2)
            .filter(|&i| self.load_room(i) > 0)
            .min_by_key(|&i| job.loaded.get(i).copied().unwrap_or(0))
            .map(|i| Task::Load(i, self.load_room(i).min(free)))
    }

    /// The source a hauler's load of `kind` goes to: the source whose reagent it is, while the job
    /// runs, still has some to load, and the lab can take it; `None` sends it to the terminal.
    fn delivery(&self, kind: ResourceType) -> Option<usize> {
        let job = self.job.filter(|j| j.phase == Phase::Run)?;
        (0..2).find(|&i| {
            job.reagents.get(i) == Some(&kind)
                && job.loaded.get(i).is_some_and(|&l| l < job.amount)
                && self.source_lab(i).is_some_and(|l| {
                    l.kind.is_none_or(|k| k == kind) && l.amount < LAB_MINERAL_CAPACITY
                })
        })
    }

    /// Whether the room's labs have work for a hauler (module docs): a load it can deliver
    /// ([`Self::deliverable`]), or a task for an empty one at a full body's room.
    pub fn has_work(&self) -> bool {
        self.haulers.iter().any(|h| self.deliverable(&h.store))
            || self.task(CARRY_CAPACITY).is_some()
    }

    /// Whether a hauler holding `store` can deliver its load now: energy or a mineral of a known
    /// type, to the source that needs it ([`Self::delivery`]) or to a terminal with room.
    fn deliverable(&self, store: &CreepStore) -> bool {
        let terminal_room = self
            .seen
            .and_then(|s| s.terminal)
            .is_some_and(|t| t.free > 0);
        if store.energy > 0 {
            return terminal_room;
        }
        store.other > 0
            && store
                .other_kind
                .is_some_and(|k| terminal_room || self.delivery(k).is_some())
    }

    /// The tick the first output's reaction is due while a job runs (module docs).
    pub fn reaction_due(&self) -> Option<u32> {
        self.next_react
    }

    /// Whether the hauler, out of the spawn, may act on the next tick: it carries something, it
    /// had a task at the last work run, or, while `enabled` (`labs.enabled`), the last read leaves
    /// it one ([`Self::has_work`]; a hauler just out of the spawn). An idle one is read on the
    /// cadence instead.
    pub fn can_act(&self, enabled: bool) -> bool {
        self.haulers.iter().any(|h| !h.spawning) && (self.busy || (enabled && self.has_work()))
    }

    /// Orders this tick's reactions (module docs): every output with its cooldown at 0, empty or
    /// holding the product with room for another, while both sources hold `LAB_REACTION_AMOUNT`
    /// of their reagents (counted down per order, V405); then the tick the next is due.
    fn react(&mut self, now: u32, orders: &mut Vec<ReactionOrder>, commit: &mut Commit) {
        let Some(job) = self.job.filter(|j| j.phase == Phase::Run) else {
            self.next_react = None;
            return;
        };
        if now < self.held_until {
            return;
        }
        let (Some(a), Some(b)) = (self.source_lab(0), self.source_lab(1)) else {
            return;
        };
        let mut left = [self.source_amount(0), self.source_amount(1)];
        let period = job.product.reaction_time().unwrap_or(1).max(1);
        let mut next: Option<u32> = None;
        let mut ordered = 0u32;
        for &li in &self.outputs {
            let Some(o) = self.labs.get(li) else {
                continue;
            };
            let fits = o.kind.is_none_or(|k| k == job.product)
                && o.amount.saturating_add(LAB_REACTION_AMOUNT) <= LAB_MINERAL_CAPACITY;
            if !fits {
                continue;
            }
            if o.cooldown > 0 {
                let at = now.saturating_add(o.cooldown);
                next = Some(next.map_or(at, |n| n.min(at)));
                continue;
            }
            if left.iter().any(|&l| l < LAB_REACTION_AMOUNT) {
                continue;
            }
            for l in &mut left {
                *l -= LAB_REACTION_AMOUNT;
            }
            orders.push(ReactionOrder {
                room: self.name,
                lab: o.handle,
                lab_row: o.row,
                source_rows: [a.row, b.row],
                product: job.product,
            });
            commit.intents(1);
            ordered = ordered.saturating_add(1);
            let at = now.saturating_add(period);
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        self.next_react = next;
        if let Some(j) = self.job.as_mut() {
            j.reactions = j.reactions.saturating_add(ordered);
        }
        self.stats.reactions = self.stats.reactions.saturating_add(ordered);
    }
}

/// A move for `creep` to next to `target`, in the Standard tier; refusals are left to movement
/// (it reports them in its own counters).
fn go(
    snap: &Snapshot,
    movement: &mut Movement,
    creep: CreepHandle,
    target: Position,
    mcfg: &MovementConfig,
    commit: &mut Commit,
) {
    let to = MoveTo {
        target,
        range: NEAR,
        priority: HAULER_MOVE_PRIORITY,
        tier: Tier::Standard,
    };
    let _ = movement.request(snap, creep, to, mcfg, commit);
}
