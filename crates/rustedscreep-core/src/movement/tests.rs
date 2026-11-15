//! Native scenario tests (TEST_PLAN §7): a small world drives the snapshot, the movement module, the
//! intent buffer and a model of the engine's move resolution over many ticks.
//!
//! The model follows V51 and V52: a move fails when its tile is blocked (terrain or an object the
//! world has, known to the bot or not) or when a creep that is not moving stands there, and a
//! failure cascades to every move into the failed creep's tile. Two moves into one tile fail the
//! test (the traffic manager must never plan them), and so does a creep ending a tick on an edge
//! tile (V151); a strict world also fails it on any dropped move.

use super::*;
use crate::config::{CACHE_MMO, MOVEMENT_MMO};
use crate::intents::{ActLog, IntentBuffer, Outcome};
use crate::snapshot::{Controller, CreepRow, ObjectKey, TickContext};
use grid::tests::{raw, tile};
use rustedscreep_consts::{ErrorCode, INTENT_CPU_COST, RoomCoordinate, Terrain};

#[derive(Clone, Copy, Debug)]
struct SimCreep {
    key: u128,
    at: Tile,
    my: bool,
    /// Fatigue every tick (a creep that never recovers stands for a fixed obstacle).
    fatigue: u32,
    /// (target, range, priority).
    goal: Option<(Tile, u8, u8)>,
    /// Without a goal: whether its job lets movers shove it anywhere (`give_way`).
    give_way: bool,
}

struct Sim {
    room: RoomName,
    raw: Vec<u8>,
    /// The world's objects: what the bot sees when it scans.
    objects: Vec<(Tile, Footprint)>,
    creeps: Vec<SimCreep>,
    time: u32,
    snap: Snapshot,
    buffer: IntentBuffer,
    movement: Movement,
    cfg: MovementConfig,
    cache: CacheConfig,
    /// Committed CPU of the last tick (requests and shoves).
    committed: f64,
    /// Progress per creep (by index) on the last tick.
    progress: Vec<Option<Result<Progress, MoveError>>>,
    max_shoves: u32,
    /// Whether the bot owns the room (an owned room is read without waiting for a request).
    owned: bool,
    /// Creeps (by index) whose next move the "game" refuses with an error code, reported back
    /// like the act step does.
    refuse: Vec<usize>,
    /// Rooms each tick's first-read and rescan jobs read.
    reads: Vec<(usize, usize)>,
    /// Tiles the engine blocks that no scan or snapshot shows (a power creep, V52).
    hidden: Vec<Tile>,
    /// Sources and minerals: the first read hands them over as statics, and the engine blocks them.
    statics: Vec<Tile>,
    /// Fail the test on any move the engine drops (for worlds the bot sees completely).
    strict: bool,
}

impl Sim {
    fn new(raw: Vec<u8>, objects: Vec<(Tile, Footprint)>) -> Self {
        Self {
            room: "W1N1".parse().expect("room"),
            raw,
            objects,
            creeps: Vec::new(),
            time: 1000,
            snap: Snapshot::default(),
            buffer: IntentBuffer::default(),
            movement: Movement::default(),
            cfg: MOVEMENT_MMO,
            cache: CACHE_MMO,
            committed: 0.0,
            progress: Vec::new(),
            max_shoves: 100,
            owned: true,
            refuse: Vec::new(),
            reads: Vec::new(),
            hidden: Vec::new(),
            statics: Vec::new(),
            strict: false,
        }
    }

    fn open() -> Self {
        Self::new(raw(|_, _| 0), Vec::new())
    }

    fn corridor() -> Self {
        let wall = Terrain::Wall as u8;
        // Everything is wall except y = 20 from x = 5 to 44.
        Self::new(
            raw(|x, y| {
                if y == 20 && (5..45).contains(&x) {
                    0
                } else {
                    wall
                }
            }),
            Vec::new(),
        )
    }

    fn add(&mut self, at: Tile, goal: Option<(Tile, u8, u8)>) -> usize {
        let key = 1 + self.creeps.len() as u128;
        self.creeps.push(SimCreep {
            key,
            at,
            my: true,
            fatigue: 0,
            goal,
            give_way: false,
        });
        self.creeps.len() - 1
    }

    fn add_idle(&mut self, at: Tile) -> usize {
        let i = self.add(at, None);
        self.creeps[i].give_way = true;
        i
    }

    fn pos(&self, t: Tile) -> Position {
        Position::new(
            RoomCoordinate::new(t.x()).expect("x"),
            RoomCoordinate::new(t.y()).expect("y"),
            self.room,
        )
    }

    fn blocked_in_world(&self, t: Tile) -> bool {
        let wall =
            self.raw[usize::from(t.y()) * 50 + usize::from(t.x())] & (Terrain::Wall as u8) != 0;
        let road = self
            .objects
            .iter()
            .any(|&(o, f)| o == t && f == Footprint::Road);
        let object = self
            .objects
            .iter()
            .any(|&(o, f)| o == t && f == Footprint::Blocked);
        (wall && !road) || object || self.hidden.contains(&t) || self.statics.contains(&t)
    }

    /// One tick: snapshot, movement, the read jobs, requests, resolve, issue, and the engine model.
    fn step(&mut self) {
        self.snap
            .begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
        let mut handles = Vec::new();
        for c in &self.creeps {
            let row = CreepRow {
                pos: self.pos(c.at),
                my: c.my,
                spawning: false,
                fatigue: c.fatigue,
            };
            handles.push(self.snap.creeps.push(ObjectKey(c.key), row).expect("creep"));
        }
        let controller = if self.owned {
            Controller::Mine { level: 1 }
        } else {
            Controller::Neutral
        };
        self.snap.rooms.push(self.room, controller);
        self.snap.finish();
        self.movement.begin(&self.snap, &self.cfg);
        self.movement.set_cache(&self.cfg, &self.cache);
        // The glue's two read jobs, which the decide step runs before any request.
        let mut rooms = Vec::new();
        self.movement.first_reads(&self.cfg, &mut rooms);
        let first = rooms.len();
        for &room in &rooms {
            assert!(self.movement.set_terrain(room, &self.raw));
            let statics = self.statics.clone();
            self.movement.set_statics(room, statics);
            let objects = self.objects.clone();
            self.movement.set_objects(room, objects);
        }
        self.movement.rescans(&self.cfg, &mut rooms);
        let rescans = rooms.len();
        for &room in &rooms {
            let objects = self.objects.clone();
            self.movement.set_objects(room, objects);
        }
        self.reads.push((first, rescans));
        self.buffer.begin(&self.snap);
        let mut commit = Commit::default();
        self.progress = vec![None; self.creeps.len()];
        for (i, c) in self.creeps.iter().enumerate() {
            if !c.my {
                continue;
            }
            if let Some((target, range, priority)) = c.goal {
                let to = MoveTo {
                    target: self.pos(target),
                    range,
                    priority,
                    tier: Tier::Critical,
                };
                let r = self
                    .movement
                    .request(&self.snap, handles[i], to, &self.cfg, &mut commit);
                self.progress[i] = Some(r);
            } else if c.give_way {
                self.movement.give_way(&self.snap, handles[i], None);
            }
        }
        let caps = ShoveCaps::from_cpu_left([f64::from(self.max_shoves) * INTENT_CPU_COST; 3]);
        self.movement
            .resolve(&self.snap, &mut self.buffer, &self.cfg, caps, &mut commit);
        self.committed = commit.cpu();
        self.buffer.resolve();
        let mut dest: Vec<Option<Tile>> = vec![None; self.creeps.len()];
        let mut log = ActLog::default();
        let refuse = std::mem::take(&mut self.refuse);
        let mut refused = Vec::new();
        self.buffer.issue(&mut log, |issue| {
            if let CreepIntent::Move { direction } = issue.intent {
                let i = issue.actor as usize;
                if refuse.contains(&i) {
                    refused.push((issue.actor, true));
                    return Some(Outcome::Failed(ErrorCode::NoBodypart));
                }
                dest[i] = self.creeps[i].at.step(direction);
                assert!(dest[i].is_some(), "a move out of the room");
            } else {
                panic!("only moves are queued here: {:?}", issue.intent);
            }
            Some(Outcome::Ok)
        });
        self.movement.moves_refused(&self.snap, refused, &self.cfg);
        // The engine model (V51, V52).
        loop {
            let mut changed = false;
            for i in 0..self.creeps.len() {
                let Some(d) = dest[i] else { continue };
                let others_aim = (0..self.creeps.len()).any(|j| j != i && dest[j] == Some(d));
                assert!(!others_aim, "two moves into {d:?}");
                let stays_there = (0..self.creeps.len())
                    .any(|j| j != i && self.creeps[j].at == d && dest[j].is_none());
                if self.blocked_in_world(d) || stays_there {
                    assert!(!self.strict, "the engine drops a move into {d:?}");
                    dest[i] = None;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for (c, d) in self.creeps.iter_mut().zip(&dest) {
            if let Some(d) = *d {
                c.at = d;
            }
        }
        for c in &self.creeps {
            assert!(
                !c.at.is_edge(),
                "a creep ended a tick on an edge tile: {:?}",
                c.at
            );
        }
        self.time += 1;
    }

    /// Steps until `done` or `limit` ticks; returns the ticks taken.
    fn run(&mut self, limit: u32, done: impl Fn(&Self) -> bool) -> u32 {
        for n in 0..limit {
            if done(self) {
                return n;
            }
            self.step();
        }
        assert!(done(self), "not done after {limit} ticks");
        limit
    }

    fn arrived(&self, i: usize) -> bool {
        matches!(self.progress.get(i), Some(Some(Ok(Progress::Arrived))))
    }
}

/// Uses: MOVE_COST_PLAIN (through the default profile), INTENT_CPU_COST. On open plain one creep
/// reaches range 1 of its target in Chebyshev distance - 1 moves, commits one intent per moving
/// tick and none once it holds, and its field is built once.
#[test]
fn a_creep_walks_to_its_goal_and_holds() {
    let mut sim = Sim::open();
    let start = tile(10, 10);
    let target = tile(30, 25);
    let c = sim.add(start, Some((target, 1, 1)));
    sim.step();
    assert!(
        (sim.committed - INTENT_CPU_COST).abs() < 1e-9,
        "one move committed"
    );
    let moves = u32::from(start.range(target)) - 1;
    let ticks = 1 + sim.run(100, |s| s.arrived(c));
    // The tick that reports Arrived plans no move; every tick before it moved one tile.
    assert_eq!(ticks, moves + 1);
    assert_eq!(sim.creeps[c].at.range(target), 1);
    assert_eq!(sim.committed, 0.0, "a holding creep commits nothing");
    sim.step();
    assert!(sim.arrived(c));
    let t = sim.movement.totals();
    assert_eq!(t.fields_built, 1);
    assert_eq!((t.detours, t.stale, t.refused), (0, 0, 0));
    assert_eq!(t.pushed, moves);
    assert_eq!(
        sim.movement.fields().hits(),
        moves - 1,
        "built once, then hits until it arrived"
    );
    assert_eq!(t.scans, 1, "one scan within the interval");
    assert_eq!(t.grids_built, 1);
}

/// A 1-wide corridor with two creeps going opposite ways: they swap (both move, V52) and both
/// arrive; a creep that gives way in the corridor is moved out of the way once (a swap puts it
/// behind the mover, which never pushes it again).
#[test]
fn creeps_cross_in_a_corridor() {
    let mut sim = Sim::corridor();
    let a = sim.add(tile(10, 20), Some((tile(40, 20), 0, 1)));
    let b = sim.add(tile(40, 20), Some((tile(10, 20), 0, 1)));
    let ticks = sim.run(80, |s| s.arrived(a) && s.arrived(b));
    assert!(ticks <= 32, "{ticks} ticks");
    assert_eq!(sim.creeps[a].at, tile(40, 20));
    assert_eq!(sim.creeps[b].at, tile(10, 20));

    let mut sim = Sim::corridor();
    let a = sim.add(tile(10, 20), Some((tile(40, 20), 0, 1)));
    let idle = sim.add_idle(tile(25, 20));
    sim.run(80, |s| s.arrived(a));
    assert_eq!(sim.creeps[a].at, tile(40, 20));
    assert_eq!(sim.movement.totals().shoved, 1, "one swap");
    assert_eq!(sim.creeps[idle].at, tile(24, 20));
}

/// An own creep with no request and no `give_way` (a worker whose job only queued its work) is
/// never moved: a mover in the corridor behind it waits, and after `stuck_ticks` its detour
/// search finds no way past and backs off.
#[test]
fn a_creep_without_a_request_is_never_shoved() {
    let mut sim = Sim::corridor();
    let a = sim.add(tile(10, 20), Some((tile(40, 20), 0, 1)));
    let worker = sim.add(tile(25, 20), None);
    for _ in 0..60 {
        sim.step();
    }
    assert_eq!(sim.creeps[worker].at, tile(25, 20), "never moved");
    assert_eq!(sim.creeps[a].at, tile(24, 20), "the mover waits behind it");
    let t = *sim.movement.totals();
    assert_eq!(t.shoved, 0);
    // Detour searches back off: after each failure the wait doubles (2, 4, 8, 16, 32 ticks), so
    // in about 45 stuck ticks there are at most 5 searches, not one every 2 ticks.
    assert!(t.detour_failures >= 2 && t.detour_failures <= 5, "{t:?}");
}

/// A creep that arrived holds within its range when another creep's route runs through its tile:
/// the shove keeps it in range (its anchor).
#[test]
fn an_arrived_creep_stays_in_range_when_shoved() {
    let wall = Terrain::Wall as u8;
    // A 3-wide corridor at y = 19..=21.
    let map = raw(|x, y| {
        if (19..22).contains(&y) && (5..45).contains(&x) {
            0
        } else {
            wall
        }
    });
    let mut sim = Sim::new(map, Vec::new());
    let post = tile(25, 20);
    let holder = sim.add(post, Some((post, 1, 5)));
    let walker = sim.add(tile(10, 20), Some((tile(40, 20), 0, 1)));
    for _ in 0..60 {
        sim.step();
        assert!(
            sim.creeps[holder].at.range(post) <= 1,
            "the holder left its range"
        );
    }
    assert_eq!(sim.creeps[walker].at, tile(40, 20));
}

/// A fixed creep (always fatigued) in the near gap of a wall: the field still points there, so the
/// mover is kept in place, then searches a detour after `stuck_ticks` and goes through the far gap.
#[test]
fn a_blocked_creep_detours_after_stuck_ticks() {
    let wall = Terrain::Wall as u8;
    // A wall at x = 25 with gaps at y = 20 (near) and y = 30 (far).
    let map = raw(|x, y| {
        if x == 25 && y != 20 && y != 30 {
            wall
        } else {
            0
        }
    });
    let mut sim = Sim::new(map, Vec::new());
    let mover = sim.add(tile(20, 20), Some((tile(30, 20), 0, 1)));
    let tired = sim.add(tile(25, 20), None);
    sim.creeps[tired].fatigue = 10;
    let ticks = sim.run(60, |s| s.arrived(mover));
    let t = *sim.movement.totals();
    assert!(t.detours >= 1, "{t:?}");
    assert_eq!(t.stale, 0, "the blocker was a creep the snapshot sees");
    assert!(ticks < 40, "{ticks} ticks");
    assert_eq!(
        sim.creeps[tired].at,
        tile(25, 20),
        "a fatigued creep is never moved"
    );
}

/// When every tile of the goal is taken by creeps that stay, the stuck mover skips the detour
/// search (nothing could succeed) and backs off; it never searches.
#[test]
fn a_full_goal_skips_the_detour_search() {
    let mut sim = Sim::open();
    let goal = tile(25, 25);
    // Pinned creeps (no request) on the goal and all 8 tiles around it.
    for (_, t) in goal.neighbors() {
        sim.add(t, None);
    }
    sim.add(goal, None);
    let mover = sim.add(tile(10, 25), Some((goal, 1, 1)));
    for _ in 0..40 {
        sim.step();
    }
    let t = *sim.movement.totals();
    assert_eq!((t.detours, t.detour_failures), (0, 0), "{t:?}");
    assert!(t.detours_skipped >= 1 && t.detours_skipped <= 4, "{t:?}");
    assert_eq!(
        sim.creeps[mover].at.range(goal),
        2,
        "it waits next to the crowd"
    );
}

/// An object the bot has not scanned (a new construction site) blocks the field's route: engine
/// failures into a tile with no creep mark the room stale, the rescan comes `rescan_min_ticks`
/// after the last scan instead of `scan_interval_ticks`, and the creep goes around.
#[test]
fn an_unknown_obstacle_triggers_an_early_rescan() {
    let mut sim = Sim::open();
    let start = tile(10, 20);
    let mover = sim.add(start, Some((tile(30, 20), 0, 1)));
    sim.step();
    // A wall of blocking sites across x = 15 appears after the first scan, with one gap at y = 5.
    for y in 1..49 {
        if y != 5 {
            sim.objects.push((tile(15, y), Footprint::Blocked));
        }
    }
    let ticks = sim.run(200, |s| s.arrived(mover));
    let t = *sim.movement.totals();
    assert!(t.stale >= 1, "{t:?}");
    assert!(t.scans >= 2 && t.changed >= 2, "{t:?}");
    // The second scan came as soon as `rescan_min_ticks` allowed, not at the interval: the tick of
    // the first rescan, counted from the first scan (tick 0).
    let rescan = sim
        .reads
        .iter()
        .position(|&(_, rescans)| rescans > 0)
        .expect("a rescan");
    let min = sim.cfg.rescan_min_ticks as usize;
    assert!(
        rescan >= min && rescan <= min + 1,
        "rescan at tick {rescan}, min {min}"
    );
    assert!(sim.cfg.rescan_min_ticks < sim.cfg.scan_interval_ticks);
    assert!(
        ticks < sim.cfg.scan_interval_ticks + 40,
        "the early rescan found the wall: {ticks} ticks"
    );
}

/// A move the game refuses (an error code: no active MOVE part) does not count as a failed move:
/// no stuck count, no detour, no early rescan. The creep is held as fixed for
/// `immobile_retry_ticks` (its requests fail with CannotMove, nothing is committed), then tried
/// again.
#[test]
fn a_move_the_game_refuses_holds_the_creep() {
    let mut sim = Sim::open();
    let c = sim.add(tile(10, 10), Some((tile(30, 10), 0, 1)));
    sim.refuse = vec![c];
    sim.step();
    for _ in 0..20 {
        sim.step();
        assert_eq!(sim.progress[c], Some(Err(MoveError::CannotMove)));
        assert_eq!(sim.committed, 0.0);
    }
    let t = *sim.movement.totals();
    assert_eq!(sim.creeps[c].at, tile(10, 10));
    assert_eq!((t.detours, t.detours_skipped, t.stale), (0, 0, 0), "{t:?}");
    assert_eq!(t.refused_by_game, 1, "asked once, then held");
    // After the retry period it is asked again, and this time the game lets it move.
    let retry = sim.cfg.immobile_retry_ticks;
    sim.run(retry + 2, |s| s.progress[c] == Some(Ok(Progress::Moving)));
    sim.step();
    assert_ne!(sim.creeps[c].at, tile(10, 10));
}

/// A creep the game will not move is fixed for the traffic manager: a mover does not push it (it
/// would be billed for a move the engine drops), it waits and searches a detour instead.
#[test]
fn an_immobile_creep_is_not_shoved() {
    let mut sim = Sim::open();
    let mover = sim.add(tile(10, 10), Some((tile(12, 10), 0, 1)));
    let broken = sim.add_idle(tile(11, 10));
    // The broken creep's first move (a shove) is refused; afterwards it is held.
    sim.refuse = vec![broken];
    sim.step();
    let shoved_before = sim.movement.totals().shoved;
    sim.run(10, |s| s.arrived(mover));
    assert_eq!(sim.creeps[broken].at, tile(11, 10));
    assert_eq!(
        sim.movement.totals().shoved,
        shoved_before,
        "never shoved again"
    );
    assert_eq!(
        sim.creeps[mover].at.range(tile(12, 10)),
        0,
        "it went around"
    );
}

/// Something only the engine sees (a power creep) on the field's route: failed moves into an
/// empty tile make it a suspect, which traffic and detours avoid, and the room is rescanned early
/// only once. The creep gets through quickly instead of failing every tick.
#[test]
fn a_hidden_blocker_is_learned() {
    let setup = || {
        let mut sim = Sim::open();
        sim.add(tile(10, 10), Some((tile(30, 10), 0, 1)));
        sim
    };
    // A twin run records the route, so the blocker goes on a tile the creep would really enter.
    let mut twin = setup();
    let mut route = Vec::new();
    for _ in 0..8 {
        twin.step();
        route.push(twin.creeps[0].at);
    }
    let mut sim = setup();
    sim.hidden.push(route[5]);
    let ticks = sim.run(60, |s| s.arrived(0));
    let t = *sim.movement.totals();
    assert!(ticks < 35, "{ticks} ticks: {t:?}");
    assert_eq!((t.suspects, t.stale), (1, 1), "{t:?}");
    assert!(t.scans <= 2, "{t:?}");
    assert!(t.pushed < ticks + 3, "few failed moves: {t:?}");
}

/// A detour taken around a creep that then leaves is dropped as soon as the field's step is free
/// again and no dearer, so the mover does not keep to the far gap.
#[test]
fn a_detour_is_dropped_when_the_jam_clears() {
    let wall = Terrain::Wall as u8;
    // A wall at x = 25 with gaps at y = 20 (near) and y = 40 (far).
    let map = raw(|x, y| {
        if x == 25 && y != 20 && y != 40 {
            wall
        } else {
            0
        }
    });
    let mut sim = Sim::new(map, Vec::new());
    let mover = sim.add(tile(22, 20), Some((tile(30, 20), 0, 1)));
    let blocker = sim.add(tile(25, 20), None);
    sim.creeps[blocker].fatigue = 10;
    for _ in 0..6 {
        sim.step();
    }
    assert!(sim.movement.totals().detours >= 1, "it detoured");
    // The blocker leaves the gap.
    sim.creeps[blocker].at = tile(45, 45);
    let ticks = sim.run(40, |s| s.arrived(mover));
    assert!(ticks <= 12, "back through the near gap: {ticks} ticks");
}

/// With `stuck_ticks` at its highest, the detour backoff still reaches its cap (the stuck count does
/// not saturate below the longest wait, 16 times `stuck_ticks`).
#[test]
fn backoff_works_at_the_highest_stuck_ticks() {
    let mut sim = Sim::corridor();
    sim.cfg.stuck_ticks = 20;
    sim.add(tile(10, 20), Some((tile(40, 20), 0, 1)));
    sim.add(tile(11, 20), None); // pinned, in the way for good
    let waits: u32 = (0..5).map(|f| 20u32 << f).sum();
    for _ in 0..waits + 10 {
        sim.step();
    }
    let t = *sim.movement.totals();
    assert_eq!(t.detour_failures, 5, "{t:?}");
}

/// A room the bot does not own is read only once a creep in it is asked to move: the first request
/// finds it not ready, the next tick's first-read job reads it, and the creep moves from then on;
/// rescans then follow the interval while requests keep coming.
#[test]
fn an_unowned_room_is_read_on_demand() {
    let mut sim = Sim::open();
    sim.owned = false;
    let idle = sim.add(tile(10, 10), None);
    for _ in 0..3 {
        sim.step();
    }
    assert_eq!(sim.movement.totals().scans, 0, "nobody asked: no read");
    sim.creeps[idle].goal = Some((tile(20, 10), 0, 1));
    sim.step();
    assert_eq!(sim.progress[idle], Some(Err(MoveError::RoomNotReady)));
    sim.step();
    assert_eq!(sim.progress[idle], Some(Ok(Progress::Moving)));
    assert_eq!(
        sim.reads.last(),
        Some(&(1, 0)),
        "read by the first-read job"
    );
    assert_eq!(sim.movement.totals().scans, 1);
}

/// With both caches off (the naive baseline) the creeps take exactly the same tiles, while the grid
/// is rebuilt and the field built on every tick.
#[test]
fn the_naive_baseline_moves_the_same() {
    let swamp = Terrain::Swamp as u8;
    let map = raw(|x, y| if (x + 2 * y) % 7 == 0 { swamp } else { 0 });
    let roads: Vec<(Tile, Footprint)> = (12..40).map(|x| (tile(x, 30), Footprint::Road)).collect();
    let mut runs = Vec::new();
    for naive in [false, true] {
        let mut sim = Sim::new(map.clone(), roads.clone());
        if naive {
            sim.cache.movement_grid = false;
            sim.cache.flow_fields = false;
        }
        sim.add(tile(10, 10), Some((tile(40, 35), 1, 1)));
        sim.add(tile(40, 12), Some((tile(12, 35), 2, 2)));
        sim.add(tile(25, 40), Some((tile(25, 5), 0, 1)));
        let mut trace = Vec::new();
        for _ in 0..60 {
            sim.step();
            trace.push(sim.creeps.iter().map(|c| c.at).collect::<Vec<_>>());
        }
        let t = *sim.movement.totals();
        if naive {
            assert_eq!(t.scans, 60);
            assert_eq!(t.grids_built, 60);
            assert_eq!(sim.movement.fields().len(), 0);
        } else {
            assert_eq!(
                (t.scans, t.grids_built),
                (2, 1),
                "a rescan at the interval finds no change"
            );
            assert!(t.fields_built <= 3);
        }
        runs.push(trace);
    }
    assert_eq!(runs[0], runs[1]);
}

/// Requests that cannot plan: another room, a room not read yet, a hostile creep, and a goal
/// walled in. A creep on an edge tile (just arrived from another room) steps into the room.
#[test]
fn requests_that_cannot_plan_and_the_edge() {
    let mut sim = Sim::open();
    sim.creeps.push(SimCreep {
        key: 99,
        at: tile(30, 30),
        my: false,
        fatigue: 0,
        goal: None,
        give_way: false,
    });
    let c = sim.add(tile(10, 10), None);
    sim.step();
    let h = sim
        .snap
        .creeps
        .handle_of(ObjectKey(sim.creeps[c].key))
        .expect("handle");
    let hostile = sim.snap.creeps.handle_of(ObjectKey(99)).expect("hostile");
    let other: RoomName = "W2N1".parse().expect("room");
    let away = Position::new(
        RoomCoordinate::new(5).expect("x"),
        RoomCoordinate::new(5).expect("y"),
        other,
    );
    let mut commit = Commit::default();
    let cfg = sim.cfg;
    let mut ask = |m: &mut Movement, snap: &Snapshot, who, target| {
        let to = MoveTo {
            target,
            range: 1,
            priority: 1,
            tier: Tier::Critical,
        };
        m.request(snap, who, to, &cfg, &mut commit)
    };
    assert_eq!(
        ask(&mut sim.movement, &sim.snap, h, away),
        Err(MoveError::OtherRoom)
    );
    let here = sim.pos(tile(20, 20));
    assert_eq!(
        ask(&mut sim.movement, &sim.snap, hostile, here),
        Err(MoveError::NotMine)
    );
    // A room the bot has never read (a fresh Movement): not ready.
    let mut fresh = Movement::default();
    fresh.begin(&sim.snap, &cfg);
    assert_eq!(
        ask(&mut fresh, &sim.snap, h, here),
        Err(MoveError::RoomNotReady)
    );
    assert_eq!(commit.cpu(), 0.0, "nothing planned, nothing committed");
    // A goal with range 0 on a blocked tile: no path.
    sim.objects.push((tile(20, 20), Footprint::Blocked));
    sim.cache.movement_grid = false; // read again now
    sim.creeps[c].goal = Some((tile(20, 20), 0, 1));
    sim.step();
    assert_eq!(sim.progress[c], Some(Err(MoveError::NoPath)));
    // Edge: a creep on (0, 25) heading in.
    let mut sim = Sim::open();
    let e = sim.add(tile(0, 25), Some((tile(10, 25), 0, 1)));
    sim.step();
    assert_eq!(sim.creeps[e].at.x(), 1);
    sim.run(20, |s| s.arrived(e));
    // MoveError names are distinct.
    for (i, a) in MoveError::ALL.into_iter().enumerate() {
        for b in &MoveError::ALL[i + 1..] {
            assert_ne!(a.as_str(), b.as_str());
        }
    }
}

/// Uses: INTENT_CPU_COST. A request commits one intent per creep per tick however often it is made;
/// a shove commits one more at resolve; with no shove budget the creep that gives way stays and
/// nothing extra is committed; a lower tier's cap limits its movers' shoves.
#[test]
fn commits_per_request_and_per_shove() {
    let mut sim = Sim::corridor();
    let a = sim.add(tile(10, 20), Some((tile(40, 20), 0, 1)));
    sim.add_idle(tile(11, 20));
    sim.step();
    let two = 2.0 * INTENT_CPU_COST;
    assert!(
        (sim.committed - two).abs() < 1e-9,
        "a mover and a shove: {}",
        sim.committed
    );
    assert_eq!(sim.creeps[a].at, tile(11, 20));
    // A second request in the same tick commits nothing more.
    let mut sim = Sim::open();
    let c = sim.add(tile(10, 10), Some((tile(20, 10), 0, 1)));
    sim.step();
    let h = sim
        .snap
        .creeps
        .handle_of(ObjectKey(sim.creeps[c].key))
        .expect("handle");
    let mut commit = Commit::default();
    let to = MoveTo {
        target: sim.pos(tile(20, 10)),
        range: 0,
        priority: 1,
        tier: Tier::Critical,
    };
    for _ in 0..3 {
        let r = sim
            .movement
            .request(&sim.snap, h, to, &sim.cfg, &mut commit);
        assert_eq!(r, Ok(Progress::Moving));
    }
    // The step already committed for this creep's plan in this tick, so these three add nothing.
    assert_eq!(commit.cpu(), 0.0);
    // No shove budget: the mover waits behind the creep that gives way.
    let mut sim = Sim::corridor();
    sim.max_shoves = 0;
    let a = sim.add(tile(10, 20), Some((tile(40, 20), 0, 1)));
    let idle = sim.add_idle(tile(11, 20));
    sim.step();
    assert_eq!(
        (sim.creeps[a].at, sim.creeps[idle].at),
        (tile(10, 20), tile(11, 20))
    );
    assert!((sim.committed - INTENT_CPU_COST).abs() < 1e-9);
    // Shove caps per tier: CPU for one shove at Critical, none at Standard.
    let caps = ShoveCaps::from_cpu_left([INTENT_CPU_COST, 0.0, f64::NAN]);
    assert_eq!(
        (
            caps.for_tier(Tier::Critical),
            caps.for_tier(Tier::Standard),
            caps.for_tier(Tier::Optional)
        ),
        (1, 0, 0)
    );
}

/// A chokepoint that a creep holds on and off (12 ticks there, 6 away, again and again): the mover
/// takes its detour through the far gap and keeps it once it is under way, instead of shuttling
/// back each time the gap opens.
#[test]
fn an_intermittent_chokepoint_does_not_make_the_mover_shuttle() {
    let wall = Terrain::Wall as u8;
    let map = raw(|x, y| {
        if x == 25 && y != 20 && y != 40 {
            wall
        } else {
            0
        }
    });
    let mut sim = Sim::new(map, Vec::new());
    let mover = sim.add(tile(22, 20), Some((tile(30, 20), 0, 1)));
    let blocker = sim.add(tile(25, 20), None);
    let mut arrived = None;
    for t in 0..150u32 {
        sim.creeps[blocker].at = if t % 18 < 12 {
            tile(25, 20)
        } else {
            tile(45, 45)
        };
        sim.step();
        if sim.arrived(mover) {
            arrived = Some(t);
            break;
        }
    }
    let t = *sim.movement.totals();
    let at = arrived.unwrap_or_else(|| panic!("never arrived: {t:?}"));
    assert!(at < 60, "arrived at tick {at}: {t:?}");
    assert!(t.detours <= 3, "{t:?}");
}

/// A hidden blocker that leaves right after it was learned keeps its tile blocked only for
/// `suspect_ticks`, so a creep whose only way is that tile waits about that long, not
/// `scan_interval_ticks`.
#[test]
fn a_suspect_expires_after_suspect_ticks() {
    let mut sim = Sim::corridor();
    let mover = sim.add(tile(10, 20), Some((tile(40, 20), 0, 1)));
    sim.hidden.push(tile(15, 20));
    let mut learned = None;
    for t in 0..40u32 {
        sim.step();
        if sim.movement.totals().suspects > 0 {
            learned = Some(t);
            break;
        }
    }
    let learned = learned.expect("the blocker is learned");
    sim.hidden.clear();
    let ticks = sim.run(120, |s| s.arrived(mover));
    let free_walk = 40 - 10;
    let wait = sim.cfg.suspect_ticks + 2 * u32::from(sim.cfg.stuck_ticks) + 4;
    assert!(
        learned + ticks <= free_walk + wait + 4,
        "learned at {learned}, then {ticks} ticks"
    );
    assert!(sim.cfg.suspect_ticks < sim.cfg.scan_interval_ticks);
}

/// A creep held as fixed (its move refused) that stands on its goal reports Arrived, not
/// CannotMove, and is pinned there.
#[test]
fn an_immobile_creep_on_its_goal_has_arrived() {
    let mut sim = Sim::open();
    let c = sim.add(tile(10, 10), Some((tile(12, 10), 0, 1)));
    sim.refuse = vec![c];
    sim.step();
    sim.creeps[c].goal = Some((tile(10, 10), 1, 1));
    sim.step();
    assert_eq!(sim.progress[c], Some(Ok(Progress::Arrived)));
    sim.creeps[c].goal = Some((tile(20, 10), 0, 1));
    sim.step();
    assert_eq!(sim.progress[c], Some(Err(MoveError::CannotMove)));
}

/// Sources and minerals (handed over with the terrain) block the grid: with a row of them across
/// the room but one gap, every move the bot makes is one the engine accepts (the strict model
/// fails the test on any dropped move), and the creep goes through the gap.
#[test]
fn sources_and_minerals_block_the_route() {
    let mut sim = Sim::open();
    sim.strict = true;
    sim.statics = (1..49).filter(|&y| y != 40).map(|y| tile(20, y)).collect();
    let c = sim.add(tile(10, 20), Some((tile(30, 20), 0, 1)));
    let ticks = sim.run(80, |s| s.arrived(c));
    assert!(ticks > 20, "it went round through the gap: {ticks} ticks");
    let t = *sim.movement.totals();
    assert_eq!((t.suspects, t.stale, t.detours), (0, 0, 0), "{t:?}");
}
