//! Movement (ROADMAP P2): in-room cost grids, flow fields to targets, cached detours that are
//! searched only when a creep is blocked, a traffic manager (swap, shove, priority), and one
//! `move(direction)` intent per moving creep.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Movement::begin`] after the snapshot (no JS call): it notes which rooms have own creeps,
//!    checks last tick's moves (a creep not on the tile it was moved toward counts as stuck) and
//!    drops rooms no own creep has been in for `field_idle_ticks`.
//! 2. In the decide step, two jobs read what movement asks for: [`Movement::first_reads`] (a
//!    Critical job, before every other: terrain, sources and minerals, and the first scan of a room
//!    a creep must move in) and [`Movement::rescans`] (a Standard job: the periodic and early
//!    rescans). Only rooms with own creeps that are owned or had a request in the last
//!    `scan_interval_ticks` are read, so a room creeps only pass through costs nothing until one of
//!    them is asked to move there. The glue hands the reads over ([`Movement::set_terrain`],
//!    [`Movement::set_statics`], [`Movement::set_objects`]).
//! 3. Jobs call [`Movement::request`] in their own tiers: the creep's next steps come from the
//!    goal's flow field (built and cached on first use, a heavy unit for the slicer) or from its
//!    detour. A request that plans a move commits one intent. A job whose creep may make way for
//!    others calls [`Movement::give_way`]; any other own creep stays put (it may be working).
//! 4. [`Movement::resolve`] once at the end of the decide step: per room with movers, the traffic
//!    manager hands each tile to one creep and the moves go into the intent buffer; shoves are
//!    capped by what the requesting job's tier has left ([`ShoveCaps`]).
//! 5. After the act step, [`Movement::moves_refused`] takes the moves the game refused: such a
//!    creep cannot move (no active MOVE part, V50, V108), so it is held as fixed for
//!    `immobile_retry_ticks` instead of being moved into or shoved.
//!
//! What the snapshot cannot see (power creeps, which the engine also blocks on, V52; an object the
//! scan left out) is learned: a tile into which moves keep failing with no creep on it becomes a
//! suspect for `suspect_ticks`, which the traffic manager and detours treat as blocked, and
//! the room is rescanned early once. Reading power creeps in the snapshot comes with P7 and P9.
//!
//! Inter-room routes (P2.2, [`route`]): grids block the room's edge tiles, so in-room moves never
//! take a creep out of its room (V151, V152). A request whose target is in another room follows a
//! route: the creep's own, else a kept route through its room whose exit it can reach, else one the
//! installed [`RouteSearch`] (PathFinder in the game, with the rooms' cost grids as its matrices)
//! finds now, in the request's unit (a heavy unit). The creep walks an `Exit` flow field
//! ([`Aim::Exit`]) onto its leg's exit and the engine moves it to the next room (V151), where
//! [`Movement::begin`] moves it on to the route's next leg and has the room read in the same tick
//! (so it does not stand on the edge and go back). A creep still on an edge of the room it was in
//! had its transfer refused (V152, V168): the room beyond is held closed and every route into it is
//! dropped. A creep that cannot reach its leg's exit searches from where it stands (dropping the
//! route for everyone when the room's grid changed since the search); one that cannot reach its
//! target inside the target's room takes or searches a route out and back in; a failed search is
//! kept by that creep only; a request that finds no way just after a crossing drops the route that
//! led there, and a creep on an edge tile steps into the room instead of going back over the edge.
//!
//! Everything lives in the heap for one global; a reset starts it empty and it rebuilds from the
//! next tick's reads (terrain and a scan per room, fields on first use).

mod field;
mod grid;
mod path;
pub mod route;
mod tile;
mod traffic;

pub use field::{Aim, Dial, FieldCache, FlowField, Goal, STEP_OPTIONS, Steps, UNREACHED};
pub use grid::{
    BLOCKED, CostGrid, CostProfile, Footprint, TerrainGrid, TerrainKind, site_footprint,
    structure_footprint,
};
pub use path::{PathSearch, Query, field_estimate, range_estimate};
pub use route::{Leg, Route, RouteBook, RouteId, RouteOutcome, RouteQuery, RouteSearch};
pub use tile::{DIRECTIONS, Side, Tile};
pub use traffic::{Limits, Plan, Resolved, Traffic, Unit};

use rustedscreep_consts::{INTENT_CPU_COST, Position, ROOM_AREA, ROOM_SIZE, RoomName};

use crate::config::{CacheConfig, MovementConfig};
use crate::intents::{CreepIntent, IntentBuffer};
use crate::scheduler::{Commit, Tier};
use crate::snapshot::{Controller, CreepHandle, Snapshot};

/// Where a creep should go: any walkable tile within `range` of `target`. Higher `priority` moves
/// first when creeps compete for a tile; `tier` is the tier of the job asking, whose CPU pays for
/// the shoves its moves need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveTo {
    pub target: Position,
    pub range: u8,
    pub priority: u8,
    pub tier: Tier,
}

/// What a request did for the creep this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// It is on a walkable tile within range: it holds there, and a mover may shove it only to
    /// another tile within range.
    Arrived,
    /// A step was planned; [`Movement::resolve`] decides whether it moves. On an inter-room route
    /// also a creep that stands on one of its exits: it holds, and the engine moves it to the next
    /// room at the end of the tick (V151).
    Moving,
    /// It is fatigued and cannot move this tick (V50); it holds.
    Tired,
}

/// Why a request planned nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveError {
    /// The creep is not in this tick's snapshot.
    GoneCreep,
    NotMine,
    Spawning,
    /// The target is in another room and no route search is installed ([`Movement::set_router`]).
    OtherRoom,
    /// The room's terrain or objects have not been read yet; the request asks for them, and the
    /// next tick's reads bring them.
    RoomNotReady,
    /// No walkable tile within range can be reached from the creep, or no route leads to the
    /// target's room (cached for `route_retry_ticks`).
    NoPath,
    /// The game refused the creep's last move (no active MOVE part, V108): it is held as fixed
    /// until `immobile_retry_ticks` have passed.
    CannotMove,
}

impl MoveError {
    pub const ALL: [Self; 7] = [
        Self::GoneCreep,
        Self::NotMine,
        Self::Spawning,
        Self::OtherRoom,
        Self::RoomNotReady,
        Self::NoPath,
        Self::CannotMove,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GoneCreep => "gone creep",
            Self::NotMine => "not mine",
            Self::Spawning => "spawning",
            Self::OtherRoom => "other room",
            Self::RoomNotReady => "room not ready",
            Self::NoPath => "no path",
            Self::CannotMove => "cannot move",
        }
    }
}

/// The most shoves each tier's movers may bring the tick's total to: the CPU left before that
/// tier's cap at one clock reading, at `INTENT_CPU_COST` per shove (V34).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShoveCaps {
    by_tier: [u32; 3],
}

impl ShoveCaps {
    /// From the CPU left to each tier, in `Tier::ALL` order. Uses: `INTENT_CPU_COST`.
    pub fn from_cpu_left(left: [f64; 3]) -> Self {
        // `as` saturates and maps NaN to 0.
        Self {
            by_tier: left.map(|l| (l.max(0.0) / INTENT_CPU_COST) as u32),
        }
    }

    pub fn for_tier(&self, tier: Tier) -> u32 {
        let i = match tier {
            Tier::Critical => 0,
            Tier::Standard => 1,
            Tier::Optional => 2,
        };
        self.by_tier.get(i).copied().unwrap_or(0)
    }
}

/// Counters for one tick, and summed since the global started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub requests: u32,
    pub arrived: u32,
    pub moving: u32,
    pub tired: u32,
    pub errors: u32,
    pub fields_built: u32,
    pub detours: u32,
    pub detour_failures: u32,
    /// Detours not searched because no goal tile was free (the creep waits).
    pub detours_skipped: u32,
    /// Movers the traffic manager moved, kept in place, and holding creeps it moved aside.
    pub moved: u32,
    pub blocked: u32,
    pub shoved: u32,
    /// Move intents queued, refused by the intent buffer, and refused by the game afterwards.
    pub pushed: u32,
    pub refused: u32,
    pub refused_by_game: u32,
    pub scans: u32,
    /// Scans whose objects differed from the last scan (each bumps the room's grid version).
    pub changed: u32,
    pub grids_built: u32,
    /// Early rescans asked for: unexplained blocks and `invalidate`.
    pub stale: u32,
    /// Tiles newly suspected of holding something the snapshot cannot see.
    pub suspects: u32,
    /// Route searches made, of them those that found no route, and edge tiles learned from paths
    /// that stepped onto an edge and back (searched again around them).
    pub routes: u32,
    pub route_failures: u32,
    pub route_dips: u32,
    /// Own creeps seen in another room than on the tick before, and transfers the game refused
    /// (an own creep still on an edge of the room it was in; V152).
    pub crossed: u32,
    pub transfers_refused: u32,
}

impl Counts {
    fn add(&mut self, o: &Self) {
        let Self {
            requests,
            arrived,
            moving,
            tired,
            errors,
            fields_built,
            detours,
            detour_failures,
            detours_skipped,
            moved,
            blocked,
            shoved,
            pushed,
            refused,
            refused_by_game,
            scans,
            changed,
            grids_built,
            stale,
            suspects,
            routes,
            route_failures,
            route_dips,
            crossed,
            transfers_refused,
        } = self;
        for (a, b) in [
            (requests, o.requests),
            (arrived, o.arrived),
            (moving, o.moving),
            (tired, o.tired),
            (errors, o.errors),
            (fields_built, o.fields_built),
            (detours, o.detours),
            (detour_failures, o.detour_failures),
            (detours_skipped, o.detours_skipped),
            (moved, o.moved),
            (blocked, o.blocked),
            (shoved, o.shoved),
            (pushed, o.pushed),
            (refused, o.refused),
            (refused_by_game, o.refused_by_game),
            (scans, o.scans),
            (changed, o.changed),
            (grids_built, o.grids_built),
            (stale, o.stale),
            (suspects, o.suspects),
            (routes, o.routes),
            (route_failures, o.route_failures),
            (route_dips, o.route_dips),
            (crossed, o.crossed),
            (transfers_refused, o.transfers_refused),
        ] {
            *a = a.saturating_add(b);
        }
    }
}

/// One room's movement data.
struct RoomNav {
    name: RoomName,
    terrain: Option<TerrainGrid>,
    /// Sources and minerals, read with the terrain once per global (they never move).
    statics: Vec<(Tile, Footprint)>,
    /// The last scan's footprints (structures, own sites, deposits), sorted and without duplicates.
    objects: Vec<(Tile, Footprint)>,
    scanned: Option<u32>,
    /// Allocated on the first build.
    grid: Option<CostGrid>,
    /// Bumped by every rebuild; 0 until the first. Fields carry the version they were built on.
    version: u32,
    built_with: Option<CostProfile>,
    built_at: u32,
    /// The objects or the terrain changed since the grid was built.
    dirty: bool,
    /// Rescan early (after `rescan_min_ticks`).
    stale: bool,
    /// The bot owns the room's controller this tick.
    owned: bool,
    /// An own creep is in the room this tick; the last tick one was.
    active: bool,
    last_active: u32,
    /// The last tick a request was made for a creep in the room.
    wanted_at: Option<u32>,
    /// Move plans this tick.
    movers: u32,
    /// Tiles moves keep failing into with no creep on them, and the tick each stays suspect until:
    /// blocked for traffic and detours meanwhile.
    suspects: Vec<(Tile, u32)>,
}

impl RoomNav {
    fn new(name: RoomName, now: u32) -> Self {
        Self {
            name,
            terrain: None,
            statics: Vec::new(),
            objects: Vec::new(),
            scanned: None,
            grid: None,
            version: 0,
            built_with: None,
            built_at: 0,
            dirty: true,
            stale: false,
            owned: false,
            active: false,
            last_active: now,
            wanted_at: None,
            movers: 0,
            suspects: Vec::new(),
        }
    }

    fn suspect(&self, t: Tile) -> bool {
        self.suspects.iter().any(|&(s, _)| s == t)
    }

    /// Own creeps are in it, and it is owned or had a request within the scan interval: its reads
    /// are worth making.
    fn wanted(&self, now: u32, cfg: &MovementConfig) -> bool {
        self.active
            && (self.owned
                || self
                    .wanted_at
                    .is_some_and(|t| now.wrapping_sub(t) <= cfg.scan_interval_ticks))
    }

    fn ready(&self) -> bool {
        self.terrain.is_some() && self.scanned.is_some()
    }
}

/// One creep's movement state, by handle slot.
#[derive(Default)]
struct Nav {
    handle: Option<CreepHandle>,
    goal: Option<(RoomName, Aim)>,
    /// The room it was in at the last `begin`.
    room: Option<RoomName>,
    /// The room it came from at its last crossing, and the tick it was seen across.
    came_from: Option<RoomName>,
    crossed_at: Option<u32>,
    /// The last tick a request was made for it.
    requested_at: Option<u32>,
    /// The route it follows and the leg it is on (the leg's room is the room it is in).
    route: Option<(RouteId, u16)>,
    /// A route search of its own that found nothing: the room, target and range, and when (kept
    /// for `route_retry_ticks`; never shared, since it says nothing about another creep's tile).
    no_route: Option<(RoomName, Position, u8, u32)>,
    /// The tile it was moved toward last tick.
    expected: Option<Tile>,
    /// Ticks in a row its move failed or the traffic manager kept it in place.
    stuck: u16,
    /// Of those, engine failures into a tile no creep stood on afterwards.
    unexplained: u8,
    /// The tile its last failed move aimed at.
    blocked_at: Option<Tile>,
    /// Detours in a row that found nothing (or were skipped): each doubles the wait before the
    /// next search, up to 16 times `stuck_ticks`.
    detour_fails: u8,
    /// A detour's tiles, the next one last.
    detour: Vec<Tile>,
    /// The game refused its move: held as fixed until this tick.
    immobile_until: Option<u32>,
    /// The field step that held it up when its detour was searched.
    detour_blocker: Option<Tile>,
}

impl Nav {
    fn immobile(&self, now: u32) -> bool {
        self.immobile_until.is_some_and(|t| now < t)
    }
}

/// How a request plans again after its route exit turned out unreachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Retry {
    No,
    /// Leave the creep's own route and take a kept route it can reach, or search one (its exit
    /// cannot be reached from where it stands).
    Search,
    /// Take a kept route through this room, or search one: the target cannot be reached inside
    /// its own room from here, so the way may run through a neighbor.
    Lookup,
    /// Search once more around the edge tiles the last search's path stepped onto and back from.
    Dips,
}

/// What a route lookup is for.
#[derive(Clone, Copy)]
struct RouteAsk {
    creep: CreepHandle,
    pos: Position,
    target: Position,
    range: u8,
    /// Leave aside the creep's own route and its own failure ([`Retry::Search`], [`Retry::Lookup`],
    /// [`Retry::Dips`]).
    fresh: bool,
    /// Search without looking at kept routes ([`Retry::Dips`]).
    search: bool,
}

#[derive(Clone, Copy)]
struct Want {
    plan: Plan,
    priority: u8,
    tier: Tier,
    /// One intent was committed for it this tick.
    committed: bool,
}

/// Tiles with a creep on them in one room this tick, for detour searches.
struct Occupancy {
    mark: Box<[u32; ROOM_AREA]>,
    stamp: u32,
    /// The room and tick the marks are for.
    filled: Option<(RoomName, u32)>,
}

impl Default for Occupancy {
    fn default() -> Self {
        Self {
            mark: Box::new([0; ROOM_AREA]),
            stamp: 0,
            filled: None,
        }
    }
}

impl Occupancy {
    /// Marks the tiles of every creep in `room` (once per room and tick).
    fn fill(&mut self, snap: &Snapshot, room: RoomName) {
        if self.filled == Some((room, snap.tick.time)) {
            return;
        }
        self.filled = Some((room, snap.tick.time));
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.mark.fill(0);
            self.stamp = 1;
        }
        for pos in snap.creeps.positions() {
            if pos.room_name() == room
                && let Some(m) = self.mark.get_mut(Tile::of(*pos).index())
            {
                *m = self.stamp;
            }
        }
    }

    fn has(&self, t: Tile) -> bool {
        self.mark.get(t.index()) == Some(&self.stamp)
    }
}

/// The movement state of one global.
#[derive(Default)]
pub struct Movement {
    /// Sorted by packed room name.
    rooms: Vec<RoomNav>,
    fields: FieldCache,
    navs: Vec<Nav>,
    /// This tick's plan per creep row.
    wants: Vec<Option<Want>>,
    search: PathSearch,
    occupancy: Occupancy,
    traffic: Traffic,
    units: Vec<Unit>,
    unit_rows: Vec<usize>,
    foreign: Vec<Tile>,
    /// Creep rows grouped by room for `resolve`: rows in room order, where each room starts (one
    /// entry more than rooms), each row's room index, and a fill cursor per room.
    by_room: Vec<u32>,
    room_starts: Vec<u32>,
    row_room: Vec<Option<u32>>,
    cursor: Vec<u32>,
    scratch: Vec<(Tile, Footprint)>,
    routes: RouteBook,
    router: Option<Box<dyn RouteSearch>>,
    /// Rooms a creep could not cross into, and the tick each stays closed until.
    closed: Vec<(RoomName, u32)>,
    /// Edge tiles a search's path stepped onto and back into the room from (a creep cannot), and
    /// the tick each is avoided until: every later search's matrix blocks them.
    dips: Vec<(RoomName, Tile, u32)>,
    route_path: Vec<Position>,
    route_legs: Vec<route::Leg>,
    /// The last search's dips: edge tiles its path stepped onto and back into the room from.
    path_dips: Vec<(RoomName, Tile)>,
    /// Candidate route legs (route, leg, exit) for a lookup.
    route_picks: Vec<(RouteId, u16, Tile)>,
    /// The last grid version handed out: versions are unique within a global, so a room dropped
    /// and read again never reuses one (route matrices and fields compare them).
    builds: u32,
    /// `CacheConfig::movement_grid` from the last [`Movement::set_cache`].
    grid_cache: bool,
    time: u32,
    tick: Counts,
    totals: Counts,
}

/// What a route search may know of the rooms: the cost grids movement has built, the rooms held
/// closed after a refused transfer, the rooms the search's caller avoids
/// ([`Movement::search_route`]), and the edge tiles paths must not step onto and back from.
pub struct RouteRooms<'a> {
    rooms: &'a [RoomNav],
    closed: &'a [(RoomName, u32)],
    avoid: &'a [RoomName],
    dips: &'a [(RoomName, Tile, u32)],
}

impl<'a> RouteRooms<'a> {
    /// The edge tiles of `room` a search must treat as blocked (255): a path stepped onto them and
    /// back into the room, which a creep cannot do (V151, V166).
    pub fn dips(&self, room: RoomName) -> impl Iterator<Item = Tile> + 'a {
        self.dips
            .iter()
            .filter(move |&&(r, _, _)| r == room)
            .map(|&(_, t, _)| t)
    }

    /// Rooms with such tiles.
    pub fn dip_rooms(&self) -> impl Iterator<Item = RoomName> + 'a {
        self.dips.iter().map(|&(r, _, _)| r)
    }

    /// Whether the search must not enter `room`: a creep could not cross into it within the last
    /// `route_ttl_ticks`, or the caller avoids it.
    pub fn closed(&self, room: RoomName) -> bool {
        self.closed.iter().any(|&(r, _)| r == room) || self.avoid.contains(&room)
    }

    /// The rooms held closed, then the avoided ones.
    pub fn closed_rooms(&self) -> impl Iterator<Item = RoomName> + 'a {
        self.closed
            .iter()
            .map(|&(r, _)| r)
            .chain(self.avoid.iter().copied())
    }

    /// The room's cost grid and its version, if movement has built one.
    pub fn grid(&self, room: RoomName) -> Option<(&'a CostGrid, u32)> {
        let i = self
            .rooms
            .binary_search_by_key(&room.packed_repr(), |r| r.name.packed_repr())
            .ok()?;
        let r = self.rooms.get(i)?;
        r.grid
            .as_ref()
            .filter(|_| r.version != 0)
            .map(|g| (g, r.version))
    }

    /// Every room with a cost grid, with its version, in room order.
    pub fn grids(&self) -> impl Iterator<Item = (RoomName, u32, &'a CostGrid)> + 'a {
        self.rooms.iter().filter_map(|r| {
            r.grid
                .as_ref()
                .filter(|_| r.version != 0)
                .map(|g| (r.name, r.version, g))
        })
    }
}

/// The cost profile in force.
pub fn profile(cfg: &MovementConfig) -> CostProfile {
    CostProfile {
        road: cfg.road_cost,
        plain: cfg.plain_cost,
        swamp: cfg.swamp_cost,
    }
    .sanitized()
}

impl Movement {
    /// Starts a tick for `snap` (already taken): this tick's rooms, last tick's moves checked, rooms
    /// and fields not used for `field_idle_ticks` dropped. No JS call.
    pub fn begin(&mut self, snap: &Snapshot, cfg: &MovementConfig) {
        let now = snap.tick.time;
        self.time = now;
        self.tick = Counts::default();
        let creeps = &snap.creeps;
        self.wants.clear();
        self.wants.resize(creeps.len(), None);
        for r in &mut self.rooms {
            r.active = false;
            r.owned = false;
            r.movers = 0;
        }
        for row in 0..creeps.len() {
            let (Some(&h), Some(&pos), Some(&my), Some(&spawning)) = (
                creeps.handles().get(row),
                creeps.positions().get(row),
                creeps.my().get(row),
                creeps.spawning().get(row),
            ) else {
                continue;
            };
            if !my {
                continue;
            }
            let room = pos.room_name();
            let ri = self.room_or_insert(room, now);
            if let Some(r) = self.rooms.get_mut(ri) {
                r.active = true;
                r.last_active = now;
            }
            if spawning {
                continue;
            }
            let Some(nav) = nav_for(&mut self.navs, h) else {
                continue;
            };
            let at = Tile::of(pos);
            let crossed = nav.room.is_some_and(|r| r != room);
            // A creep that ends a tick on an edge tile is in the next room on the next tick (V151),
            // so one still on an edge of the room it was in had its transfer refused (V152, V168).
            let refused = nav.room == Some(room) && at.is_edge();
            if crossed {
                nav.came_from = nav.room;
                nav.crossed_at = Some(now);
                // On to the route's next leg if this is its room; off the route otherwise.
                let routes = &mut self.routes;
                nav.route = nav.route.and_then(|(id, leg)| {
                    let next = leg.checked_add(1)?;
                    let r = routes.get(id, now)?;
                    (r.legs.get(usize::from(next))?.room == room).then_some((id, next))
                });
            }
            nav.room = Some(room);
            if let Some(expected) = nav.expected.take() {
                if at == expected || crossed {
                    nav.stuck = 0;
                    nav.unexplained = 0;
                    nav.blocked_at = None;
                    nav.detour_fails = 0;
                    if nav.detour.last() == Some(&at) {
                        nav.detour.pop();
                    }
                } else {
                    // The engine dropped the move. A creep standing there now explains it; an empty
                    // tile may be an object the grid lacks.
                    let explained = creeps
                        .positions()
                        .iter()
                        .any(|p| p.room_name() == room && Tile::of(*p) == expected);
                    nav.stuck = nav.stuck.saturating_add(1);
                    nav.unexplained = if explained {
                        0
                    } else {
                        nav.unexplained.saturating_add(1)
                    };
                    nav.blocked_at = Some(expected);
                }
            }
            if crossed {
                nav.detour.clear();
                nav.detour_blocker = None;
                self.tick.crossed += 1;
                // It walks a route: read its new room in this tick (the read jobs run before the
                // requests), so it does not stand on the edge and go back (V151).
                if nav.requested_at.is_some_and(|t| now.wrapping_sub(t) <= 1)
                    && let Some(r) = self.rooms.get_mut(ri)
                {
                    r.wanted_at = Some(now);
                }
            }
            if refused
                && let Some(side) = at.side()
                && let Some(beyond) = room.checked_add(side.room_offset())
            {
                let until = now.saturating_add(cfg.route_ttl_ticks.max(1));
                match self.closed.iter_mut().find(|(r, _)| *r == beyond) {
                    Some(c) => c.1 = until,
                    None => self.closed.push((beyond, until)),
                }
                // Every route into the closed room goes; its creeps search again around it.
                self.routes.remove_through(beyond);
                nav.route = None;
                self.tick.transfers_refused += 1;
            }
        }
        for (&name, controller) in snap.rooms.names().iter().zip(snap.rooms.controllers()) {
            if matches!(controller, Controller::Mine { .. })
                && let Some(r) = self.room_mut(name)
            {
                r.owned = true;
            }
        }
        // Own structures built or destroyed need no early rescan: a site of an obstacle type is
        // already blocked and its structure is too, ramparts and the other non-obstacles change
        // nothing, and a destroyed obstacle leaves the grid only more cautious until the next scan.
        for r in &mut self.rooms {
            r.suspects.retain(|&(_, until)| now < until);
        }
        self.closed.retain(|&(_, until)| now < until);
        self.dips.retain(|&(_, _, until)| now < until);
        if !self.routes.is_empty() {
            self.routes.evict(now, route_ages(cfg).0);
        }
        let idle = cfg.field_idle_ticks;
        self.rooms
            .retain(|r| r.active || now.wrapping_sub(r.last_active) <= idle);
        self.fields.evict_idle(now, idle);
    }

    /// Applies this tick's cache toggles and field cap; call after [`Self::begin`].
    pub fn set_cache(&mut self, cfg: &MovementConfig, cache: &CacheConfig) {
        let cap = usize::try_from(cfg.field_cache_size).unwrap_or(usize::MAX);
        self.fields.set_policy(cap, cache.flow_fields);
        self.grid_cache = cache.movement_grid;
        self.routes.set_capacity(
            usize::try_from(cfg.route_cache_size).unwrap_or(usize::MAX),
            self.time,
        );
    }

    /// Installs the search inter-room routes use (PathFinder, from rustedscreep-wasm). Without one, a
    /// target in another room is refused ([`MoveError::OtherRoom`]).
    pub fn set_router(&mut self, router: Box<dyn RouteSearch>) {
        self.router = Some(router);
    }

    /// Whether a route search is installed ([`Self::set_router`]).
    pub fn has_router(&self) -> bool {
        self.router.is_some()
    }

    /// Rooms whose first read is due (terrain, sources and minerals, and a scan): wanted and not
    /// read yet, or every wanted room with the grid cache off (the naive baseline). Sorted by
    /// name, for the glue's Critical job.
    pub fn first_reads(&self, cfg: &MovementConfig, out: &mut Vec<RoomName>) {
        let now = self.time;
        out.clear();
        out.extend(
            self.rooms
                .iter()
                .filter(|r| r.wanted(now, cfg) && (!r.ready() || !self.grid_cache))
                .map(|r| r.name),
        );
    }

    /// Rooms whose rescan is due: read before, wanted, and last scanned `scan_interval_ticks` ago,
    /// or `rescan_min_ticks` ago when an early rescan was asked for. Sorted by name, for the glue's
    /// Standard job. None with the grid cache off (the first reads cover every tick then).
    pub fn rescans(&self, cfg: &MovementConfig, out: &mut Vec<RoomName>) {
        let now = self.time;
        out.clear();
        if !self.grid_cache {
            return;
        }
        out.extend(
            self.rooms
                .iter()
                .filter(|r| r.wanted(now, cfg) && r.ready())
                .filter(|r| {
                    let age = r.scanned.map_or(u32::MAX, |at| now.wrapping_sub(at));
                    age >= cfg.scan_interval_ticks || (r.stale && age >= cfg.rescan_min_ticks)
                })
                .map(|r| r.name),
        );
    }

    /// Stores `room`'s terrain from `Room.Terrain.getRawBuffer()`'s bytes (row-major, V156).
    /// Returns false (and keeps what it had) when the length is wrong or the room is not tracked.
    pub fn set_terrain(&mut self, room: RoomName, raw: &[u8]) -> bool {
        let Some(terrain) = TerrainGrid::from_row_major(raw) else {
            return false;
        };
        let Some(r) = self.room_mut(room) else {
            return false;
        };
        r.terrain = Some(terrain);
        r.dirty = true;
        true
    }

    /// Stores `room`'s sources and minerals (any order), read with its terrain.
    pub fn set_statics(&mut self, room: RoomName, blockers: impl IntoIterator<Item = Tile>) {
        self.scratch.clear();
        self.scratch
            .extend(blockers.into_iter().map(|t| (t, Footprint::Blocked)));
        self.scratch.sort_unstable_by_key(|&(t, _)| t);
        self.scratch.dedup();
        let Self { rooms, scratch, .. } = self;
        let Some(r) = find_room(rooms, room) else {
            return;
        };
        if r.statics != *scratch {
            std::mem::swap(&mut r.statics, scratch);
            r.dirty = true;
        }
    }

    /// Stores a scan of `room`: the footprints of its structures, own sites and deposits (any
    /// order, duplicates allowed). The grid is rebuilt only when they differ from the last scan's.
    pub fn set_objects(
        &mut self,
        room: RoomName,
        objects: impl IntoIterator<Item = (Tile, Footprint)>,
    ) {
        self.scratch.clear();
        self.scratch.extend(objects);
        self.scratch
            .sort_unstable_by_key(|&(t, f)| (t, f == Footprint::Blocked));
        self.scratch.dedup();
        let now = self.time;
        let Self {
            rooms,
            scratch,
            tick,
            ..
        } = self;
        let Some(r) = find_room(rooms, room) else {
            return;
        };
        if r.objects != *scratch || r.scanned.is_none() {
            std::mem::swap(&mut r.objects, scratch);
            r.dirty = true;
            tick.changed += 1;
        }
        r.scanned = Some(now);
        r.stale = false;
        tick.scans += 1;
    }

    /// Asks for an early rescan of `room` (after `rescan_min_ticks`), for example after the bot
    /// placed a construction site there.
    pub fn invalidate(&mut self, room: RoomName) {
        if let Some(r) = self.room_mut(room) {
            r.stale = true;
            self.tick.stale += 1;
        }
    }

    /// Plans a step for `creep` toward `to`, or holds it there once it arrived. A planned move
    /// commits one intent (`INTENT_CPU_COST`, V34) the first time in a tick; asking again in the
    /// same tick replaces the plan. Building a field or searching a detour marks the unit heavy
    /// ([`Commit::heavy`]). Field caching follows the last [`Self::set_cache`].
    pub fn request(
        &mut self,
        snap: &Snapshot,
        creep: CreepHandle,
        to: MoveTo,
        cfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<Progress, MoveError> {
        self.tick.requests += 1;
        let mut retry = Retry::No;
        let mut result = self.plan(snap, creep, to, cfg, commit, &mut retry);
        // At most two more plans: one that looks up or searches a route from where the creep stands
        // (its exit or target cannot be reached from there), and one more search around edge tiles
        // that search learned. So a creep that just came in does not stand on the edge and go back.
        for _ in 0..2 {
            if retry == Retry::No {
                break;
            }
            result = self.plan(snap, creep, to, cfg, commit, &mut retry);
        }
        if result == Err(MoveError::NoPath) {
            self.no_way(snap, creep, to, commit);
        }
        match result {
            Ok(Progress::Arrived) => self.tick.arrived += 1,
            Ok(Progress::Moving) => self.tick.moving += 1,
            Ok(Progress::Tired) => self.tick.tired += 1,
            Err(_) => self.tick.errors += 1,
        }
        result
    }

    /// Lets `creep` make way this tick: a mover may shove it to an adjacent tile within `range` of
    /// `within` (anywhere when `None`). It replaces any plan the creep had this tick. Without this
    /// or a request, an own creep stays put. Nothing is committed: a shove commits at
    /// [`Self::resolve`].
    pub fn give_way(
        &mut self,
        snap: &Snapshot,
        creep: CreepHandle,
        within: Option<(Position, u8)>,
    ) {
        let Some(row) = snap.creeps.row_of(creep) else {
            return;
        };
        let anchor = within.map(|(p, range)| Goal {
            tile: Tile::of(p),
            range,
        });
        let mut nothing = Commit::default();
        set_want(
            &mut self.wants,
            row,
            Plan::Hold(anchor),
            0,
            Tier::Optional,
            &mut nothing,
        );
    }

    /// A route search for a job that must know a creep's way before it sends the creep (ADR-030's
    /// signer): from `from` to within `range` of `to`, with the job's own PathFinder limits, never
    /// entering a room in `avoid` (as if held closed; the target's room must not be in it). The path
    /// goes into `path` (the tiles after `from`, the last within range). A path found across rooms
    /// is also kept as a route ([`RouteBook`]), so a creep in `from`'s room that then asks to go to
    /// `to` within `range` follows it. Marks the unit heavy. `None` without an installed router.
    #[allow(clippy::too_many_arguments)]
    pub fn search_route(
        &mut self,
        from: Position,
        to: Position,
        range: u8,
        limits: (u32, u8),
        avoid: &[RoomName],
        cfg: &MovementConfig,
        commit: &mut Commit,
        path: &mut Vec<Position>,
    ) -> Option<RouteOutcome> {
        let now = self.time;
        let Self {
            rooms,
            closed,
            dips,
            router,
            routes,
            route_legs: legs,
            path_dips,
            tick,
            ..
        } = self;
        let router = router.as_mut()?;
        let p = profile(cfg);
        let query = RouteQuery {
            from,
            to,
            range: route::pathfinder_range(to, range),
            plain_cost: p.plain,
            swamp_cost: p.swamp,
            max_ops: limits.0,
            max_rooms: limits.1,
        };
        commit.heavy();
        tick.routes += 1;
        let known = RouteRooms {
            rooms: rooms.as_slice(),
            closed: closed.as_slice(),
            avoid,
            dips: dips.as_slice(),
        };
        let outcome = router.search(&query, &known, path);
        if outcome != RouteOutcome::Found {
            tick.route_failures += 1;
            return Some(outcome);
        }
        if to.room_name() != from.room_name()
            && route::route_legs(from, path, to.room_name(), legs, path_dips)
        {
            for leg in legs.iter_mut() {
                leg.version = known.grid(leg.room).map_or(0, |(_, v)| v);
            }
            routes.add(to, range, legs, now);
        }
        Some(outcome)
    }

    /// After a request found no way (`NoPath`): a creep that crossed into its room this tick drops
    /// the route that led it here, so no creep follows it again (the next search knows this room's
    /// grid); and a creep on an edge tile, which would go back over it at the end of the tick
    /// (V151), steps into the room instead.
    fn no_way(&mut self, snap: &Snapshot, creep: CreepHandle, to: MoveTo, commit: &mut Commit) {
        let now = self.time;
        let creeps = &snap.creeps;
        let (Some(row), Some(nav)) = (creeps.row_of(creep), nav_for(&mut self.navs, creep)) else {
            return;
        };
        let Some(&pos) = creeps.positions().get(row) else {
            return;
        };
        let room = pos.room_name();
        if nav.crossed_at == Some(now)
            && let Some((id, _)) = nav.route.take()
        {
            // The route led into a room with no way on from here: no creep takes it again.
            self.routes.remove(id);
        }
        let from = Tile::of(pos);
        if !from.is_edge() || nav.immobile(now) {
            return;
        }
        let Some(r) = find_room(&mut self.rooms, room) else {
            return;
        };
        let Some(grid) = r.grid.as_ref() else {
            return;
        };
        let steps = inward_steps(grid, from);
        if steps.is_empty() {
            return;
        }
        let first = !self
            .wants
            .get(row)
            .copied()
            .flatten()
            .is_some_and(|w| matches!(w.plan, Plan::Move(_)));
        set_want(
            &mut self.wants,
            row,
            Plan::Move(steps),
            to.priority,
            to.tier,
            commit,
        );
        if first {
            r.movers += 1;
        }
    }

    /// One request's plan. `retry` says how to search (a second plan, after the first set it) and
    /// is set when the route's exit turned out unreachable, so [`Self::request`] plans once more.
    fn plan(
        &mut self,
        snap: &Snapshot,
        creep: CreepHandle,
        to: MoveTo,
        cfg: &MovementConfig,
        commit: &mut Commit,
        retry: &mut Retry,
    ) -> Result<Progress, MoveError> {
        let again = *retry;
        *retry = Retry::No;
        let MoveTo {
            target,
            range,
            priority,
            tier,
        } = to;
        let creeps = &snap.creeps;
        let row = creeps.row_of(creep).ok_or(MoveError::GoneCreep)?;
        if !creeps.my().get(row).copied().unwrap_or(false) {
            return Err(MoveError::NotMine);
        }
        if creeps.spawning().get(row).copied().unwrap_or(true) {
            return Err(MoveError::Spawning);
        }
        let pos = creeps
            .positions()
            .get(row)
            .copied()
            .ok_or(MoveError::GoneCreep)?;
        let room = pos.room_name();
        let now = self.time;
        let from = Tile::of(pos);
        let elsewhere = target.room_name() != room;
        if elsewhere && self.router.is_none() {
            set_want(&mut self.wants, row, pinned(from), priority, tier, commit);
            return Err(MoveError::OtherRoom);
        }
        if let Some(nav) = nav_for(&mut self.navs, creep) {
            nav.requested_at = Some(now);
        }
        if let Some(r) = self.room_mut(room) {
            r.wanted_at = Some(now);
        }
        let ri = self.room_index(room).ok_or(MoveError::RoomNotReady)?;
        if !self.ensure_grid(ri, cfg) {
            set_want(&mut self.wants, row, pinned(from), priority, tier, commit);
            return Err(MoveError::RoomNotReady);
        }
        // Whether this request searched the route (its exit is then as good as the room's grid).
        let mut searched = false;
        let near = Aim::Near(Goal {
            tile: Tile::of(target),
            range: range.min(ROOM_SIZE - 1),
        });
        let has_router = self.router.is_some();
        // A route is followed toward a target in another room, and in the target's room too while
        // the creep's route has legs left (a path that leaves the room and comes back).
        let routed = has_router
            && (elsewhere || again != Retry::No || self.mid_route(creep, target, range, room));
        let aim = if routed {
            let ask = RouteAsk {
                creep,
                pos,
                target,
                range,
                fresh: again != Retry::No,
                search: again == Retry::Dips,
            };
            match self.route_exit(ask, cfg, commit, &mut searched) {
                // The route's own exit tile only: a tile beside it may lead into a pocket of the
                // next room that the search did not take.
                Ok(Some(exit)) => Aim::Exit(Goal {
                    tile: exit,
                    range: 0,
                }),
                Ok(None) => near,
                Err(e) => {
                    set_want(&mut self.wants, row, pinned(from), priority, tier, commit);
                    return Err(e);
                }
            }
        } else {
            near
        };
        let Self {
            rooms,
            fields,
            navs,
            wants,
            search,
            occupancy,
            tick,
            routes,
            dips,
            path_dips,
            ..
        } = self;
        let r = rooms.get_mut(ri).ok_or(MoveError::RoomNotReady)?;
        let Some(grid) = r.grid.as_ref() else {
            return Err(MoveError::RoomNotReady);
        };
        let nav = nav_for(navs, creep).ok_or(MoveError::GoneCreep)?;
        if nav.goal != Some((room, aim)) {
            nav.goal = Some((room, aim));
            nav.detour.clear();
            nav.stuck = 0;
            nav.unexplained = 0;
            nav.blocked_at = None;
            nav.detour_fails = 0;
        }
        if aim.reaches(grid, from) {
            nav.detour.clear();
            nav.stuck = 0;
            return match aim {
                Aim::Near(goal) => {
                    // A creep held as fixed stays put whatever its anchor says.
                    let plan = if nav.immobile(now) {
                        pinned(from)
                    } else {
                        Plan::Hold(Some(goal))
                    };
                    set_want(wants, row, plan, priority, tier, commit);
                    Ok(Progress::Arrived)
                }
                Aim::Exit(_) => {
                    // On one of its exits already (back over the edge, or its transfer was
                    // refused): the engine moves it to the next room at the end of the tick if it
                    // stays (V151).
                    set_want(wants, row, pinned(from), priority, tier, commit);
                    Ok(Progress::Moving)
                }
            };
        }
        if nav.immobile(now) {
            set_want(wants, row, pinned(from), priority, tier, commit);
            return Err(MoveError::CannotMove);
        }
        if creeps.fatigue().get(row).copied().unwrap_or(0) > 0 {
            set_want(wants, row, pinned(from), priority, tier, commit);
            return Ok(Progress::Tired);
        }
        let (field, built) = fields.get(room, aim, grid, r.version, now);
        if built {
            tick.fields_built += 1;
            commit.heavy();
        }
        let stuck_ticks = cfg.stuck_ticks.max(1);
        let wait = u32::from(stuck_ticks) << nav.detour_fails.min(4);
        let mut stale = false;
        let mut suspect = None;
        let mut detoured = false;
        if u32::from(nav.stuck) >= wait {
            nav.stuck = 0;
            if nav.unexplained >= stuck_ticks
                && let Some(b) = nav.blocked_at
                && grid.walkable(b)
                && !r.suspect(b)
            {
                // Engine failures into a tile no creep stood on: something the snapshot does not
                // see is there (a site or structure placed since the scan, a power creep, V52).
                // Treat the tile as blocked for a while, and scan early once in case it is an
                // object.
                suspect = Some(b);
                stale = true;
            }
            nav.unexplained = 0;
            nav.blocked_at = None;
            occupancy.fill(snap, room);
            let occ: &Occupancy = &*occupancy;
            let suspects = &r.suspects;
            let blocked = |t: Tile| {
                t != from
                    && (occ.has(t) || Some(t) == suspect || suspects.iter().any(|&(s, _)| s == t))
            };
            if !aim.tiles(grid).any(|t| !blocked(t)) {
                tick.detours_skipped += 1;
                nav.detour_fails = nav.detour_fails.saturating_add(1);
            } else {
                commit.heavy();
                let query = Query {
                    grid,
                    blocked,
                    estimate: field_estimate(field),
                    start: from,
                    goal: aim,
                    max_pops: cfg.detour_max_pops,
                };
                if search.search(query, &mut nav.detour) {
                    tick.detours += 1;
                    detoured = true;
                    // The field's own next step is the tile that held the creep up.
                    nav.detour_blocker = field.steps(grid, from).first();
                } else {
                    tick.detour_failures += 1;
                    nav.detour_fails = nav.detour_fails.saturating_add(1);
                }
            }
        }
        // Back to the field once the tile that blocked it is free again, so a jam that cleared does
        // not send the creep the long way round (a free next field step is not enough: the blocker
        // may stand further on). Only while the creep is still near that tile: once it has gone some
        // way round, a gap that opens and closes again would make it shuttle back and forth, and the
        // detour is kept. Never on the tick the detour was searched.
        if !detoured
            && let Some(&next) = nav.detour.last()
            && from.range(next) == 1
            && let Some(blocker) = nav.detour_blocker
            && from.range(blocker) <= DETOUR_DROP_RANGE
        {
            occupancy.fill(snap, room);
            let best = field.steps(grid, from);
            if !occupancy.has(blocker)
                && !r.suspect(blocker)
                && Some(blocker) != suspect
                && best.first().is_some_and(|t| t != next)
            {
                nav.detour.clear();
            }
        }
        if nav.detour.is_empty() {
            nav.detour_blocker = None;
        }
        let next = nav.detour.last().copied();
        let steps = match next {
            Some(t) if from.range(t) == 1 => Steps::from_tiles([t], 0),
            _ => {
                nav.detour.clear();
                if from.is_edge() {
                    // It leaves the room if it stays (V151): every way in, best first, not only the
                    // best ties, so one creep on the best tile does not send it back.
                    field.ranked_steps(grid, from)
                } else {
                    field.steps(grid, from)
                }
            }
        };
        if let Some(b) = suspect {
            r.suspects
                .push((b, now.saturating_add(cfg.suspect_ticks.max(1))));
            tick.suspects += 1;
        }
        if stale {
            r.stale = true;
            tick.stale += 1;
        }
        if steps.is_empty() {
            let rate_limited = nav.no_route.is_some_and(|(r, t, g, at)| {
                r == room
                    && t == target
                    && g == range
                    && now.wrapping_sub(at) <= cfg.route_retry_ticks.max(1)
            });
            if let Aim::Exit(_) = aim {
                // The room's own grid cannot reach the route's exit from here.
                if searched {
                    // This request's own search gave it, so no creep takes that route. Its path
                    // may have stepped onto an edge of this room and back to get past something
                    // (PathFinder allows it, V166; no creep can, V151): those tiles are avoided
                    // from now on, and the search runs once more around them. Otherwise no way
                    // for this creep for `route_retry_ticks`.
                    if let Some((id, _)) = nav.route.take() {
                        routes.remove(id);
                    }
                    let until = now.saturating_add(cfg.route_ttl_ticks.max(1));
                    let mut learned = false;
                    for &(dr, dt) in path_dips.iter().filter(|&&(dr, _)| dr == room) {
                        if !dips.iter().any(|&(r0, t0, _)| r0 == dr && t0 == dt) {
                            dips.push((dr, dt, until));
                            tick.route_dips += 1;
                            learned = true;
                        }
                    }
                    path_dips.clear();
                    if learned && again != Retry::Dips {
                        *retry = Retry::Dips;
                    } else {
                        nav.no_route = Some((room, target, range, now));
                        tick.route_failures += 1;
                    }
                } else if again == Retry::No {
                    // A route found on an older grid of this room is stale for every creep on it,
                    // and so is one that landed this creep here: every creep on it lands on the
                    // same tile. One found on this grid that the creep only joined is right where
                    // its path starts. Either way, take another route or search from here at once.
                    if let Some((id, leg)) = nav.route.take()
                        && (nav.crossed_at == Some(now)
                            || routes
                                .peek(id)
                                .and_then(|rt| rt.legs.get(usize::from(leg)))
                                .is_some_and(|l| l.version != r.version))
                    {
                        routes.remove(id);
                    }
                    *retry = Retry::Search;
                } else {
                    // A kept route taken on a retry turned out out of reach after all.
                    nav.route = None;
                    nav.no_route = Some((room, target, range, now));
                    tick.route_failures += 1;
                }
            } else if has_router
                && again == Retry::No
                && !rate_limited
                && reaches_an_exit(field, grid)
            {
                // No way to the target inside its room from here, but the target can be reached from
                // an exit: the way may run through a neighbour. Take a kept route through this room,
                // or search one.
                *retry = Retry::Lookup;
            } else if has_router && searched {
                nav.no_route = Some((room, target, range, now));
            }
            set_want(wants, row, pinned(from), priority, tier, commit);
            return Err(MoveError::NoPath);
        }
        let first = !wants
            .get(row)
            .copied()
            .flatten()
            .is_some_and(|w| matches!(w.plan, Plan::Move(_)));
        set_want(wants, row, Plan::Move(steps), priority, tier, commit);
        if first {
            r.movers += 1;
        }
        Ok(Progress::Moving)
    }

    /// Resolves the traffic of every room with movers (in room-name order) and queues the moves.
    /// Each mover's chains may bring the tick's shoves up to `caps` for its job's tier; a shove
    /// commits one intent on `commit`, while a mover's intent was committed by its request (one that
    /// stays in place keeps that commit, an upper bound). Call once per tick, after every request
    /// and before the intent buffer resolves.
    pub fn resolve(
        &mut self,
        snap: &Snapshot,
        buffer: &mut IntentBuffer,
        cfg: &MovementConfig,
        caps: ShoveCaps,
        commit: &mut Commit,
    ) {
        if self.rooms.iter().any(|r| r.movers > 0) {
            self.group_rows(snap);
        }
        let Self {
            rooms,
            navs,
            wants,
            traffic,
            units,
            unit_rows,
            foreign,
            by_room,
            room_starts,
            tick,
            totals,
            ..
        } = self;
        let creeps = &snap.creeps;
        let now = snap.tick.time;
        let mut shoves = 0u32;
        for (ri, r) in rooms.iter().enumerate() {
            if r.movers == 0 {
                continue;
            }
            let Some(grid) = r.grid.as_ref() else {
                continue;
            };
            let (Some(&start), Some(&end)) = (room_starts.get(ri), room_starts.get(ri + 1)) else {
                continue;
            };
            units.clear();
            unit_rows.clear();
            foreign.clear();
            foreign.extend(r.suspects.iter().map(|&(t, _)| t));
            for &row in by_room.get(start as usize..end as usize).unwrap_or(&[]) {
                let row = row as usize;
                let Some(&pos) = creeps.positions().get(row) else {
                    continue;
                };
                let at = Tile::of(pos);
                if !creeps.my().get(row).copied().unwrap_or(false) {
                    foreign.push(at);
                    continue;
                }
                if creeps.spawning().get(row).copied().unwrap_or(true) {
                    continue;
                }
                let want = wants.get(row).copied().flatten();
                let order = creeps
                    .handles()
                    .get(row)
                    .and_then(|&h| creeps.key_of(h))
                    .map_or(u128::MAX, |k| k.0);
                let immobile = creeps
                    .handles()
                    .get(row)
                    .and_then(|&h| nav_for(navs, h))
                    .is_some_and(|n| n.immobile(now));
                units.push(Unit {
                    tile: at,
                    can_move: !immobile && creeps.fatigue().get(row).copied().unwrap_or(1) == 0,
                    plan: want.map_or(pinned(at), |w| w.plan),
                    priority: want.map_or(0, |w| w.priority),
                    order,
                    shove_cap: want.map_or(0, |w| caps.for_tier(w.tier)),
                });
                unit_rows.push(row);
            }
            let limits = Limits {
                depth: cfg.shove_depth,
                claims: cfg.shove_claims,
                shoves_before: shoves,
            };
            let res = traffic.resolve(grid, units, foreign, limits);
            shoves = shoves.saturating_add(res.shoved);
            tick.moved += res.moved;
            tick.blocked += res.blocked;
            tick.shoved += res.shoved;
            for (i, (u, &row)) in units.iter().zip(unit_rows.iter()).enumerate() {
                let Some(&h) = creeps.handles().get(row) else {
                    continue;
                };
                let to = traffic.assigned().get(i).copied().unwrap_or(u.tile);
                let mover = matches!(u.plan, Plan::Move(_));
                if to == u.tile {
                    // Kept in place by the traffic manager: a creep was in the way, so this is
                    // stuck but explained.
                    if mover
                        && u.can_move
                        && let Some(nav) = nav_for(navs, h)
                    {
                        nav.stuck = nav.stuck.saturating_add(1);
                        nav.unexplained = 0;
                    }
                    continue;
                }
                let Some(direction) = u.tile.direction_to(to) else {
                    continue;
                };
                let shoved = traffic.shoved(i);
                let mut paid = Commit::default();
                let c = if shoved { &mut *commit } else { &mut paid };
                match buffer.push(snap, h, CreepIntent::Move { direction }, c) {
                    Ok(_) => {
                        tick.pushed += 1;
                        if let (true, Some(nav)) = (mover, nav_for(navs, h)) {
                            nav.expected = Some(to);
                        }
                    }
                    Err(_) => tick.refused += 1,
                }
            }
        }
        totals.add(tick);
    }

    /// After the act step: the creeps in these snapshot rows had their move refused by the game
    /// (an error code or a throw), so the move does not count as a failure next tick. Those with
    /// `no_move_part` set (`ERR_NO_BODYPART` for a `move(direction)`, V108) are also held as fixed
    /// for `immobile_retry_ticks`, since that does not pass by itself.
    pub fn moves_refused(
        &mut self,
        snap: &Snapshot,
        rows: impl IntoIterator<Item = (u32, bool)>,
        cfg: &MovementConfig,
    ) {
        let until = snap
            .tick
            .time
            .saturating_add(cfg.immobile_retry_ticks.max(1));
        for (row, no_move_part) in rows {
            let Some(&h) = snap.creeps.handles().get(row as usize) else {
                continue;
            };
            if let Some(nav) = nav_for(&mut self.navs, h) {
                nav.expected = None;
                nav.unexplained = 0;
                if no_move_part {
                    nav.stuck = 0;
                    nav.immobile_until = Some(until);
                }
                self.tick.refused_by_game += 1;
                self.totals.refused_by_game = self.totals.refused_by_game.saturating_add(1);
            }
        }
    }

    /// Whether any creep has a move planned this tick (requests that move, and creeps stepping off
    /// an edge after finding no way), so the glue knows to read the clock for the shove caps.
    pub fn has_movers(&self) -> bool {
        self.rooms.iter().any(|r| r.movers > 0)
    }

    /// Whether the creep in snapshot row `row` was requested (or given way) this tick, so a
    /// lower-priority source of requests can leave it alone.
    pub fn planned(&self, row: usize) -> bool {
        self.wants.get(row).is_some_and(Option::is_some)
    }

    /// This tick's counters (complete once [`Self::resolve`] ran).
    pub fn tick(&self) -> &Counts {
        &self.tick
    }

    /// Counters since the global started, this tick's included once it resolved.
    pub fn totals(&self) -> &Counts {
        &self.totals
    }

    /// Rooms tracked; of them with terrain read, active, and wanted this tick.
    pub fn room_counts(&self, cfg: &MovementConfig) -> (usize, usize, usize, usize) {
        let now = self.time;
        (
            self.rooms.len(),
            self.rooms.iter().filter(|r| r.terrain.is_some()).count(),
            self.rooms.iter().filter(|r| r.active).count(),
            self.rooms.iter().filter(|r| r.wanted(now, cfg)).count(),
        )
    }

    /// Every tracked room's name, grid version, last scan and object count, for the console.
    pub fn room_states(&self) -> impl Iterator<Item = (RoomName, u32, Option<u32>, usize)> + '_ {
        self.rooms.iter().map(|r| {
            (
                r.name,
                r.version,
                r.scanned,
                r.objects.len() + r.statics.len(),
            )
        })
    }

    /// The cost grid of `room` and its version, for planners that measure paths on it (the P2.4
    /// opening sizes its workers from distances on it). It is built first when it is out of date
    /// (a heavy unit, [`Commit::heavy`]), as for a request. `None` until the room's terrain and a
    /// scan have been read: [`Self::request`] asks for them, so a caller retries a tick later.
    pub fn room_grid(
        &mut self,
        room: RoomName,
        cfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Option<(&CostGrid, u32)> {
        let ri = self.room_index(room)?;
        let builds = self.tick.grids_built;
        if !self.ensure_grid(ri, cfg) {
            return None;
        }
        if self.tick.grids_built != builds {
            commit.heavy();
        }
        let r = self.rooms.get(ri)?;
        r.grid.as_ref().map(|g| (g, r.version))
    }

    /// The tiles of `room`'s sources and minerals, read with its terrain (none before that), for
    /// planners that keep their neighbors free (P2.4's extension placement).
    pub fn room_statics(&self, room: RoomName) -> impl Iterator<Item = Tile> + '_ {
        self.room_index(room)
            .and_then(|i| self.rooms.get(i))
            .into_iter()
            .flat_map(|r| r.statics.iter().map(|&(t, _)| t))
    }

    /// `room`'s terrain, once read (P4.3's planner copies it into its run).
    pub fn room_terrain(&self, room: RoomName) -> Option<&TerrainGrid> {
        self.room_index(room)
            .and_then(|i| self.rooms.get(i))
            .and_then(|r| r.terrain.as_ref())
    }

    /// The tick of `room`'s last scan, if any (P4.3's placement waits for a scan newer than the
    /// room's last change: a road's own footprint shows only in a scan).
    pub fn room_scanned(&self, room: RoomName) -> Option<u32> {
        self.room_index(room)
            .and_then(|i| self.rooms.get(i))
            .and_then(|r| r.scanned)
    }

    /// The footprints of `room`'s last scan: its structures (of every owner), own sites and
    /// deposits, sorted by tile (P4.3's placement reads the roads and the obstacles it has no
    /// object for).
    pub fn room_objects(&self, room: RoomName) -> &[(Tile, Footprint)] {
        self.room_index(room)
            .and_then(|i| self.rooms.get(i))
            .map_or(&[], |r| r.objects.as_slice())
    }

    /// Cached flow fields.
    pub fn fields(&self) -> &FieldCache {
        &self.fields
    }

    /// Kept routes.
    pub fn routes(&self) -> &RouteBook {
        &self.routes
    }

    /// Rooms held closed after a refused transfer, and the tick each stays closed until.
    pub fn closed_rooms(&self) -> &[(RoomName, u32)] {
        &self.closed
    }

    fn room_index(&self, name: RoomName) -> Option<usize> {
        self.rooms
            .binary_search_by_key(&name.packed_repr(), |r| r.name.packed_repr())
            .ok()
    }

    fn room_mut(&mut self, name: RoomName) -> Option<&mut RoomNav> {
        find_room(&mut self.rooms, name)
    }

    fn room_or_insert(&mut self, name: RoomName, now: u32) -> usize {
        match self
            .rooms
            .binary_search_by_key(&name.packed_repr(), |r| r.name.packed_repr())
        {
            Ok(i) => i,
            Err(i) => {
                self.rooms.insert(i, RoomNav::new(name, now));
                i
            }
        }
    }

    /// Groups this tick's creep rows by tracked room (a counting sort, O(creeps + rooms)), so
    /// resolve visits each row once. Rows in rooms not tracked are left out.
    fn group_rows(&mut self, snap: &Snapshot) {
        let n = self.rooms.len();
        self.room_starts.clear();
        self.room_starts.resize(n + 1, 0);
        self.row_room.clear();
        for pos in snap.creeps.positions() {
            let ri = self
                .room_index(pos.room_name())
                .and_then(|i| u32::try_from(i).ok());
            self.row_room.push(ri);
            if let Some(c) = ri.and_then(|i| self.room_starts.get_mut(i as usize + 1)) {
                *c += 1;
            }
        }
        for i in 1..=n {
            let before = self.room_starts.get(i - 1).copied().unwrap_or(0);
            if let Some(c) = self.room_starts.get_mut(i) {
                *c += before;
            }
        }
        let total = self.room_starts.get(n).copied().unwrap_or(0) as usize;
        self.by_room.clear();
        self.by_room.resize(total, 0);
        self.cursor.clear();
        self.cursor.extend(self.room_starts.iter().take(n).copied());
        for (row, ri) in self.row_room.iter().enumerate() {
            if let Some(c) = ri.and_then(|i| self.cursor.get_mut(i as usize))
                && let Some(slot) = self.by_room.get_mut(*c as usize)
            {
                *slot = u32::try_from(row).unwrap_or(u32::MAX);
                *c += 1;
            }
        }
    }

    /// Whether the creep follows a route to this target whose leg in `room` leaves the room again.
    fn mid_route(&self, creep: CreepHandle, target: Position, range: u8, room: RoomName) -> bool {
        let Some(nav) = usize::try_from(creep.slot())
            .ok()
            .and_then(|i| self.navs.get(i))
            .filter(|n| n.handle == Some(creep))
        else {
            return false;
        };
        nav.route.is_some_and(|(id, leg)| {
            self.routes.peek(id).is_some_and(|r| {
                r.target == target
                    && r.range == range
                    && r.legs
                        .get(usize::from(leg))
                        .is_some_and(|l| l.room == room && l.exit.is_some())
            })
        })
    }

    /// The exit a creep leaves its room by on its route to a target (`None` on the route's last leg,
    /// in the target's room): the route it follows; else a failure of its own within
    /// `route_retry_ticks`; else a route from this room whose first exit it can reach (shared by
    /// every creep there); else a search now (a heavy unit), whose route is kept whole. With
    /// [`RouteAsk::fresh`] it searches whatever is kept. The search's dips (edge tiles its path
    /// stepped onto and back from) are kept for [`Self::plan`]. Sets `searched` when it searched.
    fn route_exit(
        &mut self,
        ask: RouteAsk,
        cfg: &MovementConfig,
        commit: &mut Commit,
        searched: &mut bool,
    ) -> Result<Option<Tile>, MoveError> {
        let RouteAsk {
            creep,
            pos,
            target,
            range,
            fresh,
            search,
        } = ask;
        let room = pos.room_name();
        let from = Tile::of(pos);
        let now = self.time;
        let (ttl, retry) = route_ages(cfg);
        if fresh {
            // On a retry the creep's own route is what failed it.
            if let Some(nav) = nav_for(&mut self.navs, creep) {
                nav.route = None;
            }
        } else {
            if let Some(nav) = nav_for(&mut self.navs, creep) {
                if let Some((id, leg)) = nav.route {
                    let on = self
                        .routes
                        .get(id, now)
                        .filter(|r| r.target == target && r.range == range)
                        .and_then(|r| r.legs.get(usize::from(leg)))
                        .filter(|l| l.room == room);
                    match on {
                        Some(l) => return Ok(l.exit),
                        None => nav.route = None,
                    }
                }
                if let Some((r0, t0, g0, at)) = nav.no_route
                    && r0 == room
                    && t0 == target
                    && g0 == range
                {
                    if now.wrapping_sub(at) <= retry {
                        return Err(MoveError::NoPath);
                    }
                    nav.no_route = None;
                }
            }
        }
        if !search
            && let Some((id, leg, exit)) =
                self.reachable_route(room, from, (target, range), commit, ttl)
        {
            if let Some(nav) = nav_for(&mut self.navs, creep) {
                nav.route = Some((id, leg));
            }
            return Ok(Some(exit));
        }
        // A target in a room no creep could cross into: no search can get there (the room
        // callback blocks it, so PathFinder would spend all its ops).
        if self.closed.iter().any(|&(c, _)| c == target.room_name()) {
            if let Some(nav) = nav_for(&mut self.navs, creep) {
                nav.no_route = Some((room, target, range, now));
            }
            self.tick.route_failures += 1;
            return Err(MoveError::NoPath);
        }
        let Self {
            rooms,
            closed,
            dips,
            router,
            routes,
            route_path,
            route_legs: legs,
            path_dips,
            navs,
            tick,
            ..
        } = self;
        let Some(router) = router.as_mut() else {
            return Err(MoveError::OtherRoom);
        };
        let p = profile(cfg);
        let query = RouteQuery {
            from: pos,
            to: target,
            range: route::pathfinder_range(target, range),
            plain_cost: p.plain,
            swamp_cost: p.swamp,
            max_ops: cfg.route_max_ops,
            max_rooms: cfg.route_max_rooms,
        };
        let mut found = false;
        for attempt in 0..2 {
            // Only this search's dips count (a failed search leaves none behind).
            path_dips.clear();
            commit.heavy();
            *searched = true;
            tick.routes += 1;
            let known = RouteRooms {
                rooms: rooms.as_slice(),
                closed: closed.as_slice(),
                avoid: &[],
                dips: dips.as_slice(),
            };
            let outcome = router.search(&query, &known, route_path);
            found = outcome == RouteOutcome::Found
                && route::route_legs(pos, route_path, target.room_name(), legs, path_dips);
            if found || path_dips.is_empty() || attempt > 0 {
                break;
            }
            // A path that only works by stepping onto an edge of this room and back (it never
            // leaves the room): avoid those tiles and search once more. Edge tiles elsewhere on the
            // path do not stop it.
            let until = now.saturating_add(ttl);
            let mut learned = false;
            for &(dr, dt) in path_dips.iter().filter(|&&(dr, _)| dr == room) {
                if !dips.iter().any(|&(r0, t0, _)| r0 == dr && t0 == dt) {
                    dips.push((dr, dt, until));
                    tick.route_dips += 1;
                    learned = true;
                }
            }
            if !learned {
                break;
            }
        }
        let nav = nav_for(navs, creep);
        if !found {
            tick.route_failures += 1;
            if let Some(nav) = nav {
                nav.route = None;
                nav.no_route = Some((room, target, range, now));
            }
            return Err(MoveError::NoPath);
        }
        let known = RouteRooms {
            rooms: rooms.as_slice(),
            closed: closed.as_slice(),
            avoid: &[],
            dips: dips.as_slice(),
        };
        for leg in legs.iter_mut() {
            leg.version = known.grid(leg.room).map_or(0, |(_, v)| v);
        }
        let first = legs.first().and_then(|l| l.exit);
        let id = routes.add(target, range, legs, now);
        if let Some(nav) = nav {
            nav.route = Some((id, 0));
            nav.no_route = None;
        }
        Ok(first)
    }

    /// A leg in `room` of a kept route to the target whose exit the creep at `from` can reach in the
    /// room's grid (its exit field reaches the creep's tile, or a tile beside it for a creep on an
    /// edge): the one with the shortest way to its exit, a later leg on a tie (a later visit's
    /// suffix is part of an earlier one's). The creep joins the route there. Building a field to
    /// check is a heavy unit.
    fn reachable_route(
        &mut self,
        room: RoomName,
        from: Tile,
        (target, range): (Position, u8),
        commit: &mut Commit,
        ttl: u32,
    ) -> Option<(RouteId, u16, Tile)> {
        let now = self.time;
        let Self {
            rooms,
            fields,
            routes,
            route_picks,
            tick,
            ..
        } = self;
        route_picks.clear();
        route_picks.extend(routes.through(room, target, range, now, ttl));
        if route_picks.is_empty() {
            return None;
        }
        let r = find_room(rooms, room)?;
        let grid = r.grid.as_ref()?;
        let mut best: Option<(u32, RouteId, u16, Tile)> = None;
        for &(id, leg, exit) in route_picks.iter() {
            let aim = Aim::Exit(Goal {
                tile: exit,
                range: 0,
            });
            let (field, built) = fields.get(room, aim, grid, r.version, now);
            if built {
                tick.fields_built += 1;
                commit.heavy();
            }
            let dist = if grid.walkable(from) {
                Some(field.distance(from))
                    .filter(|&d| d != UNREACHED)
                    .map(u32::from)
            } else {
                let steps = field.ranked_steps(grid, from);
                (!steps.is_empty()).then_some(steps.total)
            };
            if let Some(d) = dist
                && best.is_none_or(|(b, _, bl, _)| d < b || (d == b && leg > bl))
            {
                best = Some((d, id, leg, exit));
            }
        }
        let (_, id, leg, exit) = best?;
        let _ = routes.get(id, now);
        Some((id, leg, exit))
    }

    /// Builds the room's grid if it is missing or out of date, or (the naive baseline, the grid
    /// cache off) on every tick. False when the room has no terrain or no scan yet.
    fn ensure_grid(&mut self, ri: usize, cfg: &MovementConfig) -> bool {
        let now = self.time;
        let p = profile(cfg);
        let naive = !self.grid_cache;
        let Some(r) = self.rooms.get_mut(ri) else {
            return false;
        };
        let (Some(terrain), Some(_)) = (r.terrain.as_ref(), r.scanned) else {
            return false;
        };
        let every_tick = naive && r.built_at != now;
        if r.grid.is_none() || r.version == 0 || r.dirty || r.built_with != Some(p) || every_tick {
            let objects = r.statics.iter().chain(r.objects.iter()).copied();
            r.grid
                .get_or_insert_with(CostGrid::default)
                .build(terrain, objects, p);
            self.builds = match self.builds.wrapping_add(1) {
                0 => 1,
                v => v,
            };
            r.version = self.builds;
            r.dirty = false;
            r.built_with = Some(p);
            r.built_at = now;
            self.tick.grids_built += 1;
        }
        true
    }
}

fn find_room(rooms: &mut [RoomNav], name: RoomName) -> Option<&mut RoomNav> {
    let i = rooms
        .binary_search_by_key(&name.packed_repr(), |r| r.name.packed_repr())
        .ok()?;
    rooms.get_mut(i)
}

/// How near the tile that held it up a creep on a detour must still be for the detour to be dropped
/// when that tile frees up: 2 tiles, the first steps of a detour, after which going back would cost
/// more than the detour can save and a gap that keeps opening and closing would make it shuttle.
const DETOUR_DROP_RANGE: u8 = 2;

/// Whether `field` reaches a tile beside one of the room's open exits: a way into the room from a
/// neighbor leads to the field's goal. Without one, no route out of the room and back in helps.
fn reaches_an_exit(field: &FlowField, grid: &CostGrid) -> bool {
    (0..ROOM_AREA)
        .filter_map(Tile::from_index)
        .filter(|&t| grid.open_exit(t))
        .any(|e| {
            e.neighbors()
                .any(|(_, n)| grid.walkable(n) && field.distance(n) != UNREACHED)
        })
}

/// The walkable tiles beside `from` (an edge tile), cheapest first (direction order on ties), as
/// steps: the way off the edge for a creep that has no way on.
fn inward_steps(grid: &CostGrid, from: Tile) -> Steps {
    let mut best: [(u8, usize, Option<Tile>); 8] = [(u8::MAX, 0, None); 8];
    for (i, (_, t)) in from.neighbors().enumerate() {
        if grid.walkable(t)
            && let Some(slot) = best.get_mut(i)
        {
            *slot = (grid.cost(t), i, Some(t));
        }
    }
    best.sort_unstable();
    Steps::from_tiles(best.iter().filter_map(|&(_, _, t)| t), 0)
}

/// A plan that keeps the creep on its tile: never shoved.
fn pinned(at: Tile) -> Plan {
    Plan::Hold(Some(Goal { tile: at, range: 0 }))
}

/// The ages after which a cached route (`route_ttl_ticks`) and a cached failure
/// (`route_retry_ticks`) are searched again.
fn route_ages(cfg: &MovementConfig) -> (u32, u32) {
    (cfg.route_ttl_ticks.max(1), cfg.route_retry_ticks.max(1))
}

/// The creep's state, reset when its slot now holds another creep. `None` only if the slot index
/// does not fit a `usize` (it always does).
fn nav_for(navs: &mut Vec<Nav>, h: CreepHandle) -> Option<&mut Nav> {
    let i = usize::try_from(h.slot()).ok()?;
    if navs.len() <= i {
        navs.resize_with(i + 1, Nav::default);
    }
    let nav = navs.get_mut(i)?;
    if nav.handle != Some(h) {
        nav.handle = Some(h);
        nav.goal = None;
        nav.room = None;
        nav.came_from = None;
        nav.crossed_at = None;
        nav.requested_at = None;
        nav.route = None;
        nav.no_route = None;
        nav.expected = None;
        nav.stuck = 0;
        nav.unexplained = 0;
        nav.blocked_at = None;
        nav.detour_fails = 0;
        nav.detour.clear();
        nav.immobile_until = None;
        nav.detour_blocker = None;
    }
    Some(nav)
}

fn set_want(
    wants: &mut [Option<Want>],
    row: usize,
    plan: Plan,
    priority: u8,
    tier: Tier,
    commit: &mut Commit,
) {
    let Some(slot) = wants.get_mut(row) else {
        return;
    };
    let before = slot.is_some_and(|w| w.committed);
    let moving = matches!(plan, Plan::Move(_));
    if moving && !before {
        commit.intents(1);
    }
    *slot = Some(Want {
        plan,
        priority,
        tier,
        committed: before || moving,
    });
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod route_tests;
