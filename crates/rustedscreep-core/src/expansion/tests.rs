//! Tests of scouting (P8.1, ADR-039, `expansion`). Expected values derive from the constants
//! (`OBSERVER_RANGE`, `CONTROLLER_STRUCTURES` for the observer, `CREEP_LIFE_TIME`), the
//! persistence frame's `MAX_PAYLOAD`, and the scouting config.

use std::cell::RefCell;
use std::rc::Rc;

use rustedscreep_consts::{
    CREEP_LIFE_TIME, OBSERVER_RANGE, Part, Position, ROOM_AREA, ROOM_SIZE, ResourceType,
    RoomCoordinate, RoomName, StructureType,
};

use super::*;
use crate::config::{MOVEMENT_MMO, SCOUT_MMO, SPAWN_MMO, ScoutConfig};
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{Movement, RouteOutcome, RouteQuery, RouteRooms, RouteSearch, Tile};
use crate::persist::MAX_PAYLOAD;
use crate::scheduler::Commit;
use crate::signing::RecordLoad;
use crate::snapshot::{
    Controller, CreepRow, IntelRead, MineralRead, ObjectKey, Reservation, RoomEnergy, RoomStatus,
    Snapshot, StructureStatic, TickContext,
};
use crate::spawning::{Body, BodySpec, Identity, Role, Spawner};

fn room(name: &str) -> RoomName {
    name.parse().expect("room")
}

fn tile(x: u8, y: u8) -> Tile {
    Tile::new(x, y).expect("tile")
}

fn pos(r: RoomName, x: u8, y: u8) -> Position {
    Position::new(
        RoomCoordinate::new(x).expect("x"),
        RoomCoordinate::new(y).expect("y"),
        r,
    )
}

/// The tests' config: rooms one around the home, so a test can name every one.
fn cfg() -> ScoutConfig {
    ScoutConfig {
        max_distance: 1,
        ..SCOUT_MMO
    }
}

// ---- The atlas and its record ----

#[test]
fn the_default_reach_is_the_observer_range() {
    assert_eq!(SCOUT_MMO.max_distance, OBSERVER_RANGE);
}

#[test]
fn minerals_round_trip_their_codes() {
    for m in Mineral::ALL {
        assert_eq!(Mineral::from_code(m.code()), Some(m));
        assert_ne!(m.code(), 0, "0 is no mineral");
        if let Some(r) = m.resource() {
            assert_eq!(Mineral::from_resource(r), m);
        }
    }
    assert_eq!(Mineral::from_code(0), None);
    assert_eq!(Mineral::from_code(9), None);
    assert_eq!(Mineral::from_resource(ResourceType::Energy), Mineral::Other);
    assert_eq!(Mineral::Catalyst.as_str(), "X");
}

/// One sample entry: the room, its controller, its reservation, its source count, its mineral,
/// its towers, and its invader core.
type Case = (
    &'static str,
    Controller,
    Reserved,
    usize,
    Option<Mineral>,
    u8,
    Option<u8>,
);

fn sample_atlas() -> Atlas {
    let mut a = Atlas::default();
    let cases: [Case; 6] = [
        (
            "E1N1",
            Controller::Neutral,
            Reserved::None,
            2,
            Some(Mineral::Hydrogen),
            0,
            None,
        ),
        (
            "E1N2",
            Controller::Foreign { level: 8 },
            Reserved::None,
            1,
            Some(Mineral::Catalyst),
            6,
            None,
        ),
        (
            "E2N1",
            Controller::Neutral,
            Reserved::Other { until: 9000 },
            2,
            None,
            0,
            Some(0),
        ),
        (
            "E2N3",
            Controller::Absent,
            Reserved::None,
            MAX_SOURCES,
            Some(Mineral::Other),
            0,
            Some(5),
        ),
        (
            "E3N3",
            Controller::Neutral,
            Reserved::Mine { until: 7000 },
            0,
            None,
            0,
            None,
        ),
        (
            "E3N1",
            Controller::Mine { level: 3 },
            Reserved::None,
            2,
            Some(Mineral::Utrium),
            1,
            None,
        ),
    ];
    for (i, &(name, controller, reserved, sources, mineral, towers, core)) in
        cases.iter().enumerate()
    {
        let mut e = RoomIntel::new(room(name), 100 + i as u32);
        e.known = i != 4;
        e.unreachable = i % 2 == 1;
        e.status = [
            RoomStatus::Normal,
            RoomStatus::Closed,
            RoomStatus::Novice,
            RoomStatus::Respawn,
        ][i % 4];
        e.controller = controller;
        e.controller_tile = (i != 3).then(|| tile(10 + i as u8, 20));
        e.reserved = reserved;
        e.safe_until = if towers > 0 { 5000 + i as u32 } else { 0 };
        for (s, slot) in e.sources.iter_mut().take(sources).enumerate() {
            *slot = Some(tile(5 + s as u8, 40));
        }
        e.mineral = mineral.map(|m| Deposit {
            mineral: m,
            tile: tile(45, 1 + i as u8),
            density: 1 + (i % 4) as u8,
        });
        e.towers = towers;
        e.spawns = towers / 2;
        e.lairs = if core.is_some() { 4 } else { 0 };
        e.core = core;
        e.hostiles = i as u8;
        assert!(a.put(e).is_none());
    }
    a
}

#[test]
fn the_record_round_trips() {
    let a = sample_atlas();
    let mut out = Vec::new();
    assert_eq!(a.encode(&mut out, MAX_PAYLOAD), 0);
    let want: usize = 2 + a
        .entries()
        .iter()
        .map(|e| entry_len(e.source_count()))
        .sum::<usize>();
    assert_eq!(out.len(), want);
    assert_eq!(Atlas::decode(&out), Ok(a.clone()));
    // Entries stay in room order.
    let packed: Vec<u16> = a.entries().iter().map(|e| e.room.packed_repr()).collect();
    let mut sorted = packed.clone();
    sorted.sort_unstable();
    assert_eq!(packed, sorted);
    assert_eq!(Atlas::decode(&[0, 0]), Ok(Atlas::default()));
}

#[test]
fn the_record_refuses_bad_payloads() {
    let a = sample_atlas();
    let mut good = Vec::new();
    a.encode(&mut good, MAX_PAYLOAD);
    assert_eq!(
        Atlas::decode(&good[..good.len() - 1]),
        Err(RecordError::Short)
    );
    assert_eq!(Atlas::decode(&[]), Err(RecordError::Short));
    let mut long = good.clone();
    long.push(0);
    assert_eq!(Atlas::decode(&long), Err(RecordError::Long));
    // The first entry's fields after the count (2): room 2, seen 4, flags, status, owner, level,
    // controller 2, reserve, reserve_end 4, safe_end 4, n, sources, mineral, ...
    let flags = 2 + 2 + 4;
    let status = flags + 1;
    let owner = status + 1;
    let controller = owner + 2;
    let reserve = controller + 2;
    let n = reserve + 1 + 4 + 4;
    let set = |at: usize, v: u8| {
        let mut b = good.clone();
        b[at] = v;
        Atlas::decode(&b)
    };
    assert_eq!(set(flags, 4), Err(RecordError::Code("flags", 4)));
    assert_eq!(set(status, 4), Err(RecordError::Code("status", 4)));
    assert_eq!(set(owner, 4), Err(RecordError::Code("owner", 4)));
    assert_eq!(set(reserve, 3), Err(RecordError::Code("reserve", 3)));
    let too_many = u8::try_from(MAX_SOURCES + 1).expect("small");
    assert_eq!(set(n, too_many), Err(RecordError::Sources(too_many)));
    let mut off = good.clone();
    let past = u16::try_from(ROOM_AREA).expect("area");
    off[controller..controller + 2].copy_from_slice(&past.to_le_bytes());
    assert_eq!(Atlas::decode(&off), Err(RecordError::Tile(past)));
    // The mineral byte of the first entry.
    let mineral = n + 1 + 2 * a.entries()[0].source_count();
    assert_eq!(set(mineral, 9), Err(RecordError::Code("mineral", 9)));
    // Two entries swapped are out of order.
    let len = entry_len(a.entries()[0].source_count());
    let mut swapped = good.clone();
    let (first, second) = (
        2..2 + len,
        2 + len..2 + len + entry_len(a.entries()[1].source_count()),
    );
    let mut b = good[second.clone()].to_vec();
    b.extend_from_slice(&good[first.clone()]);
    swapped[first.start..second.end].copy_from_slice(&b);
    assert_eq!(Atlas::decode(&swapped), Err(RecordError::Order));
}

/// Every room within `OBSERVER_RANGE` of one home, each with the most sources an entry keeps,
/// fits under a quarter of one segment's payload.
///
/// Uses: `OBSERVER_RANGE`, `MAX_PAYLOAD`, `entry_len`, `MAX_SOURCES`.
#[test]
fn one_home_of_rooms_fits_a_quarter_of_a_segment() {
    let side = 2 * OBSERVER_RANGE as usize + 1;
    let len = 2 + side * side * entry_len(MAX_SOURCES);
    assert!(len < MAX_PAYLOAD / 4, "{len} of {MAX_PAYLOAD}");
}

#[test]
fn an_atlas_over_its_cap_keeps_the_newest_rooms() {
    let a = sample_atlas();
    // Room for the two newest entries only (E3N1 at 105 and E3N3 at 104).
    let newest: Vec<&RoomIntel> = {
        let mut v: Vec<&RoomIntel> = a.entries().iter().collect();
        v.sort_by_key(|e| core::cmp::Reverse(e.seen));
        v.into_iter().take(2).collect()
    };
    let cap = 2 + newest
        .iter()
        .map(|e| entry_len(e.source_count()))
        .sum::<usize>();
    let mut out = Vec::new();
    assert_eq!(a.encode(&mut out, cap), a.len() - 2);
    assert!(out.len() <= cap);
    let kept = Atlas::decode(&out).expect("decodes");
    let mut rooms: Vec<RoomName> = kept.entries().iter().map(|e| e.room).collect();
    let mut want: Vec<RoomName> = newest.iter().map(|e| e.room).collect();
    want.sort_by_key(|r| r.packed_repr());
    rooms.sort_by_key(|r| r.packed_repr());
    assert_eq!(rooms, want);
}

#[test]
fn a_read_becomes_an_entry() {
    let r = room("E1N1");
    let now = 5000;
    let mut read = IntelRead {
        read: true,
        status: RoomStatus::Novice,
        controller_at: Some(pos(r, 12, 30)),
        reservation: Reservation::Other { ticks: 400 },
        safe_mode: 0,
        mineral: Some(MineralRead {
            kind: Some(ResourceType::Keanium),
            at: pos(r, 40, 8),
            density: 3,
        }),
        towers: 2,
        spawns: 1,
        lairs: 0,
        core: Some(1),
        hostiles: 3,
        ..IntelRead::default()
    };
    read.add_source(pos(r, 3, 3));
    read.add_source(pos(r, 44, 44));
    let e = RoomIntel::from_read(r, Controller::Foreign { level: 4 }, &read, now);
    assert!(e.known && !e.unreachable);
    assert_eq!(e.seen, now);
    assert_eq!(e.status, RoomStatus::Novice);
    assert_eq!(e.controller_tile, Some(tile(12, 30)));
    assert_eq!(e.reserved, Reserved::Other { until: now + 400 });
    assert_eq!(e.safe_until, 0);
    assert_eq!(
        e.source_tiles().collect::<Vec<_>>(),
        [tile(3, 3), tile(44, 44)]
    );
    assert_eq!(
        e.mineral,
        Some(Deposit {
            mineral: Mineral::Keanium,
            tile: tile(40, 8),
            density: 3
        })
    );
    assert_eq!((e.towers, e.spawns, e.core, e.hostiles), (2, 1, Some(1), 3));
    assert!(e.towered(), "another owner's towers");
    // Another owner's safe mode ends at its ticks from now.
    read.safe_mode = 1234;
    let e = RoomIntel::from_read(r, Controller::Foreign { level: 4 }, &read, now);
    assert_eq!(e.safe_until, now + 1234);
    // Towers in a room nobody owns (an invader core's) do not count as towered for the explorer.
    let e = RoomIntel::from_read(r, Controller::Neutral, &read, now);
    assert!(!e.towered());
}

#[test]
fn due_follows_the_refresh() {
    let c = cfg();
    let mut a = Atlas::default();
    let r = room("E1N1");
    assert!(a.due(r, 0, c.refresh_ticks), "no entry is due");
    let mut e = RoomIntel::new(r, 1000);
    e.known = true;
    a.put(e);
    assert!(!a.due(r, 1000 + c.refresh_ticks - 1, c.refresh_ticks));
    assert!(a.due(r, 1000 + c.refresh_ticks, c.refresh_ticks));
    // Marking it unreachable keeps the read and restarts the wait.
    a.mark_unreachable(r, 2000);
    let e = a.get(r).expect("entry");
    assert!(e.known && e.unreachable);
    assert_eq!(e.seen, 2000);
    assert!(!a.due(r, 2000 + c.refresh_ticks - 1, c.refresh_ticks));
    // An unreachable mark on a room never read makes an entry with no read.
    a.mark_unreachable(room("E2N1"), 2000);
    let e = a.get(room("E2N1")).expect("entry");
    assert!(!e.known && e.unreachable);
}

#[test]
fn the_observer_range_is_a_square_with_no_wrap() {
    let from = room("E13N13");
    let r = OBSERVER_RANGE as i32;
    for (dx, dy, inside) in [
        (r, r, true),
        (-r, r, true),
        (r + 1, 0, false),
        (0, -r - 1, false),
    ] {
        let to = from.checked_add((dx, dy)).expect("room");
        assert_eq!(in_observer_range(from, to), inside, "{dx}, {dy}");
    }
}

// ---- The world ----

/// A route search over open plains in a straight line, with rooms in `fail` refused, those in
/// `incomplete` out of ops, and every query recorded with the rooms it was told to avoid. With `long`, every found path is that many
/// steps.
#[derive(Default)]
struct Router {
    fail: Vec<RoomName>,
    /// Rooms a search runs out of ops for.
    incomplete: Vec<RoomName>,
    long: Option<usize>,
    queries: Vec<(RouteQuery, Vec<RoomName>)>,
}

struct Shared(Rc<RefCell<Router>>);

impl RouteSearch for Shared {
    fn search(
        &mut self,
        q: &RouteQuery,
        rooms: &RouteRooms<'_>,
        path: &mut Vec<Position>,
    ) -> RouteOutcome {
        path.clear();
        let mut r = self.0.borrow_mut();
        r.queries.push((*q, rooms.closed_rooms().collect()));
        if r.fail.contains(&q.to.room_name()) {
            return RouteOutcome::Failed;
        }
        if r.incomplete.contains(&q.to.room_name()) {
            return RouteOutcome::Incomplete;
        }
        let near = |p: Position| {
            let (a, b) = (p.world_coords(), q.to.world_coords());
            (a.0 - b.0).abs().max((a.1 - b.1).abs()) <= i32::from(q.range)
        };
        let last = ROOM_SIZE - 1;
        let mut cur = q.from;
        while !near(cur) {
            let (wx, wy) = cur.world_coords();
            let (gx, gy) = q.to.world_coords();
            let (mut dx, mut dy) = ((gx - wx).signum(), (gy - wy).signum());
            let (x, y) = (cur.x().u8(), cur.y().u8());
            if x == 0 || x == last {
                let across = (x == 0 && dx == -1) || (x == last && dx == 1);
                if across {
                    dy = 0;
                } else if dx == 0 {
                    dx = if x == 0 { 1 } else { -1 };
                }
            } else if y == 0 || y == last {
                let across = (y == 0 && dy == -1) || (y == last && dy == 1);
                if across {
                    dx = 0;
                } else if dy == 0 {
                    dy = if y == 0 { 1 } else { -1 };
                }
            }
            let next = Position::checked_from_world_coords(wx + dx, wy + dy).expect("world");
            if rooms.closed(next.room_name()) {
                path.clear();
                return RouteOutcome::Incomplete;
            }
            path.push(next);
            cur = next;
        }
        if let Some(n) = r.long {
            while path.len() < n {
                path.push(cur);
            }
        }
        RouteOutcome::Found
    }
}

#[derive(Clone, Copy)]
struct Creep {
    key: u128,
    serial: u64,
    home: RoomName,
    at: Position,
    death: u32,
    spawning: bool,
}

/// A visible room other than the homes, and what the snapshot read in it for the atlas.
#[derive(Clone, Copy)]
struct Seen {
    name: RoomName,
    controller: Controller,
    read: Option<IntelRead>,
}

const HOME_SPAWN: (u8, u8) = (25, 25);

struct World {
    time: u32,
    homes: Vec<(RoomName, u8)>,
    explorers: Vec<Creep>,
    seen: Vec<Seen>,
    /// Own observers, by room.
    observers: Vec<RoomName>,
    /// Owned rooms that are no home (no spawn), with their level.
    claimed: Vec<(RoomName, u8)>,
    snap: Snapshot,
    movement: Movement,
    spawner: Spawner,
    buffer: IntentBuffer,
    scouting: Scouting,
    router: Rc<RefCell<Router>>,
    cfg: ScoutConfig,
}

impl World {
    fn new() -> Self {
        let router = Rc::new(RefCell::new(Router::default()));
        let mut movement = Movement::default();
        movement.set_router(Box::new(Shared(Rc::clone(&router))));
        let mut scouting = Scouting::default();
        scouting.loaded(RecordLoad::Empty).expect("no record");
        Self {
            time: 100_000,
            homes: vec![(room("E2N2"), 5)],
            explorers: Vec::new(),
            seen: Vec::new(),
            observers: Vec::new(),
            claimed: Vec::new(),
            snap: Snapshot::default(),
            movement,
            spawner: Spawner::default(),
            buffer: IntentBuffer::default(),
            scouting,
            router,
            cfg: cfg(),
        }
    }

    fn home(&self) -> RoomName {
        self.homes[0].0
    }

    fn add_explorer(&mut self, at: Position) -> u128 {
        let key = 500 + self.explorers.len() as u128;
        let home = self.home();
        self.explorers.push(Creep {
            key,
            serial: key as u64,
            home,
            at,
            death: self.time + CREEP_LIFE_TIME,
            spawning: false,
        });
        key
    }

    /// Makes `room` visible this tick with a read of its sources and a neutral controller.
    fn see(&mut self, name: RoomName) {
        let mut read = IntelRead {
            read: true,
            controller_at: Some(pos(name, 20, 20)),
            ..IntelRead::default()
        };
        read.add_source(pos(name, 5, 5));
        read.add_source(pos(name, 40, 40));
        self.seen.push(Seen {
            name,
            controller: Controller::Neutral,
            read: Some(read),
        });
    }

    /// Every room within reach of the homes read at `seen`.
    fn all_read(&mut self, seen: u32) {
        let reach = self.cfg.max_distance as i32;
        for (home, _) in self.homes.clone() {
            for dx in -reach..=reach {
                for dy in -reach..=reach {
                    let r = home.checked_add((dx, dy)).expect("room");
                    if crate::economy::is_remote_room(r) && !self.homes.iter().any(|h| h.0 == r) {
                        let mut e = RoomIntel::new(r, seen);
                        e.known = true;
                        self.scouting.atlas.put(e);
                    }
                }
            }
        }
    }

    fn take_snapshot(&mut self) {
        self.snap
            .begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
        for (row, c) in self.explorers.iter().enumerate() {
            let r = CreepRow {
                pos: c.at,
                my: true,
                spawning: c.spawning,
                fatigue: 0,
            };
            let id = Identity {
                role: Role::Explorer,
                home: c.home,
                remote: None,
                serial: c.serial,
            };
            self.snap
                .creeps
                .push_named(ObjectKey(c.key), r, || Some(id))
                .expect("creep");
            self.snap.creeps.set_death(row, c.death);
        }
        for (i, &(home, level)) in self.homes.iter().enumerate() {
            let row = self.snap.rooms.push(home, Controller::Mine { level });
            self.snap.rooms.set_energy(
                row,
                RoomEnergy {
                    available: 300,
                    capacity: 300,
                },
            );
            let spawn = pos(home, HOME_SPAWN.0, HOME_SPAWN.1);
            let h = self
                .snap
                .structures
                .push(ObjectKey(1 + i as u128), || {
                    Ok(StructureStatic {
                        kind: StructureType::Spawn,
                        pos: spawn,
                    })
                })
                .expect("spawn");
            self.snap.spawns.push(h, 0, false);
        }
        for &(r, level) in &self.claimed {
            self.snap.rooms.push(r, Controller::Mine { level });
        }
        for (i, &r) in self.observers.iter().enumerate() {
            self.snap
                .structures
                .push(ObjectKey(100 + i as u128), || {
                    Ok(StructureStatic {
                        kind: StructureType::Observer,
                        pos: pos(r, 30, 30),
                    })
                })
                .expect("observer");
        }
        for s in self.seen.clone() {
            let row = self.snap.rooms.push(s.name, s.controller);
            if let Some(read) = s.read {
                self.snap.rooms.set_intel(row, read);
            }
        }
        self.snap.finish();
    }

    fn begin(&mut self) {
        self.take_snapshot();
        let mcfg = MOVEMENT_MMO;
        self.movement.begin(&self.snap, &mcfg);
        let mut rooms = Vec::new();
        self.movement.first_reads(&mcfg, &mut rooms);
        let mut again = Vec::new();
        self.movement.rescans(&mcfg, &mut again);
        rooms.extend(again);
        let plain = vec![0u8; ROOM_AREA];
        for &r in &rooms {
            assert!(self.movement.set_terrain(r, &plain));
            self.movement.set_objects(r, []);
        }
        self.buffer.begin(&self.snap);
        self.spawner.begin(&self.snap, &SPAWN_MMO);
        self.scouting.begin(&self.snap, &self.cfg);
    }

    fn work(&mut self) {
        let mut commit = Commit::default();
        self.scouting.request(&mut self.spawner, &self.cfg, true);
        self.scouting
            .work(
                &self.snap,
                &mut self.movement,
                &mut self.buffer,
                &self.cfg,
                &MOVEMENT_MMO,
                &mut commit,
            )
            .expect("work");
        self.scouting.observe(&self.snap, &self.cfg, &mut commit);
        self.buffer.resolve();
    }

    fn tick(&mut self) {
        self.begin();
        self.work();
        self.seen.clear();
        self.time += 1;
    }

    fn requests(&self) -> Vec<(Role, RoomName, Body, u8)> {
        (0..self.spawner.stats().requests)
            .filter_map(|i| self.spawner.request_at(i))
            .map(|r| {
                let BodySpec::Exact(b) = r.body else {
                    panic!("the explorer's body is exact");
                };
                (r.role, r.home, b, r.priority)
            })
            .collect()
    }

    fn intents(&self) -> Vec<CreepIntent> {
        self.buffer
            .planned()
            .filter(|i| !matches!(i.intent, CreepIntent::Move { .. }))
            .map(|i| i.intent)
            .collect()
    }

    fn moves(&self) -> usize {
        self.buffer
            .planned()
            .filter(|i| matches!(i.intent, CreepIntent::Move { .. }))
            .count()
    }

    fn trip(&self) -> Option<RoomName> {
        self.scouting
            .explorer()
            .and_then(|e| e.trip)
            .map(|t| t.room)
    }
}

// ---- The explorer ----

#[test]
fn a_due_room_requests_one_explorer_of_one_move_part() {
    let mut w = World::new();
    w.tick();
    let body = Body::new(&[Part::Move]).expect("body");
    assert_eq!(
        w.requests(),
        [(Role::Explorer, w.home(), body, PRIORITY_EXPLORER)]
    );
    // Not on a tick the Optional tier does not run.
    w.begin();
    w.scouting.request(&mut w.spawner, &w.cfg, false);
    assert!(w.requests().is_empty());
}

#[test]
fn nothing_is_requested_before_the_record_loads_while_disabled_or_below_the_level() {
    let mut w = World::new();
    w.scouting = Scouting::default();
    w.tick();
    assert!(w.requests().is_empty(), "the record has not loaded");
    assert!(w.scouting.wants_record(&w.cfg));
    let mut w = World::new();
    w.cfg.enabled = false;
    w.tick();
    assert!(w.requests().is_empty(), "disabled");
    assert!(!w.scouting.wants_record(&w.cfg));
    let mut w = World::new();
    w.homes[0].1 = u8::try_from(w.cfg.min_level).expect("level") - 1;
    w.tick();
    assert!(w.requests().is_empty(), "below scout.min_level");
    assert!(w.scouting.homes().is_empty());
}

#[test]
fn nothing_due_requests_nothing_until_the_earliest_refresh() {
    let mut w = World::new();
    w.all_read(w.time);
    let first = w.time;
    w.tick();
    assert!(w.requests().is_empty());
    let (due, next) = w.scouting.due_count(&w.cfg);
    assert_eq!((due, next), (0, first + w.cfg.refresh_ticks));
    w.time = first + w.cfg.refresh_ticks;
    w.tick();
    assert_eq!(w.requests().len(), 1, "every room is due again");
}

#[test]
fn the_explorer_walks_to_the_nearest_due_room_and_moves_on_once_it_is_read() {
    let mut w = World::new();
    w.all_read(w.time);
    // Two rooms due: E3N2 and E1N1.
    let refresh = w.cfg.refresh_ticks;
    for r in ["E3N2", "E1N1"] {
        w.scouting.atlas.put(RoomIntel {
            known: true,
            ..RoomIntel::new(room(r), w.time - refresh)
        });
    }
    let home = w.home();
    w.add_explorer(pos(home, 49, 25));
    w.tick();
    // E3N2 is one room east of the explorer on the east edge.
    assert_eq!(w.trip(), Some(room("E3N2")));
    assert!(w.moves() > 0 || w.scouting.explorer().is_some());
    // It arrives: the room is visible and read.
    w.explorers[0].at = pos(room("E3N2"), 0, 25);
    w.see(room("E3N2"));
    w.tick();
    let e = w.scouting.atlas().get(room("E3N2")).expect("entry");
    assert!(e.known && e.seen == w.time - 1);
    assert_eq!(e.source_count(), 2);
    assert_eq!(w.trip(), Some(room("E1N1")), "the next due room");
    assert!(
        w.scouting.events().iter().any(|ev| matches!(
            ev,
            ScoutEvent::Read {
                first: false,
                changed: false,
                ..
            }
        )) || w.scouting.stats().reads == 1
    );
    assert_eq!(w.scouting.stats().reads, 1);
}

#[test]
fn with_nothing_left_the_explorer_ends() {
    let mut w = World::new();
    w.all_read(w.time);
    let home = w.home();
    w.add_explorer(pos(home, 25, 25));
    w.tick();
    assert_eq!(w.intents(), [CreepIntent::Suicide]);
    assert!(w.scouting.explorer().is_some_and(|e| e.ending));
    // Gone after the suicide: no death counted.
    w.explorers.clear();
    w.tick();
    assert_eq!(w.scouting.stats().deaths, 0);
}

#[test]
fn an_explorer_lost_early_counts_a_death() {
    let mut w = World::new();
    let home = w.home();
    w.add_explorer(pos(room("E3N2"), 10, 10));
    w.tick();
    w.explorers.clear();
    w.tick();
    assert_eq!(w.scouting.stats().deaths, 1);
    assert!(
        w.scouting
            .events()
            .contains(&ScoutEvent::Died { room: room("E3N2") })
    );
    let _ = home;
}

#[test]
fn a_room_with_no_route_is_marked_unreachable_and_waits() {
    let mut w = World::new();
    w.all_read(w.time);
    let refresh = w.cfg.refresh_ticks;
    w.scouting.atlas.put(RoomIntel {
        known: true,
        ..RoomIntel::new(room("E3N2"), w.time - refresh)
    });
    w.router.borrow_mut().fail.push(room("E3N2"));
    let home = w.home();
    w.add_explorer(pos(home, 25, 25));
    w.tick();
    let e = w.scouting.atlas().get(room("E3N2")).expect("entry");
    assert!(e.unreachable && e.known);
    assert_eq!(e.seen, w.time - 1);
    assert!(
        w.scouting
            .events()
            .contains(&ScoutEvent::Unreachable { room: room("E3N2") })
    );
    // Nothing else is due, so the explorer ends next tick.
    w.tick();
    assert_eq!(w.intents(), [CreepIntent::Suicide]);
}

#[test]
fn a_route_longer_than_a_life_from_a_home_is_unreachable_and_elsewhere_passed_over() {
    let mut w = World::new();
    w.all_read(w.time);
    let refresh = w.cfg.refresh_ticks;
    for r in ["E3N2", "E1N1"] {
        w.scouting.atlas.put(RoomIntel {
            known: true,
            ..RoomIntel::new(room(r), w.time - refresh)
        });
    }
    w.router.borrow_mut().long = Some(CREEP_LIFE_TIME as usize);
    // On the home's east edge, E3N2 is the nearer.
    let home = w.home();
    w.add_explorer(pos(home, 49, 25));
    w.tick();
    assert!(
        w.scouting
            .atlas()
            .get(room("E3N2"))
            .is_some_and(|e| e.unreachable),
        "from the home room no life would do"
    );
    // Out of the home room, a too-long route passes the room over and ends the explorer, so a
    // fresh one starts from the home (no search a tick for each far room in turn).
    w.explorers[0].at = pos(room("E2N1"), 25, 25);
    w.tick();
    let e = w.scouting.atlas().get(room("E1N1")).expect("entry");
    assert!(!e.unreachable);
    assert!(w.scouting.skip.contains(&room("E1N1")));
    assert_eq!(w.intents(), [CreepIntent::Suicide]);
}

#[test]
fn a_search_out_of_ops_marks_from_a_home_only() {
    let refresh = cfg().refresh_ticks;
    for (start, marked) in [
        (pos(room("E2N2"), 49, 25), true),
        (pos(room("E2N1"), 49, 25), false),
    ] {
        let mut w = World::new();
        w.all_read(w.time);
        w.scouting.atlas.put(RoomIntel {
            known: true,
            ..RoomIntel::new(room("E3N2"), w.time - refresh)
        });
        w.router.borrow_mut().incomplete.push(room("E3N2"));
        w.add_explorer(start);
        w.tick();
        let e = w.scouting.atlas().get(room("E3N2")).expect("entry");
        assert_eq!(e.unreachable, marked, "from {start:?}");
    }
}

#[test]
fn owned_rooms_that_are_no_home_are_never_due() {
    let mut w = World::new();
    w.all_read(w.time);
    // E3N2 is claimed at level 1 with no spawn: owned, no home, and never read by the snapshot.
    w.scouting.atlas.retain(|e| e.room != room("E3N2"));
    w.claimed.push((room("E3N2"), 1));
    let home = w.home();
    w.add_explorer(pos(home, 49, 25));
    w.tick();
    assert_eq!(w.trip(), None);
    assert_eq!(w.intents(), [CreepIntent::Suicide], "nothing left to take");
    assert_eq!(w.scouting.due_count(&w.cfg).0, 0);
    // An explorer on a trip there drops it once the room is claimed.
    let mut w = World::new();
    w.all_read(w.time);
    w.scouting.atlas.retain(|e| e.room != room("E3N2"));
    w.add_explorer(pos(home, 49, 25));
    w.tick();
    assert_eq!(w.trip(), Some(room("E3N2")));
    w.claimed.push((room("E3N2"), 1));
    w.tick();
    assert_ne!(w.trip(), Some(room("E3N2")));
}

#[test]
fn explorers_keep_out_of_a_room_one_died_in() {
    let mut w = World::new();
    w.all_read(w.time);
    let refresh = w.cfg.refresh_ticks;
    for r in ["E3N2", "E1N1"] {
        w.scouting.atlas.put(RoomIntel {
            known: true,
            ..RoomIntel::new(room(r), w.time - refresh)
        });
    }
    let home = w.home();
    w.add_explorer(pos(room("E3N2"), 10, 10));
    w.tick();
    // It dies there.
    w.explorers.clear();
    w.tick();
    assert_eq!(w.scouting.stats().deaths, 1);
    // The next one goes to E1N1, and its route keeps out of E3N2.
    w.add_explorer(pos(home, 49, 25));
    w.tick();
    assert_eq!(w.trip(), Some(room("E1N1")));
    let avoided = w
        .router
        .borrow()
        .queries
        .last()
        .map(|(_, a)| a.clone())
        .expect("a search");
    assert!(avoided.contains(&room("E3N2")), "{avoided:?}");
    // After scout.refresh_ticks it is a target again.
    w.time += refresh;
    w.tick();
    assert!(w.scouting.deaths.is_empty());
}

#[test]
fn towered_rooms_are_never_targets_and_routes_avoid_them() {
    let mut w = World::new();
    w.all_read(w.time);
    let refresh = w.cfg.refresh_ticks;
    // E3N2 shows another owner's towers and is due; E1N1 is due and plain.
    w.scouting.atlas.put(RoomIntel {
        known: true,
        controller: Controller::Foreign { level: 4 },
        towers: 1,
        ..RoomIntel::new(room("E3N2"), w.time - refresh)
    });
    w.scouting.atlas.put(RoomIntel {
        known: true,
        ..RoomIntel::new(room("E1N1"), w.time - refresh)
    });
    let home = w.home();
    w.add_explorer(pos(home, 25, 25));
    w.tick();
    assert_eq!(w.trip(), Some(room("E1N1")));
    let (due, _) = w.scouting.due_count(&w.cfg);
    assert_eq!(due, 2, "the console counts both");
    let queries = &w.router.borrow().queries;
    let (_, avoided) = queries.last().expect("a search");
    assert!(avoided.contains(&room("E3N2")), "{avoided:?}");
}

#[test]
fn the_explorer_steps_back_out_of_a_towered_room() {
    let mut w = World::new();
    w.all_read(w.time);
    let refresh = w.cfg.refresh_ticks;
    w.scouting.atlas.put(RoomIntel {
        known: true,
        ..RoomIntel::new(room("E1N1"), w.time - refresh)
    });
    // It arrives on the west edge of E3N2 and the read shows another owner's towers.
    w.add_explorer(pos(room("E3N2"), 0, 25));
    w.seen.push(Seen {
        name: room("E3N2"),
        controller: Controller::Foreign { level: 3 },
        read: Some(IntelRead {
            read: true,
            towers: 1,
            ..IntelRead::default()
        }),
    });
    w.tick();
    assert_eq!(w.moves(), 0, "it stands on the edge");
    assert!(w.scouting.events().contains(&ScoutEvent::Retreated {
        room: room("E3N2"),
        towers: 1
    }));
    assert_eq!(w.scouting.stats().retreats, 1);
}

#[test]
fn only_one_explorer_lives() {
    let mut w = World::new();
    let home = w.home();
    w.add_explorer(pos(home, 20, 20));
    w.add_explorer(pos(home, 21, 21));
    w.tick();
    let ends = w
        .intents()
        .iter()
        .filter(|i| matches!(i, CreepIntent::Suicide))
        .count();
    assert_eq!(ends, 1, "the second ends");
    assert!(w.requests().is_empty(), "one is alive");
}

#[test]
fn wants_read_needs_a_due_room_in_reach() {
    let mut w = World::new();
    w.tick();
    let now = w.time;
    let c = w.cfg;
    assert!(w.scouting.wants_read(room("E3N3"), now, &c));
    assert!(
        !w.scouting.wants_read(room("E4N2"), now, &c),
        "out of reach"
    );
    w.scouting.atlas.put(RoomIntel {
        known: true,
        ..RoomIntel::new(room("E3N3"), now)
    });
    assert!(!w.scouting.wants_read(room("E3N3"), now, &c), "fresh");
    assert!(
        w.scouting
            .wants_read(room("E3N3"), now + c.refresh_ticks, &c)
    );
    // A highway room never has a controller: never read.
    let mut far = w.cfg;
    far.max_distance = 3;
    assert!(!w.scouting.wants_read(room("E0N2"), now, &far));
    let mut off = c;
    off.enabled = false;
    assert!(!w.scouting.wants_read(room("E1N1"), now, &off));
    let unloaded = Scouting::default();
    assert!(!unloaded.wants_read(room("E1N1"), now, &c));
}

#[test]
fn the_record_is_saved_when_changed_at_most_once_per_window() {
    let mut w = World::new();
    w.tick();
    assert!(!w.scouting.take_save(&w.cfg), "nothing changed");
    w.see(room("E1N1"));
    w.tick();
    assert!(w.scouting.is_dirty());
    assert!(w.scouting.take_save(&w.cfg));
    w.see(room("E1N2"));
    w.tick();
    assert!(!w.scouting.take_save(&w.cfg), "within scout.save_ticks");
    w.time += w.cfg.save_ticks;
    w.tick();
    assert!(w.scouting.take_save(&w.cfg));
    let mut out = Vec::new();
    w.scouting.payload(&mut out, MAX_PAYLOAD);
    let back = Atlas::decode(&out).expect("decodes");
    assert_eq!(back.len(), 2);
    // A record that could not be read is never written over.
    let mut s = Scouting::default();
    s.loaded(RecordLoad::Unavailable).expect("loads");
    assert!(!s.save_due(&w.cfg));
}

#[test]
fn a_loaded_record_is_used_and_a_bad_one_refused() {
    let a = sample_atlas();
    let mut out = Vec::new();
    a.encode(&mut out, MAX_PAYLOAD);
    let mut s = Scouting::default();
    assert_eq!(s.loaded(RecordLoad::Payload(&out)), Ok(()));
    assert_eq!(s.atlas(), &a);
    assert!(!s.is_dirty());
    let mut s = Scouting::default();
    assert_eq!(
        s.loaded(RecordLoad::Payload(&out[..3])),
        Err(RecordError::Short)
    );
    assert!(
        s.is_loaded() && s.is_dirty(),
        "written again on the next save"
    );
}

#[test]
fn rooms_out_of_every_homes_reach_are_pruned_when_the_homes_change() {
    let mut w = World::new();
    w.scouting.atlas.put(RoomIntel::new(room("E1N1"), 10));
    w.scouting.atlas.put(RoomIntel::new(room("E9N9"), 10));
    w.tick();
    assert!(w.scouting.atlas().get(room("E9N9")).is_none());
    assert!(w.scouting.atlas().get(room("E1N1")).is_some());
    assert!(
        w.scouting
            .events()
            .contains(&ScoutEvent::Pruned { rooms: 1 })
    );
    // A second home near E9N9 keeps what is read there.
    w.scouting.atlas.put(RoomIntel::new(room("E9N9"), 10));
    w.homes.push((room("E8N8"), 5));
    w.tick();
    assert!(w.scouting.atlas().get(room("E9N9")).is_some());
}

// ---- Observers ----

/// A home at E13N13 (so every room within `OBSERVER_RANGE` of it lies off this sector's center),
/// reaching one room past the range, at level `level`, with an observer.
fn observing(level: u8) -> World {
    let mut w = World::new();
    w.homes = vec![(room("E13N13"), level)];
    w.cfg.max_distance = OBSERVER_RANGE + 1;
    w.observers = vec![room("E13N13")];
    w
}

#[test]
fn an_observer_looks_at_the_stalest_due_room_in_range() {
    let level = (1..=8u8)
        .find(|&l| StructureType::Observer.controller_structures(u32::from(l)) > 0)
        .expect("an observer at some level");
    let mut w = observing(level);
    w.all_read(w.time);
    let refresh = w.cfg.refresh_ticks;
    let r = OBSERVER_RANGE as i32;
    let home = w.home();
    let edge = home.checked_add((r, 0)).expect("room");
    let older = home.checked_add((-r, 2)).expect("room");
    let past = home.checked_add((r + 1, 0)).expect("room");
    // Due: one at the range's edge read long ago, one read longer ago, one past the range.
    let start = w.time;
    for (room, seen) in [(edge, 10), (older, 5), (past, 1)] {
        w.scouting.atlas.put(RoomIntel {
            known: true,
            ..RoomIntel::new(room, seen)
        });
    }
    // The other rooms are not due yet, for the three ticks below.
    w.time = start + refresh - 10;
    w.tick();
    assert_eq!(
        w.scouting
            .looks()
            .iter()
            .map(|l| l.room)
            .collect::<Vec<_>>(),
        [older],
        "the oldest in range"
    );
    // Once it is read, the next look goes to the edge room, never past the range.
    w.scouting.atlas.put(RoomIntel {
        known: true,
        ..RoomIntel::new(older, w.time)
    });
    w.tick();
    assert_eq!(
        w.scouting
            .looks()
            .iter()
            .map(|l| l.room)
            .collect::<Vec<_>>(),
        [edge]
    );
    // A visible room is not looked at: it is read for free.
    w.see(edge);
    w.tick();
    assert!(w.scouting.looks().is_empty(), "{:?}", w.scouting.looks());
    assert!(w.scouting.stats().looks >= 2);
}

#[test]
fn a_room_never_seen_comes_before_any_read_one() {
    let mut w = observing(8);
    w.all_read(w.time);
    let home = w.home();
    let never = home.checked_add((3, 2)).expect("room");
    assert!(crate::economy::is_remote_room(never));
    w.scouting.atlas.retain(|e| e.room != never);
    let refresh = w.cfg.refresh_ticks;
    w.scouting.atlas.put(RoomIntel {
        known: true,
        ..RoomIntel::new(home.checked_add((1, 1)).expect("room"), 0)
    });
    // The other rooms are not due yet; the one read at tick 0 and the one never read are.
    w.time += refresh - 1;
    w.tick();
    assert_eq!(
        w.scouting.looks().first().map(|l| l.room),
        Some(never),
        "no read at all comes first"
    );
}

#[test]
fn no_observer_looks_below_its_level_when_nothing_is_due_or_when_off() {
    let below = (1..=8u8)
        .rev()
        .find(|&l| StructureType::Observer.controller_structures(u32::from(l)) == 0)
        .expect("a level without observers");
    let mut w = observing(below);
    w.tick();
    assert!(w.scouting.looks().is_empty(), "inactive below its level");
    let mut w = observing(8);
    w.all_read(w.time);
    w.tick();
    assert!(w.scouting.looks().is_empty(), "nothing due");
    let mut w = observing(8);
    w.cfg.observers = false;
    w.tick();
    assert!(w.scouting.looks().is_empty(), "observers off");
}

#[test]
fn a_look_that_gives_no_vision_waits() {
    let mut w = observing(8);
    w.all_read(w.time);
    let home = w.home();
    let far = home.checked_add((OBSERVER_RANGE as i32, 0)).expect("room");
    w.scouting.atlas.retain(|e| e.room != far);
    w.tick();
    assert_eq!(
        w.scouting
            .looks()
            .iter()
            .map(|l| l.room)
            .collect::<Vec<_>>(),
        [far]
    );
    // The room does not show on the next tick (past the world's border, say).
    w.tick();
    assert!(
        w.scouting
            .events()
            .contains(&ScoutEvent::Unseen { room: far })
    );
    let e = w.scouting.atlas().get(far).expect("entry");
    assert!(e.unreachable && !e.known);
    assert!(
        w.scouting.looks().is_empty(),
        "it waits scout.refresh_ticks"
    );
}

#[test]
fn two_observers_take_two_rooms() {
    let mut w = observing(8);
    w.homes.push((room("E15N13"), 8));
    w.observers.push(room("E15N13"));
    w.tick();
    let looks: Vec<RoomName> = w.scouting.looks().iter().map(|l| l.room).collect();
    assert_eq!(looks.len(), 2);
    assert_ne!(looks[0], looks[1]);
}
