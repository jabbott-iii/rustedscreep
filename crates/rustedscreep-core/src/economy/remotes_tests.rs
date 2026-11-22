//! Tests of remote mining's manager and creeps (P6.2, `economy::remotes`). Expected values derive
//! from the cost profile and the constants; the bodies from the model's own solvers.

use rustedscreep_consts::{
    BUILD_POWER, CONTAINER_HITS, CONTROLLER_RESERVE, CONTROLLER_RESERVE_MAX, Part, Position,
    REPAIR_POWER, ROOM_AREA, ROOM_SIZE, RoomCoordinate, RoomName, StructureType, Terrain,
};

use super::remote::{RemoteHome, RemoteRoom, RemoteSource, remote_crew};
use super::remotes::{
    PRIORITY_REMOTE_HAULER, PRIORITY_REMOTE_MINER, PRIORITY_RESERVER, PRIORITY_SCOUT, Remotes,
    WaySearch, candidates, is_remote_room,
};
use crate::config::{HAULING_MMO, MOVEMENT_MMO, REMOTE_MMO, RemoteConfig, SPAWN_MMO};
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{CostGrid, Footprint, Movement, Side, TerrainGrid, Tile, profile};
use crate::scheduler::Commit;
use crate::snapshot::{
    Controller, CreepRow, CreepStore, ObjectKey, ObjectKind, ObjectRow, ObjectStatic, RemoteRead,
    Reservation, RoomEnergy, Snapshot, StructureStatic, TickContext,
};
use crate::spawning::{Body, BodySpec, Identity, Role, Spawner};

fn room(name: &str) -> RoomName {
    name.parse().expect("room")
}

fn tile(x: u8, y: u8) -> Tile {
    Tile::new(x, y).expect("tile")
}

fn pos(r: RoomName, t: Tile) -> Position {
    Position::new(
        RoomCoordinate::new(t.x()).expect("x"),
        RoomCoordinate::new(t.y()).expect("y"),
        r,
    )
}

/// A room's raw terrain (row-major, V156): `terrain(x, y)` gives each tile's mask.
fn raw(terrain: impl Fn(u8, u8) -> u8) -> Vec<u8> {
    let mut out = vec![0u8; ROOM_AREA];
    for y in 0..ROOM_SIZE {
        for x in 0..ROOM_SIZE {
            out[usize::from(y) * usize::from(ROOM_SIZE) + usize::from(x)] = terrain(x, y);
        }
    }
    out
}

fn plain(_: u8, _: u8) -> u8 {
    0
}

fn swamp(_: u8, _: u8) -> u8 {
    Terrain::Swamp as u8
}

fn grid(raw: &[u8], objects: &[(Tile, Footprint)]) -> (CostGrid, TerrainGrid) {
    let terrain = TerrainGrid::from_row_major(raw).expect("terrain");
    let mut g = CostGrid::default();
    g.build(&terrain, objects.iter().copied(), profile(&MOVEMENT_MMO));
    (g, terrain)
}

#[test]
fn remote_rooms_skip_highways_and_sector_centers() {
    for (name, ok) in [
        ("E7N57", true),
        ("E8N57", true),
        ("E6N57", true),
        ("E7N56", true),
        ("E7N58", true),
        // Highways: a coordinate divisible by 10 (V231).
        ("E10N57", false),
        ("E7N60", false),
        ("W0N1", false),
        ("E0S5", false),
        // A sector's center 3 x 3: the center and the keeper rooms (V231).
        ("E5N55", false),
        ("E4N54", false),
        ("E6N56", false),
        ("W4S4", false),
        ("W6N4", false),
        ("E3N54", true),
        ("E7N55", true),
    ] {
        assert_eq!(is_remote_room(room(name)), ok, "{name}");
    }
}

#[test]
fn candidates_follow_the_open_exits() {
    let open = raw(plain);
    let (g, _) = grid(&open, &[]);
    let home = room("E2N2");
    let all = candidates(home, &g);
    assert_eq!(
        all,
        vec![
            (Side::Top, room("E2N3")),
            (Side::Right, room("E3N2")),
            (Side::Bottom, room("E2N1")),
            (Side::Left, room("E1N2")),
        ]
    );
    // A walled left side has no exit there.
    let walled = raw(|x, _| if x == 0 { Terrain::Wall as u8 } else { 0 });
    let (g, _) = grid(&walled, &[]);
    assert!(!candidates(home, &g).iter().any(|c| c.0 == Side::Left));
    // E1N1's left neighbor is the highway E0N1.
    let (g, _) = grid(&open, &[]);
    assert!(
        !candidates(room("E1N1"), &g)
            .iter()
            .any(|c| c.1 == room("E0N1"))
    );
}

/// The way from a spawn at (25, 25) of an open home to the right exit, then to a source at
/// (10, 25) of the room beyond.
#[test]
fn ways_measure_the_path_from_the_cost_profile() {
    let costs = profile(&MOVEMENT_MMO);
    let home_raw = raw(plain);
    let spawn = tile(25, 25);
    let (home, _) = grid(&home_raw, &[(spawn, Footprint::Blocked)]);
    for (fill, rate) in [
        (plain as fn(u8, u8) -> u8, costs.plain),
        (swamp as fn(u8, u8) -> u8, costs.swamp),
    ] {
        let source = tile(10, 25);
        let remote_raw = raw(fill);
        let (remote, terrain) = grid(&remote_raw, &[(source, Footprint::Blocked)]);
        let mut search = WaySearch::default();
        // Every open exit on the right side.
        assert_eq!(
            search.home(&home, spawn, Side::Right),
            usize::from(ROOM_SIZE) - 2
        );
        let (ways, controller) = search
            .remote(&remote, &terrain, costs, &[source], None, &[], &[])
            .expect("ways");
        assert!(controller.is_none());
        let w = &ways[0];
        // From next to the spawn at (26, 25): 22 tiles on plain and the step onto the exit, at
        // the grid's exit cost (the cheapest step), then free into the next room (V151).
        let home_tiles = 23u32;
        let home_cost = 22 * u32::from(costs.plain) + u32::from(costs.cheapest());
        // Then 9 steps to a tile next to the source, the lowest tile index first (column-major).
        assert_eq!(w.tile, tile(9, 24));
        assert_eq!(w.way.bare.tiles, home_tiles + 9);
        assert_eq!(w.way.bare.cost, home_cost + 9 * u32::from(rate));
        assert_eq!(
            w.way.paved.cost,
            home_cost + 8 * u32::from(costs.road) + u32::from(rate)
        );
        // The road tiles: the path off the edge and the mining tile.
        assert_eq!(w.path.len(), 8);
        assert_eq!(w.road.tiles(), 8);
        assert_eq!(w.road_new, w.road);
        let swampy = rate == costs.swamp;
        assert_eq!(w.road.swamp, if swampy { 8 } else { 0 });
        assert_eq!(w.road.plain, if swampy { 0 } else { 8 });
        assert!(w.path.iter().all(|t| !t.is_edge() && *t != w.tile));
        // Standing roads are not new road tiles.
        let half: Vec<Tile> = w.path.iter().copied().take(4).collect();
        let (ways, _) = search
            .remote(&remote, &terrain, costs, &[source], None, &[], &half)
            .expect("ways");
        assert_eq!(ways[0].road_new.tiles(), 4);
    }
}

#[test]
fn ways_take_store_tiles_and_never_share_a_mining_tile() {
    let costs = profile(&MOVEMENT_MMO);
    let spawn = tile(25, 25);
    let (home, _) = grid(&raw(plain), &[(spawn, Footprint::Blocked)]);
    let (a, b) = (tile(10, 25), tile(10, 23));
    let (remote, terrain) = grid(
        &raw(plain),
        &[(a, Footprint::Blocked), (b, Footprint::Blocked)],
    );
    let mut search = WaySearch::default();
    search.home(&home, spawn, Side::Right);
    // A container beside the first source, farther than its nearest neighbor, still wins.
    let store = tile(11, 26);
    let (ways, _) = search
        .remote(&remote, &terrain, costs, &[a, b], None, &[store], &[])
        .expect("ways");
    assert_eq!(ways[0].tile, store);
    assert_ne!(ways[0].tile, ways[1].tile);
    // (9, 24) lies next to both sources: only one may take it.
    let (ways, _) = search
        .remote(&remote, &terrain, costs, &[a, b], None, &[], &[])
        .expect("ways");
    assert_eq!(ways[0].tile, tile(9, 24));
    assert_ne!(ways[1].tile, tile(9, 24));
    // A walled-in source has no way.
    let walls: Vec<(Tile, Footprint)> = a
        .neighbors()
        .map(|(_, n)| (n, Footprint::Blocked))
        .chain(std::iter::once((a, Footprint::Blocked)))
        .collect();
    let (shut, terrain) = grid(&raw(plain), &walls);
    search.home(&home, spawn, Side::Right);
    assert!(
        search
            .remote(&shut, &terrain, costs, &[a], None, &[], &[])
            .is_none()
    );
}

// A two-room world for the manager: an owned home with one spawn, and a remote room to its right.

const HOME_SPAWN: (u8, u8) = (25, 25);
const SOURCES: [(u8, u8); 2] = [(10, 25), (30, 40)];
const CONTROLLER: (u8, u8) = (25, 10);

struct Creep {
    key: u128,
    id: Identity,
    at: Position,
    energy: u32,
    capacity: u32,
    /// Ticks to live from the world's time 0.
    death: u32,
}

struct Object {
    key: u128,
    kind: ObjectKind,
    at: Position,
    amount: u32,
    total: u32,
    hits: u32,
}

struct World {
    time: u32,
    home: RoomName,
    remote: RoomName,
    reservation: Reservation,
    visible: bool,
    hostile_structures: u32,
    creeps: Vec<Creep>,
    objects: Vec<Object>,
    snap: Snapshot,
    movement: Movement,
    spawner: Spawner,
    buffer: IntentBuffer,
    remotes: Remotes,
    cfg: RemoteConfig,
}

impl World {
    fn new() -> Self {
        let home = room("E2N2");
        let remote = room("E3N2");
        let mut objects = Vec::new();
        for (i, &(x, y)) in SOURCES.iter().enumerate() {
            objects.push(Object {
                key: 100 + i as u128,
                kind: ObjectKind::Source,
                at: pos(remote, tile(x, y)),
                amount: 3000,
                total: 3000,
                hits: 0,
            });
        }
        objects.push(Object {
            key: 200,
            kind: ObjectKind::Controller,
            at: pos(remote, tile(CONTROLLER.0, CONTROLLER.1)),
            amount: 0,
            total: 0,
            hits: 0,
        });
        // A worker of the home's own, so movement tracks the home room (as a live room always is).
        let worker = Creep {
            key: 999,
            id: Identity {
                role: Role::Worker,
                home,
                remote: None,
                serial: 999,
            },
            at: pos(home, tile(20, 30)),
            energy: 0,
            capacity: 50,
            death: 100_000,
        };
        Self {
            time: 1000,
            home,
            remote,
            reservation: Reservation::None,
            visible: true,
            hostile_structures: 0,
            creeps: vec![worker],
            objects,
            snap: Snapshot::default(),
            movement: Movement::default(),
            spawner: Spawner::default(),
            buffer: IntentBuffer::default(),
            remotes: Remotes::default(),
            cfg: REMOTE_MMO,
        }
    }

    fn add(&mut self, role: Role, at: Position, energy: u32, capacity: u32) -> u128 {
        let key = 1000 + self.creeps.len() as u128;
        self.creeps.push(Creep {
            key,
            id: Identity {
                role,
                home: self.home,
                remote: Some(self.remote),
                serial: key as u64,
            },
            at,
            energy,
            capacity,
            death: self.time + 1000,
        });
        key
    }

    fn take_snapshot(&mut self) {
        self.snap
            .begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
        for (row, c) in self.creeps.iter().enumerate() {
            let r = CreepRow {
                pos: c.at,
                my: true,
                spawning: false,
                fatigue: 0,
            };
            let id = c.id;
            self.snap
                .creeps
                .push_named(ObjectKey(c.key), r, || Some(id))
                .expect("creep");
            self.snap.creeps.set_store(
                row,
                CreepStore {
                    energy: c.energy,
                    capacity: c.capacity,
                },
            );
            self.snap.creeps.set_death(row, c.death);
        }
        let row = self
            .snap
            .rooms
            .push(self.home, Controller::Mine { level: 5 });
        self.snap.rooms.set_energy(
            row,
            RoomEnergy {
                available: 1800,
                capacity: 1800,
            },
        );
        let spawn = pos(self.home, tile(HOME_SPAWN.0, HOME_SPAWN.1));
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
        if self.visible {
            let row = self.snap.rooms.push(self.remote, Controller::Neutral);
            self.snap.rooms.set_remote(
                row,
                RemoteRead {
                    read: true,
                    reservation: self.reservation,
                    hostile_structures: self.hostile_structures,
                    roads: true,
                },
            );
            for o in &self.objects {
                let (kind, at) = (o.kind, o.at);
                self.snap
                    .objects
                    .push(
                        ObjectKey(o.key),
                        || Ok(ObjectStatic { kind, pos: at }),
                        ObjectRow {
                            amount: o.amount,
                            total: o.total,
                            hits: o.hits,
                        },
                    )
                    .expect("object");
            }
        }
        self.snap.finish();
    }

    /// One tick up to the remote jobs: the snapshot, movement's reads, the buffer, the spawner and
    /// `Remotes::begin`.
    fn begin(&mut self) {
        self.take_snapshot();
        let mcfg = MOVEMENT_MMO;
        self.movement.begin(&self.snap, &mcfg);
        let mut rooms = Vec::new();
        self.movement.first_reads(&mcfg, &mut rooms);
        let mut again = Vec::new();
        self.movement.rescans(&mcfg, &mut again);
        rooms.extend(again);
        let terrain = raw(plain);
        for &r in &rooms {
            assert!(self.movement.set_terrain(r, &terrain));
            if r == self.remote {
                let statics: Vec<Tile> = SOURCES
                    .iter()
                    .map(|&(x, y)| tile(x, y))
                    .chain([tile(CONTROLLER.0, CONTROLLER.1)])
                    .collect();
                self.movement.set_statics(r, statics);
                self.movement.set_objects(r, []);
            } else {
                self.movement
                    .set_objects(r, [(tile(HOME_SPAWN.0, HOME_SPAWN.1), Footprint::Blocked)]);
            }
        }
        self.buffer.begin(&self.snap);
        self.spawner.begin(&self.snap, &SPAWN_MMO);
        self.remotes.begin(&self.snap);
    }

    fn plan(&mut self, hauled: bool) {
        self.plan_with(hauled, true);
    }

    fn plan_with(&mut self, hauled: bool, spare: bool) {
        let mut commit = Commit::default();
        for unit in 0..self.remotes.home_count() {
            self.remotes
                .plan_home(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.spawner,
                    &self.cfg,
                    &HAULING_MMO,
                    &SPAWN_MMO,
                    &MOVEMENT_MMO,
                    hauled,
                    spare,
                    &mut commit,
                )
                .expect("plan");
        }
    }

    fn place(&mut self) {
        let mut commit = Commit::default();
        for unit in 0..self.remotes.home_count() {
            self.remotes
                .place_home(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &self.cfg,
                    &MOVEMENT_MMO,
                    &mut commit,
                )
                .expect("place");
        }
    }

    fn work(&mut self) {
        let mut commit = Commit::default();
        for unit in 0..self.remotes.creep_count() {
            self.remotes
                .work_creep(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.buffer,
                    &self.cfg,
                    &MOVEMENT_MMO,
                    &mut commit,
                )
                .expect("work");
        }
        self.buffer.resolve();
    }

    /// Ticks until the remote room is priced: a scout stands in it (so movement reads it), the
    /// placement searches the ways, and the next plan prices them.
    fn settle(&mut self) {
        if !self.creeps.iter().any(|c| c.id.role == Role::Scout) {
            let at = pos(self.remote, tile(25, 25));
            self.add(Role::Scout, at, 0, 0);
        }
        for _ in 0..4 {
            self.begin();
            self.plan(true);
            self.work();
            self.place();
            self.time += 1;
        }
    }

    fn requests(&self) -> Vec<(Role, Option<RoomName>, Body, u8)> {
        (0..self.spawner.stats().requests)
            .filter_map(|i| self.spawner.request_at(i))
            .map(|r| {
                let body = match r.body {
                    BodySpec::Exact(b) => b,
                    BodySpec::Scaled { .. } => panic!("remote bodies are exact"),
                };
                (r.role, r.remote, body, r.priority)
            })
            .collect()
    }

    fn intents(&self) -> Vec<(u128, CreepIntent)> {
        self.buffer
            .planned()
            .filter(|i| !matches!(i.intent, CreepIntent::Move { .. }))
            .map(|i| (self.creeps[i.actor as usize].key, i.intent))
            .collect()
    }
}

/// The crew the model sizes for the world's remote room under the plan the manager chose.
fn expected_crew(w: &World) -> super::remote::RemoteCrew {
    let (_, remotes) = w
        .remotes
        .home_states()
        .find(|(h, _)| *h == w.home)
        .expect("home");
    let r = remotes.iter().find(|r| r.room == w.remote).expect("remote");
    let ways = r.ways.as_ref().expect("ways");
    let sources: Vec<RemoteSource> = ways
        .sources
        .iter()
        .map(|s| RemoteSource {
            way: s.way,
            road: s.road,
            road_new: s.road_new,
            container: false,
        })
        .collect();
    let room = RemoteRoom {
        sources: &sources,
        controller: ways.controller.map(|c| c.1),
    };
    let home = RemoteHome {
        energy: 1800,
        unload: rustedscreep_consts::extension_energy_capacity(5),
        margin_pct: HAULING_MMO.margin_pct,
    };
    let plan = r.profit.expect("priced").plan;
    remote_crew(&room, plan, home).expect("crew")
}

#[test]
fn a_priced_room_gets_its_crew_requested() {
    let mut w = World::new();
    w.settle();
    w.begin();
    w.plan(true);
    let (_, remotes) = w.remotes.home_states().next().expect("home");
    let r = remotes.iter().find(|r| r.room == w.remote).expect("remote");
    assert!(r.active, "the only minable candidate is mined");
    let crew = expected_crew(&w);
    let asks = w.requests();
    // A reserver (no reservation yet), one miner a source, and each source's haulers one a tick.
    let reserver = crew.reserver.expect("reserve pays at RCL5").0;
    assert!(crew.plan.reserve);
    assert!(asks.contains(&(Role::Reserver, Some(w.remote), reserver, PRIORITY_RESERVER)));
    for s in &crew.sources {
        assert!(asks.contains(&(
            Role::RemoteMiner,
            Some(w.remote),
            s.miner,
            PRIORITY_REMOTE_MINER
        )));
        assert!(asks.contains(&(
            Role::RemoteHauler,
            Some(w.remote),
            s.hauler,
            PRIORITY_REMOTE_HAULER
        )));
    }
    let miners = asks.iter().filter(|a| a.0 == Role::RemoteMiner).count();
    assert_eq!(miners, SOURCES.len());
    // Below every home request (the opening's normal workers are at 100).
    assert!(asks.iter().all(|a| a.3 < super::PRIORITY_NORMAL));
}

#[test]
fn staffed_posts_ask_for_nothing_until_due() {
    let mut w = World::new();
    w.settle();
    w.begin();
    w.plan(true);
    let crew = expected_crew(&w);
    // Staff every post: a reserver, a miner on each mining tile, and each source's haulers.
    let at = pos(w.remote, tile(20, 20));
    w.add(Role::Reserver, at, 0, 0);
    w.reservation = Reservation::Mine { ticks: 4000 };
    let (_, remotes) = w.remotes.home_states().next().expect("home");
    let ways = remotes[1..]
        .iter()
        .chain(&remotes[..1])
        .find(|r| r.room == w.remote)
        .and_then(|r| r.ways.clone())
        .expect("ways");
    for (s, c) in ways.sources.iter().zip(&crew.sources) {
        let on = pos(w.remote, s.tile);
        w.add(Role::RemoteMiner, on, 0, 50);
        for _ in 0..c.haulers {
            w.add(Role::RemoteHauler, at, 0, 100);
        }
    }
    w.time += 1;
    w.begin();
    w.plan(true);
    let asks = w.requests();
    assert!(
        asks.iter()
            .all(|a| a.0 == Role::Scout || a.1 != Some(w.remote)),
        "{asks:?}"
    );
    // A miner near its end draws its successor (P5.2).
    let miner = w
        .creeps
        .iter_mut()
        .find(|c| c.id.role == Role::RemoteMiner)
        .expect("miner");
    miner.death = w.time + 2;
    w.time += 1;
    w.begin();
    w.plan(true);
    let miners = w
        .requests()
        .iter()
        .filter(|a| a.0 == Role::RemoteMiner)
        .count();
    assert_eq!(miners, 1);
}

#[test]
fn rooms_not_minable_or_over_the_spawn_share_are_not_mined() {
    // Reserved by another player.
    let mut w = World::new();
    w.reservation = Reservation::Other { ticks: 3000 };
    w.settle();
    w.begin();
    w.plan(true);
    assert!(w.requests().iter().all(|a| a.0 == Role::Scout));
    // A hostile structure (an invader core).
    let mut w = World::new();
    w.hostile_structures = 1;
    w.settle();
    w.begin();
    w.plan(true);
    assert!(w.requests().iter().all(|a| a.0 == Role::Scout));
    // No spawn time to spare.
    let mut w = World::new();
    w.cfg.spawn_permille = 0;
    w.settle();
    w.begin();
    w.plan(true);
    assert!(w.requests().iter().all(|a| a.0 == Role::Scout));
    // No home hauling to deliver what the haulers bring.
    let mut w = World::new();
    w.settle();
    w.begin();
    w.plan(false);
    assert!(w.requests().is_empty());
}

#[test]
fn a_scout_is_ordered_for_an_unseen_candidate() {
    let mut w = World::new();
    w.visible = false;
    w.begin();
    w.plan(true);
    let asks = w.requests();
    assert_eq!(asks.len(), 1, "{asks:?}");
    let (role, remote, body, priority) = asks[0];
    assert_eq!(role, Role::Scout);
    assert_eq!(priority, PRIORITY_SCOUT);
    assert_eq!(body.parts(), [Part::Move]);
    // The first candidate in side order: the room above.
    assert_eq!(remote, Some(room("E2N3")));
    // With a scout alive, none more.
    let at = pos(w.home, tile(20, 20));
    w.add(Role::Scout, at, 0, 0);
    w.time += 1;
    w.begin();
    w.plan(true);
    assert!(w.requests().is_empty());
}

#[test]
fn a_reserver_stops_short_of_the_cap() {
    let mut w = World::new();
    w.settle();
    w.begin();
    w.plan(true);
    let crew = expected_crew(&w);
    let claim = crew.reserver.expect("reserver").0.count(Part::Claim) as u32;
    let (_, remotes) = w.remotes.home_states().next().expect("home");
    let next_to = remotes
        .iter()
        .find(|r| r.room == w.remote)
        .and_then(|r| r.ways.as_ref())
        .and_then(|w| w.controller)
        .expect("controller way")
        .0;
    let key = w.add(Role::Reserver, pos(w.remote, next_to), 0, 0);
    let controller_key = 200;
    for (ticks, reserves) in [
        (0, true),
        (CONTROLLER_RESERVE_MAX - claim * CONTROLLER_RESERVE, true),
        (
            CONTROLLER_RESERVE_MAX - claim * CONTROLLER_RESERVE + 1,
            false,
        ),
    ] {
        w.objects
            .iter_mut()
            .find(|o| o.key == controller_key)
            .expect("controller")
            .amount = ticks;
        w.reservation = Reservation::Mine { ticks };
        w.time += 1;
        w.begin();
        w.plan(true);
        w.work();
        let reserved = w
            .intents()
            .iter()
            .any(|&(k, i)| k == key && matches!(i, CreepIntent::ReserveController { .. }));
        assert_eq!(reserved, reserves, "ticks to end {ticks}");
    }
}

#[test]
fn a_remote_miner_builds_repairs_and_harvests() {
    let mut w = World::new();
    w.settle();
    w.begin();
    w.plan(true);
    let crew = expected_crew(&w);
    let work = crew.sources[0].work;
    let (_, remotes) = w.remotes.home_states().next().expect("home");
    let mining = remotes
        .iter()
        .find(|r| r.room == w.remote)
        .and_then(|r| r.ways.as_ref())
        .map(|w| w.sources[0].tile)
        .expect("tile");
    let key = w.add(Role::RemoteMiner, pos(w.remote, mining), 0, 50);
    let at = pos(w.remote, mining);
    // A container site on the tile: harvest until the store holds a build, then build.
    w.objects.push(Object {
        key: 300,
        kind: ObjectKind::Site(StructureType::Container),
        at,
        amount: 0,
        total: 5000,
        hits: 0,
    });
    let step = |w: &mut World, energy: u32| {
        w.creeps
            .iter_mut()
            .find(|c| c.key == key)
            .expect("miner")
            .energy = energy;
        w.time += 1;
        w.begin();
        w.plan(true);
        w.work();
        w.intents()
            .into_iter()
            .filter(|&(k, _)| k == key)
            .map(|(_, i)| i)
            .collect::<Vec<_>>()
    };
    let build = (work * BUILD_POWER).min(50);
    assert!(matches!(
        step(&mut w, build - 1)[..],
        [CreepIntent::Harvest { .. }]
    ));
    assert!(matches!(
        step(&mut w, build)[..],
        [CreepIntent::Build { .. }]
    ));
    // The container stands, worn below the repair share: repair from the store's energy.
    w.objects.retain(|o| o.key != 300);
    let floor = CONTAINER_HITS / 100 * w.cfg.container_repair_pct;
    w.objects.push(Object {
        key: 301,
        kind: ObjectKind::Container,
        at,
        amount: 0,
        total: 0,
        hits: floor - 1,
    });
    assert!(matches!(
        step(&mut w, work)[..],
        [CreepIntent::RepairObject { .. }]
    ));
    // At the share, or with less than a repair's energy: harvest.
    assert!(matches!(
        step(&mut w, work - 1)[..],
        [CreepIntent::Harvest { .. }]
    ));
    w.objects.iter_mut().find(|o| o.key == 301).expect("c").hits = floor;
    assert!(matches!(
        step(&mut w, 50)[..],
        [CreepIntent::Harvest { .. }]
    ));
}

#[test]
fn a_remote_hauler_takes_whole_loads_and_works_the_road_home() {
    let mut w = World::new();
    w.settle();
    w.begin();
    w.plan(true);
    let (_, remotes) = w.remotes.home_states().next().expect("home");
    let way = remotes
        .iter()
        .find(|r| r.room == w.remote)
        .and_then(|r| r.ways.as_ref())
        .map(|w| w.sources[0].clone())
        .expect("way");
    let mining = pos(w.remote, way.tile);
    let next_to = pos(w.remote, way.path[0]);
    let key = w.add(Role::RemoteHauler, next_to, 0, 100);
    let step = |w: &mut World| {
        w.time += 1;
        w.begin();
        w.plan(true);
        w.work();
        w.intents()
            .into_iter()
            .filter(|&(k, _)| k == key)
            .map(|(_, i)| i)
            .collect::<Vec<_>>()
    };
    // Less than a load at the tile: it waits.
    w.objects.push(Object {
        key: 400,
        kind: ObjectKind::Container,
        at: mining,
        amount: 99,
        total: 0,
        hits: CONTAINER_HITS,
    });
    assert!(step(&mut w).is_empty());
    // A pile and the container make a load: the pile first.
    w.objects.push(Object {
        key: 401,
        kind: ObjectKind::Energy,
        at: mining,
        amount: 10,
        total: 0,
        hits: 0,
    });
    assert!(matches!(step(&mut w)[..], [CreepIntent::Pickup { .. }]));
    // A whole load in the container: withdraw exactly its free room.
    w.objects.retain(|o| o.key != 401);
    w.objects
        .iter_mut()
        .find(|o| o.key == 400)
        .expect("c")
        .amount = 500;
    assert!(matches!(
        step(&mut w)[..],
        [CreepIntent::WithdrawObject {
            amount: Some(100),
            ..
        }]
    ));
    // Loaded, on its way home: no take. Its road work needs a WORK part, which only a roads plan
    // gives.
    w.creeps
        .iter_mut()
        .find(|c| c.key == key)
        .expect("h")
        .energy = 100;
    let crew = expected_crew(&w);
    let work = crew.sources[0].hauler.count(Part::Work) as u32;
    w.objects.push(Object {
        key: 402,
        kind: ObjectKind::Site(StructureType::Road),
        at: pos(w.remote, way.path[1]),
        amount: 0,
        total: 300,
        hits: 0,
    });
    let loaded = step(&mut w);
    if work > 0 {
        assert!(matches!(loaded[..], [CreepIntent::Build { .. }]));
        // With no site, the road in range that lacks a whole repair.
        w.objects.retain(|o| o.key != 402);
        w.objects.push(Object {
            key: 403,
            kind: ObjectKind::Road,
            at: pos(w.remote, way.path[1]),
            amount: 0,
            total: 5000,
            hits: 5000 - work * REPAIR_POWER,
        });
        assert!(matches!(
            step(&mut w)[..],
            [CreepIntent::RepairObject { .. }]
        ));
        w.objects.iter_mut().find(|o| o.key == 403).expect("r").hits = 5000 - 1;
        assert!(step(&mut w).is_empty());
    } else {
        assert!(loaded.is_empty());
    }
}

#[test]
fn nothing_is_requested_while_the_standard_tier_is_off() {
    let mut w = World::new();
    w.settle();
    w.begin();
    w.plan_with(true, false);
    assert!(w.requests().is_empty());
    // The room stays chosen, and asks again once the tier runs.
    w.time += 1;
    w.begin();
    w.plan(true);
    assert!(w.requests().iter().any(|a| a.0 == Role::RemoteMiner));
}

/// After a global reset a room's creeps are back before its ways: the Standard job searches them
/// again for a room with creeps, and a creep in its room holds with a request meanwhile (so
/// movement keeps reading the room for the search).
#[test]
fn a_reset_searches_a_crewed_room_again() {
    let mut w = World::new();
    w.settle();
    w.begin();
    w.plan(true);
    let at = pos(w.remote, tile(20, 20));
    let key = w.add(Role::RemoteMiner, at, 0, 50);
    // A fresh global: the manager's and movement's heap are gone.
    w.remotes = Remotes::default();
    w.movement = Movement::default();
    w.time += 1;
    w.begin();
    w.plan(true);
    w.work();
    assert!(
        w.intents().iter().all(|&(k, _)| k != key),
        "it holds, no intent"
    );
    // Movement has read the room by the next tick, and the plan searches it then.
    for _ in 0..3 {
        w.time += 1;
        w.begin();
        w.plan(true);
        w.work();
    }
    let (_, remotes) = w.remotes.home_states().next().expect("home");
    let r = remotes.iter().find(|r| r.room == w.remote).expect("remote");
    assert!(r.ways.is_some(), "searched without the placement job");
    assert!(r.active);
}
