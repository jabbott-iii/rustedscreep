//! Tests of the controller signer (ADR-030, `signing`). Expected values derive from the constants
//! (`CONTROLLER_SIGN_MAX_LENGTH`, `BODYPART_HITS`, `MOVE_COST_*`, tower damage by range) and the
//! signer's config.

use std::cell::RefCell;
use std::rc::Rc;

use rustedscreep_consts::extra::BODYPART_HITS;
use rustedscreep_consts::{
    CONTROLLER_SIGN_MAX_LENGTH, CREEP_LIFE_TIME, MAX_CREEP_SIZE, MOVE_COST_PLAIN, MOVE_COST_SWAMP,
    Part, Position, ROOM_AREA, ROOM_SIZE, RoomCoordinate, RoomName, StructureType, Terrain,
};

use super::*;
use crate::config::{MOVEMENT_MMO, SIGN_MMO, SPAWN_MMO, SignConfig};
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{
    Movement, RouteOutcome, RouteQuery, RouteRooms, RouteSearch, TerrainGrid, Tile,
};
use crate::planner::validate::tower_damage;
use crate::scheduler::Commit;
use crate::snapshot::{
    Controller, CreepRow, ObjectKey, ObjectKind, ObjectRow, ObjectStatic, RoomEnergy, SIGN_TOWERS,
    SignRead, SignSeen, Snapshot, StructureStatic, TickContext,
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

fn terrain(fill: u8) -> TerrainGrid {
    TerrainGrid::from_row_major(&vec![fill; ROOM_AREA]).expect("terrain")
}

/// The tests' config: candidates one room around the home, so a test can name every one.
fn cfg() -> SignConfig {
    SignConfig {
        max_distance: 1,
        ..SIGN_MMO
    }
}

// ---- The text, the record ----

#[test]
fn the_text_fits_a_sign() {
    assert_eq!(
        SIGN_TEXT,
        "jabbott.dev rustedscreep bot - https://github.com/jabbott-iii/rustedscreep"
    );
    let units = SIGN_TEXT.encode_utf16().count();
    assert!(units <= CONTROLLER_SIGN_MAX_LENGTH as usize, "{units}");
}

#[test]
fn towers_kept_are_the_level_8_count() {
    let most = StructureType::Tower.controller_structures(8);
    assert_eq!(SIGN_TOWERS, most as usize);
    assert_eq!(MAX_TOWERS, most as usize);
}

#[test]
fn the_default_reach_is_one_creep_life() {
    // A room crossed edge to edge takes at least ROOM_SIZE - 1 steps.
    let straight = CREEP_LIFE_TIME / (u32::from(ROOM_SIZE) - 1) + 1;
    assert_eq!(SIGN_MMO.max_distance, straight);
}

#[test]
fn states_round_trip_their_codes() {
    for s in SignState::ALL {
        assert_eq!(SignState::from_code(s.code()), Some(s));
    }
    assert_eq!(SignState::from_code(6), None);
}

fn sample_book() -> Book {
    let mut b = Book::default();
    let cases = [
        ("E1N1", SignState::Signed, Some(tile(10, 10)), 0),
        ("E1N2", SignState::Unsigned, None, 0),
        ("E2N1", SignState::Deadly, Some(tile(20, 30)), MAX_TOWERS),
        ("E2N3", SignState::Unreachable, None, 1),
        ("E3N3", SignState::System, Some(tile(1, 48)), 0),
        ("E3N1", SignState::NoController, None, 0),
    ];
    for (i, &(name, state, controller, towers)) in cases.iter().enumerate() {
        let e = b.entry(room(name), 100 + i as u32).expect("entry");
        e.state = state;
        e.controller = controller;
        e.towers = (0..towers).map(|t| tile(5 + t as u8, 6)).collect();
        e.deaths = i as u8;
        e.overwrites = (i % 3) as u8;
    }
    b
}

#[test]
fn the_record_round_trips() {
    let b = sample_book();
    let mut out = Vec::new();
    b.encode(&mut out);
    let towers: usize = b.entries().iter().map(|e| e.towers.len()).sum();
    assert_eq!(out.len(), payload_len(b.len(), towers));
    assert_eq!(Book::decode(&out), Ok(b.clone()));
    // Entries stay in room order.
    let packed: Vec<u16> = b.entries().iter().map(|e| e.room.packed_repr()).collect();
    let mut sorted = packed.clone();
    sorted.sort_unstable();
    assert_eq!(packed, sorted);
    assert_eq!(Book::decode(&[0, 0]), Ok(Book::default()));
}

#[test]
fn the_record_refuses_bad_payloads() {
    let b = sample_book();
    let mut good = Vec::new();
    b.encode(&mut good);
    assert_eq!(
        Book::decode(&good[..good.len() - 1]),
        Err(RecordError::Short)
    );
    assert_eq!(Book::decode(&[]), Err(RecordError::Short));
    let mut long = good.clone();
    long.push(0);
    assert_eq!(Book::decode(&long), Err(RecordError::Long));
    // The first entry: room (2 bytes after the count), state, seen, controller, ...
    let mut state = good.clone();
    state[4] = 9;
    assert_eq!(Book::decode(&state), Err(RecordError::State(9)));
    let mut tile_off = good.clone();
    let bad = ROOM_AREA as u16;
    tile_off[9..11].copy_from_slice(&bad.to_le_bytes());
    assert_eq!(Book::decode(&tile_off), Err(RecordError::Tile(bad)));
    let mut towers = good.clone();
    towers[13] = MAX_TOWERS as u8 + 1;
    assert_eq!(
        Book::decode(&towers),
        Err(RecordError::Towers(MAX_TOWERS as u8 + 1))
    );
    // Two entries in the wrong order.
    let mut two = Book::default();
    two.entry(room("E1N1"), 1).expect("a");
    two.entry(room("E5N5"), 1).expect("b");
    let mut out = Vec::new();
    two.encode(&mut out);
    let first = out[2..14].to_vec();
    let second = out[14..26].to_vec();
    let mut swapped = out[..2].to_vec();
    swapped.extend(second);
    swapped.extend(first);
    assert_eq!(Book::decode(&swapped), Err(RecordError::Order));
}

#[test]
fn due_follows_the_state() {
    let c = cfg();
    let mut e = Entry::new(room("E1N1"), 1000);
    assert_eq!(e.due_at(&c), 0, "unsigned: at once");
    e.state = SignState::Signed;
    assert_eq!(e.due_at(&c), 1000 + c.recheck_ticks);
    for s in [SignState::System, SignState::Unreachable, SignState::Deadly] {
        e.state = s;
        assert_eq!(e.due_at(&c), 1000 + c.retry_ticks, "{s:?}");
    }
    e.state = SignState::NoController;
    assert_eq!(e.due_at(&c), u32::MAX);
    e.state = SignState::Unsigned;
    e.deaths = c.max_deaths as u8;
    assert_eq!(e.due_at(&c), 1000 + c.retry_ticks, "too many deaths");
    e.deaths = 0;
    e.overwrites = c.max_overwrites as u8;
    assert_eq!(e.due_at(&c), 1000 + c.retry_ticks, "too many overwrites");
    let empty = Book::default();
    assert_eq!(empty.due_at(room("E1N1"), &c), 0, "an unknown room");
}

// ---- The body ----

#[test]
fn a_tile_holds_a_body_for_its_fatigue() {
    let plain = u64::from(MOVE_COST_PLAIN);
    let swamp = u64::from(MOVE_COST_SWAMP);
    for (tough, moves) in [(0u32, 1u32), (1, 1), (2, 1), (3, 2), (10, 1), (5, 5)] {
        let shed = 2 * u64::from(moves);
        let expect = |rate: u64| (u64::from(tough) * rate).div_ceil(shed).max(1);
        let mut e = Exposure::default();
        e.add(Step::Plain, 0);
        assert_eq!(
            e.ticks(tough, moves),
            expect(plain) + 2,
            "plain {tough}/{moves}"
        );
        let mut e = Exposure::default();
        e.add(Step::Swamp, 0);
        assert_eq!(
            e.ticks(tough, moves),
            expect(swamp) + 2,
            "swamp {tough}/{moves}"
        );
        let mut e = Exposure::default();
        e.add(Step::Free, 0);
        assert_eq!(e.ticks(tough, moves), 3, "free {tough}/{moves}");
    }
}

#[test]
fn a_safe_trip_gets_one_move_part() {
    let mut e = Exposure::default();
    for _ in 0..200 {
        e.add(Step::Swamp, 0);
    }
    let body = signer_body(&e, CREEP_LIFE_TIME, 300, 25).expect("body");
    assert_eq!(body.parts(), &[Part::Move]);
    assert_eq!(body.cost(), Part::Move.cost());
    assert_eq!(plain_signer().expect("plain").parts(), &[Part::Move]);
}

/// Whether a body of `tough` and `moves` survives `e` and fits `life`.
fn fits(e: &Exposure, tough: u32, moves: u32, margin: u32, life: u32) -> bool {
    let w = e.walk(tough, moves, margin);
    w.survives && w.ticks <= u64::from(life)
}

#[test]
fn the_sized_body_is_the_cheapest_that_survives() {
    // One tower far off: every tile at TOWER_FALLOFF_RANGE or more takes its least damage.
    let towers = [tile(45, 25)];
    for (swamps, length) in [(0usize, 3usize), (2, 8), (5, 12), (0, 20)] {
        let mut e = Exposure::default();
        // Ten safe tiles before the room.
        for _ in 0..10 {
            e.add(Step::Plain, 0);
        }
        e.add(Step::Free, damage_at(tile(0, 25), &towers));
        for i in 1..length {
            let at = tile(i as u8, 25);
            let step = if i <= swamps {
                Step::Swamp
            } else {
                Step::Plain
            };
            e.add(step, damage_at(at, &towers));
        }
        let margin = 25;
        let body = signer_body(&e, CREEP_LIFE_TIME, 10_000, margin).expect("a body");
        let tough = body.count(Part::Tough) as u32;
        let moves = body.count(Part::Move) as u32;
        assert_eq!(tough as usize + moves as usize, body.len());
        // TOUGH in front, MOVE behind.
        assert!(
            body.parts()
                .iter()
                .take(tough as usize)
                .all(|&p| p == Part::Tough)
        );
        assert!(
            fits(&e, tough, moves, margin, CREEP_LIFE_TIME),
            "{swamps}/{length}"
        );
        // No cheaper body survives.
        for m in 1..=MAX_CREEP_SIZE {
            for t in 0..=MAX_CREEP_SIZE - m {
                let cost = t * Part::Tough.cost() + m * Part::Move.cost();
                if cost < body.cost() {
                    assert!(
                        !fits(&e, t, m, margin, CREEP_LIFE_TIME),
                        "{t} TOUGH and {m} MOVE are cheaper than {tough}/{moves} ({swamps}/{length})"
                    );
                }
            }
        }
    }
}

/// Two ticks under one far tower (the landing and one plain step): two TOUGH and two MOVE parts
/// hold out for less than the four MOVE parts the hits alone would take.
#[test]
fn tough_parts_buy_hits_cheaper_than_move_parts() {
    let towers = [tile(45, 25)];
    let per = damage_at(tile(0, 25), &towers);
    assert_eq!(per, tower_damage(45));
    let margin = 25;
    let mut e = Exposure::default();
    e.add(Step::Free, per);
    e.add(Step::Plain, damage_at(tile(1, 25), &towers));
    let tick = (u64::from(per) * (100 + u64::from(margin))).div_ceil(100);
    let w = e.walk(2, 2, margin);
    assert_eq!(w.damage, 2 * tick);
    assert!(w.survives);
    // Hits alone: the fewest MOVE parts whose hits exceed the damage.
    let only_moves = (2 * tick / u64::from(BODYPART_HITS) + 1) as u32;
    assert!(e.walk(0, only_moves, margin).survives);
    assert!(!e.walk(0, only_moves - 1, margin).survives);
    let body = signer_body(&e, CREEP_LIFE_TIME, 10_000, margin).expect("body");
    assert_eq!(
        body.parts(),
        &[Part::Tough, Part::Tough, Part::Move, Part::Move]
    );
    assert!(body.cost() < only_moves * Part::Move.cost());
}

/// A MOVE part lost to damage slows the walk: 2 TOUGH and 2 MOVE parts that lose one MOVE part
/// shed a plain step's fatigue (2 weighing parts at `MOVE_COST_PLAIN`) in two ticks, not one.
#[test]
fn lost_move_parts_slow_the_walk() {
    let hits = BODYPART_HITS;
    let walk = |first: u32| {
        let mut e = Exposure::default();
        e.add(Step::Free, first);
        for _ in 0..3 {
            e.add(Step::Plain, 1);
        }
        e.walk(2, 2, 0)
    };
    // The two TOUGH parts and one MOVE part go on the first tile.
    let hurt = walk(3 * hits);
    let calm = walk(1);
    assert!(hurt.survives && calm.survives);
    // The first plain step's fatigue comes off in the step's own tick, before that tick's damage
    // (V55, V278); the steps onto the second and third plain tiles take a tick more each (the
    // step onto the signing tile never waits).
    let per_step = (2 * u64::from(MOVE_COST_PLAIN)).div_ceil(2);
    assert_eq!(
        hurt.ticks - calm.ticks,
        2 * (per_step - 1),
        "{hurt:?} {calm:?}"
    );
}

#[test]
fn no_body_survives_too_much_or_costs_too_much() {
    let towers = [tile(25, 25); 6];
    let mut e = Exposure::default();
    // Thirty ticks next to six towers at full power.
    for _ in 0..30 {
        e.add(Step::Plain, damage_at(tile(25, 26), &towers));
    }
    assert_eq!(damage_at(tile(25, 26), &towers), 6 * tower_damage(1));
    assert!(signer_body(&e, CREEP_LIFE_TIME, 100_000, 25).is_none());
    // One tick at range 20 of one tower fits a big budget, not one MOVE part's cost.
    let mut e = Exposure::default();
    e.add(Step::Free, damage_at(tile(5, 25), &[tile(25, 25)]));
    assert!(signer_body(&e, CREEP_LIFE_TIME, 10_000, 25).is_some());
    assert!(signer_body(&e, CREEP_LIFE_TIME, Part::Move.cost(), 25).is_none());
    // A walk longer than the life left.
    let mut e = Exposure::default();
    for _ in 0..100 {
        e.add(Step::Plain, 0);
    }
    assert!(signer_body(&e, 50, 10_000, 25).is_none());
}

#[test]
fn steps_follow_the_edges_and_the_terrain() {
    let a = room("E2N2");
    let b = room("E3N2");
    let from = pos(a, 47, 25);
    // 48, the exit 49, then across: b's 0 (the landing), 1 (swamp), 2.
    let path = [
        pos(a, 48, 25),
        pos(a, 49, 25),
        pos(b, 0, 25),
        pos(b, 1, 25),
        pos(b, 2, 25),
    ];
    let plain = terrain(0);
    let swamp = terrain(Terrain::Swamp as u8);
    assert_eq!(step_at(from, &path, 0, &plain), Some(Step::Plain));
    assert_eq!(step_at(from, &path, 1, &plain), None, "an exit tile");
    assert_eq!(
        step_at(from, &path, 2, &swamp),
        Some(Step::Free),
        "the landing"
    );
    assert_eq!(step_at(from, &path, 3, &swamp), Some(Step::Swamp));
    assert_eq!(step_at(from, &path, 9, &swamp), None, "past the end");
    // Onto an edge from inside (a dip): no fatigue.
    let dip = [pos(a, 49, 20), pos(a, 48, 21)];
    assert_eq!(step_at(pos(a, 48, 19), &dip, 0, &plain), Some(Step::Free));
}

// ---- The manager ----

/// The fake route search: a straight walk in world coordinates, crossing edges as PathFinder does
/// (straight across, V166), that fails on a room held closed (avoided) or a target room it is told
/// to fail.
#[derive(Default)]
struct Router {
    fail: Vec<RoomName>,
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
        if r.fail.contains(&q.to.room_name()) || rooms.closed(q.to.room_name()) {
            return RouteOutcome::Failed;
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
            if path.len() > 5000 {
                return RouteOutcome::Incomplete;
            }
        }
        RouteOutcome::Found
    }
}

#[derive(Clone, Copy)]
struct Creep {
    key: u128,
    serial: u64,
    at: Position,
    death: u32,
    spawning: bool,
}

/// A room visible this tick other than the home: its controller and what a signer there reads.
#[derive(Clone)]
struct Seen {
    name: RoomName,
    controller: Controller,
    controller_at: Option<Position>,
    sign: SignSeen,
    towers: Vec<Position>,
}

const HOME_SPAWN: (u8, u8) = (25, 25);
const HOME_CONTROLLER: (u8, u8) = (10, 10);

struct World {
    time: u32,
    home: RoomName,
    home_sign: SignSeen,
    signers: Vec<Creep>,
    seen: Vec<Seen>,
    snap: Snapshot,
    movement: Movement,
    spawner: Spawner,
    buffer: IntentBuffer,
    signing: Signing,
    router: Rc<RefCell<Router>>,
    cfg: SignConfig,
    capacity: u32,
    level: u8,
}

impl World {
    fn new() -> Self {
        let router = Rc::new(RefCell::new(Router::default()));
        let mut movement = Movement::default();
        movement.set_router(Box::new(Shared(Rc::clone(&router))));
        let mut signing = Signing::default();
        signing.loaded(RecordLoad::Empty).expect("no record");
        Self {
            time: 1000,
            home: room("E2N2"),
            home_sign: SignSeen::None,
            signers: Vec::new(),
            seen: Vec::new(),
            snap: Snapshot::default(),
            movement,
            spawner: Spawner::default(),
            buffer: IntentBuffer::default(),
            signing,
            router,
            cfg: cfg(),
            capacity: 1800,
            level: 5,
        }
    }

    fn add_signer(&mut self, at: Position) -> u128 {
        let key = 500 + self.signers.len() as u128;
        self.signers.push(Creep {
            key,
            serial: key as u64,
            at,
            death: self.time + 1000,
            spawning: false,
        });
        key
    }

    fn signer_rooms(&self) -> Vec<RoomName> {
        self.signers.iter().map(|c| c.at.room_name()).collect()
    }

    fn take_snapshot(&mut self) {
        self.snap
            .begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
        for (row, c) in self.signers.iter().enumerate() {
            let r = CreepRow {
                pos: c.at,
                my: true,
                spawning: c.spawning,
                fatigue: 0,
            };
            let id = Identity {
                role: Role::Signer,
                home: self.home,
                remote: None,
                serial: c.serial,
            };
            self.snap
                .creeps
                .push_named(ObjectKey(c.key), r, || Some(id))
                .expect("creep");
            self.snap.creeps.set_death(row, c.death);
        }
        let standing = self.signer_rooms();
        let row = self
            .snap
            .rooms
            .push(self.home, Controller::Mine { level: self.level });
        self.snap.rooms.set_energy(
            row,
            RoomEnergy {
                available: self.capacity,
                capacity: self.capacity,
            },
        );
        let home_controller = pos(self.home, HOME_CONTROLLER.0, HOME_CONTROLLER.1);
        if standing.contains(&self.home) {
            self.snap.rooms.set_sign(
                row,
                SignRead {
                    read: true,
                    sign: self.home_sign,
                    ..SignRead::default()
                },
            );
            self.push_controller(1, home_controller);
        }
        let spawn = pos(self.home, HOME_SPAWN.0, HOME_SPAWN.1);
        let h = self
            .snap
            .structures
            .push(ObjectKey(1), || {
                Ok(StructureStatic {
                    kind: StructureType::Spawn,
                    pos: spawn,
                })
            })
            .expect("spawn");
        self.snap.spawns.push(h, 0, false);
        self.snap
            .structures
            .push(ObjectKey(2), || {
                Ok(StructureStatic {
                    kind: StructureType::Controller,
                    pos: home_controller,
                })
            })
            .expect("controller");
        for (i, s) in self.seen.clone().iter().enumerate() {
            let row = self.snap.rooms.push(s.name, s.controller);
            if standing.contains(&s.name) {
                let mut read = SignRead {
                    read: true,
                    sign: s.sign,
                    ..SignRead::default()
                };
                for &t in &s.towers {
                    read.add_tower(t);
                }
                self.snap.rooms.set_sign(row, read);
                if let Some(at) = s.controller_at {
                    self.push_controller(10 + i as u128, at);
                }
            }
        }
        self.snap.finish();
    }

    fn push_controller(&mut self, key: u128, at: Position) {
        self.snap
            .objects
            .push(
                ObjectKey(1000 + key),
                || {
                    Ok(ObjectStatic {
                        kind: ObjectKind::Controller,
                        pos: at,
                    })
                },
                ObjectRow::default(),
            )
            .expect("controller object");
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
        self.signing.begin(&self.snap, &self.cfg);
    }

    fn work(&mut self) {
        let mut commit = Commit::default();
        self.signing
            .work(
                &self.snap,
                &mut self.movement,
                &mut self.buffer,
                &self.cfg,
                &MOVEMENT_MMO,
                &mut commit,
            )
            .expect("work");
        self.buffer.resolve();
    }

    fn tick(&mut self) {
        self.begin();
        self.work();
        self.time += 1;
    }

    fn requests(&self) -> Vec<(Role, Body, u8)> {
        (0..self.spawner.stats().requests)
            .filter_map(|i| self.spawner.request_at(i))
            .map(|r| {
                let BodySpec::Exact(b) = r.body else {
                    panic!("the signer's body is exact");
                };
                (r.role, b, r.priority)
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

    /// Every candidate around the home signed at `seen`.
    fn all_signed(&mut self, seen: u32) {
        let reach = self.cfg.max_distance as i32;
        for dx in -reach..=reach {
            for dy in -reach..=reach {
                let r = self.home.checked_add((dx, dy)).expect("room");
                if crate::economy::is_remote_room(r) {
                    let e = self.signing.book.entry(r, seen).expect("entry");
                    e.state = SignState::Signed;
                    e.seen = seen;
                }
            }
        }
    }
}

#[test]
fn the_home_controller_comes_first_with_one_move_part() {
    let mut w = World::new();
    w.tick();
    let Plan::Ready {
        home, body, trip, ..
    } = w.signing.plan().clone()
    else {
        panic!("planned: {:?}", w.signing.plan());
    };
    assert_eq!(home, w.home);
    assert_eq!(body.parts(), &[Part::Move]);
    assert_eq!(trip.room, w.home);
    assert_eq!(trip.goal, pos(w.home, HOME_CONTROLLER.0, HOME_CONTROLLER.1));
    assert_eq!((trip.range, trip.look, trip.dangerous), (1, false, false));
    // The route search started at the spawn.
    let r = w.router.borrow();
    assert_eq!(r.queries[0].0.from, pos(w.home, HOME_SPAWN.0, HOME_SPAWN.1));
    drop(r);
    // The Critical request waits for the Optional tier.
    w.begin();
    w.signing.request(&mut w.spawner, &w.cfg, false);
    assert!(w.requests().is_empty());
    w.signing.request(&mut w.spawner, &w.cfg, true);
    assert_eq!(w.requests(), vec![(Role::Signer, body, PRIORITY_SIGNER)]);
}

#[test]
fn a_home_below_the_least_level_sends_no_signer() {
    let mut w = World::new();
    w.level = (w.cfg.min_level - 1) as u8;
    w.signing = Signing::default();
    w.begin();
    assert!(
        !w.signing.wants_record(&w.cfg),
        "its record is not even requested"
    );
    assert!(w.signing.homes().is_empty());
    w.level = w.cfg.min_level as u8;
    w.begin();
    assert!(w.signing.wants_record(&w.cfg));
    // Once loaded, the Optional work has a plan to make (with a route search installed).
    w.signing.loaded(RecordLoad::Empty).expect("empty");
    assert!(w.signing.has_work(&w.cfg, true));
    assert!(
        !w.signing.has_work(&w.cfg, false),
        "no route search: nothing to do"
    );
}

#[test]
fn nothing_is_planned_before_the_record_loads_or_while_disabled() {
    let mut w = World::new();
    w.signing = Signing::default();
    w.tick();
    assert_eq!(w.signing.plan(), &Plan::None);
    assert!(w.router.borrow().queries.is_empty());
    let mut w = World::new();
    w.cfg.enabled = false;
    w.tick();
    assert_eq!(w.signing.plan(), &Plan::None);
}

#[test]
fn nothing_due_plans_nothing_until_a_recheck() {
    let mut w = World::new();
    let signed_at = w.time;
    w.all_signed(signed_at);
    w.tick();
    assert_eq!(w.signing.plan(), &Plan::None);
    assert!(w.router.borrow().queries.is_empty());
    // Until the first recheck nothing is searched.
    w.time = signed_at + w.cfg.recheck_ticks - 1;
    w.tick();
    assert!(w.router.borrow().queries.is_empty());
    w.time = signed_at + w.cfg.recheck_ticks;
    w.tick();
    assert!(matches!(w.signing.plan(), Plan::Ready { .. }));
}

#[test]
fn the_signer_signs_next_to_the_controller_and_sees_it_signed() {
    let mut w = World::new();
    w.tick();
    // The planned signer appears next to the home controller.
    w.add_signer(pos(w.home, HOME_CONTROLLER.0 + 1, HOME_CONTROLLER.1 + 1));
    w.begin();
    w.work();
    assert!(
        w.intents()
            .iter()
            .any(|i| matches!(i, CreepIntent::SignController { .. })),
        "{:?}",
        w.intents()
    );
    w.time += 1;
    w.home_sign = SignSeen::Ours;
    w.begin();
    assert!(
        w.signing
            .events()
            .contains(&SignEvent::Signed { room: w.home })
    );
    assert_eq!(
        w.signing.book().get(w.home).map(|e| e.state),
        Some(SignState::Signed)
    );
    w.work();
    assert!(
        !w.intents()
            .iter()
            .any(|i| matches!(i, CreepIntent::SignController { .. })),
        "no sign over its own text"
    );
}

#[test]
fn a_room_with_no_route_is_given_up() {
    let mut w = World::new();
    w.all_signed(w.time);
    let target = room("E3N2");
    w.signing.book.entry(target, 0).expect("entry").state = SignState::Unsigned;
    w.router.borrow_mut().fail.push(target);
    w.tick();
    assert!(w.signing.events().contains(&SignEvent::GaveUp {
        room: target,
        state: SignState::Unreachable
    }));
    let c = w.cfg;
    assert_eq!(
        w.signing.book().due_at(target, &c),
        w.time - 1 + c.retry_ticks
    );
}

#[test]
fn routes_avoid_towered_rooms_but_the_target() {
    let mut w = World::new();
    w.all_signed(w.time);
    let towered = room("E3N2");
    let target = room("E3N1");
    {
        let e = w.signing.book.entry(towered, 0).expect("towered");
        e.towers.push(tile(25, 25));
    }
    w.signing.book.entry(target, 0).expect("target").state = SignState::Unsigned;
    w.tick();
    let r = w.router.borrow();
    let (q, closed) = r.queries.last().expect("a search");
    assert_eq!(q.to.room_name(), target);
    assert!(closed.contains(&towered));
    assert!(!closed.contains(&target));
    assert!(!closed.contains(&w.home));
}

#[test]
fn a_towered_target_gets_a_body_sized_for_its_towers() {
    let mut w = World::new();
    w.all_signed(w.time);
    let target = room("E3N2");
    // A controller near the entrance, a tower far off: two ticks under fire before the sign.
    let controller = tile(3, 25);
    let tower = tile(45, 25);
    {
        let e = w.signing.book.entry(target, 0).expect("target");
        e.state = SignState::Unsigned;
        e.controller = Some(controller);
        e.towers.push(tower);
    }
    w.tick();
    assert!(matches!(w.signing.plan(), Plan::Sizing { .. }));
    // The terrain is fed room by room: the home's, then the target's.
    assert_eq!(w.signing.terrain_wanted(), Some(w.home));
    let c = w.cfg;
    w.signing.feed_terrain(w.home, &terrain(0), &c);
    assert_eq!(w.signing.terrain_wanted(), Some(target));
    w.signing.feed_terrain(target, &terrain(0), &c);
    let Plan::Ready { body, trip, .. } = w.signing.plan().clone() else {
        panic!("sized: {:?}", w.signing.plan());
    };
    assert!(trip.dangerous);
    assert_eq!(trip.room, target);
    assert!(
        body.cost() > Part::Move.cost(),
        "more than the plain signer"
    );
    // The towers reach every tile of the target room it walks before the sign.
    let path = w.router.borrow().queries.last().expect("search").0;
    assert_eq!(path.to, pos(target, controller.x(), controller.y()));
    // Too little energy for any such body: deadly.
    let mut w2 = World::new();
    w2.capacity = Part::Move.cost();
    w2.all_signed(w2.time);
    {
        let e = w2.signing.book.entry(target, 0).expect("target");
        e.state = SignState::Unsigned;
        e.controller = Some(controller);
        e.towers.push(tower);
    }
    w2.tick();
    w2.signing.feed_terrain(w2.home, &terrain(0), &c);
    w2.signing.feed_terrain(target, &terrain(0), &c);
    assert_eq!(w2.signing.plan(), &Plan::None);
    assert_eq!(
        w2.signing.book().get(target).map(|e| e.state),
        Some(SignState::Deadly)
    );
}

#[test]
fn a_plain_signer_steps_back_out_of_a_towered_room() {
    let mut w = World::new();
    let hostile = room("E3N2");
    w.seen.push(Seen {
        name: hostile,
        controller: Controller::Foreign { level: 4 },
        controller_at: Some(pos(hostile, 20, 20)),
        sign: SignSeen::Other,
        towers: vec![pos(hostile, 30, 30)],
    });
    w.add_signer(pos(hostile, 0, 25));
    w.begin();
    w.work();
    assert!(w.signing.events().contains(&SignEvent::Retreated {
        room: hostile,
        towers: 1
    }));
    // No move: on the edge tile the engine carries it back (V151).
    assert!(!w.movement.planned(0));
    assert_eq!(
        w.signing.book().get(hostile).map(|e| e.towers.clone()),
        Some(vec![tile(30, 30)])
    );
}

#[test]
fn a_signer_lost_early_counts_a_death_there() {
    let mut w = World::new();
    let far = room("E3N2");
    w.seen.push(Seen {
        name: far,
        controller: Controller::Neutral,
        controller_at: Some(pos(far, 20, 20)),
        sign: SignSeen::None,
        towers: Vec::new(),
    });
    w.add_signer(pos(far, 10, 25));
    w.tick();
    w.signers.clear();
    w.begin();
    assert!(w.signing.events().contains(&SignEvent::Died {
        room: far,
        deaths: 1
    }));
    // A signer that dies of age counts nothing.
    let mut w = World::new();
    w.seen.push(Seen {
        name: far,
        controller: Controller::Neutral,
        controller_at: Some(pos(far, 20, 20)),
        sign: SignSeen::None,
        towers: Vec::new(),
    });
    w.add_signer(pos(far, 10, 25));
    w.signers[0].death = w.time + 1;
    w.tick();
    w.signers.clear();
    w.begin();
    assert!(
        !w.signing
            .events()
            .iter()
            .any(|e| matches!(e, SignEvent::Died { .. }))
    );
}

#[test]
fn a_replaced_sign_counts_an_overwrite() {
    let mut w = World::new();
    let r = room("E3N2");
    w.signing.book.entry(r, 0).expect("entry").state = SignState::Signed;
    w.seen.push(Seen {
        name: r,
        controller: Controller::Foreign { level: 2 },
        controller_at: Some(pos(r, 20, 20)),
        sign: SignSeen::Other,
        towers: Vec::new(),
    });
    w.add_signer(pos(r, 10, 25));
    w.begin();
    let e = w.signing.book().get(r).expect("entry");
    assert_eq!((e.state, e.overwrites), (SignState::Unsigned, 1));
    // The server's own sign: nothing a player writes shows.
    w.seen[0].sign = SignSeen::System;
    w.time += 1;
    w.begin();
    assert_eq!(
        w.signing.book().get(r).map(|e| e.state),
        Some(SignState::System)
    );
}

#[test]
fn only_one_signer_lives() {
    let mut w = World::new();
    w.add_signer(pos(w.home, 30, 30));
    w.add_signer(pos(w.home, 31, 30));
    w.begin();
    w.work();
    let suicides = w
        .intents()
        .iter()
        .filter(|i| matches!(i, CreepIntent::Suicide))
        .count();
    assert_eq!(suicides, 1);
    assert_eq!(
        w.signing.signer().map(|s| s.handle),
        w.snap.creeps.handles().first().copied()
    );
    // With one alive, nothing is requested.
    w.signing.request(&mut w.spawner, &w.cfg, true);
    assert!(w.requests().is_empty());
}

#[test]
fn the_record_is_saved_when_changed_at_most_once_per_window() {
    let mut w = World::new();
    w.tick();
    assert!(!w.signing.save_due(&w.cfg), "nothing changed yet");
    let r = room("E3N2");
    w.signing.book.entry(r, 0).expect("entry");
    w.signing.give_up(r, SignState::Unreachable);
    assert!(w.signing.save_due(&w.cfg));
    w.signing.saved();
    w.signing.give_up(r, SignState::Deadly);
    assert!(!w.signing.save_due(&w.cfg), "within save_ticks");
    w.time += w.cfg.save_ticks;
    w.begin();
    assert!(w.signing.save_due(&w.cfg));
    let mut payload = Vec::new();
    w.signing.payload(&mut payload);
    assert_eq!(Book::decode(&payload).as_ref(), Ok(w.signing.book()));
    // A refused record is rewritten.
    let mut fresh = Signing::default();
    assert!(fresh.loaded(RecordLoad::Payload(&[1])).is_err());
    assert!(fresh.is_loaded());
    assert!(fresh.save_due(&w.cfg));
    // A segment that could not be read is never written over.
    let mut blind = Signing::default();
    blind.loaded(RecordLoad::Unavailable).expect("unavailable");
    blind.give_up(room("E3N2"), SignState::Unreachable);
    assert!(!blind.save_due(&w.cfg));
    // take_save marks it saved.
    let mut once = Signing::default();
    once.loaded(RecordLoad::Refused).expect("refused");
    assert!(once.take_save(&w.cfg));
    assert!(!once.save_due(&w.cfg));
}

#[test]
fn a_signer_alive_before_the_record_loads_changes_nothing() {
    let mut w = World::new();
    w.signing = Signing::default();
    let far = room("E3N2");
    w.seen.push(Seen {
        name: far,
        controller: Controller::Neutral,
        controller_at: Some(pos(far, 20, 20)),
        sign: SignSeen::None,
        towers: Vec::new(),
    });
    w.add_signer(pos(far, 10, 25));
    w.tick();
    assert!(w.signing.book().is_empty(), "no read goes into the book");
    assert!(
        !w.signing.save_due(&w.cfg),
        "nothing to save over the record"
    );
    // Lost before the load: no death is counted.
    w.signers.clear();
    w.begin();
    assert!(w.signing.book().is_empty());
    assert!(w.signing.events().is_empty());
    // Once loaded, the reads go in.
    w.signing.loaded(RecordLoad::Empty).expect("empty");
    w.add_signer(pos(far, 10, 25));
    w.begin();
    assert_eq!(
        w.signing.book().get(far).map(|e| e.state),
        Some(SignState::Unsigned)
    );
}

#[test]
fn a_sign_that_does_not_show_counts_as_replaced() {
    let mut w = World::new();
    w.all_signed(w.time);
    let r = room("E3N2");
    w.signing.book.entry(r, 0).expect("entry").state = SignState::Unsigned;
    w.seen.push(Seen {
        name: r,
        controller: Controller::Foreign { level: 3 },
        controller_at: Some(pos(r, 20, 20)),
        sign: SignSeen::Other,
        towers: Vec::new(),
    });
    w.add_signer(pos(r, 21, 21));
    let signs = |w: &World| {
        w.intents()
            .iter()
            .filter(|i| matches!(i, CreepIntent::SignController { .. }))
            .count()
    };
    for round in 1..=w.cfg.max_overwrites {
        w.begin();
        w.work();
        assert_eq!(signs(&w), 1, "round {round}");
        w.time += 1;
        w.begin();
        assert_eq!(
            w.signing.book().get(r).map(|e| u32::from(e.overwrites)),
            Some(round)
        );
    }
    // Given up: no longer due, so no sign while it waits.
    let c = w.cfg;
    assert!(w.signing.book().due_at(r, &c) > w.time);
    w.work();
    assert_eq!(signs(&w), 0);
}

#[test]
fn a_dying_signer_ends_without_a_search() {
    let mut w = World::new();
    w.all_signed(w.time);
    let r = room("E3N2");
    {
        let e = w.signing.book.entry(r, 0).expect("entry");
        e.state = SignState::Unsigned;
        e.controller = Some(tile(40, 25));
    }
    w.add_signer(pos(w.home, 30, 25));
    // Ten ticks left: the shortest walk to (40, 25) next door is 59 tiles.
    w.signers[0].death = w.time + 10;
    let before = w.router.borrow().queries.len();
    w.begin();
    w.work();
    assert_eq!(w.router.borrow().queries.len(), before, "no search");
    assert!(
        w.signing
            .events()
            .contains(&SignEvent::Done { room: w.home })
    );
    assert!(w.intents().contains(&CreepIntent::Suicide));
    // Gone after its suicide: no death counted.
    w.time += 1;
    w.signers.clear();
    w.begin();
    assert!(
        !w.signing
            .events()
            .iter()
            .any(|e| matches!(e, SignEvent::Died { .. }))
    );
}

#[test]
fn a_route_the_live_signer_cannot_find_passes_the_room_over_only() {
    let mut w = World::new();
    w.all_signed(w.time);
    let r = room("E3N2");
    w.signing.book.entry(r, 0).expect("entry").state = SignState::Unsigned;
    w.router.borrow_mut().fail.push(r);
    w.home_sign = SignSeen::Ours;
    w.add_signer(pos(w.home, 30, 25));
    w.tick();
    // Not unreachable for everyone: only the home spawn's search says that.
    assert_eq!(
        w.signing.book().get(r).map(|e| e.state),
        Some(SignState::Unsigned)
    );
    assert!(
        !w.signing
            .events()
            .iter()
            .any(|e| matches!(e, SignEvent::GaveUp { .. }))
    );
    // With nothing else due it ends.
    w.begin();
    w.work();
    assert!(
        w.signing
            .events()
            .contains(&SignEvent::Done { room: w.home })
    );
}
