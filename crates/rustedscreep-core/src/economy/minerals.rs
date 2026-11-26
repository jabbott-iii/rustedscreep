//! Mineral mining (ROADMAP P9.1, ADR-044): each owned room with an own extractor on its mineral,
//! a container next to the mineral and an own terminal mines the mineral into the terminal, while
//! the terminal holds less than `minerals.stock_max` of it (the user's three choices: the terminal
//! is the sink, mining runs under a stock cap, and a dedicated hauler carries).
//!
//! - **The miner** ([`Role::MineralMiner`]): WORK and MOVE parts, no CARRY. It stands on the
//!   container next to the mineral, so each harvest overflows into it (`drop` through
//!   `_create-energy`, which fills a container on the creep's tile first, V387, V193). It
//!   harvests only when the extractor's `cooldown` reads 0 (V385), the mineral holds an amount,
//!   and the container has room for a whole harvest, so nothing piles on the ground. A harvest
//!   yields `HARVEST_MINERAL_POWER` a WORK part (V387) and sets the extractor's cooldown to
//!   `EXTRACTOR_COOLDOWN`, which the engine counts down from the next tick (V388, V389), so one
//!   mineral gives a harvest every [`HARVEST_PERIOD`] ticks. One miner a room; no pre-spawn
//!   replacement (P5.2): the gap between two miners only delays the mineral's end, and with it
//!   its regeneration `MINERAL_REGEN_TIME` later (V390), by about 1% of the long-run yield.
//! - **The haulers** ([`Role::MineralHauler`]): CARRY and MOVE parts, sized by the source
//!   haulers' solver (`sizing::hauler_plan`) to the miner's rate over a round between the
//!   container and the terminal ([`rate_capacity`] turns the rate into the energy a source would
//!   yield in `ENERGY_REGEN_TIME`, the solver's unit). An empty hauler withdraws the mineral once
//!   the container holds a full load for it, or anything while the miner is not harvesting; a
//!   loaded one carries it to the terminal and transfers it all (V399). The haulers carry nothing
//!   else, so what the snapshot reads past their energy is the mineral (`CreepStore::other`).
//! - **When a room mines** ([`Why`]): `minerals.enabled`, a level that allows the extractor and
//!   the terminal (`CONTROLLER_STRUCTURES`, V393), an own extractor on the mineral, a container
//!   next to it, an own terminal, a mineral type this build knows, a mineral amount above 0, the
//!   terminal holding less than `minerals.stock_max` of the mineral and at least
//!   `minerals.free_min` free, and a plan (a spawn, and ways from it to the container and from the
//!   container to the terminal on movement's grid). While it mines, one miner is requested; the
//!   haulers the plan wants are requested while it mines, and one while a stopped room's
//!   container holds a hauler's load the terminal can take. The haulers wait for the miner: none is
//!   requested before one lives (spawning ones count), so the miner, ordered first, is not held up
//!   behind a hauler that would only wait at an empty container. Requests go out only on ticks the
//!   Standard tier runs, at [`PRIORITY_MINERAL_MINER`] and [`PRIORITY_MINERAL_HAULER`], below
//!   every economy and defense role.
//! - **The upgraders' budget** ([`Minerals::upkeeps`]): while a room mines, its miner's and
//!   haulers' bodies over a creep life, which `economy::Upgrading::set_minerals` leaves out of the
//!   upgraders' budget like every other creep's.
//!
//! The reads (the snapshot's `MineralSites`, about 14 JS calls a room): a room is read on the
//! tick after one that asks for it ([`Minerals::reads`]): every tick while a mineral creep of the
//! room out of the spawn can act ([`MineralRoom::can_act`]: the room mines, its container holds
//! some of the mineral, or a hauler carries some) and the Standard tier ran, else once
//! `minerals.read_ticks` have passed since its last read, or once the mineral's regeneration is
//! due (`ticksToRegeneration`, V391). Between reads the room's requests come from what the last
//! read saw; the creeps act only on read ticks.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Minerals::begin`] after the snapshot: the rooms (an own extractor, or a mineral creep's
//!    home), this tick's read of each, and each room's miners and haulers by serial.
//! 2. The Critical `minerals` job runs [`Minerals::plan_room`] per room, before the opening's job
//!    (which sizes the upgraders, [`Minerals::upkeeps`]) and the spawn plan: the room's plan
//!    (built again when its key changes: movement's grid version, the capacity, the spawn,
//!    container and terminal tiles, `minerals.max_work`, `hauling.margin_pct`), why it mines or
//!    not, and its requests.
//! 3. The Standard `minerals work` job runs [`Minerals::work_room`] per room read this tick that
//!    has creeps: the miner's move and harvest, each hauler's move, withdraw and transfer.
//! 4. [`Minerals::reads`]: the rooms the next tick's snapshot reads.
//!
//! The plan and the rooms are kept in the heap. A global reset rebuilds them from the next reads
//! (every room is due on its first tick); nothing is read from or written to `Memory`.

use rustedscreep_consts::{
    CARRY_CAPACITY, CONTAINER_CAPACITY, CREEP_LIFE_TIME, ENERGY_REGEN_TIME, EXTRACTOR_COOLDOWN,
    HARVEST_MINERAL_POWER, MAX_CREEP_SIZE, Part, Position, ResourceType, RoomName, StructureType,
};

use super::sizing::{Trip, hauler_plan, miner_body_work, walk_ticks};
use crate::config::{HaulingConfig, MineralsConfig, MovementConfig};
use crate::intents::{CreepIntent, IntentBuffer, Target};
use crate::movement::{Dial, FlowField, Goal, MoveTo, Movement, Tile, UNREACHED};
use crate::planner::rules::earliest_level;
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{Controller, CreepHandle, MineralSite, Snapshot};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner};

/// The spawn priority of a mineral miner: below every economy and defense role, and below the
/// signer (70), the explorer (72) and the claimer (78), so the room's energy goes to the mineral
/// only when nothing else waits for a spawn.
pub const PRIORITY_MINERAL_MINER: u8 = 65;

/// The spawn priority of a mineral hauler: just above the miner, so a filling container is
/// emptied before a second miner life starts.
pub const PRIORITY_MINERAL_HAULER: u8 = 66;

/// Ticks between two harvests of one mineral: a harvest sets the extractor's cooldown to
/// `EXTRACTOR_COOLDOWN`, which the objects' tick pass of the same tick does not count down
/// (V388, V389, inferred from the engine's order), so the cooldown reads 0 again
/// `EXTRACTOR_COOLDOWN + 1` ticks later. Unit: ticks.
///
/// Uses: `EXTRACTOR_COOLDOWN`.
pub const HARVEST_PERIOD: u32 = EXTRACTOR_COOLDOWN + 1;

/// How hard the miner's moves push past other creeps: above the haulers and the upgraders, so a
/// creep on the container tile makes way, below the source miners.
const MINER_MOVE_PRIORITY: u8 = 3;

/// How hard a mineral hauler's moves push past other creeps.
const HAULER_MOVE_PRIORITY: u8 = 2;

/// Harvesting, withdrawing and transferring reach 1 tile (`isNearTo`, V385, V399, V400).
const NEAR: u8 = 1;

/// What a miner of `work` WORK parts takes from its mineral in `ENERGY_REGEN_TIME` ticks: a
/// harvest of `HARVEST_MINERAL_POWER` a part every [`HARVEST_PERIOD`] ticks (V387, V389), rounded
/// up. The source sizing helpers take a source's capacity, its yield in `ENERGY_REGEN_TIME`
/// (V184), so this lets them size the mineral's creeps. Unit: units of the mineral.
///
/// Uses: `HARVEST_MINERAL_POWER`, `ENERGY_REGEN_TIME`, [`HARVEST_PERIOD`].
pub fn rate_capacity(work: u32) -> u32 {
    work.saturating_mul(HARVEST_MINERAL_POWER)
        .saturating_mul(ENERGY_REGEN_TIME)
        .div_ceil(HARVEST_PERIOD)
}

/// What a miner of `body` harvests in its life when its walk out takes `walk` ticks: the
/// harvests of its `CREEP_LIFE_TIME` less the walk (its life starts when it leaves the spawn,
/// V170), each `HARVEST_MINERAL_POWER` a WORK part. An upper bound: the mineral may run out first.
/// Unit: units of the mineral.
///
/// Uses: `CREEP_LIFE_TIME`, `HARVEST_MINERAL_POWER`, [`HARVEST_PERIOD`].
pub fn life_yield(work: u32, walk: u32) -> u64 {
    let harvests = CREEP_LIFE_TIME.saturating_sub(walk) / HARVEST_PERIOD;
    u64::from(harvests) * u64::from(work.saturating_mul(HARVEST_MINERAL_POWER))
}

/// The mineral miner's body for a room that holds at most `energy`, whose container is `trip`
/// from the spawn: of the bodies of 1 to `max_work` WORK parts (and at most
/// `MAX_CREEP_SIZE - 1`) with the MOVE parts `sizing::miner_body_work` solves against their walk
/// out, the one that harvests the most over its life ([`life_yield`]), ties to the cheaper. More
/// WORK parts harvest more a tick, but in a room of little energy they leave room for fewer MOVE
/// parts and walk out slower. `None` when no WORK and MOVE part fit `energy`.
///
/// Uses: `BODYPART_COST` (`Part::cost`), `MAX_CREEP_SIZE`, [`rate_capacity`], [`life_yield`],
/// `sizing::walk_ticks`.
pub fn miner_body(energy: u32, max_work: u32, trip: Trip) -> Option<Body> {
    let afford = energy.saturating_sub(Part::Move.cost()) / Part::Work.cost().max(1);
    let most = afford.min(max_work).min(MAX_CREEP_SIZE.saturating_sub(1));
    let mut best: Option<(u64, u32, Body)> = None;
    for work in 1..=most {
        let Some(body) = miner_body_work(work, 0, rate_capacity(work), trip, energy) else {
            continue;
        };
        let moves = u32::try_from(body.count(Part::Move)).unwrap_or(0);
        let Some(walk) = walk_ticks(trip, work, moves) else {
            continue;
        };
        let got = life_yield(work, walk);
        let cost = body.cost();
        if best
            .as_ref()
            .is_none_or(|&(y, c, _)| got > y || (got == y && cost < c))
        {
            best = Some((got, cost, body));
        }
    }
    best.map(|(_, _, b)| b)
}

/// The mineral haulers for a miner of `work` WORK parts whose loads travel `trip` from the
/// container to the terminal, in a room that holds at most `energy`: `sizing::hauler_plan` at
/// the miner's rate ([`rate_capacity`]), with a load handed over in one transfer (the terminal
/// takes any amount that fits, V399), so `CONTAINER_CAPACITY`, the most a hauler takes, is the
/// unload. A body and how many of it. `None` as for `hauler_plan`.
///
/// Uses: `CONTAINER_CAPACITY`, [`rate_capacity`], `sizing::hauler_plan`.
pub fn hauler_body(work: u32, trip: Trip, energy: u32, margin_pct: u32) -> Option<(Body, u32)> {
    hauler_plan(
        rate_capacity(work),
        trip,
        energy,
        margin_pct,
        CONTAINER_CAPACITY,
    )
}

/// Why a room mines or does not, from what its last read saw (module docs). The order is the
/// order of the checks, so a room shows the first that fails.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Why {
    /// It mines.
    Mining,
    /// `minerals.enabled` is off.
    Off,
    /// Not read yet in this global.
    #[default]
    Unread,
    /// Not owned, or its level allows no extractor or no terminal.
    Level,
    NoExtractor,
    NoContainer,
    NoTerminal,
    /// The mineral's type is not one this build lists.
    UnknownType,
    /// The mineral holds nothing: it regenerates `MINERAL_REGEN_TIME` after it ran out (V390).
    Regenerating,
    /// The terminal holds `minerals.stock_max` of the mineral or more.
    Stocked,
    /// The terminal has less than `minerals.free_min` free.
    TerminalFull,
    /// No plan: no spawn, no way to the container or from it to the terminal, or no body fits.
    NoPlan,
}

impl Why {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mining => "mining",
            Self::Off => "off",
            Self::Unread => "not read yet",
            Self::Level => "level too low",
            Self::NoExtractor => "no extractor",
            Self::NoContainer => "no container",
            Self::NoTerminal => "no terminal",
            Self::UnknownType => "unknown mineral",
            Self::Regenerating => "regenerating",
            Self::Stocked => "terminal stocked",
            Self::TerminalFull => "terminal full",
            Self::NoPlan => "no plan",
        }
    }
}

/// What a room's last read saw, kept for the ticks it is not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    pub time: u32,
    pub level: u8,
    pub capacity: u32,
    pub kind: Option<ResourceType>,
    pub mineral: Position,
    pub amount: u32,
    pub extractor: bool,
    pub container: Option<Position>,
    /// The container's stock of the mineral and everything in it. Unit: units.
    pub container_stock: u32,
    pub container_used: u32,
    pub terminal: Option<Position>,
    /// The terminal's stock of the mineral and its free room. Unit: units.
    pub terminal_stock: u32,
    pub terminal_free: u32,
}

/// A room's plan: the bodies and trips, built again when its key changes (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MineralPlan {
    /// The key: movement's grid version, the capacity, the spawn, container and terminal tiles,
    /// `minerals.max_work`, `hauling.margin_pct`.
    pub version: u32,
    pub capacity: u32,
    pub spawn: Position,
    pub container: Position,
    pub terminal: Position,
    pub max_work: u32,
    pub margin_pct: u32,
    /// The walk from the spawn to the container, and from next to the container to next to the
    /// terminal.
    pub miner_trip: Trip,
    pub haul_trip: Trip,
    pub miner: Option<Body>,
    pub hauler: Option<Body>,
    /// Haulers the plan wants while the room mines. Unit: creeps.
    pub haulers: u32,
}

impl MineralPlan {
    /// The miner's WORK parts.
    pub fn work(&self) -> u32 {
        self.miner
            .map_or(0, |b| u32::try_from(b.count(Part::Work)).unwrap_or(0))
    }

    /// What one hauler carries. Unit: units.
    pub fn load(&self) -> u32 {
        self.hauler.map_or(0, |b| {
            u32::try_from(b.count(Part::Carry))
                .unwrap_or(0)
                .saturating_mul(CARRY_CAPACITY)
        })
    }

    /// The bodies over one creep life while the room mines: the miner's and the wanted
    /// haulers'. Unit: energy.
    ///
    /// Uses: `BODYPART_COST` (`Body::cost`).
    pub fn upkeep(&self) -> u64 {
        let miner = self.miner.map_or(0, |b| u64::from(b.cost()));
        let haulers = self
            .hauler
            .map_or(0, |b| u64::from(b.cost()) * u64::from(self.haulers));
        miner.saturating_add(haulers)
    }
}

/// One mineral creep of this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub handle: CreepHandle,
    pub serial: u64,
    pub row: usize,
    pub at: Position,
    pub spawning: bool,
    /// A hauler that holds anything but energy at the tick's start (`CreepStore::other`).
    pub carrying: bool,
}

/// One room's counters for the console.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MineralStats {
    pub reads: u32,
    pub miner_requests: u32,
    pub hauler_requests: u32,
    pub harvests: u32,
    pub withdraws: u32,
    pub transfers: u32,
}

/// One room the module tracks.
#[derive(Clone, Debug)]
pub struct MineralRoom {
    pub name: RoomName,
    /// This tick's read, if the room was read.
    pub site: Option<MineralSite>,
    /// The last read's facts.
    pub seen: Option<Seen>,
    /// The tick the mineral regenerates, from the last read that saw it empty.
    pub regen_at: Option<u32>,
    pub plan: Option<MineralPlan>,
    pub why: Why,
    pub miners: Vec<Member>,
    pub haulers: Vec<Member>,
    /// The tick this room was last tracked (rooms not seen for a tick are dropped).
    seen_tick: u32,
    pub stats: MineralStats,
}

/// A change in a room's mining, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MineralEvent {
    /// The room started mining: its mineral and what it holds.
    Started {
        room: RoomName,
        kind: ResourceType,
        amount: u32,
    },
    /// The room stopped mining, and why.
    Stopped { room: RoomName, why: Why },
}

/// The mineral module's state for one global.
#[derive(Default)]
pub struct Minerals {
    time: u32,
    rooms: Vec<MineralRoom>,
    events: Vec<MineralEvent>,
    reads: Vec<RoomName>,
    upkeeps: Vec<(RoomName, u64)>,
    field: FlowField,
    dial: Dial,
    /// Whether `minerals.enabled` was on at `begin`.
    enabled: bool,
}

impl Minerals {
    /// Starts the tick (module docs, step 1).
    pub fn begin(&mut self, snap: &Snapshot, cfg: &MineralsConfig) {
        self.time = snap.tick.time;
        self.enabled = cfg.enabled;
        self.events.clear();
        self.upkeeps.clear();
        let now = self.time;
        // The rooms with an own extractor.
        let st = &snap.structures;
        for (&kind, pos) in st.kinds().iter().zip(st.positions()) {
            if kind == StructureType::Extractor {
                self.track(pos.room_name(), now);
            }
        }
        // The mineral creeps, by home.
        for room in &mut self.rooms {
            room.miners.clear();
            room.haulers.clear();
            room.site = None;
        }
        let creeps = &snap.creeps;
        for (row, (id, (&mine, &h))) in creeps
            .identities()
            .iter()
            .zip(creeps.my().iter().zip(creeps.handles()))
            .enumerate()
        {
            let Some(id) =
                id.filter(|id| matches!(id.role, Role::MineralMiner | Role::MineralHauler) && mine)
            else {
                continue;
            };
            let Some(&at) = creeps.positions().get(row) else {
                continue;
            };
            let spawning = creeps.spawning().get(row).copied().unwrap_or(true);
            let carrying = creeps.stores().get(row).is_some_and(|s| s.other > 0);
            let ri = self.track(id.home, now);
            let Some(room) = self.rooms.get_mut(ri) else {
                continue;
            };
            let m = Member {
                handle: h,
                serial: id.serial,
                row,
                at,
                spawning,
                carrying,
            };
            if id.role == Role::MineralMiner {
                room.miners.push(m);
            } else {
                room.haulers.push(m);
            }
        }
        // A room dropped (no own extractor, no mineral creep) costs the upgraders nothing more.
        for r in &self.rooms {
            if r.seen_tick != now {
                self.upkeeps.push((r.name, 0));
            }
        }
        self.rooms.retain(|r| r.seen_tick == now);
        for room in &mut self.rooms {
            room.miners.sort_by_key(|m| m.serial);
            room.haulers.sort_by_key(|m| m.serial);
            if let Some(site) = snap.minerals.get(room.name) {
                room.site = Some(*site);
                room.stats.reads = room.stats.reads.saturating_add(1);
                let row = snap.rooms.row_of(room.name);
                let level = row
                    .and_then(|r| snap.rooms.controllers().get(r).copied())
                    .and_then(|c| match c {
                        Controller::Mine { level } => Some(level),
                        _ => None,
                    })
                    .unwrap_or(0);
                let capacity = row
                    .and_then(|r| snap.rooms.energy().get(r))
                    .map_or(0, |e| e.capacity);
                room.regen_at = site.regen.map(|t| now.saturating_add(t));
                room.seen = Some(Seen {
                    time: now,
                    level,
                    capacity,
                    kind: site.kind,
                    mineral: site.at,
                    amount: site.amount,
                    extractor: site.extractor.is_some(),
                    container: site.container.map(|c| c.at),
                    container_stock: site.container.map_or(0, |c| c.stock),
                    container_used: site.container.map_or(0, |c| c.used),
                    terminal: site.terminal.map(|t| t.at),
                    terminal_stock: site.terminal.map_or(0, |t| t.stock),
                    terminal_free: site.terminal.map_or(0, |t| t.free),
                });
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
                    MineralRoom {
                        name: room,
                        site: None,
                        seen: None,
                        regen_at: None,
                        plan: None,
                        why: Why::default(),
                        miners: Vec::new(),
                        haulers: Vec::new(),
                        seen_tick: now,
                        stats: MineralStats::default(),
                    },
                );
                i
            }
        }
    }

    /// The rooms, by name.
    pub fn rooms(&self) -> &[MineralRoom] {
        &self.rooms
    }

    /// The Critical job's units: one a room.
    pub fn units(&self) -> u32 {
        u32::try_from(self.rooms.len()).unwrap_or(u32::MAX)
    }

    /// This tick's changes in the rooms' mining.
    pub fn events(&self) -> &[MineralEvent] {
        &self.events
    }

    /// Each room's mineral creeps' bodies over a creep life while it mines, 0 while it does not
    /// (module docs), from this tick's plan runs and the rooms dropped at `begin`, for
    /// `economy::Upgrading::set_minerals`.
    pub fn upkeeps(&self) -> &[(RoomName, u64)] {
        &self.upkeeps
    }

    /// The Critical half of unit `unit`'s tick (module docs, step 2): the plan, why the room mines
    /// or not, and its requests while `standard` (the Standard tier runs this tick). The plan is
    /// built only for a room that passes every check before it ([`gate`]), or whose container
    /// holds a load for a hauler.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        cfg: &MineralsConfig,
        hcfg: &HaulingConfig,
        mcfg: &MovementConfig,
        standard: bool,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let ri = usize::try_from(unit).map_err(|_| Fault::new("unit out of range", unit))?;
        let (name, seen, prior) = {
            let r = self
                .rooms
                .get(ri)
                .ok_or(Fault::new("no room for the unit", unit))?;
            (r.name, r.seen, r.why)
        };
        if let Some(seen) = seen
            && (gate(cfg, Some(&seen)) == Why::Mining || seen.container_stock > 0)
        {
            self.ensure_plan(ri, snap, &seen, movement, cfg, hcfg, mcfg, commit);
        }
        let r = self
            .rooms
            .get_mut(ri)
            .ok_or(Fault::new("no room for the unit", unit))?;
        let why = why(cfg, seen.as_ref(), r.plan.as_ref());
        r.why = why;
        if why != prior {
            match (why, seen.and_then(|s| s.kind)) {
                (Why::Mining, Some(kind)) => self.events.push(MineralEvent::Started {
                    room: name,
                    kind,
                    amount: seen.map_or(0, |s| s.amount),
                }),
                _ if prior == Why::Mining => {
                    self.events.push(MineralEvent::Stopped { room: name, why });
                }
                _ => {}
            }
        }
        let r = self
            .rooms
            .get_mut(ri)
            .ok_or(Fault::new("no room for the unit", unit))?;
        let mining = why == Why::Mining;
        let upkeep = if mining {
            r.plan.as_ref().map_or(0, MineralPlan::upkeep)
        } else {
            0
        };
        self.upkeeps.push((name, upkeep));
        if !(standard && cfg.enabled) {
            return Ok(());
        }
        let Some(plan) = r.plan else {
            return Ok(());
        };
        if mining
            && r.miners.is_empty()
            && let Some(body) = plan.miner
            && spawner
                .request(SpawnRequest {
                    home: name,
                    role: Role::MineralMiner,
                    remote: None,
                    body: BodySpec::Exact(body),
                    budget: EnergyBudget::Capacity,
                    priority: PRIORITY_MINERAL_MINER,
                })
                .is_some()
        {
            r.stats.miner_requests = r.stats.miner_requests.saturating_add(1);
        }
        let want = haulers_wanted(why, seen.as_ref(), &plan, r.miners.len());
        let have = u32::try_from(r.haulers.len()).unwrap_or(u32::MAX);
        if have < want
            && let Some(body) = plan.hauler
            && spawner
                .request(SpawnRequest {
                    home: name,
                    role: Role::MineralHauler,
                    remote: None,
                    body: BodySpec::Exact(body),
                    budget: EnergyBudget::Capacity,
                    priority: PRIORITY_MINERAL_HAULER,
                })
                .is_some()
        {
            r.stats.hauler_requests = r.stats.hauler_requests.saturating_add(1);
        }
        Ok(())
    }

    /// Builds room `ri`'s plan when it has none or its key changed (module docs); without a
    /// grid, a spawn, a container or a terminal it keeps none.
    #[allow(clippy::too_many_arguments)]
    fn ensure_plan(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        seen: &Seen,
        movement: &mut Movement,
        cfg: &MineralsConfig,
        hcfg: &HaulingConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) {
        let Some(name) = self.rooms.get(ri).map(|r| r.name) else {
            return;
        };
        let (Some(container), Some(terminal), Some(spawn)) =
            (seen.container, seen.terminal, first_spawn(snap, name))
        else {
            if let Some(r) = self.rooms.get_mut(ri) {
                r.plan = None;
            }
            return;
        };
        let Some((grid, version)) = movement.room_grid(name, mcfg, commit) else {
            return;
        };
        let current = self.rooms.get(ri).and_then(|r| r.plan);
        if current.is_some_and(|p| {
            p.version == version
                && p.capacity == seen.capacity
                && p.spawn == spawn
                && p.container == container
                && p.terminal == terminal
                && p.max_work == cfg.max_work
                && p.margin_pct == hcfg.margin_pct
        }) {
            return;
        }
        commit.heavy();
        let (c, t, s) = (Tile::of(container), Tile::of(terminal), Tile::of(spawn));
        // The miner's walk: from the spawn to onto the container.
        self.field.compute(
            grid,
            Goal {
                tile: s,
                range: NEAR,
            },
            &mut self.dial,
        );
        let from_spawn = self.field.distance(c);
        // The haulers' walk: from next to the container to next to the terminal.
        self.field.compute(
            grid,
            Goal {
                tile: t,
                range: NEAR,
            },
            &mut self.dial,
        );
        let to_terminal = c
            .neighbors()
            .map(|(_, n)| n)
            .filter(|&n| grid.walkable(n))
            .map(|n| self.field.distance(n))
            .min()
            .unwrap_or(UNREACHED);
        let plan = if from_spawn == UNREACHED || to_terminal == UNREACHED {
            None
        } else {
            let miner_trip = Trip {
                tiles: u32::from(c.range(s)).saturating_sub(u32::from(NEAR)),
                cost: u32::from(from_spawn),
            };
            let haul_trip = Trip {
                tiles: u32::from(c.range(t)).saturating_sub(2 * u32::from(NEAR)),
                cost: u32::from(to_terminal),
            };
            let miner = miner_body(seen.capacity, cfg.max_work, miner_trip);
            let work = miner.map_or(0, |b| u32::try_from(b.count(Part::Work)).unwrap_or(0));
            let haul = (work > 0)
                .then(|| hauler_body(work, haul_trip, seen.capacity, hcfg.margin_pct))
                .flatten();
            Some(MineralPlan {
                version,
                capacity: seen.capacity,
                spawn,
                container,
                terminal,
                max_work: cfg.max_work,
                margin_pct: hcfg.margin_pct,
                miner_trip,
                haul_trip,
                miner,
                hauler: haul.map(|h| h.0),
                haulers: haul.map_or(0, |h| h.1),
            })
        };
        if let Some(r) = self.rooms.get_mut(ri) {
            r.plan = plan;
        }
    }

    /// Whether the Standard work has a room to work: one read this tick with a creep out of the
    /// spawn.
    pub fn has_work(&self) -> bool {
        self.rooms
            .iter()
            .any(|r| r.site.is_some() && r.miners.iter().chain(&r.haulers).any(|m| !m.spawning))
    }

    /// The Standard half of unit `unit`'s tick (module docs, step 3): in a room read this tick,
    /// the miner's move and harvest and each hauler's move, withdraw and transfer. Call the units
    /// in order, one a room ([`Self::units`]).
    pub fn work_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let ri = usize::try_from(unit).map_err(|_| Fault::new("unit out of range", unit))?;
        let r = self
            .rooms
            .get_mut(ri)
            .ok_or(Fault::new("no room for the unit", unit))?;
        let Some(site) = r.site else {
            return Ok(());
        };
        let (Some(kind), Some(container)) = (site.kind, site.container) else {
            return Ok(());
        };
        let mining = r.why == Why::Mining;
        let mut harvesting = false;
        // The miner (the oldest; any other stands where it is until it dies).
        if let Some(m) = r.miners.first().copied().filter(|m| !m.spawning) {
            if m.at != container.at {
                go(
                    snap,
                    movement,
                    m.handle,
                    container.at,
                    0,
                    MINER_MOVE_PRIORITY,
                    mcfg,
                    commit,
                );
            } else if mining {
                harvesting = true;
                // The plan's WORK parts (the snapshot reads no own body); none known, no harvest.
                let work = r.plan.as_ref().map_or(0, MineralPlan::work);
                let harvest = work.saturating_mul(HARVEST_MINERAL_POWER).min(site.amount);
                let room_left = CONTAINER_CAPACITY.saturating_sub(container.used);
                let ready = site.extractor.is_some_and(|(_, cooldown)| cooldown == 0);
                if ready
                    && harvest > 0
                    && room_left >= harvest
                    && buffer
                        .push(
                            snap,
                            m.handle,
                            CreepIntent::Harvest {
                                target: site.handle,
                            },
                            commit,
                        )
                        .is_ok()
                {
                    r.stats.harvests = r.stats.harvests.saturating_add(1);
                }
            }
        }
        // The haulers, oldest first, sharing what the container holds this tick.
        let mut stock = container.stock;
        let stores = snap.creeps.stores();
        for i in 0..r.haulers.len() {
            let Some(h) = r.haulers.get(i).copied().filter(|h| !h.spawning) else {
                continue;
            };
            let store = stores.get(h.row).copied().unwrap_or_default();
            let here = Tile::of(h.at);
            if store.other > 0 {
                let Some(terminal) = site.terminal else {
                    continue;
                };
                if h.at.room_name() == terminal.at.room_name()
                    && here.range(Tile::of(terminal.at)) <= NEAR
                {
                    if terminal.free > 0
                        && buffer
                            .push(
                                snap,
                                h.handle,
                                CreepIntent::Transfer {
                                    target: Target::Structure(terminal.handle),
                                    resource: kind,
                                    amount: None,
                                },
                                commit,
                            )
                            .is_ok()
                    {
                        r.stats.transfers = r.stats.transfers.saturating_add(1);
                    }
                } else {
                    go(
                        snap,
                        movement,
                        h.handle,
                        terminal.at,
                        NEAR,
                        HAULER_MOVE_PRIORITY,
                        mcfg,
                        commit,
                    );
                }
                continue;
            }
            let free = store.free_all();
            // A full load, or what is left once the miner is not harvesting.
            let take = stock > 0 && free > 0 && (stock >= free || !harvesting);
            let near = h.at.room_name() == container.at.room_name()
                && here.range(Tile::of(container.at)) <= NEAR;
            if take
                && near
                && buffer
                    .push(
                        snap,
                        h.handle,
                        CreepIntent::WithdrawObject {
                            target: container.handle,
                            resource: kind,
                            amount: None,
                        },
                        commit,
                    )
                    .is_ok()
            {
                r.stats.withdraws = r.stats.withdraws.saturating_add(1);
                stock = stock.saturating_sub(free);
            } else if !near && h.at != container.at {
                // Waits next to the container (the miner stands on it).
                go(
                    snap,
                    movement,
                    h.handle,
                    container.at,
                    NEAR,
                    HAULER_MOVE_PRIORITY,
                    mcfg,
                    commit,
                );
            }
        }
        Ok(())
    }

    /// The rooms the next tick's snapshot reads (module docs), sorted by packed name: while
    /// `standard` (the Standard tier ran this tick, so the creeps' work can run), those with a
    /// mineral creep that can act ([`MineralRoom::can_act`]); and, while `minerals.enabled` is on,
    /// those whose read is due.
    pub fn reads(&mut self, cfg: &MineralsConfig, standard: bool) -> &[RoomName] {
        let next = self.time.saturating_add(1);
        self.reads.clear();
        for r in &self.rooms {
            let due = r.seen.is_none_or(|s| {
                next >= s.time.saturating_add(cfg.read_ticks.max(1))
                    || r.regen_at.is_some_and(|at| next >= at)
            });
            if (standard && r.can_act()) || (cfg.enabled && due) {
                self.reads.push(r.name);
            }
        }
        &self.reads
    }
}

impl MineralRoom {
    /// Whether a mineral creep of the room out of the spawn may act on the next tick: while the
    /// room mines (the miner walks or harvests, the haulers wait for loads), while its container
    /// held some of the mineral at the last read, or while a hauler carries some. A room whose
    /// creeps only spawn, or stand idle after a stop with nothing left to carry, is read on the
    /// `minerals.read_ticks` cadence instead (the hot-path review, 2026-11-26).
    pub fn can_act(&self) -> bool {
        let out = self.miners.iter().chain(&self.haulers).any(|m| !m.spawning);
        out && (self.why == Why::Mining
            || self.seen.is_some_and(|s| s.container_stock > 0)
            || self.haulers.iter().any(|h| h.carrying && !h.spawning))
    }
}

/// Why a room with the last read `seen` and plan `plan` mines or not (module docs): [`gate`]'s
/// reason, else [`Why::NoPlan`] without a plan with a miner's and a hauler's body.
pub fn why(cfg: &MineralsConfig, seen: Option<&Seen>, plan: Option<&MineralPlan>) -> Why {
    match gate(cfg, seen) {
        Why::Mining if plan.is_none_or(|p| p.miner.is_none() || p.hauler.is_none()) => Why::NoPlan,
        w => w,
    }
}

/// Every check of [`why`] before the plan's, in order: [`Why::Mining`] when all pass. The plan
/// is built only for a room this passes (or whose container holds a load), so a room that cannot
/// mine builds no flow field.
///
/// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`: the extractor's and the terminal's
/// first levels, V393).
pub fn gate(cfg: &MineralsConfig, seen: Option<&Seen>) -> Why {
    if !cfg.enabled {
        return Why::Off;
    }
    let Some(s) = seen else {
        return Why::Unread;
    };
    let first =
        earliest_level(StructureType::Extractor, 0).max(earliest_level(StructureType::Terminal, 0));
    if first.is_none_or(|l| s.level < l) {
        return Why::Level;
    }
    if !s.extractor {
        return Why::NoExtractor;
    }
    if s.container.is_none() {
        return Why::NoContainer;
    }
    if s.terminal.is_none() {
        return Why::NoTerminal;
    }
    if s.kind.is_none() {
        return Why::UnknownType;
    }
    if s.amount == 0 {
        return Why::Regenerating;
    }
    if s.terminal_stock >= cfg.stock_max {
        return Why::Stocked;
    }
    if s.terminal_free < cfg.free_min {
        return Why::TerminalFull;
    }
    Why::Mining
}

/// The haulers a room wants (module docs): the plan's while it mines and a miner lives (`miners`,
/// spawning ones included), so no hauler waits at an empty container for the miner's spawn; one
/// while the container holds a hauler's load and the terminal has room for it (a stopped room's
/// leftover, or a load a gone miner left); none otherwise.
pub fn haulers_wanted(why: Why, seen: Option<&Seen>, plan: &MineralPlan, miners: usize) -> u32 {
    if why == Why::Mining && miners > 0 {
        return plan.haulers;
    }
    let load = plan.load();
    let leftover = seen.is_some_and(|s| {
        s.kind.is_some()
            && s.terminal.is_some()
            && load > 0
            && s.container_stock >= load
            && s.terminal_free >= load
    });
    u32::from(leftover && plan.hauler.is_some())
}

/// The room's spawn on the lowest tile index, if it has one: the miner's walk starts there.
fn first_spawn(snap: &Snapshot, room: RoomName) -> Option<Position> {
    let st = &snap.structures;
    snap.spawns
        .rows()
        .iter()
        .filter_map(|&row| st.positions().get(row as usize).copied())
        .filter(|p| p.room_name() == room)
        .min_by_key(|&p| Tile::of(p).index())
}

/// A move for `creep` to within `range` of `target`, in the Standard tier; refusals are left to
/// movement (it reports them in its own counters).
#[allow(clippy::too_many_arguments)]
fn go(
    snap: &Snapshot,
    movement: &mut Movement,
    creep: CreepHandle,
    target: Position,
    range: u8,
    priority: u8,
    mcfg: &MovementConfig,
    commit: &mut Commit,
) {
    let to = MoveTo {
        target,
        range,
        priority,
        tier: Tier::Standard,
    };
    let _ = movement.request(snap, creep, to, mcfg, commit);
}
