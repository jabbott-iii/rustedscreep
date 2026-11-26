//! Upgraders (ROADMAP P3.4): WORK-heavy creeps that stand at posts next to the room's controller
//! container (P3.3, ADR-025) and controller links (P3.2, ADR-024), withdraw the energy the haulers
//! and the links bring there, and upgrade the controller with it, in place of the opening's
//! generalist workers.
//!
//! A room gets upgraders once energy reaches its controller: a source whose haulers carry it to the
//! standing controller container (the hauling plan wants haulers for it, and the room has a
//! hauler), or whose miner sends it through a source link while a controller link stands
//! ([`MineSource::linked`]); either needs a miner in the room. A miner or hauler counts for
//! `upgrading.crew_grace_ticks` after the room last had one (spawning ones count), so the ticks
//! between a death and its replacement's order change nothing. With mining off, or a miner the
//! room cannot pay for, no energy comes and the workers keep upgrading; with hauling off only the
//! sent sources count, and the workers of the others keep upgrading with their energy. Their budget is
//! counted per creep life ([`yield_per_life`]): what those sources yield (a sent source's less the
//! links' loss, [`link_loss`]), less every other creep the room keeps: the mining plan's miners, the
//! hauling plan's haulers, the opening's planned workers at its capacity, and the mineral miner
//! and haulers while the room mines (P9.1, [`Upgrading::set_minerals`]). The rest pays for the
//! upgraders' bodies and what they upgrade ([`upgrader_plan`]): one body, sized and counted so they
//! spend it all, at most one per post and, at level 8, at most `CONTROLLER_MAX_UPGRADE_PER_TICK`
//! WORK parts in all (V188). Workers that build take energy from the same stores before the
//! upgraders do (below), so building goes first and the upgraders spend what is left.
//!
//! Posts: the walkable tiles within upgrading range of the controller (`WORK_RANGE`, V187) next to
//! one of its fed stores (the container while a counted source is hauled, the controller links
//! while one is sent; the container's own tile too), off the mining plan's mining tiles and link
//! tiles, but `upgrading.free_tiles` of those next to the first fed store (the container while it
//! is fed), the ones nearest the spawn, kept free for the haulers and the workers; best first: next
//! to the most stores, then the farthest from the spawn. Each upgrader holds one post (range 0: it
//! is never shoved off it).
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Upgrading::begin`] after the snapshot: the owned rooms and each room's upgraders (own creeps
//!    whose name says `upgrader` and that room, by id).
//! 2. [`Upgrading::select`] after the `hauling` job: the rooms to run, those with a controller
//!    store (a container on the mining plan's controller container tile, or a site there, or a
//!    controller link) or an upgrader.
//! 3. The Critical `upgrading` job runs [`Upgrading::plan_room`] per selected room, before
//!    `opening` and the spawn job: the room's plan (built again when its key changes: movement's
//!    grid version, the room's capacity and level, the controller, the stores, the budget's two
//!    sums or `upgrading.free_tiles`), each upgrader's post, one spawn request while the room is
//!    short of upgraders, and the view the opening reads ([`UpgradeView`]): the controller's energy
//!    is the upgraders' from the tick the room has its first one (spawning counts) until the plan
//!    wants none.
//! 4. The Standard `upgrading work` job runs [`Upgrading::work_room`] per room with an upgrader out
//!    of the spawn ([`Upgrading::work_count`]): the withdraws, upgrades and moves (upgrading, which
//!    CLAUDE.md puts in Standard). It runs after `opening work`, so the energy the opening's
//!    workers took this tick (`Opening::taken`) is not offered again.
//!
//! An upgrader upgrades every tick it holds energy within range of the controller, and withdraws on
//! the tick its store would run dry: when it holds no more than one tick's upgrade (its WORK parts
//! as the plan's body had them when it was first seen, the tick after its order: bodies are not in
//! the snapshot; after a global reset, the plan's body then). Its withdraw runs before its upgrade in the same
//! tick (V45, V46), so the upgrade spends the new energy. It takes from the first store next to it
//! whose energy left this tick covers its free space, controller links before the container
//! (energy left in a link holds back the next send), else from the one with the most left. A
//! withdraw asks for exactly the energy claimed, its free space or what the store has left after
//! this tick's other claims.
//!
//! Upgraders keep their posts between ticks in the heap; after a global reset each takes the post it
//! stands on, else the first free one in id order. Nothing is read from or written to `Memory`.

use rustedscreep_consts::{
    CONTROLLER_MAX_UPGRADE_PER_TICK, MOVE_COST_PLAIN, Part, Position, ResourceType, RoomCoordinate,
    RoomName, StructureType, UPGRADE_CONTROLLER_POWER,
};

use super::hauling::HaulPlan;
use super::mining::{MinePlan, MineSource, Receiver, link_loss};
use super::opening::Plan as WorkerPlan;
use super::sizing::{Trip, upgrader_plan, walk_ticks_max, yield_per_life};
use crate::config::{MovementConfig, SpawnConfig, UpgradingConfig};
use crate::intents::{CreepIntent, IntentBuffer, Target};
use crate::movement::{Dial, FlowField, Goal, MoveError, MoveTo, Movement, Tile, UNREACHED};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{
    Controller, CreepHandle, ObjectHandle, ObjectKind, Snapshot, StructureHandle,
};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner, is_due, lead};

/// The spawn priority of an upgrader request: below a hauler's (`PRIORITY_HAULER`, which brings
/// its energy) and a room short of workers (`PRIORITY_LOW`), above the opening's normal worker
/// requests (`PRIORITY_NORMAL`).
pub const PRIORITY_UPGRADER: u8 = 110;

/// Withdrawing reaches 1 tile (`isNearTo`, V108, V192, V220); upgrading 3 (`inRangeTo(target, 3)`,
/// V187, V188).
const NEAR: u8 = 1;
const WORK_RANGE: u8 = 3;

/// How hard an upgrader's moves push past other creeps (`MoveTo::priority`): as the opening's
/// workers that take energy (2), below a hauler that fills or stores (3).
const MOVE_PRIORITY: u8 = 2;

/// How near the best post an upgrader with no post waits (P5.2). It moves below the post holders'
/// priority, so a holder walking to its post shoves it off that tile.
const WAIT_RANGE: u8 = 2;

/// Posts movement found no way to that a room remembers at once.
const UNREACHABLE_MAX: usize = 16;

/// What an upgrader did this tick, for the console.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpgradeTask {
    /// On its way to its post (it upgrades on the way once in range with energy).
    Walk,
    /// At its post, upgrading.
    Upgrade,
    /// At its post, withdrawing (and upgrading while it holds energy).
    Refill,
    /// At its post with nothing to upgrade with, or the controller's upgrading is blocked.
    Wait,
    /// No post: it makes way.
    #[default]
    Idle,
}

impl UpgradeTask {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Walk => "walk",
            Self::Upgrade => "upgrade",
            Self::Refill => "refill",
            Self::Wait => "wait",
            Self::Idle => "idle",
        }
    }

    /// Every kind, in a fixed order (the order of [`UpgradeStats::tasks`]).
    pub const KINDS: [&'static str; 5] = ["walk", "upgrade", "refill", "wait", "idle"];

    const fn kind_index(self) -> usize {
        match self {
            Self::Walk => 0,
            Self::Upgrade => 1,
            Self::Refill => 2,
            Self::Wait => 3,
            Self::Idle => 4,
        }
    }
}

/// One of the controller's stores this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Store {
    Link(StructureHandle),
    Container(ObjectHandle),
}

impl Store {
    const fn target(self) -> Target {
        match self {
            Self::Link(h) => Target::Structure(h),
            Self::Container(h) => Target::Object(h),
        }
    }
}

/// A room's upgrading plan: built again when its key changes (module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradePlan {
    pub version: u32,
    pub capacity: u32,
    pub level: u8,
    pub controller: Position,
    /// The fed stores the posts were picked around: the controller container's tile while a
    /// counted source is hauled, then the controller links while one is sent, by place.
    pub stores: Vec<Position>,
    pub free_tiles: u32,
    /// What reaches the controller's stores in a creep life, and the other creeps' bodies over
    /// one. Unit: energy.
    pub delivered: u64,
    pub upkeep: u64,
    /// The sources counted in `delivered`, by place.
    pub sources: Vec<Position>,
    /// The walk from the room's first spawn to next to the first store; `None` when there is no
    /// spawn or no way.
    pub trip: Option<Trip>,
    /// The posts upgraders take, best first (`free_tiles` already left out).
    pub posts: Vec<Position>,
    /// The upgraders' body at the room's capacity ([`upgrader_plan`]), `None` when none fits or
    /// the budget is empty.
    pub body: Option<Body>,
    /// Upgraders the room keeps. Unit: creeps.
    pub want: u32,
}

impl UpgradePlan {
    /// The budget for the upgraders: what reaches the controller less the other creeps' bodies, in
    /// a creep life. Unit: energy.
    pub fn budget(&self) -> u64 {
        self.delivered.saturating_sub(self.upkeep)
    }
}

/// One room's counters for the console, from its last runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpgradeStats {
    pub time: u32,
    /// Upgraders of the room, spawning ones included, and of them the ones out of the spawn.
    pub upgraders: u32,
    pub working: u32,
    pub want: u32,
    /// Upgrader requests made this tick.
    pub requested: u32,
    /// Upgraders due for replacement (P5.2): they work on, but the plan counts them no more.
    pub due: u32,
    /// Upgraders per task in the last `upgrading work` run, in [`UpgradeTask::KINDS`] order.
    pub tasks: [u32; UpgradeTask::KINDS.len()],
    /// Upgrade calls pushed in that run.
    pub upgrades: u32,
    /// The view gave the controller's energy to the upgraders ([`UpgradeView::takes`]).
    pub takes: bool,
}

/// What the opening reads of a room's upgrading this tick ([`Upgrading::view`]). The default
/// (nothing taken) is what a room with no upgrading run this tick gets, and one whose plan wants
/// upgraders before its first one is ordered: until then the workers go on upgrading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpgradeView<'a> {
    /// The room's plan wants upgraders and can order them, and the room has had one since (its
    /// `engaged` state): the energy hauled or sent to the controller is theirs, so the opening sizes
    /// no worker for those sources and forces none to upgrade while others build
    /// (`opening.min_upgraders`).
    pub takes: bool,
    /// The sources the plan counts in its budget ([`delivered`]), by place: while `takes`, none
    /// gets a worker of its own, also while its miner is briefly gone (a worker spawned for that
    /// gap would live on for a creep life).
    pub sources: &'a [Position],
    /// The most WORK parts a worker gets while `takes` (`upgrading.worker_repeats`): the workers
    /// only build, repair and fill then. The opening applies it only while `takes`, and solves the
    /// body it counts in the upgraders' budget with it always (P3.6). `u8::MAX` for a room the job
    /// does not run.
    pub worker_repeats: u8,
}

impl Default for UpgradeView<'_> {
    fn default() -> Self {
        Self {
            takes: false,
            sources: &[],
            worker_repeats: u8::MAX,
        }
    }
}

struct UpgradeRoom {
    name: RoomName,
    row: Option<usize>,
    /// Selected to run this tick ([`Upgrading::select`]).
    selected: bool,
    plan: Option<UpgradePlan>,
    /// Members of [`Upgrading::members`] that belong to it this tick.
    members: (usize, usize),
    /// This tick's controller (handle, place, upgrading blocked) and stores (store, place, energy
    /// left to claim), from its Critical run.
    controller: Option<(StructureHandle, Position, bool)>,
    stores: Vec<(Store, Position, u32)>,
    /// This tick's [`UpgradeView::takes`], and `upgrading.worker_repeats` from its last run.
    takes: bool,
    worker_repeats: u8,
    /// The room's upgraders took over the controller's energy: once it had an upgrader (spawning
    /// ones count) while its plan wanted them, and until the plan wants none or cannot order them.
    /// Kept between ticks, so an upgrader's death before its replacement spawns does not hand the
    /// energy back to the workers (who would be sized and spawned again for a creep life); a global
    /// reset forgets it until an upgrader is seen again.
    engaged: bool,
    /// The last tick the room had a miner, and a hauler (spawning ones count; `None` before the
    /// first): either counts for `upgrading.crew_grace_ticks` after (module docs).
    miner_at: Option<u32>,
    hauler_at: Option<u32>,
    /// Posts movement found no way to, each skipped until the tick given
    /// (`movement.route_retry_ticks` later). At most [`UNREACHABLE_MAX`], oldest dropped first.
    unreachable: Vec<(Position, u32)>,
    /// The energy its remote rooms bring it in a creep life ([`Upgrading::set_remote`], P6.2).
    remote: u64,
    /// The mineral creeps' bodies over a creep life while it mines ([`Upgrading::set_minerals`],
    /// P9.1).
    minerals: u64,
    stats: UpgradeStats,
}

/// One upgrader of this tick, in the order the jobs walk them: by room, then by id.
#[derive(Clone, Copy, Debug)]
struct Member {
    room: usize,
    key: u128,
    row: usize,
    handle: CreepHandle,
}

/// An upgrader's state between ticks, in the slot of its handle.
#[derive(Clone, Copy, Debug)]
struct Upgrader {
    handle: CreepHandle,
    post: Option<Position>,
    task: UpgradeTask,
    /// Its WORK parts, as the plan's body had them when it was first seen (spawning, the tick
    /// after its order, so almost always the body it was ordered with): bodies are not in the
    /// snapshot. 0 when the plan had no body then. Unit: parts.
    work: u32,
}

/// Upgrading's state for one global.
#[derive(Default)]
pub struct Upgrading {
    time: u32,
    rooms: Vec<UpgradeRoom>,
    members: Vec<Member>,
    /// Indexed by creep handle slot.
    upgraders: Vec<Option<Upgrader>>,
    field: FlowField,
    dial: Dial,
    /// The selected rooms (by index): the `upgrading` job's units.
    units: Vec<usize>,
    /// The rooms (by index) with an upgrader out of the spawn this tick: the `upgrading work`
    /// job's units.
    work_rooms: Vec<usize>,
    /// Scratch, kept for its capacity: the room's sources (place, capacity), its posts open this
    /// tick and the posts taken.
    sources: Vec<(Position, u32)>,
    open_posts: Vec<Position>,
    taken_posts: Vec<Position>,
}

impl Upgrading {
    /// Starts a tick: the owned rooms, by name, and their upgraders, by room and id.
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
                    UpgradeRoom {
                        name,
                        row: Some(row),
                        selected: false,
                        plan: None,
                        members: (0, 0),
                        controller: None,
                        stores: Vec::new(),
                        takes: false,
                        worker_repeats: u8::MAX,
                        engaged: false,
                        miner_at: None,
                        hauler_at: None,
                        unreachable: Vec::new(),
                        remote: 0,
                        minerals: 0,
                        stats: UpgradeStats::default(),
                    },
                ),
            }
        }
        self.rooms.retain(|r| r.row.is_some());
        self.units.clear();
        self.work_rooms.clear();
        for r in &mut self.rooms {
            // The view stays last tick's until the room's run this tick (or `select` drops it), so
            // a run that faults flips nothing.
            r.selected = false;
            r.controller = None;
            r.stores.clear();
        }
        self.members.clear();
        let creeps = &snap.creeps;
        for (row, identity) in creeps.identities().iter().enumerate() {
            let Some(id) = identity.filter(|id| id.role == Role::Upgrader) else {
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

    /// Picks this tick's rooms for the `upgrading` job: those with an upgrader, or for which
    /// `has_store` says the controller has a store (a container or a site on the mining plan's
    /// controller container tile, or a controller link). The others keep no plan; no unit, so no
    /// clock reading, runs for them.
    pub fn select(&mut self, has_store: impl Fn(RoomName) -> bool) {
        self.units.clear();
        for (i, r) in self.rooms.iter_mut().enumerate() {
            r.selected = r.members.0 != r.members.1 || has_store(r.name);
            if r.selected {
                self.units.push(i);
            } else {
                r.plan = None;
                r.takes = false;
                r.worker_repeats = u8::MAX;
                r.engaged = false;
                r.stats = UpgradeStats {
                    time: self.time,
                    ..UpgradeStats::default()
                };
            }
        }
    }

    /// The `upgrading` job's units this tick ([`Upgrading::select`]).
    pub fn unit_count(&self) -> u32 {
        u32::try_from(self.units.len()).unwrap_or(u32::MAX)
    }

    /// The name of the room of unit `unit` ([`Upgrading::unit_count`]).
    pub fn unit_room(&self, unit: u32) -> Option<RoomName> {
        let i = *self.units.get(usize::try_from(unit).ok()?)?;
        self.rooms.get(i).map(|r| r.name)
    }

    /// The `upgrading work` job's units this tick: the rooms with an upgrader out of the spawn.
    pub fn work_count(&self) -> u32 {
        u32::try_from(self.work_rooms.len()).unwrap_or(u32::MAX)
    }

    /// The name of the room of work unit `unit` ([`Upgrading::work_count`]).
    pub fn work_room_name(&self, unit: u32) -> Option<RoomName> {
        let i = *self.work_rooms.get(usize::try_from(unit).ok()?)?;
        self.rooms.get(i).map(|r| r.name)
    }

    fn room(&self, room: RoomName) -> Option<&UpgradeRoom> {
        self.rooms
            .binary_search_by_key(&room.packed_repr(), |r| r.name.packed_repr())
            .ok()
            .and_then(|i| self.rooms.get(i))
    }

    /// What the opening reads of `room`'s upgrading this tick: from the room's run this tick, else
    /// (before it, or when it faulted) the last one's; nothing for a room the job does not run.
    pub fn view(&self, room: RoomName) -> UpgradeView<'_> {
        self.room(room)
            .map_or_else(UpgradeView::default, |r| UpgradeView {
                takes: r.takes,
                sources: r
                    .plan
                    .as_ref()
                    .filter(|_| r.takes)
                    .map_or(&[], |p| &p.sources[..]),
                worker_repeats: r.worker_repeats,
            })
    }

    /// Each room's plan and counters, by name.
    pub fn room_states(
        &self,
    ) -> impl Iterator<Item = (RoomName, Option<&UpgradePlan>, &UpgradeStats)> {
        self.rooms
            .iter()
            .map(|r| (r.name, r.plan.as_ref(), &r.stats))
    }

    /// The post of upgrader `handle`, if it is one of this tick's upgraders and holds one.
    pub fn post(&self, handle: CreepHandle) -> Option<Position> {
        self.upgrader(handle).and_then(|u| u.post)
    }

    /// The WORK parts upgrader `handle` is taken to have (its plan's body when first seen), if it
    /// is one of this tick's upgraders and that body was known.
    pub fn work_parts(&self, handle: CreepHandle) -> Option<u32> {
        self.upgrader(handle).map(|u| u.work).filter(|&w| w > 0)
    }

    /// The Critical half of unit `unit`'s tick (module docs, step 3). `mine` is the room's mining
    /// plan (`Mining::plan`), `receivers` its controller links this tick (`Mining::receivers`),
    /// `haul` its hauling plan (`Hauling::plan`), `crew` its miners and haulers this tick, spawning
    /// ones included (`Mining::stats`, `Hauling::stats`; latched for `upgrading.crew_grace_ticks`),
    /// and `workers` the opening's last plan
    /// (`Opening::plan`); `cap`, the most energy the opening's watchdog saw the room hold while it
    /// never filled (`Watchdog::cap`), holds back an upgrader that costs more.
    /// Sets the energy `room`'s remote rooms bring it in a creep life (`economy::Remotes::delivered`,
    /// P6.2), which the upgraders' budget counts while the controller container stands. It holds
    /// until set again; a room with none set counts 0.
    pub fn set_remote(&mut self, room: RoomName, energy: u64) {
        if let Ok(i) = self
            .rooms
            .binary_search_by_key(&room.packed_repr(), |r| r.name.packed_repr())
            && let Some(r) = self.rooms.get_mut(i)
        {
            r.remote = energy;
        }
    }

    /// Sets the energy `room`'s mineral creeps cost in a creep life while it mines
    /// (`economy::Minerals::upkeep`, P9.1, ADR-044): their bodies, which the upgraders' budget
    /// leaves out like every other creep's, so the upgraders are sized to what is left. It holds
    /// until set again; a room with none set counts 0.
    pub fn set_minerals(&mut self, room: RoomName, energy: u64) {
        if let Ok(i) = self
            .rooms
            .binary_search_by_key(&room.packed_repr(), |r| r.name.packed_repr())
            && let Some(r) = self.rooms.get_mut(i)
        {
            r.minerals = energy;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        cfg: &UpgradingConfig,
        mcfg: &MovementConfig,
        scfg: &SpawnConfig,
        mine: Option<&MinePlan>,
        receivers: &[Receiver],
        haul: Option<&HaulPlan>,
        crew: Crew,
        workers: Option<&WorkerPlan>,
        cap: Option<u32>,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let ri = usize::try_from(unit)
            .ok()
            .and_then(|u| self.units.get(u).copied())
            .ok_or(Fault::new("unit out of range", unit))?;
        let (name, row, members) = {
            let r = self
                .rooms
                .get(ri)
                .ok_or(Fault::new("no room for the unit", unit))?;
            (r.name, r.row, r.members)
        };
        let row = row.ok_or(Fault::new("room not in this tick", unit))?;
        let now = self.time;
        // A miner or hauler counts for the grace after the room last had one (module docs).
        let crew = match self.rooms.get_mut(ri) {
            Some(r) => {
                if crew.miners > 0 {
                    r.miner_at = Some(now);
                }
                if crew.haulers > 0 {
                    r.hauler_at = Some(now);
                }
                let recent = |at: Option<u32>| {
                    at.is_some_and(|t| now.wrapping_sub(t) <= cfg.crew_grace_ticks)
                };
                Crew {
                    miners: u32::from(recent(r.miner_at)),
                    haulers: u32::from(recent(r.hauler_at)),
                }
            }
            None => crew,
        };
        self.gather(ri, snap, name, mine, receivers);
        self.ensure_plan(
            ri, snap, row, movement, cfg, mcfg, mine, receivers, haul, crew, workers, commit,
        );
        self.assign_posts(ri, snap, members);

        let mut stats = UpgradeStats {
            time: now,
            upgraders: u32::try_from(members.1.saturating_sub(members.0)).unwrap_or(u32::MAX),
            ..UpgradeStats::default()
        };
        let mut working = false;
        let lead = self
            .rooms
            .get(ri)
            .and_then(|r| r.plan.as_ref())
            .map_or(0, |p| upgrader_lead(p, scfg));
        for i in members.0..members.1 {
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            if is_due(snap.creeps.ticks_to_live(m.row, now), lead) {
                stats.due = stats.due.saturating_add(1);
            }
            if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
                continue;
            }
            stats.working = stats.working.saturating_add(1);
            working = true;
            // The Standard job moves it later in the tick, if the tier runs at all; one on its
            // post stays there (no billed shove off it and walk back while the tier is off).
            let at = snap.creeps.positions().get(m.row).copied();
            let held = self
                .upgrader(m.handle)
                .and_then(|u| u.post)
                .filter(|&p| Some(p) == at);
            movement.give_way(snap, m.handle, held.map(|p| (p, 0)));
        }
        // By room index, so the work job's units keep their rooms whatever this job's order.
        if working && let Err(at) = self.work_rooms.binary_search(&ri) {
            self.work_rooms.insert(at, ri);
        }

        // The request, and the view: the plan's upgraders take the controller's energy once the
        // room has one (`engaged`).
        let mut wants = false;
        if let Some(plan) = self.rooms.get(ri).and_then(|r| r.plan.as_ref()) {
            stats.want = plan.want;
            if cfg.enabled
                && plan.want > 0
                && let Some(body) = plan.body
                && cap.is_none_or(|c| body.cost() <= c)
            {
                wants = true;
                // A due upgrader no longer fills its place (P5.2).
                if stats.upgraders.saturating_sub(stats.due) < plan.want {
                    let asked = spawner.request(SpawnRequest {
                        home: name,
                        role: Role::Upgrader,
                        remote: None,
                        body: BodySpec::Exact(body),
                        budget: EnergyBudget::Capacity,
                        priority: PRIORITY_UPGRADER,
                    });
                    if asked.is_some() {
                        stats.requested = 1;
                    }
                }
            }
        }
        if let Some(r) = self.rooms.get_mut(ri) {
            r.engaged = wants && (r.engaged || stats.upgraders > 0);
            let takes = r.engaged;
            stats.takes = takes;
            // The last work run's counters stay until the next one.
            stats.tasks = r.stats.tasks;
            stats.upgrades = r.stats.upgrades;
            r.takes = takes;
            r.worker_repeats = repeats(cfg);
            r.stats = stats;
        }
        Ok(())
    }

    /// The Standard half of the tick for unit `unit` of [`Upgrading::work_count`]: that room's
    /// withdraws, upgrades and moves. `taken` is what the opening's workers take this tick
    /// (`Opening::taken`) and `hauled` what the haulers take (`HaulView::taken`: since P5.3 a
    /// hauler may take from the controller container while the room's spawn queue waits for
    /// energy), per store: neither is offered again.
    #[allow(clippy::too_many_arguments)]
    pub fn work_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        mcfg: &MovementConfig,
        taken: &[(Target, u32)],
        hauled: &[(ObjectHandle, u32)],
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let ri = usize::try_from(unit)
            .ok()
            .and_then(|u| self.work_rooms.get(u).copied())
            .ok_or(Fault::new("unit out of range", unit))?;
        let now = self.time;
        let (members, controller, plan_work, first_post) = {
            let r = self
                .rooms
                .get_mut(ri)
                .ok_or(Fault::new("no room for the unit", unit))?;
            for s in &mut r.stores {
                let target = s.0.target();
                let took = taken
                    .iter()
                    .filter(|t| t.0 == target)
                    .fold(0u32, |a, t| a.saturating_add(t.1));
                let hauls = hauled
                    .iter()
                    .filter(|t| Target::Object(t.0) == target)
                    .fold(0u32, |a, t| a.saturating_add(t.1));
                s.2 = s.2.saturating_sub(took).saturating_sub(hauls);
            }
            r.unreachable.retain(|&(_, until)| now < until);
            // The best post movement has not found unreachable (as `assign_posts` filters them).
            let first_post = r.plan.as_ref().and_then(|p| {
                p.posts
                    .iter()
                    .copied()
                    .find(|q| !r.unreachable.iter().any(|u| u.0 == *q))
            });
            (
                r.members,
                r.controller,
                body_work(r.plan.as_ref()),
                first_post,
            )
        };
        let mut tasks = [0u32; UpgradeTask::KINDS.len()];
        let mut upgrades = 0u32;
        for i in members.0..members.1 {
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
                continue;
            }
            let Some(&at) = snap.creeps.positions().get(m.row) else {
                continue;
            };
            let store = snap.creeps.stores().get(m.row).copied().unwrap_or_default();
            let post = self.upgrader(m.handle).and_then(|u| u.post);
            let Some(post) = post else {
                // With every post held (a successor ordered before its predecessor is gone, P5.2),
                // it waits near the best reachable post, so it takes the first one to free within a
                // step or two; below the post holders' priority, it makes way for them. With no such
                // post, or no way there, it makes way anywhere.
                let waited = first_post.is_some_and(|near| {
                    let to = MoveTo {
                        target: near,
                        range: WAIT_RANGE,
                        priority: MOVE_PRIORITY.saturating_sub(1),
                        tier: Tier::Standard,
                    };
                    movement.request(snap, m.handle, to, mcfg, commit) != Err(MoveError::NoPath)
                });
                if !waited {
                    movement.give_way(snap, m.handle, None);
                }
                self.set_task(m.handle, UpgradeTask::Idle);
                if let Some(t) = tasks.get_mut(UpgradeTask::Idle.kind_index()) {
                    *t += 1;
                }
                continue;
            };
            let blocked = controller.is_none_or(|c| c.2);
            // Withdraw on the tick the store would run dry: one tick's upgrade by its own body as
            // first seen, else the plan's (bodies are not in the snapshot).
            let work = self
                .work_parts(m.handle)
                .or(plan_work)
                .map(|w| w.saturating_mul(UPGRADE_CONTROLLER_POWER));
            let threshold = work
                .unwrap_or(store.capacity / 2)
                .min(store.capacity.saturating_sub(1));
            let mut refilled = false;
            let free = store.free();
            if !blocked
                && free > 0
                && store.energy <= threshold
                && let Some(r) = self.rooms.get_mut(ri)
                && let Some(k) = pick_store(&r.stores, at, free)
                && let Some(s) = r.stores.get_mut(k)
            {
                // Exactly what it claims: with no amount it would take all it finds, and when the
                // engine runs it before a worker that took here this tick (`taken`), the worker's
                // withdraw would find the store empty, billed for nothing. The game side refuses an
                // amount above the free space or the stock at the tick's start (V192, V220).
                let amount = Some(free.min(s.2));
                let intent = match s.0 {
                    Store::Link(h) => CreepIntent::Withdraw {
                        target: h,
                        resource: ResourceType::Energy,
                        amount,
                    },
                    Store::Container(h) => CreepIntent::WithdrawObject {
                        target: h,
                        resource: ResourceType::Energy,
                        amount,
                    },
                };
                if buffer.push(snap, m.handle, intent, commit).is_ok() {
                    s.2 = s.2.saturating_sub(free);
                    refilled = true;
                }
            }
            // Upgrade with the energy held at the tick's start (the game side refuses an empty
            // store, V187).
            let mut upgraded = false;
            if !blocked
                && store.energy > 0
                && let Some((c, cpos, _)) = controller
                && range(at, cpos) <= WORK_RANGE
                && buffer
                    .push(
                        snap,
                        m.handle,
                        CreepIntent::UpgradeController { target: c },
                        commit,
                    )
                    .is_ok()
            {
                upgraded = true;
                upgrades = upgrades.saturating_add(1);
            }
            let task = if at != post {
                UpgradeTask::Walk
            } else if refilled {
                UpgradeTask::Refill
            } else if upgraded {
                UpgradeTask::Upgrade
            } else {
                UpgradeTask::Wait
            };
            if let Some(t) = tasks.get_mut(task.kind_index()) {
                *t += 1;
            }
            self.set_task(m.handle, task);
            let to = MoveTo {
                target: post,
                range: 0,
                priority: MOVE_PRIORITY,
                tier: Tier::Standard,
            };
            if movement.request(snap, m.handle, to, mcfg, commit) == Err(MoveError::NoPath) {
                let until = now.saturating_add(mcfg.route_retry_ticks);
                if let Some(r) = self.rooms.get_mut(ri) {
                    if r.unreachable.len() >= UNREACHABLE_MAX {
                        r.unreachable.remove(0);
                    }
                    r.unreachable.push((post, until));
                }
                if let Some(u) = self.upgrader_mut(m.handle) {
                    u.post = None;
                }
            }
        }
        if let Some(r) = self.rooms.get_mut(ri) {
            r.stats.tasks = tasks;
            r.stats.upgrades = upgrades;
        }
        Ok(())
    }

    fn upgrader(&self, handle: CreepHandle) -> Option<&Upgrader> {
        let slot = usize::try_from(handle.slot()).ok()?;
        self.upgraders
            .get(slot)?
            .as_ref()
            .filter(|u| u.handle == handle)
    }

    fn upgrader_mut(&mut self, handle: CreepHandle) -> Option<&mut Upgrader> {
        let slot = usize::try_from(handle.slot()).ok()?;
        self.upgraders
            .get_mut(slot)?
            .as_mut()
            .filter(|u| u.handle == handle)
    }

    /// The state of upgrader `handle`, created when its slot holds another creep or none, with
    /// `work` WORK parts (the plan's body now).
    fn upgrader_or_new(&mut self, handle: CreepHandle, work: u32) -> Option<&mut Upgrader> {
        let slot = usize::try_from(handle.slot()).ok()?;
        if self.upgraders.len() <= slot {
            self.upgraders.resize(slot + 1, None);
        }
        let cell = self.upgraders.get_mut(slot)?;
        if cell.is_none_or(|u| u.handle != handle) {
            *cell = Some(Upgrader {
                handle,
                post: None,
                task: UpgradeTask::Idle,
                work,
            });
        }
        cell.as_mut()
    }

    fn set_task(&mut self, handle: CreepHandle, task: UpgradeTask) {
        if let Some(u) = self.upgrader_mut(handle) {
            u.task = task;
        }
    }

    /// Reads room `ri`'s controller, its stores (the controller links in `receivers`, by place, then
    /// the container on the mining plan's controller container tile) and its sources.
    fn gather(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        name: RoomName,
        mine: Option<&MinePlan>,
        receivers: &[Receiver],
    ) {
        self.sources.clear();
        let Some(r) = self.rooms.get_mut(ri) else {
            return;
        };
        r.stores.clear();
        r.controller = None;
        let blocked = r
            .row
            .and_then(|row| snap.rooms.controller_states().get(row))
            .is_some_and(|s| s.upgrade_blocked);
        let st = &snap.structures;
        for ((&h, &kind), &pos) in st.handles().iter().zip(st.kinds()).zip(st.positions()) {
            if kind == StructureType::Controller && pos.room_name() == name {
                r.controller = Some((h, pos, blocked));
                break;
            }
        }
        // The links first, by place (module docs), then the container.
        r.stores.extend(
            receivers
                .iter()
                .map(|l| (Store::Link(l.link), l.pos, l.energy)),
        );
        r.stores.sort_unstable_by_key(|l| tile_key(l.1));
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
                    r.stores.push((Store::Container(h), pos, amount));
                }
                _ => {}
            }
        }
    }

    /// Builds room `ri`'s plan when its key changed (module docs), or drops it while the room has
    /// no controller; with no fed store the plan has no post and wants no upgrader. Without a grid
    /// yet the room keeps its last plan.
    #[allow(clippy::too_many_arguments)]
    fn ensure_plan(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        row: usize,
        movement: &mut Movement,
        cfg: &UpgradingConfig,
        mcfg: &MovementConfig,
        mine: Option<&MinePlan>,
        receivers: &[Receiver],
        haul: Option<&HaulPlan>,
        crew: Crew,
        workers: Option<&WorkerPlan>,
        commit: &mut Commit,
    ) {
        let Some(r) = self.rooms.get(ri) else {
            return;
        };
        let name = r.name;
        let Some((_, controller, _)) = r.controller else {
            if let Some(r) = self.rooms.get_mut(ri) {
                r.plan = None;
            }
            return;
        };
        let capacity = snap.rooms.energy().get(row).map_or(0, |e| e.capacity);
        let level = match snap.rooms.controllers().get(row) {
            Some(&Controller::Mine { level }) => level,
            _ => 0,
        };
        let ways = Ways {
            container: r.stores.iter().any(|s| matches!(s.0, Store::Container(_))),
            link: !receivers.is_empty(),
            crew,
        };
        let sources = mine.map_or(&[][..], |m| &m.sources[..]);
        let fed = |sent: bool| {
            sources
                .iter()
                .any(|s| delivery(s, haul, ways) == Some(sent))
        };
        let (fed_container, fed_links) = (fed(false), fed(true));
        // The fed stores, in the plan's order: the container, then the links by place.
        let stores = || {
            r.stores
                .iter()
                .filter(move |s| matches!(s.0, Store::Container(_)) && fed_container)
                .chain(
                    r.stores
                        .iter()
                        .filter(move |s| matches!(s.0, Store::Link(_)) && fed_links),
                )
                .map(|s| s.1)
        };
        let counted = || {
            sources
                .iter()
                .filter(|s| delivery(s, haul, ways).is_some())
                .map(|s| s.source)
        };
        // P6.2: the remote rooms' energy reaches the controller container through the room's
        // haulers (ADR-029's "home sinks"), so it counts while that container stands.
        let remote = if ways.container { r.remote } else { 0 };
        let delivered = delivered(mine, haul, &self.sources, ways).saturating_add(remote);
        let upkeep = upkeep(mine, haul, workers).saturating_add(r.minerals);
        let Some((grid, version)) = movement.room_grid(name, mcfg, commit) else {
            return;
        };
        if r.plan.as_ref().is_some_and(|p| {
            p.version == version
                && p.capacity == capacity
                && p.level == level
                && p.controller == controller
                && p.free_tiles == cfg.free_tiles
                && p.delivered == delivered
                && p.upkeep == upkeep
                && p.stores.iter().copied().eq(stores())
                && p.sources.iter().copied().eq(counted())
        }) {
            return;
        }
        let stores: Vec<Position> = stores().collect();
        let counted: Vec<Position> = counted().collect();
        // Tiles that are someone else's: the mining tiles, and the link tiles (a planned one is not
        // built on yet, so the grid still shows it walkable).
        let mut reserved: Vec<Position> = Vec::new();
        if let Some(m) = mine {
            reserved.extend(m.sources.iter().filter_map(|s| s.tile));
            reserved.extend(m.sources.iter().filter_map(|s| s.link));
            reserved.extend(m.controller_link);
        }
        let spawn = mine.and_then(|m| m.spawn);
        let ct = Tile::of(controller);
        let mut posts: Vec<(u8, u8, (u8, u8), Position)> = Vec::new();
        for dy in -i16::from(WORK_RANGE)..=i16::from(WORK_RANGE) {
            for dx in -i16::from(WORK_RANGE)..=i16::from(WORK_RANGE) {
                let x = i16::from(ct.x()) + dx;
                let y = i16::from(ct.y()) + dy;
                // Off the edge tiles (exits).
                if !(1..=48).contains(&x) || !(1..=48).contains(&y) {
                    continue;
                }
                let (Ok(x), Ok(y)) = (u8::try_from(x), u8::try_from(y)) else {
                    continue;
                };
                let Some(t) = Tile::new(x, y) else {
                    continue;
                };
                if !grid.walkable(t) {
                    continue;
                }
                let Some(p) = place(name, t) else {
                    continue;
                };
                if reserved.contains(&p) && !stores.contains(&p) {
                    continue;
                }
                let near = stores.iter().filter(|&&s| range(p, s) <= NEAR).count();
                if near == 0 {
                    continue;
                }
                let from_spawn = spawn.map_or(0, |s| range(p, s));
                posts.push((
                    u8::try_from(near).unwrap_or(u8::MAX),
                    from_spawn,
                    tile_key(p),
                    p,
                ));
            }
        }
        // The free tiles: next to the first fed store (the container while it is fed), nearest
        // the spawn, where the haulers and the workers come from.
        if let Some(&access) = stores.first() {
            posts.sort_by(|a, b| {
                let near = |p: Position| range(p, access) <= NEAR;
                near(b.3)
                    .cmp(&near(a.3))
                    .then(a.1.cmp(&b.1))
                    .then(a.2.cmp(&b.2))
            });
            let free = usize::try_from(cfg.free_tiles)
                .unwrap_or(usize::MAX)
                .min(posts.iter().filter(|p| range(p.3, access) <= NEAR).count());
            posts.drain(..free);
        }
        // Next to the most stores, then the farthest from the spawn, then by place.
        posts.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        let posts: Vec<Position> = posts.iter().map(|p| p.3).collect();

        // The walk out: from next to the spawn to next to the first store (no field without a
        // spawn or a post).
        let anchor = stores.first().copied().filter(|_| !posts.is_empty());
        let trip = spawn.zip(anchor).and_then(|(s, anchor)| {
            commit.heavy();
            let goal = Goal {
                tile: Tile::of(anchor),
                range: NEAR,
            };
            if self.field.compute(grid, goal, &mut self.dial) == 0 {
                return None;
            }
            let at = Tile::of(s);
            let cost = at
                .neighbors()
                .filter(|&(_, t)| grid.walkable(t))
                .map(|(_, t)| self.field.distance(t))
                .filter(|&d| d != UNREACHED)
                .min()?;
            Some(Trip {
                tiles: u32::from(at.range(goal.tile)).saturating_sub(2 * u32::from(NEAR)),
                cost: u32::from(cost),
            })
        });
        let budget = delivered.saturating_sub(upkeep);
        let max_work =
            (level >= 8).then(|| CONTROLLER_MAX_UPGRADE_PER_TICK / UPGRADE_CONTROLLER_POWER.max(1));
        let planned = trip.and_then(|t| {
            upgrader_plan(
                budget,
                t,
                capacity,
                u32::try_from(posts.len()).unwrap_or(u32::MAX),
                max_work,
            )
        });
        if let Some(r) = self.rooms.get_mut(ri) {
            r.plan = Some(UpgradePlan {
                version,
                capacity,
                level,
                controller,
                stores,
                free_tiles: cfg.free_tiles,
                delivered,
                upkeep,
                sources: counted,
                trip,
                posts,
                body: planned.map(|p| p.0),
                want: planned.map_or(0, |p| p.1),
            });
        }
    }

    /// Each upgrader's post for this tick: one kept from the last tick while it is in the plan and
    /// not unreachable, else the free one it stands on (after a global reset), else the first free
    /// one in the plan's order.
    fn assign_posts(&mut self, ri: usize, snap: &Snapshot, members: (usize, usize)) {
        let now = self.time;
        let mut taken = std::mem::take(&mut self.taken_posts);
        taken.clear();
        let mut posts = std::mem::take(&mut self.open_posts);
        posts.clear();
        let work = self
            .rooms
            .get(ri)
            .and_then(|r| body_work(r.plan.as_ref()))
            .unwrap_or(0);
        if let Some(r) = self.rooms.get_mut(ri) {
            r.unreachable.retain(|&(_, until)| now < until);
            if let Some(p) = &r.plan {
                posts.extend(
                    p.posts
                        .iter()
                        .copied()
                        .filter(|p| !r.unreachable.iter().any(|u| u.0 == *p)),
                );
            }
        }
        // Kept posts first, so a new upgrader never takes one an older one holds.
        for i in members.0..members.1 {
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            let kept = self
                .upgrader_or_new(m.handle, work)
                .and_then(|u| u.post)
                .filter(|p| posts.contains(p) && !taken.contains(p));
            match kept {
                Some(p) => taken.push(p),
                None => {
                    if let Some(u) = self.upgrader_mut(m.handle) {
                        u.post = None;
                    }
                }
            }
        }
        for i in members.0..members.1 {
            let Some(m) = self.members.get(i).copied() else {
                continue;
            };
            if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
                continue;
            }
            if self.upgrader(m.handle).is_some_and(|u| u.post.is_some()) {
                continue;
            }
            // The post it stands on (after a global reset), else the first free one.
            let at = snap.creeps.positions().get(m.row).copied();
            let pick = at
                .filter(|p| posts.contains(p) && !taken.contains(p))
                .or_else(|| posts.iter().copied().find(|p| !taken.contains(p)));
            if let Some(p) = pick {
                taken.push(p);
            }
            if let Some(u) = self.upgrader_mut(m.handle) {
                u.post = pick;
            }
        }
        self.taken_posts = taken;
        self.open_posts = posts;
    }
}

/// How many tiles past the plan's trip an upgrader's post may lie (P5.2): the trip ends on the
/// first store's neighbor nearest the spawn, and every post is another neighbor of a store, at
/// most this far round the store's ring from it. Unit: tiles.
pub(super) const POST_STEP: u32 = 2;

/// The lead of a replacement for one of `plan`'s upgraders (P5.2, [`lead`]): its body's spawn
/// time, its walk out at most on the plan's trip and then [`POST_STEP`] plain tiles round the
/// store to its post (empty, so only the WORK parts weigh, V54, as
/// [`upgrader_upgrade`](super::sizing::upgrader_upgrade) counts it; [`walk_ticks_max`]), and the
/// margin; 0 without a body (no upgrader is requested), and no walk without a trip. The post step
/// is an estimate: a swamp tile on the ring walks longer, a road shorter.
///
/// Uses: `MOVE_COST_PLAIN`, [`POST_STEP`], [`lead`], [`walk_ticks_max`].
pub(super) fn upgrader_lead(plan: &UpgradePlan, scfg: &SpawnConfig) -> u32 {
    let Some(body) = plan.body else {
        return 0;
    };
    let parts = |p: Part| u32::try_from(body.count(p)).unwrap_or(u32::MAX);
    let walk = plan
        .trip
        .map(|t| Trip {
            tiles: t.tiles.saturating_add(POST_STEP),
            cost: t
                .cost
                .saturating_add(POST_STEP.saturating_mul(MOVE_COST_PLAIN)),
        })
        .and_then(|t| walk_ticks_max(t, parts(Part::Work), parts(Part::Move)))
        .unwrap_or(0);
    lead(scfg, &body, walk)
}

/// The store an upgrader at `at` with `free` room withdraws from (module docs): of those next to
/// it with energy left this tick, the first whose energy covers `free` (the links come first in
/// `stores`), else the one with the most. Its index in `stores`.
pub(super) fn pick_store(
    stores: &[(Store, Position, u32)],
    at: Position,
    free: u32,
) -> Option<usize> {
    let near = |s: &&(Store, Position, u32)| s.2 > 0 && range(at, s.1) <= NEAR;
    stores
        .iter()
        .position(|s| near(&s) && s.2 >= free)
        .or_else(|| {
            stores
                .iter()
                .enumerate()
                .filter(|(_, s)| near(s))
                .max_by_key(|(k, s)| (s.2, std::cmp::Reverse(*k)))
                .map(|(k, _)| k)
        })
}

/// A room's miners and haulers this tick, spawning ones included. Unit: creeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Crew {
    pub miners: u32,
    pub haulers: u32,
}

/// The ways a source's energy can reach the controller this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ways {
    /// The controller container stands.
    pub container: bool,
    /// A controller link stands.
    pub link: bool,
    pub crew: Crew,
}

/// What reaches room's controller stores in a creep life (module docs): the yield of each source
/// whose haulers the hauling plan wants, and of each source the mining plan sends through a link,
/// less the links' loss, while `ways` lets it through (see [`delivering`]). `sources` gives each
/// source's capacity. Unit: energy.
///
/// Uses: [`yield_per_life`], [`link_loss`].
pub fn delivered(
    mine: Option<&MinePlan>,
    haul: Option<&HaulPlan>,
    sources: &[(Position, u32)],
    ways: Ways,
) -> u64 {
    let Some(mine) = mine else {
        return 0;
    };
    let capacity = |p: Position| sources.iter().find(|s| s.0 == p).map_or(0, |s| s.1);
    mine.sources
        .iter()
        .map(|s| {
            let y = yield_per_life(capacity(s.source));
            match delivery(s, haul, ways) {
                Some(true) => {
                    let lost = u32::try_from(y).map_or(u64::MAX, |y| u64::from(link_loss(y)));
                    y.saturating_sub(lost)
                }
                Some(false) => y,
                None => 0,
            }
        })
        .fold(0u64, u64::saturating_add)
}

/// The sources [`delivered`] counts, in the mining plan's order: a sent one while a controller link
/// stands, a hauled one while the container stands and the room has a hauler, either only while
/// the room has a miner.
pub fn delivering(mine: Option<&MinePlan>, haul: Option<&HaulPlan>, ways: Ways) -> Vec<Position> {
    mine.map_or_else(Vec::new, |m| {
        m.sources
            .iter()
            .filter(|s| delivery(s, haul, ways).is_some())
            .map(|s| s.source)
            .collect()
    })
}

/// How a source of the mining plan reaches the controller: `Some(true)` through the links (its
/// miner is paid for and sends, and a controller link stands), `Some(false)` with haulers (its
/// miner is paid for, the hauling plan wants haulers for it, the container stands and the room has
/// a hauler), else `None`; never while the room has no miner.
fn delivery(s: &MineSource, haul: Option<&HaulPlan>, ways: Ways) -> Option<bool> {
    if s.body.is_none() || ways.crew.miners == 0 {
        return None;
    }
    let hauled = ways.container
        && ways.crew.haulers > 0
        && haul.is_some_and(|h| {
            h.sources
                .iter()
                .any(|x| x.source == s.source && x.want > 0 && x.body.is_some())
        });
    match (s.linked, ways.link, hauled) {
        (true, true, _) => Some(true),
        (false, _, true) => Some(false),
        _ => None,
    }
}

/// The bodies, over one creep life, of every creep the room keeps but its upgraders: the mining
/// plan's miners, the hauling plan's haulers and the opening's planned workers, each of the body
/// the opening solved for them as they are while upgraders take the controller's energy (its
/// plan's `capped` body, P3.6). Unit: energy.
///
/// Uses: `BODYPART_COST` (`Body::cost`).
pub fn upkeep(
    mine: Option<&MinePlan>,
    haul: Option<&HaulPlan>,
    workers: Option<&WorkerPlan>,
) -> u64 {
    let miners = mine.map_or(0, |m| {
        m.sources
            .iter()
            .filter_map(|s| s.body)
            .fold(0u64, |a, b| a.saturating_add(u64::from(b.cost())))
    });
    let haulers = haul.map_or(0, |h| {
        h.sources
            .iter()
            .filter_map(|s| s.body.map(|b| u64::from(b.cost()) * u64::from(s.want)))
            .fold(0u64, u64::saturating_add)
    });
    let workers = workers.map_or(0, |p| {
        p.capped
            .map_or(0, |b| u64::from(b.cost()) * u64::from(p.want))
    });
    miners.saturating_add(haulers).saturating_add(workers)
}

/// The position of tile `t` in `room`.
fn place(room: RoomName, t: Tile) -> Option<Position> {
    Some(Position::new(
        RoomCoordinate::new(t.x()).ok()?,
        RoomCoordinate::new(t.y()).ok()?,
        room,
    ))
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

/// `upgrading.worker_repeats` as a cap on WORK parts.
fn repeats(cfg: &UpgradingConfig) -> u8 {
    u8::try_from(cfg.worker_repeats).unwrap_or(u8::MAX)
}

/// The WORK parts of `plan`'s body, if it has one.
fn body_work(plan: Option<&UpgradePlan>) -> Option<u32> {
    plan.and_then(|p| p.body)
        .map(|b| u32::try_from(b.count(Part::Work)).unwrap_or(u32::MAX))
}
