//! The RCL1–3 opening (ROADMAP P2.4): generalist workers that harvest, fill the spawns and
//! extensions, build the room's own sites and upgrade the controller, with the spawn requests that
//! keep a room at the count its sources can keep busy, zero-creep recovery and the watchdogs. The
//! P3 economy (static miners, haulers, upgraders) replaces it; until then it runs at every level.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Opening::begin`] after the snapshot: the owned rooms (the jobs' units, by name) and each
//!    room's workers (own creeps whose name says `worker` and that room, by id).
//! 2. The Critical job runs [`Opening::plan_room`] per room, before the spawn job: the room's plan
//!    (worker count from [`super::sizing`], built again when movement's grid or the room's
//!    capacity changes), the watchdog, the spawn request, every worker's mode and task, and the
//!    Critical tasks' intents and moves: harvesting, taking energy, filling, and the upgrade the
//!    downgrade guard keeps.
//! 3. The Standard job runs [`Opening::work_room`] per room: building and upgrading.
//!
//! A worker gathers until its store is full and works until it is empty. Each tick its task is
//! chosen again, in id order with reservations, so no two workers fetch the same energy or fill the
//! same free space: first the gatherers (a pile, tombstone, ruin or container no farther than its
//! source, else its source), then the workers with energy: filling until the room's free spawn
//! energy is covered, the guard's upgrader, `min_upgraders`, building, upgrading. A task kept from
//! the last tick goes first, so assignments do not flip.
//!
//! Distances are Chebyshev ranges; only sizing measures paths (movement's grid). State lives in the
//! heap: a global reset loses the modes, sources and tasks, and the next tick rebuilds them from the
//! stores (a worker with energy works, an empty one gathers). Nothing is read from or written to
//! `Memory`.

use rustedscreep_consts::{
    BUILD_POWER, HARVEST_POWER, Position, ResourceType, RoomName, StructureType,
};

use super::mining::Mined;
use super::sizing::{Trip, carry_round, worker_body, worker_round, workers_for_source};
use super::watchdog::{Event, Observation, Watchdog};
use crate::config::{MovementConfig, OpeningConfig};
use crate::intents::{CreepIntent, IntentBuffer, Target};
use crate::movement::{Dial, FlowField, Goal, MoveError, MoveTo, Movement, Tile, UNREACHED};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{
    Controller, CreepHandle, ObjectHandle, ObjectKind, Snapshot, StructureHandle,
};
use crate::spawning::{BodySpec, EnergyBudget, Role, SpawnRequest, Spawner, WORKER_PATTERN};

/// Spawn priorities of the opening's requests (higher first): a room with no worker that can work,
/// a room below `min_workers`, and the rest.
pub const PRIORITY_RECOVERY: u8 = 200;
pub const PRIORITY_LOW: u8 = 150;
pub const PRIORITY_NORMAL: u8 = 100;

/// Upgrading and building reach 3 tiles (`inRangeTo(target, 3)`); harvesting, taking, filling 1
/// (`isNearTo`) (V108, V182, V185, V187, V190, V192).
const WORK_RANGE: u8 = 3;
const NEAR: u8 = 1;

/// What a worker does this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    Harvest(ObjectHandle),
    /// Take energy from a pile (`pickup`) or a tombstone, ruin or container (`withdraw`).
    Take(ObjectHandle),
    /// Transfer energy to a spawn or an extension.
    Fill(StructureHandle),
    Build(ObjectHandle),
    /// `upgradeController`; `guard` when it is the downgrade guard's upgrader (Critical).
    Upgrade {
        controller: StructureHandle,
        guard: bool,
    },
    /// A gatherer next to its mined source's tile while too little energy lies there (P3.1): it
    /// waits there with no intent.
    Wait(Position),
    /// Nothing to do: it makes way.
    Idle,
}

impl Task {
    /// The tier of the job that runs it: what keeps the room alive is Critical, the rest Standard.
    pub const fn tier(self) -> Tier {
        match self {
            Self::Harvest(_)
            | Self::Take(_)
            | Self::Fill(_)
            | Self::Upgrade { guard: true, .. }
            | Self::Wait(_)
            | Self::Idle => Tier::Critical,
            Self::Build(_) | Self::Upgrade { guard: false, .. } => Tier::Standard,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Harvest(_) => "harvest",
            Self::Take(_) => "take",
            Self::Fill(_) => "fill",
            Self::Build(_) => "build",
            Self::Upgrade { guard: true, .. } => "guard",
            Self::Upgrade { guard: false, .. } => "upgrade",
            Self::Wait(_) => "wait",
            Self::Idle => "idle",
        }
    }

    /// Every kind, in a fixed order (the order of [`RoomStats::tasks`]).
    pub const KINDS: [&'static str; 8] = [
        "harvest", "take", "fill", "build", "guard", "upgrade", "wait", "idle",
    ];

    fn kind_index(self) -> usize {
        match self {
            Self::Harvest(_) => 0,
            Self::Take(_) => 1,
            Self::Fill(_) => 2,
            Self::Build(_) => 3,
            Self::Upgrade { guard: true, .. } => 4,
            Self::Upgrade { guard: false, .. } => 5,
            Self::Wait(_) => 6,
            Self::Idle => 7,
        }
    }

    /// How hard its moves push past other creeps (`MoveTo::priority`).
    const fn move_priority(self) -> u8 {
        match self {
            Self::Fill(_) | Self::Upgrade { guard: true, .. } => 3,
            Self::Harvest(_) | Self::Take(_) | Self::Wait(_) => 2,
            Self::Build(_) | Self::Upgrade { guard: false, .. } => 1,
            Self::Idle => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Gather,
    Work,
}

/// A worker's state between ticks, in the slot of its handle.
#[derive(Clone, Copy, Debug)]
struct Worker {
    handle: CreepHandle,
    mode: Mode,
    /// The source it harvests (by position: sources never move).
    source: Option<Position>,
    task: Task,
}

/// One source in a room's plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourcePlan {
    pub pos: Position,
    /// Walkable tiles next to it on movement's grid. Unit: tiles.
    pub open: u32,
    /// Workers it keeps busy ([`workers_for_source`]). Unit: creeps.
    pub want: u32,
    /// The way from it to the controller.
    pub trip: Option<Trip>,
    /// Its miner was next to it when the plan was built (P3.1): workers take its energy off the
    /// miner's tile ([`carry_round`]) instead of harvesting it.
    pub mined: bool,
}

/// A room's plan: built again when movement's grid version, the room's energy capacity or the
/// sources its miners mine change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub version: u32,
    pub capacity: u32,
    /// The controller and the sources the plan was built with: a plan built before the room's
    /// objects were all seen is built again once they are.
    pub controller: bool,
    pub sources: Vec<SourcePlan>,
    /// The mined sources the plan was built with, by index in `sources` (bit `i`).
    pub mined: u32,
    /// Workers the room spawns up to: the sources' sum within `min_workers..=max_workers` (0 when
    /// no source has an open tile). Unit: creeps.
    pub want: u32,
}

/// One room's counters for the console, from its last Critical run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoomStats {
    pub time: u32,
    pub alive: u32,
    pub working: u32,
    pub want: u32,
    /// Workers per task, in [`Task::KINDS`] order.
    pub tasks: [u32; 8],
    /// What this tick asked of the spawner: `None`, or the budget and priority.
    pub request: Option<(EnergyBudget, u8)>,
    pub sources: u32,
    pub sites: u32,
    /// Energy in piles, tombstones, ruins and containers. Unit: energy.
    pub loose: u32,
}

struct RoomState {
    name: RoomName,
    row: Option<usize>,
    plan: Option<Plan>,
    watch: Watchdog,
    stats: RoomStats,
    /// Members of [`Opening::members`] that belong to it this tick.
    members: (usize, usize),
    /// Targets movement found no way to from this room (sources, energy to take, spawns and
    /// extensions, sites, the controller), each skipped until the tick given
    /// (`movement.route_retry_ticks` later): by the worker that failed on it (`Some`), or by every
    /// worker once a second one failed on it too (`None`), so one worker that can reach nothing
    /// does not close the room to the others. At most [`UNREACHABLE_MAX`], oldest dropped first.
    unreachable: Vec<(Position, u32, Option<CreepHandle>)>,
}

/// Unreachable targets a room remembers at once.
const UNREACHABLE_MAX: usize = 16;

/// One worker of this tick, in the order the jobs walk them: by room, then by id.
#[derive(Clone, Copy, Debug)]
struct Member {
    room: usize,
    key: u128,
    row: usize,
    handle: CreepHandle,
}

/// What a room holds this tick, gathered once per Critical run.
#[derive(Default)]
struct Facts {
    controller: Option<(StructureHandle, Position)>,
    /// Spawns and extensions with room for energy: handle, place, free space left to reserve.
    fills: Vec<(StructureHandle, Position, u32)>,
    /// Sources: handle, place, energy, capacity.
    sources: Vec<(ObjectHandle, Position, u32, u32)>,
    /// Own sites: handle, place, type, progress, progress total, id.
    sites: Vec<(ObjectHandle, Position, StructureType, u32, u32, u128)>,
    /// Energy to take: handle, place, kind, energy left to reserve.
    loose: Vec<(ObjectHandle, Position, ObjectKind, u32)>,
    /// Sources whose miner is next to them this tick (`economy::mining`, P3.1).
    mined: Vec<Mined>,
}

/// The opening's state for one global.
#[derive(Default)]
pub struct Opening {
    time: u32,
    rooms: Vec<RoomState>,
    members: Vec<Member>,
    /// Indexed by creep handle slot.
    workers: Vec<Option<Worker>>,
    facts: Facts,
    events: Vec<(RoomName, Event)>,
    field: FlowField,
    dial: Dial,
    /// Scratch, kept for its capacity: indexes into `members` of the room being run, the plan's
    /// sources with their loads, and the workers with energy as they are assigned.
    order: Vec<usize>,
    loads: Vec<u32>,
    plan_sources: Vec<(Position, u32)>,
    working: Vec<(usize, Position, u32, Task, u8)>,
    left: Vec<(usize, Position, u32, Task, u8)>,
    assigned: Vec<(usize, Task)>,
    /// This tick's unreachable targets of the room being run: for every worker, and for one.
    blocked: Vec<Position>,
    private: Vec<(Position, CreepHandle)>,
    /// What is left this tick of each source's energy and each site's progress, less a lower
    /// bound of what each harvest or build already pushed takes (`HARVEST_POWER`, `BUILD_POWER`
    /// for one WORK part), so a target that is used up gets no more billed no-ops.
    source_left: Vec<(ObjectHandle, u32)>,
    site_left: Vec<(ObjectHandle, u32)>,
}

impl Opening {
    /// Starts a tick: the owned rooms, by name, and their workers, by room and id.
    pub fn begin(&mut self, snap: &Snapshot) {
        self.time = snap.tick.time;
        self.site_left.clear();
        let rooms = &snap.rooms;
        for state in &mut self.rooms {
            state.row = None;
        }
        for row in rooms.owned() {
            let Some(&name) = rooms.names().get(row) else {
                continue;
            };
            match self
                .rooms
                .binary_search_by_key(&name.packed_repr(), |r| r.name.packed_repr())
            {
                Ok(i) => {
                    if let Some(r) = self.rooms.get_mut(i) {
                        r.row = Some(row);
                    }
                }
                Err(i) => self.rooms.insert(
                    i,
                    RoomState {
                        name,
                        row: Some(row),
                        plan: None,
                        watch: Watchdog::default(),
                        stats: RoomStats::default(),
                        members: (0, 0),
                        unreachable: Vec::new(),
                    },
                ),
            }
        }
        self.rooms.retain(|r| r.row.is_some());
        self.members.clear();
        let creeps = &snap.creeps;
        for (row, identity) in creeps.identities().iter().enumerate() {
            let Some(id) = identity.filter(|id| id.role == Role::Worker) else {
                continue;
            };
            if !creeps.my().get(row).copied().unwrap_or(false) {
                continue;
            }
            let Ok(room) = self
                .rooms
                .binary_search_by_key(&id.home.packed_repr(), |r| r.name.packed_repr())
            else {
                continue;
            };
            let Some(&handle) = creeps.handles().get(row) else {
                continue;
            };
            let key = creeps.key_of(handle).map_or(u128::MAX, |k| k.0);
            self.members.push(Member {
                room,
                key,
                row,
                handle,
            });
        }
        // Few members; a sorted insert would do as well, but this runs once per tick.
        self.members.sort_unstable_by_key(|m| (m.room, m.key));
        let mut start = 0;
        for (i, state) in self.rooms.iter_mut().enumerate() {
            let end = start
                + self
                    .members
                    .get(start..)
                    .map_or(0, |rest| rest.iter().take_while(|m| m.room == i).count());
            state.members = (start, end);
            start = end;
        }
    }

    /// The owned rooms of this tick, by name: unit `i` of both jobs is room `i`.
    pub fn rooms(&self) -> impl ExactSizeIterator<Item = RoomName> + '_ {
        self.rooms.iter().map(|r| r.name)
    }

    pub fn room_count(&self) -> u32 {
        u32::try_from(self.rooms.len()).unwrap_or(u32::MAX)
    }

    /// The name of room `unit`.
    pub fn room_name(&self, unit: u32) -> Option<RoomName> {
        self.rooms.get(usize::try_from(unit).ok()?).map(|r| r.name)
    }

    /// The most energy `room`'s watchdog saw it hold while it never filled ([`Watchdog::cap`]), from
    /// its last run.
    pub fn cap(&self, room: RoomName) -> Option<u32> {
        self.rooms
            .binary_search_by_key(&room.packed_repr(), |r| r.name.packed_repr())
            .ok()
            .and_then(|i| self.rooms.get(i))
            .and_then(|r| r.watch.cap())
    }

    /// The Critical half of room `unit`'s tick (module docs, step 2). `mined` lists the room's
    /// sources whose miner is next to them this tick (`Mining::mined`; empty without miners).
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        buffer: &mut IntentBuffer,
        cfg: &OpeningConfig,
        mcfg: &MovementConfig,
        mined: &[Mined],
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let ri = usize::try_from(unit).map_err(|_| Fault::new("unit out of range", unit))?;
        let (name, row, members) = {
            let r = self
                .rooms
                .get(ri)
                .ok_or(Fault::new("no room for the unit", unit))?;
            (r.name, r.row, r.members)
        };
        let row = row.ok_or(Fault::new("room not in this tick", unit))?;
        self.gather_facts(snap, name, mined);
        self.ensure_plan(ri, snap, row, movement, cfg, mcfg, commit);
        let want = self
            .rooms
            .get(ri)
            .and_then(|r| r.plan.as_ref())
            .map_or(cfg.min_workers, |p| p.want);

        // Who is there.
        let mut alive = 0u32;
        let mut working = 0u32;
        for m in self.members.get(members.0..members.1).unwrap_or(&[]) {
            alive += 1;
            let spawning = snap.creeps.spawning().get(m.row).copied().unwrap_or(true);
            let store = snap.creeps.stores().get(m.row).copied().unwrap_or_default();
            if !spawning && store.capacity > 0 {
                working += 1;
            }
        }

        // The watchdog.
        let level = match snap.rooms.controllers().get(row) {
            Some(Controller::Mine { level }) => *level,
            _ => 0,
        };
        let state = snap
            .rooms
            .controller_states()
            .get(row)
            .copied()
            .unwrap_or_default();
        let energy = snap.rooms.energy().get(row).copied().unwrap_or_default();
        let building = self
            .facts
            .sites
            .iter()
            .fold(0u32, |s, site| s.saturating_add(site.3));
        let requesting = alive < want;
        let loose = self
            .facts
            .loose
            .iter()
            .fold(0u32, |s, l| s.saturating_add(l.3));
        let in_sources = self
            .facts
            .sources
            .iter()
            .fold(0u32, |s, src| s.saturating_add(src.2));
        let mut events = Vec::new();
        let (stalled, guard, cap, recovering) = match self.rooms.get_mut(ri) {
            Some(r) => {
                let stalled = r.watch.observe(
                    &Observation {
                        time: self.time,
                        alive,
                        working,
                        want,
                        level,
                        controller: state,
                        energy: energy.available,
                        capacity: energy.capacity,
                        sources: in_sources,
                        building,
                        requesting,
                    },
                    cfg,
                    &mut events,
                );
                (
                    stalled,
                    r.watch.guard(),
                    r.watch.cap(),
                    r.watch.recovering(),
                )
            }
            None => (false, false, None, false),
        };
        self.events.extend(events.into_iter().map(|e| (name, e)));
        if stalled {
            self.replan(ri);
        }

        // The spawn request.
        let kind = if working == 0 || recovering {
            (EnergyBudget::Available, PRIORITY_RECOVERY)
        } else if working < cfg.min_workers {
            (EnergyBudget::Available, PRIORITY_LOW)
        } else if let Some(cap) = cap {
            (EnergyBudget::UpTo(cap), PRIORITY_NORMAL)
        } else {
            (EnergyBudget::Capacity, PRIORITY_NORMAL)
        };
        let request = requesting.then_some(kind);
        if let Some((budget, priority)) = request {
            let _ = spawner.request(SpawnRequest {
                home: name,
                role: Role::Worker,
                body: BodySpec::Scaled {
                    pattern: &WORKER_PATTERN,
                    max_repeats: u8::MAX,
                },
                budget,
                priority,
            });
        }

        // Modes and tasks, then the Critical tasks' intents and moves.
        self.assign(
            ri,
            snap,
            members,
            cfg,
            guard && !state.upgrade_blocked,
            state,
        );
        self.source_left.clear();
        self.source_left
            .extend(self.facts.sources.iter().map(|s| (s.0, s.2)));
        // Builds run in the Standard job after every room's Critical half: keep every room's sites.
        self.site_left.extend(
            self.facts
                .sites
                .iter()
                .map(|s| (s.0, s.4.saturating_sub(s.3))),
        );
        let mut tasks = [0u32; 8];
        for i in members.0..members.1 {
            if let Some(task) = self.task_of(i, snap) {
                if let Some(t) = tasks.get_mut(task.kind_index()) {
                    *t += 1;
                }
                if task.tier() == Tier::Critical {
                    self.act(ri, i, task, snap, movement, buffer, mcfg, commit);
                } else if let Some(m) = self.members.get(i) {
                    // Standard work runs later in the tick, if the tier runs at all: until its
                    // request replaces this, the worker may be shoved out of a Critical mover's way.
                    movement.give_way(snap, m.handle, None);
                }
            }
        }
        let sources = u32::try_from(self.facts.sources.len()).unwrap_or(u32::MAX);
        let sites = u32::try_from(self.facts.sites.len()).unwrap_or(u32::MAX);
        if let Some(r) = self.rooms.get_mut(ri) {
            r.stats = RoomStats {
                time: self.time,
                alive,
                working,
                want,
                tasks,
                request,
                sources,
                sites,
                loose,
            };
        }
        Ok(())
    }

    /// The Standard half of room `unit`'s tick: the building and upgrading the Critical half chose.
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
        let members = self
            .rooms
            .get(ri)
            .ok_or(Fault::new("no room for the unit", unit))?
            .members;
        for i in members.0..members.1 {
            if let Some(task) = self.task_of(i, snap)
                && task.tier() == Tier::Standard
            {
                self.act(ri, i, task, snap, movement, buffer, mcfg, commit);
            }
        }
        Ok(())
    }

    /// What the watchdogs reported since the last call, by room.
    pub fn drain_events(&mut self) -> impl Iterator<Item = (RoomName, Event)> + '_ {
        self.events.drain(..)
    }

    /// Each room's plan, watchdog and counters, by name.
    pub fn room_states(
        &self,
    ) -> impl Iterator<Item = (RoomName, Option<&Plan>, &Watchdog, &RoomStats)> + '_ {
        self.rooms
            .iter()
            .map(|r| (r.name, r.plan.as_ref(), &r.watch, &r.stats))
    }

    /// The task worker `handle` was given this tick, if it is one of the opening's workers.
    pub fn task(&self, handle: CreepHandle) -> Option<Task> {
        self.worker(handle).map(|w| w.task)
    }

    fn worker(&self, handle: CreepHandle) -> Option<&Worker> {
        let slot = usize::try_from(handle.slot()).ok()?;
        self.workers
            .get(slot)?
            .as_ref()
            .filter(|w| w.handle == handle)
    }

    /// The state of the worker in `members[i]`, created when its slot holds another creep or none.
    fn worker_mut(&mut self, i: usize, snap: &Snapshot) -> Option<&mut Worker> {
        let m = *self.members.get(i)?;
        let slot = usize::try_from(m.handle.slot()).ok()?;
        if self.workers.len() <= slot {
            self.workers.resize(slot + 1, None);
        }
        let cell = self.workers.get_mut(slot)?;
        if cell.is_none_or(|w| w.handle != m.handle) {
            // New, or first seen after a reset: one with energy works, an empty one gathers.
            let energy = snap.creeps.stores().get(m.row).map_or(0, |s| s.energy);
            *cell = Some(Worker {
                handle: m.handle,
                mode: if energy > 0 { Mode::Work } else { Mode::Gather },
                source: None,
                task: Task::Idle,
            });
        }
        cell.as_mut()
    }

    /// The task of `members[i]` this tick, or `None` while it is spawning.
    fn task_of(&self, i: usize, snap: &Snapshot) -> Option<Task> {
        let m = self.members.get(i)?;
        if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
            return None;
        }
        self.worker(m.handle).map(|w| w.task)
    }

    /// Drops room `ri`'s plan and its workers' sources and tasks (after a stall).
    fn replan(&mut self, ri: usize) {
        let Some(r) = self.rooms.get_mut(ri) else {
            return;
        };
        r.plan = None;
        let (start, end) = r.members;
        for i in start..end {
            let Some(m) = self.members.get(i) else {
                continue;
            };
            let slot = usize::try_from(m.handle.slot()).unwrap_or(usize::MAX);
            if let Some(Some(w)) = self.workers.get_mut(slot)
                && w.handle == m.handle
            {
                w.source = None;
                w.task = Task::Idle;
            }
        }
    }

    /// Reads what room `name` holds this tick, and its mined sources, into `self.facts`.
    fn gather_facts(&mut self, snap: &Snapshot, name: RoomName, mined: &[Mined]) {
        let f = &mut self.facts;
        f.controller = None;
        f.fills.clear();
        f.sources.clear();
        f.sites.clear();
        f.loose.clear();
        f.mined.clear();
        f.mined
            .extend(mined.iter().filter(|m| m.source.room_name() == name));
        let st = &snap.structures;
        for (((&h, &kind), &pos), &free) in st
            .handles()
            .iter()
            .zip(st.kinds())
            .zip(st.positions())
            .zip(st.free_energy())
        {
            if pos.room_name() != name {
                continue;
            }
            match kind {
                StructureType::Controller => f.controller = Some((h, pos)),
                StructureType::Spawn | StructureType::Extension if free > 0 => {
                    f.fills.push((h, pos, free));
                }
                _ => {}
            }
        }
        let ob = &snap.objects;
        for ((((&h, &kind), &pos), &amount), &total) in ob
            .handles()
            .iter()
            .zip(ob.kinds())
            .zip(ob.positions())
            .zip(ob.amounts())
            .zip(ob.totals())
        {
            if pos.room_name() != name {
                continue;
            }
            match kind {
                ObjectKind::Source => f.sources.push((h, pos, amount, total)),
                ObjectKind::Site(t) => {
                    let key = ob.key_of(h).map_or(u128::MAX, |k| k.0);
                    f.sites.push((h, pos, t, amount, total, key));
                }
                k if k.holds_energy() && amount > 0 => f.loose.push((h, pos, k, amount)),
                _ => {}
            }
        }
        // Sources in a fixed order (rows follow the engine's, V102): by place.
        f.sources.sort_unstable_by_key(|s| tile_key(s.1));
    }

    /// Builds room `ri`'s plan when it has none, or movement's grid version or the room's capacity
    /// changed. Without a grid yet (the room's terrain or first scan is still to come), the room
    /// keeps its last plan, or plans `min_workers` until then.
    #[allow(clippy::too_many_arguments)]
    fn ensure_plan(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        row: usize,
        movement: &mut Movement,
        cfg: &OpeningConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) {
        let Some(r) = self.rooms.get(ri) else {
            return;
        };
        let name = r.name;
        let capacity = snap.rooms.energy().get(row).map_or(0, |e| e.capacity);
        let Some((grid, version)) = movement.room_grid(name, mcfg, commit) else {
            return;
        };
        let controller = self.facts.controller.is_some();
        let seen = self.facts.sources.len();
        let mined = self
            .facts
            .sources
            .iter()
            .enumerate()
            .filter(|(_, s)| self.facts.mined.iter().any(|m| m.source == s.1))
            .fold(0u32, |bits, (i, _)| {
                bits | u32::try_from(i)
                    .ok()
                    .and_then(|i| 1u32.checked_shl(i))
                    .unwrap_or(0)
            });
        if r.plan.as_ref().is_some_and(|p| {
            p.version == version
                && p.capacity == capacity
                && p.controller == controller
                && p.sources.len() == seen
                && p.mined == mined
        }) {
            return;
        }
        let body = worker_body(capacity);
        let goal = self.facts.controller.map(|(_, pos)| Goal {
            tile: Tile::of(pos),
            range: WORK_RANGE,
        });
        let reached = match goal {
            Some(g) => {
                commit.heavy();
                self.field.compute(grid, g, &mut self.dial) > 0
            }
            None => false,
        };
        let mut sources = Vec::with_capacity(self.facts.sources.len());
        for (i, &(_, pos, _, source_capacity)) in self.facts.sources.iter().enumerate() {
            let is_mined = u32::try_from(i)
                .ok()
                .and_then(|i| 1u32.checked_shl(i))
                .is_some_and(|bit| mined & bit != 0);
            let at = Tile::of(pos);
            let open = at.neighbors().filter(|&(_, t)| grid.walkable(t)).count();
            let open = u32::try_from(open).unwrap_or(0);
            let cost = at
                .neighbors()
                .filter(|&(_, t)| grid.walkable(t))
                .map(|(_, t)| self.field.distance(t))
                .filter(|&d| d != UNREACHED)
                .min();
            let trip = match (reached, cost, goal) {
                (true, Some(cost), Some(g)) => Some(Trip {
                    // It stands next to the source and within range of the controller.
                    tiles: u32::from(at.range(g.tile))
                        .saturating_sub(u32::from(NEAR) + u32::from(WORK_RANGE)),
                    cost: u32::from(cost),
                }),
                _ => None,
            };
            // At a mined source a worker takes its load off the miner's tile in one tick.
            let round = if is_mined { carry_round } else { worker_round };
            let want = match (body.as_ref(), trip) {
                (Some(b), Some(t)) => round(b, t).map_or(0, |round| {
                    workers_for_source(source_capacity, open, b, round)
                }),
                _ => 0,
            };
            sources.push(SourcePlan {
                pos,
                open,
                want,
                trip,
                mined: is_mined,
            });
        }
        let sum = sources.iter().fold(0u32, |s, p| s.saturating_add(p.want));
        let want = if sum == 0 {
            0
        } else {
            sum.clamp(cfg.min_workers.min(cfg.max_workers), cfg.max_workers)
        };
        if let Some(r) = self.rooms.get_mut(ri) {
            r.plan = Some(Plan {
                version,
                capacity,
                controller,
                sources,
                mined,
                want,
            });
        }
    }

    /// Every worker's mode and task for this tick (module docs).
    fn assign(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        members: (usize, usize),
        cfg: &OpeningConfig,
        guard: bool,
        controller: crate::snapshot::ControllerState,
    ) {
        let now = self.time;
        // This tick's unreachable targets: every worker skips them.
        self.blocked.clear();
        self.private.clear();
        if let Some(r) = self.rooms.get_mut(ri) {
            r.unreachable.retain(|&(_, until, _)| now < until);
            for &(p, _, by) in &r.unreachable {
                match by {
                    None => self.blocked.push(p),
                    Some(h) => self.private.push((p, h)),
                }
            }
        }
        // Modes; spawning workers get nothing.
        self.order.clear();
        for i in members.0..members.1 {
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
                continue;
            }
            let store = snap.creeps.stores().get(m.row).copied().unwrap_or_default();
            let Some(w) = self.worker_mut(i, snap) else {
                continue;
            };
            if store.capacity == 0 {
                w.task = Task::Idle;
                continue;
            }
            if store.energy == 0 {
                w.mode = Mode::Gather;
            } else if store.free() == 0 {
                w.mode = Mode::Work;
            }
            self.order.push(i);
        }

        // Sources: each worker keeps one, new ones go where the plan wants more.
        let mut plan_sources = std::mem::take(&mut self.plan_sources);
        plan_sources.clear();
        match self.rooms.get(ri).and_then(|r| r.plan.as_ref()) {
            Some(p) => plan_sources.extend(p.sources.iter().map(|s| (s.pos, s.want))),
            None => plan_sources.extend(self.facts.sources.iter().map(|s| (s.1, 1))),
        }
        plan_sources.retain(|s| !self.blocked.contains(&s.0));
        self.loads.clear();
        self.loads.resize(plan_sources.len(), 0);
        for k in 0..self.order.len() {
            let Some(&i) = self.order.get(k) else {
                continue;
            };
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            let src = self.worker(m.handle).and_then(|w| w.source);
            if let Some(j) = src.and_then(|p| plan_sources.iter().position(|s| s.0 == p))
                && let Some(l) = self.loads.get_mut(j)
            {
                *l += 1;
            }
        }
        for k in 0..self.order.len() {
            let Some(&i) = self.order.get(k) else {
                continue;
            };
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            let has = self
                .worker(m.handle)
                .and_then(|w| w.source)
                .is_some_and(|p| {
                    plan_sources.iter().any(|s| s.0 == p) && !skipped_by(&self.private, p, m.handle)
                });
            if has {
                continue;
            }
            // The most wanted source relative to its load, then the first.
            let pick = plan_sources
                .iter()
                .enumerate()
                .filter(|(_, s)| s.1 > 0 && !skipped_by(&self.private, s.0, m.handle))
                .min_by_key(|&(j, s)| {
                    let load = self.loads.get(j).copied().unwrap_or(0);
                    // load / want, compared without division.
                    (u64::from(load) * 1000 / u64::from(s.1.max(1)), j)
                })
                .map(|(j, s)| (j, s.0));
            if let Some(w) = self.worker_mut(i, snap) {
                w.source = pick.map(|(_, pos)| pos);
            }
            if let Some((j, _)) = pick
                && let Some(l) = self.loads.get_mut(j)
            {
                *l += 1;
            }
        }
        self.plan_sources = plan_sources;

        // Gatherers first: a gatherer with energy and nothing to gather turns to work.
        let scavenge_min = cfg.scavenge_min.max(1);
        for k in 0..self.order.len() {
            let Some(&i) = self.order.get(k) else {
                continue;
            };
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            let Some(w) = self.worker(m.handle).copied() else {
                continue;
            };
            if w.mode != Mode::Gather {
                continue;
            }
            let store = snap.creeps.stores().get(m.row).copied().unwrap_or_default();
            let Some(&at) = snap.creeps.positions().get(m.row) else {
                continue;
            };
            let free = store.free();
            let task = match self.pick_gather(w, at, free, scavenge_min) {
                Some(t) => t,
                None if store.energy > 0 => {
                    if let Some(w) = self.worker_mut(i, snap) {
                        w.mode = Mode::Work;
                    }
                    continue;
                }
                // Nothing at all: wait at its mined source's tile for a load, or at its source for
                // the refill.
                None => match w.source.and_then(|p| self.mined_tile(p)) {
                    Some(tile)
                        if !self.blocked.contains(&tile)
                            && !skipped_by(&self.private, tile, w.handle) =>
                    {
                        Task::Wait(tile)
                    }
                    Some(_) => Task::Idle,
                    None => self
                        .facts
                        .sources
                        .iter()
                        .find(|s| Some(s.1) == w.source)
                        .map_or(Task::Idle, |s| Task::Harvest(s.0)),
                },
            };
            if let Task::Take(h) = task
                && let Some(l) = self.facts.loose.iter_mut().find(|l| l.0 == h)
            {
                l.3 = l.3.saturating_sub(free);
            }
            if let Some(w) = self.worker_mut(i, snap) {
                w.task = task;
            }
        }

        // Workers with energy, in passes: the guard's upgrader of last tick (until it is empty),
        // filling, the guard, `min_upgraders`, building, upgrading.
        let upgrade = self
            .facts
            .controller
            .filter(|&(_, pos)| !controller.upgrade_blocked && !self.blocked.contains(&pos));
        let mut workers = std::mem::take(&mut self.working);
        let mut assigned = std::mem::take(&mut self.assigned);
        let mut left = std::mem::take(&mut self.left);
        workers.clear();
        assigned.clear();
        left.clear();
        // Spawns and extensions that cannot be reached take no energy.
        let blocked = &self.blocked;
        self.facts.fills.retain(|f| !blocked.contains(&f.1));
        for k in 0..self.order.len() {
            let Some(&i) = self.order.get(k) else {
                continue;
            };
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            let Some(w) = self.worker(m.handle).copied() else {
                continue;
            };
            if w.mode != Mode::Work {
                continue;
            }
            let store = snap.creeps.stores().get(m.row).copied().unwrap_or_default();
            let Some(&at) = snap.creeps.positions().get(m.row) else {
                continue;
            };
            if let (Task::Upgrade { guard: true, .. }, Some((c, cpos))) = (w.task, upgrade)
                && !skipped_by(&self.private, cpos, m.handle)
            {
                assigned.push((
                    i,
                    Task::Upgrade {
                        controller: c,
                        guard: true,
                    },
                ));
                continue;
            }
            let nearest = self
                .facts
                .fills
                .iter()
                .map(|f| range(at, f.1))
                .min()
                .unwrap_or(u8::MAX);
            workers.push((i, at, store.energy, w.task, nearest));
        }
        {
            // Filling: workers that filled last tick first, then the nearest to free space.
            let fills = &mut self.facts.fills;
            let private = &self.private;
            let members = &self.members;
            let handle_of = |i: usize| members.get(i).map(|m| m.handle);
            workers.sort_by_key(|&(i, _, _, prev, nearest)| {
                (
                    !matches!(prev, Task::Fill(_)),
                    nearest,
                    members.get(i).map_or(u128::MAX, |m| m.key),
                )
            });
            for &(i, at, energy, prev, nearest) in &workers {
                let reachable = |f: &(StructureHandle, Position, u32)| {
                    f.2 > 0 && handle_of(i).is_none_or(|h| !skipped_by(private, f.1, h))
                };
                let open = fills.iter().any(reachable);
                if !open {
                    left.push((i, at, energy, prev, nearest));
                    continue;
                }
                // Keep last tick's target while it has room, else the nearest with room.
                let keep = match prev {
                    Task::Fill(h) => fills.iter().position(|f| f.0 == h && reachable(f)),
                    _ => None,
                };
                let pick = keep.or_else(|| {
                    fills
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| reachable(f))
                        .min_by_key(|(j, f)| (range(at, f.1), *j))
                        .map(|(j, _)| j)
                });
                match pick.and_then(|j| fills.get_mut(j)) {
                    Some(f) => {
                        f.2 = f.2.saturating_sub(energy);
                        assigned.push((i, Task::Fill(f.0)));
                    }
                    None => left.push((i, at, energy, prev, nearest)),
                }
            }
        }
        // The guard's upgrader: the nearest to the controller (one kept from last tick counts).
        let key_of = |members: &[Member], i: usize| members.get(i).map_or(u128::MAX, |m| m.key);
        let kept_guard = assigned
            .iter()
            .any(|(_, t)| matches!(t, Task::Upgrade { guard: true, .. }));
        let can_upgrade = |i: usize, cpos: Position| {
            self.members
                .get(i)
                .is_none_or(|m| !skipped_by(&self.private, cpos, m.handle))
        };
        if guard
            && !kept_guard
            && let Some((c, cpos)) = upgrade
        {
            let pick = left
                .iter()
                .enumerate()
                .filter(|&(_, &(i, ..))| can_upgrade(i, cpos))
                .min_by_key(|&(_, &(i, at, _, prev, _))| {
                    (
                        !matches!(prev, Task::Upgrade { .. }),
                        range(at, cpos),
                        key_of(&self.members, i),
                    )
                })
                .map(|(j, _)| j);
            if let Some(j) = pick {
                let (i, ..) = left.remove(j);
                assigned.push((
                    i,
                    Task::Upgrade {
                        controller: c,
                        guard: true,
                    },
                ));
            }
        }
        // `min_upgraders` (the guard's counts), then building, then upgrading.
        let reachable_sites = self
            .facts
            .sites
            .iter()
            .any(|s| !self.blocked.contains(&s.1));
        let mut upgraders = assigned
            .iter()
            .filter(|(_, t)| matches!(t, Task::Upgrade { .. }))
            .count();
        let min_upgraders = usize::try_from(cfg.min_upgraders).unwrap_or(usize::MAX);
        if let Some((c, cpos)) = upgrade {
            while upgraders < min_upgraders && !left.is_empty() && reachable_sites {
                let pick = left
                    .iter()
                    .enumerate()
                    .filter(|&(_, &(i, ..))| can_upgrade(i, cpos))
                    .min_by_key(|&(_, &(i, at, _, prev, _))| {
                        (
                            !matches!(prev, Task::Upgrade { .. }),
                            range(at, cpos),
                            key_of(&self.members, i),
                        )
                    })
                    .map(|(j, _)| j);
                let Some(j) = pick else {
                    break;
                };
                let (i, ..) = left.remove(j);
                assigned.push((
                    i,
                    Task::Upgrade {
                        controller: c,
                        guard: false,
                    },
                ));
                upgraders += 1;
            }
        }
        let site = best_site(&self.facts.sites, &self.blocked);
        for &(i, _, _, prev, _) in &left {
            let handle = self.members.get(i).map(|m| m.handle);
            let open = |p: Position| {
                !self.blocked.contains(&p)
                    && handle.is_none_or(|h| !skipped_by(&self.private, p, h))
            };
            let site_at = |h: ObjectHandle| self.facts.sites.iter().find(|s| s.0 == h).map(|s| s.1);
            let keep = match prev {
                Task::Build(h) => site_at(h).is_some_and(open),
                _ => false,
            };
            let task = match (
                prev,
                site.filter(|&h| site_at(h).is_some_and(open)),
                upgrade,
            ) {
                (Task::Build(h), _, _) if keep => Task::Build(h),
                (_, Some(h), _) => Task::Build(h),
                (_, None, Some((c, cpos))) if open(cpos) => Task::Upgrade {
                    controller: c,
                    guard: false,
                },
                _ => Task::Idle,
            };
            assigned.push((i, task));
        }
        for &(i, task) in &assigned {
            if let Some(w) = self.worker_mut(i, snap) {
                w.task = task;
            }
        }
        self.working = workers;
        self.assigned = assigned;
        self.left = left;
    }

    /// A gatherer's task: the nearest pile, tombstone, ruin or container with enough energy
    /// (`scavenge_min`, or what its store needs if less) that is no farther than its source, else
    /// its source while it has energy, else another source with energy, else any energy to take.
    /// Targets in this tick's unreachable list are skipped.
    ///
    /// Mined sources (P3.1) are never harvested. Energy on a miner's tile counts only once it fills
    /// the gatherer's store: the miner adds to it every tick, so one take of a whole load replaces
    /// several small ones (each a billed intent) in the same time; a take kept from the last tick
    /// follows the same rule. A gatherer whose own source is mined takes its load there, or the
    /// nearer energy found first, and otherwise waits there (`Task::Wait`), with a partial load
    /// too, so it carries whole loads.
    fn pick_gather(&self, w: Worker, at: Position, free: u32, scavenge_min: u32) -> Option<Task> {
        let enough = scavenge_min.min(free).max(1);
        let f = &self.facts;
        let open =
            |p: Position| !self.blocked.contains(&p) && !skipped_by(&self.private, p, w.handle);
        let at_mined = |p: Position| f.mined.iter().any(|m| m.tile == p);
        let mined = |p: Position| f.mined.iter().any(|m| m.source == p);
        // Last tick's take, while it still has energy for this worker (a whole load on a miner's
        // tile).
        if let Task::Take(h) = w.task
            && f.loose
                .iter()
                .any(|l| l.0 == h && l.3 > 0 && open(l.1) && (!at_mined(l.1) || l.3 >= free.max(1)))
        {
            return Some(Task::Take(h));
        }
        let worth = |l: &&(ObjectHandle, Position, ObjectKind, u32)| {
            open(l.1)
                && if at_mined(l.1) {
                    l.3 >= free.max(1)
                } else {
                    l.3 >= enough
                }
        };
        let take = f
            .loose
            .iter()
            .filter(worth)
            .min_by_key(|l| (range(at, l.1), tile_key(l.1)))
            .copied();
        if let Some(tile) = w.source.and_then(|p| self.mined_tile(p)) {
            return take
                .filter(|t| range(at, t.1) <= range(at, tile))
                .map(|t| Task::Take(t.0))
                .or_else(|| open(tile).then_some(Task::Wait(tile)));
        }
        let own = f
            .sources
            .iter()
            .find(|s| Some(s.1) == w.source && s.2 > 0 && open(s.1) && !mined(s.1))
            .copied();
        match (take, own) {
            (Some(t), Some(s)) if range(at, t.1) <= range(at, s.1) => Some(Task::Take(t.0)),
            (_, Some(s)) => Some(Task::Harvest(s.0)),
            (take, None) => f
                .sources
                .iter()
                .filter(|s| s.2 > 0 && open(s.1) && !mined(s.1))
                .min_by_key(|s| (range(at, s.1), tile_key(s.1)))
                .map(|s| Task::Harvest(s.0))
                .or_else(|| take.map(|t| Task::Take(t.0)))
                .or_else(|| {
                    f.loose
                        .iter()
                        .filter(|l| l.3 > 0 && open(l.1) && !at_mined(l.1))
                        .min_by_key(|l| (range(at, l.1), tile_key(l.1)))
                        .map(|l| Task::Take(l.0))
                }),
        }
    }

    /// The miner's tile of `source` when its miner is next to it this tick.
    fn mined_tile(&self, source: Position) -> Option<Position> {
        self.facts
            .mined
            .iter()
            .find(|m| m.source == source)
            .map(|m| m.tile)
    }

    /// Pushes `members[i]`'s intent when it is in range of its task's target, and asks movement to
    /// take it there or hold it there. A harvest or a build is pushed only while this tick's
    /// reservations (`HARVEST_POWER`, or `BUILD_POWER` capped by the builder's energy, per push)
    /// leave the target something to give; a target movement finds no way to goes on
    /// room `ri`'s unreachable list and the worker's task is dropped.
    #[allow(clippy::too_many_arguments)]
    fn act(
        &mut self,
        ri: usize,
        i: usize,
        task: Task,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) {
        let Some(m) = self.members.get(i).copied() else {
            return;
        };
        let Some(&at) = snap.creeps.positions().get(m.row) else {
            return;
        };
        let object = |h: ObjectHandle| {
            let row = snap.objects.row_of(h)?;
            Some((
                *snap.objects.positions().get(row)?,
                *snap.objects.kinds().get(row)?,
            ))
        };
        let structure = |h: StructureHandle| {
            let row = snap.structures.row_of(h)?;
            snap.structures.positions().get(row).copied()
        };
        let energy = ResourceType::Energy;
        let plan = match task {
            Task::Harvest(h) => {
                object(h).map(|(pos, _)| (pos, NEAR, Some(CreepIntent::Harvest { target: h })))
            }
            Task::Take(h) => object(h).map(|(pos, kind)| {
                let intent = if kind == ObjectKind::Energy {
                    CreepIntent::Pickup { target: h }
                } else {
                    CreepIntent::WithdrawObject {
                        target: h,
                        resource: energy,
                        amount: None,
                    }
                };
                (pos, NEAR, Some(intent))
            }),
            Task::Fill(h) => structure(h).map(|pos| {
                let intent = CreepIntent::Transfer {
                    target: Target::Structure(h),
                    resource: energy,
                    amount: None,
                };
                (pos, NEAR, Some(intent))
            }),
            Task::Build(h) => {
                object(h).map(|(pos, _)| (pos, WORK_RANGE, Some(CreepIntent::Build { target: h })))
            }
            Task::Upgrade { controller, .. } => structure(controller).map(|pos| {
                (
                    pos,
                    WORK_RANGE,
                    Some(CreepIntent::UpgradeController { target: controller }),
                )
            }),
            Task::Wait(tile) => Some((tile, NEAR, None)),
            Task::Idle => None,
        };
        let Some((target, reach, intent)) = plan else {
            movement.give_way(snap, m.handle, None);
            return;
        };
        if range(at, target) <= reach
            && let Some(intent) = intent
        {
            // What this tick has left to harvest or build here (a lower bound per push: one WORK).
            let left = match intent {
                CreepIntent::Harvest { target } => self
                    .source_left
                    .iter_mut()
                    .find(|s| s.0 == target)
                    .map(|s| (&mut s.1, HARVEST_POWER)),
                CreepIntent::Build { target } => {
                    // A build spends at most the builder's energy (V186).
                    let energy = snap.creeps.stores().get(m.row).map_or(0, |s| s.energy);
                    self.site_left
                        .iter_mut()
                        .find(|s| s.0 == target)
                        .map(|s| (&mut s.1, BUILD_POWER.min(energy)))
                }
                _ => None,
            };
            match left {
                Some((n, _)) if *n == 0 => {}
                Some((n, power)) => {
                    // A refusal is counted by the buffer; the snapshot rules most of them out.
                    if buffer.push(snap, m.handle, intent, commit).is_ok() {
                        *n = n.saturating_sub(power);
                    }
                }
                None => {
                    let _ = buffer.push(snap, m.handle, intent, commit);
                }
            }
        }
        let to = MoveTo {
            target,
            range: reach,
            priority: task.move_priority(),
            tier: task.tier(),
        };
        if movement.request(snap, m.handle, to, mcfg, commit) == Err(MoveError::NoPath) {
            let until = self.time.saturating_add(mcfg.route_retry_ticks);
            if let Some(r) = self.rooms.get_mut(ri) {
                match r.unreachable.iter_mut().find(|u| u.0 == target) {
                    // A second worker failed on it: every worker skips it now.
                    Some(u) => {
                        u.1 = until;
                        if u.2 != Some(m.handle) {
                            u.2 = None;
                        }
                    }
                    None => {
                        if r.unreachable.len() >= UNREACHABLE_MAX {
                            r.unreachable.remove(0);
                        }
                        r.unreachable.push((target, until, Some(m.handle)));
                    }
                }
            }
            if let Some(w) = self.worker_mut(i, snap) {
                w.task = Task::Idle;
                if w.source == Some(target) {
                    w.source = None;
                }
            }
        }
    }
}

/// Whether `target` is on the list of targets worker `h` skips on its own.
fn skipped_by(private: &[(Position, CreepHandle)], target: Position, h: CreepHandle) -> bool {
    private.iter().any(|&(p, by)| p == target && by == h)
}

/// The site the room's builders share: spawns first, then extensions, then everything but roads
/// and walls, then those; among them the one closest to done, then by id. Unreachable sites are
/// skipped.
fn best_site(
    sites: &[(ObjectHandle, Position, StructureType, u32, u32, u128)],
    blocked: &[Position],
) -> Option<ObjectHandle> {
    let rank = |t: StructureType| match t {
        StructureType::Spawn => 0u8,
        StructureType::Extension => 1,
        StructureType::Road | StructureType::Wall | StructureType::Rampart => 3,
        _ => 2,
    };
    sites
        .iter()
        .filter(|s| !blocked.contains(&s.1))
        .min_by_key(|s| {
            let left = s.4.saturating_sub(s.3);
            (rank(s.2), left, s.5)
        })
        .map(|s| s.0)
}

/// Chebyshev range, or `u8::MAX` between rooms.
fn range(a: Position, b: Position) -> u8 {
    if a.room_name() != b.room_name() {
        return u8::MAX;
    }
    a.x()
        .u8()
        .abs_diff(b.x().u8())
        .max(a.y().u8().abs_diff(b.y().u8()))
}

/// A stable order for objects in one room: by tile.
fn tile_key(p: Position) -> (u8, u8) {
    (p.x().u8(), p.y().u8())
}
