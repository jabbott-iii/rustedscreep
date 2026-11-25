//! Tests of the claimer and the first spawn site (P8.3, ADR-042, `expansion::claimer`). Expected
//! values derive from the constants (`MOVE_COST_SWAMP`, `MOVE_POWER`, `BODYPART_COST`,
//! `CREEP_CLAIM_LIFE_TIME`), `placement::APPEAR_TICKS`, and the claim config.

use std::cell::RefCell;
use std::rc::Rc;

use rustedscreep_consts::{
    CREEP_CLAIM_LIFE_TIME, MOVE_COST_SWAMP, MOVE_POWER, Part, Position, ROOM_AREA, ROOM_SIZE,
    RoomCoordinate, RoomName, StructureType,
};

use super::*;
use crate::config::{CLAIM_MMO, ClaimConfig, MOVEMENT_MMO, SCOUT_MMO, SPAWN_MMO, ScoutConfig};
use crate::economy::placement::APPEAR_TICKS;
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{
    Movement, RouteOutcome, RouteQuery, RouteRooms, RouteSearch, ShoveCaps, Tile,
};
use crate::scheduler::Commit;
use crate::snapshot::{
    Controller, CreepRow, ObjectKey, ObjectKind, ObjectRow, ObjectStatic, RoomEnergy, Snapshot,
    StructureStatic, TickContext,
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

const HOME: &str = "E2N2";
const TARGET: &str = "E3N2";
const HOME_SPAWN: (u8, u8) = (25, 25);
const CONTROLLER: (u8, u8) = (20, 20);

// ---- The world ----

/// A route search over open plains in a straight line (the scouting tests' router), with rooms in
/// `fail` refused, and every query recorded with the rooms it was told to avoid. With `long`,
/// every found path is that many steps.
#[derive(Default)]
struct Router {
    fail: Vec<RoomName>,
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
    home: RoomName,
    target: RoomName,
    at: Position,
    death: u32,
    spawning: bool,
}

struct World {
    time: u32,
    /// Owned rooms with a spawn, with their level.
    homes: Vec<(RoomName, u8)>,
    /// Owned rooms with no spawn: their level and whether an own spawn site stands.
    claimed: Vec<(RoomName, u8, bool)>,
    /// A neutral room the claimers can see this tick (its controller is pushed as an object).
    visible: Vec<RoomName>,
    claimers: Vec<Creep>,
    /// Rooms with terrain of their own (row-major, `TERRAIN_MASK_WALL` for walls); plain otherwise.
    terrain: Vec<(RoomName, Vec<u8>)>,
    snap: Snapshot,
    movement: Movement,
    spawner: Spawner,
    buffer: IntentBuffer,
    atlas: Atlas,
    claims: Claims,
    claiming: Claiming,
    router: Rc<RefCell<Router>>,
    cfg: ClaimConfig,
    help_range: u32,
}

/// The atlas's neutral read of `name` at `seen`, with its controller at [`CONTROLLER`].
fn neutral(name: RoomName, seen: u32) -> RoomIntel {
    let mut e = RoomIntel::new(name, seen);
    e.known = true;
    e.controller = Controller::Neutral;
    e.controller_tile = Some(tile(CONTROLLER.0, CONTROLLER.1));
    e.sources[0] = Some(tile(5, 5));
    e.sources[1] = Some(tile(40, 40));
    e
}

fn fit() -> Fit {
    Fit {
        spawn: tile(24, 30),
        ramparts: 10,
        source_ways: 20,
        controller_way: 10,
        open: [true; 4],
    }
}

impl World {
    fn new() -> Self {
        let router = Rc::new(RefCell::new(Router::default()));
        let mut movement = Movement::default();
        movement.set_router(Box::new(Shared(Rc::clone(&router))));
        let time = 100_000;
        let mut atlas = Atlas::default();
        atlas.put(neutral(room(TARGET), time - 10));
        let mut claims = Claims::default();
        claims.rank_for_test(vec![Candidate {
            room: room(TARGET),
            home: room(HOME),
            distance: 1,
            sources: 2,
            terms: Terms::default(),
            plan: Some(fit()),
        }]);
        Self {
            time,
            homes: vec![(room(HOME), 5)],
            claimed: Vec::new(),
            visible: Vec::new(),
            claimers: Vec::new(),
            terrain: Vec::new(),
            snap: Snapshot::default(),
            movement,
            spawner: Spawner::default(),
            buffer: IntentBuffer::default(),
            atlas,
            claims,
            claiming: Claiming::default(),
            router,
            cfg: CLAIM_MMO,
            help_range: SPAWN_MMO.help_range,
        }
    }

    fn add_claimer(&mut self, at: Position) -> u128 {
        let key = 700 + self.claimers.len() as u128;
        self.claimers.push(Creep {
            key,
            home: room(HOME),
            target: room(TARGET),
            at,
            death: self.time + CREEP_CLAIM_LIFE_TIME,
            spawning: false,
        });
        key
    }

    fn take_snapshot(&mut self) {
        self.snap
            .begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
        for (row, c) in self.claimers.iter().enumerate() {
            let r = CreepRow {
                pos: c.at,
                my: true,
                spawning: c.spawning,
                fatigue: 0,
            };
            let id = Identity {
                role: Role::Claimer,
                home: c.home,
                remote: Some(c.target),
                serial: c.key as u64,
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
                    available: 1800,
                    capacity: 1800,
                },
            );
            let h = self
                .snap
                .structures
                .push(ObjectKey(1 + i as u128), || {
                    Ok(StructureStatic {
                        kind: StructureType::Spawn,
                        pos: pos(home, HOME_SPAWN.0, HOME_SPAWN.1),
                    })
                })
                .expect("spawn");
            self.snap.spawns.push(h, 0, false);
        }
        for (i, &(r, level, site)) in self.claimed.iter().enumerate() {
            self.snap.rooms.push(r, Controller::Mine { level });
            if site {
                self.snap
                    .objects
                    .push(
                        ObjectKey(300 + i as u128),
                        || {
                            Ok(ObjectStatic {
                                kind: ObjectKind::Site(StructureType::Spawn),
                                pos: pos(r, 24, 30),
                            })
                        },
                        ObjectRow::default(),
                    )
                    .expect("site");
            }
        }
        for (i, &r) in self.visible.iter().enumerate() {
            self.snap.rooms.push(r, Controller::Neutral);
            self.snap
                .objects
                .push(
                    ObjectKey(900 + i as u128),
                    || {
                        Ok(ObjectStatic {
                            kind: ObjectKind::Controller,
                            pos: pos(r, CONTROLLER.0, CONTROLLER.1),
                        })
                    },
                    ObjectRow::default(),
                )
                .expect("controller");
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
            let t = self
                .terrain
                .iter()
                .find(|t| t.0 == r)
                .map_or(&plain, |t| &t.1);
            assert!(self.movement.set_terrain(r, t));
            self.movement.set_objects(r, []);
        }
        self.buffer.begin(&self.snap);
        self.spawner.begin(&self.snap, &SPAWN_MMO);
        self.claiming.begin(&self.snap, &self.cfg);
    }

    /// The Critical request, the Standard work and the Optional site pick, as the glue runs them.
    fn work(&mut self) {
        let mut commit = Commit::default();
        self.claiming.request(
            &mut self.spawner,
            &self.claims,
            &self.cfg,
            self.help_range,
            true,
        );
        self.claiming
            .work(
                &self.snap,
                &self.atlas,
                &mut self.movement,
                &mut self.buffer,
                &self.cfg,
                &SCOUT_MMO,
                &MOVEMENT_MMO,
                &mut commit,
            )
            .expect("work");
        self.claiming.pick_sites(&self.claims, &self.cfg);
        let caps = ShoveCaps::from_cpu_left([100.0; 3]);
        self.movement.resolve(
            &self.snap,
            &mut self.buffer,
            &MOVEMENT_MMO,
            caps,
            &mut commit,
        );
        self.buffer.resolve();
    }

    fn tick(&mut self) {
        self.begin();
        self.work();
        self.time += 1;
    }

    fn requests(&self) -> Vec<(Role, RoomName, Option<RoomName>, Body, u8)> {
        (0..self.spawner.stats().requests)
            .filter_map(|i| self.spawner.request_at(i))
            .map(|r| {
                let BodySpec::Exact(b) = r.body else {
                    panic!("the claimer's body is exact");
                };
                (r.role, r.home, r.remote, b, r.priority)
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

    fn failures(&self) -> Vec<(RoomName, Failure)> {
        self.claiming
            .events()
            .iter()
            .filter_map(|e| match *e {
                ClaimerEvent::Failed { room, why } => Some((room, why)),
                _ => None,
            })
            .collect()
    }

    /// The GCL read the glue makes when it is due.
    fn read_gcl(&mut self, level: u32) {
        if self
            .claiming
            .gcl_due(&self.claims, &self.cfg, self.help_range)
        {
            self.claiming.set_gcl(level);
        }
    }
}

// ---- The body ----

#[test]
fn the_claimer_walks_a_tile_a_tick_on_swamp() {
    let body = claimer_body().expect("body");
    assert_eq!(CLAIMER_MOVES, MOVE_COST_SWAMP.div_ceil(MOVE_POWER));
    assert_eq!(body.parts().first(), Some(&Part::Claim));
    let moves = body.parts().iter().filter(|&&p| p == Part::Move).count() as u32;
    assert_eq!(moves, CLAIMER_MOVES);
    assert_eq!(body.parts().len() as u32, 1 + CLAIMER_MOVES);
    // One CLAIM part's swamp fatigue is taken away within the tick.
    assert!(MOVE_POWER * moves >= MOVE_COST_SWAMP);
    assert_eq!(
        body.cost(),
        Part::Claim.cost() + CLAIMER_MOVES * Part::Move.cost()
    );
}

// ---- The order ----

#[test]
fn a_claimer_is_ordered_once_the_gcl_allows_another_room() {
    let mut w = World::new();
    w.begin();
    assert!(
        w.claiming.gcl_due(&w.claims, &w.cfg, w.help_range),
        "never read"
    );
    w.work();
    assert!(w.requests().is_empty(), "no GCL read yet");
    // GCL 1 with one room owned: no claim (V378), and no read again within `score_ticks`.
    w.read_gcl(1);
    w.work();
    assert!(w.requests().is_empty());
    assert!(!w.claiming.gcl_due(&w.claims, &w.cfg, w.help_range));
    w.time += w.cfg.score_ticks;
    w.begin();
    assert!(w.claiming.gcl_due(&w.claims, &w.cfg, w.help_range));
    w.read_gcl(2);
    w.work();
    let body = claimer_body().expect("body");
    assert_eq!(
        w.requests(),
        vec![(
            Role::Claimer,
            room(HOME),
            Some(room(TARGET)),
            body,
            PRIORITY_CLAIMER
        )]
    );
}

#[test]
fn no_claimer_goes_while_one_lives_a_claim_waits_for_its_spawn_or_it_is_off() {
    let check = |setup: &dyn Fn(&mut World), optional: bool, want: usize, why: &str| {
        let mut w = World::new();
        setup(&mut w);
        w.begin();
        w.claiming.set_gcl(5);
        w.claiming
            .request(&mut w.spawner, &w.claims, &w.cfg, w.help_range, optional);
        assert_eq!(w.requests().len(), want, "{why}");
    };
    check(&|_| {}, true, 1, "the base case orders one");
    check(&|_| {}, false, 0, "only on a tick the Optional tier runs");
    check(
        &|w| w.claimed.push((room("E1N2"), 1, false)),
        true,
        0,
        "a claimed room has no spawn yet",
    );
    check(
        &|w| w.claimed.push((room("E1N2"), 1, true)),
        true,
        0,
        "a spawn site is not a spawn",
    );
    check(
        &|w| {
            let at = pos(room(HOME), 30, 30);
            w.add_claimer(at);
        },
        true,
        0,
        "a claimer lives",
    );
    check(&|w| w.cfg.claimers = false, true, 0, "claimers off");
    check(&|w| w.cfg.enabled = false, true, 0, "scoring off");
    check(
        &|w| w.homes[0].1 = (w.cfg.min_home_level - 1) as u8,
        true,
        0,
        "the home is below claim.min_home_level",
    );
    check(
        &|w| w.help_range = 0,
        true,
        0,
        "no spawn room within help range",
    );
    check(
        &|w| w.claims = Claims::default(),
        true,
        0,
        "no best candidate",
    );
}

// ---- The walk and the claim ----

#[test]
fn the_claimer_searches_once_walks_and_claims_then_ends() {
    let mut w = World::new();
    w.add_claimer(pos(room(HOME), 26, 25));
    w.tick();
    assert_eq!(w.router.borrow().queries.len(), 1, "one search");
    assert_eq!(w.moves(), 1);
    assert!(w.intents().is_empty());
    w.tick();
    assert_eq!(w.router.borrow().queries.len(), 1, "the route is kept");
    // Next to the controller, in the room: one claimController a tick, at the controller.
    w.claimers[0].at = pos(room(TARGET), CONTROLLER.0 + 1, CONTROLLER.1);
    w.visible.push(room(TARGET));
    w.begin();
    w.work();
    let controller = w
        .snap
        .objects
        .kinds()
        .iter()
        .zip(w.snap.objects.handles())
        .find(|(k, _)| **k == ObjectKind::Controller)
        .map(|(_, &h)| h)
        .expect("controller");
    assert_eq!(
        w.intents(),
        vec![CreepIntent::ClaimController { target: controller }]
    );
    assert_eq!(w.moves(), 0);
    w.time += 1;
    // The claim held: the room is owned (and has no spawn).
    w.visible.clear();
    w.claimed.push((room(TARGET), 1, false));
    w.begin();
    assert!(
        w.claiming
            .events()
            .contains(&ClaimerEvent::Claimed { room: room(TARGET) })
    );
    w.work();
    assert_eq!(w.intents(), vec![CreepIntent::Suicide]);
    assert_eq!(w.claiming.stats().claims, 1);
    assert!(w.failures().is_empty());
    w.time += 1;
    // Gone after its suicide: no loss.
    w.claimers.clear();
    w.tick();
    assert!(w.failures().is_empty());
    assert!(w.claiming.failed().is_empty());
}

#[test]
fn a_refused_claim_expires_the_entry_waits_for_the_read_and_holds_the_room_back() {
    let mut w = World::new();
    w.add_claimer(pos(room(TARGET), CONTROLLER.0 + 1, CONTROLLER.1 + 1));
    w.visible.push(room(TARGET));
    w.tick();
    assert!(
        w.intents()
            .iter()
            .any(|i| matches!(i, CreepIntent::ClaimController { .. }))
    );
    // Next tick the room is still not owned: refused.
    let refused_at = w.time;
    w.begin();
    assert_eq!(w.failures(), vec![(room(TARGET), Failure::Refused)]);
    assert_eq!(w.claiming.expired(), &[room(TARGET)]);
    assert_eq!(
        w.claiming.failed(),
        &[(room(TARGET), refused_at + w.cfg.retry_ticks)]
    );
    w.work();
    assert!(w.intents().is_empty(), "it waits for the read");
    w.time += 1;
    // The atlas reads the room again: it ends.
    w.atlas.put(neutral(room(TARGET), w.time));
    w.begin();
    assert!(w.claiming.expired().is_empty(), "expired once");
    w.work();
    assert_eq!(w.intents(), vec![CreepIntent::Suicide]);
    w.time += 1;
    // Gone, and the room waits `claim.retry_ticks`.
    w.claimers.clear();
    w.visible.clear();
    w.begin();
    assert!(w.failures().is_empty(), "an ended claimer is no loss");
    w.claiming.set_gcl(5);
    w.work();
    assert!(w.requests().is_empty(), "the room waits");
    w.time = refused_at + w.cfg.retry_ticks;
    w.begin();
    w.claiming.set_gcl(5);
    w.work();
    assert_eq!(w.requests().len(), 1, "and is tried again after the wait");
}

#[test]
fn a_claimer_lost_on_the_way_fails_the_claim() {
    let mut w = World::new();
    w.add_claimer(pos(room(HOME), 26, 25));
    w.tick();
    w.claimers.clear();
    w.begin();
    assert_eq!(w.failures(), vec![(room(TARGET), Failure::Lost)]);
    // One still spawning when it went is no loss.
    let mut w = World::new();
    w.add_claimer(pos(room(HOME), 25, 25));
    w.claimers[0].spawning = true;
    w.tick();
    w.claimers.clear();
    w.begin();
    assert!(w.failures().is_empty());
}

#[test]
fn routes_keep_out_of_towered_rooms_and_one_too_long_or_missing_fails() {
    let mut w = World::new();
    let mut towered = neutral(room("E3N3"), w.time - 10);
    towered.controller = Controller::Foreign { level: 5 };
    towered.towers = 2;
    w.atlas.put(towered);
    w.add_claimer(pos(room(HOME), 26, 25));
    w.tick();
    let closed = w.router.borrow().queries[0].1.clone();
    assert!(closed.contains(&room("E3N3")), "{closed:?}");
    assert!(!closed.contains(&room(TARGET)));

    // A route longer than a claimer's life, less the margin.
    let mut w = World::new();
    w.router.borrow_mut().long =
        Some((CREEP_CLAIM_LIFE_TIME - SCOUT_MMO.life_margin_ticks + 1) as usize);
    w.add_claimer(pos(room(HOME), 26, 25));
    w.tick();
    assert_eq!(w.failures(), vec![(room(TARGET), Failure::TooFar)]);
    assert_eq!(w.intents(), vec![CreepIntent::Suicide]);

    // Just within it.
    let mut w = World::new();
    w.router.borrow_mut().long =
        Some((CREEP_CLAIM_LIFE_TIME - SCOUT_MMO.life_margin_ticks) as usize);
    w.add_claimer(pos(room(HOME), 26, 25));
    w.tick();
    assert!(w.failures().is_empty());

    // No route.
    let mut w = World::new();
    w.router.borrow_mut().fail.push(room(TARGET));
    w.add_claimer(pos(room(HOME), 26, 25));
    w.tick();
    assert_eq!(w.failures(), vec![(room(TARGET), Failure::NoRoute)]);
    assert_eq!(w.intents(), vec![CreepIntent::Suicide]);
}

#[test]
fn only_one_claimer_lives_and_off_ends_it() {
    let mut w = World::new();
    w.add_claimer(pos(room(HOME), 26, 25));
    w.add_claimer(pos(room(HOME), 27, 25));
    w.tick();
    assert_eq!(
        w.intents(),
        vec![CreepIntent::Suicide],
        "the second ends at once"
    );
    let mut w = World::new();
    w.cfg.claimers = false;
    w.add_claimer(pos(room(HOME), 26, 25));
    w.tick();
    assert_eq!(w.intents(), vec![CreepIntent::Suicide]);
    assert!(w.failures().is_empty());
}

// ---- The first spawn site ----

#[test]
fn a_claimed_room_gets_one_spawn_site_at_its_plan_tile() {
    let mut w = World::new();
    w.claimed.push((room(TARGET), 1, false));
    let target = room(TARGET);
    let atlas = Atlas::default();
    let mut a = atlas;
    a.put(neutral(target, w.time - 10));
    w.claims.site_for_test(target, &a, Ok(fit()));
    w.tick();
    assert_eq!(
        w.claiming.sites(),
        &[SitePick {
            room: target,
            tile: fit().spawn
        }]
    );
    assert_eq!(SitePick::KIND, StructureType::Spawn);
    assert_eq!(
        w.claiming.site_rooms(&w.cfg).collect::<Vec<_>>(),
        vec![target]
    );
    let pick = w.claiming.sites()[0];
    w.claiming.site_placed(pick);
    // Not again while it may still appear.
    for _ in 0..APPEAR_TICKS - 1 {
        w.tick();
        assert!(w.claiming.sites().is_empty());
    }
    // It never appeared: placed again.
    w.tick();
    assert_eq!(w.claiming.sites().len(), 1);
    // Once a site stands, none.
    w.claimed[0].2 = true;
    w.tick();
    assert!(w.claiming.sites().is_empty());
    assert!(w.claiming.site_rooms(&w.cfg).next().is_none());
}

#[test]
fn a_refused_spawn_tile_is_not_tried_again_but_the_site_cap_is() {
    let target = room(TARGET);
    let setup = || {
        let mut w = World::new();
        w.claimed.push((target, 1, false));
        let mut a = Atlas::default();
        a.put(neutral(target, w.time - 10));
        w.claims.site_for_test(target, &a, Ok(fit()));
        w.tick();
        w
    };
    let mut w = setup();
    let pick = w.claiming.sites()[0];
    w.claiming.site_refused(pick, false);
    for _ in 0..=APPEAR_TICKS {
        w.tick();
        assert!(w.claiming.sites().is_empty());
    }
    let mut w = setup();
    let pick = w.claiming.sites()[0];
    w.claiming.site_refused(pick, true);
    for _ in 0..APPEAR_TICKS {
        w.tick();
    }
    assert_eq!(w.claiming.sites().len(), 1, "ERR_FULL passes");
}

#[test]
fn a_claimed_room_with_no_valid_plan_logs_once_and_places_nothing() {
    let target = room(TARGET);
    let mut w = World::new();
    w.claimed.push((target, 1, false));
    let mut a = Atlas::default();
    a.put(neutral(target, w.time - 10));
    w.claims.site_for_test(target, &a, Err("invalid plan"));
    w.tick();
    assert!(w.claiming.sites().is_empty());
    assert!(w.claiming.events().contains(&ClaimerEvent::NoSite {
        room: target,
        why: "invalid plan"
    }));
    w.tick();
    assert!(
        !w.claiming
            .events()
            .iter()
            .any(|e| matches!(e, ClaimerEvent::NoSite { .. }))
    );
}

// ---- The atlas ----

#[test]
fn expire_makes_an_entry_due_and_keeps_its_read() {
    let refresh = SCOUT_MMO.refresh_ticks;
    let now = 10 * refresh;
    let mut a = Atlas::default();
    a.put(neutral(room(TARGET), now - 1));
    assert!(!a.due(room(TARGET), now, refresh));
    assert!(a.expire(room(TARGET), now, refresh));
    assert!(a.due(room(TARGET), now, refresh));
    assert_eq!(
        a.get(room(TARGET)).map(|e| e.controller_tile),
        Some(Some(tile(CONTROLLER.0, CONTROLLER.1)))
    );
    assert!(!a.expire(room("E9N9"), now, refresh), "no entry");
}

/// A route found but not walked (movement finds no way inside the room): the claimer searches once
/// more, and the second miss fails the claim, so it never searches every tick of its life (the
/// hot-path review's high finding).
#[test]
fn a_route_found_but_not_walked_fails_on_the_second_miss() {
    let mut w = World::new();
    // The controller's neighbors walled off: a ring of walls two tiles out.
    let mut t = vec![0u8; ROOM_AREA];
    for y in CONTROLLER.1 - 2..=CONTROLLER.1 + 2 {
        for x in CONTROLLER.0 - 2..=CONTROLLER.0 + 2 {
            let ring = x.abs_diff(CONTROLLER.0).max(y.abs_diff(CONTROLLER.1)) == 2;
            if ring {
                t[usize::from(y) * 50 + usize::from(x)] = rustedscreep_consts::Terrain::Wall as u8;
            }
        }
    }
    w.terrain.push((room(TARGET), t));
    w.add_claimer(pos(room(TARGET), 35, 35));
    // The room is read after the first request there (movement's first read), then each move
    // finds no way.
    let mut failed = false;
    for _ in 0..6 {
        w.tick();
        if !w.failures().is_empty() {
            failed = true;
            break;
        }
    }
    assert!(failed, "the claim never failed");
    assert_eq!(w.router.borrow().queries.len(), 2, "two searches in all");
    assert_eq!(w.failures(), vec![(room(TARGET), Failure::NoRoute)]);
    assert_eq!(w.intents(), vec![CreepIntent::Suicide]);
    w.tick();
    assert_eq!(
        w.router.borrow().queries.len(),
        2,
        "no search after the end"
    );
}

/// After a refused claim the claimer waits for the atlas's read at most `REREAD_WAIT` ticks, not at
/// all with scouting off, and its later end is no second failure.
#[test]
fn a_refused_claimer_waits_a_bounded_time_and_fails_once() {
    let refused = |scout: ScoutConfig| {
        let mut w = World::new();
        w.add_claimer(pos(room(TARGET), CONTROLLER.0 + 1, CONTROLLER.1 + 1));
        w.visible.push(room(TARGET));
        w.tick();
        let at = w.time;
        let mut ended = None;
        for _ in 0..=REREAD_WAIT {
            w.begin();
            let mut commit = Commit::default();
            w.claiming
                .work(
                    &w.snap,
                    &w.atlas,
                    &mut w.movement,
                    &mut w.buffer,
                    &w.cfg,
                    &scout,
                    &MOVEMENT_MMO,
                    &mut commit,
                )
                .expect("work");
            w.buffer.resolve();
            if ended.is_none() && w.intents() == vec![CreepIntent::Suicide] {
                ended = Some(w.time - at);
            }
            w.time += 1;
        }
        // Gone after its end, with no second failure.
        w.claimers.clear();
        w.begin();
        assert!(w.failures().is_empty(), "the refusal counted once");
        assert_eq!(w.claiming.stats().failures, 1);
        ended
    };
    assert_eq!(refused(SCOUT_MMO), Some(REREAD_WAIT), "no read came");
    let off = ScoutConfig {
        enabled: false,
        ..SCOUT_MMO
    };
    assert_eq!(refused(off), Some(0), "scouting off: no wait");
}
