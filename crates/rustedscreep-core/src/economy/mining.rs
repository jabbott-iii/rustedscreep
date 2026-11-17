//! Static miners (ROADMAP P3.1): one creep per source that stands on the source's mining tile and
//! harvests every tick the source holds energy. Its body is [`miner_work`] WORK parts and the MOVE
//! parts [`miner_body`] picks, with no CARRY: harvesting has no store check (V182), and what does
//! not fit the store goes into a container on the miner's tile or onto a pile there (V183, V193).
//! The opening's workers leave a source whose miner is next to it alone and take its energy off
//! the tile ([`Mined`]); the container the miner stands on is placed by `economy::placement`
//! (ADR-023) from [`Mining::container_needs`]. P3.2 adds links, P3.3 the haulers that replace the
//! workers' carrying.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Mining::begin`] after the snapshot: the owned rooms (the job's units, by name) and each
//!    room's miners (own creeps whose name says `miner` and that room, by id).
//! 2. The Critical `mining` job runs [`Mining::plan_room`] per room, before the opening's job and
//!    the spawn job: the room's plan (built again when movement's grid version, the room's
//!    capacity, its first spawn, its sources or the containers next to them change), each miner's
//!    source, one spawn request per source with no miner, the mined sources, the tiles that need a
//!    container site, and each miner's harvest and move.
//!
//! The mining tile is the source's walkable neighbour, not on the room's edge and not another
//! source's mining tile, nearest the room's first spawn by path on movement's grid; a tile with a
//! container or a container site wins, so a container placed earlier (by the bot or by hand) keeps
//! its miner. The miner's walk out is
//! measured on the same field. A source gets a miner only once the room's capacity pays for one
//! that drains it; below that the opening's workers harvest it as before.
//!
//! Miners keep their source between ticks in the heap. After a global reset a miner on a mining
//! tile takes that tile's source, and the others take the nearest source with no miner, so a reset
//! never orders a second miner for a mined source. Nothing is read from or written to `Memory`.

use rustedscreep_consts::{Position, RoomCoordinate, RoomName, StructureType};

use super::sizing::{Trip, miner_body};
use crate::config::{MiningConfig, MovementConfig};
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{Dial, FlowField, Goal, MoveError, MoveTo, Movement, Tile, UNREACHED};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{CreepHandle, ObjectHandle, ObjectKind, Snapshot};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner};

/// The spawn priority of a miner request: above the opening's normal worker requests
/// (`PRIORITY_NORMAL`), below a room short of workers (`PRIORITY_LOW`) or recovering
/// (`PRIORITY_RECOVERY`), whose workers carry what the miner drops.
pub const PRIORITY_MINER: u8 = 125;

/// How hard a miner's moves push past other creeps (`MoveTo::priority`): above every opening task,
/// so a worker standing on a mining tile makes way.
const MOVE_PRIORITY: u8 = 4;

/// Harvesting reaches 1 tile (`isNearTo`, V182).
const NEAR: u8 = 1;

/// One source in a room's mining plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MineSource {
    pub source: Position,
    /// Where its miner stands, or `None` when no walkable tile next to it can be reached from the
    /// spawn.
    pub tile: Option<Position>,
    /// The miner's walk from the spawn to `tile`.
    pub trip: Option<Trip>,
    /// The miner's body at the room's capacity, or `None` when the room cannot pay for one that
    /// drains the source ([`miner_body`]).
    pub body: Option<Body>,
}

/// A room's mining plan: built again when movement's grid version, the room's capacity, its first
/// spawn, its sources or the containers and container sites next to them change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinePlan {
    pub version: u32,
    pub capacity: u32,
    /// The room's first spawn by id; no spawn, no tiles.
    pub spawn: Option<Position>,
    /// Containers and container sites next to the sources. Unit: structures and sites.
    pub containers: u32,
    pub sources: Vec<MineSource>,
}

/// A source whose miner stands next to it this tick: the opening's workers do not harvest it and
/// take its energy off `tile`, where the miner stands (its mining tile, or a tile next to the source
/// on its way there or while the mining tile cannot be reached), so where its harvest lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mined {
    pub source: Position,
    pub tile: Position,
}

/// One room's counters for the console, from its last run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MineStats {
    pub time: u32,
    /// Miners of the room, spawning ones included. Unit: creeps.
    pub miners: u32,
    /// Sources with a miner next to them.
    pub mined: u32,
    /// Harvest intents pushed this tick.
    pub harvests: u32,
    /// Miner requests made this tick.
    pub requested: u32,
    /// Miners with no source to mine (more miners than sources with a tile).
    pub idle: u32,
}

struct MineRoom {
    name: RoomName,
    row: Option<usize>,
    plan: Option<MinePlan>,
    /// Members of [`Mining::members`] that belong to it this tick.
    members: (usize, usize),
    mined: Vec<Mined>,
    /// Mining tiles with no container and no container site, once the room pays for their miner.
    containers: Vec<Tile>,
    /// Mining tiles movement found no way to, each skipped until the tick given
    /// (`movement.route_retry_ticks` later): the miner goes next to its source instead.
    unreachable: Vec<(Position, u32)>,
    stats: MineStats,
}

/// One miner of this tick, in the order the job walks them: by room, then by id.
#[derive(Clone, Copy, Debug)]
struct Member {
    room: usize,
    key: u128,
    row: usize,
    handle: CreepHandle,
}

/// A miner's state between ticks, in the slot of its handle.
#[derive(Clone, Copy, Debug)]
struct Miner {
    handle: CreepHandle,
    /// The source it mines (by position: sources never move).
    source: Option<Position>,
}

/// Static mining's state for one global.
#[derive(Default)]
pub struct Mining {
    time: u32,
    rooms: Vec<MineRoom>,
    members: Vec<Member>,
    /// Indexed by creep handle slot.
    miners: Vec<Option<Miner>>,
    field: FlowField,
    dial: Dial,
    /// Scratch, kept for its capacity: the room's sources (handle, place, energy, capacity), its
    /// containers and container sites, the plan's sources with their tiles, and the member index of
    /// each plan source's miner.
    sources: Vec<(ObjectHandle, Position, u32, u32)>,
    stores: Vec<Position>,
    targets: Vec<(Position, Option<Position>)>,
    claims: Vec<Option<usize>>,
}

impl Mining {
    /// Starts a tick: the owned rooms, by name, and their miners, by room and id.
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
                    MineRoom {
                        name,
                        row: Some(row),
                        plan: None,
                        members: (0, 0),
                        mined: Vec::new(),
                        containers: Vec::new(),
                        unreachable: Vec::new(),
                        stats: MineStats::default(),
                    },
                ),
            }
        }
        self.rooms.retain(|r| r.row.is_some());
        for r in &mut self.rooms {
            // Until the room's run this tick, nothing is mined and nothing needs a container.
            r.mined.clear();
            r.containers.clear();
        }
        self.members.clear();
        let creeps = &snap.creeps;
        for (row, identity) in creeps.identities().iter().enumerate() {
            let Some(id) = identity.filter(|id| id.role == Role::Miner) else {
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
        // Few members; this runs once per tick.
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

    pub fn room_count(&self) -> u32 {
        u32::try_from(self.rooms.len()).unwrap_or(u32::MAX)
    }

    /// The name of room `unit`.
    pub fn room_name(&self, unit: u32) -> Option<RoomName> {
        self.rooms.get(usize::try_from(unit).ok()?).map(|r| r.name)
    }

    /// The sources of `room` whose miner stood next to them at its last run this tick (none before
    /// it, or for a room that is not owned).
    pub fn mined(&self, room: RoomName) -> &[Mined] {
        self.room(room).map_or(&[], |r| r.mined.as_slice())
    }

    /// The mining tiles of `room` that need a container site, from its run this tick (none while
    /// `mining.place_containers` is off).
    pub fn container_needs(&self, room: RoomName) -> &[Tile] {
        self.room(room).map_or(&[], |r| r.containers.as_slice())
    }

    /// Each room's plan and counters, by name.
    pub fn room_states(&self) -> impl Iterator<Item = (RoomName, Option<&MinePlan>, &MineStats)> {
        self.rooms
            .iter()
            .map(|r| (r.name, r.plan.as_ref(), &r.stats))
    }

    /// The source miner `handle` mines, if it is one of the room miners this module drives.
    pub fn source_of(&self, handle: CreepHandle) -> Option<Position> {
        self.miner(handle).and_then(|m| m.source)
    }

    fn room(&self, room: RoomName) -> Option<&MineRoom> {
        self.rooms
            .binary_search_by_key(&room.packed_repr(), |r| r.name.packed_repr())
            .ok()
            .and_then(|i| self.rooms.get(i))
    }

    /// Room `unit`'s tick (module docs, step 2). `cap`, the most energy the opening's watchdog saw
    /// the room hold while it never filled (`Watchdog::cap`), holds back a miner that costs more.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        buffer: &mut IntentBuffer,
        cfg: &MiningConfig,
        mcfg: &MovementConfig,
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
        let spawn = self.gather(snap, name);
        self.ensure_plan(ri, snap, row, spawn, movement, mcfg, commit);
        self.assign(ri, snap, members);
        let now = self.time;
        if let Some(r) = self.rooms.get_mut(ri) {
            r.unreachable.retain(|&(_, until)| now < until);
        }

        let Self {
            rooms,
            members: all,
            sources,
            stores,
            claims,
            ..
        } = self;
        let Some(r) = rooms.get_mut(ri) else {
            return Ok(());
        };
        let mut stats = MineStats {
            time: now,
            miners: u32::try_from(members.1.saturating_sub(members.0)).unwrap_or(u32::MAX),
            ..MineStats::default()
        };
        r.mined.clear();
        r.containers.clear();
        let plan_sources = r.plan.as_ref().map_or(&[][..], |p| p.sources.as_slice());
        for (j, s) in plan_sources.iter().enumerate() {
            let claim = claims.get(j).copied().flatten();
            // The spawn request: a source with no miner, once the room pays for one (and it has
            // a tile, without which no miner could claim it).
            if claim.is_none()
                && cfg.enabled
                && s.tile.is_some()
                && let Some(body) = s.body
                && cap.is_none_or(|c| body.cost() <= c)
            {
                let asked = spawner.request(SpawnRequest {
                    home: name,
                    role: Role::Miner,
                    body: BodySpec::Exact(body),
                    budget: EnergyBudget::Capacity,
                    priority: PRIORITY_MINER,
                });
                if asked.is_some() {
                    stats.requested = stats.requested.saturating_add(1);
                }
            }
            // A container site for its tile, once the room pays for its miner.
            if cfg.place_containers
                && s.body.is_some()
                && let Some(tile) = s.tile
                && !stores.contains(&tile)
            {
                r.containers.push(Tile::of(tile));
            }
            let Some(m) = claim.and_then(|i| all.get(i)).copied() else {
                continue;
            };
            if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
                continue;
            }
            let Some(&at) = snap.creeps.positions().get(m.row) else {
                continue;
            };
            let near = range(at, s.source) <= NEAR;
            if near {
                // Where its harvest lands this tick: its tile, or wherever it stands next to the
                // source on the way there.
                r.mined.push(Mined {
                    source: s.source,
                    tile: at,
                });
                stats.mined = stats.mined.saturating_add(1);
            }
            // Harvest while the source holds energy: an empty one would be a billed no-op.
            let source = sources.iter().find(|x| x.1 == s.source);
            if near
                && let Some(&(h, _, energy, _)) = source
                && energy > 0
                && buffer
                    .push(snap, m.handle, CreepIntent::Harvest { target: h }, commit)
                    .is_ok()
            {
                stats.harvests = stats.harvests.saturating_add(1);
            }
            // Onto its tile, or next to the source while the tile cannot be reached.
            let blocked = |p: Position| r.unreachable.iter().any(|u| u.0 == p);
            let (target, reach) = match s.tile {
                Some(tile) if !blocked(tile) => (tile, 0),
                _ => (s.source, NEAR),
            };
            let to = MoveTo {
                target,
                range: reach,
                priority: MOVE_PRIORITY,
                tier: Tier::Critical,
            };
            if movement.request(snap, m.handle, to, mcfg, commit) == Err(MoveError::NoPath)
                && reach == 0
            {
                let until = now.saturating_add(mcfg.route_retry_ticks);
                r.unreachable.push((target, until));
            }
        }
        // Miners with no source make way.
        for i in members.0..members.1 {
            if claims.contains(&Some(i)) {
                continue;
            }
            let Some(m) = all.get(i) else {
                continue;
            };
            if !snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
                movement.give_way(snap, m.handle, None);
                stats.idle = stats.idle.saturating_add(1);
            }
        }
        r.stats = stats;
        Ok(())
    }

    fn miner(&self, handle: CreepHandle) -> Option<&Miner> {
        let slot = usize::try_from(handle.slot()).ok()?;
        self.miners
            .get(slot)?
            .as_ref()
            .filter(|m| m.handle == handle)
    }

    /// The state of the miner in `members[i]`, created when its slot holds another creep or none.
    fn miner_mut(&mut self, i: usize) -> Option<&mut Miner> {
        let m = *self.members.get(i)?;
        let slot = usize::try_from(m.handle.slot()).ok()?;
        if self.miners.len() <= slot {
            self.miners.resize(slot + 1, None);
        }
        let cell = self.miners.get_mut(slot)?;
        if cell.is_none_or(|w| w.handle != m.handle) {
            *cell = Some(Miner {
                handle: m.handle,
                source: None,
            });
        }
        cell.as_mut()
    }

    /// Reads room `name`'s sources (by tile), containers and container sites into the scratch
    /// lists, and returns its first spawn by id.
    fn gather(&mut self, snap: &Snapshot, name: RoomName) -> Option<Position> {
        self.sources.clear();
        self.stores.clear();
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
                ObjectKind::Source => self.sources.push((h, pos, amount, total)),
                ObjectKind::Container | ObjectKind::Site(StructureType::Container) => {
                    self.stores.push(pos);
                }
                _ => {}
            }
        }
        // Sources in a fixed order (rows follow the engine's, V102): by place.
        self.sources.sort_unstable_by_key(|s| tile_key(s.1));
        let st = &snap.structures;
        let mut spawn: Option<(u128, Position)> = None;
        for ((&h, &kind), &pos) in st.handles().iter().zip(st.kinds()).zip(st.positions()) {
            if kind != StructureType::Spawn || pos.room_name() != name {
                continue;
            }
            let key = st.key_of(h).map_or(u128::MAX, |k| k.0);
            if spawn.is_none_or(|(k, _)| key < k) {
                spawn = Some((key, pos));
            }
        }
        spawn.map(|(_, pos)| pos)
    }

    /// Builds room `ri`'s plan when it has none or its key changed (module docs). Without a grid
    /// yet (the room's terrain or first scan is still to come), the room keeps its last plan, or has
    /// none until then.
    #[allow(clippy::too_many_arguments)]
    fn ensure_plan(
        &mut self,
        ri: usize,
        snap: &Snapshot,
        row: usize,
        spawn: Option<Position>,
        movement: &mut Movement,
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
        let near_source = |p: &Position| self.sources.iter().any(|s| range(*p, s.1) <= NEAR);
        let containers =
            u32::try_from(self.stores.iter().filter(|p| near_source(p)).count()).unwrap_or(0);
        if r.plan.as_ref().is_some_and(|p| {
            p.version == version
                && p.capacity == capacity
                && p.spawn == spawn
                && p.containers == containers
                && p.sources.len() == self.sources.len()
                && p.sources
                    .iter()
                    .zip(&self.sources)
                    .all(|(a, b)| a.source == b.1)
        }) {
            return;
        }
        let reached = match spawn {
            Some(pos) => {
                commit.heavy();
                let goal = Goal {
                    tile: Tile::of(pos),
                    range: NEAR,
                };
                self.field.compute(grid, goal, &mut self.dial) > 0
            }
            None => false,
        };
        let mut sources: Vec<MineSource> = Vec::with_capacity(self.sources.len());
        for &(_, pos, _, source_capacity) in &self.sources {
            let at = Tile::of(pos);
            let has_store = |t: Tile| self.stores.iter().any(|&p| Tile::of(p) == t);
            // Two sources can share a neighbour: each tile holds one miner.
            let taken = |t: Tile| {
                sources
                    .iter()
                    .any(|s| s.tile.is_some_and(|p| Tile::of(p) == t))
            };
            let pick = at
                .neighbors()
                .map(|(_, t)| t)
                .filter(|&t| reached && grid.walkable(t) && !t.is_edge() && !taken(t))
                .map(|t| (t, self.field.distance(t)))
                .filter(|&(_, d)| d != UNREACHED)
                .min_by_key(|&(t, d)| (!has_store(t), d, t.index()));
            let tile = pick.and_then(|(t, _)| place(name, t));
            let trip = match (pick, spawn) {
                (Some((t, d)), Some(s)) => Some(Trip {
                    // It leaves the spawn from a neighbouring tile.
                    tiles: u32::from(t.range(Tile::of(s))).saturating_sub(u32::from(NEAR)),
                    cost: u32::from(d),
                }),
                _ => None,
            };
            let body = trip.and_then(|t| miner_body(source_capacity, t, capacity));
            sources.push(MineSource {
                source: pos,
                tile,
                trip,
                body,
            });
        }
        if let Some(r) = self.rooms.get_mut(ri) {
            r.plan = Some(MinePlan {
                version,
                capacity,
                spawn,
                containers,
                sources,
            });
        }
    }

    /// Each miner's source for this tick (module docs): one kept from the last tick first, then a
    /// miner standing on a mining tile takes it, then the rest, by id, take the nearest source with
    /// no miner. `self.claims` holds the result per plan source.
    fn assign(&mut self, ri: usize, snap: &Snapshot, members: (usize, usize)) {
        let mut claims = std::mem::take(&mut self.claims);
        let mut plan = std::mem::take(&mut self.targets);
        claims.clear();
        plan.clear();
        if let Some(p) = self.rooms.get(ri).and_then(|r| r.plan.as_ref()) {
            plan.extend(p.sources.iter().map(|s| (s.source, s.tile)));
        }
        claims.resize(plan.len(), None);
        // Kept sources.
        for i in members.0..members.1 {
            let Some(w) = self.miner_mut(i) else {
                continue;
            };
            let kept = w.source.and_then(|src| {
                plan.iter()
                    .position(|p| p.0 == src && p.1.is_some())
                    .filter(|&j| claims.get(j).copied().flatten().is_none())
            });
            match kept {
                Some(j) => {
                    if let Some(c) = claims.get_mut(j) {
                        *c = Some(i);
                    }
                }
                None => w.source = None,
            }
        }
        // Then a miner on a mining tile, then the nearest source with no miner.
        for pass in 0..2 {
            for i in members.0..members.1 {
                if claims.contains(&Some(i)) {
                    continue;
                }
                let Some(m) = self.members.get(i).copied() else {
                    continue;
                };
                let Some(&at) = snap.creeps.positions().get(m.row) else {
                    continue;
                };
                let free = |j: usize| claims.get(j).copied().flatten().is_none();
                let pick = if pass == 0 {
                    plan.iter()
                        .enumerate()
                        .find(|&(j, p)| free(j) && p.1 == Some(at))
                        .map(|(j, _)| j)
                } else {
                    plan.iter()
                        .enumerate()
                        .filter(|&(j, p)| free(j) && p.1.is_some())
                        .min_by_key(|&(j, p)| (range(at, p.0), j))
                        .map(|(j, _)| j)
                };
                let Some(j) = pick else {
                    continue;
                };
                if let Some(c) = claims.get_mut(j) {
                    *c = Some(i);
                }
                let source = plan.get(j).map(|p| p.0);
                if let Some(w) = self.miner_mut(i) {
                    w.source = source;
                }
            }
        }
        self.claims = claims;
        self.targets = plan;
    }
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
