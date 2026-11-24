//! Defender tests (P7.5, ADR-036), among them the P7 gate's healer-duo scenario against the room's
//! towers and defenders. Expected values come from the constants (`BODYPART_COST` through
//! `Part::cost`, `RANGED_ATTACK_POWER`, `HEAL_POWER`, `TOWER_POWER_ATTACK` and the falloff,
//! `BODYPART_HITS`, `MAX_CREEP_SIZE`) and the engine's order within a tick (damage, then heal, then
//! death; V287), written out here apart from the code under test.

use rustedscreep_consts::extra::BODYPART_HITS;
use rustedscreep_consts::{
    Boost, CREEP_RANGED_ACTION_RANGE, HEAL_POWER, MAX_CREEP_SIZE, Part, Position,
    RANGED_ATTACK_POWER, ROOM_AREA, ResourceType, RoomCoordinate, RoomName, StructureType,
    TOWER_CAPACITY, TOWER_FALLOFF, TOWER_FALLOFF_RANGE, TOWER_OPTIMAL_RANGE, TOWER_POWER_ATTACK,
};

use super::defenders::{
    DefenderEvent, Defenders, INSIDE, ON_RAMPART, OUTSIDE, PATTERN, PRIORITY_DEFENDER, body_for,
    flood, least_power,
};
use super::threat::Threats;
use super::towers::{Shot, TowerControl, TowerEvent, TowerOrder};
use crate::config::{DEFENSE_MMO, DefenseConfig, MOVEMENT_MMO, SPAWN_MMO};
use crate::economy::{
    PRIORITY_HAULER, PRIORITY_LOW, PRIORITY_MINER, PRIORITY_RECOVERY, PRIORITY_UPGRADER,
};
use crate::intents::{CreepIntent, IntentBuffer, Rejected};
use crate::movement::{CostGrid, Footprint, Movement, TerrainGrid, Tile, profile};
use crate::scheduler::Commit;
use crate::snapshot::{
    BodyPartRead, BodyRead, Controller, CreepRow, ObjectKey, RoomEnergy, Snapshot, StructureStatic,
    TickContext,
};
use crate::spawning::{BodySpec, EnergyBudget, Identity, Role, SpawnRequest, Spawner};

const ROOM: &str = "W1N1";

/// A ranged attack's reach (V339), and the melee reach plus one step that makes a post exposed.
const REACH: u8 = CREEP_RANGED_ACTION_RANGE;
const MELEE_LEAD: u8 = 2;

fn room() -> RoomName {
    ROOM.parse().expect("valid room name")
}

fn at(x: u8, y: u8) -> Position {
    Position::new(
        RoomCoordinate::new(x).expect("in room"),
        RoomCoordinate::new(y).expect("in room"),
        room(),
    )
}

fn tile(x: u8, y: u8) -> Tile {
    Tile::new(x, y).expect("in room")
}

fn plain(part: Part) -> BodyPartRead {
    BodyPartRead { part, boost: None }
}

fn body(spec: &[(Part, u32)]) -> Vec<BodyPartRead> {
    spec.iter()
        .flat_map(|&(p, n)| std::iter::repeat_n(plain(p), usize::try_from(n).expect("small")))
        .collect()
}

fn full(parts: &[BodyPartRead]) -> u32 {
    u32::try_from(parts.len()).expect("small") * BODYPART_HITS
}

/// The engine's tower falloff with the operations in its order (V239), from the constants.
fn falloff(range: u32) -> u32 {
    let (opt, far) = (
        u32::from(TOWER_OPTIMAL_RANGE),
        u32::from(TOWER_FALLOFF_RANGE),
    );
    if range <= opt {
        return TOWER_POWER_ATTACK;
    }
    let p = f64::from(TOWER_POWER_ATTACK);
    let r = range.min(far);
    (p - p * TOWER_FALLOFF * f64::from(r - opt) / f64::from(far - opt)).floor() as u32
}

/// The fight of an unboosted creep in the engine's order within a tick (V287): damage, heal capped
/// at `hitsMax`, death at 0 or less; `None` when a tick leaves it no lower.
fn unboosted_ticks(hits: u32, max: u32, damage: u32, heal: u32, limit: u32) -> Option<u32> {
    let mut h = i64::from(hits);
    for t in 1..=limit {
        let after = h - i64::from(damage) + i64::from(heal);
        if after <= 0 {
            return Some(t);
        }
        let after = after.min(i64::from(max));
        if after >= h {
            return None;
        }
        h = after;
    }
    None
}

/// The least RANGED_ATTACK parts that kill an unboosted creep with the towers' `towers` damage a
/// tick, against `heal`, within `limit` ticks, by a linear search with [`unboosted_ticks`].
fn brute_parts(hits: u32, towers: u32, heal: u32, limit: u32, most: u32) -> Option<u32> {
    (1..=most).find(|&p| {
        unboosted_ticks(hits, hits, towers + p * RANGED_ATTACK_POWER, heal, limit).is_some()
    })
}

/// The line of the test room: a ring of walls from (18, 18) to (32, 32), with an own rampart gate
/// at (25, 18), and a spawn at (25, 25) inside it.
const LOW: u8 = 18;
const HIGH: u8 = 32;
const GATE: (u8, u8) = (25, 18);
const SPAWN: (u8, u8) = (25, 25);

fn ring(gap: Option<(u8, u8)>) -> Vec<Tile> {
    let mut walls = Vec::new();
    for x in LOW..=HIGH {
        for y in LOW..=HIGH {
            let edge = x == LOW || x == HIGH || y == LOW || y == HIGH;
            if edge && (x, y) != GATE && Some((x, y)) != gap {
                walls.push(tile(x, y));
            }
        }
    }
    walls
}

fn grid_of(blocked: &[Tile]) -> CostGrid {
    let terrain = TerrainGrid::from_row_major(&[0u8; ROOM_AREA]).expect("terrain");
    let mut grid = CostGrid::default();
    grid.build(
        &terrain,
        blocked.iter().map(|&t| (t, Footprint::Blocked)),
        profile(&MOVEMENT_MMO),
    );
    grid
}

#[test]
fn the_body_is_whole_pairs_of_move_then_ranged_attack() {
    let pair = Part::Move.cost() + Part::RangedAttack.cost();
    let most = MAX_CREEP_SIZE / 2;
    assert_eq!(body_for(pair - 1), None, "below one pair");
    for capacity in [pair, 300, 550, 1300, 2300, 5600, 12_900] {
        let b = body_for(capacity).expect("a pair fits");
        let pairs = (capacity / pair).min(most);
        let n = usize::try_from(pairs).expect("small");
        assert_eq!(b.count(Part::Move), n, "capacity {capacity}");
        assert_eq!(b.count(Part::RangedAttack), n, "capacity {capacity}");
        assert_eq!(b.cost(), pairs * pair);
        assert!(b.cost() <= capacity);
        // MOVE first, so damage takes the legs before the weapons (V283).
        assert!(b.parts()[..n].iter().all(|&p| p == PATTERN[0]));
        assert!(b.parts()[n..].iter().all(|&p| p == PATTERN[1]));
    }
}

#[test]
fn least_power_is_the_fewest_parts_that_kill() {
    let mut scratch = Vec::new();
    let limit = DEFENSE_MMO.max_kill_ticks;
    for hits in [100u32, 1000, 2000, 5000] {
        let parts = body(&[(Part::Attack, hits / BODYPART_HITS)]);
        for tower in [0u32, falloff(20), falloff(9), TOWER_POWER_ATTACK] {
            for heal in [0u32, 120, 600, 1200] {
                for most in [0u32, 11, 33, 75] {
                    let towers = if tower > 0 {
                        vec![Shot {
                            damage: tower,
                            shots: TOWER_CAPACITY,
                        }]
                    } else {
                        Vec::new()
                    };
                    let got =
                        least_power(&parts, hits, hits, &towers, heal, limit, most, &mut scratch);
                    assert_eq!(
                        got,
                        brute_parts(hits, tower, heal, limit, most),
                        "hits {hits}, tower {tower}, heal {heal}, most {most}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_inside_stops_at_the_line() {
    let spawn = tile(SPAWN.0, SPAWN.1);
    let gate = tile(GATE.0, GATE.1);
    let mut blocked = ring(None);
    blocked.push(spawn);
    let grid = grid_of(&blocked);
    let mut out = [OUTSIDE; ROOM_AREA];
    let mut queue = Vec::new();
    flood(&grid, &[gate], &[spawn], &mut out, &mut queue);
    let cell = |x: u8, y: u8| out[tile(x, y).index()];
    assert_eq!(cell(25, 26), INSIDE, "next to the spawn");
    assert_eq!(cell(LOW + 1, LOW + 1), INSIDE, "an inside corner");
    assert_eq!(cell(GATE.0, GATE.1), ON_RAMPART, "the gate is taken in");
    assert_eq!(cell(GATE.0, GATE.1 - 1), OUTSIDE, "not passed");
    assert_eq!(cell(10, 10), OUTSIDE);
    assert_eq!(cell(LOW, 25), OUTSIDE, "a wall");
    assert_eq!(cell(SPAWN.0, SPAWN.1), OUTSIDE, "the spawn's own tile");
    // No inside tile touches an outside walkable one (V244): the line separates them.
    for i in 0..ROOM_AREA {
        let t = Tile::from_index(i).expect("tile");
        if out[i] != INSIDE {
            continue;
        }
        for (_, n) in t.neighbors() {
            assert!(
                !grid.walkable(n) || out[n.index()] != OUTSIDE,
                "{t:?} inside touches {n:?} outside"
            );
        }
    }
    // A gap lets the flood out: the whole room is inside.
    let mut leaky = ring(Some((LOW, 25)));
    leaky.push(spawn);
    flood(&grid_of(&leaky), &[gate], &[spawn], &mut out, &mut queue);
    assert_eq!(out[tile(10, 10).index()], INSIDE);
    // No spawn: every walkable tile, the ramparts marked.
    flood(&grid, &[gate], &[], &mut out, &mut queue);
    assert_eq!(out[tile(10, 10).index()], INSIDE);
    assert_eq!(out[gate.index()], ON_RAMPART);
    assert_eq!(out[tile(LOW, 25).index()], OUTSIDE, "a wall");
}

#[test]
fn the_defender_priority_is_above_every_economy_role_but_recovery() {
    const { assert!(PRIORITY_RECOVERY > PRIORITY_DEFENDER) };
    for economy in [
        PRIORITY_LOW,
        PRIORITY_MINER,
        PRIORITY_HAULER,
        PRIORITY_UPGRADER,
    ] {
        assert!(PRIORITY_DEFENDER > economy, "{economy}");
    }
}

/// A hostile creep in the test world.
struct Foe {
    key: u128,
    pos: (u8, u8),
    parts: Vec<BodyPartRead>,
}

/// One of the bot's defenders in the test world.
struct Def {
    key: u128,
    pos: Position,
    spawning: bool,
    parts: Vec<BodyPartRead>,
    /// Its hits as read; `None` for whole.
    hits: Option<u32>,
}

fn defender(key: u128, x: u8, y: u8, pairs: u32) -> Def {
    Def {
        key,
        pos: at(x, y),
        spawning: false,
        parts: body(&[(Part::Move, pairs), (Part::RangedAttack, pairs)]),
        hits: None,
    }
}

/// The test room (the ring, its gate and spawn), its towers, and the jobs in the glue's order.
struct World {
    snap: Snapshot,
    threats: Threats,
    towers: TowerControl,
    defenders: Defenders,
    movement: Movement,
    spawner: Spawner,
    buffer: IntentBuffer,
    cfg: DefenseConfig,
    capacity: u32,
    /// Tower positions, each full.
    tower_at: Vec<(u8, u8)>,
}

/// The tick a test world starts at.
const T0: u32 = 1_000;

impl World {
    fn new(towers: &[(u8, u8)]) -> Self {
        Self {
            snap: Snapshot::default(),
            threats: Threats::default(),
            towers: TowerControl::default(),
            defenders: Defenders::default(),
            movement: Movement::default(),
            spawner: Spawner::default(),
            buffer: IntentBuffer::default(),
            cfg: DEFENSE_MMO,
            capacity: 2300,
            tower_at: towers.to_vec(),
        }
    }

    fn tick(&mut self, time: u32, foes: &[Foe], defs: &[Def]) {
        let snap = &mut self.snap;
        snap.begin(TickContext::from_raw(time, 20, 500.0, 10_000));
        let row = snap.rooms.push(room(), Controller::Mine { level: 8 });
        snap.rooms.set_energy(
            row,
            RoomEnergy {
                available: self.capacity,
                capacity: self.capacity,
            },
        );
        // A worker inside, so movement keeps the room's grid (it builds grids only where an own
        // creep stands).
        let worker = Identity {
            role: Role::Worker,
            home: room(),
            remote: None,
            serial: 1,
        };
        let row = CreepRow {
            pos: at(27, 27),
            my: true,
            spawning: false,
            fatigue: 0,
        };
        snap.creeps
            .push_named(ObjectKey(500), row, || Some(worker))
            .expect("worker");
        for d in defs {
            let id = Identity {
                role: Role::Defender,
                home: room(),
                remote: None,
                serial: u64::try_from(d.key).expect("small"),
            };
            let row = CreepRow {
                pos: d.pos,
                my: true,
                spawning: d.spawning,
                fatigue: 0,
            };
            snap.creeps
                .push_named(ObjectKey(d.key), row, || Some(id))
                .expect("defender");
            let index = snap.creeps.len() - 1;
            if !d.spawning {
                snap.creeps
                    .set_body(index, BodyRead::of(&d.parts).expect("fits"));
                snap.creeps
                    .set_own_hits(index, d.hits.unwrap_or(full(&d.parts)), full(&d.parts));
            }
        }
        for f in foes {
            let row = CreepRow {
                pos: at(f.pos.0, f.pos.1),
                my: false,
                spawning: false,
                fatigue: 0,
            };
            snap.creeps.push(ObjectKey(f.key), row).expect("hostile");
            let index = snap.creeps.len() - 1;
            snap.creeps.set_assessed(index, full(&f.parts));
            if !snap.creeps.knows_body(index) {
                snap.creeps
                    .set_body(index, BodyRead::of(&f.parts).expect("fits"));
            }
        }
        let spawn = snap
            .structures
            .push(ObjectKey(9_000), || {
                Ok(StructureStatic {
                    kind: StructureType::Spawn,
                    pos: at(SPAWN.0, SPAWN.1),
                })
            })
            .expect("spawn");
        let row = u32::try_from(snap.structures.len() - 1).expect("small");
        snap.spawns.push(spawn, row, false);
        snap.structures
            .push(ObjectKey(9_001), || {
                Ok(StructureStatic {
                    kind: StructureType::Rampart,
                    pos: at(GATE.0, GATE.1),
                })
            })
            .expect("rampart");
        for (i, &(x, y)) in self.tower_at.iter().enumerate() {
            let key = 9_100 + u128::try_from(i).expect("small");
            let handle = snap
                .structures
                .push(ObjectKey(key), || {
                    Ok(StructureStatic {
                        kind: StructureType::Tower,
                        pos: at(x, y),
                    })
                })
                .expect("tower");
            let row = u32::try_from(snap.structures.len() - 1).expect("small");
            snap.towers.push(handle, row, TOWER_CAPACITY);
        }
        snap.finish();
        // Movement's reads, as the glue's jobs hand them over.
        let mut blocked = ring(None);
        blocked.push(tile(SPAWN.0, SPAWN.1));
        blocked.extend(self.tower_at.iter().map(|&(x, y)| tile(x, y)));
        self.movement.begin(&self.snap, &MOVEMENT_MMO);
        let mut rooms = Vec::new();
        self.movement.first_reads(&MOVEMENT_MMO, &mut rooms);
        for &r in &rooms {
            assert!(self.movement.set_terrain(r, &[0u8; ROOM_AREA]));
            self.movement.set_statics(r, []);
            self.movement
                .set_objects(r, blocked.iter().map(|&t| (t, Footprint::Blocked)));
        }
        let snap = &self.snap;
        self.threats.assess(snap, &self.cfg);
        self.defenders.begin(snap, &self.threats);
        self.towers.set_allies(self.defenders.allies());
        self.towers.begin(snap, &self.threats, &self.cfg);
        for unit in 0..self.towers.unit_count() {
            self.towers
                .plan_room(unit, snap, &self.threats, &self.cfg)
                .expect("towers");
        }
        self.towers.finish(time, &self.threats);
        self.spawner.begin(snap, &SPAWN_MMO);
        let mut commit = Commit::default();
        for unit in 0..self.defenders.unit_count() {
            self.defenders
                .plan_room(
                    unit,
                    snap,
                    &self.threats,
                    &self.towers,
                    &mut self.movement,
                    &mut self.spawner,
                    &self.cfg,
                    &MOVEMENT_MMO,
                    &mut commit,
                )
                .expect("plan");
        }
        self.buffer.begin(snap);
        if self.defenders.idle(&self.threats) {
            self.defenders.hold_all(snap, &mut self.movement);
        } else {
            for unit in 0..self.defenders.member_count() {
                self.defenders
                    .work(
                        unit,
                        snap,
                        &self.threats,
                        &self.towers,
                        &mut self.movement,
                        &mut self.buffer,
                        &MOVEMENT_MMO,
                        &mut commit,
                    )
                    .expect("work");
            }
        }
        self.defenders.finish(&self.threats);
    }

    fn requests(&self) -> Vec<SpawnRequest> {
        (0..self.spawner.stats().requests)
            .filter_map(|i| self.spawner.request_at(i).copied())
            .collect()
    }

    fn key_at(&self, row: u32) -> u128 {
        let c = &self.snap.creeps;
        let h = c.handles()[usize::try_from(row).expect("small")];
        c.key_of(h).expect("live").0
    }

    /// This tick's ranged attacks as (defender key, target key).
    fn shots(&mut self) -> Vec<(u128, u128)> {
        self.buffer.resolve();
        let mut out = Vec::new();
        for issue in self.buffer.planned() {
            if let CreepIntent::RangedAttack { target } = issue.intent {
                let c = &self.snap.creeps;
                let t = c.key_of(target).expect("live").0;
                out.push((self.key_at(issue.actor), t));
            }
        }
        out
    }
}

/// The healer duo of the gate scenario: an attacker (10 ATTACK, 10 MOVE) two tiles outside the
/// gate, within range 3 of the first tile inside it, with two healers of 25 HEAL next to it.
const ATTACKER: (u8, u8) = (25, 16);

fn duo(heal_parts: u32) -> Vec<Foe> {
    vec![
        Foe {
            key: 1,
            pos: ATTACKER,
            parts: body(&[(Part::Attack, 10), (Part::Move, 10)]),
        },
        Foe {
            key: 2,
            pos: (25, 15),
            parts: body(&[(Part::Heal, heal_parts)]),
        },
        Foe {
            key: 3,
            pos: (24, 15),
            parts: body(&[(Part::Heal, heal_parts)]),
        },
    ]
}

/// The tower of the gate scenario, inside the line at range 9 of the attacker.
const TOWER: (u8, u8) = (24, 25);

fn tower_range() -> u32 {
    u32::from(
        TOWER
            .0
            .abs_diff(ATTACKER.0)
            .max(TOWER.1.abs_diff(ATTACKER.1)),
    )
}

/// P7 gate scenario: a healer duo against the room's tower, then its defenders. Each healer gives
/// the attacker `HEAL_POWER` per HEAL part (both at range 1), more than the tower deals at its
/// range, so the towers hold fire. The room asks for the fewest RANGED_ATTACK parts that kill the
/// attacker within `max_kill_ticks` with the tower, in full bodies at its capacity; once two
/// defenders stand within range 3, the tower fires with them and they shoot the attacker.
#[test]
fn scenario_healer_duo_against_the_towers_and_defenders() {
    let mut w = World::new(&[TOWER]);
    let foes = duo(25);
    let heal = 2 * 25 * HEAL_POWER;
    let tower = falloff(tower_range());
    assert!(tower < heal, "the tower alone cannot win");
    let attacker = full(&foes[0].parts);
    let limit = w.cfg.max_kill_ticks;
    let pairs = w.capacity / (Part::Move.cost() + Part::RangedAttack.cost());
    let most = pairs * w.cfg.max_defenders;
    let parts = brute_parts(attacker, tower, heal, limit, most).expect("the cap wins");
    let bodies = parts.div_ceil(pairs);

    // Rows: the worker, then the attacker (row 1) and its healers.
    w.tick(T0, &foes, &[]);
    assert!(
        w.towers
            .events()
            .iter()
            .any(|e| matches!(e, TowerEvent::Hold { .. })),
        "the towers hold fire"
    );
    let requests = w.requests();
    assert_eq!(requests.len(), usize::try_from(bodies).expect("small"));
    let want = body_for(w.capacity).expect("body");
    for r in &requests {
        assert_eq!(r.role, Role::Defender);
        assert_eq!(r.home, room());
        assert_eq!(r.priority, PRIORITY_DEFENDER);
        assert_eq!(r.body, BodySpec::Exact(want));
        assert_eq!(r.budget, EnergyBudget::Capacity);
    }
    let ticks = unboosted_ticks(
        attacker,
        attacker,
        tower + parts * RANGED_ATTACK_POWER,
        heal,
        limit,
    )
    .expect("kills");
    assert_eq!(
        w.defenders.events(),
        &[DefenderEvent::Need {
            room: room(),
            target: 1,
            parts,
            defenders: bodies,
            per_body: pairs,
            cost: want.cost(),
            ticks,
            have: 0,
        }]
    );

    // One defender spawning counts its full body: one body fewer is asked for, and no new line.
    let mut spawning = defender(10, SPAWN.0, SPAWN.1, pairs);
    spawning.spawning = true;
    w.tick(T0 + 1, &foes, &[spawning]);
    assert_eq!(
        w.requests().len(),
        usize::try_from(bodies - 1).expect("small")
    );
    assert!(w.defenders.events().is_empty());

    // Both in range of the attacker: the tower fires with them, and both shoot it. (Its `Fire`
    // line waits for `threat_log_ticks` after the `Hold` line; a fresh world shows its figures.)
    let defs = [defender(10, 25, 19, pairs), defender(11, 24, 19, pairs)];
    w.tick(T0 + 2, &foes, &defs);
    assert!(w.requests().is_empty(), "the team is whole");
    let orders: Vec<u128> = w
        .towers
        .orders()
        .iter()
        .map(|o| match *o {
            TowerOrder::Attack { target, .. } => w.key_at(target),
            TowerOrder::Heal { .. } => panic!("no heal: {o:?}"),
        })
        .collect();
    assert_eq!(orders, vec![1], "the tower fires at the attacker");
    let mut shots = w.shots();
    shots.sort_unstable();
    assert_eq!(shots, vec![(10, 1), (11, 1)]);

    let mut fresh = World::new(&[TOWER]);
    fresh.tick(T0, &foes, &defs);
    let fire = fresh
        .towers
        .events()
        .iter()
        .find_map(|e| match *e {
            TowerEvent::Fire {
                target,
                damage,
                heal: h,
                ticks,
                ..
            } => Some((fresh.key_at(target), damage, h, ticks)),
            _ => None,
        })
        .expect("the towers fire");
    let together = tower + 2 * pairs * RANGED_ATTACK_POWER;
    let ticks = unboosted_ticks(attacker, attacker, together, heal, limit).expect("kills");
    // The line's damage is the towers' own; the kill counts the defenders'.
    assert_eq!(fire, (1, tower, heal, ticks));
    assert!(
        fresh.requests().is_empty(),
        "two full bodies hold the parts"
    );
}

/// The P7 gate's melee and ranged attackers: the tower kills an unhealed one alone, so no defender
/// is asked for.
#[test]
fn scenario_no_defender_while_the_towers_win() {
    for parts in [
        body(&[(Part::Attack, 10), (Part::Move, 10)]),
        body(&[(Part::RangedAttack, 10), (Part::Move, 10)]),
    ] {
        let mut w = World::new(&[TOWER]);
        let foe = Foe {
            key: 1,
            pos: ATTACKER,
            parts,
        };
        let hits = full(&foe.parts);
        let ticks = unboosted_ticks(hits, hits, falloff(tower_range()), 0, w.cfg.max_kill_ticks);
        assert!(ticks.is_some(), "the tower alone wins");
        w.tick(T0, &[foe], &[]);
        assert!(w.requests().is_empty());
        assert!(w.defenders.events().is_empty());
    }
}

/// With no tower, any dangerous hostile in reach needs defenders: a small melee creep needs the
/// fewest parts that kill it within `max_kill_ticks` alone.
#[test]
fn a_room_with_no_tower_asks_for_defenders() {
    let mut w = World::new(&[]);
    let foe = Foe {
        key: 1,
        pos: ATTACKER,
        parts: body(&[(Part::Attack, 2), (Part::Move, 2)]),
    };
    let hits = full(&foe.parts);
    let pairs = w.capacity / (Part::Move.cost() + Part::RangedAttack.cost());
    let parts = brute_parts(hits, 0, 0, w.cfg.max_kill_ticks, pairs).expect("one body wins");
    w.tick(T0, &[foe], &[]);
    assert_eq!(w.requests().len(), 1);
    assert!(matches!(
        w.defenders.events(),
        [DefenderEvent::Need { parts: p, defenders: 1, .. }] if *p == parts
    ));
}

/// A duo whose heal beats the tower and the most defenders the cap allows is logged once a threat,
/// and nothing is asked for.
#[test]
fn a_hostile_beyond_the_cap_is_logged_once() {
    let mut w = World::new(&[TOWER]);
    let pairs = w.capacity / (Part::Move.cost() + Part::RangedAttack.cost());
    let most = pairs * w.cfg.max_defenders;
    let heal_parts = 50;
    let foes = duo(heal_parts);
    let heal = 2 * heal_parts * HEAL_POWER;
    let tower = falloff(tower_range());
    assert!(tower + most * RANGED_ATTACK_POWER < heal, "beyond the cap");
    w.tick(T0, &foes, &[]);
    assert!(w.requests().is_empty());
    assert_eq!(
        w.defenders.events(),
        &[DefenderEvent::Outmatched {
            room: room(),
            target: 1,
            defenders: w.cfg.max_defenders,
            per_body: pairs,
        }]
    );
    w.tick(T0 + 1, &foes, &[]);
    assert!(w.requests().is_empty());
    assert!(w.defenders.events().is_empty(), "once a threat");
}

/// A duo parked more than 3 tiles from every tile inside the line cannot be reached: no request.
#[test]
fn a_hostile_out_of_reach_needs_nothing() {
    let mut w = World::new(&[TOWER]);
    let mut foes = duo(25);
    for (f, pos) in foes.iter_mut().zip([(25u8, 13u8), (25, 12), (24, 12)]) {
        f.pos = pos;
    }
    // The nearest inside tile is the gate, 5 tiles from the attacker.
    w.tick(T0, &foes, &[]);
    assert!(w.requests().is_empty());
    assert!(w.defenders.events().is_empty());
}

/// With `defense.defenders` off nothing is asked for, but the need is still logged.
#[test]
fn defenders_off_requests_nothing() {
    let mut w = World::new(&[TOWER]);
    w.cfg.defenders = false;
    w.tick(T0, &duo(25), &[]);
    assert!(w.requests().is_empty());
    assert!(matches!(w.defenders.events(), [DefenderEvent::Need { .. }]));
}

/// A defender's post: inside the line, within range 3 of the target, out of a melee hostile's
/// reach, and the nearest such tile; it shoots nothing out of range.
#[test]
fn a_post_is_inside_in_range_and_out_of_melee() {
    let mut w = World::new(&[]);
    let melee = (25u8, 17u8);
    let foe = Foe {
        key: 1,
        pos: melee,
        parts: body(&[(Part::Attack, 2), (Part::Move, 2)]),
    };
    let pairs = 11;
    let me = (28u8, 28u8);
    w.tick(T0, &[foe], &[defender(10, me.0, me.1, pairs)]);
    let &[(r, post)] = w.defenders.posts() else {
        panic!("one post: {:?}", w.defenders.posts());
    };
    assert_eq!(r, room());
    let target = tile(melee.0, melee.1);
    let mine = tile(me.0, me.1);
    assert!(post.range(target) <= REACH, "{post:?} in range");
    assert!(
        post.range(target) > MELEE_LEAD,
        "{post:?} out of melee reach"
    );
    // Inside the ring, not on the line.
    assert!((LOW + 1..HIGH).contains(&post.x()) && (LOW + 1..HIGH).contains(&post.y()));
    // No inside tile in range and out of melee reach is nearer.
    for x in LOW + 1..HIGH {
        for y in LOW + 1..HIGH {
            let c = tile(x, y);
            if c.range(target) <= REACH && c.range(target) > MELEE_LEAD {
                assert!(mine.range(c) >= mine.range(post), "{c:?} is nearer");
            }
        }
    }
    assert!(w.shots().is_empty(), "nothing in range");
}

/// The intent buffer never lets a defender shoot an own creep (the game would, V339, V340).
#[test]
fn a_ranged_attack_on_an_own_creep_is_refused() {
    let mut w = World::new(&[]);
    w.tick(T0, &[], &[defender(10, 26, 26, 1), defender(11, 26, 27, 1)]);
    let c = &w.snap.creeps;
    let handle = |key: u128| {
        let i = c
            .handles()
            .iter()
            .position(|&h| c.key_of(h).expect("live").0 == key)
            .expect("present");
        c.handles()[i]
    };
    let (a, b) = (handle(10), handle(11));
    let mut buffer = IntentBuffer::default();
    buffer.begin(&w.snap);
    let mut commit = Commit::default();
    for target in [a, b] {
        assert_eq!(
            buffer.push(
                &w.snap,
                a,
                CreepIntent::RangedAttack { target },
                &mut commit
            ),
            Err(Rejected::WrongTarget)
        );
    }
}

/// Plans are deterministic: the same ticks give the same requests, events, posts and shots.
#[test]
fn plans_are_deterministic() {
    let run = || {
        let mut w = World::new(&[TOWER]);
        let foes = duo(25);
        let mut out = Vec::new();
        for (i, defs) in [
            vec![],
            vec![defender(10, 27, 27, 11), defender(11, 23, 23, 11)],
        ]
        .into_iter()
        .enumerate()
        {
            w.tick(T0 + u32::try_from(i).expect("small"), &foes, &defs);
            out.push((
                w.requests(),
                w.defenders.events().to_vec(),
                w.defenders.posts().to_vec(),
                w.shots(),
            ));
        }
        out
    };
    assert_eq!(run(), run());
}

/// The hot-path review's case: a healer left after its attacker died is harmless, so the room is not
/// planned, but the towers' sim counts the defenders in range of it and fires; the defenders shoot
/// the towers' focus, so the damage the towers expect lands (no false miss for the shot check).
#[test]
fn defenders_shoot_the_towers_focus_with_no_dangerous_hostile() {
    let mut w = World::new(&[TOWER]);
    // Self-heal that matches the tower at its range: 40 HEAL parts heal 40 × HEAL_POWER.
    let heal_parts = falloff(tower_range()).div_ceil(HEAL_POWER);
    let healer = Foe {
        key: 2,
        pos: ATTACKER,
        parts: body(&[(Part::Heal, heal_parts)]),
    };
    let tower = falloff(tower_range());
    let pairs = 11;
    let heal = heal_parts * HEAL_POWER;
    let hits = full(&healer.parts);
    let limit = w.cfg.max_kill_ticks;
    // The tower alone does not win against its self-heal in time; with both defenders it does.
    assert!(unboosted_ticks(hits, hits, tower, heal, limit).is_none());
    let together = tower + 2 * pairs * RANGED_ATTACK_POWER;
    assert!(unboosted_ticks(hits, hits, together, heal, limit).is_some());
    let defs = [defender(10, 25, 19, pairs), defender(11, 24, 19, pairs)];
    w.tick(T0, &[healer], &defs);
    assert_eq!(w.defenders.unit_count(), 0, "no dangerous hostile");
    let orders: Vec<u128> = w
        .towers
        .orders()
        .iter()
        .filter_map(|o| match *o {
            TowerOrder::Attack { target, .. } => Some(w.key_at(target)),
            TowerOrder::Heal { .. } => None,
        })
        .collect();
    assert_eq!(orders, vec![2], "the tower fires at the healer");
    let mut shots = w.shots();
    shots.sort_unstable();
    assert_eq!(shots, vec![(10, 2), (11, 2)]);
}

/// Two defenders never share a post: one standing on the other's best tile keeps it, and each
/// defender keeps its post from one tick to the next while it stays valid.
#[test]
fn defenders_keep_distinct_and_steady_posts() {
    let mut w = World::new(&[]);
    // A ranged hostile (no melee reach) within range 3 of the gate and of the first inside row.
    let foe = Foe {
        key: 1,
        pos: (25, 16),
        parts: body(&[(Part::RangedAttack, 2), (Part::Move, 2)]),
    };
    let defs = [defender(10, 25, 18, 11), defender(11, 26, 19, 11)];
    w.tick(T0, std::slice::from_ref(&foe), &defs);
    let first = w.defenders.posts().to_vec();
    assert_eq!(first.len(), 2);
    assert_ne!(first[0].1, first[1].1, "distinct posts: {first:?}");
    // The defender on the gate rampart, within range 3 of the hostile, keeps it.
    assert!(first.contains(&(room(), tile(GATE.0, GATE.1))), "{first:?}");
    w.tick(T0 + 1, &[foe], &defs);
    assert_eq!(w.defenders.posts(), &first[..], "steady posts");
}

/// The combat-math audit's H2: a creep with a CLAIM part gets as many towers as without the
/// defenders (P7.4's claim trigger counts only the towers firing at it). Three towers within the
/// optimal range against 15 CLAIM parts: with two defenders' help two towers would kill it in one
/// tick, but all the towers the kill alone needs fire.
#[test]
fn a_claimer_gets_the_towers_it_needs_alone() {
    let towers = [(24, 24), (26, 24), (25, 26)];
    let mut w = World::new(&towers);
    let claimer = Foe {
        key: 1,
        pos: (25, 21),
        parts: body(&[(Part::Claim, 15)]),
    };
    let hits = full(&claimer.parts);
    let pairs = 25;
    let defs = [defender(10, 25, 22, pairs), defender(11, 24, 22, pairs)];
    let allies = 2 * pairs * RANGED_ATTACK_POWER;
    let alone = hits.div_ceil(TOWER_POWER_ATTACK);
    let helped = hits.saturating_sub(allies).div_ceil(TOWER_POWER_ATTACK);
    assert!(helped < alone, "the defenders would save a tower");
    w.tick(T0, &[claimer], &defs);
    let attacks = w
        .towers
        .orders()
        .iter()
        .filter(|o| matches!(o, TowerOrder::Attack { .. }))
        .count();
    assert_eq!(attacks, usize::try_from(alone).expect("small"));
}

/// `least_power` against a linear search over the same sim on bodies with boosted TOUGH parts in
/// front (each compound's `damage` ratio from the `BOOSTS` table), and more parts never slow a kill.
#[test]
fn least_power_holds_on_boosted_bodies() {
    let mut scratch = Vec::new();
    let limit = DEFENSE_MMO.max_kill_ticks;
    let compounds: Vec<ResourceType> = [
        ResourceType::GhodiumOxide,
        ResourceType::GhodiumAlkalide,
        ResourceType::CatalyzedGhodiumAlkalide,
    ]
    .into_iter()
    .filter(|r| matches!(r.boost(), Some(Boost::Tough(_))))
    .collect();
    assert_eq!(compounds.len(), 3, "the three TOUGH compounds");
    for &compound in &compounds {
        for tough in [1u32, 5, 10] {
            let mut parts: Vec<BodyPartRead> = (0..tough)
                .map(|_| BodyPartRead {
                    part: Part::Tough,
                    boost: Some(compound),
                })
                .collect();
            parts.extend(body(&[(Part::Attack, 10), (Part::Move, 10)]));
            let hits = full(&parts);
            for tower in [0u32, falloff(20), falloff(9)] {
                for heal in [0u32, 300, 600] {
                    let towers: Vec<Shot> = (tower > 0)
                        .then_some(Shot {
                            damage: tower,
                            shots: TOWER_CAPACITY,
                        })
                        .into_iter()
                        .collect();
                    let kills = |p: u32| {
                        let mut shots = towers.clone();
                        shots.push(Shot {
                            damage: p * RANGED_ATTACK_POWER,
                            shots: u32::MAX,
                        });
                        super::towers::kill_ticks(&parts, hits, hits, &shots, heal, limit)
                    };
                    let most = 75;
                    for p in 1..most {
                        if let Some(t) = kills(p) {
                            let next = kills(p + 1).expect("more parts still kill");
                            assert!(next <= t, "{compound:?} x{tough}: {p} parts");
                        }
                    }
                    let linear = (1..=most).find(|&p| kills(p).is_some());
                    let got =
                        least_power(&parts, hits, hits, &towers, heal, limit, most, &mut scratch);
                    assert_eq!(
                        got, linear,
                        "{compound:?} x{tough}, tower {tower}, heal {heal}"
                    );
                }
            }
        }
    }
}

/// A damaged defender counts only its live RANGED_ATTACK parts: the parts fill from the last
/// (V283), so with MOVE parts first, the weapons are the last to go.
#[test]
fn a_damaged_defender_counts_its_live_parts() {
    let pairs = 11u32;
    let len = 2 * pairs;
    for hits in [len * BODYPART_HITS, len * BODYPART_HITS - 150, 1000, 250] {
        let mut w = World::new(&[TOWER]);
        let mut d = defender(10, 25, 19, pairs);
        d.hits = Some(hits);
        w.tick(T0, &duo(25), &[d]);
        let live = hits.div_ceil(BODYPART_HITS).min(pairs);
        assert_eq!(
            w.defenders.allies(),
            &[(at(25, 19), live * RANGED_ATTACK_POWER)],
            "hits {hits}"
        );
    }
}
