//! Static miners (ROADMAP P3.1) and the link network (P3.2): one creep per source that stands on the
//! source's mining tile and harvests every tick the source holds energy, and from the level that
//! allows links a source link beside each mining tile that sends to the controller link.
//!
//! A miner's body is [`miner_work`] WORK parts and the MOVE parts [`miner_body`] picks. With no
//! CARRY, harvesting has no store check (V182), and what does not fit the store goes into a
//! container on the miner's tile or onto a pile there (V183, V193). The opening's workers leave a
//! source whose miner is next to it alone and take its energy off the tile ([`Mined`]); the
//! container the miner stands on is placed by `economy::placement` (ADR-023) from
//! [`Mining::container_needs`]. `economy::hauling`'s haulers (P3.3) carry it from there.
//!
//! The controller container (ADR-025): the plan also holds a tile within upgrading range of the
//! controller where the haulers unload and the workers take: a container or container site within
//! [`RECEIVER_RANGE`] of the controller that no source's mining tile holds, else a free tile at
//! [`CONTROLLER_LINK_RANGE`], nearest the spawn by path, under the link tiles' rules (but a
//! container blocks no move, so its neighbours need not stay connected), off the link tiles and
//! not next to a source or a mineral. [`Mining::container_needs`] lists it after the mining tiles
//! once a source with no link pays for its miner, from the level the caller asks for.
//!
//! Links (ADR-024): the plan holds a controller link tile and a source link tile per mining tile;
//! `economy::placement` places them from [`Mining::link_needs`]. A miner of a source with a built
//! source link gets one CARRY part ([`miner_body_with`]): it transfers its store into the link on
//! the tick its harvest would overflow (a creep's transfer runs before its harvest, V45), so the
//! harvest lands in an empty store; while the link is full the harvest overflows into the container
//! as before; an inactive link (V218) that is not a controller link gets neither the transfers nor
//! the CARRY part, since it could never pass the energy on. A source link that holds at least
//! `links.send_min` sends that much or more to the controller link with the most room, an explicit
//! amount the receiver's room at the start of the tick can take (V216, V217), once a tick, and a
//! refused send holds its link for `links.hold_ticks`. The receiver gets the amount less `LINK_LOSS_RATIO` of it, rounded
//! up ([`link_loss`]), and the sender waits `LINK_COOLDOWN` per tile between them. The opening's
//! workers take whole loads from the controller link ([`Receiver`]) and wait next to it for a
//! linked source's energy ([`Mined::link`]). When the controller link backs up, the source links
//! fill, the miners stop transferring and the containers take the harvest again: nothing is lost.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Mining::begin`] after the snapshot: the owned rooms (the job's units, by name) and each
//!    room's miners (own creeps whose name says `miner` and that room, by id).
//! 2. The Critical `mining` job runs [`Mining::plan_room`] per room, before the opening's job and
//!    the spawn job: the room's plan (built again when movement's grid version, the room's
//!    capacity, its first spawn, its controller, its sources, the containers next to them, its links
//!    and link sites, the network's switch, the level or the placement's refused tiles change), each
//!    miner's source, one spawn request per source with no miner, the mined sources, the tiles that
//!    need a container or a link site (never a refused one), each miner's transfer, harvest and move,
//!    and whether a source link may send.
//! 3. The Standard `links` job runs [`Mining::send_room`] per room with a link that may send: each
//!    source link sends at most once ([`Mining::link_orders`]; moving energy is hauling, which
//!    CLAUDE.md puts in Standard). With the tier off the source link fills, the miner stops
//!    transferring and the container takes the harvest.
//!
//! The mining tile is the source's walkable neighbour, not on the room's edge and not another
//! source's mining tile, nearest the room's first spawn by path on movement's grid; a tile with a
//! container or a container site wins, so a container placed earlier (by the bot or by hand) keeps
//! its miner. The miner's walk out is measured on the same field. A source gets a miner only once
//! the room's capacity pays for one that drains it; below that the opening's workers harvest it as
//! before. Link tiles follow the extension pattern's rules for not closing the room (ADR-020): a
//! link tile is free, off any road, at x and y of [`LINK_MARGIN`] to `ROOM_SIZE - 1 -
//! LINK_MARGIN`, not a mining tile, and its free neighbours stay connected without it.
//!
//! Miners keep their source between ticks in the heap. After a global reset a miner on a mining
//! tile takes that tile's source, and the others take the nearest source with no miner, so a reset
//! never orders a second miner for a mined source. Link roles are read from the links' places every
//! tick. Nothing is read from or written to `Memory`.

use rustedscreep_consts::{
    CREEP_LIFE_TIME, ENERGY_REGEN_TIME, HARVEST_POWER, INTENT_CPU_COST, LINK_CAPACITY,
    LINK_LOSS_RATIO, Position, ROOM_SIZE, ResourceType, RoomCoordinate, RoomName, StructureType,
};

use super::placement::keeps_neighbours_connected;
use super::sizing::{Trip, miner_body, miner_body_with, miner_work};
use crate::config::{LinkConfig, MiningConfig, MovementConfig};
use crate::intents::{CreepIntent, IntentBuffer, Outcome, Target};
use crate::movement::{
    CostGrid, Dial, FlowField, Goal, MoveError, MoveTo, Movement, Tile, UNREACHED,
};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{
    Controller, CreepHandle, ObjectHandle, ObjectKind, Snapshot, StructureHandle,
};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner};

/// The spawn priority of a miner request: above the opening's normal worker requests
/// (`PRIORITY_NORMAL`), below a room short of workers (`PRIORITY_LOW`) or recovering
/// (`PRIORITY_RECOVERY`), whose workers carry what the miner drops.
pub const PRIORITY_MINER: u8 = 125;

/// How far from the controller the bot plans its controller link (ADR-024): every tile next to a
/// link at this range is within `upgradeController`'s range of the controller (V187), so up to
/// eight upgraders can stand around it.
pub const CONTROLLER_LINK_RANGE: u8 = 2;

/// A link this close to the controller is a controller link, whoever placed it: an upgrader next to
/// it can stand within `upgradeController`'s range (`inRangeTo(target, 3)`, V187).
pub const RECEIVER_RANGE: u8 = 3;

/// The least x and y of a planned link tile (and `ROOM_SIZE - 1` less it, the most): clear of the
/// engine's edge and exit rules for sites (V205), which refuse a link at 0 or 49 and, beside an
/// exit, at 1 or 48.
pub const LINK_MARGIN: u8 = 2;

/// How hard a miner's moves push past other creeps (`MoveTo::priority`): above every opening task,
/// so a worker standing on a mining tile makes way.
const MOVE_PRIORITY: u8 = 4;

/// Harvesting and transferring reach 1 tile (`isNearTo`, V182, V108).
const NEAR: u8 = 1;

/// The energy a link's send of `amount` loses on the way: `Math.ceil(amount * LINK_LOSS_RATIO)`
/// (links/transfer.js L46, V217). The engine's ratio is the double 0.03 and the crate's the f32
/// nearest it (V134); over every amount from 1 to `LINK_CAPACITY` both give the same loss (the
/// test checks it against the double), which is all one send can move. Unit: energy.
///
/// Uses: `LINK_LOSS_RATIO`.
pub fn link_loss(amount: u32) -> u32 {
    let loss = (f64::from(amount) * f64::from(LINK_LOSS_RATIO)).ceil();
    if loss <= 0.0 {
        0
    } else if loss >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        loss as u32
    }
}

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
    /// drains the source ([`miner_body`]); with one CARRY part while a built link stands next to
    /// `tile` and the network runs ([`miner_body_with`]).
    pub body: Option<Body>,
    /// Its source link's tile (ADR-024): a link or a link site next to `tile`, else the tile picked
    /// for one; `None` without a mining tile or a tile that qualifies.
    pub link: Option<Position>,
    /// A built link that can pass the energy on stands next to `tile` and the network runs: its
    /// miner gets a CARRY part and sends through the link, so no hauler carries it (P3.3).
    pub linked: bool,
}

/// A room's mining plan: built again when movement's grid version, the room's capacity, its first
/// spawn, its controller, its sources, the containers and container sites next to them, its links
/// and link sites or the network's switch change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinePlan {
    pub version: u32,
    pub capacity: u32,
    /// The room's first spawn by id; no spawn, no tiles.
    pub spawn: Option<Position>,
    /// Containers and container sites next to the sources or within [`RECEIVER_RANGE`] of the
    /// controller. Unit: structures and sites.
    pub containers: u32,
    pub controller: Option<Position>,
    /// The room's links and link sites, and of them the built links. Unit: structures and sites.
    pub links: u32,
    pub built: u32,
    /// `links.enabled` when the plan was built (a miner's CARRY part depends on it), the level
    /// (which links are active, V218) and the placement's refused tiles in the room (none is picked).
    pub network: bool,
    pub level: u8,
    pub refused: u32,
    /// The controller link's tile (ADR-024): a link or a link site within [`RECEIVER_RANGE`] of the
    /// controller, else the tile picked for one at [`CONTROLLER_LINK_RANGE`], nearest the spawn by
    /// path; `None` when none qualifies.
    pub controller_link: Option<Position>,
    /// The controller container's tile (ADR-025, module docs); `None` when none qualifies.
    pub controller_container: Option<Position>,
    /// A container or a container site stands on that tile (the plan's key counts them, so a new
    /// one or one gone rebuilds the plan).
    pub controller_store: bool,
    pub sources: Vec<MineSource>,
}

/// A source whose miner stands next to it this tick: the opening's workers do not harvest it and
/// take its energy off `tile`, where the miner stands (its mining tile, or a tile next to the source
/// on its way there or while the mining tile cannot be reached), so where its harvest lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mined {
    pub source: Position,
    pub tile: Position,
    /// Where the source's energy goes instead of `tile` (P3.2): the controller link's place, while
    /// the miner holds a CARRY part next to a source link that sends there or is that link. The
    /// workers take its loads there; only an overflow lands on `tile`.
    pub link: Option<Position>,
}

/// A controller link (P3.2): an own link within [`RECEIVER_RANGE`] of the controller, where the
/// opening's workers take energy to upgrade with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receiver {
    pub link: StructureHandle,
    pub pos: Position,
    /// The energy it holds at the start of the tick. Unit: energy.
    pub energy: u32,
    /// No send fits it: its room is below `links.send_min` (or the network is off), so energy
    /// arrives only once a worker takes some. Then any take is worth it, not only a whole load.
    pub full: bool,
}

/// One `StructureLink.transferEnergy(target, amount)` call for the act step (V216).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkOrder {
    pub from: StructureHandle,
    /// The sender's and the receiver's rows in this tick's structures (their JS objects).
    pub from_row: u32,
    pub to: StructureHandle,
    pub to_row: u32,
    /// Unit: energy, at least 1 and at most the sender's energy and the receiver's room.
    pub amount: u32,
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
    /// Built links in the room. Unit: structures.
    pub links: u32,
    /// Mined sources whose energy goes to a controller link this tick.
    pub linked: u32,
    /// Miner transfers into source links pushed this tick.
    pub transfers: u32,
    /// Link sends ordered this tick.
    pub sends: u32,
}

/// Link sends since the global started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkTotals {
    pub ordered: u32,
    /// Of `ordered`, calls the game accepted.
    pub sent: u32,
    pub failed: u32,
    /// What the accepted sends moved, before the loss. Unit: energy.
    pub energy: u64,
}

/// Regeneration cycles in one creep life (`CREEP_LIFE_TIME` / `ENERGY_REGEN_TIME`, 5): the P3
/// gate's harvest efficiency is a miner life's mean (`MiningConfig::harvest_efficiency_pct`).
///
/// Uses: `CREEP_LIFE_TIME`, `ENERGY_REGEN_TIME`.
pub const LIFE_CYCLES: usize = (CREEP_LIFE_TIME / ENERGY_REGEN_TIME) as usize;

/// One source's harvest per regeneration cycle, for the P3 gate (TEST_PLAN §8): what the cycle
/// harvested, its capacity less the energy it left at the refill. A source refills to its capacity
/// in one step after a tick's harvests (V184, V215), and nothing else raises its energy before P9's
/// power creeps (`PWR_REGEN_SOURCE`), so a rise between two ticks' snapshots is a refill, and the
/// energy it left is the earlier snapshot's less what that tick's harvest took: `min(energy, WORK ×
/// HARVEST_POWER)` of the miner's harvest pushed that tick (V183), none without one. Every cycle a
/// miner harvested in counts, the one its miner died in too (the gate's mean is over a life for
/// that loss, TUNING); a cycle no miner harvested in (an unmined source, its workers' harvests
/// unknown here) and one that ends after a skipped tick (no snapshot between) are counted apart
/// and kept out of the means. The miner's take on the last tick is the plan's WORK parts ×
/// `HARVEST_POWER` once its harvest is pushed (the snapshot holds no body): a cycle reads higher
/// than it was by at most that when the miner's WORK parts are damaged or its harvest refused on
/// that tick (10 of a 3000 source's cycle), and lower when someone else harvests the source then
/// (a worker, its miner gone). In the heap: a global reset starts it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceHarvest {
    pub source: Position,
    /// The last snapshot of it: the tick, its energy, and at most what that tick's miner harvest
    /// took (0 without one).
    last: Option<(u32, u32, u32)>,
    /// A miner harvested it since the last refill.
    mined: bool,
    /// The last [`LIFE_CYCLES`] cycles counted, oldest first: (harvested, capacity), `len` of them.
    recent: [(u32, u32); LIFE_CYCLES],
    len: u8,
    /// Cycles counted since the global started, and what they harvested of what they held. Unit:
    /// cycles, energy.
    pub cycles: u32,
    pub harvested: u64,
    pub capacity: u64,
    /// Refills kept out: of a cycle no miner harvested in, and after a skipped tick.
    pub unmined: u32,
    pub missed: u32,
}

impl SourceHarvest {
    pub const fn new(source: Position) -> Self {
        Self {
            source,
            last: None,
            mined: false,
            recent: [(0, 0); LIFE_CYCLES],
            len: 0,
            cycles: 0,
            harvested: 0,
            capacity: 0,
            unmined: 0,
            missed: 0,
        }
    }

    /// This tick's snapshot of the source, before any harvest: a rise over the last one ends a
    /// cycle (struct docs).
    pub fn observe(&mut self, now: u32, energy: u32, capacity: u32) {
        if let Some((time, before, took)) = self.last
            && energy > before
        {
            if time.saturating_add(1) != now {
                self.missed = self.missed.saturating_add(1);
            } else if !self.mined {
                self.unmined = self.unmined.saturating_add(1);
            } else {
                let left = before.saturating_sub(took.min(before));
                self.record(capacity.saturating_sub(left), capacity);
            }
            self.mined = false;
        }
        self.last = Some((now, energy, 0));
    }

    /// A miner's harvest of the source this tick that takes at most `power` (its WORK parts ×
    /// `HARVEST_POWER`).
    pub fn harvest(&mut self, power: u32) {
        if let Some((_, _, took)) = self.last.as_mut() {
            *took = took.saturating_add(power);
            self.mined = true;
        }
    }

    fn record(&mut self, harvested: u32, capacity: u32) {
        self.cycles = self.cycles.saturating_add(1);
        self.harvested = self.harvested.saturating_add(u64::from(harvested));
        self.capacity = self.capacity.saturating_add(u64::from(capacity));
        let len = usize::from(self.len);
        if len < LIFE_CYCLES {
            if let Some(slot) = self.recent.get_mut(len) {
                *slot = (harvested, capacity);
                self.len = self.len.saturating_add(1);
            }
        } else {
            self.recent.rotate_left(1);
            if let Some(slot) = self.recent.last_mut() {
                *slot = (harvested, capacity);
            }
        }
    }

    /// The last cycles counted, oldest first, at most [`LIFE_CYCLES`]: (harvested, capacity).
    pub fn recent(&self) -> &[(u32, u32)] {
        self.recent.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// The P3 gate's figures over [`Self::recent`] (up to one miner life): what they harvested of
    /// what they held, and the worst cycle, in permille ([`permille`]); `None` before the first
    /// cycle. Unit: permille.
    pub fn life(&self) -> Option<(u64, u64)> {
        let (got, held) = self
            .recent()
            .iter()
            .fold((0u64, 0u64), |(g, c), &(h, cap)| {
                (
                    g.saturating_add(u64::from(h)),
                    c.saturating_add(u64::from(cap)),
                )
            });
        let worst = self
            .recent()
            .iter()
            .filter_map(|&(h, cap)| permille(u64::from(h), u64::from(cap)))
            .min()?;
        Some((permille(got, held)?, worst))
    }

    /// What every counted cycle since the global started harvested of what it held; `None`
    /// before the first. Unit: permille.
    pub fn overall(&self) -> Option<u64> {
        permille(self.harvested, self.capacity)
    }
}

/// What `harvested` is of `capacity`, in tenths of a percent, rounded down; `None` for no
/// capacity. Unit: permille.
pub fn permille(harvested: u64, capacity: u64) -> Option<u64> {
    harvested.saturating_mul(1000).checked_div(capacity)
}

struct MineRoom {
    name: RoomName,
    row: Option<usize>,
    plan: Option<MinePlan>,
    /// The plan's mining tiles (P3.5: the containers the workers repair), set with the plan.
    tiles: Vec<Position>,
    /// Members of [`Mining::members`] that belong to it this tick.
    members: (usize, usize),
    mined: Vec<Mined>,
    /// Mining tiles with no container and no container site, once the room pays for their miner.
    containers: Vec<Tile>,
    /// Link tiles with no link and no link site, in the order to place them (ADR-024).
    link_sites: Vec<Tile>,
    receivers: Vec<Receiver>,
    /// This tick's built links with what the miners promised them, and the controller, for the
    /// Standard `links` job ([`Mining::send_room`]).
    links: Vec<RoomLink>,
    controller: Option<Position>,
    /// Mining tiles movement found no way to, each skipped until the tick given
    /// (`movement.route_retry_ticks` later): the miner goes next to its source instead.
    unreachable: Vec<(Position, u32)>,
    stats: MineStats,
    /// Each source's harvest per regeneration cycle (the P3 gate), by place.
    harvest: Vec<SourceHarvest>,
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

/// One built link of the room being run.
#[derive(Clone, Copy, Debug)]
struct RoomLink {
    handle: StructureHandle,
    row: u32,
    pos: Position,
    energy: u32,
    cooldown: u32,
    /// Among the links the level allows (the engine keeps the nearest to the controller active,
    /// V218); an inactive link cannot send.
    active: bool,
    /// Energy promised to it this tick: miner transfers in, sends in.
    incoming: u32,
}

/// What [`Mining::gather`] found in a room besides the scratch lists.
#[derive(Clone, Copy, Debug)]
struct Found {
    spawn: Option<Position>,
    controller: Option<Position>,
    level: u8,
}

/// Static mining's state for one global.
#[derive(Default)]
pub struct Mining {
    time: u32,
    rooms: Vec<MineRoom>,
    members: Vec<Member>,
    /// Indexed by creep handle slot.
    miners: Vec<Option<Miner>>,
    orders: Vec<LinkOrder>,
    totals: LinkTotals,
    /// The rooms (by index) with a link that may send this tick: the Standard `links` job's units.
    send_rooms: Vec<usize>,
    /// Senders whose last call the game refused, each held until the tick given
    /// (`links.hold_ticks` later), so a refusal repeats at most once per hold.
    held: Vec<(StructureHandle, u32)>,
    field: FlowField,
    dial: Dial,
    /// Scratch, kept for its capacity: the room's sources (handle, place, energy, capacity), its
    /// containers and container sites, its links and link sites, its built links, the tiles a
    /// structure, a site or a container takes (with whether it blocks movement), its sources and
    /// minerals, the plan's sources with their tiles, and the member index of each plan source's
    /// miner.
    sources: Vec<(ObjectHandle, Position, u32, u32)>,
    stores: Vec<Position>,
    link_spots: Vec<Position>,
    links: Vec<RoomLink>,
    occupied: Vec<(Tile, bool)>,
    statics: Vec<Tile>,
    targets: Vec<(Position, Option<Position>)>,
    claims: Vec<Option<usize>>,
}

impl Mining {
    /// Starts a tick: the owned rooms, by name, their miners, by room and id, and no link orders.
    pub fn begin(&mut self, snap: &Snapshot) {
        self.time = snap.tick.time;
        self.orders.clear();
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
                        tiles: Vec::new(),
                        members: (0, 0),
                        mined: Vec::new(),
                        containers: Vec::new(),
                        link_sites: Vec::new(),
                        receivers: Vec::new(),
                        links: Vec::new(),
                        controller: None,
                        unreachable: Vec::new(),
                        stats: MineStats::default(),
                        harvest: Vec::new(),
                    },
                ),
            }
        }
        self.rooms.retain(|r| r.row.is_some());
        for r in &mut self.rooms {
            // Until the room's run this tick, nothing is mined and nothing needs a site.
            r.mined.clear();
            r.containers.clear();
            r.link_sites.clear();
            r.receivers.clear();
            r.links.clear();
        }
        self.send_rooms.clear();
        self.held.retain(|&(_, until)| self.time < until);
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

    /// The mining tiles of `room`'s plan, one per source that has one, and its controller container's
    /// tile (ADR-025) (P3.5: the containers on them are the ones the workers repair), whether or not
    /// a miner stands there this tick.
    pub fn mine_tiles(&self, room: RoomName) -> &[Position] {
        self.room(room).map_or(&[], |r| r.tiles.as_slice())
    }

    /// `room`'s plan, from its last run (none before the room's grid is known).
    pub fn plan(&self, room: RoomName) -> Option<&MinePlan> {
        self.room(room).and_then(|r| r.plan.as_ref())
    }

    /// The controller links of `room` from its run this tick (P3.2).
    pub fn receivers(&self, room: RoomName) -> &[Receiver] {
        self.room(room).map_or(&[], |r| r.receivers.as_slice())
    }

    /// The tiles of `room` that need a container site, from its run this tick: the mining tiles
    /// (none while `mining.place_containers` is off), then the controller container's (ADR-025;
    /// only while the run was asked for it).
    pub fn container_needs(&self, room: RoomName) -> &[Tile] {
        self.room(room).map_or(&[], |r| r.containers.as_slice())
    }

    /// The link tiles of `room` that need a link site, in the order to place them (ADR-024): from
    /// its run this tick, none while `links.place_links` is off or below the level that allows links.
    pub fn link_needs(&self, room: RoomName) -> &[Tile] {
        self.room(room).map_or(&[], |r| r.link_sites.as_slice())
    }

    /// This tick's link sends, for the act step.
    pub fn link_orders(&self) -> &[LinkOrder] {
        &self.orders
    }

    /// The outcome of link order `index`. A sender whose call the game refused (or that threw)
    /// sends no more for `lcfg.hold_ticks`: the planner models every check of V216, so a refusal
    /// means something it cannot see, and the hold keeps it from repeating every tick.
    pub fn record_send(&mut self, index: usize, outcome: Outcome, lcfg: &LinkConfig) {
        let Some(order) = self.orders.get(index) else {
            return;
        };
        let t = &mut self.totals;
        if outcome == Outcome::Ok {
            t.sent = t.sent.saturating_add(1);
            t.energy = t.energy.saturating_add(u64::from(order.amount));
        } else {
            t.failed = t.failed.saturating_add(1);
            let until = self.time.saturating_add(lcfg.hold_ticks.max(1));
            self.held.push((order.from, until));
        }
    }

    /// Units of the Standard `links` job: the rooms with a source link that may send this tick
    /// (from their `mining` runs).
    pub fn send_count(&self) -> u32 {
        u32::try_from(self.send_rooms.len()).unwrap_or(u32::MAX)
    }

    /// The Standard `links` job's unit: in the room, each built source link (next to a mining tile)
    /// that is not a controller link, is active, has no cooldown, is not held and holds at least
    /// `links.send_min` sends once, to the controller link with the most room left this tick (after
    /// the miners' transfers and the earlier sends), an explicit amount: the smaller of its energy
    /// and that room, when it reaches `send_min` (V216, V217). A link sends at most once a tick: the
    /// engine keeps one call per object and method, the last (V34).
    ///
    /// Uses: `LINK_CAPACITY` (a link's room), `INTENT_CPU_COST` (each send, V34).
    pub fn send_room(
        &mut self,
        unit: u32,
        lcfg: &LinkConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let ri = usize::try_from(unit)
            .ok()
            .and_then(|u| self.send_rooms.get(u))
            .copied()
            .ok_or(Fault::new("no room for the unit", unit))?;
        let Self {
            rooms,
            orders,
            totals,
            held,
            ..
        } = self;
        let MineRoom {
            plan,
            links,
            controller,
            stats,
            ..
        } = rooms
            .get_mut(ri)
            .ok_or(Fault::new("no room for the unit", unit))?;
        let controller = *controller;
        let receiver = |p: Position| controller.is_some_and(|c| range(p, c) <= RECEIVER_RANGE);
        let plan_sources = plan.as_ref().map_or(&[][..], |p| p.sources.as_slice());
        let mining_tile = |p: Position| {
            plan_sources
                .iter()
                .any(|s| s.tile.is_some_and(|t| range(p, t) <= NEAR))
        };
        let room = |l: &RoomLink| {
            LINK_CAPACITY
                .saturating_sub(l.energy)
                .saturating_sub(l.incoming)
        };
        let send_min = lcfg.send_min.max(1);
        let mut sends = 0u32;
        for i in 0..links.len() {
            let Some(from) = links.get(i).copied() else {
                continue;
            };
            if !(from.active
                && from.cooldown == 0
                && from.energy >= send_min
                && !receiver(from.pos)
                && mining_tile(from.pos)
                && !held.iter().any(|h| h.0 == from.handle))
            {
                continue;
            }
            let best = links
                .iter()
                .enumerate()
                .filter(|(_, l)| receiver(l.pos) && l.handle != from.handle)
                .max_by_key(|(_, l)| (room(l), std::cmp::Reverse(tile_key(l.pos))))
                .map(|(j, _)| j);
            let Some(to) = best.and_then(|j| links.get_mut(j)) else {
                continue;
            };
            let amount = from.energy.min(room(to));
            if amount < send_min {
                continue;
            }
            to.incoming = to.incoming.saturating_add(amount);
            orders.push(LinkOrder {
                from: from.handle,
                from_row: from.row,
                to: to.handle,
                to_row: to.row,
                amount,
            });
            commit.add(INTENT_CPU_COST);
            sends = sends.saturating_add(1);
        }
        stats.sends = stats.sends.saturating_add(sends);
        totals.ordered = totals.ordered.saturating_add(sends);
        Ok(())
    }

    pub fn link_totals(&self) -> &LinkTotals {
        &self.totals
    }

    /// Each source's harvest per regeneration cycle in `room` (the P3 gate), since the global
    /// started.
    pub fn harvest(&self, room: RoomName) -> &[SourceHarvest] {
        self.room(room).map_or(&[], |r| &r.harvest[..])
    }

    /// `room`'s counters, from its last run (P3.4's upgraders count on its miners).
    pub fn stats(&self, room: RoomName) -> Option<&MineStats> {
        self.room(room).map(|r| &r.stats)
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
    /// `refused` are the room's tiles the placement gave up on (`Placement::refused_tiles`): the
    /// plan picks no link tile or controller container tile there. `controller_container` asks for
    /// the controller container's site from that level on (ADR-025: haulers and their container
    /// site on, `hauling.container_level`; `None` asks for none).
    ///
    /// Uses: `HARVEST_POWER` and [`miner_work`] (a miner's harvest a tick), `LINK_CAPACITY` (a
    /// link's room), `CONTROLLER_STRUCTURES` (`StructureType::controller_structures`: the links the
    /// level allows).
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        buffer: &mut IntentBuffer,
        cfg: &MiningConfig,
        lcfg: &LinkConfig,
        mcfg: &MovementConfig,
        cap: Option<u32>,
        refused: &[Tile],
        controller_container: Option<u32>,
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
        let found = self.gather(snap, name, row);
        let allowed = StructureType::Link.controller_structures(u32::from(found.level));
        // Before the plan: a miner's CARRY part is only for a link that can pass the energy on.
        self.rank_links(found.controller, allowed);
        self.ensure_plan(ri, snap, row, found, movement, lcfg, mcfg, refused, commit);
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
            link_spots,
            links,
            claims,
            send_rooms,
            held,
            ..
        } = self;
        let Some(r) = rooms.get_mut(ri) else {
            return Ok(());
        };
        let mut stats = MineStats {
            time: now,
            miners: u32::try_from(members.1.saturating_sub(members.0)).unwrap_or(u32::MAX),
            links: u32::try_from(links.len()).unwrap_or(u32::MAX),
            ..MineStats::default()
        };
        r.mined.clear();
        r.containers.clear();
        r.link_sites.clear();
        r.receivers.clear();
        // Each source's snapshot for its harvest cycles (the P3 gate), before the miners harvest.
        for &(_, pos, energy, capacity) in sources.iter() {
            let i = match r.harvest.iter().position(|h| h.source == pos) {
                Some(i) => i,
                None => {
                    r.harvest.push(SourceHarvest::new(pos));
                    r.harvest.len() - 1
                }
            };
            if let Some(h) = r.harvest.get_mut(i) {
                h.observe(now, energy, capacity);
            }
        }
        let receiver = |p: Position| {
            found
                .controller
                .is_some_and(|c| range(p, c) <= RECEIVER_RANGE)
        };
        r.receivers
            .extend(links.iter().filter(|l| receiver(l.pos)).map(|l| Receiver {
                link: l.handle,
                pos: l.pos,
                energy: l.energy,
                full: !lcfg.enabled
                    || LINK_CAPACITY.saturating_sub(l.energy) < lcfg.send_min.max(1),
            }));
        let receiver_at = r
            .receivers
            .iter()
            .map(|x| x.pos)
            .min_by_key(|&p| tile_key(p));
        let plan = r.plan.as_ref();
        let plan_sources = plan.map_or(&[][..], |p| p.sources.as_slice());
        // The link sites to place (ADR-024), while the room has fewer links and link sites than the
        // level allows (the placement's own count, without its scan): the controller link first,
        // then, once it stands or is placed, the source links, farthest source first.
        let spots = u32::try_from(link_spots.len()).unwrap_or(u32::MAX);
        if lcfg.place_links && spots < allowed {
            let has = |p: Position| link_spots.contains(&p);
            let controller_link = plan.and_then(|p| p.controller_link);
            if let Some(c) = controller_link.filter(|&c| !has(c)) {
                r.link_sites.push(Tile::of(c));
            }
            if controller_link.is_some_and(has) || receiver_at.is_some() {
                // The `source_links` sources farthest from the spawn (by trip, then the plan's
                // order), whether their links stand already or not, farthest first: a source's rank
                // is the number of qualifying sources before it.
                let linkable = |s: &MineSource| s.body.is_some() && s.link.is_some();
                let cost = |s: &MineSource| s.trip.map_or(0, |t| t.cost);
                let rank = |j: usize, s: &MineSource| {
                    plan_sources
                        .iter()
                        .enumerate()
                        .filter(|&(k, o)| {
                            linkable(o) && (cost(o) > cost(s) || (cost(o) == cost(s) && k < j))
                        })
                        .count()
                };
                let take = usize::try_from(lcfg.source_links).unwrap_or(usize::MAX);
                for want in 0..take.min(plan_sources.len()) {
                    let next = plan_sources
                        .iter()
                        .enumerate()
                        .find(|&(j, s)| linkable(s) && rank(j, s) == want);
                    if let Some((_, s)) = next
                        && let Some(l) = s.link.filter(|&l| !has(l))
                    {
                        r.link_sites.push(Tile::of(l));
                    }
                }
            }
        }
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
            // Its source link: the built link next to its tile, the planned one first.
            let source_link = s.tile.and_then(|tile| {
                links
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| range(l.pos, tile) <= NEAR)
                    .min_by_key(|(_, l)| (Some(l.pos) != s.link, tile_key(l.pos)))
                    .map(|(i, _)| i)
            });
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
            let store = snap.creeps.stores().get(m.row).copied().unwrap_or_default();
            // Its energy goes to the controller link: it can hold a harvest, it stands next to a
            // source link, and that link is a controller link or an active link that sends to one
            // (an inactive one could never pass it on, V218).
            let link = source_link
                .and_then(|i| links.get(i))
                .filter(|l| lcfg.enabled && store.capacity > 0 && range(at, l.pos) <= NEAR);
            let linked = link.and_then(|l| {
                if receiver(l.pos) {
                    Some(l.pos)
                } else if l.active {
                    receiver_at
                } else {
                    None
                }
            });
            let has_link = linked.is_some();
            if near {
                // Where its harvest lands this tick: its tile, or wherever it stands next to the
                // source on the way there; or the controller link.
                r.mined.push(Mined {
                    source: s.source,
                    tile: at,
                    link: linked,
                });
                stats.mined = stats.mined.saturating_add(1);
                if linked.is_some() {
                    stats.linked = stats.linked.saturating_add(1);
                }
            }
            // Its store into the link on the tick the harvest would not fit (the transfer runs
            // first, V45), when the link has room for all of it at the start of the tick (V216).
            let per_tick = miner_work(sources.iter().find(|x| x.1 == s.source).map_or(0, |x| x.3))
                .saturating_mul(HARVEST_POWER);
            if let Some(i) = source_link
                && has_link
                && store.energy > 0
                && store.energy.saturating_add(per_tick) > store.capacity
                && let Some(l) = links.get_mut(i)
                && LINK_CAPACITY
                    .saturating_sub(l.energy)
                    .saturating_sub(l.incoming)
                    >= store.energy
            {
                let intent = CreepIntent::Transfer {
                    target: Target::Structure(l.handle),
                    resource: ResourceType::Energy,
                    amount: None,
                };
                if buffer.push(snap, m.handle, intent, commit).is_ok() {
                    l.incoming = l.incoming.saturating_add(store.energy);
                    stats.transfers = stats.transfers.saturating_add(1);
                }
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
                if let Some(rec) = r.harvest.iter_mut().find(|x| x.source == s.source) {
                    rec.harvest(per_tick);
                }
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
        // The controller container's site (ADR-025), after the mining tiles', from the level asked
        // for, once a source that sends no energy through a link pays for its miner.
        if controller_container.is_some_and(|l| u32::from(found.level) >= l)
            && let Some(c) = plan.and_then(|p| p.controller_container)
            && !stores.contains(&c)
            && plan_sources
                .iter()
                .any(|s| s.tile.is_some() && s.body.is_some() && !s.linked)
        {
            r.containers.push(Tile::of(c));
        }
        // The sends are the Standard `links` job's (`send_room`): the room is a unit when a
        // source link may send to a controller link this tick.
        r.links.clear();
        r.links.extend(links.iter().copied());
        r.controller = found.controller;
        let mining_tile = |p: Position| {
            plan_sources
                .iter()
                .any(|s| s.tile.is_some_and(|t| range(p, t) <= NEAR))
        };
        let may_send = |l: &RoomLink| {
            l.active
                && l.cooldown == 0
                && l.energy >= lcfg.send_min.max(1)
                && !receiver(l.pos)
                && mining_tile(l.pos)
                && !held.iter().any(|h| h.0 == l.handle)
        };
        if lcfg.enabled && receiver_at.is_some() && links.iter().any(may_send) {
            send_rooms.push(ri);
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

    /// Reads room `name`'s sources (by tile), containers and container sites, links and link sites
    /// and built links into the scratch lists, and returns its first spawn by id, its controller and
    /// its level.
    fn gather(&mut self, snap: &Snapshot, name: RoomName, row: usize) -> Found {
        self.sources.clear();
        self.stores.clear();
        self.link_spots.clear();
        self.links.clear();
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
                ObjectKind::Site(StructureType::Link) => self.link_spots.push(pos),
                _ => {}
            }
        }
        // Sources in a fixed order (rows follow the engine's, V102): by place.
        self.sources.sort_unstable_by_key(|s| tile_key(s.1));
        let st = &snap.structures;
        let mut spawn: Option<(u128, Position)> = None;
        let mut controller = None;
        for ((&h, &kind), &pos) in st.handles().iter().zip(st.kinds()).zip(st.positions()) {
            if pos.room_name() != name {
                continue;
            }
            match kind {
                StructureType::Spawn => {
                    let key = st.key_of(h).map_or(u128::MAX, |k| k.0);
                    if spawn.is_none_or(|(k, _)| key < k) {
                        spawn = Some((key, pos));
                    }
                }
                StructureType::Controller => controller = Some(pos),
                StructureType::Link => self.link_spots.push(pos),
                _ => {}
            }
        }
        let ln = &snap.links;
        for (((&handle, &lrow), &energy), &cooldown) in ln
            .handles()
            .iter()
            .zip(ln.rows())
            .zip(ln.energy())
            .zip(ln.cooldowns())
        {
            let Some(&pos) = usize::try_from(lrow)
                .ok()
                .and_then(|r| st.positions().get(r))
            else {
                continue;
            };
            if pos.room_name() == name {
                self.links.push(RoomLink {
                    handle,
                    row: lrow,
                    pos,
                    energy,
                    cooldown,
                    active: true,
                    incoming: 0,
                });
            }
        }
        let level = match snap.rooms.controllers().get(row) {
            Some(&Controller::Mine { level }) => level,
            _ => 0,
        };
        Found {
            spawn: spawn.map(|(_, pos)| pos),
            controller,
            level,
        }
    }

    /// Reads the tiles of room `name` that a structure, a site or a container takes, with whether it
    /// blocks movement, into `self.occupied`: a road or a rampart does not refuse a site (V205), every
    /// other structure, any site and a container do. Only a plan rebuild needs them.
    fn gather_occupied(&mut self, snap: &Snapshot, name: RoomName) {
        self.occupied.clear();
        let ob = &snap.objects;
        for (&kind, &pos) in ob.kinds().iter().zip(ob.positions()) {
            if pos.room_name() != name {
                continue;
            }
            match kind {
                ObjectKind::Container => self.occupied.push((Tile::of(pos), false)),
                ObjectKind::Site(t) => self.occupied.push((Tile::of(pos), t.is_obstacle())),
                _ => {}
            }
        }
        let st = &snap.structures;
        for (&kind, &pos) in st.kinds().iter().zip(st.positions()) {
            if pos.room_name() == name
                && !matches!(kind, StructureType::Road | StructureType::Rampart)
            {
                self.occupied.push((Tile::of(pos), kind.is_obstacle()));
            }
        }
    }

    /// Marks the links the level keeps active: the engine counts, for each link, the links strictly
    /// nearer the controller (Chebyshev range) and those at the same range met first in its object
    /// order, and keeps it active while that count is below `allowed` (V218). The object order is not
    /// the bot's to see, so a link counts as active only when at most `allowed` links stand at its
    /// range or nearer: links tied at the last allowed range count as inactive.
    fn rank_links(&mut self, controller: Option<Position>, allowed: u32) {
        let at = |l: &RoomLink| controller.map_or(u8::MAX, |c| range(l.pos, c));
        for i in 0..self.links.len() {
            let Some(d) = self.links.get(i).map(at) else {
                continue;
            };
            let level = self.links.iter().filter(|l| at(l) <= d).count();
            if let Some(l) = self.links.get_mut(i) {
                l.active = u32::try_from(level).unwrap_or(u32::MAX) <= allowed;
            }
        }
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
        found: Found,
        movement: &mut Movement,
        lcfg: &LinkConfig,
        mcfg: &MovementConfig,
        refused: &[Tile],
        commit: &mut Commit,
    ) {
        let Some(r) = self.rooms.get(ri) else {
            return;
        };
        let name = r.name;
        let capacity = snap.rooms.energy().get(row).map_or(0, |e| e.capacity);
        let refused_n = u32::try_from(refused.len()).unwrap_or(u32::MAX);
        let Some((_, version)) = movement.room_grid(name, mcfg, commit) else {
            return;
        };
        let near_source = |p: &Position| self.sources.iter().any(|s| range(*p, s.1) <= NEAR);
        let near_controller = |p: &Position| {
            found
                .controller
                .is_some_and(|c| range(*p, c) <= RECEIVER_RANGE)
        };
        let containers = u32::try_from(
            self.stores
                .iter()
                .filter(|p| near_source(p) || near_controller(p))
                .count(),
        )
        .unwrap_or(0);
        let links = u32::try_from(self.link_spots.len()).unwrap_or(u32::MAX);
        let built = u32::try_from(self.links.len()).unwrap_or(u32::MAX);
        let spawn = found.spawn;
        if r.plan.as_ref().is_some_and(|p| {
            p.version == version
                && p.capacity == capacity
                && p.spawn == spawn
                && p.containers == containers
                && p.controller == found.controller
                && p.links == links
                && p.built == built
                && p.network == lcfg.enabled
                && p.level == found.level
                && p.refused == refused_n
                && p.sources.len() == self.sources.len()
                && p.sources
                    .iter()
                    .zip(&self.sources)
                    .all(|(a, b)| a.source == b.1)
        }) {
            return;
        }
        // A rebuild: the sources and minerals, the taken tiles, then the grid (cached since the
        // call above).
        self.statics.clear();
        self.statics.extend(movement.room_statics(name));
        self.gather_occupied(snap, name);
        let profile = crate::movement::profile(mcfg);
        let Some((grid, _)) = movement.room_grid(name, mcfg, commit) else {
            return;
        };
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
        for &(_, pos, _, _) in &self.sources {
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
            sources.push(MineSource {
                source: pos,
                tile,
                trip,
                body: None,
                link: None,
                linked: false,
            });
        }
        // The link tiles (ADR-024), the controller link first.
        let mining: Vec<Tile> = sources
            .iter()
            .filter_map(|s| s.tile.map(Tile::of))
            .collect();
        let mut planned: Vec<Tile> = self.link_spots.iter().map(|&p| Tile::of(p)).collect();
        let controller_link = found.controller.and_then(|c| {
            let existing = self
                .link_spots
                .iter()
                .copied()
                .filter(|&p| range(p, c) <= RECEIVER_RANGE)
                .min_by_key(|&p| (range(p, c), tile_key(p)));
            existing.or_else(|| {
                let ok = |t: Tile| {
                    reached
                        && link_tile_ok(t, grid, profile.road != profile.plain, profile.road)
                        && !refused.contains(&t)
                        && !self.occupied.iter().any(|&(o, _)| o == t)
                        && !mining.contains(&t)
                        && !self.statics.iter().any(|&s| t.range(s) <= NEAR)
                        && self.field.distance(t) != UNREACHED
                        && keeps_neighbours_connected(t, |n| {
                            blocks(n, grid, &self.occupied, &planned)
                        })
                };
                let ct = Tile::of(c);
                let best = (0..rustedscreep_consts::ROOM_AREA)
                    .filter_map(Tile::from_index)
                    .filter(|&t| t.range(ct) == CONTROLLER_LINK_RANGE && ok(t))
                    .min_by_key(|&t| (self.field.distance(t), t.index()));
                best.and_then(|t| place(name, t))
            })
        });
        if let Some(c) = controller_link {
            planned.push(Tile::of(c));
        }
        // The controller container (ADR-025, module docs), off the link tiles planned so far.
        let near_source = |p: Position| self.sources.iter().any(|s| range(p, s.1) <= NEAR);
        let controller_container = found.controller.and_then(|c| {
            let existing = self
                .stores
                .iter()
                .copied()
                .filter(|&p| range(p, c) <= RECEIVER_RANGE && !near_source(p))
                .min_by_key(|&p| (range(p, c), tile_key(p)));
            existing.or_else(|| {
                let ok = |t: Tile| {
                    reached
                        && link_tile_ok(t, grid, profile.road != profile.plain, profile.road)
                        && !refused.contains(&t)
                        && !self.occupied.iter().any(|&(o, _)| o == t)
                        && !mining.contains(&t)
                        && !planned.contains(&t)
                        && !self.statics.iter().any(|&s| t.range(s) <= NEAR)
                        && self.field.distance(t) != UNREACHED
                };
                let ct = Tile::of(c);
                let best = (0..rustedscreep_consts::ROOM_AREA)
                    .filter_map(Tile::from_index)
                    .filter(|&t| t.range(ct) == CONTROLLER_LINK_RANGE && ok(t))
                    .min_by_key(|&t| (self.field.distance(t), t.index()));
                best.and_then(|t| place(name, t))
            })
        });
        let container_tile = controller_container.map(Tile::of);
        let controller_store = controller_container.is_some_and(|c| self.stores.contains(&c));
        for s in &mut sources {
            let Some(tile) = s.tile else {
                continue;
            };
            let existing = self
                .link_spots
                .iter()
                .copied()
                .filter(|&p| range(p, tile) <= NEAR)
                .min_by_key(|&p| tile_key(p));
            let link = existing.or_else(|| {
                let source = Tile::of(s.source);
                let best = Tile::of(tile)
                    .neighbors()
                    .map(|(_, t)| t)
                    .filter(|&t| {
                        reached
                            && link_tile_ok(t, grid, profile.road != profile.plain, profile.road)
                            && !refused.contains(&t)
                            && !self.occupied.iter().any(|&(o, _)| o == t)
                            && !mining.contains(&t)
                            && !planned.contains(&t)
                            && Some(t) != container_tile
                            && self.field.distance(t) != UNREACHED
                            && keeps_neighbours_connected(t, |n| {
                                blocks(n, grid, &self.occupied, &planned)
                            })
                    })
                    // Off the source's other tiles first (workers harvest there while it has no
                    // miner), then away from the spawn's way in.
                    .min_by_key(|&t| {
                        (
                            t.range(source) <= NEAR,
                            std::cmp::Reverse(self.field.distance(t)),
                            t.index(),
                        )
                    });
                best.and_then(|t| place(name, t))
            });
            if let Some(l) = link {
                planned.push(Tile::of(l));
            }
            s.link = link;
        }
        // Bodies: one CARRY part while a built link that can pass the energy on (a controller link,
        // or an active one, V218) stands next to the mining tile.
        let passes = |l: &RoomLink| {
            l.active
                || found
                    .controller
                    .is_some_and(|c| range(l.pos, c) <= RECEIVER_RANGE)
        };
        for (s, &(_, _, _, source_capacity)) in sources.iter_mut().zip(&self.sources) {
            let linked = lcfg.enabled
                && s.tile.is_some_and(|t| {
                    self.links
                        .iter()
                        .any(|l| range(l.pos, t) <= NEAR && passes(l))
                });
            s.linked = linked;
            s.body = s.trip.and_then(|t| {
                if linked {
                    miner_body_with(source_capacity, t, capacity, 1)
                } else {
                    miner_body(source_capacity, t, capacity)
                }
            });
        }
        if let Some(r) = self.rooms.get_mut(ri) {
            r.plan = Some(MinePlan {
                version,
                capacity,
                spawn,
                containers,
                controller: found.controller,
                links,
                built,
                network: lcfg.enabled,
                level: found.level,
                refused: refused_n,
                controller_link,
                controller_container,
                controller_store,
                sources,
            });
            r.tiles.clear();
            if let Some(p) = &r.plan {
                r.tiles.extend(p.sources.iter().filter_map(|s| s.tile));
                r.tiles.extend(p.controller_container);
            }
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

/// Whether `t` may take a link (module docs): inside [`LINK_MARGIN`], walkable, and not a road
/// while roads cost less than plain (a road keeps its tile, as for extensions).
///
/// Uses: [`LINK_MARGIN`], `ROOM_SIZE`.
fn link_tile_ok(t: Tile, grid: &CostGrid, roads: bool, road: u8) -> bool {
    let high = ROOM_SIZE.saturating_sub(1).saturating_sub(LINK_MARGIN);
    let inside = |c: u8| (LINK_MARGIN..=high).contains(&c);
    inside(t.x()) && inside(t.y()) && grid.walkable(t) && !(roads && grid.cost(t) == road)
}

/// Whether `n` blocks a way around a link tile: a wall, a structure or a site that blocks
/// movement, or a link tile already planned.
fn blocks(n: Tile, grid: &CostGrid, occupied: &[(Tile, bool)], planned: &[Tile]) -> bool {
    !grid.walkable(n) || occupied.iter().any(|&(o, b)| o == n && b) || planned.contains(&n)
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
