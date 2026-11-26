//! The factory (ROADMAP P9.3, ADR-046): each owned room with an own factory and an own terminal
//! compresses its surplus minerals into bars (the user's four choices: bars from the surplus, the
//! source haulers fill the terminal with the factory's energy, a factory hauler, the factory
//! built once it has work).
//!
//! - **The recipes** ([`recipes`]). A factory with no level (no power creep has operated it,
//!   V421) makes only the `COMMODITIES` recipes with no level. Of those, a bar takes a mineral
//!   and energy and makes the mineral's bar: 500 of the mineral and 200 energy make 100 bars,
//!   with a cooldown of 20 ticks (V131's table, read from the crate's `commodity_recipe`). The
//!   module handles the 8 minerals that have a bar ([`FACTORY_RESOURCES`]).
//! - **The job** ([`pick`]): the first of those minerals (the current one first) that the room
//!   holds at least a batch above `factory.mineral_keep` of, in its terminal, its factory and its
//!   hauler together, while the room holds fewer than `factory.bar_max` of its bar once the batch
//!   is made. The hauler's load counts, so taking a batch out of the terminal does not end the
//!   job. A job ends at the reserve or the bar cap, with a log line.
//! - **The pace** ([`pace`]). `produce` takes a batch's components and sets the factory's
//!   cooldown to the recipe's, a deadline (V422, V423). The module also waits
//!   `CREEP_LIFE_TIME / (factory.energy_per_life / a batch's energy)` ticks between batches
//!   (rounded up, never less than the cooldown), so the factory spends at most
//!   `factory.energy_per_life` in any creep life; that energy, with the hauler's body, is left out
//!   of the upgraders' budget while the room compresses ([`Factories::upkeeps`],
//!   `economy::Upgrading::set_factory`).
//! - **The energy** ([`Factories::banks`]). While a room whose sources are hauled compresses and
//!   its terminal holds less than `factory.energy_keep`, the terminal joins the source haulers'
//!   fills after the spawns,
//!   extensions and towers (`economy::Hauling::set_banks`, the user's choice). A room whose
//!   terminal is open is read for its terminal's energy alone on the next tick (the snapshot's
//!   bank read: `store` and `getUsedCapacity("energy")`, 2 calls), so the haulers stop once it
//!   is full.
//! - **The factory hauler** ([`Role::FactoryHauler`]): CARRY and MOVE parts (the lab hauler's
//!   body, `economy::lab_hauler_body`), one a room, requested while the factory has work
//!   ([`FactoryRoom::has_work`]) and the Standard tier runs, at [`PRIORITY_FACTORY_HAULER`], below
//!   the lab hauler. An empty one ([`FactoryRoom::task`]) takes out of the factory anything but the
//!   job's mineral and bar, and the bar once a whole load of it is there; else it loads the factory
//!   from the terminal with the job's mineral up to a batch, then with energy up to a batch. It
//!   withdraws from the terminal with the amount and from the factory with none (V426), and
//!   transfers with no amount (V415): the job's mineral and energy to the factory, anything else
//!   to the terminal.
//! - **Building the factory** ([`Factories::wants_factory`]): a room wants one (`planner.factory`
//!   places it only there, `planner::build::Gates::with_factory`) once an own factory stands in
//!   it, or once [`pick`] finds a job in what its terminal holds.
//!
//! The reads (the snapshot's `FactorySites`): a room is tracked once it has an own factory, or an
//! own terminal at a level that allows a factory (`CONTROLLER_STRUCTURES`, from RCL7), or a
//! factory hauler. It is read on the tick after one that asks for it ([`Factories::reads`]):
//! every tick the Standard tier runs while its hauler is out of the spawn and has work, on the
//! tick after a batch (the batch took the factory's components, which the hauler then replaces
//! at once), on the tick its next batch is due ([`FactoryRoom::next_produce`]), and otherwise
//! once `factory.read_ticks` have passed since its last read. Between reads nothing acts.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Factories::begin`] after the snapshot: the rooms, this tick's read of each (or its bank
//!    read), and each room's factory haulers.
//! 2. The Critical `factory` job runs [`Factories::plan_room`] per room, before the hauling job
//!    and the spawn plan: the job, whether the room wants a factory, the upkeep, the bank, and the
//!    hauler's request. Then [`Factories::banks`] goes to the haulers.
//! 3. The Standard `factory work` job runs [`Factories::work_room`] per room read this tick: the
//!    batch ([`Factories::orders`], which the act step issues as `produce`) and the hauler's move,
//!    withdraw and transfer.
//! 4. [`Factories::reads`]: the rooms the next tick's snapshot reads in full, and those whose
//!    terminal's energy alone it reads.
//!
//! Everything is kept in the heap. After a global reset a room's first read picks its job again
//! from what it holds, and the pace starts over (one batch may come early). Nothing is read from
//! or written to `Memory`.

use rustedscreep_consts::{
    CARRY_CAPACITY, CREEP_LIFE_TIME, Position, ResourceType, RoomName, StructureType,
};

use super::labs::hauler_body;
use crate::config::{FactoryConfig, MovementConfig};
use crate::intents::{CreepIntent, IntentBuffer, Outcome, Target};
use crate::movement::{MoveTo, Movement, Tile};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{
    Controller, CreepHandle, CreepStore, FACTORY_MINERALS, FACTORY_RESOURCES, FactoryRead,
    FactoryStock, Snapshot, StructureHandle, factory_resource,
};
use crate::spawning::{BodySpec, EnergyBudget, Role, SpawnRequest, Spawner};

/// The spawn priority of a factory hauler: below the lab hauler (64), so a room's labs are served
/// before its surplus is compressed.
pub const PRIORITY_FACTORY_HAULER: u8 = 63;

/// How hard the factory hauler's moves push past other creeps: as the lab hauler's.
const HAULER_MOVE_PRIORITY: u8 = 2;

/// Withdrawing and transferring reach 1 tile (`isNearTo`, V399, V400).
const NEAR: u8 = 1;

/// One bar recipe: the mineral it takes, the bar it makes, and its numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipe {
    pub mineral: ResourceType,
    pub bar: ResourceType,
    /// The mineral a batch takes. Unit: units.
    pub take: u32,
    /// The energy a batch takes. Unit: energy.
    pub energy: u32,
    /// The bars a batch makes. Unit: units.
    pub make: u32,
    /// The factory's cooldown after a batch. Unit: ticks.
    pub cooldown: u32,
}

/// The bar recipes, in [`FACTORY_RESOURCES`] order (module docs): for each mineral, its bar's
/// `COMMODITIES` entry, kept when it has no level and takes only the mineral and energy. Call
/// once a global: the crate builds each entry's components as a map.
///
/// Uses: `COMMODITIES` (`ResourceType::commodity_recipe`, V131: equal to the engine's table).
pub fn recipes() -> Vec<Recipe> {
    let mut out = Vec::with_capacity(FACTORY_MINERALS);
    for i in 0..FACTORY_MINERALS {
        let (Some(&mineral), Some(&bar)) = (
            FACTORY_RESOURCES.get(i),
            FACTORY_RESOURCES.get(i.saturating_add(FACTORY_MINERALS)),
        ) else {
            continue;
        };
        let Some(r) = bar.commodity_recipe() else {
            continue;
        };
        if r.level.is_some_and(|l| l > 0) {
            continue;
        }
        let take = r.components.get(&mineral).copied().unwrap_or(0);
        let energy = r
            .components
            .get(&ResourceType::Energy)
            .copied()
            .unwrap_or(0);
        let others = r
            .components
            .keys()
            .any(|&k| k != mineral && k != ResourceType::Energy);
        if take == 0 || others || r.amount == 0 {
            continue;
        }
        out.push(Recipe {
            mineral,
            bar,
            take,
            energy,
            make: r.amount,
            cooldown: r.cooldown,
        });
    }
    out
}

/// The ticks between two batches of `r` (module docs): `CREEP_LIFE_TIME` over the batches
/// `factory.energy_per_life` pays for, rounded up, and never less than the recipe's cooldown.
/// `None` when the budget does not pay for one batch: the factory then makes nothing.
///
/// Uses: `CREEP_LIFE_TIME`, `COMMODITIES` (the recipe's energy and cooldown).
pub fn pace(r: &Recipe, cfg: &FactoryConfig) -> Option<u32> {
    let batches = cfg.energy_per_life / r.energy.max(1);
    if batches == 0 {
        return None;
    }
    Some(CREEP_LIFE_TIME.div_ceil(batches).max(r.cooldown).max(1))
}

/// The most energy the factory spends on `r` in any `CREEP_LIFE_TIME` ticks at [`pace`]: a batch
/// every pace, so `ceil(CREEP_LIFE_TIME / pace)` batches, which is at most the batches
/// `factory.energy_per_life` pays for. Unit: energy.
///
/// Uses: `CREEP_LIFE_TIME`, `COMMODITIES` (the recipe's energy).
pub fn life_energy(r: &Recipe, cfg: &FactoryConfig) -> u64 {
    pace(r, cfg).map_or(0, |p| {
        u64::from(CREEP_LIFE_TIME.div_ceil(p)).saturating_mul(u64::from(r.energy))
    })
}

/// The recipe to work (module docs), as an index into `recipes`: `current` while the room still
/// holds a batch above `factory.mineral_keep` of its mineral with room under `factory.bar_max`
/// for its bars, else the first that does; none whose batch `factory.energy_per_life` cannot pay
/// for ([`pace`] is `None`). `held` is what the room holds of a resource.
pub fn pick(
    recipes: &[Recipe],
    held: impl Fn(ResourceType) -> u32,
    current: Option<usize>,
    cfg: &FactoryConfig,
) -> Option<usize> {
    let ok = |r: &Recipe| {
        held(r.mineral) >= cfg.mineral_keep.saturating_add(r.take)
            && held(r.bar).saturating_add(r.make) <= cfg.bar_max
            && pace(r, cfg).is_some()
    };
    current
        .filter(|&i| recipes.get(i).is_some_and(ok))
        .or_else(|| recipes.iter().position(ok))
}

fn stock_of(stock: &FactoryStock, kind: ResourceType) -> u32 {
    factory_resource(kind)
        .and_then(|i| stock.get(i).copied())
        .unwrap_or(0)
}

/// What a room's factory does, from its last read (module docs). The order is the order of the
/// checks, so a room shows the first that holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Why {
    /// `factory.enabled` is off.
    Off,
    /// Not read yet in this global.
    #[default]
    Unread,
    NoTerminal,
    /// No own factory stands (the room may want one built, [`FactoryRoom::wants`]).
    NoFactory,
    /// A job runs.
    Compressing,
    /// No job: the factory holds minerals or bars that go back to the terminal.
    Clearing,
    /// No job: no mineral is held a batch above the reserve with its bars under the cap.
    Idle,
}

impl Why {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Unread => "not read yet",
            Self::NoTerminal => "no terminal",
            Self::NoFactory => "no factory",
            Self::Compressing => "compressing",
            Self::Clearing => "clearing",
            Self::Idle => "idle",
        }
    }
}

/// Why a job ended, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The room holds less than a batch above `factory.mineral_keep` of the mineral.
    Reserve,
    /// Another batch would take the room past `factory.bar_max` of the bar.
    Bars,
    /// `factory.enabled` was turned off, or the factory or terminal is gone.
    Off,
}

impl Stop {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reserve => "at its reserve",
            Self::Bars => "at the bar cap",
            Self::Off => "off",
        }
    }
}

/// A factory hauler of this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub handle: CreepHandle,
    pub serial: u64,
    pub row: usize,
    pub at: Position,
    pub spawning: bool,
    pub store: CreepStore,
}

/// One task of an empty factory hauler ([`FactoryRoom::task`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    /// Withdraw all it can hold of this resource from the factory.
    Unload(ResourceType),
    /// Withdraw this much of this resource from the terminal, for the factory.
    Load(ResourceType, u32),
}

/// One room's counters for the console.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FactoryStats {
    pub reads: u32,
    pub bank_reads: u32,
    pub requests: u32,
    pub jobs: u32,
    pub batches: u32,
    pub refused: u32,
    pub withdraws: u32,
    pub transfers: u32,
}

/// One `StructureFactory.produce(resourceType)` call for the act step (V420).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProduceOrder {
    pub room: RoomName,
    pub factory: StructureHandle,
    /// The factory's row in this tick's structures (its JS object).
    pub row: u32,
    pub product: ResourceType,
}

/// A change in a room's jobs, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactoryEvent {
    /// A job started: its recipe and what the room held of the mineral.
    Started {
        room: RoomName,
        recipe: Recipe,
        held: u32,
    },
    /// A job ended: its recipe, the batches ordered for it, and why.
    Stopped {
        room: RoomName,
        recipe: Recipe,
        batches: u32,
        stop: Stop,
    },
}

/// Where a hauler's load goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dest {
    Factory,
    Terminal,
}

/// One room the module tracks.
#[derive(Clone, Debug)]
pub struct FactoryRoom {
    pub name: RoomName,
    /// Whether this tick read the room.
    pub read: bool,
    /// The last read: its terminal and factory with their stocks (the terminal's energy updated
    /// by any later bank read).
    pub seen: Option<FactoryRead>,
    pub seen_time: u32,
    pub job: Option<Recipe>,
    /// Batches ordered for the job.
    pub batches: u32,
    pub why: Why,
    /// Whether the room wants a factory built ([`Factories::wants_factory`]).
    pub wants: bool,
    /// The room's energy capacity at the last plan. Unit: energy.
    pub capacity: u32,
    pub haulers: Vec<Member>,
    /// Whether the hauler had work at the last work run (a task or a load).
    pub busy: bool,
    /// The tick of the last batch ordered in this global.
    pub last_produce: Option<u32>,
    /// The tick the next batch is due (the cooldown's end or the pace's), while a job runs.
    pub next_produce: Option<u32>,
    /// No batch until this tick (after a refused one).
    pub held_until: u32,
    /// Whether the terminal takes the haulers' energy this tick ([`Factories::banks`]).
    pub bank: bool,
    /// Whether a source hauler of the room carries energy home (its hauling plan wants haulers),
    /// at the last plan: without one and with no energy at hand, no factory energy is counted.
    pub hauled: bool,
    /// Whether the room's controller level allows a factory (`CONTROLLER_STRUCTURES`), this tick:
    /// a factory in a room below it is inactive and gets no job.
    pub allowed: bool,
    seen_tick: u32,
    pub stats: FactoryStats,
}

/// The factory module's state for one global.
#[derive(Default)]
pub struct Factories {
    time: u32,
    recipes: Vec<Recipe>,
    /// Whether [`Self::recipes`] was built in this global (once, whatever it holds).
    built: bool,
    rooms: Vec<FactoryRoom>,
    events: Vec<FactoryEvent>,
    orders: Vec<ProduceOrder>,
    reads: Vec<RoomName>,
    bank_reads: Vec<RoomName>,
    banks: Vec<(RoomName, StructureHandle, Position, u32)>,
    wanted: Vec<RoomName>,
    upkeeps: Vec<(RoomName, u64)>,
}

impl Factories {
    /// Starts the tick (module docs, step 1).
    pub fn begin(&mut self, snap: &Snapshot) {
        self.time = snap.tick.time;
        self.events.clear();
        self.orders.clear();
        self.upkeeps.clear();
        self.banks.clear();
        let now = self.time;
        let st = &snap.structures;
        let rooms = &snap.rooms;
        let allows = |name: RoomName| {
            rooms
                .row_of(name)
                .and_then(|r| rooms.controllers().get(r))
                .is_some_and(|c| match *c {
                    Controller::Mine { level } => {
                        StructureType::Factory.controller_structures(u32::from(level)) > 0
                    }
                    _ => false,
                })
        };
        for (&kind, pos) in st.kinds().iter().zip(st.positions()) {
            let name = pos.room_name();
            let tracked = match kind {
                StructureType::Factory => true,
                StructureType::Terminal => allows(name),
                _ => false,
            };
            if tracked {
                self.track(name, now);
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
            let Some(id) = id.filter(|id| id.role == Role::FactoryHauler && mine) else {
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
        // A room dropped (no factory, no terminal at a factory's level, no hauler) costs the
        // upgraders nothing more.
        for r in &self.rooms {
            if r.seen_tick != now {
                self.upkeeps.push((r.name, 0));
            }
        }
        self.rooms.retain(|r| r.seen_tick == now);
        // The bar recipes, once a global, from the first tick a room is tracked (the crate builds
        // a map an entry, V428).
        if !self.built && !self.rooms.is_empty() {
            self.recipes = recipes();
            self.built = true;
        }
        for room in &mut self.rooms {
            room.haulers.sort_by_key(|m| m.serial);
            room.allowed = allows(room.name);
            if let Some(read) = snap.factory.get(room.name) {
                room.read = true;
                room.seen = Some(*read);
                room.seen_time = now;
                room.stats.reads = room.stats.reads.saturating_add(1);
            } else if let Some(energy) = snap.factory.bank(room.name) {
                room.stats.bank_reads = room.stats.bank_reads.saturating_add(1);
                if let Some(t) = room.seen.as_mut().and_then(|s| s.terminal.as_mut()) {
                    t.energy = energy;
                }
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
                    FactoryRoom {
                        name: room,
                        read: false,
                        seen: None,
                        seen_time: 0,
                        job: None,
                        batches: 0,
                        why: Why::default(),
                        wants: false,
                        capacity: 0,
                        haulers: Vec::new(),
                        busy: false,
                        last_produce: None,
                        next_produce: None,
                        held_until: 0,
                        bank: false,
                        hauled: false,
                        allowed: false,
                        seen_tick: now,
                        stats: FactoryStats::default(),
                    },
                );
                i
            }
        }
    }

    /// The bar recipes the module works ([`recipes`]), from the first `begin`.
    pub fn recipes(&self) -> &[Recipe] {
        &self.recipes
    }

    /// The rooms, by name.
    pub fn rooms(&self) -> &[FactoryRoom] {
        &self.rooms
    }

    /// The Critical job's units: one a room.
    pub fn units(&self) -> u32 {
        u32::try_from(self.rooms.len()).unwrap_or(u32::MAX)
    }

    /// This tick's job starts and ends.
    pub fn events(&self) -> &[FactoryEvent] {
        &self.events
    }

    /// This tick's batches, for the act step.
    pub fn orders(&self) -> &[ProduceOrder] {
        &self.orders
    }

    /// Each room's factory upkeep over a creep life (its hauler's body while the factory has work,
    /// and [`life_energy`] while it compresses with energy to be had), 0 while it has none, from
    /// this tick's plan runs and the rooms dropped at `begin`, for
    /// `economy::Upgrading::set_factory`.
    pub fn upkeeps(&self) -> &[(RoomName, u64)] {
        &self.upkeeps
    }

    /// The terminals that take the haulers' energy this tick, from the plan runs (module docs):
    /// room, terminal, its place, and how much more it takes, for `economy::Hauling::set_banks`.
    pub fn banks(&self) -> &[(RoomName, StructureHandle, Position, u32)] {
        &self.banks
    }

    /// The rooms that want a factory built (module docs), sorted by packed name, from the last
    /// plan runs: `economy::placement::Placement::set_factory_rooms` takes them.
    pub fn wants_factory(&mut self) -> &[RoomName] {
        self.wanted.clear();
        self.wanted
            .extend(self.rooms.iter().filter(|r| r.wants).map(|r| r.name));
        &self.wanted
    }

    /// The Critical half of unit `unit`'s tick (module docs, step 2): on a read tick the job and
    /// whether the room wants a factory; the upkeep; the bank while `standard` (the Standard tier
    /// runs this tick); and the hauler's request while `standard`. `hauled` says whether the
    /// room's source haulers carry energy home (its hauling plan wants haulers).
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        spawner: &mut Spawner,
        cfg: &FactoryConfig,
        standard: bool,
        hauled: bool,
    ) -> Result<(), Fault> {
        let ri = usize::try_from(unit).map_err(|_| Fault::new("unit out of range", unit))?;
        let Self {
            recipes,
            rooms,
            events,
            banks,
            upkeeps,
            ..
        } = self;
        let r = rooms
            .get_mut(ri)
            .ok_or(Fault::new("no room for the unit", unit))?;
        let name = r.name;
        if let Some(row) = snap.rooms.row_of(name)
            && let Some(e) = snap.rooms.energy().get(row)
        {
            r.capacity = e.capacity;
        }
        r.hauled = hauled;
        if r.read || !cfg.enabled {
            r.advance(recipes, cfg, events);
            // Whether the room wants a factory changes only with what a read sees.
            r.wants = cfg.enabled
                && r.seen.is_some_and(|s| {
                    s.factory.is_some()
                        || (s.terminal.is_some()
                            && pick(recipes, |k| r.held(k), None, cfg).is_some())
                });
        }
        r.why = r.why_now(cfg);
        let cap = cfg.hauler_carry.saturating_mul(CARRY_CAPACITY);
        let work = cfg.enabled && r.has_work(cap, cfg);
        // The body counts for the whole job, not only while the hauler has a task, so the
        // upgraders' plan is not built again twice a batch (the hot-path review, 2026-11-26).
        let body = (work || (cfg.enabled && r.job.is_some()))
            .then(|| hauler_body(r.capacity, cfg.hauler_carry))
            .flatten();
        let mut upkeep = body.map_or(0, |b| u64::from(b.cost()));
        let terminal = r.seen.and_then(|s| s.terminal);
        if let Some(j) = r.job.filter(|_| cfg.enabled) {
            let at_hand = terminal.is_some_and(|t| t.energy >= j.energy)
                || r.seen
                    .and_then(|s| s.factory)
                    .is_some_and(|f| f.energy >= j.energy);
            if hauled || at_hand {
                upkeep = upkeep.saturating_add(life_energy(&j, cfg));
            }
        }
        upkeeps.push((name, upkeep));
        r.bank = false;
        // Only where a source hauler can bring the energy (the sandbox sim, 2026-11-26: a room
        // with no hauled source read its terminal's energy every tick for nothing).
        if standard
            && cfg.enabled
            && hauled
            && r.job.is_some()
            && let Some(t) = terminal
            && t.energy < cfg.energy_keep
            && t.free > 0
            && snap.structures.row_of(t.handle).is_some()
        {
            r.bank = true;
            let want = cfg.energy_keep.saturating_sub(t.energy).min(t.free);
            banks.push((name, t.handle, t.at, want));
        }
        if standard
            && work
            && r.haulers.is_empty()
            && let Some(body) = body
            && spawner
                .request(SpawnRequest {
                    home: name,
                    role: Role::FactoryHauler,
                    remote: None,
                    body: BodySpec::Exact(body),
                    budget: EnergyBudget::Capacity,
                    priority: PRIORITY_FACTORY_HAULER,
                })
                .is_some()
        {
            r.stats.requests = r.stats.requests.saturating_add(1);
        }
        Ok(())
    }

    /// Whether the Standard work has a room to work: one read this tick with a hauler out of the
    /// spawn or a job.
    pub fn has_work(&self) -> bool {
        self.rooms
            .iter()
            .any(|r| r.read && (r.haulers.iter().any(|m| !m.spawning) || r.job.is_some()))
    }

    /// The Standard half of unit `unit`'s tick (module docs, step 3), in a room read this tick:
    /// the batch while a job runs and `factory.enabled` is on, then the oldest hauler's move,
    /// withdraw or transfer. Call the units in order, one a room ([`Self::units`]).
    #[allow(clippy::too_many_arguments)]
    pub fn work_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &FactoryConfig,
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
            r.produce(now, cfg, &mut self.orders, commit);
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
        // A load first: its energy or its one other resource.
        if store.energy > 0 || store.other > 0 {
            let kind = if store.energy > 0 {
                Some(ResourceType::Energy)
            } else {
                store.other_kind
            };
            // A type the module does not handle: nothing to name it by.
            let Some(kind) = kind else {
                r.busy = false;
                return Ok(());
            };
            let Some(dest) = r.delivery(kind, cfg.enabled) else {
                // Nowhere to put it (a full terminal): no read is kept going for it.
                r.busy = false;
                return Ok(());
            };
            r.busy = true;
            let (target, at) = match (dest, seen.factory) {
                (Dest::Factory, Some(f)) => (f.handle, f.at),
                _ => (terminal.handle, terminal.at),
            };
            if !near(at) {
                go(snap, movement, h.handle, at, mcfg, commit);
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
            }
            return Ok(());
        }
        let task = if cfg.enabled {
            r.task(store.free_all(), store.capacity, cfg)
        } else {
            None
        };
        r.busy = task.is_some();
        let Some(task) = task else {
            return Ok(());
        };
        let (target, at, resource, amount) = match task {
            Task::Unload(kind) => {
                let Some(f) = seen.factory else {
                    return Ok(());
                };
                (f.handle, f.at, kind, None)
            }
            Task::Load(kind, n) => (terminal.handle, terminal.at, kind, Some(n)),
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
    /// holds the room's batches for `factory.read_ticks` (module docs). Returns whether this
    /// call started the hold.
    pub fn record_produce(&mut self, index: usize, outcome: Outcome, cfg: &FactoryConfig) -> bool {
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
                let started = r.held_until <= self.time;
                r.held_until = until;
                r.next_produce = Some(until);
                started
            }
            None => false,
        }
    }

    /// The rooms the next tick's snapshot reads in full (module docs), sorted by packed name:
    /// while `standard` (the Standard tier ran this tick, so the hauler's work and the batch could
    /// run), those whose hauler is out of the spawn with work ([`FactoryRoom::can_act`]), that
    /// ordered a batch this tick, or whose next batch is due; and, while `factory.enabled` is on,
    /// those whose `factory.read_ticks` read is due. Then the rooms whose terminal took the
    /// haulers' energy this tick and that are not read in full: their terminal's energy alone is
    /// read (the bank reads), also sorted.
    pub fn reads(&mut self, cfg: &FactoryConfig, standard: bool) -> (&[RoomName], &[RoomName]) {
        let next = self.time.saturating_add(1);
        self.reads.clear();
        let cap = cfg.hauler_carry.saturating_mul(CARRY_CAPACITY);
        for r in &self.rooms {
            let cadence =
                r.seen.is_none() || next >= r.seen_time.saturating_add(cfg.read_ticks.max(1));
            // A batch is ordered only in the Standard work, so its deadline asks for a read only
            // while that tier runs (as the labs' reactions, ADR-045's review).
            let due = cfg.enabled && r.job.is_some() && r.next_produce.is_some_and(|t| next >= t);
            // A batch this tick took what the factory held after the read the hauler worked from
            // (found by the unit tests: the hauler otherwise idled a whole pace).
            let produced = r.last_produce == Some(self.time);
            if (standard && (r.can_act(cap, cfg) || due || produced)) || (cfg.enabled && cadence) {
                self.reads.push(r.name);
            }
        }
        self.bank_reads.clear();
        for r in &self.rooms {
            if r.bank
                && self
                    .reads
                    .binary_search_by_key(&r.name.packed_repr(), |n| n.packed_repr())
                    .is_err()
            {
                self.bank_reads.push(r.name);
            }
        }
        (&self.reads, &self.bank_reads)
    }
}

impl FactoryRoom {
    /// What the room holds of `kind`: in its terminal and factory at the last read, and in its
    /// haulers' stores.
    pub fn held(&self, kind: ResourceType) -> u32 {
        let (terminal, factory) = self.seen.map_or((0, 0), |s| {
            (
                stock_of(&s.stock, kind),
                s.factory.map_or(0, |f| stock_of(&f.stock, kind)),
            )
        });
        let carried = self
            .haulers
            .iter()
            .filter(|h| h.store.other_kind == Some(kind))
            .fold(0u32, |a, h| a.saturating_add(h.store.other));
        terminal.saturating_add(factory).saturating_add(carried)
    }

    /// Picks the job again from the last read ([`pick`]), with a log event when it changes.
    fn advance(&mut self, recipes: &[Recipe], cfg: &FactoryConfig, events: &mut Vec<FactoryEvent>) {
        let ready = self.allowed
            && self
                .seen
                .is_some_and(|s| s.terminal.is_some() && s.factory.is_some());
        let current = self
            .job
            .and_then(|j| recipes.iter().position(|r| r.mineral == j.mineral));
        let next = if cfg.enabled && ready {
            pick(recipes, |k| self.held(k), current, cfg).and_then(|i| recipes.get(i).copied())
        } else {
            None
        };
        if next == self.job {
            return;
        }
        if let Some(old) = self.job {
            let stop = if !cfg.enabled || !ready {
                Stop::Off
            } else if self.held(old.bar).saturating_add(old.make) > cfg.bar_max {
                Stop::Bars
            } else {
                Stop::Reserve
            };
            events.push(FactoryEvent::Stopped {
                room: self.name,
                recipe: old,
                batches: self.batches,
                stop,
            });
        }
        if let Some(new) = next {
            events.push(FactoryEvent::Started {
                room: self.name,
                recipe: new,
                held: self.held(new.mineral),
            });
            self.stats.jobs = self.stats.jobs.saturating_add(1);
        }
        self.job = next;
        self.batches = 0;
        self.next_produce = None;
    }

    fn why_now(&self, cfg: &FactoryConfig) -> Why {
        if !cfg.enabled {
            return Why::Off;
        }
        let Some(s) = self.seen else {
            return Why::Unread;
        };
        if s.terminal.is_none() {
            return Why::NoTerminal;
        }
        let Some(f) = s.factory else {
            return Why::NoFactory;
        };
        if self.job.is_some() {
            Why::Compressing
        } else if f.stock.iter().any(|&n| n > 0) {
            Why::Clearing
        } else {
            Why::Idle
        }
    }

    /// The task of an empty hauler with `free` room in a store of `cap` (module docs): take out
    /// of the factory anything but the job's mineral and bar, and the bar once `cap` of it is
    /// there (each only while the terminal has room); else load the job's mineral up to a batch,
    /// then energy for a batch. `None` with nothing to do.
    pub fn task(&self, free: u32, cap: u32, cfg: &FactoryConfig) -> Option<Task> {
        if free == 0 {
            return None;
        }
        let s = self.seen?;
        let f = s.factory?;
        let t = s.terminal?;
        let job = self.job.filter(|_| cfg.enabled);
        if t.free > 0 {
            for (&kind, &n) in FACTORY_RESOURCES.iter().zip(f.stock.iter()) {
                if n == 0 {
                    continue;
                }
                let kept = job.is_some_and(|j| kind == j.mineral || (kind == j.bar && n < cap));
                if !kept {
                    return Some(Task::Unload(kind));
                }
            }
        }
        let j = job?;
        let have = stock_of(&f.stock, j.mineral);
        if have < j.take {
            let n = j
                .take
                .saturating_sub(have)
                .min(free)
                .min(f.free)
                .min(stock_of(&s.stock, j.mineral));
            if n > 0 {
                return Some(Task::Load(j.mineral, n));
            }
        }
        if f.energy < j.energy {
            let n = free.min(f.free).min(t.energy);
            if n > 0 && f.energy.saturating_add(n) >= j.energy {
                return Some(Task::Load(ResourceType::Energy, n));
            }
        }
        None
    }

    /// Where a hauler's load of `kind` goes: the job's mineral and energy to the factory while it
    /// has room, anything else to the terminal while it has room; `None` when neither can take it.
    fn delivery(&self, kind: ResourceType, enabled: bool) -> Option<Dest> {
        let s = self.seen?;
        let for_factory = enabled
            && self
                .job
                .is_some_and(|j| kind == j.mineral || kind == ResourceType::Energy)
            && s.factory.is_some_and(|f| f.free > 0);
        if for_factory {
            Some(Dest::Factory)
        } else if s.terminal.is_some_and(|t| t.free > 0) {
            Some(Dest::Terminal)
        } else {
            None
        }
    }

    /// Whether the factory has work for its hauler (module docs): for each hauler, a load it
    /// holds that can be delivered, or, empty, a task for its store; with no hauler yet, a task
    /// for an empty one of `cap`. A load with nowhere to go is no work, whatever an empty hauler
    /// could do (the hot-path review, 2026-11-26, as ADR-045's for labs).
    pub fn has_work(&self, cap: u32, cfg: &FactoryConfig) -> bool {
        if self.haulers.is_empty() {
            return self.task(cap.max(CARRY_CAPACITY), cap, cfg).is_some();
        }
        self.haulers.iter().any(|h| {
            if h.store.energy > 0 || h.store.other > 0 {
                self.deliverable(&h.store, cfg)
            } else {
                self.task(h.store.free_all(), h.store.capacity, cfg)
                    .is_some()
            }
        })
    }

    /// Whether a hauler holding `store` can deliver its load now ([`Self::delivery`]).
    fn deliverable(&self, store: &CreepStore, cfg: &FactoryConfig) -> bool {
        if store.energy > 0 {
            return self.delivery(ResourceType::Energy, cfg.enabled).is_some();
        }
        store.other > 0
            && store
                .other_kind
                .is_some_and(|k| self.delivery(k, cfg.enabled).is_some())
    }

    /// The tick the next batch is due while a job runs (module docs).
    pub fn next_produce(&self) -> Option<u32> {
        self.next_produce
    }

    /// Whether the hauler, out of the spawn, may act on the next tick: it had work at the last
    /// work run, or, while `factory.enabled`, the last read leaves it some ([`Self::has_work`]; a
    /// hauler just out of the spawn). An idle one is read on the cadence instead.
    pub fn can_act(&self, cap: u32, cfg: &FactoryConfig) -> bool {
        self.haulers.iter().any(|h| !h.spawning)
            && (self.busy || (cfg.enabled && self.has_work(cap, cfg)))
    }

    /// Orders this tick's batch (module docs): with a job, no hold, the factory's cooldown at 0,
    /// the pace passed since the last batch, a batch's components in the factory, and room for
    /// its bars once they are taken (V420, V425); then the tick the next is due.
    fn produce(
        &mut self,
        now: u32,
        cfg: &FactoryConfig,
        orders: &mut Vec<ProduceOrder>,
        commit: &mut Commit,
    ) {
        let Some(j) = self.job else {
            self.next_produce = None;
            return;
        };
        if now < self.held_until {
            return;
        }
        let Some(f) = self.seen.and_then(|s| s.factory) else {
            return;
        };
        let Some(pace) = pace(&j, cfg) else {
            self.next_produce = None;
            return;
        };
        let paced = self.last_produce.map_or(now, |l| l.saturating_add(pace));
        if f.cooldown > 0 || now < paced {
            self.next_produce = Some(now.saturating_add(f.cooldown).max(paced));
            return;
        }
        let used = j.take.saturating_add(j.energy);
        if stock_of(&f.stock, j.mineral) < j.take
            || f.energy < j.energy
            || f.free.saturating_add(used) < j.make
        {
            // The hauler's work (a read every tick it acts) brings the rest.
            self.next_produce = None;
            return;
        }
        orders.push(ProduceOrder {
            room: self.name,
            factory: f.handle,
            row: f.row,
            product: j.bar,
        });
        commit.intents(1);
        self.last_produce = Some(now);
        self.next_produce = Some(now.saturating_add(pace));
        self.batches = self.batches.saturating_add(1);
        self.stats.batches = self.stats.batches.saturating_add(1);
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
