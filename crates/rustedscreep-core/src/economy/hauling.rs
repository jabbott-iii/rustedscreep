//! Haulers (ROADMAP P3.3, ADR-025): creeps of CARRY and MOVE parts that carry a mined source's
//! energy from its mining tile to the room's spawns and extensions, then to the controller
//! container, so the opening's workers stop walking between the sources and the controller.
//!
//! A source gets haulers once its miner is paid for (`economy::mining`'s plan gives it a body), no
//! link carries its energy ([`MineSource::linked`]), and the room's controller container stands
//! (a container at [`MinePlan::controller_container`]). Their body and count come from the source's
//! yield and the longer of its two trips, the mining plan's way from the spawn and this plan's way
//! to the container, with each load handed over at most the level's extension capacity a tick
//! ([`hauler_plan`], with `hauling.margin_pct` to spare and at most `hauling.max_per_source`).
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Hauling::begin`] after the snapshot: the owned rooms (the jobs' units, by name) and each
//!    room's haulers (own creeps whose name says `hauler` and that room, by id).
//! 2. The Critical `hauling` job runs [`Hauling::plan_room`] per room, after `mining` and before
//!    `opening` and the spawn job: the room's plan (built again when movement's grid version, the
//!    room's capacity, the container's place, the mining plan's sources or the sizing's two values
//!    change), each hauler's source, one spawn request per source short of haulers while its miner
//!    is next to it, every hauler's mode and task, and the Critical tasks' intents and moves: taking
//!    at the mining tile, waiting there, and filling.
//! 3. The Standard `hauling work` job runs [`Hauling::work_room`] per room with a hauler to store
//!    this tick ([`Hauling::work_count`]): the transfers into the controller container (hauling,
//!    which CLAUDE.md puts in Standard).
//!
//! A hauler gathers until its store is full and delivers until it is empty. Gathering, it takes the
//! pile on its source's mining tile first (piles decay, V193), else a whole load from the container
//! there, else waits next to the tile; one whose source has no miner next to it delivers what it
//! holds. Delivering, it fills the spawns and extensions with room (nearest first, reserved per
//! tick; last tick's fillers first), else the controller container while it has room. Its intent
//! goes out on the tick it starts next to the target; its task is chosen again next tick.
//!
//! The opening reads [`Hauling::view`]: the sources whose haulers work this tick (its workers take
//! that energy at the controller container, sized with no walk, as at a linked source), whether a
//! hauler works in the room (its workers then fill nothing), and the energy the haulers took this
//! tick (so no worker takes it too).
//!
//! Haulers keep their source and mode between ticks in the heap; after a global reset they get a
//! source again (the most wanted one relative to its haulers) and a mode from their store. Nothing
//! is read from or written to `Memory`.

use rustedscreep_consts::{
    CARRY_CAPACITY, CONTAINER_CAPACITY, Position, ResourceType, RoomName, StructureType,
    extension_energy_capacity,
};

use super::mining::{MinePlan, MineSource, Mined};
use super::sizing::{Trip, hauler_plan};
use crate::config::{HaulingConfig, MovementConfig};
use crate::intents::{CreepIntent, IntentBuffer, Target};
use crate::movement::{Dial, FlowField, Goal, MoveError, MoveTo, Movement, Tile, UNREACHED};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{
    Controller, CreepHandle, ObjectHandle, ObjectKind, Snapshot, StructureHandle,
};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner};

/// The spawn priority of a hauler request: below a miner's (`PRIORITY_MINER`, whose source a hauler
/// would wait at), above the opening's normal worker requests (`PRIORITY_NORMAL`), below a room
/// short of workers or recovering (`PRIORITY_LOW`, `PRIORITY_RECOVERY`).
pub const PRIORITY_HAULER: u8 = 115;

/// Taking, filling and transferring reach 1 tile (`isNearTo`, V108, V190, V191, V192).
const NEAR: u8 = 1;

/// Targets movement found no way to that a room remembers at once.
const UNREACHABLE_MAX: usize = 16;

/// What a hauler does this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaulTask {
    /// `pickup` of the pile on its source's mining tile, or `withdraw` from the container there.
    Take(ObjectHandle),
    /// Next to its source's mining tile while too little energy lies there for a whole load.
    Wait(Position),
    /// `transfer` to a spawn or an extension.
    Fill(StructureHandle),
    /// `transfer` to the controller container.
    Store(ObjectHandle),
    /// Nothing to do: it makes way.
    Idle,
}

impl HaulTask {
    /// The tier of the job that runs it: what keeps the spawns filled is Critical (as the opening's
    /// taking and filling), the controller's share is hauling, Standard.
    pub const fn tier(self) -> Tier {
        match self {
            Self::Take(_) | Self::Wait(_) | Self::Fill(_) | Self::Idle => Tier::Critical,
            Self::Store(_) => Tier::Standard,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Take(_) => "take",
            Self::Wait(_) => "wait",
            Self::Fill(_) => "fill",
            Self::Store(_) => "store",
            Self::Idle => "idle",
        }
    }

    /// Every kind, in a fixed order (the order of [`HaulStats::tasks`]).
    pub const KINDS: [&'static str; 5] = ["take", "wait", "fill", "store", "idle"];

    const fn kind_index(self) -> usize {
        match self {
            Self::Take(_) => 0,
            Self::Wait(_) => 1,
            Self::Fill(_) => 2,
            Self::Store(_) => 3,
            Self::Idle => 4,
        }
    }

    /// How hard its moves push past other creeps (`MoveTo::priority`): above the opening's workers
    /// that wait or take at the container (2), below a miner (4).
    const fn move_priority(self) -> u8 {
        match self {
            Self::Fill(_) | Self::Store(_) => 3,
            Self::Take(_) | Self::Wait(_) => 2,
            Self::Idle => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Gather,
    Deliver,
}

/// One source in a room's hauling plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HaulSource {
    pub source: Position,
    /// Its mining tile, where its miner stands and its haulers take.
    pub tile: Position,
    /// The trip its haulers are sized for: the longer, in tiles and in cost, of the mining plan's
    /// way from the spawn and the way from next to the tile to next to the controller container.
    pub trip: Trip,
    /// Its haulers' body at the room's capacity ([`hauler_plan`]), `None` when none fits.
    pub body: Option<Body>,
    /// Haulers it gets: [`hauler_plan`]'s count, at most `hauling.max_per_source`. Unit: creeps.
    pub want: u32,
}

/// One source of the mining plan as a hauling plan was built from it (part of its key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HaulInput {
    pub source: Position,
    pub tile: Option<Position>,
    /// The mining plan's way from the spawn to the tile.
    pub trip: Option<Trip>,
    /// A link carries its energy ([`MineSource::linked`]).
    pub linked: bool,
    /// Its miner is paid for (the mining plan has a body for it).
    pub paid: bool,
}

impl HaulInput {
    fn of(s: &MineSource) -> Self {
        Self {
            source: s.source,
            tile: s.tile,
            trip: s.trip,
            linked: s.linked,
            paid: s.body.is_some(),
        }
    }
}

/// A room's hauling plan: built again when its key changes (module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HaulPlan {
    pub version: u32,
    pub capacity: u32,
    /// The most one transfer puts into a fill: the level's `EXTENSION_ENERGY_CAPACITY`.
    pub unload: u32,
    /// The controller container the plan was built for.
    pub container: Position,
    pub margin_pct: u32,
    pub max_per_source: u32,
    /// The mining plan's sources it was built from.
    pub inputs: Vec<HaulInput>,
    pub sources: Vec<HaulSource>,
}

/// One room's counters for the console, from its last run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HaulStats {
    pub time: u32,
    /// Haulers of the room, spawning ones included, and of them the ones out of the spawn.
    pub haulers: u32,
    pub working: u32,
    /// Haulers the plan wants, over its sources.
    pub want: u32,
    /// Hauler requests made this tick.
    pub requested: u32,
    /// Haulers per task, in [`HaulTask::KINDS`] order.
    pub tasks: [u32; HaulTask::KINDS.len()],
    /// Sources whose haulers work this tick ([`HaulView::hauled`]).
    pub hauled: u32,
    /// The controller container stands.
    pub container: bool,
}

/// What the opening reads of a room's haulers this tick ([`Hauling::view`]). The default (no
/// hauler, nothing hauled) is what a room with no hauling run this tick gets.
#[derive(Clone, Copy, Debug, Default)]
pub struct HaulView<'a> {
    /// The controller container's place, where the hauled sources' energy goes.
    pub container: Option<Position>,
    /// Mined sources whose haulers work this tick: the opening's workers take their energy at the
    /// container instead of the mining tile.
    pub hauled: &'a [Position],
    /// A hauler of the room out of the spawn hauls a source mined this tick (`hauled` is not
    /// empty): the spawns and extensions are its to fill.
    pub fills: bool,
    /// Energy the haulers take this tick, per object: no worker should take it too. Unit: energy.
    pub taken: &'a [(ObjectHandle, u32)],
    /// Room the haulers fill this tick, per spawn or extension, and whether a hauler's transfer
    /// into it goes out this tick (one is next to it): a worker that fills too (in a recovery, or
    /// while no source is hauled) leaves the room claimed to them, and the whole structure while a
    /// transfer goes out (a worker's transfer, with no amount, would fill all it finds and leave
    /// the hauler's, processed after it, billed for nothing; P3.4). Unit: energy.
    pub filled: &'a [(StructureHandle, u32, bool)],
}

impl HaulView<'_> {
    /// `mined` as the opening should see it: a hauled source's energy goes to the controller
    /// container ([`Mined::link`]) unless a link carries it already.
    pub fn redirect(&self, m: Mined) -> Mined {
        match (m.link, self.container) {
            (None, Some(c)) if self.hauled.contains(&m.source) => Mined { link: Some(c), ..m },
            _ => m,
        }
    }
}

struct HaulRoom {
    name: RoomName,
    row: Option<usize>,
    plan: Option<HaulPlan>,
    /// Members of [`Hauling::members`] that belong to it this tick.
    members: (usize, usize),
    /// This tick's view (cleared in [`Hauling::begin`], so a room whose run faulted shows none).
    container: Option<Position>,
    hauled: Vec<Position>,
    fills: bool,
    taken: Vec<(ObjectHandle, u32)>,
    filled: Vec<(StructureHandle, u32, bool)>,
    /// Targets movement found no way to, each skipped until the tick given
    /// (`movement.route_retry_ticks` later). At most [`UNREACHABLE_MAX`], oldest dropped first.
    unreachable: Vec<(Position, u32)>,
    stats: HaulStats,
}

/// One hauler of this tick, in the order the jobs walk them: by room, then by id.
#[derive(Clone, Copy, Debug)]
struct Member {
    room: usize,
    key: u128,
    row: usize,
    handle: CreepHandle,
}

/// A hauler's state between ticks, in the slot of its handle.
#[derive(Clone, Copy, Debug)]
struct Hauler {
    handle: CreepHandle,
    mode: Mode,
    /// The source it hauls for (by position: sources never move).
    source: Option<Position>,
    task: HaulTask,
}

/// Hauling's state for one global.
#[derive(Default)]
pub struct Hauling {
    time: u32,
    rooms: Vec<HaulRoom>,
    members: Vec<Member>,
    /// Indexed by creep handle slot.
    haulers: Vec<Option<Hauler>>,
    field: FlowField,
    dial: Dial,
    /// Scratch, kept for its capacity: the room's spawns and extensions with room (handle, place,
    /// room left to reserve), its sources (place, capacity), the piles and containers on its plan's
    /// mining tiles (handle, place, kind, energy left to reserve), the controller container (handle,
    /// place, room left to reserve), each plan source's haulers this tick, and the delivering
    /// haulers (member index, range to the nearest fill, last tick's task).
    fills: Vec<(StructureHandle, Position, u32)>,
    sources: Vec<(Position, u32)>,
    loose: Vec<(ObjectHandle, Position, ObjectKind, u32)>,
    container: Option<(ObjectHandle, Position, u32)>,
    loads: Vec<u32>,
    delivering: Vec<(usize, u8, HaulTask)>,
    /// The plan's sources of the room being run (source, want, mining tile), its unreachable
    /// targets, and the hauled sources being gathered (swapped into the room's view).
    wants: Vec<(Position, u32, Position)>,
    blocked: Vec<Position>,
    hauled: Vec<Position>,
    /// The rooms (by index) with a Standard task this tick: the `hauling work` job's units.
    work_rooms: Vec<usize>,
}

impl Hauling {
    /// Starts a tick: the owned rooms, by name, and their haulers, by room and id.
    pub fn begin(&mut self, snap: &Snapshot) {
        self.time = snap.tick.time;
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
                    HaulRoom {
                        name,
                        row: Some(row),
                        plan: None,
                        members: (0, 0),
                        container: None,
                        hauled: Vec::new(),
                        fills: false,
                        taken: Vec::new(),
                        filled: Vec::new(),
                        unreachable: Vec::new(),
                        stats: HaulStats::default(),
                    },
                ),
            }
        }
        self.rooms.retain(|r| r.row.is_some());
        self.work_rooms.clear();
        for r in &mut self.rooms {
            // Until the room's run this tick, nothing is hauled.
            r.container = None;
            r.hauled.clear();
            r.fills = false;
            r.taken.clear();
            r.filled.clear();
        }
        self.members.clear();
        let creeps = &snap.creeps;
        for (row, identity) in creeps.identities().iter().enumerate() {
            let Some(id) = identity.filter(|id| id.role == Role::Hauler) else {
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
            // A sorted insert, by room then id: no sort instantiation for a few creeps.
            let at = self
                .members
                .partition_point(|m| (m.room, m.key) < (room, key));
            self.members.insert(
                at,
                Member {
                    room,
                    key,
                    row,
                    handle,
                },
            );
        }
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

    pub fn room_count(&self) -> u32 {
        u32::try_from(self.rooms.len()).unwrap_or(u32::MAX)
    }

    /// The rooms whose run this tick gave a hauler a Standard task: the units of
    /// [`Hauling::work_room`].
    pub fn work_count(&self) -> u32 {
        u32::try_from(self.work_rooms.len()).unwrap_or(u32::MAX)
    }

    /// The name of room `unit`.
    pub fn room_name(&self, unit: u32) -> Option<RoomName> {
        self.rooms.get(usize::try_from(unit).ok()?).map(|r| r.name)
    }

    fn room(&self, room: RoomName) -> Option<&HaulRoom> {
        self.rooms
            .binary_search_by_key(&room.packed_repr(), |r| r.name.packed_repr())
            .ok()
            .and_then(|i| self.rooms.get(i))
    }

    /// What the opening reads of `room`'s haulers this tick ([`HaulView`]); nothing before the
    /// room's run, or when it faulted.
    pub fn view(&self, room: RoomName) -> HaulView<'_> {
        self.room(room)
            .map_or_else(HaulView::default, |r| HaulView {
                container: r.container,
                hauled: &r.hauled,
                fills: r.fills,
                taken: &r.taken,
                filled: &r.filled,
            })
    }

    /// `room`'s plan, from its last run (P3.4's upgraders count its haulers).
    pub fn plan(&self, room: RoomName) -> Option<&HaulPlan> {
        self.room(room).and_then(|r| r.plan.as_ref())
    }

    /// `room`'s counters, from its last run.
    pub fn stats(&self, room: RoomName) -> Option<&HaulStats> {
        self.room(room).map(|r| &r.stats)
    }

    /// Each room's plan and counters, by name.
    pub fn room_states(&self) -> impl Iterator<Item = (RoomName, Option<&HaulPlan>, &HaulStats)> {
        self.rooms
            .iter()
            .map(|r| (r.name, r.plan.as_ref(), &r.stats))
    }

    /// The task hauler `handle` was given this tick, if it is one of the room's haulers.
    pub fn task(&self, handle: CreepHandle) -> Option<HaulTask> {
        self.hauler(handle).map(|h| h.task)
    }

    /// The Critical half of room `unit`'s tick (module docs, step 2). `mine` is the room's mining
    /// plan (`Mining::plan`) and `mined` its sources whose miner is next to them this tick
    /// (`Mining::mined`); `cap`, the most energy the opening's watchdog saw the room hold while it
    /// never filled (`Watchdog::cap`), holds back a hauler that costs more.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        buffer: &mut IntentBuffer,
        cfg: &HaulingConfig,
        mcfg: &MovementConfig,
        mine: Option<&MinePlan>,
        mined: &[Mined],
        cap: Option<u32>,
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
        let now = self.time;
        // No hauler and nothing at the controller container's tile: nothing to plan or move, so no
        // scan of the room's objects (most rooms below `hauling.container_level`).
        if members.0 == members.1 && !mine.is_some_and(|m| m.controller_store) {
            if let Some(r) = self.rooms.get_mut(ri) {
                r.plan = None;
                r.stats = HaulStats {
                    time: now,
                    ..HaulStats::default()
                };
            }
            return Ok(());
        }
        self.gather(snap, name, mine);
        self.ensure_plan(ri, snap, row, movement, cfg, mcfg, mine, commit);
        self.wants.clear();
        self.blocked.clear();
        if let Some(r) = self.rooms.get_mut(ri) {
            r.unreachable.retain(|&(_, until)| now < until);
            self.blocked.extend(r.unreachable.iter().map(|u| u.0));
            if let Some(p) = &r.plan {
                self.wants
                    .extend(p.sources.iter().map(|s| (s.source, s.want, s.tile)));
            }
        }
        self.assign_sources(snap, members);

        // Who is there, and the requests: one per source short of haulers while its miner is next
        // to it (its haulers would only wait otherwise).
        let mut stats = HaulStats {
            time: now,
            haulers: u32::try_from(members.1.saturating_sub(members.0)).unwrap_or(u32::MAX),
            container: self.container.is_some(),
            ..HaulStats::default()
        };
        for m in self.members.get(members.0..members.1).unwrap_or(&[]) {
            if !snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
                stats.working = stats.working.saturating_add(1);
            }
        }
        if let Some(plan) = self.rooms.get(ri).and_then(|r| r.plan.as_ref()) {
            for (j, s) in plan.sources.iter().enumerate() {
                stats.want = stats.want.saturating_add(s.want);
                let have = self.loads.get(j).copied().unwrap_or(0);
                if cfg.enabled
                    && have < s.want
                    && mined.iter().any(|m| m.source == s.source)
                    && let Some(body) = s.body
                    && cap.is_none_or(|c| body.cost() <= c)
                {
                    let asked = spawner.request(SpawnRequest {
                        home: name,
                        role: Role::Hauler,
                        body: BodySpec::Exact(body),
                        budget: EnergyBudget::Capacity,
                        priority: PRIORITY_HAULER,
                    });
                    if asked.is_some() {
                        stats.requested = stats.requested.saturating_add(1);
                    }
                }
            }
        }

        self.assign_tasks(ri, snap, members, mined);
        let mut standard = false;
        for i in members.0..members.1 {
            let Some(task) = self.task_of(i, snap) else {
                continue;
            };
            if let Some(t) = stats.tasks.get_mut(task.kind_index()) {
                *t = t.saturating_add(1);
            }
            if task.tier() == Tier::Critical {
                self.act(ri, i, task, snap, movement, buffer, mcfg, commit);
            } else if let Some(m) = self.members.get(i) {
                // The Standard job moves it later in the tick, if the tier runs at all.
                movement.give_way(snap, m.handle, None);
                standard = true;
            }
        }
        if standard && !self.work_rooms.contains(&ri) {
            self.work_rooms.push(ri);
        }

        // The view for the opening: the sources one of whose haulers is out of the spawn.
        let container = self.container.map(|c| c.1);
        let mut hauled = std::mem::take(&mut self.hauled);
        hauled.clear();
        for i in members.0..members.1 {
            let Some(m) = self.members.get(i) else {
                continue;
            };
            if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
                continue;
            }
            if let Some(src) = self.hauler(m.handle).and_then(|h| h.source)
                && !hauled.contains(&src)
                && mined.iter().any(|x| x.source == src)
            {
                hauled.push(src);
            }
        }
        if let Some(r) = self.rooms.get_mut(ri) {
            stats.hauled = u32::try_from(hauled.len()).unwrap_or(u32::MAX);
            // Only a hauler whose source is planned and mined this tick can fill: one waiting at a
            // source whose miner is gone, or with no source to haul, leaves the filling to the
            // workers.
            r.container = container.filter(|_| !hauled.is_empty());
            r.fills = !hauled.is_empty();
            std::mem::swap(&mut r.hauled, &mut hauled);
            r.stats = stats;
        }
        self.hauled = hauled;
        Ok(())
    }

    /// The Standard half of the tick for unit `unit` of [`Hauling::work_count`]: that room's
    /// transfers into the controller container.
    pub fn work_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let ri = usize::try_from(unit)
            .ok()
            .and_then(|u| self.work_rooms.get(u).copied())
            .ok_or(Fault::new("unit out of range", unit))?;
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

    fn hauler(&self, handle: CreepHandle) -> Option<&Hauler> {
        let slot = usize::try_from(handle.slot()).ok()?;
        self.haulers
            .get(slot)?
            .as_ref()
            .filter(|h| h.handle == handle)
    }

    /// The state of the hauler in `members[i]`, created when its slot holds another creep or none:
    /// one with energy delivers, an empty one gathers.
    fn hauler_mut(&mut self, i: usize, snap: &Snapshot) -> Option<&mut Hauler> {
        let m = *self.members.get(i)?;
        let slot = usize::try_from(m.handle.slot()).ok()?;
        if self.haulers.len() <= slot {
            self.haulers.resize(slot + 1, None);
        }
        let cell = self.haulers.get_mut(slot)?;
        if cell.is_none_or(|h| h.handle != m.handle) {
            let energy = snap.creeps.stores().get(m.row).map_or(0, |s| s.energy);
            *cell = Some(Hauler {
                handle: m.handle,
                mode: if energy > 0 {
                    Mode::Deliver
                } else {
                    Mode::Gather
                },
                source: None,
                task: HaulTask::Idle,
            });
        }
        cell.as_mut()
    }

    /// The task of `members[i]` this tick, or `None` while it is spawning.
    fn task_of(&self, i: usize, snap: &Snapshot) -> Option<HaulTask> {
        let m = self.members.get(i)?;
        if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
            return None;
        }
        self.hauler(m.handle).map(|h| h.task)
    }

    /// Reads room `name`'s spawns and extensions with room, its sources, the piles and containers
    /// on its mining plan's tiles and its controller container into the scratch lists.
    fn gather(&mut self, snap: &Snapshot, name: RoomName, mine: Option<&MinePlan>) {
        self.fills.clear();
        self.sources.clear();
        self.loose.clear();
        self.container = None;
        let st = &snap.structures;
        for (((&h, &kind), &pos), &free) in st
            .handles()
            .iter()
            .zip(st.kinds())
            .zip(st.positions())
            .zip(st.free_energy())
        {
            if pos.room_name() == name
                && matches!(kind, StructureType::Spawn | StructureType::Extension)
                && free > 0
            {
                self.fills.push((h, pos, free));
            }
        }
        let tile_of =
            |p: Position| mine.is_some_and(|m| m.sources.iter().any(|s| s.tile == Some(p)));
        let container_at = mine.and_then(|m| m.controller_container);
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
                ObjectKind::Source => self.sources.push((pos, total)),
                ObjectKind::Container if Some(pos) == container_at => {
                    self.container = Some((h, pos, CONTAINER_CAPACITY.saturating_sub(amount)));
                }
                ObjectKind::Container | ObjectKind::Energy if tile_of(pos) && amount > 0 => {
                    self.loose.push((h, pos, kind, amount));
                }
                _ => {}
            }
        }
    }

    /// Builds room `ri`'s plan when its key changed (module docs), or drops it while the room has
    /// no mining plan or no controller container. Without a grid yet the room keeps its last plan.
    #[allow(clippy::too_many_arguments)]
    fn ensure_plan(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        row: usize,
        movement: &mut Movement,
        cfg: &HaulingConfig,
        mcfg: &MovementConfig,
        mine: Option<&MinePlan>,
        commit: &mut Commit,
    ) {
        let Some(r) = self.rooms.get_mut(ri) else {
            return;
        };
        let name = r.name;
        let (Some(mine), Some((_, container, _))) = (mine, self.container) else {
            r.plan = None;
            return;
        };
        let capacity = snap.rooms.energy().get(row).map_or(0, |e| e.capacity);
        let level = match snap.rooms.controllers().get(row) {
            Some(&Controller::Mine { level }) => level,
            _ => 0,
        };
        let unload = extension_energy_capacity(u32::from(level));
        let Some((grid, version)) = movement.room_grid(name, mcfg, commit) else {
            return;
        };
        let same_inputs = |p: &HaulPlan| {
            p.inputs.len() == mine.sources.len()
                && p.inputs
                    .iter()
                    .zip(&mine.sources)
                    .all(|(a, s)| *a == HaulInput::of(s))
        };
        if r.plan.as_ref().is_some_and(|p| {
            p.version == version
                && p.capacity == capacity
                && p.unload == unload
                && p.container == container
                && p.margin_pct == cfg.margin_pct
                && p.max_per_source == cfg.max_per_source
                && same_inputs(p)
        }) {
            return;
        }
        commit.heavy();
        let goal = Goal {
            tile: Tile::of(container),
            range: NEAR,
        };
        let reached = self.field.compute(grid, goal, &mut self.dial) > 0;
        let mut sources = Vec::with_capacity(mine.sources.len());
        for s in &mine.sources {
            let (Some(tile), false, true) = (s.tile, s.linked, s.body.is_some()) else {
                continue;
            };
            let at = Tile::of(tile);
            // It stands next to the mining tile (the miner stands on it) and next to the container.
            let cost = at
                .neighbors()
                .filter(|&(_, t)| grid.walkable(t))
                .map(|(_, t)| self.field.distance(t))
                .filter(|&d| d != UNREACHED)
                .min();
            let (true, Some(cost)) = (reached, cost) else {
                continue;
            };
            let to_container = Trip {
                tiles: u32::from(at.range(goal.tile)).saturating_sub(2 * u32::from(NEAR)),
                cost: u32::from(cost),
            };
            let trip = s.trip.map_or(to_container, |t| Trip {
                tiles: t.tiles.max(to_container.tiles),
                cost: t.cost.max(to_container.cost),
            });
            let capacity_of = self
                .sources
                .iter()
                .find(|x| x.0 == s.source)
                .map_or(0, |x| x.1);
            let planned = hauler_plan(capacity_of, trip, capacity, cfg.margin_pct, unload);
            sources.push(HaulSource {
                source: s.source,
                tile,
                trip,
                body: planned.map(|p| p.0),
                want: planned.map_or(0, |p| p.1.min(cfg.max_per_source)),
            });
        }
        if let Some(r) = self.rooms.get_mut(ri) {
            r.plan = Some(HaulPlan {
                version,
                capacity,
                unload,
                container,
                margin_pct: cfg.margin_pct,
                max_per_source: cfg.max_per_source,
                inputs: mine.sources.iter().map(HaulInput::of).collect(),
                sources,
            });
        }
    }

    /// Each hauler's source for this tick: one kept from the last tick while its source is in the
    /// plan with haulers wanted, else the most wanted source relative to its haulers (`self.wants`).
    /// `self.loads` holds each plan source's haulers (spawning ones included).
    fn assign_sources(&mut self, snap: &Snapshot, members: (usize, usize)) {
        self.loads.clear();
        self.loads.resize(self.wants.len(), 0);
        let kept_at = |h: Option<&Hauler>, wants: &[(Position, u32, Position)]| {
            h.and_then(|h| h.source)
                .and_then(|p| wants.iter().position(|w| w.0 == p && w.1 > 0))
        };
        for i in members.0..members.1 {
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            if let Some(j) = kept_at(self.hauler(m.handle), &self.wants)
                && let Some(l) = self.loads.get_mut(j)
            {
                *l += 1;
            }
        }
        for i in members.0..members.1 {
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            if kept_at(self.hauler(m.handle), &self.wants).is_some() {
                continue;
            }
            // load / want, compared without division; the first on a tie.
            let loads = &self.loads;
            let pick = self
                .wants
                .iter()
                .enumerate()
                .filter(|(_, w)| w.1 > 0)
                .min_by_key(|&(j, w)| {
                    let load = loads.get(j).copied().unwrap_or(0);
                    (u64::from(load) * 1000 / u64::from(w.1.max(1)), j)
                })
                .map(|(j, w)| (j, w.0));
            if let Some((j, _)) = pick
                && let Some(l) = self.loads.get_mut(j)
            {
                *l += 1;
            }
            if let Some(h) = self.hauler_mut(i, snap) {
                h.source = pick.map(|p| p.1);
            }
        }
    }

    /// Every hauler's mode and task for this tick (module docs).
    fn assign_tasks(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        members: (usize, usize),
        mined: &[Mined],
    ) {
        let blocked = std::mem::take(&mut self.blocked);
        let mut delivering = std::mem::take(&mut self.delivering);
        delivering.clear();
        for i in members.0..members.1 {
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
                continue;
            }
            let store = snap.creeps.stores().get(m.row).copied().unwrap_or_default();
            let Some(&at) = snap.creeps.positions().get(m.row) else {
                continue;
            };
            let Some(h) = self.hauler_mut(i, snap).copied() else {
                continue;
            };
            let mut mode = h.mode;
            if store.capacity == 0 {
                self.set_task(i, snap, mode, HaulTask::Idle);
                continue;
            }
            if store.energy == 0 {
                mode = Mode::Gather;
            } else if store.free() == 0 {
                mode = Mode::Deliver;
            }
            if mode == Mode::Gather {
                let free = store.free();
                let tile = h
                    .source
                    .and_then(|src| self.wants.iter().find(|w| w.0 == src).map(|w| w.2));
                let mined_now = h
                    .source
                    .is_some_and(|src| mined.iter().any(|x| x.source == src));
                let task = tile.filter(|t| !blocked.contains(t)).and_then(|tile| {
                    // A whole load in one take: the pile first (it decays, V193), else the
                    // container; else the pile when the container makes up the rest next tick.
                    // Behind a full container the miner's overflow makes a small pile every tick,
                    // so taking any pile would be a billed pickup a tick and never empty it.
                    let at_tile = |kind: ObjectKind| {
                        self.loose
                            .iter()
                            .position(|l| l.1 == tile && l.2 == kind && l.3 > 0)
                    };
                    let left =
                        |k: Option<usize>| k.and_then(|k| self.loose.get(k)).map_or(0, |l| l.3);
                    let (pile, container) =
                        (at_tile(ObjectKind::Energy), at_tile(ObjectKind::Container));
                    let (p, c) = (left(pile), left(container));
                    let pick = if p >= free {
                        pile
                    } else if c >= free {
                        container
                    } else if p > 0 && p.saturating_add(c) >= free {
                        pile
                    } else {
                        None
                    };
                    match pick {
                        Some(k) => {
                            let l = self.loose.get_mut(k)?;
                            l.3 = l.3.saturating_sub(free);
                            Some(HaulTask::Take(l.0))
                        }
                        None if store.energy > 0 && !mined_now => None,
                        None => Some(HaulTask::Wait(tile)),
                    }
                });
                match task {
                    Some(t) => {
                        if let HaulTask::Take(o) = t {
                            self.claim(ri, o, free);
                        }
                        self.set_task(i, snap, Mode::Gather, t);
                        continue;
                    }
                    None if store.energy > 0 => mode = Mode::Deliver,
                    None => {
                        self.set_task(i, snap, Mode::Gather, HaulTask::Idle);
                        continue;
                    }
                }
            }
            let nearest = self
                .fills
                .iter()
                .filter(|f| !blocked.contains(&f.1))
                .map(|f| range(at, f.1))
                .min()
                .unwrap_or(u8::MAX);
            if let Some(hm) = self.hauler_mut(i, snap) {
                hm.mode = mode;
            }
            // Last tick's fillers first, then the nearest to a fill, then by id: a sorted insert.
            let key = |&(k, near, prev): &(usize, u8, HaulTask)| {
                (!matches!(prev, HaulTask::Fill(_)), near, k)
            };
            let entry = (i, nearest, h.task);
            let pos = delivering.partition_point(|e| key(e) < key(&entry));
            delivering.insert(pos, entry);
        }
        for &(i, _, prev) in &delivering {
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            let energy = snap.creeps.stores().get(m.row).map_or(0, |s| s.energy);
            let Some(&at) = snap.creeps.positions().get(m.row) else {
                continue;
            };
            let open = |f: &(StructureHandle, Position, u32)| f.2 > 0 && !blocked.contains(&f.1);
            let keep = match prev {
                HaulTask::Fill(h) => self.fills.iter().position(|f| f.0 == h && open(f)),
                _ => None,
            };
            let pick = keep.or_else(|| {
                self.fills
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| open(f))
                    .min_by_key(|(j, f)| (range(at, f.1), *j))
                    .map(|(j, _)| j)
            });
            let task = match pick.and_then(|j| self.fills.get_mut(j)) {
                Some(f) => {
                    let claim = energy.min(f.2);
                    f.2 = f.2.saturating_sub(energy);
                    let h = f.0;
                    let now = range(at, f.1) <= NEAR;
                    if let Some(r) = self.rooms.get_mut(ri) {
                        match r.filled.iter_mut().find(|x| x.0 == h) {
                            Some(x) => {
                                x.1 = x.1.saturating_add(claim);
                                x.2 |= now;
                            }
                            None => r.filled.push((h, claim, now)),
                        }
                    }
                    HaulTask::Fill(h)
                }
                // The container only with room for its load or a CARRY part's worth: no walk to move
                // a few energy (a billed transfer).
                None => match self.container.as_mut() {
                    Some(c)
                        if c.2 >= energy.clamp(1, CARRY_CAPACITY) && !blocked.contains(&c.1) =>
                    {
                        c.2 = c.2.saturating_sub(energy);
                        HaulTask::Store(c.0)
                    }
                    _ => HaulTask::Idle,
                },
            };
            self.set_task(i, snap, Mode::Deliver, task);
        }
        self.delivering = delivering;
        self.blocked = blocked;
    }

    /// Records that a hauler of room `ri` takes up to `free` energy from `object` this tick.
    fn claim(&mut self, ri: usize, object: ObjectHandle, free: u32) {
        if let Some(r) = self.rooms.get_mut(ri) {
            match r.taken.iter_mut().find(|t| t.0 == object) {
                Some(t) => t.1 = t.1.saturating_add(free),
                None => r.taken.push((object, free)),
            }
        }
    }

    fn set_task(&mut self, i: usize, snap: &Snapshot, mode: Mode, task: HaulTask) {
        if let Some(h) = self.hauler_mut(i, snap) {
            h.mode = mode;
            h.task = task;
        }
    }

    /// Pushes `members[i]`'s intent when it is next to its task's target, and asks movement to take
    /// it there or hold it there. A target movement finds no way to goes on room `ri`'s unreachable
    /// list and the hauler's task is dropped.
    #[allow(clippy::too_many_arguments)]
    fn act(
        &mut self,
        ri: usize,
        i: usize,
        task: HaulTask,
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
            HaulTask::Take(h) => object(h).map(|(pos, kind)| {
                let intent = if kind == ObjectKind::Energy {
                    CreepIntent::Pickup { target: h }
                } else {
                    CreepIntent::WithdrawObject {
                        target: h,
                        resource: energy,
                        amount: None,
                    }
                };
                (pos, Some(intent))
            }),
            HaulTask::Fill(h) => structure(h).map(|pos| {
                let intent = CreepIntent::Transfer {
                    target: Target::Structure(h),
                    resource: energy,
                    amount: None,
                };
                (pos, Some(intent))
            }),
            HaulTask::Store(h) => object(h).map(|(pos, _)| {
                let intent = CreepIntent::Transfer {
                    target: Target::Object(h),
                    resource: energy,
                    amount: None,
                };
                (pos, Some(intent))
            }),
            HaulTask::Wait(tile) => Some((tile, None)),
            HaulTask::Idle => None,
        };
        let Some((target, intent)) = plan else {
            movement.give_way(snap, m.handle, None);
            return;
        };
        if range(at, target) <= NEAR
            && let Some(intent) = intent
        {
            // A refusal is counted by the buffer; the snapshot rules most of them out.
            let _ = buffer.push(snap, m.handle, intent, commit);
        }
        let to = MoveTo {
            target,
            range: NEAR,
            priority: task.move_priority(),
            tier: task.tier(),
        };
        if movement.request(snap, m.handle, to, mcfg, commit) == Err(MoveError::NoPath) {
            let until = self.time.saturating_add(mcfg.route_retry_ticks);
            if let Some(r) = self.rooms.get_mut(ri) {
                if r.unreachable.len() >= UNREACHABLE_MAX {
                    r.unreachable.remove(0);
                }
                r.unreachable.push((target, until));
            }
            if let Some(h) = self.hauler_mut(i, snap) {
                h.task = HaulTask::Idle;
            }
        }
    }
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
