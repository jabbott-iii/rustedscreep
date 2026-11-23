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
//! 3. The Standard job runs [`Opening::work_room`] per room: repairing, building and upgrading.
//!
//! A worker gathers until its store is full and works until it is empty. Each tick its task is
//! chosen again, in id order with reservations, so no two workers fetch the same energy or fill the
//! same free space: first the gatherers (a pile, tombstone, ruin or container no farther than its
//! source, else its source), then the workers with energy: filling until the room's free spawn
//! energy is covered, the guard's upgrader, `min_upgraders`, repairing the containers on the mining
//! plan's tiles (P3.5: one worker per container below `repair.start_pct` of its `hitsMax`, until it
//! is full, from the workers within range of it, or any below `repair.walk_pct`), building,
//! upgrading.
//! A task kept from the last tick goes first, so assignments do not flip.
//!
//! Haulers (P3.3, `economy::hauling`): a source whose haulers work this tick is treated as a linked
//! one ([`HaulView::redirect`]): its gatherers take its energy at the controller container and its
//! workers are sized with no walk; energy the haulers take this tick is not offered to gatherers,
//! nor room they fill to fillers, nor at all a spawn or extension a hauler's transfer goes into this
//! tick (P3.4); and while a hauler hauls a mined source no worker fills, except in a recovery or
//! while the room's spawn queue waits for energy (P5.3, `Spawner::waits_for_energy`: the workers
//! then fill too, from the room's piles and containers).
//!
//! Upgraders (P3.4, `economy::upgrading`): while the room's upgraders take the energy at the
//! controller ([`UpgradeView::takes`]), a linked or hauled source gets no worker of its own (the
//! room keeps `min_workers`, which build, repair and fill), no worker is held to upgrading while
//! others build (`min_upgraders`), and a worker with nothing else to do holds its energy, unless its
//! source is one the upgraders do not count. What the workers take this tick is recorded per target
//! ([`Opening::taken`]), so the upgraders, whose job runs later in the tick, do not take it too.
//!
//! Distances are Chebyshev ranges; only sizing measures paths (movement's grid). State lives in the
//! heap: a global reset loses the modes, sources and tasks, and the next tick rebuilds them from the
//! stores (a worker with energy works, an empty one gathers). Nothing is read from or written to
//! `Memory`.

use rustedscreep_consts::{
    BUILD_POWER, CONTAINER_HITS, HARVEST_POWER, Part, Position, ResourceType, RoomName,
    StructureType, extension_energy_capacity,
};

use super::hauling::HaulView;
use super::mining::{Mined, Receiver};
use super::sizing::{
    Trip, WorkerScratch, WorkerSource, WorkerTargets, miner_work, smallest_worker, source_wants,
    worker_plan,
};
use super::upgrading::UpgradeView;
use super::watchdog::{Event, Observation, Watchdog};
use crate::config::{MovementConfig, OpeningConfig, RepairConfig};
use crate::intents::{CreepIntent, IntentBuffer, Target};
use crate::movement::{Dial, FlowField, Goal, MoveError, MoveTo, Movement, Tile, UNREACHED};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::RoomEnergy;
use crate::snapshot::{
    Controller, CreepHandle, ObjectHandle, ObjectKind, Snapshot, StructureHandle,
};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner};

/// Spawn priorities of the opening's requests (higher first): a room with no worker that can work,
/// a room below `min_workers`, and the rest.
pub const PRIORITY_RECOVERY: u8 = 200;
pub const PRIORITY_LOW: u8 = 150;
pub const PRIORITY_NORMAL: u8 = 100;

/// Upgrading and building reach 3 tiles (`inRangeTo(target, 3)`); harvesting, taking, filling 1
/// (`isNearTo`) (V108, V182, V185, V187, V190, V192).
const WORK_RANGE: u8 = 3;
const NEAR: u8 = 1;

/// How close a gatherer waits to a controller link or the controller container (P3.3): off the
/// tiles next to it, which the workers taking a load and the haulers unloading need; a worker that
/// waits next to it could not be shoved off without leaving its goal.
const WAIT_RANGE: u8 = 2;

/// What a worker does this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    Harvest(ObjectHandle),
    /// Take energy from a pile (`pickup`) or a tombstone, ruin or container (`withdraw`).
    Take(ObjectHandle),
    /// Take energy from a controller link (`withdraw`, P3.2). Counted with [`Task::Take`].
    Withdraw(StructureHandle),
    /// Transfer energy to a spawn or an extension.
    Fill(StructureHandle),
    Build(ObjectHandle),
    /// `repair` of a container (P3.5).
    Repair(ObjectHandle),
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
            | Self::Withdraw(_)
            | Self::Fill(_)
            | Self::Upgrade { guard: true, .. }
            | Self::Wait(_)
            | Self::Idle => Tier::Critical,
            Self::Build(_) | Self::Repair(_) | Self::Upgrade { guard: false, .. } => Tier::Standard,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Harvest(_) => "harvest",
            Self::Take(_) | Self::Withdraw(_) => "take",
            Self::Fill(_) => "fill",
            Self::Build(_) => "build",
            Self::Repair(_) => "repair",
            Self::Upgrade { guard: true, .. } => "guard",
            Self::Upgrade { guard: false, .. } => "upgrade",
            Self::Wait(_) => "wait",
            Self::Idle => "idle",
        }
    }

    /// Every kind, in a fixed order (the order of [`RoomStats::tasks`]).
    pub const KINDS: [&'static str; 9] = [
        "harvest", "take", "fill", "build", "repair", "guard", "upgrade", "wait", "idle",
    ];

    fn kind_index(self) -> usize {
        match self {
            Self::Harvest(_) => 0,
            Self::Take(_) | Self::Withdraw(_) => 1,
            Self::Fill(_) => 2,
            Self::Build(_) => 3,
            Self::Repair(_) => 4,
            Self::Upgrade { guard: true, .. } => 5,
            Self::Upgrade { guard: false, .. } => 6,
            Self::Wait(_) => 7,
            Self::Idle => 8,
        }
    }

    /// How hard its moves push past other creeps (`MoveTo::priority`).
    const fn move_priority(self) -> u8 {
        match self {
            Self::Fill(_) | Self::Upgrade { guard: true, .. } => 3,
            Self::Harvest(_) | Self::Take(_) | Self::Withdraw(_) | Self::Wait(_) => 2,
            Self::Build(_) | Self::Repair(_) | Self::Upgrade { guard: false, .. } => 1,
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
    /// What it refills to every `ENERGY_REGEN_TIME` ticks. Unit: energy.
    pub capacity: u32,
    /// Walkable tiles next to it on movement's grid. Unit: tiles.
    pub open: u32,
    /// Workers of the plan's body it keeps busy ([`source_wants`]). Unit: creeps.
    pub want: u32,
    /// The way from it to the controller.
    pub trip: Option<Trip>,
    /// Its miner was next to it when the plan was built (P3.1): workers take its energy off the
    /// miner's tile ([`super::sizing::carry_round`]) instead of harvesting it.
    pub mined: bool,
    /// Its miner sent its energy to a controller link (P3.2), or its haulers carried it to the
    /// controller container (P3.3, `hauled`), when the plan was built: workers take it there,
    /// within upgrading range of the controller, so their round has no walk.
    pub linked: bool,
    /// Of `linked`, its haulers carried it.
    pub hauled: bool,
}

/// A room's plan: built again when movement's grid version, the room's energy capacity, the
/// sources its miners mine, the upgraders' state or the solver's config change, or (while the
/// workers' energy is by the controller) its count of construction sites.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub version: u32,
    pub capacity: u32,
    /// The controller and the sources the plan was built with: a plan built before the room's
    /// objects were all seen is built again once they are.
    pub controller: bool,
    pub sources: Vec<SourcePlan>,
    /// The mined sources the plan was built with, by index in `sources` (bit `i`), of them the
    /// linked ones (a link or haulers carry them), and of those the hauled ones.
    pub mined: u32,
    pub linked: u32,
    pub hauled: u32,
    /// The room's upgraders took the energy at the controller (P3.4, [`UpgradeView::takes`]): the
    /// linked sources and those the upgraders count (`upgraded`, by index as `mined`) want no
    /// worker, and the room keeps `min_workers`.
    pub upgraders: bool,
    pub upgraded: u32,
    /// The most WORK parts a worker gets while the upgraders take ([`UpgradeView::worker_repeats`]).
    /// Unit: parts.
    pub max_work: u8,
    /// The room's own construction sites when the plan was built while the workers' energy is by
    /// the controller (the upgraders take, or a source is linked; 0 otherwise), and what they still
    /// needed then (one energy a progress point, V186). Unit: sites, energy.
    pub sites: u32,
    pub build: u32,
    /// A builder's way from the room's first spawn, by its sites, to the controller, where it takes
    /// its energy while the upgraders take ([`WorkerTargets::build_trip`]).
    pub build_trip: Option<Trip>,
    /// While no hauler fills (`hauled` is empty): the miners' WORK parts' cost, which the workers
    /// refill besides their own bodies ([`WorkerTargets::fill`]). Unit: energy.
    pub fill: Option<u32>,
    /// The level's `EXTENSION_ENERGY_CAPACITY`, what one transfer into a fill takes at most. Unit:
    /// energy.
    pub unload: u32,
    /// `opening.min_workers`, `opening.max_workers` and `opening.body_slack_pct` when the plan was
    /// built: the solver reads them, so an override builds the plan again.
    pub limits: (u32, u32, u32),
    /// The workers' body at `capacity`, solved from the plan's targets ([`worker_plan`], P3.6);
    /// `None` when the capacity does not pay for the smallest worker.
    pub body: Option<Body>,
    /// The body solved as the room's workers get it while the upgraders take: `body` when they
    /// do, else solved with the linked sources spent, `max_work` WORK parts at most and the build
    /// work; `None` while the upgrading job does not run for the room (`max_work` is `u8::MAX`).
    /// The upgraders' budget counts it for every worker (`upgrading::upkeep`).
    pub capped: Option<Body>,
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
    pub tasks: [u32; Task::KINDS.len()],
    /// What this tick asked of the spawner: `None`, or the budget and priority.
    pub request: Option<(EnergyBudget, u8)>,
    pub sources: u32,
    pub sites: u32,
    /// Energy in piles, tombstones, ruins and containers. Unit: energy.
    pub loose: u32,
    /// Containers on the mining plan's tiles (the ones the workers repair, P3.5), those under repair
    /// (below `repair.start_pct` of `CONTAINER_HITS`, or being repaired up to full), and the fewest
    /// hits among them (`None` without one).
    pub containers: u32,
    pub repairing: u32,
    pub lowest_hits: Option<u32>,
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
    /// Containers under repair (P3.5): below `repair.start_pct` of their `hitsMax` once, and not
    /// full since. A global reset forgets them; they are found again below the share.
    repairing: Vec<ObjectHandle>,
    /// What its workers take this tick (P3.4): each pushed pickup or withdraw's target and the
    /// taker's free room, cleared in [`Opening::begin`]. Unit: energy.
    took: Vec<(Target, u32)>,
    /// The last body solved for a request below the plan's capacity (P3.6: a recovery's energy, or
    /// the watchdog's cap), by that energy over [`part_step`], so a room that waits is not solved
    /// again every tick; cleared when the plan is built again.
    sized: Option<(u32, Option<Body>)>,
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
    /// The room's first spawn by id: its key and place (P3.6: the builders' way).
    spawn: Option<(u128, Position)>,
    /// Spawns and extensions with room for energy: handle, place, free space left to reserve.
    fills: Vec<(StructureHandle, Position, u32)>,
    /// Sources: handle, place, energy, capacity.
    sources: Vec<(ObjectHandle, Position, u32, u32)>,
    /// Own sites: handle, place, type, progress, progress total, id.
    sites: Vec<(ObjectHandle, Position, StructureType, u32, u32, u128)>,
    /// Energy to take: handle, place, kind, energy left to reserve.
    loose: Vec<(ObjectHandle, Position, ObjectKind, u32)>,
    /// Controller links (`economy::mining`, P3.2): handle, place, energy left to reserve, and
    /// whether no send fits it ([`Receiver::full`]).
    links: Vec<(StructureHandle, Position, u32, bool)>,
    /// Sources whose miner is next to them this tick (`economy::mining`, P3.1).
    mined: Vec<Mined>,
    /// Sources whose haulers work this tick (P3.3, [`HaulView::hauled`]).
    hauled: Vec<Position>,
    /// Containers on the mining plan's tiles (P3.5): handle, place, hits. Others in the room (a
    /// previous owner's, say) are never repaired; their energy is still taken (`loose`).
    containers: Vec<(ObjectHandle, Position, u32)>,
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
    /// The room's containers to repair this tick (P3.5), as in `Facts::containers`.
    repairs: Vec<(ObjectHandle, Position, u32)>,
    /// This tick's unreachable targets of the room being run: for every worker, and for one.
    blocked: Vec<Position>,
    private: Vec<(Position, CreepHandle)>,
    /// What is left this tick of each source's energy and each site's progress, less a lower
    /// bound of what each harvest or build already pushed takes (`HARVEST_POWER`, `BUILD_POWER`
    /// for one WORK part), so a target that is used up gets no more billed no-ops.
    source_left: Vec<(ObjectHandle, u32)>,
    site_left: Vec<(ObjectHandle, u32)>,
    /// Scratch for the worker solver (P3.6): the plan's sources as it sees them, and the solver's.
    worker_sources: Vec<WorkerSource>,
    solver: WorkerScratch,
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
                        repairing: Vec::new(),
                        took: Vec::new(),
                        sized: None,
                    },
                ),
            }
        }
        self.rooms.retain(|r| r.row.is_some());
        for r in &mut self.rooms {
            r.took.clear();
        }
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

    fn room(&self, room: RoomName) -> Option<&RoomState> {
        self.rooms
            .binary_search_by_key(&room.packed_repr(), |r| r.name.packed_repr())
            .ok()
            .and_then(|i| self.rooms.get(i))
    }

    /// `room`'s plan, from its last run.
    pub fn plan(&self, room: RoomName) -> Option<&Plan> {
        self.room(room).and_then(|r| r.plan.as_ref())
    }

    /// What `room`'s workers take this tick (P3.4): per pushed `pickup` or `withdraw`, its target
    /// and the taker's free room; empty before the room's Critical run. Unit: energy.
    pub fn taken(&self, room: RoomName) -> &[(Target, u32)] {
        self.room(room).map_or(&[], |r| &r.took[..])
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
    /// sources whose miner is next to them this tick (`Mining::mined`; empty without miners),
    /// `mine_tiles` the mining plan's tiles (`Mining::mine_tiles`), whose containers are repaired,
    /// `haul` what the room's haulers do this tick (`Hauling::view`; the default without), and
    /// `upgrade` whether its upgraders take the energy at the controller (`Upgrading::view`).
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        buffer: &mut IntentBuffer,
        cfg: &OpeningConfig,
        rcfg: &RepairConfig,
        mcfg: &MovementConfig,
        tower_refill_min: u32,
        mined: &[Mined],
        mine_tiles: &[Position],
        receivers: &[Receiver],
        haul: &HaulView<'_>,
        upgrade: &UpgradeView<'_>,
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
        self.gather_facts(
            snap,
            name,
            mined,
            mine_tiles,
            receivers,
            haul,
            tower_refill_min,
        );
        // The capacity the room's workers are sized at: its own; for an owned room without a spawn
        // (one that lost it), at most that of the nearest spawn room that would serve it (P5.1), so
        // its worker fits there and its plan stays put while the nearest room does.
        let own = snap.rooms.energy().get(row).copied().unwrap_or_default();
        let helper = spawner.spawn_side(name).filter(|side| side.room != name);
        let capacity = helper.map_or(own.capacity, |side| own.capacity.min(side.energy.capacity));
        self.ensure_plan(
            ri, snap, row, capacity, movement, cfg, mcfg, upgrade, commit,
        );
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

        // The spawn request. Below `min_workers` a room spawns at the energy it holds so someone
        // fills its spawns, unless its haulers do (P3.3).
        let kind = if working == 0 || recovering {
            (EnergyBudget::Available, PRIORITY_RECOVERY)
        } else if working < cfg.min_workers && !haul.fills {
            (EnergyBudget::Available, PRIORITY_LOW)
        } else if let Some(cap) = cap {
            (EnergyBudget::UpTo(cap), PRIORITY_NORMAL)
        } else {
            (EnergyBudget::Capacity, PRIORITY_NORMAL)
        };
        let request = requesting.then_some(kind);
        if let Some((budget, priority)) = request
            && let Some(body) = {
                // A spawn of the room is free this tick: only then can a body solved below the
                // plan's capacity be ordered, so only then is one solved. A room without a spawn
                // that another serves asks for its plan's body, solved at the capacity above: a
                // room serves another only on a tick it starts full (P5.1), so that body is what
                // it can pay, and no stand-in solved for a busy spawn is ordered there (with no
                // plan yet, a recovery asks for the smallest worker, as in any room).
                let (at, ready) = match (helper, spawner.spawn_side(name)) {
                    (Some(_), _) => (
                        RoomEnergy {
                            available: capacity,
                            capacity,
                        },
                        true,
                    ),
                    (None, Some(side)) => (energy, side.idle),
                    (None, None) => (energy, false),
                };
                self.request_body(ri, budget, at, ready, cfg, commit)
            }
        {
            let _ = spawner.request(SpawnRequest {
                home: name,
                role: Role::Worker,
                remote: None,
                body: BodySpec::Exact(body),
                budget,
                priority,
            });
        }

        // Modes and tasks, then the Critical tasks' intents and moves. Working haulers fill the
        // room (P3.3), except in a recovery, or while its spawn queue waits for energy (P5.3: then
        // the workers fill too, from the room's piles and containers).
        let refill = spawner.waits_for_energy(name);
        self.assign(
            ri,
            snap,
            members,
            cfg,
            rcfg,
            guard && !state.upgrade_blocked,
            state,
            !haul.fills || recovering || refill,
            upgrade.takes,
            upgrade.sources,
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
        let mut tasks = [0u32; Task::KINDS.len()];
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
        let containers = u32::try_from(self.facts.containers.len()).unwrap_or(u32::MAX);
        let lowest_hits = self.facts.containers.iter().map(|c| c.2).min();
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
                containers,
                repairing: u32::try_from(r.repairing.len()).unwrap_or(u32::MAX),
                lowest_hits,
            };
        }
        Ok(())
    }

    /// The Standard half of room `unit`'s tick: the repairing, building and upgrading the Critical
    /// half chose.
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

    /// Reads what room `name` holds this tick, its mined sources (a hauled one's energy at the
    /// controller container), the containers on its mining tiles and its controller links, into
    /// `self.facts`; energy the haulers take this tick is left out.
    #[allow(clippy::too_many_arguments)]
    fn gather_facts(
        &mut self,
        snap: &Snapshot,
        name: RoomName,
        mined: &[Mined],
        mine_tiles: &[Position],
        receivers: &[Receiver],
        haul: &HaulView<'_>,
        tower_refill_min: u32,
    ) {
        let f = &mut self.facts;
        f.controller = None;
        f.spawn = None;
        f.fills.clear();
        f.sources.clear();
        f.sites.clear();
        f.loose.clear();
        f.links.clear();
        f.mined.clear();
        f.hauled.clear();
        f.hauled.extend_from_slice(haul.hauled);
        f.containers.clear();
        f.mined.extend(
            mined
                .iter()
                .filter(|m| m.source.room_name() == name)
                .map(|&m| haul.redirect(m)),
        );
        f.links.extend(
            receivers
                .iter()
                .filter(|r| r.pos.room_name() == name && r.energy > 0)
                .map(|r| (r.link, r.pos, r.energy, r.full)),
        );
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
            if kind == StructureType::Spawn {
                let key = st.key_of(h).map_or(u128::MAX, |k| k.0);
                if f.spawn.is_none_or(|(k, _)| key < k) {
                    f.spawn = Some((key, pos));
                }
            }
            match kind {
                StructureType::Controller => f.controller = Some((h, pos)),
                StructureType::Spawn | StructureType::Extension if free > 0 => {
                    // Less what the haulers fill this tick (P3.3), and none a hauler's transfer
                    // goes into this tick (P3.4): a worker's transfer fills all the room it finds
                    // (no amount), so the hauler's, processed after it, would find the structure
                    // full and be billed for nothing.
                    let claim = haul.filled.iter().find(|x| x.0 == h);
                    let filled = claim.map_or(0, |x| x.1);
                    if !claim.is_some_and(|x| x.2) && free > filled {
                        f.fills.push((h, pos, free - filled));
                    }
                }
                _ => {}
            }
        }
        // P7.2: the towers, once every spawn and extension is full, each with at least
        // `hauling.tower_refill_min` room, less what the haulers fill this tick.
        if f.fills.is_empty() {
            super::hauling::push_towers(&mut f.fills, snap, name, tower_refill_min);
            f.fills.retain_mut(|t| {
                let claim = haul.filled.iter().find(|x| x.0 == t.0);
                t.2 = t.2.saturating_sub(claim.map_or(0, |x| x.1));
                !claim.is_some_and(|x| x.2) && t.2 >= tower_refill_min
            });
        }
        let ob = &snap.objects;
        for (((((&h, &kind), &pos), &amount), &total), &hits) in ob
            .handles()
            .iter()
            .zip(ob.kinds())
            .zip(ob.positions())
            .zip(ob.amounts())
            .zip(ob.totals())
            .zip(ob.hits())
        {
            if pos.room_name() != name {
                continue;
            }
            if kind == ObjectKind::Container && mine_tiles.contains(&pos) {
                f.containers.push((h, pos, hits));
            }
            match kind {
                ObjectKind::Source => f.sources.push((h, pos, amount, total)),
                ObjectKind::Site(t) => {
                    let key = ob.key_of(h).map_or(u128::MAX, |k| k.0);
                    f.sites.push((h, pos, t, amount, total, key));
                }
                k if k.holds_energy() && amount > 0 => {
                    let taken = haul.taken.iter().find(|t| t.0 == h).map_or(0, |t| t.1);
                    if amount > taken {
                        f.loose.push((h, pos, k, amount - taken));
                    }
                }
                _ => {}
            }
        }
        // Sources in a fixed order (rows follow the engine's, V102): by place.
        f.sources.sort_unstable_by_key(|s| tile_key(s.1));
    }

    /// Builds room `ri`'s plan when it has none, or movement's grid version or `capacity` changed
    /// (the capacity its workers are sized at: the room's own, or for a room without a spawn at
    /// most that of the spawn room nearest it, P5.1). Without a grid yet (the room's terrain or first scan is still to come), the room
    /// keeps its last plan, or plans `min_workers` until then.
    #[allow(clippy::too_many_arguments)]
    fn ensure_plan(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        row: usize,
        capacity: u32,
        movement: &mut Movement,
        cfg: &OpeningConfig,
        mcfg: &MovementConfig,
        upgrade: &UpgradeView<'_>,
        commit: &mut Commit,
    ) {
        let upgraders = upgrade.takes;
        let Some(r) = self.rooms.get(ri) else {
            return;
        };
        let name = r.name;
        let Some((grid, version)) = movement.room_grid(name, mcfg, commit) else {
            return;
        };
        let controller = self.facts.controller.is_some();
        let seen = self.facts.sources.len();
        let mask = |linked: bool, hauled: bool| {
            self.facts
                .sources
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    self.facts.mined.iter().any(|m| {
                        m.source == s.1
                            && (!linked || m.link.is_some())
                            && (!hauled || self.facts.hauled.contains(&s.1))
                    })
                })
                .fold(0u32, |bits, (i, _)| {
                    bits | u32::try_from(i)
                        .ok()
                        .and_then(|i| 1u32.checked_shl(i))
                        .unwrap_or(0)
                })
        };
        let (mined, linked, hauled) = (mask(false, false), mask(true, false), mask(true, true));
        let upgraded = self
            .facts
            .sources
            .iter()
            .enumerate()
            .filter(|(_, s)| upgraders && upgrade.sources.contains(&s.1))
            .fold(0u32, |bits, (i, _)| {
                bits | u32::try_from(i)
                    .ok()
                    .and_then(|i| 1u32.checked_shl(i))
                    .unwrap_or(0)
            });
        // The cap applies while the upgraders take; the capped body is solved with it always.
        let max_work = upgrade.worker_repeats;
        // The build work sizes the workers while their energy is by the controller (the upgraders
        // take, or a source is linked: `WorkerTargets::duties`), so a change in the sites then
        // builds the plan again.
        let sites = if upgraders || linked != 0 {
            u32::try_from(self.facts.sites.len()).unwrap_or(u32::MAX)
        } else {
            0
        };
        if r.plan.as_ref().is_some_and(|p| {
            p.version == version
                && p.capacity == capacity
                && p.controller == controller
                && p.sources.len() == seen
                && p.mined == mined
                && p.linked == linked
                && p.hauled == hauled
                && p.upgraders == upgraders
                && p.upgraded == upgraded
                && p.max_work == max_work
                && p.sites == sites
                && p.limits == (cfg.min_workers, cfg.max_workers, cfg.body_slack_pct)
        }) {
            return;
        }
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
        // The way from `at` to within upgrading range of the controller, when the field reaches a
        // tile next to it: from a tile next to `at` (a worker stands next to a source).
        let trip_from = |at: Tile| {
            let cost = at
                .neighbors()
                .filter(|&(_, t)| grid.walkable(t))
                .map(|(_, t)| self.field.distance(t))
                .filter(|&d| d != UNREACHED)
                .min();
            match (reached, cost, goal) {
                (true, Some(cost), Some(g)) => Some(Trip {
                    // It stands next to `at` and within range of the controller.
                    tiles: u32::from(at.range(g.tile))
                        .saturating_sub(u32::from(NEAR) + u32::from(WORK_RANGE)),
                    cost: u32::from(cost),
                }),
                _ => None,
            }
        };
        let mut sources = Vec::with_capacity(self.facts.sources.len());
        for (i, &(_, pos, _, source_capacity)) in self.facts.sources.iter().enumerate() {
            let bit = u32::try_from(i).ok().and_then(|i| 1u32.checked_shl(i));
            let at = Tile::of(pos);
            let open = at.neighbors().filter(|&(_, t)| grid.walkable(t)).count();
            sources.push(SourcePlan {
                pos,
                capacity: source_capacity,
                open: u32::try_from(open).unwrap_or(0),
                want: 0,
                trip: trip_from(at),
                mined: bit.is_some_and(|bit| mined & bit != 0),
                linked: bit.is_some_and(|bit| linked & bit != 0),
                hauled: bit.is_some_and(|bit| hauled & bit != 0),
            });
        }
        // The builders' way (P3.6): the interim extensions and most sites are by the first spawn
        // (ADR-020), the energy they take while the upgraders take is by the controller.
        let build_trip = self
            .facts
            .spawn
            .and_then(|(_, pos)| trip_from(Tile::of(pos)));
        let build = self.facts.sites.iter().fold(0u32, |b, site| {
            b.saturating_add(site.4.saturating_sub(site.3))
        });
        // While no hauler fills, the workers refill the miners' bodies too (their WORK parts, the
        // most of them: a lower bound).
        let fill = (hauled == 0).then(|| {
            sources.iter().filter(|p| p.mined).fold(0u32, |e, p| {
                e.saturating_add(Part::Work.cost().saturating_mul(miner_work(p.capacity)))
            })
        });
        let level = match snap.rooms.controllers().get(row) {
            Some(&Controller::Mine { level }) => u32::from(level),
            _ => 0,
        };
        let mut plan = Plan {
            version,
            capacity,
            controller,
            sources,
            mined,
            linked,
            hauled,
            upgraders,
            upgraded,
            max_work,
            sites,
            build,
            build_trip,
            fill,
            unload: extension_energy_capacity(level),
            limits: (cfg.min_workers, cfg.max_workers, cfg.body_slack_pct),
            body: None,
            capped: None,
            want: 0,
        };
        // The bodies (P3.6): the search is the plan's heavy part with the field.
        commit.heavy();
        worker_sources(&plan, &mut self.worker_sources);
        let own = targets(&plan, &self.worker_sources, cfg, upgraders);
        plan.body = worker_plan(&own, capacity, &mut self.solver);
        // The upgraders' budget reads it only for a room the upgrading job runs (its cap below
        // `u8::MAX`); elsewhere the second search is skipped.
        plan.capped = if upgraders {
            plan.body
        } else if max_work == u8::MAX {
            None
        } else {
            let takes = targets(&plan, &self.worker_sources, cfg, true);
            worker_plan(&takes, capacity, &mut self.solver)
        };
        let wants = &mut self.solver.wants;
        wants.clear();
        if let Some(b) = plan.body.as_ref() {
            source_wants(&own, b, wants);
        }
        let mut sum = 0u32;
        for (p, &w) in plan.sources.iter_mut().zip(wants.iter()) {
            p.want = w;
            sum = sum.saturating_add(w);
        }
        // With upgraders the room still keeps `min_workers` to build, repair and fill (P3.4).
        plan.want = own.room_want(sum);
        if let Some(r) = self.rooms.get_mut(ri) {
            r.plan = Some(plan);
            r.sized = None;
        }
    }

    /// The body room `ri` requests a worker with this tick, for `budget` and the room's `energy`
    /// (P3.6): the plan's body at its capacity, else the body solved from the plan's targets at the
    /// energy the budget allows (cached in [`RoomState::sized`]), else (below the smallest worker's
    /// cost) the smallest worker, which waits for its cost. While no spawn of the room is `ready`
    /// the last solved body (or the smallest worker) stands in, unsolved: it cannot be ordered this
    /// tick. With no plan, the smallest worker for a recovery and none otherwise.
    fn request_body(
        &mut self,
        ri: usize,
        budget: EnergyBudget,
        energy: RoomEnergy,
        ready: bool,
        cfg: &OpeningConfig,
        commit: &mut Commit,
    ) -> Option<Body> {
        let Self {
            rooms,
            worker_sources: seen,
            solver,
            ..
        } = self;
        let r = rooms.get_mut(ri)?;
        // With no plan (no grid yet, or a stall's plan dropped this tick) only a recovery spawns,
        // with the smallest worker: a request at capacity waits for the plan, so no small worker
        // is spawned for a whole life.
        let Some(plan) = r.plan.as_ref() else {
            return match budget {
                EnergyBudget::Available => smallest_worker(),
                EnergyBudget::Capacity | EnergyBudget::UpTo(_) => None,
            };
        };
        let at = match budget {
            EnergyBudget::Capacity => energy.capacity,
            EnergyBudget::UpTo(cap) => cap.min(energy.capacity),
            EnergyBudget::Available => energy.available.min(energy.capacity),
        };
        if at >= plan.capacity {
            return plan.body.or_else(smallest_worker);
        }
        let key = at / part_step();
        if let Some((k, body)) = r.sized
            && (k == key || !ready)
        {
            return body.or_else(smallest_worker);
        }
        if !ready {
            return smallest_worker();
        }
        // A solve is the heavy part of the tick's run for this room (as in `ensure_plan`).
        commit.heavy();
        worker_sources(plan, seen);
        let t = targets(plan, seen, cfg, plan.upgraders);
        let body = worker_plan(&t, at, solver);
        r.sized = Some((key, body));
        body.or_else(smallest_worker)
    }

    /// Every worker's mode and task for this tick (module docs).
    #[allow(clippy::too_many_arguments)]
    fn assign(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        members: (usize, usize),
        cfg: &OpeningConfig,
        rcfg: &RepairConfig,
        guard: bool,
        controller: crate::snapshot::ControllerState,
        fill: bool,
        upgraders_take: bool,
        counted: &[Position],
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
            if let Task::Withdraw(h) = task
                && let Some(l) = self.facts.links.iter_mut().find(|l| l.0 == h)
            {
                l.2 = l.2.saturating_sub(free);
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
        // Spawns and extensions that cannot be reached take no energy; none while haulers fill.
        let blocked = &self.blocked;
        self.facts.fills.retain(|f| fill && !blocked.contains(&f.1));
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
        // The containers to repair (P3.5): below `repair.start_pct` of their `hitsMax`, or under
        // repair and not full. The room remembers them before the unreachable ones are left out, so
        // one that is cut off for a while is repaired up to full once it is reached again.
        let mut repairs = std::mem::take(&mut self.repairs);
        repairs.clear();
        if rcfg.enabled {
            let repairing = self.rooms.get(ri).map_or(&[][..], |r| &r.repairing[..]);
            repairs.extend(
                self.facts
                    .containers
                    .iter()
                    .filter(|c| needs_repair(c.2, rcfg.start_pct, repairing.contains(&c.0)))
                    .copied(),
            );
        }
        if let Some(r) = self.rooms.get_mut(ri) {
            r.repairing.clear();
            r.repairing.extend(repairs.iter().map(|c| c.0));
        }
        repairs.retain(|c| !self.blocked.contains(&c.1));
        // `min_upgraders` (the guard's counts), then repairing, building, upgrading.
        let other_work = !repairs.is_empty()
            || self
                .facts
                .sites
                .iter()
                .any(|s| !self.blocked.contains(&s.1));
        let mut upgraders = assigned
            .iter()
            .filter(|(_, t)| matches!(t, Task::Upgrade { .. }))
            .count();
        // With upgraders at the controller (P3.4) no worker is held to upgrading.
        let min_upgraders = if upgraders_take {
            0
        } else {
            usize::try_from(cfg.min_upgraders).unwrap_or(usize::MAX)
        };
        if let Some((c, cpos)) = upgrade {
            while upgraders < min_upgraders && !left.is_empty() && other_work {
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
        // One worker per container, from the workers already within repair range of it (at an
        // unlinked source its gatherers take their loads there), or below `repair.walk_pct` any (a
        // linked source's container, which no gatherer comes to). First last tick's repairers keep
        // their container while they still may repair it, so none swaps between two; then each
        // container left, fewest hits first, takes the nearest. `can_upgrade` tells whether the
        // worker skips a target on its own.
        let may_repair = |i: usize, at: Position, c: &(ObjectHandle, Position, u32)| {
            can_upgrade(i, c.1) && (below_share(c.2, rcfg.walk_pct) || range(at, c.1) <= WORK_RANGE)
        };
        let mut k = 0;
        while k < left.len() {
            let Some(&(i, at, _, prev, _)) = left.get(k) else {
                break;
            };
            let kept = match prev {
                Task::Repair(h) => repairs
                    .iter()
                    .position(|c| c.0 == h && may_repair(i, at, c)),
                _ => None,
            };
            match kept {
                Some(j) => {
                    let (h, ..) = repairs.swap_remove(j);
                    left.remove(k);
                    assigned.push((i, Task::Repair(h)));
                }
                None => k += 1,
            }
        }
        while !left.is_empty() {
            let Some(j) = repairs
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| (c.2, tile_key(c.1)))
                .map(|(j, _)| j)
            else {
                break;
            };
            let c = repairs.swap_remove(j);
            let pick = left
                .iter()
                .enumerate()
                .filter(|&(_, &(i, at, ..))| may_repair(i, at, &c))
                .min_by_key(|&(_, &(i, at, ..))| (range(at, c.1), key_of(&self.members, i)))
                .map(|(k, _)| k);
            let h = c.0;
            if let Some(k) = pick {
                let (i, ..) = left.remove(k);
                assigned.push((i, Task::Repair(h)));
            }
        }
        self.repairs = repairs;
        let site = best_site(&self.facts.sites, &self.blocked);
        let holds = |i: usize| {
            upgraders_take
                && self
                    .members
                    .get(i)
                    .and_then(|m| self.worker(m.handle))
                    .and_then(|w| w.source)
                    .is_none_or(|src| counted.contains(&src))
        };
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
                // With upgraders at the controller (P3.4) a worker with nothing else to do holds
                // its energy for the next site: an upgrade by a small worker bills an intent for a
                // few energy the upgraders would spend anyway. Not a worker whose source the
                // upgraders do not count (say, an unhauled source while hauling is off): no one
                // else spends that energy.
                (_, None, Some((c, cpos))) if open(cpos) && !holds(i) => Task::Upgrade {
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
    /// too, so it carries whole loads. A controller link (P3.2) fills by whole sends, so it follows
    /// the same rule as a miner's tile until no send fits it ([`Receiver::full`]): then its energy
    /// comes only once a worker takes some, so `enough` is worth a take. A gatherer whose mined
    /// source sends its energy there ([`Mined::link`]) takes and waits next to it.
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
        let link_worth = |l: &(StructureHandle, Position, u32, bool)| {
            open(l.1) && (l.2 >= free.max(1) || (l.3 && l.2 >= enough))
        };
        if let Task::Withdraw(h) = w.task
            && f.links.iter().any(|l| l.0 == h && link_worth(l))
        {
            return Some(Task::Withdraw(h));
        }
        let worth = |l: &&(ObjectHandle, Position, ObjectKind, u32)| {
            open(l.1)
                && if at_mined(l.1) {
                    l.3 >= free.max(1)
                } else {
                    l.3 >= enough
                }
        };
        let object = f
            .loose
            .iter()
            .filter(worth)
            .map(|l| (Task::Take(l.0), l.1))
            .min_by_key(|t| (range(at, t.1), tile_key(t.1)));
        let link = f
            .links
            .iter()
            .filter(|l| link_worth(l))
            .map(|l| (Task::Withdraw(l.0), l.1))
            .min_by_key(|t| (range(at, t.1), tile_key(t.1)));
        let take = match (object, link) {
            (Some(o), Some(l)) => Some(
                if (range(at, l.1), tile_key(l.1)) < (range(at, o.1), tile_key(o.1)) {
                    l
                } else {
                    o
                },
            ),
            (o, l) => o.or(l),
        };
        if let Some(tile) = w.source.and_then(|p| self.mined_tile(p)) {
            // At a controller link or the controller container (P3.3) the other one serves as well:
            // both are within upgrading range.
            let receiver = |p: Position| f.mined.iter().any(|m| m.link == Some(p));
            let serves =
                |p: Position| range(at, p) <= range(at, tile) || (receiver(tile) && receiver(p));
            return take
                .filter(|t| serves(t.1))
                .map(|t| t.0)
                .or_else(|| open(tile).then_some(Task::Wait(tile)));
        }
        let own = f
            .sources
            .iter()
            .find(|s| Some(s.1) == w.source && s.2 > 0 && open(s.1) && !mined(s.1))
            .copied();
        match (take, own) {
            (Some(t), Some(s)) if range(at, t.1) <= range(at, s.1) => Some(t.0),
            (_, Some(s)) => Some(Task::Harvest(s.0)),
            (take, None) => f
                .sources
                .iter()
                .filter(|s| s.2 > 0 && open(s.1) && !mined(s.1))
                .min_by_key(|s| (range(at, s.1), tile_key(s.1)))
                .map(|s| Task::Harvest(s.0))
                .or_else(|| take.map(|t| t.0))
                .or_else(|| {
                    f.loose
                        .iter()
                        .filter(|l| l.3 > 0 && open(l.1) && !at_mined(l.1))
                        .min_by_key(|l| (range(at, l.1), tile_key(l.1)))
                        .map(|l| Task::Take(l.0))
                }),
        }
    }

    /// Where the energy of `source` lands when its miner is next to it this tick: the miner's tile,
    /// or the controller link it sends to (P3.2).
    fn mined_tile(&self, source: Position) -> Option<Position> {
        self.facts
            .mined
            .iter()
            .find(|m| m.source == source)
            .map(|m| m.link.unwrap_or(m.tile))
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
            Task::Withdraw(h) => structure(h).map(|pos| {
                let intent = CreepIntent::Withdraw {
                    target: h,
                    resource: energy,
                    amount: None,
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
            Task::Repair(h) => object(h).map(|(pos, _)| {
                (
                    pos,
                    WORK_RANGE,
                    Some(CreepIntent::RepairObject { target: h }),
                )
            }),
            Task::Upgrade { controller, .. } => structure(controller).map(|pos| {
                (
                    pos,
                    WORK_RANGE,
                    Some(CreepIntent::UpgradeController { target: controller }),
                )
            }),
            Task::Wait(tile) => {
                let receiver = self.facts.mined.iter().any(|m| m.link == Some(tile));
                Some((tile, if receiver { WAIT_RANGE } else { NEAR }, None))
            }
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
                    let taken = match intent {
                        CreepIntent::Pickup { target }
                        | CreepIntent::WithdrawObject { target, .. } => {
                            Some(Target::Object(target))
                        }
                        CreepIntent::Withdraw { target, .. } => Some(Target::Structure(target)),
                        _ => None,
                    };
                    if buffer.push(snap, m.handle, intent, commit).is_ok()
                        && let Some(t) = taken
                        && let Some(r) = self.rooms.get_mut(ri)
                    {
                        let free = snap.creeps.stores().get(m.row).map_or(0, |s| s.free());
                        r.took.push((t, free));
                    }
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

/// Whether a container with `hits` is repaired this tick (P3.5): below `start_pct` of its `hitsMax`,
/// or `under_repair` (below it once since) and not yet full.
///
/// Uses: CONTAINER_HITS, a container's `hitsMax` (V223).
pub fn needs_repair(hits: u32, start_pct: u32, under_repair: bool) -> bool {
    hits < CONTAINER_HITS && (below_share(hits, start_pct) || under_repair)
}

/// Whether a container's `hits` are below `pct` percent of its `hitsMax`.
///
/// Uses: CONTAINER_HITS, a container's `hitsMax` (V223).
pub fn below_share(hits: u32, pct: u32) -> bool {
    u64::from(hits) < u64::from(CONTAINER_HITS) * u64::from(pct) / 100
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

/// The plan's sources as the worker solver sees them (P3.6), into `out`: those the upgraders spend
/// are the linked ones and the ones they count (`upgraded`).
fn worker_sources(plan: &Plan, out: &mut Vec<WorkerSource>) {
    out.clear();
    out.extend(plan.sources.iter().enumerate().map(|(i, s)| {
        let bit = u32::try_from(i).ok().and_then(|i| 1u32.checked_shl(i));
        WorkerSource {
            capacity: s.capacity,
            open: s.open,
            trip: s.trip,
            mined: s.mined,
            linked: s.linked,
            spent: s.linked || bit.is_some_and(|bit| plan.upgraded & bit != 0),
            // A miner works from its mining tile's container (ADR-023), which the workers repair.
            container: s.mined,
        }
    }));
}

/// What `plan`'s workers are sized for (P3.6), as they are while the upgraders take or not
/// (`upgraders`): the cap on their WORK parts applies only while they do.
fn targets<'a>(
    plan: &Plan,
    sources: &'a [WorkerSource],
    cfg: &OpeningConfig,
    upgraders: bool,
) -> WorkerTargets<'a> {
    WorkerTargets {
        sources,
        upgraders,
        min: cfg.min_workers,
        max: cfg.max_workers,
        max_work: if upgraders {
            u32::from(plan.max_work)
        } else {
            u32::MAX
        },
        build: plan.build,
        build_trip: plan.build_trip,
        fill: plan.fill,
        unload: plan.unload,
        slack_pct: cfg.body_slack_pct,
    }
}

/// The step every worker body's cost is a multiple of: the greatest common divisor of the WORK,
/// CARRY and MOVE costs (`BODYPART_COST`, V170), so the solver gives the same body for every energy
/// between two multiples. Unit: energy.
///
/// Uses: `BODYPART_COST` (`Part::cost`).
fn part_step() -> u32 {
    const fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    gcd(
        gcd(Part::Work.cost(), Part::Carry.cost()),
        Part::Move.cost(),
    )
    .max(1)
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
