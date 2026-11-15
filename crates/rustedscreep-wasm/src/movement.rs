//! Movement glue (ROADMAP P2, movement; rustedscreep-core `movement`): the room reads movement asks
//! for, as scheduled jobs in the decide step, the console's move order, and the traffic resolution
//! at the end of the decide step, which queues the moves for the act step.
//!
//! JS calls:
//! - every tick: none here (the snapshot reads each non-spawning own creep's `fatigue`, snapshot.rs).
//! - a room's first read, while own creeps are in it: for an owned room on the global's first tick,
//!   for another on the tick after a request first found it not read (and every tick with
//!   `cache.movement_grid` off): the terrain, 5 calls (`getTerrain`, V161,
//!   `getRawBuffer`, `length`, js-sys `copy_to`'s `length` and copy; V156, V157), sources and
//!   minerals, 2 `find` calls with their `length`s and 3 calls each (V162), and a scan (below). A Critical
//!   job, `movement read`, one unit per room, each a heavy unit.
//! - a scan, also every `movement.scan_interval_ticks` (or `rescan_min_ticks` after an early-rescan
//!   request) while the room is owned or had a request within the interval: 3 `find` calls
//!   (structures, own construction sites, deposits; V158, V162) with their `length`s, then per structure 4
//!   (`get`, `structureType`, `pos`, `__packedPos`) plus `my` and `isPublic` for a rampart (V159),
//!   per site 4 and per deposit 3. The periodic rescans are a Standard job, `movement rescan`. A
//!   structure of a type this build does not know, or an object with a bad position, is left out
//!   and counted.
//! - resolve: one `Game.cpu.getUsed()` (the tiers' shove budgets) on a tick with a planned move,
//!   none otherwise.
//! - an inter-room route search, only when a request finds no cached exit (route.rs gives the
//!   calls): inside that request's job unit, as a heavy unit.
//! - intents: one `move(direction)` per creep the traffic manager moves (issued by act.rs).

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::rc::Rc;

use js_sys::Uint8Array;
use rustedscreep_consts::{Position, ROOM_AREA, ROOM_SIZE, RoomCoordinate, RoomName};
use rustedscreep_core::config::{CacheConfig, MovementConfig};
use rustedscreep_core::intents::IntentBuffer;
use rustedscreep_core::movement::{
    Counts, Footprint, MoveError, MoveTo, Movement, Progress, ShoveCaps, Tile, site_footprint,
    structure_footprint,
};
use rustedscreep_core::scheduler::{Clock, Fault, Job, JobState, TickCpu, Tier};
use rustedscreep_core::snapshot::{CreepHandle, Snapshot, known_structure_type, parse_room_name};
use screeps::{ConstructionSite, FindConstant, Room, RoomObject, Structure, StructureType, find};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

use crate::route::{PathFinderRoutes, RouteState};
use crate::snapshot::{Findable, JsWorld, position_of};

#[wasm_bindgen]
extern "C" {
    /// A room, for `getTerrain` with `catch` (screeps-game-api's `Room::get_terrain` has none).
    type TerrainSource;

    #[wasm_bindgen(method, catch, js_name = getTerrain)]
    fn get_terrain(this: &TerrainSource) -> Result<RawTerrain, JsValue>;

    /// A `Room.Terrain`.
    type RawTerrain;

    /// `getRawBuffer()`: a new Uint8Array copy, row-major (V156).
    #[wasm_bindgen(method, catch, js_name = getRawBuffer)]
    fn raw_buffer(this: &RawTerrain) -> Result<Uint8Array, JsValue>;

    /// An owned structure's `my` as the engine gives it: `undefined` without a user (V159).
    type Owned;

    #[wasm_bindgen(method, getter = my)]
    fn my_value(this: &Owned) -> Option<bool>;

    /// A rampart, for `isPublic` (a boolean, V159).
    type Rampart;

    #[wasm_bindgen(method, getter = isPublic)]
    fn is_public_value(this: &Rampart) -> Option<bool>;
}

/// The console's move order (`rustedscreep.moveTo`): every own creep in `from` (the target's room
/// unless given) without a request of its own heads for the target, and keeps going once it left
/// that room, until all of them are in range on one tick.
#[derive(Clone)]
struct Order {
    target: Position,
    range: u8,
    from: RoomName,
    /// The creeps the order took so far (those that left `from` included).
    members: Vec<CreepHandle>,
}

/// The console order's job: Standard tier, one unit per creep.
const CONSOLE: Job = Job {
    name: "movement console",
    tier: Tier::Standard,
};

/// Console requests move after every job's (P2 jobs use higher priorities).
const CONSOLE_PRIORITY: u8 = 0;

/// The first reads' job (terrain, sources and minerals, the first scan): Critical, since nothing
/// in the room can move without them, and run before every other job.
const READ: Job = Job {
    name: "movement read",
    tier: Tier::Critical,
};

/// The periodic and early rescans' job: Standard, so they stop when the bucket is low.
const RESCAN: Job = Job {
    name: "movement rescan",
    tier: Tier::Standard,
};

/// Movement's state between ticks: the core's, and the glue's scratch and counters.
pub struct MovementGlue {
    pub core: Movement,
    terrain: Box<[u8; ROOM_AREA]>,
    rooms: Vec<RoomName>,
    read_job: JobState,
    rescan_job: JobState,
    order: Option<Order>,
    order_job: JobState,
    order_creeps: Vec<usize>,
    /// What the PathFinder route search (installed in `core`) answers its room callback from.
    routes: Rc<RefCell<RouteState>>,
    /// Since the global started: room reads that failed, scan objects left out.
    read_failures: u32,
    scan_skips: u32,
}

impl Default for MovementGlue {
    fn default() -> Self {
        let mut core = Movement::default();
        let (search, routes) = PathFinderRoutes::new();
        core.set_router(Box::new(search));
        Self {
            core,
            terrain: Box::new([0; ROOM_AREA]),
            rooms: Vec::new(),
            read_job: JobState::default(),
            rescan_job: JobState::default(),
            order: None,
            order_job: JobState::default(),
            order_creeps: Vec::new(),
            routes,
            read_failures: 0,
            scan_skips: 0,
        }
    }
}

impl MovementGlue {
    /// The snapshot step's part (no JS call): starts movement's tick.
    pub fn begin(&mut self, snapshot: &Snapshot, cfg: &MovementConfig, cache: &CacheConfig) {
        self.core.begin(snapshot, cfg);
        self.core.set_cache(cfg, cache);
        if let Ok(mut routes) = self.routes.try_borrow_mut() {
            // A search indexes at most `route_max_rooms` rooms, and each can have 4 neighbours it
            // blocks (blocked rooms take no slot, V165): keep that many statuses at least.
            let statuses = cfg
                .route_cache_size
                .max(u32::from(cfg.route_max_rooms).saturating_mul(5));
            routes.begin(snapshot.tick.time, cfg.route_ttl_ticks, statuses);
        }
    }

    /// The decide step's first job (Critical): reads the rooms movement needs before anything in
    /// them can move. One unit per room, each a heavy unit for the slicer.
    pub fn read_first<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        js: &JsWorld,
        cfg: &MovementConfig,
    ) {
        self.core.first_reads(cfg, &mut self.rooms);
        let count = u32::try_from(self.rooms.len()).unwrap_or(u32::MAX);
        let Self {
            core,
            terrain,
            rooms,
            read_job,
            read_failures,
            scan_skips,
            ..
        } = self;
        tick.run_units(&READ, read_job, count, |unit, commit| {
            commit.heavy();
            let name = *rooms
                .get(unit as usize)
                .ok_or(Fault::new("no room", unit))?;
            let room = js_room(snapshot, js, name).ok_or(Fault::new("room not visible", unit))?;
            if !read_terrain(room, terrain) || !core.set_terrain(name, &terrain[..]) {
                *read_failures = read_failures.saturating_add(1);
                return Err(Fault::new("terrain read failed", unit));
            }
            let skipped = read_statics(core, name, room).saturating_add(scan(core, name, room));
            *scan_skips = scan_skips.saturating_add(skipped);
            Ok(())
        });
    }

    /// A Standard job: the rooms whose periodic or early rescan is due. One unit per room, each a
    /// heavy unit for the slicer.
    pub fn rescan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        js: &JsWorld,
        cfg: &MovementConfig,
    ) {
        self.core.rescans(cfg, &mut self.rooms);
        let count = u32::try_from(self.rooms.len()).unwrap_or(u32::MAX);
        let Self {
            core,
            rooms,
            rescan_job,
            scan_skips,
            ..
        } = self;
        tick.run_units(&RESCAN, rescan_job, count, |unit, commit| {
            commit.heavy();
            let name = *rooms
                .get(unit as usize)
                .ok_or(Fault::new("no room", unit))?;
            let room = js_room(snapshot, js, name).ok_or(Fault::new("room not visible", unit))?;
            *scan_skips = scan_skips.saturating_add(scan(core, name, room));
            Ok(())
        });
    }

    /// The decide step's console order (Standard tier): requests for the order's creeps that have no
    /// request of their own this tick.
    pub fn run_console<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        cfg: &MovementConfig,
    ) {
        let Some(order) = self.order.as_mut() else {
            return;
        };
        let creeps = &snapshot.creeps;
        order.members.retain(|&h| creeps.row_of(h).is_some());
        self.order_creeps.clear();
        for (row, pos) in creeps.positions().iter().enumerate() {
            let mine = creeps.my().get(row).copied().unwrap_or(false);
            let spawning = creeps.spawning().get(row).copied().unwrap_or(true);
            let Some(&h) = creeps.handles().get(row) else {
                continue;
            };
            let member = order.members.contains(&h);
            if mine
                && !spawning
                && (member || pos.room_name() == order.from)
                && !self.core.planned(row)
            {
                if !member {
                    order.members.push(h);
                }
                self.order_creeps.push(row);
            }
        }
        let (target, range) = (order.target, order.range);
        // A stable order across ticks (V102): by id.
        self.order_creeps.sort_unstable_by_key(|&row| {
            creeps
                .handles()
                .get(row)
                .and_then(|&h| creeps.key_of(h))
                .map_or(u128::MAX, |k| k.0)
        });
        let to = MoveTo {
            target,
            range,
            priority: CONSOLE_PRIORITY,
            tier: CONSOLE.tier,
        };
        let count = u32::try_from(self.order_creeps.len()).unwrap_or(u32::MAX);
        let arrived = Cell::new(0u32);
        let (core, rows) = (&mut self.core, &self.order_creeps);
        let report = tick.run_units(&CONSOLE, &mut self.order_job, count, |unit, commit| {
            let row = rows
                .get(unit as usize)
                .copied()
                .ok_or(Fault::new("no row", unit))?;
            let h = creeps
                .handles()
                .get(row)
                .copied()
                .ok_or(Fault::new("no handle", unit))?;
            match core.request(snapshot, h, to, cfg, commit) {
                Ok(Progress::Arrived) => arrived.set(arrived.get() + 1),
                // Waiting is not a fault: a room is read on the tick after its first request.
                Ok(_)
                | Err(MoveError::NoPath | MoveError::RoomNotReady | MoveError::CannotMove) => {}
                Err(e) => return Err(Fault::new(e.as_str(), unit)),
            }
            Ok(())
        });
        if count > 0 && report.ran == count && arrived.get() == count {
            self.order = None;
            if log::log_enabled!(log::Level::Info) {
                let (x, y) = (target.x().u8(), target.y().u8());
                log::info!(
                    "rustedscreep moveTo: all {count} creep(s) in {} are within {range} of ({x}, {y}); order done",
                    target.room_name(),
                );
            }
        }
    }

    /// The end of the decide step: resolves every room's traffic and queues the moves. Each
    /// mover's shoves are capped by what is left before its job's tier cap, at `INTENT_CPU_COST`
    /// each (V34): one clock reading, only on a tick with a planned move (a creep stepping off an
    /// edge after it found no way counts).
    pub fn resolve<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        buffer: &mut IntentBuffer,
        cfg: &MovementConfig,
    ) {
        let caps = if self.core.has_movers() {
            tick.sample();
            let spent = tick.spent();
            let budget = *tick.budget();
            ShoveCaps::from_cpu_left(Tier::ALL.map(|t| budget.cap(t).map_or(0.0, |c| c - spent)))
        } else {
            ShoveCaps::default()
        };
        self.core
            .resolve(snapshot, buffer, cfg, caps, tick.commit());
    }

    /// After the act step: the moves the game refused do not count as blocked.
    pub fn after_act(
        &mut self,
        snapshot: &Snapshot,
        refused_moves: &[(u32, bool)],
        cfg: &MovementConfig,
    ) {
        self.core
            .moves_refused(snapshot, refused_moves.iter().copied(), cfg);
    }

    /// Sets or clears the console's move order (`rustedscreep.moveTo`); returns the reply. `from` is
    /// the room whose creeps go (the target's room when `None`).
    pub fn set_order(
        &mut self,
        room: Option<&str>,
        (x, y, range): (Option<u32>, Option<u32>, Option<u32>),
        from: Option<&str>,
    ) -> String {
        let Some(room) = room else {
            let had = self.order.take().is_some();
            return String::from(if had {
                "rustedscreep moveTo: order cancelled"
            } else {
                "rustedscreep moveTo: no order to cancel"
            });
        };
        let Some(name) = parse_room_name(room.as_bytes()) else {
            return String::from("rustedscreep moveTo: not a room name");
        };
        let coord = |v: Option<u32>| {
            v.and_then(|v| u8::try_from(v).ok())
                .and_then(|v| RoomCoordinate::new(v).ok())
        };
        let (Some(cx), Some(cy)) = (coord(x), coord(y)) else {
            return format!(
                "rustedscreep moveTo: x and y must be 0 to {}",
                ROOM_SIZE - 1
            );
        };
        let origin = match from {
            None => name,
            Some(f) => match parse_room_name(f.as_bytes()) {
                Some(f) => f,
                None => return String::from("rustedscreep moveTo: not a room name to start from"),
            },
        };
        let range = range.map_or(1, |r| r.min(u32::from(ROOM_SIZE - 1)));
        let range = u8::try_from(range).unwrap_or(1);
        self.order = Some(Order {
            target: Position::new(cx, cy, name),
            range,
            from: origin,
            members: Vec::new(),
        });
        let (x, y) = (cx.u8(), cy.u8());
        if origin == name {
            format!(
                "rustedscreep moveTo: own creeps in {name} head for ({x}, {y}) within {range} from the next tick, until all are in range; rustedscreep.moveTo() cancels"
            )
        } else {
            format!(
                "rustedscreep moveTo: own creeps in {origin} head for ({x}, {y}) in {name} within {range} from the next tick, until all are in range; rustedscreep.moveTo() cancels"
            )
        }
    }

    /// One line for `rustedscreep.movement()`.
    pub fn summary(&self, snapshot: &Snapshot, cfg: &MovementConfig) -> String {
        let m = &self.core;
        let (tracked, with_terrain, active, wanted) = m.room_counts(cfg);
        let mut text = format!(
            "rustedscreep movement at tick {}: rooms tracked {tracked} (terrain {with_terrain}, active {active}, read {wanted})",
            snapshot.tick.time
        );
        for (name, version, scanned, objects) in m.room_states() {
            let _ = write!(
                text,
                " ({name}: grid v{version}, {objects} objects, scanned "
            );
            let _ = match scanned {
                Some(at) => write!(text, "at {at})"),
                None => write!(text, "never)"),
            };
        }
        let _ = write!(
            text,
            " | fields cached {} (built {}, hits {})",
            m.fields().len(),
            m.fields().built(),
            m.fields().hits()
        );
        text.push_str(" | this tick:");
        counts(&mut text, m.tick());
        text.push_str(" | since boot:");
        counts(&mut text, m.totals());
        let _ = write!(
            text,
            " | read failures {}, scan skips {}",
            self.read_failures, self.scan_skips
        );
        let _ = write!(text, " | routes kept {}, closed rooms", m.routes().len());
        if m.closed_rooms().is_empty() {
            text.push_str(" none");
        }
        for (room, until) in m.closed_rooms() {
            let _ = write!(text, " {room} (until {until})");
        }
        if let Ok(routes) = self.routes.try_borrow() {
            let r = routes.stats;
            let _ = write!(
                text,
                ", PathFinder searches {} (threw {}, incomplete {}, ops {}), room callbacks {} (blocked: closed {}, status {}, bad name {}), matrices {}",
                r.searches,
                r.throws,
                r.incomplete,
                r.ops,
                r.callbacks,
                r.blocked_closed,
                r.blocked_status,
                r.bad_names,
                routes.matrices()
            );
        }
        match &self.order {
            Some(o) => {
                let _ = write!(
                    text,
                    " | moveTo order: ({}, {}) in {} within {}",
                    o.target.x().u8(),
                    o.target.y().u8(),
                    o.target.room_name(),
                    o.range
                );
                if o.from != o.target.room_name() {
                    let _ = write!(text, " from {} ({} creep(s))", o.from, o.members.len());
                }
            }
            None => text.push_str(" | no moveTo order"),
        }
        text
    }
}

/// The room's JS object from this tick's snapshot, if it is visible.
fn js_room<'a>(snapshot: &Snapshot, js: &'a JsWorld, name: RoomName) -> Option<&'a Room> {
    snapshot
        .rooms
        .row_of(name)
        .and_then(|row| js.rooms.get(row))
}

fn counts(text: &mut String, c: &Counts) {
    let _ = write!(
        text,
        " requests {} (arrived {}, moving {}, tired {}, errors {}), fields built {}, detours {} (failed {}, skipped {}), \
         traffic moved {} blocked {} shoved {}, moves queued {} (refused {}, refused by the game {}), scans {} (changed {}), \
         grids built {}, early rescans {}, suspect tiles {}, routes searched {} (failed {}, edge tiles learned {}), \
         rooms crossed {} (transfers refused {})",
        c.requests,
        c.arrived,
        c.moving,
        c.tired,
        c.errors,
        c.fields_built,
        c.detours,
        c.detour_failures,
        c.detours_skipped,
        c.moved,
        c.blocked,
        c.shoved,
        c.pushed,
        c.refused,
        c.refused_by_game,
        c.scans,
        c.changed,
        c.grids_built,
        c.stale,
        c.suspects,
        c.routes,
        c.route_failures,
        c.route_dips,
        c.crossed,
        c.transfers_refused,
    );
}

/// Copies the room's terrain into `buf`: 5 JS calls (`getTerrain`, `getRawBuffer`, `length`, and
/// js-sys `copy_to`'s own `length` check and copy; V156, V157). False when a call throws or the buffer
/// is not `ROOM_AREA` long (checked before the copy, whose length assertion would panic).
fn read_terrain(room: &Room, buf: &mut [u8; ROOM_AREA]) -> bool {
    let Ok(terrain) = room.unchecked_ref::<TerrainSource>().get_terrain() else {
        return false;
    };
    let Ok(raw) = terrain.raw_buffer() else {
        return false;
    };
    if usize::try_from(raw.length()).ok() != Some(ROOM_AREA) {
        return false;
    }
    raw.copy_to(&mut buf[..]);
    true
}

/// Reads one room's sources and minerals (obstacles that never move, in OBSTACLE_OBJECT_TYPES;
/// V131) into movement, once per global with its terrain; returns how many it left out.
fn read_statics(movement: &mut Movement, name: RoomName, room: &Room) -> u32 {
    let finder = room.unchecked_ref::<Findable>();
    let sources = finder.find_array(find::SOURCES.find_code());
    let minerals = finder.find_array(find::MINERALS.find_code());
    let skipped = Cell::new(0u32);
    let tiles = [sources, minerals].into_iter().flat_map(|list| {
        let skipped = &skipped;
        (0..list.length()).filter_map(move |i| {
            let object: RoomObject = list.get(i).unchecked_into();
            match position_of(&object) {
                Ok(p) => Some(Tile::of(p)),
                Err(_) => {
                    skipped.set(skipped.get().saturating_add(1));
                    None
                }
            }
        })
    });
    movement.set_statics(name, tiles);
    skipped.get()
}

/// Scans one visible room's structures, own construction sites and deposits into movement (V158,
/// V159); returns how many it left out.
fn scan(movement: &mut Movement, name: RoomName, room: &Room) -> u32 {
    let finder = room.unchecked_ref::<Findable>();
    let structures = finder.find_array(find::STRUCTURES.find_code());
    let sites = finder.find_array(find::MY_CONSTRUCTION_SITES.find_code());
    let deposits = finder.find_array(find::DEPOSITS.find_code());
    let skipped = Cell::new(0u32);
    let skip = || skipped.set(skipped.get().saturating_add(1));
    let structure_footprints = (0..structures.length()).filter_map(|i| {
        let s: Structure = structures.get(i).unchecked_into();
        let Some(kind) = known_structure_type(s.structure_type()) else {
            skip();
            return None;
        };
        let Ok(pos) = position_of(&s) else {
            skip();
            return None;
        };
        let (mine, public) = if kind == StructureType::Rampart {
            (
                s.unchecked_ref::<Owned>().my_value().unwrap_or(false),
                s.unchecked_ref::<Rampart>()
                    .is_public_value()
                    .unwrap_or(false),
            )
        } else {
            (false, false)
        };
        structure_footprint(kind, mine, public).map(|f| (Tile::of(pos), f))
    });
    let site_footprints = (0..sites.length()).filter_map(|i| {
        let site: ConstructionSite = sites.get(i).unchecked_into();
        let Some(kind) = known_structure_type(site.structure_type()) else {
            skip();
            return None;
        };
        let Ok(pos) = position_of(&site) else {
            skip();
            return None;
        };
        site_footprint(kind).map(|f| (Tile::of(pos), f))
    });
    // Deposits are in OBSTACLE_OBJECT_TYPES (V131); they appear and decay, so every scan reads them.
    let deposit_footprints = (0..deposits.length()).filter_map(|i| {
        let object: RoomObject = deposits.get(i).unchecked_into();
        match position_of(&object) {
            Ok(p) => Some((Tile::of(p), Footprint::Blocked)),
            Err(_) => {
                skip();
                None
            }
        }
    });
    movement.set_objects(
        name,
        structure_footprints
            .chain(site_footprints)
            .chain(deposit_footprints),
    );
    skipped.get()
}
