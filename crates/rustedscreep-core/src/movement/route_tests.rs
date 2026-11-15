//! Native scenario tests for inter-room routes (TEST_PLAN §7): a world of several rooms drives the
//! snapshot, movement with a model of PathFinder installed, the intent buffer, and a model of the
//! engine's moves and room transfers over many ticks.
//!
//! The PathFinder model follows V163 to V166, with the matrices the glue hands PathFinder (a room's
//! grid, or for a room without one [`route::unknown_room_matrix`]): a matrix byte other than 0 is
//! the step cost (255 blocks), 0 falls back to the terrain (a wall blocks, plain and swamp cost the query's costs), a
//! closed room blocks, a move from an edge tile goes straight across or back into its room (never
//! along the edge), range is the world Chebyshev distance, and the path leaves out the origin. It is
//! an A* with the range as its heuristic (every step costs at least 1), so its paths are cheapest
//! ones; it does not count PathFinder's ops (jump-point expansions), so `max_ops` is not modelled
//! and a search fails only when no path exists. The engine model follows V51 and V52 for moves (a move
//! into a blocked tile or a tile whose creep stays fails, two moves into one tile fail the test)
//! and V151 and V152 for transfers: a creep that ends a tick on an edge tile is on the tile across
//! in the next room on the next tick, unless that room refuses it, when it stays where it is.

use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::rc::Rc;

use super::*;
use crate::config::{CACHE_MMO, MOVEMENT_MMO};
use crate::intents::{ActLog, IntentBuffer, Outcome};
use crate::snapshot::{Controller, CreepRow, ObjectKey, TickContext};
use grid::tests::{raw, tile};
use rustedscreep_consts::{RoomCoordinate, Terrain};

const LAST: u8 = ROOM_SIZE - 1;

fn name(n: &str) -> RoomName {
    n.parse().expect("room")
}

fn at(room: RoomName, x: u8, y: u8) -> Position {
    Position::new(
        RoomCoordinate::new(x).expect("x"),
        RoomCoordinate::new(y).expect("y"),
        room,
    )
}

fn mask(raw: &[u8], t: Tile) -> u8 {
    raw[usize::from(t.y()) * usize::from(ROOM_SIZE) + usize::from(t.x())]
}

/// The world's terrain as the PathFinder model sees it: refreshed from the world at the start of
/// every tick, so terrain a test sets after building the world counts.
type Terrains = Rc<RefCell<Vec<(RoomName, Vec<u8>)>>>;

/// The PathFinder model (V163 to V166).
struct ModelPathFinder {
    terrain: Terrains,
    searches: Rc<Cell<u32>>,
}

impl ModelPathFinder {
    /// The cost of entering `p`, or `None` where PathFinder treats it as an obstacle.
    fn look(
        &self,
        p: Position,
        q: &RouteQuery,
        rooms: &RouteRooms<'_>,
        matrices: &mut HashMap<RoomName, Option<[u8; ROOM_AREA]>>,
    ) -> Option<u32> {
        let room = p.room_name();
        let terrain = self.terrain.borrow();
        let raw = &terrain.iter().find(|(n, _)| *n == room)?.1;
        if rooms.closed(room) {
            return None;
        }
        let t = Tile::of(p);
        let matrix = matrices.entry(room).or_insert_with(|| {
            let mut bytes = [0u8; ROOM_AREA];
            match rooms.grid(room) {
                Some((g, _)) => g.write_matrix(&mut bytes, rooms.dips(room)),
                None => {
                    route::unknown_room_matrix(&mut bytes);
                    for t in rooms.dips(room) {
                        bytes[t.index()] = BLOCKED;
                    }
                }
            }
            Some(bytes)
        });
        if let Some(m) = matrix {
            match m[t.index()] {
                0 => {}
                BLOCKED => return None,
                v => return Some(u32::from(v)),
            }
        }
        let m = mask(raw, t);
        if m & (Terrain::Wall as u8) != 0 {
            None
        } else if m & (Terrain::Swamp as u8) != 0 {
            Some(u32::from(q.swamp_cost))
        } else {
            Some(u32::from(q.plain_cost))
        }
    }
}

/// The moves PathFinder considers from a tile (pf.cc's astar, V166).
fn model_neighbors(p: Position) -> impl Iterator<Item = Position> {
    let (wx, wy) = p.world_coords();
    let (x, y) = (p.x().u8(), p.y().u8());
    DIRECTIONS.into_iter().filter_map(move |d| {
        let (dx, dy): (i32, i32) = d.into();
        if x == 0 || x == LAST {
            let across = (x == 0 && dx == -1) || (x == LAST && dx == 1);
            if (across && dy != 0) || dx == 0 {
                return None;
            }
        } else if y == 0 || y == LAST {
            let across = (y == 0 && dy == -1) || (y == LAST && dy == 1);
            if (across && dx != 0) || dy == 0 {
                return None;
            }
        }
        Position::checked_from_world_coords(wx + dx, wy + dy).ok()
    })
}

impl RouteSearch for ModelPathFinder {
    fn search(
        &mut self,
        q: &RouteQuery,
        rooms: &RouteRooms<'_>,
        path: &mut Vec<Position>,
    ) -> RouteOutcome {
        self.searches.set(self.searches.get() + 1);
        path.clear();
        let in_range = |p: Position| {
            let (a, b) = (p.world_coords(), q.to.world_coords());
            (a.0 - b.0).abs().max((a.1 - b.1).abs()) <= i32::from(q.range)
        };
        if in_range(q.from) {
            return RouteOutcome::Found;
        }
        let mut matrices = HashMap::new();
        let mut dist: HashMap<(i32, i32), u32> = HashMap::new();
        let mut parent: HashMap<(i32, i32), Position> = HashMap::new();
        let goal = q.to.world_coords();
        let h = |w: (i32, i32)| {
            let d = (w.0 - goal.0).abs().max((w.1 - goal.1).abs());
            u32::try_from((d - i32::from(q.range)).max(0)).expect("non-negative")
        };
        let mut heap = BinaryHeap::new();
        let origin = q.from.world_coords();
        dist.insert(origin, 0);
        heap.push(Reverse((h(origin), 0u32, origin)));
        while let Some(Reverse((_, g, w))) = heap.pop() {
            if dist.get(&w).is_some_and(|&d| d < g) {
                continue;
            }
            let u = Position::checked_from_world_coords(w.0, w.1).expect("in the world");
            if in_range(u) {
                let mut p = u;
                while p != q.from {
                    path.push(p);
                    p = parent[&p.world_coords()];
                }
                path.reverse();
                return RouteOutcome::Found;
            }
            for v in model_neighbors(u) {
                let Some(cost) = self.look(v, q, rooms, &mut matrices) else {
                    continue;
                };
                let next = g + cost;
                let key = v.world_coords();
                if dist.get(&key).is_none_or(|&d| next < d) {
                    dist.insert(key, next);
                    parent.insert(key, u);
                    heap.push(Reverse((next + h(key), next, key)));
                }
            }
        }
        RouteOutcome::Incomplete
    }
}

struct Room {
    name: RoomName,
    raw: Vec<u8>,
    objects: Vec<(Tile, Footprint)>,
    owned: bool,
}

struct Walker {
    key: u128,
    pos: Position,
    goal: Option<(Position, u8)>,
}

struct World {
    rooms: Vec<Room>,
    /// Rooms the engine refuses transfers into (not in `accessibleRooms`, V152, V168).
    refuse: Vec<RoomName>,
    creeps: Vec<Walker>,
    time: u32,
    snap: Snapshot,
    buffer: IntentBuffer,
    movement: Movement,
    cfg: MovementConfig,
    searches: Rc<Cell<u32>>,
    terrain: Terrains,
    /// Per tick: the rooms the first-read job read.
    reads: Vec<Vec<RoomName>>,
    /// Per tick and creep: the request's result.
    progress: Vec<Vec<Option<Result<Progress, MoveError>>>>,
    /// Ticks a creep started on an edge tile of the room it was in on the tick before.
    held_on_edge: Vec<u32>,
}

impl World {
    fn new(rooms: Vec<Room>) -> Self {
        let terrain: Terrains = Rc::new(RefCell::new(Vec::new()));
        let searches = Rc::new(Cell::new(0));
        let mut movement = Movement::default();
        movement.set_router(Box::new(ModelPathFinder {
            terrain: Rc::clone(&terrain),
            searches: Rc::clone(&searches),
        }));
        Self {
            rooms,
            refuse: Vec::new(),
            creeps: Vec::new(),
            time: 5000,
            snap: Snapshot::default(),
            buffer: IntentBuffer::default(),
            movement,
            cfg: MOVEMENT_MMO,
            searches,
            terrain,
            reads: Vec::new(),
            progress: Vec::new(),
            held_on_edge: Vec::new(),
        }
    }

    /// Open plain rooms; the first one is owned.
    fn open(names: &[&str]) -> Self {
        Self::new(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| Room {
                    name: name(n),
                    raw: raw(|_, _| 0),
                    objects: Vec::new(),
                    owned: i == 0,
                })
                .collect(),
        )
    }

    fn room(&mut self, n: &str) -> &mut Room {
        let n = name(n);
        self.rooms.iter_mut().find(|r| r.name == n).expect("room")
    }

    fn add(&mut self, pos: Position, goal: Option<(Position, u8)>) -> usize {
        self.creeps.push(Walker {
            key: 1 + self.creeps.len() as u128,
            pos,
            goal,
        });
        self.held_on_edge.push(0);
        self.creeps.len() - 1
    }

    fn blocked(&self, p: Position) -> bool {
        let r = self
            .rooms
            .iter()
            .find(|r| r.name == p.room_name())
            .expect("room");
        let t = Tile::of(p);
        let road = r
            .objects
            .iter()
            .any(|&(o, f)| o == t && f == Footprint::Road);
        let object = r
            .objects
            .iter()
            .any(|&(o, f)| o == t && f == Footprint::Blocked);
        (mask(&r.raw, t) & (Terrain::Wall as u8) != 0 && !road) || object
    }

    fn step(&mut self) {
        *self.terrain.borrow_mut() = self.rooms.iter().map(|r| (r.name, r.raw.clone())).collect();
        self.snap
            .begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
        let mut handles = Vec::new();
        for c in &self.creeps {
            let row = CreepRow {
                pos: c.pos,
                my: true,
                spawning: false,
                fatigue: 0,
            };
            handles.push(self.snap.creeps.push(ObjectKey(c.key), row).expect("creep"));
        }
        for r in &self.rooms {
            if r.owned || self.creeps.iter().any(|c| c.pos.room_name() == r.name) {
                let controller = if r.owned {
                    Controller::Mine { level: 1 }
                } else {
                    Controller::Neutral
                };
                self.snap.rooms.push(r.name, controller);
            }
        }
        self.snap.finish();
        self.movement.begin(&self.snap, &self.cfg);
        self.movement.set_cache(&self.cfg, &CACHE_MMO);
        let mut list = Vec::new();
        self.movement.first_reads(&self.cfg, &mut list);
        for &n in &list {
            let r = self.rooms.iter().find(|r| r.name == n).expect("room");
            assert!(self.movement.set_terrain(n, &r.raw));
            self.movement.set_statics(n, []);
            self.movement.set_objects(n, r.objects.clone());
        }
        self.reads.push(list.clone());
        self.movement.rescans(&self.cfg, &mut list);
        for &n in &list {
            let r = self.rooms.iter().find(|r| r.name == n).expect("room");
            self.movement.set_objects(n, r.objects.clone());
        }
        self.buffer.begin(&self.snap);
        let mut commit = Commit::default();
        let mut progress = vec![None; self.creeps.len()];
        for (i, c) in self.creeps.iter().enumerate() {
            if let Some((target, range)) = c.goal {
                let to = MoveTo {
                    target,
                    range,
                    priority: 1,
                    tier: Tier::Standard,
                };
                progress[i] =
                    Some(
                        self.movement
                            .request(&self.snap, handles[i], to, &self.cfg, &mut commit),
                    );
            }
        }
        self.progress.push(progress);
        let caps = ShoveCaps::from_cpu_left([100.0; 3]);
        self.movement
            .resolve(&self.snap, &mut self.buffer, &self.cfg, caps, &mut commit);
        self.buffer.resolve();
        let mut dest: Vec<Option<Position>> = vec![None; self.creeps.len()];
        let mut log = ActLog::default();
        self.buffer.issue(&mut log, |issue| {
            let CreepIntent::Move { direction } = issue.intent else {
                panic!("only moves are queued here: {:?}", issue.intent);
            };
            let i = issue.actor as usize;
            let c = &self.creeps[i];
            let to = Tile::of(c.pos)
                .step(direction)
                .expect("a move inside the room");
            dest[i] = Some(at(c.pos.room_name(), to.x(), to.y()));
            Some(Outcome::Ok)
        });
        loop {
            let mut changed = false;
            for i in 0..self.creeps.len() {
                let Some(d) = dest[i] else { continue };
                assert!(
                    !(0..self.creeps.len()).any(|j| j != i && dest[j] == Some(d)),
                    "two moves into {d:?}"
                );
                let stays = (0..self.creeps.len())
                    .any(|j| j != i && self.creeps[j].pos == d && dest[j].is_none());
                if self.blocked(d) || stays {
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
                c.pos = d;
            }
        }
        // Transfers (V151, V152).
        for (i, c) in self.creeps.iter_mut().enumerate() {
            let t = Tile::of(c.pos);
            if !t.is_edge() {
                continue;
            }
            let side = t.side().expect("a creep never ends on a corner");
            let next = c.pos.room_name().checked_add(side.room_offset());
            let open = next
                .filter(|n| self.rooms.iter().any(|r| r.name == *n) && !self.refuse.contains(n));
            match (open, t.across()) {
                (Some(n), Some(a)) => c.pos = at(n, a.x(), a.y()),
                _ => self.held_on_edge[i] += 1,
            }
        }
        self.time += 1;
    }

    /// Steps until every creep with a goal arrived on one tick, at most `limit` ticks; returns the
    /// ticks taken.
    fn run(&mut self, limit: u32) -> u32 {
        for n in 1..=limit {
            self.step();
            let last = self.progress.last().expect("a tick");
            let all = self
                .creeps
                .iter()
                .zip(last)
                .filter(|(c, _)| c.goal.is_some())
                .all(|(_, p)| *p == Some(Ok(Progress::Arrived)));
            if all {
                return n;
            }
        }
        panic!(
            "not arrived in {limit} ticks: {:?}",
            self.creeps.iter().map(|c| c.pos).collect::<Vec<_>>()
        );
    }

    fn errors(&self) -> Vec<MoveError> {
        self.progress
            .iter()
            .flatten()
            .filter_map(|p| match p {
                Some(Err(e)) => Some(*e),
                _ => None,
            })
            .collect()
    }
}

/// Uses: ROOM_SIZE. A creep crosses two open rooms to a target in a third: one search serves the
/// whole route (the middle room's exit comes from it), each crossing lands it on the tile across
/// (V151), its new room is read on the tick it arrives so it never waits on the edge, and it takes
/// one tick per world tile less the free transfer of each crossing, plus the owned room's first
/// read. A creep that starts in the middle room later rides the same cache.
#[test]
fn a_creep_crosses_two_rooms_on_one_search() {
    let mut w = World::open(&["E0N0", "E1N0", "E2N0"]);
    let start = at(name("E0N0"), 10, 25);
    let target = at(name("E2N0"), 25, 25);
    w.add(start, Some((target, 1)));
    let mut ticks = 0u32;
    loop {
        w.step();
        ticks += 1;
        // A crossing is a move that worked: it never counts toward a detour or a suspect tile.
        assert!(
            w.movement
                .navs
                .iter()
                .all(|n| n.stuck == 0 && n.unexplained == 0),
            "a crossing counted as a failed move at tick {ticks}"
        );
        if w.progress.last().expect("tick")[0] == Some(Ok(Progress::Arrived)) {
            break;
        }
        assert!(ticks < 200, "not arrived");
    }
    // World tiles from x 10 to within 1 of x 25 two rooms on, less one free step per crossing.
    let world = (2 * i32::from(ROOM_SIZE) + 25 - 1) - 10;
    let crossings = 2;
    assert_eq!(
        i64::from(ticks),
        i64::from(world - crossings) + 1,
        "one tick per step"
    );
    assert_eq!(w.searches.get(), 1);
    assert_eq!(w.movement.totals().routes, 1);
    assert_eq!(w.movement.totals().crossed, 2);
    assert_eq!(w.movement.totals().transfers_refused, 0);
    assert!(w.errors().is_empty(), "{:?}", w.errors());
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
    // Each room after the first was read on the tick the creep came in.
    let middle = w.reads.iter().position(|r| r.contains(&name("E1N0")));
    let last = w.reads.iter().position(|r| r.contains(&name("E2N0")));
    assert!(middle.is_some() && last.is_some() && middle < last);
    // A second creep starting in the middle room: the middle room's exit is still cached.
    let i = w.add(at(name("E1N0"), 5, 40), Some((target, 1)));
    w.creeps[0].goal = None;
    let before = w.searches.get();
    w.run(200);
    assert_eq!(w.searches.get(), before, "no second search");
    assert_eq!(
        w.progress.last().expect("tick")[i],
        Some(Ok(Progress::Arrived))
    );
}

/// Uses: ROOM_SIZE. Two creeps cross a one-tile gap in a room border in opposite directions, timed so
/// that the one coming in lands on the exit the other is about to step onto, next to it: the
/// traffic manager moves them past each other (V52) instead of leaving both stuck, and neither goes
/// back over the edge.
#[test]
fn creeps_pass_each_other_through_a_one_tile_exit() {
    let wall = Terrain::Wall as u8;
    let gap = 25;
    let mut w = World::open(&["E0N0", "E1N0"]);
    w.room("E0N0").raw = raw(|x, y| if x == LAST && y != gap { wall } else { 0 });
    w.room("E1N0").raw = raw(|x, y| if x == 0 && y != gap { wall } else { 0 });
    w.room("E1N0").owned = true;
    let east = at(name("E1N0"), 30, gap);
    let west = at(name("E0N0"), 20, gap);
    // Both are 8 moves from where they meet: A beside the exit, B on it after its transfer.
    let a = w.add(at(name("E0N0"), LAST - 9, gap), Some((east, 0)));
    let b = w.add(at(name("E1N0"), 8, gap), Some((west, 0)));
    let mut met = false;
    for _ in 0..100 {
        w.step();
        let exit = at(name("E0N0"), LAST, gap);
        let beside = |p: Position| {
            p.room_name() == exit.room_name() && Tile::of(p).range(Tile::of(exit)) == 1
        };
        met |= beside(w.creeps[a].pos) && w.creeps[b].pos == exit;
        let last = w.progress.last().expect("tick");
        if last.iter().all(|p| *p == Some(Ok(Progress::Arrived))) {
            break;
        }
    }
    assert!(met, "the two never met at the gap");
    assert_eq!(w.creeps[a].pos, east);
    assert_eq!(w.creeps[b].pos, west);
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
    assert_eq!(w.movement.totals().crossed, 2);
    assert!(w.errors().is_empty(), "{:?}", w.errors());
}

/// Uses: ROOM_SIZE, MOVE_COST_SWAMP. The cheap route runs through a room the engine refuses
/// creeps into (V152, V168). The creep steps onto that exit, stays in its room, and on the next tick
/// the room beyond is held closed and the route is searched again around it, through the swamp,
/// so it stands on the refused exit for one tick only and still arrives.
#[test]
fn a_refused_transfer_closes_the_room_and_reroutes() {
    let swamp = Terrain::Swamp as u8;
    let mut w = World::open(&["E0N0", "E1N0", "E0N1", "E1N1"]);
    w.room("E0N1").raw = raw(|_, _| swamp);
    w.refuse.push(name("E1N0"));
    let target = at(name("E1N1"), 10, 45);
    w.add(at(name("E0N0"), 45, 40), Some((target, 1)));
    w.run(400);
    assert_eq!(w.held_on_edge, vec![1]);
    let t = w.movement.totals();
    assert_eq!(t.transfers_refused, 1);
    assert_eq!(t.routes, 2, "the first route and the one around");
    assert_eq!(w.searches.get(), 2);
    assert_eq!(t.crossed, 2, "into E0N1 and E1N1");
    let closed: Vec<RoomName> = w.movement.closed_rooms().iter().map(|&(r, _)| r).collect();
    assert_eq!(closed, vec![name("E1N0")]);
}

/// A route searched while the middle room was known only by its terrain leads to exits that the
/// room's objects wall off. When the creep comes in, the room is read, the cached exit cannot be
/// reached in its grid, and the same request searches again from there, so the creep does not go
/// back over the edge (V151) and arrives by the other gap.
#[test]
fn an_unreachable_exit_is_searched_again() {
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E0N0", "E1N0", "E2N0"]);
    // One way in from the west at y 25; two ways out east: y 20..=30 and y 5..=6.
    w.room("E0N0").raw = raw(|x, y| if x == LAST && y != 25 { wall } else { 0 });
    w.room("E1N0").raw = raw(|x, y| {
        let west_closed = x == 0 && y != 25;
        let east_closed = x == LAST && !(20..=30).contains(&y) && !(5..=6).contains(&y);
        if west_closed || east_closed { wall } else { 0 }
    });
    // A pocket of walls around the wide east gap, which terrain alone does not show.
    let mut pocket = Vec::new();
    for y in 18..=32 {
        pocket.push((tile(LAST - 2, y), Footprint::Blocked));
    }
    for x in LAST - 2..LAST {
        pocket.push((tile(x, 18), Footprint::Blocked));
        pocket.push((tile(x, 32), Footprint::Blocked));
    }
    w.room("E1N0").objects = pocket;
    // South of the wide gap's line, so the way through it is strictly the shorter.
    let target = at(name("E2N0"), 25, 40);
    w.add(at(name("E0N0"), 30, 25), Some((target, 1)));
    w.run(300);
    assert_eq!(w.searches.get(), 2, "from E0N0, then again from E1N0");
    assert_eq!(w.movement.totals().route_failures, 0);
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
    assert!(
        !w.errors().contains(&MoveError::NoPath),
        "the re-search ran in the same request"
    );
    assert_eq!(w.movement.totals().crossed, 2);
}

/// Uses: the default `route_retry_ticks`. A target no route reaches: the first request searches and
/// fails, the creep holds (`NoPath`) in its room, and the failure is not searched again until
/// `route_retry_ticks` have passed.
#[test]
fn no_route_is_remembered_for_retry_ticks() {
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E0N0", "E1N0"]);
    w.room("E0N0").raw = raw(|x, _| if x == LAST { wall } else { 0 });
    let start = at(name("E0N0"), 30, 25);
    w.add(start, Some((at(name("E1N0"), 25, 25), 1)));
    let retry = w.cfg.route_retry_ticks;
    for _ in 0..=retry {
        w.step();
    }
    assert_eq!(w.searches.get(), 1);
    assert_eq!(w.creeps[0].pos, start);
    assert!(
        w.progress
            .iter()
            .all(|p| p[0] == Some(Err(MoveError::NoPath)))
    );
    assert_eq!(w.movement.totals().route_failures, 1);
    w.step();
    assert_eq!(
        w.searches.get(),
        2,
        "searched again after route_retry_ticks"
    );
}

/// Without a route search installed (the P2.1 behaviour), a target in another room is refused and
/// the creep's room is not read for it.
#[test]
fn without_a_router_another_room_is_refused() {
    let mut w = World::open(&["E0N0", "E1N0"]);
    w.movement = Movement::default();
    w.room("E0N0").owned = false;
    w.add(
        at(name("E0N0"), 30, 25),
        Some((at(name("E1N0"), 25, 25), 1)),
    );
    for _ in 0..3 {
        w.step();
    }
    assert!(
        w.progress
            .iter()
            .all(|p| p[0] == Some(Err(MoveError::OtherRoom)))
    );
    assert!(w.reads.iter().all(Vec::is_empty));
}

/// A route search that hands back a path whose exit the room's own grid cannot reach (a pocket the
/// search's matrix did not know about).
struct Lying {
    path: Vec<Position>,
    searches: Rc<Cell<u32>>,
}

impl RouteSearch for Lying {
    fn search(
        &mut self,
        _: &RouteQuery,
        _: &RouteRooms<'_>,
        path: &mut Vec<Position>,
    ) -> RouteOutcome {
        self.searches.set(self.searches.get() + 1);
        path.clear();
        path.extend(self.path.iter().copied());
        RouteOutcome::Found
    }
}

/// Uses: the default `route_retry_ticks`. A fresh search whose exit the creep cannot reach in its
/// room's grid is remembered as no way for that creep: it is not searched again (twice per tick, as
/// the review found) before `route_retry_ticks`, and the creep holds.
#[test]
fn an_unreachable_exit_from_a_fresh_search_is_not_searched_every_tick() {
    let mut w = World::open(&["E0N0", "E1N0"]);
    let mut pocket = vec![
        (tile(LAST - 1, 9), Footprint::Blocked),
        (tile(LAST - 1, 13), Footprint::Blocked),
    ];
    pocket.extend((9..=13).map(|y| (tile(LAST - 2, y), Footprint::Blocked)));
    w.room("E0N0").objects = pocket;
    let target = at(name("E1N0"), 25, 11);
    let mut path = vec![at(name("E0N0"), LAST, 11)];
    path.extend((0..=24).map(|x| at(name("E1N0"), x, 11)));
    let searches = Rc::new(Cell::new(0));
    w.movement.set_router(Box::new(Lying {
        path,
        searches: Rc::clone(&searches),
    }));
    let start = at(name("E0N0"), 30, 30);
    w.add(start, Some((target, 1)));
    let retry = w.cfg.route_retry_ticks;
    for _ in 0..=retry {
        w.step();
    }
    assert_eq!(searches.get(), 1, "one search per route_retry_ticks");
    assert_eq!(w.movement.totals().route_failures, 1);
    assert_eq!(w.creeps[0].pos, start);
    assert!(
        w.progress
            .iter()
            .all(|p| p[0] == Some(Err(MoveError::NoPath)))
    );
    w.step();
    assert_eq!(searches.get(), 2, "searched again after route_retry_ticks");
}

/// Uses: ROOM_SIZE. A room split in two by a wall, whose halves join only through the room to the
/// north: the path leaves the start room twice. The route is kept whole and the creep follows it leg
/// by leg, so it tells the two visits apart: one search, three crossings, no `NoPath`, never back
/// over an edge. A second creep in the first half joins the kept route (its first exit is in reach)
/// and makes no search at all.
#[test]
fn a_room_crossed_twice_is_one_route_for_every_creep_in_its_first_half() {
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E0N0", "E0N1", "E1N0"]);
    let top = |x: u8| (10..=20).contains(&x) || (30..=40).contains(&x);
    w.room("E0N0").raw = raw(|x, y| {
        let edge_wall =
            (y == 0 && !top(x)) || y == LAST || x == 0 || (x == LAST && !(20..=30).contains(&y));
        if edge_wall || (x == 25 && y > 0 && y < LAST) {
            wall
        } else {
            0
        }
    });
    w.room("E0N1").raw = raw(|x, y| {
        let edge = x == 0 || y == 0 || x == LAST || (y == LAST && !top(x));
        if edge { wall } else { 0 }
    });
    w.room("E1N0").raw = raw(|x, y| {
        let edge = y == 0 || y == LAST || x == LAST || (x == 0 && !(20..=30).contains(&y));
        if edge { wall } else { 0 }
    });
    let target = at(name("E1N0"), 25, 25);
    w.add(at(name("E0N0"), 10, 25), Some((target, 1)));
    w.run(400);
    assert_eq!(w.searches.get(), 1, "one search for the whole trip");
    assert_eq!(
        w.movement.totals().crossed,
        3,
        "into E0N1, back into E0N0, into E1N0"
    );
    assert!(w.errors().is_empty(), "{:?}", w.errors());
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
    w.creeps[0].goal = None;
    let i = w.add(at(name("E0N0"), 12, 30), Some((target, 1)));
    for _ in 0..400 {
        w.step();
        if w.progress.last().expect("tick")[i] == Some(Ok(Progress::Arrived)) {
            break;
        }
    }
    assert_eq!(
        w.progress.last().expect("tick")[i],
        Some(Ok(Progress::Arrived))
    );
    assert_eq!(w.searches.get(), 1, "the second creep rode the kept route");
    assert_eq!(w.movement.totals().crossed, 6);
}

/// Uses: ROOM_SIZE, MOVE_COST_SWAMP. The cheapest way south runs out of the start room into the
/// east one, round a swamp band, and back in further south (the fix check's case). With first exits
/// cached, creeps went round between the two rooms (22 crossings in 600 ticks); with the last
/// visit's exits the creep arrives after three crossings on one search.
#[test]
fn a_path_that_dips_through_a_neighbour_does_not_loop() {
    let swamp = Terrain::Swamp as u8;
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E0N0", "E1N0", "E0S0"]);
    w.room("E0N0").raw = raw(|x, y| {
        if x == 0 || y == 0 {
            wall
        } else if (10..=40).contains(&y) {
            swamp
        } else {
            0
        }
    });
    w.room("E1N0").raw = raw(|x, y| {
        if y == 0 || y == LAST || x == LAST {
            wall
        } else {
            0
        }
    });
    w.room("E0S0").raw = raw(|x, y| {
        if x == 0 || x == LAST || y == LAST {
            wall
        } else {
            0
        }
    });
    let target = at(name("E0S0"), 45, 25);
    w.add(at(name("E0N0"), 45, 5), Some((target, 1)));
    w.run(300);
    assert_eq!(w.searches.get(), 1);
    assert_eq!(
        w.movement.totals().crossed,
        3,
        "into E1N0, back into E0N0, into E0S0"
    );
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
}

/// A creep sealed in by blocked tiles searches first and finds nothing; that failure is its own, so
/// a creep elsewhere in the room with a way out still searches and arrives (the fix check's case:
/// a shared failure stopped it for every tick).
#[test]
fn one_creeps_failed_search_does_not_stop_the_others() {
    let mut w = World::open(&["E1N0", "E2N0"]);
    let ring: Vec<(Tile, Footprint)> = tile(10, 10)
        .neighbors()
        .map(|(_, t)| (t, Footprint::Blocked))
        .collect();
    w.room("E1N0").objects = ring;
    let target = at(name("E2N0"), 25, 25);
    let sealed = w.add(at(name("E1N0"), 10, 10), Some((target, 1)));
    let free = w.add(at(name("E1N0"), 20, 30), Some((target, 1)));
    for _ in 0..150 {
        w.step();
        if w.progress.last().expect("tick")[free] == Some(Ok(Progress::Arrived)) {
            break;
        }
    }
    assert_eq!(
        w.progress.last().expect("tick")[free],
        Some(Ok(Progress::Arrived))
    );
    assert_eq!(w.creeps[sealed].pos, at(name("E1N0"), 10, 10));
    assert_eq!(
        w.searches.get(),
        2,
        "one each; the sealed creep's failure is held"
    );
}

/// Uses: ROOM_SIZE. The route runs through a room the first search knew only by its terrain, where
/// objects cut the way on. When the creep comes in, the cached exit (searched on no grid) is searched
/// again from there and nothing is found; the room it came from forgets its exit, and the creep, on
/// the edge tile it landed on, steps into the room instead of going back over the edge (V151).
#[test]
fn a_dead_end_room_is_not_left_by_going_back_over_the_edge() {
    let mut w = World::open(&["E0N0", "E1N0", "E2N0"]);
    let wall = Terrain::Wall as u8;
    for n in ["E0N0", "E1N0", "E2N0"] {
        w.room(n).raw = raw(|_, y| if y == 0 || y == LAST { wall } else { 0 });
    }
    w.room("E1N0").objects = (1..LAST)
        .map(|y| (tile(30, y), Footprint::Blocked))
        .collect();
    let target = at(name("E2N0"), 25, 25);
    w.add(at(name("E0N0"), 10, 25), Some((target, 1)));
    for _ in 0..80 {
        w.step();
    }
    let c = &w.creeps[0];
    assert_eq!(c.pos.room_name(), name("E1N0"));
    assert!(!Tile::of(c.pos).is_edge(), "{:?}", c.pos);
    assert_eq!(w.movement.totals().crossed, 1, "it never went back");
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
    assert_eq!(
        w.searches.get(),
        2,
        "from E0N0, then again from E1N0; the failure is held"
    );
    let now = w.time;
    assert_eq!(
        w.movement
            .routes()
            .through(name("E0N0"), target, 1, now, u32::MAX)
            .count(),
        0,
        "the route into the dead end is gone"
    );
    assert_eq!(
        w.progress.last().expect("tick")[0],
        Some(Err(MoveError::NoPath))
    );
}

/// Uses: ROOM_SIZE, MOVE_COST_ROAD and MOVE_COST_PLAIN (through the profile). A road makes the tile
/// straight in from the arrival tile the only best step, and an own creep that has no request stands
/// on it (it is pinned). The arriving creep, which would go back over the edge if it stayed (V151),
/// takes another way in at once, so it crosses once and arrives.
#[test]
fn a_creep_whose_best_step_in_is_taken_does_not_go_back() {
    let wall = Terrain::Wall as u8;
    let gap = 25;
    let mut w = World::open(&["E0N0", "E1N0"]);
    w.room("E0N0").raw = raw(|x, y| if x == LAST && y != gap { wall } else { 0 });
    w.room("E1N0").raw = raw(|x, y| if x == 0 && y != gap { wall } else { 0 });
    w.room("E1N0").objects = (1..25).map(|x| (tile(x, gap), Footprint::Road)).collect();
    let target = at(name("E1N0"), 25, gap);
    let i = w.add(at(name("E0N0"), 40, gap), Some((target, 1)));
    w.add(at(name("E1N0"), 1, gap), None);
    for _ in 0..100 {
        w.step();
        if w.progress.last().expect("tick")[i] == Some(Ok(Progress::Arrived)) {
            break;
        }
    }
    assert_eq!(
        w.progress.last().expect("tick")[i],
        Some(Ok(Progress::Arrived))
    );
    assert_eq!(w.movement.totals().crossed, 1, "no bounce");
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
}

/// Uses: ROOM_SIZE. A room split in two whose halves leave by different sides: the east half straight
/// into the target room, the west half north and round through two other rooms. A creep in the east
/// half searches first, so the shared exit is the east one; a creep in the west half cannot reach it
/// and searches for itself, which finds the north exit on the same grid, and that must not replace
/// the shared exit: a third creep in the east half then goes on the cached exit with no search.
#[test]
fn a_cut_off_creeps_search_does_not_take_the_shared_exit() {
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E0N0", "E0N1", "E1N1", "E1N0"]);
    let gap = |v: u8| (20..=30).contains(&v);
    let west_gap = |v: u8| (10..=20).contains(&v);
    w.room("E0N0").raw = raw(|x, y| {
        let open = (y == 0 && west_gap(x)) || (x == LAST && gap(y));
        let edge = x == 0 || y == 0 || x == LAST || y == LAST;
        if (edge && !open) || (x == 25 && !edge) {
            wall
        } else {
            0
        }
    });
    w.room("E0N1").raw = raw(|x, y| {
        let open = (y == LAST && west_gap(x)) || (x == LAST && gap(y));
        let edge = x == 0 || y == 0 || x == LAST || y == LAST;
        if edge && !open { wall } else { 0 }
    });
    w.room("E1N1").raw = raw(|x, y| {
        let open = (x == 0 && gap(y)) || (y == LAST && gap(x));
        let edge = x == 0 || y == 0 || x == LAST || y == LAST;
        if edge && !open { wall } else { 0 }
    });
    w.room("E1N0").raw = raw(|x, y| {
        let open = (y == 0 && gap(x)) || (x == 0 && gap(y));
        let edge = x == 0 || y == 0 || x == LAST || y == LAST;
        if edge && !open { wall } else { 0 }
    });
    let target = at(name("E1N0"), 25, 25);
    let arrive = |w: &mut World, pos: Position| {
        for c in &mut w.creeps {
            c.goal = None;
        }
        let i = w.add(pos, Some((target, 1)));
        for _ in 0..400 {
            w.step();
            if w.progress.last().expect("tick")[i] == Some(Ok(Progress::Arrived)) {
                return;
            }
        }
        panic!("creep {i} from {pos:?} did not arrive");
    };
    arrive(&mut w, at(name("E0N0"), 40, 25));
    assert_eq!(w.searches.get(), 1);
    arrive(&mut w, at(name("E0N0"), 10, 25));
    assert_eq!(w.searches.get(), 2, "the west creep searched for itself");
    arrive(&mut w, at(name("E0N0"), 40, 30));
    assert_eq!(w.searches.get(), 2, "the east creep rode the shared exit");
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
}

/// Uses: ROOM_SIZE, MOVE_COST_SWAMP. The cheap way runs north and through a room that refuses
/// creeps (V152, V168); the other way is through a swamp room. With exits kept per room, the
/// re-route from the north room kept the start room's old exit north and the north room's new exit
/// south, and the creep went back and forth between them (the second fix check: 1175 crossings in
/// 1200 ticks). With routes kept whole, the refusal drops the route and the creep follows the new
/// one from where it stands, and arrives.
#[test]
fn a_reroute_after_a_refusal_does_not_loop_between_two_rooms() {
    let swamp = Terrain::Swamp as u8;
    let mut w = World::open(&["E1N0", "E1N1", "E2N1", "E3N1", "E2N0", "E3N0"]);
    w.room("E2N0").raw = raw(|_, _| swamp);
    w.refuse.push(name("E2N1"));
    let target = at(name("E3N1"), 25, 25);
    w.add(at(name("E1N0"), 25, 25), Some((target, 1)));
    w.run(600);
    assert_eq!(w.movement.totals().transfers_refused, 1);
    assert_eq!(w.searches.get(), 2, "the first route and the one around");
    assert!(
        w.movement.totals().crossed <= 5,
        "{}",
        w.movement.totals().crossed
    );
}

/// Uses: ROOM_SIZE. A wall appears across the middle room while the creep is in it, and the room is
/// rescanned (a new grid version): the creep cannot reach its exit, its route is dropped (found on
/// an older grid) and the creep searches from there; the new route runs back through the start
/// room and round through the rooms to the north. With exits kept per room the start room still
/// sent it back east (1457 crossings in 1500 ticks in the fix check); with routes kept whole it
/// arrives.
#[test]
fn a_reroute_after_a_rescan_does_not_loop_between_two_rooms() {
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E0N0", "E1N0", "E2N0", "E0N1", "E1N1", "E2N1"]);
    // The middle room is closed to the north, so the way round runs through the start room.
    w.room("E1N0").raw = raw(|_, y| if y == 0 || y == LAST { wall } else { 0 });
    let target = at(name("E2N0"), 25, 25);
    w.add(at(name("E0N0"), 25, 25), Some((target, 1)));
    let mut walled = false;
    for _ in 0..1500 {
        w.step();
        let c = &w.creeps[0];
        if !walled && c.pos.room_name() == name("E1N0") && c.pos.x().u8() >= 5 {
            w.room("E1N0").objects = (1..LAST)
                .map(|y| (tile(40, y), Footprint::Blocked))
                .collect();
            w.movement.invalidate(name("E1N0"));
            walled = true;
        }
        if w.progress.last().expect("tick")[0] == Some(Ok(Progress::Arrived)) {
            break;
        }
    }
    assert!(walled);
    assert_eq!(
        w.progress.last().expect("tick")[0],
        Some(Ok(Progress::Arrived))
    );
    // The first route (east through the walled room) went with the rescan; only the new one runs
    // through the middle room now.
    let now = w.time;
    assert_eq!(
        w.movement
            .routes()
            .through(name("E1N0"), target, 1, now, u32::MAX)
            .count(),
        1
    );
    assert!(
        w.movement.totals().crossed <= 6,
        "{}",
        w.movement.totals().crossed
    );
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
}

/// Uses: ROOM_SIZE. A wall cuts the start room from top to bottom but for the bottom edge tile under
/// it, and the cheapest path steps onto that edge tile and back into the room (PathFinder allows
/// it, V166), which no creep can do (V151). The bot learns the tile, searches again around it
/// (through the rooms to the south) and arrives.
#[test]
fn a_path_that_steps_onto_an_edge_and_back_is_searched_again_around_it() {
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E1N0", "E2N0", "E1S0", "E2S0"]);
    w.room("E1N0").raw = raw(|x, y| if x == 40 && y < LAST { wall } else { 0 });
    let target = at(name("E2N0"), 25, 25);
    w.add(at(name("E1N0"), 20, 25), Some((target, 1)));
    w.run(400);
    let t = w.movement.totals();
    assert_eq!(t.route_dips, 1, "the edge tile under the wall");
    assert_eq!(w.searches.get(), 2, "the dip, then the way round");
    assert_eq!(t.route_failures, 0);
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
}

/// Uses: ROOM_SIZE. The target is in the east part of its room, which the west part reaches only
/// through the room to the north (the wall's top end is an edge tile, which a path can step onto
/// and back from, V166, and no creep can, V151: the bot learns it). A creep from the room to the
/// west follows its route through the target room's west part and the north room (its leg in the
/// target room leaves it again); and a creep that starts in the west part, with the target in its
/// own room and no way to it inside the room, searches a route out and back in. Both arrive (the
/// second fix check: both held `NoPath`).
#[test]
fn a_target_reached_only_through_a_neighbour_is_reached() {
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E0N0", "E1N0", "E1N1"]);
    w.room("E1N0").raw = raw(|x, y| {
        if y == LAST || (x == 25 && y > 0) {
            wall
        } else {
            0
        }
    });
    w.room("E1N1").raw = raw(|_, y| if y == 0 { wall } else { 0 });
    let target = at(name("E1N0"), 35, 25);
    let a = w.add(at(name("E0N0"), 25, 25), Some((target, 1)));
    for _ in 0..300 {
        w.step();
        if w.progress.last().expect("tick")[a] == Some(Ok(Progress::Arrived)) {
            break;
        }
    }
    assert_eq!(
        w.progress.last().expect("tick")[a],
        Some(Ok(Progress::Arrived))
    );
    // From the west room (its path steps over the wall's top end), again from the west part when the
    // creep finds no way inside the room, and once more around the learned edge tile, all in the
    // request on the tick it comes in: it never holds `NoPath` (the final review: 101 ticks).
    assert_eq!(w.searches.get(), 3);
    assert!(
        w.progress
            .iter()
            .all(|p| p.get(a).copied().flatten() != Some(Err(MoveError::NoPath))),
        "no NoPath on the way"
    );
    // At least the wall's top end; the model's path may touch the edge at a few more tiles on
    // equal-cost ties, and those are learned with it.
    assert!(w.movement.totals().route_dips >= 1);
    w.creeps[a].goal = None;
    let b = w.add(at(name("E1N0"), 10, 25), Some((target, 1)));
    for _ in 0..300 {
        w.step();
        if w.progress.last().expect("tick")[b] == Some(Ok(Progress::Arrived)) {
            break;
        }
    }
    assert_eq!(
        w.progress.last().expect("tick")[b],
        Some(Ok(Progress::Arrived))
    );
    // The west part's route out and back in is kept: the second creep joins it.
    assert_eq!(w.searches.get(), 3);
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
}

/// Uses: ROOM_SIZE. The target lies inside a ring of blocked tiles in its room, which the first
/// search knew only by its terrain. The creep follows the route in, finds no way to the target and
/// no way round, steps off the edge, and the route that led it there is dropped, so no other creep
/// from the start room follows it into the dead end.
#[test]
fn a_route_into_a_sealed_target_is_dropped() {
    let mut w = World::open(&["E0N0", "E1N0"]);
    let target = at(name("E1N0"), 25, 25);
    let mut ring: Vec<(Tile, Footprint)> = Vec::new();
    for x in 22..=28 {
        for y in 22..=28 {
            if x == 22 || x == 28 || y == 22 || y == 28 {
                ring.push((tile(x, y), Footprint::Blocked));
            }
        }
    }
    w.room("E1N0").objects = ring;
    w.add(at(name("E0N0"), 25, 25), Some((target, 1)));
    for _ in 0..80 {
        w.step();
    }
    let c = &w.creeps[0];
    assert_eq!(c.pos.room_name(), name("E1N0"));
    assert!(!Tile::of(c.pos).is_edge(), "{:?}", c.pos);
    assert_eq!(w.movement.totals().crossed, 1);
    let now = w.time;
    assert_eq!(
        w.movement
            .routes()
            .through(name("E0N0"), target, 1, now, u32::MAX)
            .count(),
        0,
        "the route into the sealed target is gone"
    );
}

/// Uses: ROOM_SIZE. The middle room, known only by its terrain when the route was searched, has a
/// wall that its path got past by stepping onto the edge tile under it and back (V166). When the
/// creep comes in, its exit is out of reach: the route is dropped, the search from there steps over
/// the same tile, the tile is learned, and one more search goes round through the rooms to the
/// south, all in the request on the tick it arrives (the final review: it held `NoPath` for 101
/// ticks).
#[test]
fn a_dip_in_a_room_known_only_by_terrain_is_searched_around_on_arrival() {
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E0N0", "E1N0", "E2N0", "E1S0", "E2S0"]);
    w.room("E1N0").raw = raw(|x, y| if x == 40 && y < LAST { wall } else { 0 });
    let target = at(name("E2N0"), 25, 25);
    w.add(at(name("E0N0"), 25, 25), Some((target, 1)));
    w.run(400);
    assert!(w.movement.totals().route_dips >= 1);
    assert_eq!(w.movement.totals().route_failures, 0);
    assert!(!w.errors().contains(&MoveError::NoPath), "{:?}", w.errors());
    assert!(w.held_on_edge.iter().all(|&n| n == 0));
}

/// Six creeps head for six targets two rooms on with room for four routes. Routes in use are never
/// evicted (the book grows past its capacity while they travel), so each creep searches once (the
/// final review: with eight travellers and four slots, five searches ran every tick).
#[test]
fn more_travellers_than_kept_routes_do_not_search_every_tick() {
    let mut w = World::open(&["E0N0", "E1N0", "E2N0"]);
    w.cfg.route_cache_size = 4;
    for i in 0..6u8 {
        let target = at(name("E2N0"), 10 + 5 * i, 25);
        w.add(at(name("E0N0"), 10 + 5 * i, 10), Some((target, 1)));
    }
    w.run(300);
    assert_eq!(w.searches.get(), 6, "one search per creep");
}

/// A target inside a ring of blocked tiles in the creep's own room: no way to it inside the room,
/// and none from any exit either, so no route out and back in can help and no PathFinder search is
/// made (P2.1 held `NoPath` for free; the final review found every creep searching).
#[test]
fn an_enclosed_target_in_the_creeps_room_costs_no_search() {
    let mut w = World::open(&["E0N0", "E1N0"]);
    let target = at(name("E0N0"), 25, 25);
    let mut ring: Vec<(Tile, Footprint)> = Vec::new();
    for x in 22..=28 {
        for y in 22..=28 {
            if x == 22 || x == 28 || y == 22 || y == 28 {
                ring.push((tile(x, y), Footprint::Blocked));
            }
        }
    }
    w.room("E0N0").objects = ring;
    for i in 0..3 {
        w.add(at(name("E0N0"), 5 + i, 5), Some((target, 1)));
    }
    for _ in 0..50 {
        w.step();
    }
    assert_eq!(w.searches.get(), 0);
    assert!(
        w.progress
            .iter()
            .skip(1)
            .all(|p| p.iter().all(|r| *r == Some(Err(MoveError::NoPath))))
    );
}

/// A target in a room held closed after a refused transfer: no search is made toward it (the room
/// callback would block it, so PathFinder would spend all its ops), and the creep holds.
#[test]
fn a_target_in_a_closed_room_costs_no_search() {
    let mut w = World::open(&["E0N0", "E1N0"]);
    w.movement.closed.push((name("E1N0"), u32::MAX));
    w.add(
        at(name("E0N0"), 25, 25),
        Some((at(name("E1N0"), 25, 25), 1)),
    );
    for _ in 0..20 {
        w.step();
    }
    assert_eq!(w.searches.get(), 0);
    assert!(
        w.progress
            .iter()
            .skip(1)
            .all(|p| p[0] == Some(Err(MoveError::NoPath)))
    );
}

/// Uses: ROOM_SIZE, MOVE_COST_SWAMP. On the route that dips through a neighbour (out north-east,
/// back in further south, then out south), a second creep that starts near the south exit joins the
/// route at the start room's later leg, the one with the shorter way to its exit, and leaves by the
/// south exit at once (the final review: joining the first leg sent it round through the swamp and
/// the neighbour, three crossings instead of one).
#[test]
fn a_creep_joins_a_kept_route_at_its_nearest_leg() {
    let swamp = Terrain::Swamp as u8;
    let wall = Terrain::Wall as u8;
    let mut w = World::open(&["E0N0", "E1N0", "E0S0"]);
    w.room("E0N0").raw = raw(|x, y| {
        if x == 0 || y == 0 {
            wall
        } else if (10..=40).contains(&y) {
            swamp
        } else {
            0
        }
    });
    w.room("E1N0").raw = raw(|x, y| {
        if y == 0 || y == LAST || x == LAST {
            wall
        } else {
            0
        }
    });
    w.room("E0S0").raw = raw(|x, y| {
        if x == 0 || x == LAST || y == LAST {
            wall
        } else {
            0
        }
    });
    let target = at(name("E0S0"), 45, 25);
    w.add(at(name("E0N0"), 45, 5), Some((target, 1)));
    w.run(300);
    assert_eq!(w.movement.totals().crossed, 3);
    w.creeps[0].goal = None;
    let i = w.add(at(name("E0N0"), 45, 46), Some((target, 1)));
    for _ in 0..300 {
        w.step();
        if w.progress.last().expect("tick")[i] == Some(Ok(Progress::Arrived)) {
            break;
        }
    }
    assert_eq!(
        w.progress.last().expect("tick")[i],
        Some(Ok(Progress::Arrived))
    );
    assert_eq!(w.searches.get(), 1, "it joined the kept route");
    assert_eq!(
        w.movement.totals().crossed,
        4,
        "one crossing for the second creep"
    );
}

/// A creep's route along the top of its room touches the top edge on equal-cost ties (dips that do
/// not matter, so none is learned). A second creep's search toward a room the world does not have
/// then fails: it is held after one search, and the first route's dips are not learned from it
/// (the final review: a failed search learned the last search's dips and searched twice).
#[test]
fn a_failed_search_does_not_learn_another_searchs_dips() {
    let mut w = World::open(&["E0N0", "E1N0"]);
    let a = w.add(at(name("E0N0"), 5, 1), Some((at(name("E1N0"), 25, 1), 0)));
    w.step();
    assert!(
        !w.movement.path_dips.is_empty(),
        "the first path touches the edge"
    );
    for _ in 0..200 {
        w.step();
        if w.progress.last().expect("tick")[a] == Some(Ok(Progress::Arrived)) {
            break;
        }
    }
    assert_eq!(w.movement.totals().route_dips, 0);
    let before = w.searches.get();
    w.creeps[a].goal = None;
    w.add(
        at(name("E0N0"), 30, 30),
        Some((at(name("E5N5"), 25, 25), 1)),
    );
    for _ in 0..5 {
        w.step();
    }
    assert_eq!(w.searches.get(), before + 1);
    assert_eq!(w.movement.totals().route_dips, 0);
}
