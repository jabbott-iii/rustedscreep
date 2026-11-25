//! Remote mining's manager and creeps (ROADMAP P6.2, SPEC §3.5, ADR-029): which remote rooms each
//! home mines, the creeps they need, and what those creeps do. A remote room is a room the bot
//! mines without owning it; P6.1's model ([`remote_profit`], [`best_plan`]) prices each one, and
//! the manager spawns the bodies the model counted ([`remote_crew`]).
//!
//! # Candidates and intel
//!
//! A home's candidates are the rooms through its open exits (the home's cost grid knows them,
//! V151), less highway rooms and its sector's center rooms ([`is_remote_room`], V231). The bot sees
//! a room's sources and controller only while one of its creeps is there (V101), and scouting is
//! P8's, so ADR-029 adds a scout: one MOVE part, ordered while a home wants another remote room
//! and has a candidate it knows nothing about, or one last seen more than `remote.rescout_ticks`
//! ago. What a creep in the room sees ([`Intel`]) is kept in the heap: its controller and who
//! holds or reserves it, its sources, its hostile structures (an invader core or a keeper's lair)
//! and its hostile creeps. A room the bot mines stays visible through its own creeps, so a global
//! reset forgets only the rooms it does not mine.
//!
//! # RemoteWays and the choice of rooms
//!
//! Each candidate's ways ([`RemoteWays`], [`WaySearch`]): a search over the home's grid from next to its
//! first spawn to the exits on the candidate's side, then over the candidate's grid from the tiles
//! across them (a creep on an edge tile moves to the mirrored tile of the next room at the end of
//! the tick, V151), to each source's mining tile (its walkable neighbor nearest the home by the
//! search, a tile with a container or a container site first) and to the controller. The way to a
//! mining tile gives its trip bare (the terrain's costs in the remote room) and paved (the road
//! cost there), and its road tiles: the path in the remote room, off the edge and the mining tile,
//! by terrain, and those without a road or a road site yet. The home room's part is walked on its
//! grid as it stands (its roads are the base planner's).
//!
//! [`best_plan`] prices each candidate whose intel allows mining it: a neutral controller not
//! reserved by anyone else, at least one source, no hostile structure, and no hostile creep when
//! last seen. A home mines the candidates that net the most, at most `remote.max_rooms`, while the
//! model's spawn time of all of them stays within `remote.spawn_permille` of each of the home's
//! spawns. When a mined room is threatened, contested or unprofitable, the next section applies.
//!
//! # Creeps
//!
//! Their names carry the remote room (`spawning::CreepName`, ADR-029), so a reset loses only the
//! source each one works, which is chosen again: a miner on a mining tile takes it, the others the
//! source with the fewest of their role for what it wants.
//!
//! - **Scout:** walks into its remote room, then into each other candidate of its home the bot
//!   knows nothing about, nearest first. With nothing left to see, and every room it can see
//!   priced, it calls `suicide` (one billed intent, against the snapshot's reads of a room it
//!   would hold for the rest of its life).
//! - **Reserver:** walks next to the controller and reserves it while the reservation can take
//!   its work: a reserve that would take `ticksToEnd` past `CONTROLLER_RESERVE_MAX` is billed and
//!   dropped (V263, V264), so it is not sent. The next one is ordered when the reservation would
//!   otherwise run out before it arrives (its spawn time and walk, `spawn.replace_margin_ticks`).
//! - **Remote miner:** stands on its source's mining tile. It builds the container site there
//!   from its one CARRY part's energy, repairs the container below `remote.container_repair_pct`,
//!   and harvests otherwise (a `build` or `repair` blocks the tick's `harvest`, V44). Its successor
//!   (P5.2) waits next to the tile until it is free.
//! - **Remote hauler:** gathers at its source's mining tile (the pile there, then the container,
//!   a whole load at a time), then walks home. In the home room the home's hauling drives it while
//!   it holds energy (`economy::Hauling`, ADR-029's "home sinks"). With a WORK part, on the way
//!   home through the remote room, it builds a road site within range 3, else repairs the most
//!   worn road within range 3 that lacks a whole repair (`REPAIR_POWER`), on the ticks the
//!   snapshot reads the roads (`remote.road_read_ticks`).
//!
//! The Optional placement places, one site a tick a room, the container sites on the mining tiles,
//! then, under a plan with roads, the road sites along each source's way, nearest the source first,
//! at most `remote.max_sites` own sites a room.
//!
//! # Threats, contests and audits (P6.3)
//!
//! - **Threatened:** hostile creeps with an active ATTACK or RANGED_ATTACK part (the snapshot's
//!   `Combat`, V269) seen in a mined room start a [`Threat`] of `remote.evacuate_ticks`, extended
//!   from each later sight. Its creeps (not its scouts) walk home and wait near the first spawn,
//!   its haulers' loads are delivered there by the home's hauling, and no creep is requested for
//!   it. When the wait runs out they go back; armed hostiles seen again start a new threat. A
//!   hostile with no such part (another player's scout or hauler) changes nothing. P7's defense
//!   can start or end a wait itself ([`Remotes::evacuate`], [`Remotes::clear_threat`]): since P7.6
//!   (ADR-037) a home may send remote defenders (`defense::remote`), and the wait ends as soon as a
//!   creep sees the room with no armed hostile; a threat ended so does not count toward
//!   `remote.contest_threats`.
//! - **Contested:** a mined room seen with another player's controller, another user's
//!   reservation (an invader core's among them, V273), or a hostile structure (V267) is abandoned
//!   ([`Intel::contested`]). A harvest or a reserve there fails (V182, V263, V271), so its miners
//!   and reserver call `suicide` (one billed intent, against the bot's work for them each tick of
//!   their remaining lives), and its haulers bring their loads home first. The `remote.contest_threats`-th
//!   threat within `remote.abandon_ticks` abandons a room the same way.
//! - **Unprofitable:** each mined room keeps an [`Audit`] over windows of `remote.audit_ticks`:
//!   the energy its haulers carry into the home room (counted on the tick one crosses in) against
//!   its new creeps' bodies (its crew's body for the role). A window that delivered less abandons
//!   the room, except its first window in a global, which holds its start; its creeps work on
//!   until they die, since their bodies are paid for.
//!
//! An abandoned room is not chosen again for `remote.abandon_ticks`; then, unseen since, it is
//! scouted again first. Each start, end and abandon is a [`RemoteEvent`] the glue logs. All of it
//! lives in the heap: a reset forgets it, and the room's own creeps bring its intel back.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Remotes::begin`] after the snapshot: the owned homes, the intel of each remote room read
//!    this tick, and each home's remote creeps (by remote room, then id).
//! 2. The Standard `remote` job runs [`Remotes::plan_home`] per home, before the spawn job: its
//!    candidates, prices, chosen rooms, the creeps' sources, and the spawn requests.
//! 3. The Standard `remote work` job runs [`Remotes::work_creep`] per remote creep: its intents and
//!    moves (remote mining is spare work, so its harvests are not Critical, SPEC §2's bucket
//!    policy).
//! 4. The Optional `remote placement` job runs [`Remotes::place_home`] per home: one way search a
//!    step (a heavy unit), then the sites ([`Remotes::sites`]), which the act step places.
//!
//! Nothing is read from or written to `Memory`.

use core::cmp::Reverse;
use std::collections::BinaryHeap;

use rustedscreep_consts::{
    BUILD_POWER, CONTAINER_HITS, CONTROLLER_RESERVE, CONTROLLER_RESERVE_MAX, INTENT_CPU_COST, Part,
    Position, REPAIR_POWER, ROOM_AREA, ResourceType, RoomCoordinate, RoomName, StructureType,
    extension_energy_capacity,
};

use super::placement::APPEAR_TICKS;
use super::remote::{
    RemoteCrew, RemoteHome, RemoteProfit, RemoteRoom, RemoteSource, RemoteWay, RoadTiles,
    best_plan, remote_crew,
};
use super::sizing::Trip;
use crate::config::{HaulingConfig, MovementConfig, RemoteConfig, SpawnConfig};
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{
    CostGrid, CostProfile, Footprint, MoveTo, Movement, Side, TerrainGrid, TerrainKind, Tile,
    profile,
};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{Controller, CreepHandle, ObjectHandle, ObjectKind, Reservation, Snapshot};
use crate::spawning::{
    Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner, is_due, lead, room_distance,
};

/// Spawn priorities of the remote creeps: below every request of the home's own (the opening's
/// normal workers are `PRIORITY_NORMAL`, 100), so a home's economy never waits for its remote
/// rooms. The miner first (its source idles without it), then the haulers, the reserver and the
/// scout.
pub const PRIORITY_REMOTE_MINER: u8 = 90;
pub const PRIORITY_REMOTE_HAULER: u8 = 85;
pub const PRIORITY_RESERVER: u8 = 80;
pub const PRIORITY_SCOUT: u8 = 75;

/// Harvesting, taking and reserving reach 1 tile (V182, V190, V192, V263).
const NEAR: u8 = 1;

/// Building and repairing reach 3 tiles (V185, V222).
const WORK_RANGE: u8 = 3;

/// The room's center, and the range from it that holds every tile off the two outer rings: a move
/// "into the room" arrives one step past the edge.
const CENTER: u8 = 25;
const INSIDE: u8 = 23;

/// How near the home's first spawn an evacuated creep waits (P6.3): in the home room, off the
/// spawn's own ring. Unit: tiles.
const EVACUATE_RANGE: u8 = 5;

/// How hard a remote miner's moves push past other creeps (as a home miner's, `economy::mining`).
const MINER_PRIORITY: u8 = 4;

/// How hard the other remote creeps' moves push.
const MOVE_PRIORITY: u8 = 2;

/// No parent in a [`WaySearch`]: a seed.
const NO_PARENT: u16 = u16::MAX;

/// Whether `room` can be a remote room by its coordinates: not a highway room (a coordinate
/// divisible by 10, which has no controller) and not in its sector's center 3 × 3 rooms (the
/// center and the source keeper rooms around it). Both rules are V231's (inferred: the docs'
/// `findRoute` example and the wiki; the corpus's sector matched them). A room that passes but has
/// no controller or a keeper's lair is left out by its intel.
pub fn is_remote_room(room: RoomName) -> bool {
    // The name's numbers: W n is x = -n - 1, E n is x = n (V103).
    let number = |c: i32| if c < 0 { -c - 1 } else { c };
    let (x, y) = (number(room.x_coord()) % 10, number(room.y_coord()) % 10);
    let highway = x == 0 || y == 0;
    let center = (4..=6).contains(&x) && (4..=6).contains(&y);
    !highway && !center
}

/// The rooms across `home`'s sides that have an open exit on its grid, and that pass
/// [`is_remote_room`], in [`Side::ALL`]'s order.
pub fn candidates(home: RoomName, grid: &CostGrid) -> Vec<(Side, RoomName)> {
    let mut open = [false; 4];
    for i in 0..ROOM_AREA {
        if let Some(t) = Tile::from_index(i)
            && grid.open_exit(t)
            && let Some(side) = t.side()
            && let Some(slot) = Side::ALL
                .iter()
                .position(|&s| s == side)
                .and_then(|k| open.get_mut(k))
        {
            *slot = true;
        }
    }
    Side::ALL
        .iter()
        .zip(open)
        .filter(|&(_, o)| o)
        .filter_map(|(&side, _)| {
            home.checked_add(side.room_offset())
                .filter(|&r| is_remote_room(r))
                .map(|r| (side, r))
        })
        .collect()
}

/// What the bot last saw of a candidate room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intel {
    /// The tick it was seen. Unit: game ticks.
    pub seen: u32,
    pub controller: Controller,
    pub controller_tile: Option<Tile>,
    pub reservation: Reservation,
    /// Its sources, by tile.
    pub sources: Vec<Tile>,
    pub hostile_structures: u32,
    /// Hostile creeps in it (other players', NPC invaders, source keepers; V101). Unit: creeps.
    pub hostiles: u32,
    /// Of them, those with an active ATTACK or RANGED_ATTACK part (P6.3, V269). Unit: creeps.
    pub armed: u32,
}

impl Intel {
    /// Whether someone else holds the room (P6.3): another player owns its controller, another user
    /// reserves it (a player, or an invader core, V273), or a hostile structure stands there (V267).
    /// A harvest there fails (V182, V271), so a mined room that is contested is abandoned.
    pub fn contested(&self) -> bool {
        matches!(self.controller, Controller::Foreign { .. })
            || matches!(self.reservation, Reservation::Other { .. })
            || self.hostile_structures > 0
    }

    /// Whether the room may be chosen: a neutral controller, at least one source, not contested,
    /// and no hostile creep when it was seen.
    pub fn minable(&self) -> bool {
        matches!(self.controller, Controller::Neutral)
            && self.controller_tile.is_some()
            && !self.contested()
            && !self.sources.is_empty()
            && self.hostiles == 0
    }
}

/// A threat to a mined room (P6.3): armed hostile creeps seen there, or an evacuation P7 asked for
/// ([`Remotes::evacuate`]). Its creeps wait in the home until `until`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Threat {
    /// The tick it started. Unit: game ticks.
    pub since: u32,
    /// The tick its creeps go back. Unit: game ticks.
    pub until: u32,
    /// Armed hostile creeps when last seen. Unit: creeps.
    pub armed: u32,
}

/// Why a room was abandoned (P6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbandonReason {
    /// Another user holds it ([`Intel::contested`]).
    Contested,
    /// It was threatened `remote.contest_threats` times within `remote.abandon_ticks`.
    Threats,
    /// Its haulers brought home less energy than its creeps' bodies cost over a
    /// `remote.audit_ticks` window.
    Unprofitable,
}

impl AbandonReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contested => "contested",
            Self::Threats => "threatened too often",
            Self::Unprofitable => "unprofitable",
        }
    }
}

/// An abandoned room: not chosen again until `until`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abandon {
    pub since: u32,
    pub until: u32,
    pub reason: AbandonReason,
}

/// A mined room's energy balance over the current `remote.audit_ticks` window (P6.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Audit {
    /// The window's first tick; `None` until the room is first mined in this global.
    pub start: Option<u32>,
    /// Windows completed in this global (the first is not judged: it holds the room's start).
    pub windows: u32,
    /// Energy its haulers carried into the home room this window. Unit: energy.
    pub delivered: u64,
    /// Its new creeps' bodies this window. Unit: energy.
    pub spent: u64,
    /// The room was threatened during the window (it is then not judged).
    pub threatened: bool,
}

/// Something the manager did this tick that the glue logs (P6.3; the P6 gate's live evidence).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteEvent {
    /// Armed hostiles in a mined room: its creeps go home until `until`.
    Threatened {
        home: RoomName,
        room: RoomName,
        armed: u32,
        until: u32,
    },
    /// The threat's time ran out: its creeps go back.
    Resumed { home: RoomName, room: RoomName },
    Abandoned {
        home: RoomName,
        room: RoomName,
        reason: AbandonReason,
        until: u32,
    },
}

/// One source's way from the home ([`WaySearch::remote`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceWay {
    pub source: Tile,
    /// Its mining tile, where its miner stands and its container goes.
    pub tile: Tile,
    pub way: RemoteWay,
    /// The path's road tiles in the remote room (off the edge and the mining tile), by terrain,
    /// and of them those without a road or a road site.
    pub road: RoadTiles,
    pub road_new: RoadTiles,
    /// Those tiles, from the mining tile's neighbor back to the room's edge.
    pub path: Vec<Tile>,
}

/// A candidate's ways from its home, for one pair of grid versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteWays {
    /// The home's and the remote room's grid versions it was searched on.
    pub versions: (u32, u32),
    /// The home spawn's tile it starts next to.
    pub spawn: Tile,
    pub sources: Vec<SourceWay>,
    /// The tile next to the controller the reserver stands on, and its way.
    pub controller: Option<(Tile, RemoteWay)>,
}

/// What [`WaySearch::remote`] finds: each source's way, and the controller's (the tile next to it
/// and the way there).
pub type Found = (Vec<SourceWay>, Option<(Tile, RemoteWay)>);

/// Scratch for [`WaySearch::home`] and [`WaySearch::remote`]: a Dijkstra search by (cost, steps,
/// tile) over one room's grid, kept for its buffers.
#[derive(Default)]
pub struct WaySearch {
    dist: Vec<u32>,
    steps: Vec<u32>,
    parent: Vec<u16>,
    heap: BinaryHeap<Reverse<(u32, u32, u16)>>,
    /// The home room's exits reached: tile, cost and steps to it.
    exits: Vec<(Tile, u32, u32)>,
}

impl WaySearch {
    fn reset(&mut self) {
        self.dist.clear();
        self.dist.resize(ROOM_AREA, u32::MAX);
        self.steps.clear();
        self.steps.resize(ROOM_AREA, u32::MAX);
        self.parent.clear();
        self.parent.resize(ROOM_AREA, NO_PARENT);
        self.heap.clear();
    }

    fn at(&self, t: Tile) -> (u32, u32) {
        (
            self.dist.get(t.index()).copied().unwrap_or(u32::MAX),
            self.steps.get(t.index()).copied().unwrap_or(u32::MAX),
        )
    }

    fn relax(&mut self, t: Tile, cost: u32, steps: u32, parent: u16, push: bool) {
        if (cost, steps) >= self.at(t) {
            return;
        }
        let i = t.index();
        if let (Some(d), Some(s), Some(p)) = (
            self.dist.get_mut(i),
            self.steps.get_mut(i),
            self.parent.get_mut(i),
        ) {
            *d = cost;
            *s = steps;
            *p = parent;
        }
        if push {
            self.heap.push(Reverse((
                cost,
                steps,
                u16::try_from(i).unwrap_or(NO_PARENT),
            )));
        }
    }

    /// The home room's part: from the walkable neighbors of `spawn` (where a new creep appears) to
    /// every open exit on `side`, each with the cost of stepping onto it (the grid's exit cost) and
    /// the steps. Returns how many exits it reached.
    pub fn home(&mut self, grid: &CostGrid, spawn: Tile, side: Side) -> usize {
        self.reset();
        self.exits.clear();
        for (_, n) in spawn.neighbors() {
            if grid.walkable(n) {
                self.relax(n, 0, 0, NO_PARENT, true);
            }
        }
        while let Some(Reverse((d, st, i))) = self.heap.pop() {
            let Some(t) = Tile::from_index(usize::from(i)) else {
                continue;
            };
            if (d, st) > self.at(t) {
                continue;
            }
            for (_, v) in t.neighbors() {
                if grid.open_exit(v) {
                    if v.side() == Some(side) {
                        let c = d.saturating_add(u32::from(grid.exit_cost()));
                        self.relax(v, c, st.saturating_add(1), i, false);
                    }
                    continue;
                }
                if grid.walkable(v) {
                    let c = d.saturating_add(u32::from(grid.cost(v)));
                    self.relax(v, c, st.saturating_add(1), i, true);
                }
            }
        }
        let mut exits = std::mem::take(&mut self.exits);
        for i in 0..ROOM_AREA {
            if let Some(t) = Tile::from_index(i)
                && t.side() == Some(side)
                && grid.open_exit(t)
            {
                let (d, s) = self.at(t);
                if d != u32::MAX {
                    exits.push((t, d, s));
                }
            }
        }
        let n = exits.len();
        self.exits = exits;
        n
    }

    /// The remote room's part, from the tiles across the exits [`Self::home`] reached: each
    /// source's mining tile (a tile of `stores` first, never the tile another source took), its
    /// way bare and paved, and its road tiles (`paved` tiles hold a road or a road site); and the
    /// controller's. `None` when a source has no reachable tile, or no home exit was reached.
    ///
    /// Uses: the cost profile (`MOVE_COST_*` by default, V54) for the bare and paved costs.
    #[allow(clippy::too_many_arguments)]
    pub fn remote(
        &mut self,
        grid: &CostGrid,
        terrain: &TerrainGrid,
        costs: CostProfile,
        sources: &[Tile],
        controller: Option<Tile>,
        stores: &[Tile],
        paved: &[Tile],
    ) -> Option<Found> {
        let seeds = std::mem::take(&mut self.exits);
        self.reset();
        for &(e, c, s) in &seeds {
            if let Some(a) = e.across()
                && grid.open_exit(a)
            {
                self.relax(a, c, s, NO_PARENT, true);
            }
        }
        self.exits = seeds;
        if self.heap.is_empty() {
            return None;
        }
        while let Some(Reverse((d, st, i))) = self.heap.pop() {
            let Some(t) = Tile::from_index(usize::from(i)) else {
                continue;
            };
            if (d, st) > self.at(t) {
                continue;
            }
            for (_, v) in t.neighbors() {
                // A creep that ends a tick on an edge tile leaves the room (V151).
                if v.is_edge() || !grid.walkable(v) {
                    continue;
                }
                let c = d.saturating_add(u32::from(grid.cost(v)));
                self.relax(v, c, st.saturating_add(1), i, true);
            }
        }
        let terrain_cost = |t: Tile| match terrain.kind(t) {
            TerrainKind::Plain => u32::from(costs.plain),
            TerrainKind::Swamp => u32::from(costs.swamp),
            // A wall tile is walkable only with a road on it (a tunnel, V154).
            TerrainKind::Wall => u32::from(costs.road),
        };
        let mut ways: Vec<SourceWay> = Vec::with_capacity(sources.len());
        for &source in sources {
            let tile = source
                .neighbors()
                .map(|(_, n)| n)
                .filter(|n| !n.is_edge() && grid.walkable(*n))
                .filter(|n| !ways.iter().any(|w| w.tile == *n))
                .filter(|n| self.at(*n).0 != u32::MAX)
                .min_by_key(|n| {
                    let (d, s) = self.at(*n);
                    (!stores.contains(n), d, s, n.index())
                })?;
            let (root, path) = self.path(tile);
            let (home_cost, _) = self.at(root);
            let mut road = RoadTiles::NONE;
            let mut road_new = RoadTiles::NONE;
            let (mut bare, mut on_road) = (home_cost, home_cost);
            for &t in &path {
                let c = terrain_cost(t);
                bare = bare.saturating_add(c);
                on_road = on_road.saturating_add(u32::from(costs.road));
                let count = |r: &mut RoadTiles| match terrain.kind(t) {
                    TerrainKind::Plain => r.plain = r.plain.saturating_add(1),
                    TerrainKind::Swamp => r.swamp = r.swamp.saturating_add(1),
                    TerrainKind::Wall => r.wall = r.wall.saturating_add(1),
                };
                count(&mut road);
                if !paved.contains(&t) {
                    count(&mut road_new);
                }
            }
            let last = terrain_cost(tile);
            let tiles = self.at(tile).1;
            ways.push(SourceWay {
                source,
                tile,
                way: RemoteWay {
                    bare: Trip {
                        tiles,
                        cost: bare.saturating_add(last),
                    },
                    paved: Trip {
                        tiles,
                        cost: on_road.saturating_add(last),
                    },
                },
                road,
                road_new,
                path,
            });
        }
        let controller = controller.and_then(|c| {
            let tile = c
                .neighbors()
                .map(|(_, n)| n)
                .filter(|n| !n.is_edge() && grid.walkable(*n))
                .filter(|n| self.at(*n).0 != u32::MAX)
                .min_by_key(|n| {
                    let (d, s) = self.at(*n);
                    (d, s, n.index())
                })?;
            let (root, path) = self.path(tile);
            let (home_cost, _) = self.at(root);
            let (mut bare, mut on_road) = (home_cost, home_cost);
            let roads = |t: &Tile| ways.iter().any(|w| w.path.contains(t));
            for t in path.iter().chain(core::iter::once(&tile)) {
                let c = terrain_cost(*t);
                bare = bare.saturating_add(c);
                on_road = on_road.saturating_add(if roads(t) { u32::from(costs.road) } else { c });
            }
            let tiles = self.at(tile).1;
            Some((
                tile,
                RemoteWay {
                    bare: Trip { tiles, cost: bare },
                    paved: Trip {
                        tiles,
                        cost: on_road,
                    },
                },
            ))
        });
        Some((ways, controller))
    }

    /// The seed (an edge tile) `to`'s path starts from, and the path's tiles from `to`'s parent
    /// back to the tile after the seed.
    fn path(&self, to: Tile) -> (Tile, Vec<Tile>) {
        let mut out = Vec::new();
        let mut at = to;
        // At most one visit a tile: a parent chain is a tree.
        for _ in 0..ROOM_AREA {
            let p = self.parent.get(at.index()).copied().unwrap_or(NO_PARENT);
            let Some(prev) = Tile::from_index(usize::from(p)).filter(|_| p != NO_PARENT) else {
                break;
            };
            if prev.is_edge() {
                return (prev, out);
            }
            out.push(prev);
            at = prev;
        }
        (at, out)
    }
}

/// A site the placement wants this tick ([`Remotes::sites`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteSite {
    pub room: RoomName,
    pub tile: Tile,
    pub kind: StructureType,
}

/// One remote room's counters for the console, from its home's last run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteStats {
    pub scouts: u32,
    pub reservers: u32,
    pub miners: u32,
    pub haulers: u32,
    /// Spawn requests made this tick.
    pub requested: u32,
    /// The reservation when last seen.
    pub reservation: Reservation,
    /// Hostile creeps in it this tick (0 when it is not visible).
    pub hostiles: u32,
}

/// One candidate of a home.
#[derive(Clone, Debug)]
pub struct Remote {
    pub room: RoomName,
    pub side: Option<Side>,
    pub ways: Option<RemoteWays>,
    /// The home's and the room's grid versions its last way search ran on, whatever it found (a
    /// search that finds no way is not run again until a grid changes).
    pub searched: Option<(u32, u32)>,
    /// The plan and figures it is priced at, and the creeps it needs, with the capacity and the
    /// ways' versions they were priced for.
    pub profit: Option<RemoteProfit>,
    pub crew: Option<RemoteCrew>,
    priced: Option<(u32, (u32, u32))>,
    /// The home mines it (chosen, or named by a live creep of the home).
    pub active: bool,
    pub stats: RemoteStats,
    /// Its creeps wait in the home (P6.3).
    pub threat: Option<Threat>,
    /// It is not chosen again until then (P6.3).
    pub abandoned: Option<Abandon>,
    /// The ticks its threats started within the last `remote.abandon_ticks`.
    threats_at: Vec<u32>,
    pub audit: Audit,
    /// The tick it was last abandoned: it is chosen again only on intel seen after it.
    pub left_at: Option<u32>,
}

struct Home {
    name: RoomName,
    level: u8,
    capacity: u32,
    spawns: u32,
    spawn: Option<Tile>,
    /// The home grid's version the candidates come from.
    version: u32,
    remotes: Vec<Remote>,
    /// Members of [`Remotes::members`] of this home this tick.
    members: (usize, usize),
}

/// One remote creep of this tick.
#[derive(Clone, Copy, Debug)]
struct Member {
    home: usize,
    remote: RoomName,
    role: Role,
    key: u128,
    row: usize,
    handle: CreepHandle,
}

/// A remote creep's state between ticks, in the slot of its handle.
#[derive(Clone, Copy, Debug)]
struct Worker {
    handle: CreepHandle,
    /// The source it works, by tile in its remote room.
    source: Option<Tile>,
    /// A hauler walking home with its load.
    returning: bool,
    /// The room it stood in last tick (a hauler that crosses into its home room with energy
    /// delivers it, for the audit).
    last: Option<RoomName>,
    /// Its body is in its room's audit (or it was there before this global's first audit).
    counted: bool,
    /// Its load is in its room's audit (cleared once it is empty).
    delivered: bool,
}

/// What a remote miner or hauler needs of its source's way and crew this tick.
#[derive(Clone, Copy, Debug)]
struct Post {
    source: Tile,
    tile: Tile,
    miner_work: u32,
    hauler_work: u32,
}

/// Remote mining's state for one global.
#[derive(Default)]
pub struct Remotes {
    time: u32,
    homes: Vec<Home>,
    members: Vec<Member>,
    /// What the bot knows of each room it has seen, by name.
    intel: Vec<(RoomName, Intel)>,
    /// Indexed by creep handle slot.
    workers: Vec<Option<Worker>>,
    search: WaySearch,
    sites: Vec<RemoteSite>,
    /// Tiles whose site the game refused, skipped for the rest of the global.
    refused: Vec<(RoomName, Tile)>,
    /// Tiles whose site the game accepted, not offered again until the tick given.
    placed: Vec<(RoomName, Tile, u32)>,
    /// Scratch: objects claimed this tick (handle, energy).
    claimed: Vec<(ObjectHandle, u32)>,
    /// This tick's events, for the glue's log lines (P6.3).
    events: Vec<RemoteEvent>,
}

/// `t` in `room` as a position.
fn at(room: RoomName, t: Tile) -> Option<Position> {
    Some(Position::new(
        RoomCoordinate::new(t.x()).ok()?,
        RoomCoordinate::new(t.y()).ok()?,
        room,
    ))
}

impl Remotes {
    /// Starts a tick: the owned homes, the intel of the remote rooms read this tick, and the
    /// homes' remote creeps.
    pub fn begin(&mut self, snap: &Snapshot) {
        self.time = snap.tick.time;
        self.sites.clear();
        self.claimed.clear();
        self.events.clear();
        let rooms = &snap.rooms;
        let mut keep: Vec<RoomName> = Vec::new();
        for row in rooms.owned() {
            let (Some(&name), Some(&Controller::Mine { level })) =
                (rooms.names().get(row), rooms.controllers().get(row))
            else {
                continue;
            };
            keep.push(name);
            let capacity = rooms.energy().get(row).map_or(0, |e| e.capacity);
            let (spawns, spawn) = home_spawns(snap, name);
            let i = match self
                .homes
                .binary_search_by_key(&name.packed_repr(), |h| h.name.packed_repr())
            {
                Ok(i) => i,
                Err(i) => {
                    self.homes.insert(
                        i,
                        Home {
                            name,
                            level,
                            capacity,
                            spawns,
                            spawn,
                            version: 0,
                            remotes: Vec::new(),
                            members: (0, 0),
                        },
                    );
                    i
                }
            };
            if let Some(h) = self.homes.get_mut(i) {
                h.level = level;
                h.capacity = capacity;
                h.spawns = spawns;
                h.spawn = spawn;
            }
        }
        self.homes.retain(|h| keep.contains(&h.name));

        // Intel from the remote rooms read this tick.
        for (row, read) in rooms.remote_reads().iter().enumerate() {
            if !read.read {
                continue;
            }
            let (Some(&name), Some(&controller)) =
                (rooms.names().get(row), rooms.controllers().get(row))
            else {
                continue;
            };
            let objects = &snap.objects;
            let in_room = |kind: ObjectKind| {
                objects
                    .kinds()
                    .iter()
                    .zip(objects.positions())
                    .filter(move |(k, p)| **k == kind && p.room_name() == name)
                    .map(|(_, &p)| Tile::of(p))
            };
            let creeps = &snap.creeps;
            let (mut hostiles, mut armed) = (0u32, 0u32);
            for ((&my, p), c) in creeps
                .my()
                .iter()
                .zip(creeps.positions())
                .zip(creeps.combat())
            {
                if !my && p.room_name() == name {
                    hostiles = hostiles.saturating_add(1);
                    if c.armed() {
                        armed = armed.saturating_add(1);
                    }
                }
            }
            let at = match self
                .intel
                .binary_search_by_key(&name.packed_repr(), |e| e.0.packed_repr())
            {
                Ok(i) => i,
                Err(i) => {
                    self.intel.insert(
                        i,
                        (
                            name,
                            Intel {
                                seen: 0,
                                controller,
                                controller_tile: None,
                                reservation: Reservation::None,
                                sources: Vec::new(),
                                hostile_structures: 0,
                                hostiles: 0,
                                armed: 0,
                            },
                        ),
                    );
                    i
                }
            };
            // Updated in place: its source list keeps its buffer.
            if let Some((_, intel)) = self.intel.get_mut(at) {
                intel.seen = self.time;
                intel.controller = controller;
                intel.controller_tile = in_room(ObjectKind::Controller).next();
                intel.reservation = read.reservation;
                intel.sources.clear();
                intel.sources.extend(in_room(ObjectKind::Source));
                intel.sources.sort_unstable();
                intel.hostile_structures = read.hostile_structures;
                intel.hostiles = hostiles;
                intel.armed = armed;
            }
        }

        // The homes' remote creeps, by home, then remote room, then id.
        self.members.clear();
        let creeps = &snap.creeps;
        for (row, identity) in creeps.identities().iter().enumerate() {
            // The remote defenders (P7.6) carry their remote room too, but defense drives them.
            let Some(id) =
                identity.filter(|id| id.role.is_remote() && id.role != Role::RemoteDefender)
            else {
                continue;
            };
            let Some(remote) = id.remote else {
                continue;
            };
            if !creeps.my().get(row).copied().unwrap_or(false) {
                continue;
            }
            let Ok(home) = self
                .homes
                .binary_search_by_key(&id.home.packed_repr(), |h| h.name.packed_repr())
            else {
                continue;
            };
            let Some(&handle) = creeps.handles().get(row) else {
                continue;
            };
            let key = creeps.key_of(handle).map_or(u128::MAX, |k| k.0);
            let order = (home, remote.packed_repr(), key);
            let at = self
                .members
                .partition_point(|m| (m.home, m.remote.packed_repr(), m.key) < order);
            self.members.insert(
                at,
                Member {
                    home,
                    remote,
                    role: id.role,
                    key,
                    row,
                    handle,
                },
            );
        }
        let mut start = 0;
        for (i, h) in self.homes.iter_mut().enumerate() {
            let end = start
                + self
                    .members
                    .get(start..)
                    .map_or(0, |rest| rest.iter().take_while(|m| m.home == i).count());
            h.members = (start, end);
            start = end;
        }
        // Forget the state of creeps no longer here.
        for slot in &mut self.workers {
            if let Some(w) = slot
                && creeps.row_of(w.handle).is_none()
            {
                *slot = None;
            }
        }
    }

    pub fn home_count(&self) -> u32 {
        u32::try_from(self.homes.len()).unwrap_or(u32::MAX)
    }

    /// The remote creeps this tick: the units of [`Self::work_creep`].
    pub fn creep_count(&self) -> u32 {
        u32::try_from(self.members.len()).unwrap_or(u32::MAX)
    }

    /// The home `unit`'s name.
    pub fn home_name(&self, unit: u32) -> Option<RoomName> {
        self.homes.get(usize::try_from(unit).ok()?).map(|h| h.name)
    }

    /// What the bot knows of `room`.
    pub fn intel(&self, room: RoomName) -> Option<&Intel> {
        self.intel
            .binary_search_by_key(&room.packed_repr(), |e| e.0.packed_repr())
            .ok()
            .and_then(|i| self.intel.get(i))
            .map(|e| &e.1)
    }

    /// Each home and its candidates, by name, for the console.
    pub fn home_states(&self) -> impl Iterator<Item = (RoomName, &[Remote])> {
        self.homes.iter().map(|h| (h.name, &h.remotes[..]))
    }

    /// The sites the placement picked this tick, for the act step.
    pub fn sites(&self) -> &[RemoteSite] {
        &self.sites
    }

    /// Records that the game refused `site`: its tile is skipped for the rest of the global.
    pub fn site_refused(&mut self, site: RemoteSite) {
        if !self.refused.contains(&(site.room, site.tile)) {
            self.refused.push((site.room, site.tile));
        }
    }

    /// This tick's events (P6.3), for the glue's log lines.
    pub fn events(&self) -> &[RemoteEvent] {
        &self.events
    }

    /// Every threatened room (P6.3, P7's hook): its home, its name and the threat.
    pub fn threats(&self) -> impl Iterator<Item = (RoomName, RoomName, Threat)> + '_ {
        self.homes.iter().flat_map(|h| {
            h.remotes
                .iter()
                .filter_map(move |r| r.threat.map(|t| (h.name, r.room, t)))
        })
    }

    /// Evacuates `room` until `until` (P7's hook: its creeps wait in the home, and no replacement is
    /// ordered), or extends its threat to then. Returns whether some home mines it.
    pub fn evacuate(&mut self, room: RoomName, until: u32) -> bool {
        let now = self.time;
        let mut found = false;
        for r in self
            .homes
            .iter_mut()
            .flat_map(|h| h.remotes.iter_mut())
            .filter(|r| r.room == room)
        {
            found = true;
            match r.threat.as_mut() {
                Some(t) => t.until = t.until.max(until),
                None => {
                    r.threat = Some(Threat {
                        since: now,
                        until,
                        armed: 0,
                    });
                }
            }
        }
        found
    }

    /// Ends `room`'s threat now (P7's hook, once a creep sees the room with no armed hostile,
    /// `defense::remote`): its creeps go back, and the threat no longer counts toward
    /// `remote.contest_threats` (the room was held). Returns whether it had one.
    pub fn clear_threat(&mut self, room: RoomName) -> bool {
        let mut cleared = false;
        for r in self
            .homes
            .iter_mut()
            .flat_map(|h| h.remotes.iter_mut())
            .filter(|r| r.room == room)
        {
            if let Some(t) = r.threat.take() {
                cleared = true;
                if let Some(i) = r.threats_at.iter().rposition(|&at| at == t.since) {
                    r.threats_at.remove(i);
                }
            }
        }
        cleared
    }

    /// Records that the game accepted `site`: its tile is not offered again for
    /// `placement::APPEAR_TICKS` (an accepted site shows on the next tick, V204, unless the
    /// processor drops it).
    pub fn site_placed(&mut self, site: RemoteSite) {
        let until = self.time.saturating_add(APPEAR_TICKS);
        self.placed.retain(|p| p.2 > self.time);
        self.placed.push((site.room, site.tile, until));
    }

    /// The energy the remote rooms of `home` bring it in a creep life, for its upgraders' budget
    /// (P3.4): each mined room's net under its plan ([`RemoteProfit::net`]; the bodies, the repairs
    /// and the builds already taken out), while one of its haulers is alive. Unit: energy.
    pub fn delivered(&self, home: RoomName) -> u64 {
        let Ok(i) = self
            .homes
            .binary_search_by_key(&home.packed_repr(), |h| h.name.packed_repr())
        else {
            return 0;
        };
        self.homes.get(i).map_or(0, |h| {
            h.remotes
                .iter()
                .filter(|r| r.active && r.threat.is_none() && r.stats.haulers > 0)
                .filter_map(|r| r.profit)
                .map(|p| u64::try_from(p.net).unwrap_or(0))
                .fold(0u64, u64::saturating_add)
        })
    }

    /// Home `unit`'s run in the `remote` job (module docs, step 2). `hauled`: the home's hauling
    /// runs (its plan exists), which delivers what the remote haulers bring. `spare`: the Standard
    /// tier runs this tick; without it no remote creep is requested (none would be driven) and no
    /// way is searched here.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_home(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        cfg: &RemoteConfig,
        hcfg: &HaulingConfig,
        scfg: &SpawnConfig,
        mcfg: &MovementConfig,
        hauled: bool,
        spare: bool,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let hi = usize::try_from(unit).map_err(|_| Fault::new("unit out of range", unit))?;
        let (name, members) = {
            let h = self
                .homes
                .get(hi)
                .ok_or(Fault::new("no home for the unit", unit))?;
            (h.name, h.members)
        };
        // The candidates, from the home's grid (built if needed, a heavy unit).
        let known = self.homes.get(hi).map_or(0, |h| h.version);
        if let Some((grid, version)) = movement.room_grid(name, mcfg, commit)
            && version != known
        {
            let list = candidates(name, grid);
            if let Some(h) = self.homes.get_mut(hi) {
                h.version = version;
                for &(side, room) in &list {
                    match h.remotes.iter_mut().find(|r| r.room == room) {
                        Some(r) => r.side = Some(side),
                        None => h.remotes.push(new_remote(room, Some(side))),
                    }
                }
                for r in &mut h.remotes {
                    if !list.iter().any(|&(_, room)| room == r.room) {
                        r.side = None;
                    }
                }
            }
        }
        // Rooms its creeps name are its remote rooms too, candidates or not.
        for k in members.0..members.1 {
            let Some(m) = self.members.get(k).copied() else {
                continue;
            };
            if let Some(h) = self.homes.get_mut(hi)
                && !h.remotes.iter().any(|r| r.room == m.remote)
            {
                h.remotes.push(new_remote(m.remote, None));
            }
        }
        self.price(hi, hcfg, cfg, commit);
        self.count(hi, snap);
        self.guard(hi, cfg);
        self.audit_creeps(hi, snap);
        self.choose(hi, cfg);
        self.assign(hi, snap);
        if !spare {
            return Ok(());
        }
        // A room with creeps but no search yet (after a global reset) is searched here, without
        // waiting for the Optional placement: its creeps hold until it has ways.
        let unsearched = self.homes.get(hi).and_then(|h| {
            h.remotes
                .iter()
                .find(|r| {
                    r.searched.is_none()
                        && r.side.is_some()
                        && r.stats.reservers + r.stats.miners + r.stats.haulers > 0
                })
                .map(|r| r.room)
        });
        if let Some(room) = unsearched {
            self.search(hi, room, snap, movement, mcfg, commit);
        }
        if !cfg.enabled || !hauled {
            return Ok(());
        }
        let Some(h) = self.homes.get(hi) else {
            return Ok(());
        };
        if u32::from(h.level) < cfg.min_level || h.spawn.is_none() {
            return Ok(());
        }
        self.scout(hi, spawner, cfg);
        self.request(hi, snap, spawner, scfg);
        Ok(())
    }

    /// Prices each candidate whose intel allows mining it and whose ways are searched, again when
    /// the home's capacity or the ways' grid versions change.
    fn price(&mut self, hi: usize, hcfg: &HaulingConfig, cfg: &RemoteConfig, commit: &mut Commit) {
        let Some(h) = self.homes.get(hi) else {
            return;
        };
        let home = RemoteHome {
            energy: h.capacity,
            unload: extension_energy_capacity(u32::from(h.level)).max(1),
            margin_pct: hcfg.margin_pct,
        };
        let capacity = h.capacity;
        let minable: Vec<bool> = h
            .remotes
            .iter()
            .map(|r| self.intel(r.room).is_some_and(Intel::minable))
            .collect();
        let Some(h) = self.homes.get_mut(hi) else {
            return;
        };
        for (r, ok) in h.remotes.iter_mut().zip(minable) {
            let Some(ways) = r.ways.as_ref().filter(|_| ok) else {
                if !r.active {
                    r.profit = None;
                    r.crew = None;
                    r.priced = None;
                }
                continue;
            };
            let key = (capacity, ways.versions);
            if r.priced == Some(key) {
                continue;
            }
            // Four plans, each sizing bodies (heavy for the slicer).
            commit.heavy();
            let sources: Vec<RemoteSource> = ways
                .sources
                .iter()
                .map(|s| RemoteSource {
                    way: s.way,
                    road: s.road,
                    road_new: s.road_new,
                    // The placement builds it; until then the build is priced.
                    container: false,
                })
                .collect();
            let room = RemoteRoom {
                sources: &sources,
                controller: ways.controller.map(|c| c.1),
            };
            r.profit = best_plan(&room, home, cfg);
            r.crew = r.profit.and_then(|p| remote_crew(&room, p.plan, home));
            r.priced = Some(key);
        }
    }

    /// Counts each remote room's creeps, by role, and its hostiles this tick.
    fn count(&mut self, hi: usize, snap: &Snapshot) {
        let Some(h) = self.homes.get(hi) else {
            return;
        };
        let members = h.members;
        let mut counts: Vec<(RoomName, RemoteStats)> = h
            .remotes
            .iter()
            .map(|r| {
                let hostiles = snap
                    .creeps
                    .my()
                    .iter()
                    .zip(snap.creeps.positions())
                    .filter(|(m, p)| !**m && p.room_name() == r.room)
                    .count();
                (
                    r.room,
                    RemoteStats {
                        reservation: self
                            .intel(r.room)
                            .map_or(Reservation::None, |i| i.reservation),
                        hostiles: u32::try_from(hostiles).unwrap_or(u32::MAX),
                        ..RemoteStats::default()
                    },
                )
            })
            .collect();
        for m in self.members.get(members.0..members.1).unwrap_or(&[]) {
            if let Some((_, s)) = counts.iter_mut().find(|c| c.0 == m.remote) {
                let n = match m.role {
                    Role::Scout => &mut s.scouts,
                    Role::Reserver => &mut s.reservers,
                    Role::RemoteMiner => &mut s.miners,
                    _ => &mut s.haulers,
                };
                *n = n.saturating_add(1);
            }
        }
        if let Some(h) = self.homes.get_mut(hi) {
            for (r, (_, s)) in h.remotes.iter_mut().zip(counts) {
                r.stats = s;
            }
        }
    }

    /// The audit's counts (P6.3), after [`Self::guard`] rolled the window over: each new creep's
    /// body (its role's body in its room's crew; those alive on the tick a room's first window of
    /// the global starts were paid for before it), and each remote hauler's energy on the tick it
    /// crosses into its home room.
    fn audit_creeps(&mut self, hi: usize, snap: &Snapshot) {
        let Some(h) = self.homes.get(hi) else {
            return;
        };
        let (home, members) = (h.name, h.members);
        for k in members.0..members.1 {
            let Some(m) = self.members.get(k).copied() else {
                continue;
            };
            if m.role == Role::Scout {
                continue;
            }
            let here = snap.creeps.positions().get(m.row).map(|p| p.room_name());
            let energy = snap.creeps.stores().get(m.row).map_or(0, |s| s.energy);
            let now = self.time;
            let (cost, paid) = self
                .homes
                .get(hi)
                .and_then(|h| h.remotes.iter().find(|r| r.room == m.remote))
                .map_or((0, true), |r| {
                    let fresh = r.audit.windows == 0 && r.audit.start == Some(now);
                    (
                        body_cost(r.crew.as_ref(), m.role),
                        r.audit.start.is_none() || fresh,
                    )
                });
            let Some(w) = self.worker_mut(m.handle) else {
                continue;
            };
            let spend = (!w.counted && !paid).then_some(cost);
            w.counted = true;
            let crossed = m.role == Role::RemoteHauler
                && energy > 0
                && !w.delivered
                && here == Some(home)
                && w.last.is_some_and(|l| l != home);
            if crossed {
                w.delivered = true;
            } else if energy == 0 {
                w.delivered = false;
            }
            w.last = here;
            if let Some(r) = self
                .homes
                .get_mut(hi)
                .and_then(|h| h.remotes.iter_mut().find(|r| r.room == m.remote))
            {
                if let Some(c) = spend {
                    r.audit.spent = r.audit.spent.saturating_add(c);
                }
                if crossed {
                    r.audit.delivered = r.audit.delivered.saturating_add(u64::from(energy));
                }
            }
        }
    }

    /// Threats, contests and audits (P6.3), for each room the home mines (chosen or crewed):
    /// - a threat ends at its `until`; an abandon at its `until`;
    /// - a room seen contested this tick is abandoned (`Contested`);
    /// - armed hostiles seen this tick start a threat of `remote.evacuate_ticks` (or extend one),
    ///   and the `remote.contest_threats`-th start within `remote.abandon_ticks` abandons the room
    ///   (`Threats`);
    /// - an audit window of `remote.audit_ticks` that is not the room's first in this global and
    ///   delivered less than it spent abandons the room (`Unprofitable`).
    fn guard(&mut self, hi: usize, cfg: &RemoteConfig) {
        let now = self.time;
        let Some(h) = self.homes.get(hi) else {
            return;
        };
        let home = h.name;
        let looks: Vec<(bool, u32, bool)> = h
            .remotes
            .iter()
            .map(|r| {
                self.intel(r.room).map_or((false, 0, false), |i| {
                    (i.seen == now, i.armed, i.contested())
                })
            })
            .collect();
        let mut events: Vec<RemoteEvent> = Vec::new();
        let Some(h) = self.homes.get_mut(hi) else {
            return;
        };
        for (r, (seen, armed, contested)) in h.remotes.iter_mut().zip(looks) {
            if r.threat.is_some_and(|t| t.until <= now) {
                r.threat = None;
                events.push(RemoteEvent::Resumed { home, room: r.room });
            }
            if r.abandoned.is_some_and(|a| a.until <= now) {
                r.abandoned = None;
            }
            r.threats_at
                .retain(|&t| now.saturating_sub(t) < cfg.abandon_ticks);
            let crewed = r.stats.reservers + r.stats.miners + r.stats.haulers > 0;
            // An unprofitable room's creeps work on, so it is still guarded (no audit, no count
            // of threats); any other abandoned or unmined room is not.
            let working = r
                .abandoned
                .is_none_or(|a| a.reason == AbandonReason::Unprofitable && crewed);
            if !(r.active || crewed) || !working {
                r.audit = Audit::default();
                continue;
            }
            let judged = r.abandoned.is_none();
            let mut abandon = None;
            if seen && contested {
                abandon = Some(AbandonReason::Contested);
            } else if seen && armed > 0 {
                let until = now.saturating_add(cfg.evacuate_ticks);
                match r.threat.as_mut() {
                    Some(t) => {
                        t.until = t.until.max(until);
                        t.armed = armed;
                    }
                    None => {
                        r.threat = Some(Threat {
                            since: now,
                            until,
                            armed,
                        });
                        r.threats_at.push(now);
                        events.push(RemoteEvent::Threatened {
                            home,
                            room: r.room,
                            armed,
                            until,
                        });
                        let count = u32::try_from(r.threats_at.len()).unwrap_or(u32::MAX);
                        if judged && count >= cfg.contest_threats.max(1) {
                            abandon = Some(AbandonReason::Threats);
                        }
                    }
                }
            }
            // The audit window (a window that held a threat is not judged: its creeps waited).
            if r.threat.is_some() {
                r.audit.threatened = true;
            }
            match r.audit.start {
                _ if !judged => {}
                None => {
                    r.audit = Audit {
                        start: Some(now),
                        ..Audit::default()
                    };
                }
                Some(start) if now.saturating_sub(start) >= cfg.audit_ticks.max(1) => {
                    if abandon.is_none()
                        && r.audit.windows > 0
                        && !r.audit.threatened
                        && r.audit.delivered < r.audit.spent
                    {
                        abandon = Some(AbandonReason::Unprofitable);
                    }
                    r.audit = Audit {
                        start: Some(now),
                        windows: r.audit.windows.saturating_add(1),
                        ..Audit::default()
                    };
                }
                Some(_) => {}
            }
            if let Some(reason) = abandon {
                let until = now.saturating_add(cfg.abandon_ticks);
                r.abandoned = Some(Abandon {
                    since: now,
                    until,
                    reason,
                });
                r.left_at = Some(now);
                r.active = false;
                // Its creeps end or come home now; no wait is left to run out.
                if reason != AbandonReason::Unprofitable {
                    r.threat = None;
                }
                events.push(RemoteEvent::Abandoned {
                    home,
                    room: r.room,
                    reason,
                    until,
                });
            }
        }
        self.events.extend(events);
    }

    /// The rooms the home mines: those its working creeps name, and, while it mines fewer than
    /// `remote.max_rooms` and the spawn time allows, the priced candidates that net the most (not
    /// abandoned, not threatened, and with no hostile creep when seen). A mined room stays mined
    /// through hostile creeps (threats are [`Self::guard`]'s) until it is contested or abandoned.
    fn choose(&mut self, hi: usize, cfg: &RemoteConfig) {
        // (minable: may be chosen; held: may stay mined, hostile creeps aside, which are threats).
        let looks: Vec<(bool, bool)> = self
            .homes
            .get(hi)
            .map(|h| {
                h.remotes
                    .iter()
                    .map(|r| {
                        self.intel(r.room).map_or((false, false), |i| {
                            // A room abandoned before is chosen again only on a newer look.
                            let fresh = r.left_at.is_none_or(|t| i.seen > t);
                            (
                                i.minable() && fresh,
                                !i.contested() && i.controller_tile.is_some(),
                            )
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let minable: Vec<bool> = looks.iter().map(|l| l.0).collect();
        let Some(h) = self.homes.get_mut(hi) else {
            return;
        };
        for (r, &(_, held)) in h.remotes.iter_mut().zip(&looks) {
            let crewed = r.stats.reservers + r.stats.miners + r.stats.haulers > 0;
            r.active = held && r.abandoned.is_none() && r.profit.is_some() && (r.active || crewed);
        }
        let budget = u64::from(cfg.spawn_permille).saturating_mul(u64::from(h.spawns.max(1)));
        let mut used: u64 = h
            .remotes
            .iter()
            .filter(|r| r.active)
            .filter_map(|r| r.profit)
            .map(|p| p.spawn_permille())
            .fold(0, u64::saturating_add);
        let mut count = h.remotes.iter().filter(|r| r.active).count();
        let max = usize::try_from(cfg.max_rooms).unwrap_or(usize::MAX);
        if !cfg.enabled {
            return;
        }
        loop {
            if count >= max {
                break;
            }
            let next = h
                .remotes
                .iter()
                .enumerate()
                .filter(|&(k, r)| {
                    !r.active
                        && r.side.is_some()
                        && r.abandoned.is_none()
                        && r.threat.is_none()
                        && minable.get(k).copied().unwrap_or(false)
                        && r.profit
                            .is_some_and(|p| used.saturating_add(p.spawn_permille()) <= budget)
                })
                .max_by_key(|(_, r)| {
                    (
                        r.profit.map_or(i64::MIN, |p| p.net),
                        Reverse(r.room.packed_repr()),
                    )
                })
                .map(|(k, _)| k);
            let Some(r) = next.and_then(|k| h.remotes.get_mut(k)) else {
                break;
            };
            r.active = true;
            used = used.saturating_add(r.profit.map_or(0, |p| p.spawn_permille()));
            count += 1;
        }
    }

    /// Gives each remote miner and hauler a source of its room: the one it had, else (after a
    /// reset or for a new creep) a miner the source whose mining tile it stands on, else for both
    /// the source with the fewest of its role for what it wants.
    fn assign(&mut self, hi: usize, snap: &Snapshot) {
        let Some(h) = self.homes.get(hi) else {
            return;
        };
        let members = h.members;
        for k in members.0..members.1 {
            let Some(m) = self.members.get(k).copied() else {
                continue;
            };
            if !matches!(m.role, Role::RemoteMiner | Role::RemoteHauler) {
                self.worker_mut(m.handle);
                continue;
            }
            let Some(r) = self
                .homes
                .get(hi)
                .and_then(|h| h.remotes.iter().find(|r| r.room == m.remote))
            else {
                continue;
            };
            let Some(ways) = r.ways.as_ref() else {
                continue;
            };
            let current = self.worker(m.handle).and_then(|w| w.source);
            if current.is_some_and(|s| ways.sources.iter().any(|w| w.source == s)) {
                continue;
            }
            let here = snap.creeps.positions().get(m.row).copied();
            let on_tile = here.and_then(|p| {
                (m.role == Role::RemoteMiner && p.room_name() == m.remote)
                    .then(|| {
                        ways.sources
                            .iter()
                            .find(|w| w.tile == Tile::of(p))
                            .map(|w| w.source)
                    })
                    .flatten()
            });
            let want = |j: usize| -> u32 {
                match m.role {
                    Role::RemoteMiner => 1,
                    _ => r
                        .crew
                        .as_ref()
                        .and_then(|c| c.sources.get(j))
                        .map_or(1, |c| c.haulers.max(1)),
                }
            };
            let pick = on_tile.or_else(|| {
                ways.sources
                    .iter()
                    .enumerate()
                    .min_by_key(|&(j, w)| {
                        let n = self.assigned(hi, m.remote, m.role, w.source);
                        // n / want, compared as n × other's want: by (n × 1000 / want).
                        (u64::from(n).saturating_mul(1000) / u64::from(want(j)), j)
                    })
                    .map(|(_, w)| w.source)
            });
            if let Some(w) = self.worker_mut(m.handle) {
                w.source = pick;
            }
        }
    }

    /// Creeps of `role` of home `hi` assigned to `source` of `room`.
    fn assigned(&self, hi: usize, room: RoomName, role: Role, source: Tile) -> u32 {
        let Some(h) = self.homes.get(hi) else {
            return 0;
        };
        let n = self
            .members
            .get(h.members.0..h.members.1)
            .unwrap_or(&[])
            .iter()
            .filter(|m| m.remote == room && m.role == role)
            .filter(|m| self.worker(m.handle).and_then(|w| w.source) == Some(source))
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    /// Orders a scout while the home mines fewer rooms than it may and has a candidate it knows
    /// nothing about (or saw longer ago than `remote.rescout_ticks` and does not mine), and no
    /// scout of the home is alive.
    fn scout(&mut self, hi: usize, spawner: &mut Spawner, cfg: &RemoteConfig) {
        let Some(h) = self.homes.get(hi) else {
            return;
        };
        let active = h.remotes.iter().filter(|r| r.active).count();
        if active >= usize::try_from(cfg.max_rooms).unwrap_or(usize::MAX) {
            return;
        }
        let scouting = self
            .members
            .get(h.members.0..h.members.1)
            .unwrap_or(&[])
            .iter()
            .any(|m| m.role == Role::Scout);
        if scouting {
            return;
        }
        let Some(target) = h
            .remotes
            .iter()
            .find(|r| {
                r.side.is_some()
                    && !r.active
                    && r.abandoned.is_none()
                    && r.threat.is_none()
                    && (self.needs_look(r.room, cfg) || self.left_unseen(r))
            })
            .map(|r| r.room)
        else {
            return;
        };
        let Some(body) = Body::new(&[Part::Move]) else {
            return;
        };
        let asked = spawner.request(SpawnRequest {
            home: h.name,
            role: Role::Scout,
            remote: Some(target),
            body: BodySpec::Exact(body),
            budget: EnergyBudget::Capacity,
            priority: PRIORITY_SCOUT,
        });
        if asked.is_some()
            && let Some(r) = self
                .homes
                .get_mut(hi)
                .and_then(|h| h.remotes.iter_mut().find(|r| r.room == target))
        {
            r.stats.requested = r.stats.requested.saturating_add(1);
        }
    }

    /// Whether `r` was abandoned and not seen since (P6.3): a scout looks again before it is chosen.
    fn left_unseen(&self, r: &Remote) -> bool {
        r.left_at
            .is_some_and(|t| self.intel(r.room).is_none_or(|i| i.seen <= t))
    }

    /// Whether `room` needs a look: no intel, or intel older than `remote.rescout_ticks`.
    fn needs_look(&self, room: RoomName, cfg: &RemoteConfig) -> bool {
        self.intel(room)
            .is_none_or(|i| self.time.saturating_sub(i.seen) > cfg.rescout_ticks)
    }

    /// The spawn requests of each room the home mines: a reserver, one miner a source and its
    /// haulers, each replaced ahead of its death (P5.2).
    fn request(&mut self, hi: usize, snap: &Snapshot, spawner: &mut Spawner, scfg: &SpawnConfig) {
        let Some(h) = self.homes.get(hi) else {
            return;
        };
        let (home, members) = (h.name, h.members);
        let now = self.time;
        // The home's remote creeps: room, role, source and ticks to live.
        let crew: Vec<(RoomName, Role, Option<Tile>, Option<u32>)> = self
            .members
            .get(members.0..members.1)
            .unwrap_or(&[])
            .iter()
            .map(|m| {
                (
                    m.remote,
                    m.role,
                    self.worker(m.handle).and_then(|w| w.source),
                    snap.creeps.ticks_to_live(m.row, now),
                )
            })
            .collect();
        let mut asks: Vec<(RoomName, Role, Body, u8)> = Vec::new();
        for r in h.remotes.iter().filter(|r| r.active && r.threat.is_none()) {
            let (Some(plan), Some(ways)) = (r.crew.as_ref(), r.ways.as_ref()) else {
                continue;
            };
            let live = |role: Role, source: Option<Tile>, lead: u32| {
                crew.iter()
                    .filter(|c| c.0 == r.room && c.1 == role)
                    .filter(|c| source.is_none() || c.2 == source)
                    .filter(|c| !is_due(c.3, lead))
                    .count()
            };
            // The reserver: when none is alive or due, and the reservation would run out before
            // the next one arrives.
            if let Some((body, walk)) = plan.reserver {
                let lead = lead(scfg, &body, walk);
                let left = match self.intel(r.room).map(|i| (i.reservation, i.seen)) {
                    Some((Reservation::Mine { ticks }, seen)) => {
                        ticks.saturating_sub(now.saturating_sub(seen))
                    }
                    _ => 0,
                };
                let arrive = body.spawn_time().saturating_add(walk);
                if live(Role::Reserver, None, lead) == 0 && left <= lead.max(arrive) {
                    asks.push((r.room, Role::Reserver, body, PRIORITY_RESERVER));
                }
            }
            for (j, s) in ways.sources.iter().enumerate() {
                let Some(c) = plan.sources.get(j) else {
                    continue;
                };
                let miner_lead = lead(scfg, &c.miner, c.miner_walk);
                if live(Role::RemoteMiner, Some(s.source), miner_lead) == 0 {
                    asks.push((r.room, Role::RemoteMiner, c.miner, PRIORITY_REMOTE_MINER));
                }
                let hauler_lead = lead(scfg, &c.hauler, c.hauler_walk);
                let haulers = live(Role::RemoteHauler, Some(s.source), hauler_lead);
                if u32::try_from(haulers).unwrap_or(u32::MAX) < c.haulers {
                    asks.push((r.room, Role::RemoteHauler, c.hauler, PRIORITY_REMOTE_HAULER));
                }
            }
        }
        for (room, role, body, priority) in asks {
            let asked = spawner.request(SpawnRequest {
                home,
                role,
                remote: Some(room),
                body: BodySpec::Exact(body),
                budget: EnergyBudget::Capacity,
                priority,
            });
            if asked.is_some()
                && let Some(r) = self
                    .homes
                    .get_mut(hi)
                    .and_then(|h| h.remotes.iter_mut().find(|r| r.room == room))
            {
                r.stats.requested = r.stats.requested.saturating_add(1);
            }
        }
    }

    fn worker(&self, handle: CreepHandle) -> Option<&Worker> {
        self.workers
            .get(usize::try_from(handle.slot()).ok()?)
            .and_then(Option::as_ref)
            .filter(|w| w.handle == handle)
    }

    fn worker_mut(&mut self, handle: CreepHandle) -> Option<&mut Worker> {
        let slot = usize::try_from(handle.slot()).ok()?;
        if self.workers.len() <= slot {
            self.workers.resize(slot.saturating_add(1), None);
        }
        let entry = self.workers.get_mut(slot)?;
        if entry.is_none_or(|w| w.handle != handle) {
            *entry = Some(Worker {
                handle,
                source: None,
                returning: false,
                last: None,
                counted: false,
                delivered: false,
            });
        }
        entry.as_mut()
    }

    /// The Standard half of remote creep `unit`'s tick (module docs, step 3): its intents and
    /// moves. A remote hauler in its home room with energy is the home's hauling's
    /// ([`is_guest`]), and nothing is done for it here.
    #[allow(clippy::too_many_arguments)]
    pub fn work_creep(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &RemoteConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let m = usize::try_from(unit)
            .ok()
            .and_then(|u| self.members.get(u).copied())
            .ok_or(Fault::new("unit out of range", unit))?;
        if snap.creeps.spawning().get(m.row).copied().unwrap_or(true) {
            return Ok(());
        }
        let here = snap
            .creeps
            .positions()
            .get(m.row)
            .copied()
            .ok_or(Fault::new("no position", unit))?;
        if m.role == Role::RemoteHauler && is_guest(snap, m.row) {
            return Ok(());
        }
        let source = self.worker(m.handle).and_then(|w| w.source);
        let home = self.homes.get(m.home).ok_or(Fault::new("no home", unit))?;
        let home_name = home.name;
        let home_spot = home.spawn.and_then(|t| at(home.name, t));
        let remote = home.remotes.iter().find(|r| r.room == m.remote);
        // What this creep needs of its room's ways and crew, copied out (no clone of a list).
        let controller_tile = remote
            .and_then(|r| r.ways.as_ref())
            .and_then(|w| w.controller)
            .map(|c| c.0);
        let claim = remote
            .and_then(|r| r.crew.as_ref())
            .and_then(|c| c.reserver)
            .map_or(1, |(b, _)| u32::try_from(b.count(Part::Claim)).unwrap_or(1));
        let post = remote.and_then(|r| {
            let ways = r.ways.as_ref()?;
            let j = ways.sources.iter().position(|s| Some(s.source) == source)?;
            let s = ways.sources.get(j)?;
            let c = r.crew.as_ref().and_then(|c| c.sources.get(j));
            Some(Post {
                source: s.source,
                tile: s.tile,
                miner_work: c.map_or(1, |c| c.work.max(1)),
                hauler_work: c.map_or(0, |c| {
                    u32::try_from(c.hauler.count(Part::Work)).unwrap_or(0)
                }),
            })
        });
        let go = |movement: &mut Movement,
                  to: Position,
                  range: u8,
                  priority: u8,
                  commit: &mut Commit| {
            let _ = movement.request(
                snap,
                m.handle,
                MoveTo {
                    target: to,
                    range,
                    priority,
                    tier: Tier::Standard,
                },
                mcfg,
                commit,
            );
        };
        let inside = |room: RoomName| Tile::new(CENTER, CENTER).and_then(|t| at(room, t));
        // P6.3: a threatened room's creeps wait in the home; an abandoned (contested or too often
        // threatened) room's haulers bring their load home and the rest end their lives (a harvest
        // or a reserve there fails, V182, V263, V271), while an unprofitable room's work on.
        let now = self.time;
        let threatened = remote.is_some_and(|r| r.threat.is_some_and(|t| t.until > now));
        let given_up = remote
            .and_then(|r| r.abandoned)
            .is_some_and(|a| a.reason != AbandonReason::Unprofitable);
        if m.role != Role::Scout && (threatened || given_up) {
            let energy = snap.creeps.stores().get(m.row).map_or(0, |s| s.energy);
            let carries = m.role == Role::RemoteHauler && energy > 0;
            let spot = home_spot.or_else(|| inside(home_name));
            if given_up && !carries {
                let _ = buffer.push(snap, m.handle, CreepIntent::Suicide, commit);
            } else if let Some(p) = spot {
                // Walk to within range of the spot (a creep that just crossed in stands on an edge
                // tile, where a hold would carry it back out, V151), then make way within it.
                let near = here.room_name() == home_name
                    && !Tile::of(here).is_edge()
                    && Tile::of(here).range(Tile::of(p)) <= EVACUATE_RANGE;
                if near {
                    movement.give_way(snap, m.handle, Some((p, EVACUATE_RANGE)));
                } else {
                    go(movement, p, EVACUATE_RANGE, MOVE_PRIORITY, commit);
                }
            }
            return Ok(());
        }
        match m.role {
            Role::Scout => match self.scout_goal(m, here, cfg) {
                Some(room) => {
                    if let Some(p) = inside(room) {
                        go(movement, p, INSIDE, MOVE_PRIORITY, commit);
                    }
                }
                None => {
                    let _ = buffer.push(snap, m.handle, CreepIntent::Suicide, commit);
                }
            },
            Role::Reserver => {
                let Some(target) = controller_tile.and_then(|t| at(m.remote, t)) else {
                    // No ways yet (after a reset, until they are searched again): walk into the
                    // room, or hold where it stands there, with a request, so movement keeps the
                    // room's grid read for the search.
                    if here.room_name() == m.remote {
                        go(movement, here, 0, MOVE_PRIORITY, commit);
                    } else if let Some(p) = inside(m.remote) {
                        go(movement, p, INSIDE, MOVE_PRIORITY, commit);
                    }
                    return Ok(());
                };
                go(movement, target, 0, MOVE_PRIORITY, commit);
                let other = snap
                    .rooms
                    .row_of(m.remote)
                    .and_then(|r| snap.rooms.remote_reads().get(r))
                    .is_some_and(|r| matches!(r.reservation, Reservation::Other { .. }));
                if let Some((controller, ticks)) = remote_controller(snap, m.remote)
                    && here.room_name() == m.remote
                    && !other
                    // A reserve past the cap is billed and dropped (V264).
                    && ticks.saturating_add(claim.saturating_mul(CONTROLLER_RESERVE))
                        <= CONTROLLER_RESERVE_MAX
                {
                    let _ = buffer.push(
                        snap,
                        m.handle,
                        CreepIntent::ReserveController { target: controller },
                        commit,
                    );
                }
            }
            Role::RemoteMiner => {
                let Some(post) = post else {
                    // No ways yet (after a reset, until they are searched again): walk into the
                    // room, or hold where it stands there, with a request, so movement keeps the
                    // room's grid read for the search.
                    if here.room_name() == m.remote {
                        go(movement, here, 0, MOVE_PRIORITY, commit);
                    } else if let Some(p) = inside(m.remote) {
                        go(movement, p, INSIDE, MOVE_PRIORITY, commit);
                    }
                    return Ok(());
                };
                let Some(tile) = at(m.remote, post.tile) else {
                    return Ok(());
                };
                // Its predecessor holds the tile: wait next to it (P5.2).
                let creeps = &snap.creeps;
                let held = here != tile
                    && creeps
                        .positions()
                        .iter()
                        .zip(creeps.identities())
                        .zip(creeps.my())
                        .any(|((&p, id), &my)| {
                            my && p == tile && id.is_some_and(|id| id.role == Role::RemoteMiner)
                        });
                if held {
                    go(movement, tile, NEAR, MINER_PRIORITY, commit);
                    return Ok(());
                }
                go(movement, tile, 0, MINER_PRIORITY, commit);
                if here != tile {
                    return Ok(());
                }
                let store = creeps.stores().get(m.row).copied().unwrap_or_default();
                let objects = &snap.objects;
                let on = |want: fn(ObjectKind) -> bool| {
                    objects
                        .kinds()
                        .iter()
                        .zip(objects.positions())
                        .position(|(&k, &p)| want(k) && p == tile)
                };
                let site = on(|k| matches!(k, ObjectKind::Site(StructureType::Container)));
                let container = on(|k| k == ObjectKind::Container);
                // A build spends BUILD_POWER a WORK part; a repair restores REPAIR_POWER a WORK
                // part for REPAIR_COST a hit, one energy a WORK part (V186, V221, V222).
                let build_energy = post.miner_work.saturating_mul(BUILD_POWER);
                let repair_energy = post.miner_work;
                let floor = CONTAINER_HITS / 100 * cfg.container_repair_pct.min(100);
                let intent = if let Some(k) = site
                    && store.energy >= build_energy.min(store.capacity.max(1))
                {
                    objects
                        .handles()
                        .get(k)
                        .map(|&h| CreepIntent::Build { target: h })
                } else if let Some(k) = container
                    && objects.hits().get(k).copied().unwrap_or(CONTAINER_HITS) < floor
                    && store.energy >= repair_energy
                {
                    objects
                        .handles()
                        .get(k)
                        .map(|&h| CreepIntent::RepairObject { target: h })
                } else {
                    objects
                        .kinds()
                        .iter()
                        .zip(objects.positions())
                        .position(|(&k, &p)| {
                            k == ObjectKind::Source
                                && p.room_name() == m.remote
                                && Tile::of(p) == post.source
                        })
                        .filter(|&k| objects.amounts().get(k).copied().unwrap_or(0) > 0)
                        .and_then(|k| objects.handles().get(k))
                        .map(|&h| CreepIntent::Harvest { target: h })
                };
                if let Some(i) = intent {
                    let _ = buffer.push(snap, m.handle, i, commit);
                }
            }
            Role::RemoteHauler => {
                let store = snap.creeps.stores().get(m.row).copied().unwrap_or_default();
                let was = self.worker(m.handle).is_some_and(|w| w.returning);
                let returning = store.energy > 0 && (store.energy >= store.capacity || was);
                if let Some(w) = self.worker_mut(m.handle) {
                    w.returning = returning;
                }
                if returning {
                    if let Some(p) = home_spot {
                        go(movement, p, WORK_RANGE, MOVE_PRIORITY, commit);
                    }
                    let work = post.map_or(0, |p| p.hauler_work);
                    if here.room_name() == m.remote
                        && let Some(intent) = road_work(snap, here, store.energy, work)
                    {
                        let _ = buffer.push(snap, m.handle, intent, commit);
                    }
                    return Ok(());
                }
                let Some(tile) = post.and_then(|p| at(m.remote, p.tile)) else {
                    // No ways yet (after a reset, until they are searched again): walk into the
                    // room, or hold where it stands there, with a request, so movement keeps the
                    // room's grid read for the search.
                    if here.room_name() == m.remote {
                        go(movement, here, 0, MOVE_PRIORITY, commit);
                    } else if let Some(p) = inside(m.remote) {
                        go(movement, p, INSIDE, MOVE_PRIORITY, commit);
                    }
                    return Ok(());
                };
                go(movement, tile, NEAR, MOVE_PRIORITY, commit);
                if here.room_name() != m.remote || Tile::of(here).range(Tile::of(tile)) > NEAR {
                    return Ok(());
                }
                let free = store.capacity.saturating_sub(store.energy);
                if let Some(intent) = self.take(snap, tile, free) {
                    let _ = buffer.push(snap, m.handle, intent, commit);
                }
            }
            Role::Worker
            | Role::Miner
            | Role::Hauler
            | Role::Upgrader
            | Role::Signer
            | Role::Defender
            | Role::RemoteDefender => {}
        }
        Ok(())
    }

    /// The room a scout goes to next: the room it stands in while that room may be mined and its
    /// ways are not searched yet (movement reads a room's grid only while a creep of the bot moves
    /// there), else its own remote room while the bot knows nothing of it, else the nearest other
    /// candidate of its home that needs a look ([`Self::needs_look`]); `None` when none is left.
    fn scout_goal(&self, m: Member, here: Position, cfg: &RemoteConfig) -> Option<RoomName> {
        let h = self.homes.get(m.home)?;
        let room = here.room_name();
        let unsearched = h.remotes.iter().any(|r| {
            r.room == room && r.searched.is_none() && self.intel(room).is_some_and(Intel::minable)
        });
        if unsearched {
            return Some(room);
        }
        let own_threatened = h
            .remotes
            .iter()
            .any(|r| r.room == m.remote && r.threat.is_some());
        if self.needs_look(m.remote, cfg) && !own_threatened {
            return Some(m.remote);
        }
        h.remotes
            .iter()
            .filter(|r| {
                r.side.is_some()
                    && r.threat.is_none()
                    && r.abandoned.is_none()
                    && (self.needs_look(r.room, cfg) || self.left_unseen(r))
            })
            .min_by_key(|r| (room_distance(room, r.room), r.room.packed_repr()))
            .map(|r| r.room)
    }

    /// What a hauler next to `tile` takes for `free` room: the pile there with a whole load, else
    /// the container with one, else the pile when the two make one; claimed for this tick.
    fn take(&mut self, snap: &Snapshot, tile: Position, free: u32) -> Option<CreepIntent> {
        if free == 0 {
            return None;
        }
        let objects = &snap.objects;
        let find = |kind: ObjectKind| {
            objects
                .kinds()
                .iter()
                .zip(objects.positions())
                .zip(objects.handles())
                .zip(objects.amounts())
                .find(|&(((&k, &p), _), _)| k == kind && p == tile)
                .map(|(((_, _), &h), &a)| (h, a))
        };
        let left = |c: &[(ObjectHandle, u32)], o: Option<(ObjectHandle, u32)>| {
            o.map(|(h, a)| {
                let taken = c.iter().filter(|x| x.0 == h).map(|x| x.1).sum::<u32>();
                (h, a.saturating_sub(taken))
            })
        };
        let pile = left(&self.claimed, find(ObjectKind::Energy));
        let container = left(&self.claimed, find(ObjectKind::Container));
        let (p, c) = (pile.map_or(0, |x| x.1), container.map_or(0, |x| x.1));
        let (pick, pickup) = if p >= free {
            (pile, true)
        } else if c >= free {
            (container, false)
        } else if p > 0 && p.saturating_add(c) >= free {
            (pile, true)
        } else {
            (None, false)
        };
        let (h, _) = pick?;
        self.claimed.push((h, free));
        Some(if pickup {
            CreepIntent::Pickup { target: h }
        } else {
            CreepIntent::WithdrawObject {
                target: h,
                resource: ResourceType::Energy,
                amount: Some(free.min(c)),
            }
        })
    }

    /// Searches `room`'s ways from home `hi` ([`WaySearch`]) when its intel allows mining it, both
    /// grids are built, and it was not searched on their versions; records the search, whatever it
    /// finds. Returns whether it searched (a heavy unit, [`Commit::heavy`]).
    fn search(
        &mut self,
        hi: usize,
        room: RoomName,
        snap: &Snapshot,
        movement: &mut Movement,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> bool {
        let Some(h) = self.homes.get(hi) else {
            return false;
        };
        let (name, spawn) = (h.name, h.spawn);
        let Some(side) = h
            .remotes
            .iter()
            .find(|r| r.room == room)
            .and_then(|r| r.side)
        else {
            return false;
        };
        let Some(spawn) = spawn else {
            return false;
        };
        if !self.intel(room).is_some_and(Intel::minable) {
            return false;
        }
        let home_version = movement.room_grid(name, mcfg, commit).map(|g| g.1);
        let remote_version = movement.room_grid(room, mcfg, commit).map(|g| g.1);
        let (Some(hv), Some(rv)) = (home_version, remote_version) else {
            return false;
        };
        let searched = h
            .remotes
            .iter()
            .find(|r| r.room == room)
            .and_then(|r| r.searched);
        if searched == Some((hv, rv)) {
            return false;
        }
        commit.heavy();
        let found = self.search_ways(name, room, spawn, side, snap, movement, mcfg, commit);
        if let Some(r) = self
            .homes
            .get_mut(hi)
            .and_then(|h| h.remotes.iter_mut().find(|r| r.room == room))
        {
            r.searched = Some((hv, rv));
            r.ways = found.map(|(sources, controller)| RemoteWays {
                versions: (hv, rv),
                spawn,
                sources,
                controller,
            });
        }
        true
    }

    /// The two-room search itself: the home's part, then the remote room's from its stores (its
    /// containers and container sites) and its standing roads and road sites.
    #[allow(clippy::too_many_arguments)]
    fn search_ways(
        &mut self,
        name: RoomName,
        room: RoomName,
        spawn: Tile,
        side: Side,
        snap: &Snapshot,
        movement: &mut Movement,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Option<Found> {
        let (sources, controller) = {
            let i = self.intel(room)?;
            (i.sources.clone(), i.controller_tile)
        };
        let reached = movement
            .room_grid(name, mcfg, commit)
            .map_or(0, |(g, _)| self.search.home(g, spawn, side));
        if reached == 0 {
            return None;
        }
        let objects = &snap.objects;
        let in_room = |want: fn(ObjectKind) -> bool| {
            objects
                .kinds()
                .iter()
                .zip(objects.positions())
                .filter(move |&(&k, p)| p.room_name() == room && want(k))
                .map(|(_, &p)| Tile::of(p))
        };
        let stores: Vec<Tile> = in_room(|k| {
            matches!(
                k,
                ObjectKind::Container | ObjectKind::Site(StructureType::Container)
            )
        })
        .collect();
        let mut paved: Vec<Tile> = movement
            .room_objects(room)
            .iter()
            .filter(|(_, f)| *f == Footprint::Road)
            .map(|&(t, _)| t)
            .collect();
        paved.extend(in_room(|k| {
            matches!(k, ObjectKind::Road | ObjectKind::Site(StructureType::Road))
        }));
        let terrain = movement.room_terrain(room)?.clone();
        let costs = profile(mcfg);
        let (grid, _) = movement.room_grid(room, mcfg, commit)?;
        self.search
            .remote(grid, &terrain, costs, &sources, controller, &stores, &paved)
    }

    /// The Optional half of home `unit`'s tick (module docs, step 4): one way search (a heavy
    /// unit), else the sites for the rooms it mines.
    pub fn place_home(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        cfg: &RemoteConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let hi = usize::try_from(unit).map_err(|_| Fault::new("unit out of range", unit))?;
        let h = self
            .homes
            .get(hi)
            .ok_or(Fault::new("no home for the unit", unit))?;
        if h.spawn.is_none() {
            return Ok(());
        }
        // One search: the first candidate whose intel allows mining it and that was not searched
        // on these grid versions.
        let rooms: Vec<RoomName> = h
            .remotes
            .iter()
            .filter(|r| r.side.is_some())
            .map(|r| r.room)
            .collect();
        for room in rooms {
            if self.search(hi, room, snap, movement, mcfg, commit) {
                return Ok(());
            }
        }
        // The sites: one a tick per room it mines and can see.
        let Some(h) = self.homes.get(hi) else {
            return Ok(());
        };
        let mut picks: Vec<RemoteSite> = Vec::new();
        for r in h.remotes.iter().filter(|r| r.active && r.threat.is_none()) {
            let (Some(ways), Some(crew)) = (r.ways.as_ref(), r.crew.as_ref()) else {
                continue;
            };
            let read = snap
                .rooms
                .row_of(r.room)
                .and_then(|row| snap.rooms.remote_reads().get(row))
                .copied()
                .unwrap_or_default();
            if !read.read {
                continue;
            }
            let objects = &snap.objects;
            let mut sites = 0u32;
            let mut taken: Vec<Tile> = Vec::new();
            for ((&k, &p), _) in objects
                .kinds()
                .iter()
                .zip(objects.positions())
                .zip(objects.handles())
            {
                if p.room_name() != r.room {
                    continue;
                }
                match k {
                    ObjectKind::Site(_) => {
                        sites = sites.saturating_add(1);
                        taken.push(Tile::of(p));
                    }
                    ObjectKind::Container | ObjectKind::Road => taken.push(Tile::of(p)),
                    _ => {}
                }
            }
            if sites >= cfg.max_sites {
                continue;
            }
            let roads: Vec<Tile> = movement
                .room_objects(r.room)
                .iter()
                .filter(|(_, f)| *f == Footprint::Road)
                .map(|&(t, _)| t)
                .collect();
            let now = self.time;
            let refused = |t: Tile| {
                self.refused.contains(&(r.room, t))
                    || self
                        .placed
                        .iter()
                        .any(|&(room, tile, until)| room == r.room && tile == t && now < until)
            };
            let container = ways
                .sources
                .iter()
                .map(|s| s.tile)
                .find(|&t| !taken.contains(&t) && !refused(t))
                .map(|tile| RemoteSite {
                    room: r.room,
                    tile,
                    kind: StructureType::Container,
                });
            // Roads only on a tick the room's roads were read, so a road built since movement's
            // last scan counts as taken.
            let road = (crew.plan.roads && read.roads).then(|| {
                ways.sources.iter().find_map(|s| {
                    s.path
                        .iter()
                        .find(|&&t| !taken.contains(&t) && !roads.contains(&t) && !refused(t))
                        .map(|&tile| RemoteSite {
                            room: r.room,
                            tile,
                            kind: StructureType::Road,
                        })
                })
            });
            if let Some(site) = container.or(road.flatten()) {
                picks.push(site);
            }
        }
        // A site call is billed like an intent (V203).
        for _ in &picks {
            commit.add(INTENT_CPU_COST);
        }
        self.sites.extend(picks);
        Ok(())
    }
}

/// A new candidate entry.
fn new_remote(room: RoomName, side: Option<Side>) -> Remote {
    Remote {
        room,
        side,
        ways: None,
        searched: None,
        profit: None,
        crew: None,
        priced: None,
        active: false,
        stats: RemoteStats::default(),
        threat: None,
        abandoned: None,
        threats_at: Vec::new(),
        audit: Audit::default(),
        left_at: None,
    }
}

/// A remote creep's body cost for the audit (P6.3): its role's body in the room's crew (the first
/// source's for a miner or a hauler; a scout's is not the room's). Unit: energy.
fn body_cost(crew: Option<&RemoteCrew>, role: Role) -> u64 {
    let Some(c) = crew else {
        return 0;
    };
    let body = match role {
        Role::Reserver => c.reserver.map(|r| r.0),
        Role::RemoteMiner => c.sources.first().map(|s| s.miner),
        Role::RemoteHauler => c.sources.first().map(|s| s.hauler),
        _ => None,
    };
    body.map_or(0, |b| u64::from(b.cost()))
}

/// The home's spawns: how many, and the tile of the first by id (the ways start next to it).
fn home_spawns(snap: &Snapshot, home: RoomName) -> (u32, Option<Tile>) {
    let st = &snap.structures;
    let mut count = 0u32;
    let mut first: Option<(u128, Tile)> = None;
    for ((&kind, &pos), &h) in st.kinds().iter().zip(st.positions()).zip(st.handles()) {
        if kind != StructureType::Spawn || pos.room_name() != home {
            continue;
        }
        count = count.saturating_add(1);
        let key = st.key_of(h).map_or(u128::MAX, |k| k.0);
        if first.is_none_or(|f| key < f.0) {
            first = Some((key, Tile::of(pos)));
        }
    }
    (count, first.map(|f| f.1))
}

/// Whether the creep at `row` is a remote hauler the home's hauling drives this tick: in its home
/// room with energy (ADR-029's "home sinks").
pub fn is_guest(snap: &Snapshot, row: usize) -> bool {
    let creeps = &snap.creeps;
    let Some(id) = creeps.identities().get(row).copied().flatten() else {
        return false;
    };
    id.role == Role::RemoteHauler
        && creeps.my().get(row).copied().unwrap_or(false)
        && creeps
            .positions()
            .get(row)
            .is_some_and(|p| p.room_name() == id.home)
        && creeps.stores().get(row).is_some_and(|s| s.energy > 0)
}

/// The controller object of a remote room this tick and its reservation's `ticksToEnd`.
fn remote_controller(snap: &Snapshot, room: RoomName) -> Option<(ObjectHandle, u32)> {
    let objects = &snap.objects;
    objects
        .kinds()
        .iter()
        .zip(objects.positions())
        .zip(objects.handles())
        .zip(objects.amounts())
        .find(|&(((&k, &p), _), _)| k == ObjectKind::Controller && p.room_name() == room)
        .map(|(((_, _), &h), &a)| (h, a))
}

/// A loaded hauler's road work at `here`, with `work` WORK parts: the nearest road site within
/// `WORK_RANGE`, else, on a tick the roads were read, the road within range that lacks the most
/// hits, if it lacks a whole repair (`REPAIR_POWER` a WORK part).
///
/// Uses: `REPAIR_POWER`.
fn road_work(snap: &Snapshot, here: Position, energy: u32, work: u32) -> Option<CreepIntent> {
    if work == 0 || energy == 0 {
        return None;
    }
    let objects = &snap.objects;
    let me = Tile::of(here);
    let near =
        |p: Position| p.room_name() == here.room_name() && me.range(Tile::of(p)) <= WORK_RANGE;
    let site = objects
        .kinds()
        .iter()
        .zip(objects.positions())
        .zip(objects.handles())
        .filter(|&((&k, &p), _)| k == ObjectKind::Site(StructureType::Road) && near(p))
        .min_by_key(|&((_, &p), &h)| (me.range(Tile::of(p)), h.slot()))
        .map(|(_, &h)| h);
    if let Some(h) = site {
        return Some(CreepIntent::Build { target: h });
    }
    let whole = work.saturating_mul(REPAIR_POWER);
    objects
        .kinds()
        .iter()
        .zip(objects.positions())
        .zip(objects.handles())
        .zip(objects.hits().iter().zip(objects.totals()))
        .filter(|&(((&k, &p), _), _)| k == ObjectKind::Road && near(p))
        .map(|(((_, _), &h), (&hits, &total))| (h, total.saturating_sub(hits)))
        .filter(|&(_, lack)| lack >= whole)
        .max_by_key(|&(h, lack)| (lack, Reverse(h.slot())))
        .map(|(h, _)| CreepIntent::RepairObject { target: h })
}
