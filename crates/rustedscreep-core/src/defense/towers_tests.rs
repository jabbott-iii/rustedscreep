//! Tower control tests (P7.2), among them the P7 gate's combat scenarios: a melee attacker, a ranged
//! attacker, and a healer duo against a room's towers. Expected values come from the constants
//! (`TOWER_POWER_ATTACK`, `TOWER_POWER_HEAL`, the falloff constants, `TOWER_ENERGY_COST`, the creep
//! powers, `BODYPART_HITS`) and the engine's order within a tick (damage, then heal, then death;
//! V287), never from the code under test.

use rustedscreep_consts::extra::BODYPART_HITS;
use rustedscreep_consts::{
    HEAL_POWER, Part, Position, RANGED_HEAL_POWER, ResourceType, RoomCoordinate, RoomName,
    StructureType, TOWER_CAPACITY, TOWER_ENERGY_COST, TOWER_FALLOFF, TOWER_FALLOFF_RANGE,
    TOWER_OPTIMAL_RANGE, TOWER_POWER_ATTACK, TOWER_POWER_HEAL,
};

use super::threat::{Power, Threats};
use super::towers::{
    Shot, TowerControl, TowerEvent, TowerOrder, heal_on, kill_ticks, tower_amount,
};
use crate::config::{DEFENSE_MMO, DefenseConfig};
use crate::planner::validate::tower_damage;
use crate::snapshot::{
    BodyPartRead, BodyRead, Controller, CreepRow, ObjectKey, Snapshot, StructureStatic, TickContext,
};
use rustedscreep_consts::Boost;

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

const ROOM: &str = "W1N1";

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

/// The engine's falloff with the operations in its order (towers/attack.js and heal.js, V239,
/// V296), written out here from the constants.
fn falloff(power: u32, range: u32) -> u32 {
    let (opt, far) = (
        u32::from(TOWER_OPTIMAL_RANGE),
        u32::from(TOWER_FALLOFF_RANGE),
    );
    if range <= opt {
        return power;
    }
    let p = f64::from(power);
    let r = range.min(far);
    (p - p * TOWER_FALLOFF * f64::from(r - opt) / f64::from(far - opt)).floor() as u32
}

/// The fight of an unboosted creep, in the engine's order within a tick (V287): damage, heal capped
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

#[test]
fn tower_amount_follows_the_falloff() {
    for range in 0..=50u32 {
        let attack = tower_amount(TOWER_POWER_ATTACK, range);
        assert_eq!(
            attack,
            falloff(TOWER_POWER_ATTACK, range),
            "attack at {range}"
        );
        // The planner's tower damage is the verified attack formula (V239).
        assert_eq!(attack, tower_damage(u8::try_from(range).expect("small")));
        assert_eq!(
            tower_amount(TOWER_POWER_HEAL, range),
            falloff(TOWER_POWER_HEAL, range),
            "heal at {range}"
        );
        if range > 0 {
            assert!(attack <= tower_amount(TOWER_POWER_ATTACK, range - 1));
        }
    }
    let floor = |p: u32| (f64::from(p) * (1.0 - TOWER_FALLOFF)).floor() as u32;
    assert_eq!(
        tower_amount(TOWER_POWER_ATTACK, u32::from(TOWER_FALLOFF_RANGE)),
        floor(TOWER_POWER_ATTACK)
    );
    assert_eq!(
        tower_amount(TOWER_POWER_HEAL, u32::from(TOWER_FALLOFF_RANGE)),
        floor(TOWER_POWER_HEAL)
    );
    assert_eq!(
        tower_amount(TOWER_POWER_ATTACK, u32::from(TOWER_OPTIMAL_RANGE)),
        TOWER_POWER_ATTACK
    );
}

#[test]
fn heal_on_counts_heal_by_range() {
    let healer = Power {
        heal: 3,
        ..Power::default()
    };
    let c = at(20, 20);
    for (x, want) in [
        (20u8, 3 * HEAL_POWER),
        (21, 3 * HEAL_POWER),
        (22, 3 * RANGED_HEAL_POWER),
        (23, 3 * RANGED_HEAL_POWER),
        (24, 0),
    ] {
        assert_eq!(heal_on(c, &[(at(x, 20), healer)]), want, "at x {x}");
    }
    let other = Position::new(
        RoomCoordinate::new(20).expect("in room"),
        RoomCoordinate::new(20).expect("in room"),
        "W2N1".parse().expect("valid"),
    );
    assert_eq!(heal_on(c, &[(other, healer)]), 0, "another room's healer");
    // Every healer in reach counts: the worst case for the bot.
    assert_eq!(
        heal_on(c, &[(at(20, 21), healer), (at(23, 20), healer)]),
        3 * HEAL_POWER + 3 * RANGED_HEAL_POWER
    );
}

/// For unboosted bodies the sim matches the engine's tick order written out above, and it stops a
/// tower that runs out of shots.
#[test]
fn kill_ticks_matches_the_tick_order() {
    let parts = body(&[(Part::Attack, 5), (Part::Move, 5)]);
    let max = full(&parts);
    for &hits in &[1, max / 2, max] {
        for &damage in &[
            TOWER_POWER_ATTACK / 4,
            TOWER_POWER_ATTACK,
            2 * TOWER_POWER_ATTACK,
        ] {
            for &heal in &[0, HEAL_POWER * 10, damage, damage + 1] {
                let towers = [Shot {
                    damage,
                    shots: TOWER_CAPACITY / TOWER_ENERGY_COST,
                }];
                let limit = DEFENSE_MMO.max_kill_ticks;
                assert_eq!(
                    kill_ticks(&parts, hits, max, &towers, heal, limit),
                    unboosted_ticks(hits, max, damage, heal, limit),
                    "{hits} hits, {damage} damage, {heal} heal"
                );
            }
        }
    }
    // A tower with two shots left stops after two ticks.
    let towers = [Shot {
        damage: TOWER_POWER_ATTACK / 4,
        shots: 2,
    }];
    assert_eq!(kill_ticks(&parts, max, max, &towers, 0, 100), None);
    assert_eq!(kill_ticks(&parts, max, max, &[], 0, 100), None);
}

/// More towers never slow a kill; more heal never speeds it; the result is within the limit.
#[test]
fn kill_ticks_is_monotone() {
    let parts = vec![
        BodyPartRead {
            part: Part::Tough,
            boost: Some(ResourceType::CatalyzedGhodiumAlkalide),
        },
        plain(Part::Heal),
        plain(Part::Move),
    ];
    let max = full(&parts);
    let shot = |range| Shot {
        damage: tower_amount(TOWER_POWER_ATTACK, range),
        shots: TOWER_CAPACITY / TOWER_ENERGY_COST,
    };
    let limit = DEFENSE_MMO.max_kill_ticks;
    for range in [1u32, 10, 20] {
        let mut last: Option<Option<u32>> = None;
        for n in 1..=6usize {
            let towers = vec![shot(range); n];
            let t = kill_ticks(&parts, max, max, &towers, 0, limit);
            assert!(t.is_none_or(|t| (1..=limit).contains(&t)));
            if let Some(Some(a)) = last {
                let b = t.expect("one more tower still kills");
                assert!(b <= a, "{n} towers at {range}");
            }
            last = Some(t);
        }
        let towers = vec![shot(range); 2];
        let mut prev = kill_ticks(&parts, max, max, &towers, 0, limit);
        for heal in (0..=TOWER_POWER_ATTACK * 2).step_by(50) {
            let t = kill_ticks(&parts, max, max, &towers, heal, limit);
            match (prev, t) {
                (Some(a), Some(b)) => assert!(b >= a, "heal {heal} at {range}"),
                (None, Some(_)) => panic!("more heal made it killable at {range}"),
                _ => {}
            }
            prev = t;
        }
    }
}

/// One tick's scenario world: an owned room at RCL8 with towers, hostile creeps, and own creeps.
#[derive(Clone)]
struct Tower {
    key: u128,
    pos: Position,
    energy: u32,
}

#[derive(Clone)]
struct Foe {
    key: u128,
    pos: Position,
    parts: Vec<BodyPartRead>,
    hits: u32,
}

#[derive(Clone)]
struct Own {
    key: u128,
    pos: Position,
    hits: u32,
    max: u32,
}

fn foe(key: u128, x: u8, y: u8, parts: Vec<BodyPartRead>) -> Foe {
    let hits = full(&parts);
    Foe {
        key,
        pos: at(x, y),
        parts,
        hits,
    }
}

fn tower(key: u128, x: u8, y: u8) -> Tower {
    Tower {
        key,
        pos: at(x, y),
        energy: TOWER_CAPACITY,
    }
}

struct World {
    snap: Snapshot,
    threats: Threats,
    control: TowerControl,
    cfg: DefenseConfig,
}

impl World {
    fn new() -> Self {
        Self {
            snap: Snapshot::default(),
            threats: Threats::default(),
            control: TowerControl::default(),
            cfg: DEFENSE_MMO,
        }
    }

    /// Takes the tick's snapshot as the wasm glue does, assesses it, and plans the towers.
    fn tick(&mut self, time: u32, towers: &[Tower], foes: &[Foe], owns: &[Own]) {
        let snap = &mut self.snap;
        snap.begin(TickContext::from_raw(time, 20, 500.0, 10_000));
        snap.rooms.push(room(), Controller::Mine { level: 8 });
        for o in owns {
            let row = CreepRow {
                pos: o.pos,
                my: true,
                spawning: false,
                fatigue: 0,
            };
            snap.creeps.push(ObjectKey(o.key), row).expect("own");
            let index = snap.creeps.len() - 1;
            snap.creeps.set_own_hits(index, o.hits, o.max);
        }
        for f in foes {
            let row = CreepRow {
                pos: f.pos,
                my: false,
                spawning: false,
                fatigue: 0,
            };
            snap.creeps.push(ObjectKey(f.key), row).expect("hostile");
            let index = snap.creeps.len() - 1;
            snap.creeps.set_assessed(index, f.hits);
            if !snap.creeps.knows_body(index) {
                snap.creeps
                    .set_body(index, BodyRead::of(&f.parts).expect("fits"));
            }
        }
        for t in towers {
            let handle = snap
                .structures
                .push(ObjectKey(t.key), || {
                    Ok(StructureStatic {
                        kind: StructureType::Tower,
                        pos: t.pos,
                    })
                })
                .expect("tower");
            let row = u32::try_from(snap.structures.len() - 1).expect("small");
            snap.towers.push(handle, row, t.energy);
        }
        snap.finish();
        self.threats.assess(&self.snap, &self.cfg);
        self.control.begin(&self.snap, &self.threats, &self.cfg);
        for unit in 0..self.control.unit_count() {
            self.control
                .plan_room(unit, &self.snap, &self.threats, &self.cfg)
                .expect("plan");
        }
        self.control.finish(time, &self.threats);
    }

    /// The creep key at a row of this tick.
    fn creep_key(&self, row: u32) -> u128 {
        let c = &self.snap.creeps;
        let h = c.handles()[usize::try_from(row).expect("small")];
        c.key_of(h).expect("live").0
    }

    /// This tick's orders as (tower key, attack or heal, creep key).
    fn orders(&self) -> Vec<(u128, &'static str, u128)> {
        let st = &self.snap.structures;
        let tower_key = |row: u32| {
            let h = st.handles()[usize::try_from(row).expect("small")];
            st.key_of(h).expect("live").0
        };
        self.control
            .orders()
            .iter()
            .map(|o| match *o {
                TowerOrder::Attack { tower, target } => {
                    (tower_key(tower), "attack", self.creep_key(target))
                }
                TowerOrder::Heal { tower, target } => {
                    (tower_key(tower), "heal", self.creep_key(target))
                }
            })
            .collect()
    }
}

/// P7 gate scenario: a melee attacker (unboosted ATTACK and MOVE) against one tower. The tower fires
/// on every tick until it dies, after the ticks its hits take at the tower's range, and logs one
/// `Fire` event.
#[test]
fn scenario_melee_attacker_against_one_tower() {
    let parts = body(&[(Part::Attack, 10), (Part::Move, 10)]);
    let hits = full(&parts);
    let t = tower(100, 10, 10);
    let range = 12u32;
    let damage = falloff(TOWER_POWER_ATTACK, range);
    let want = hits.div_ceil(damage);
    let mut w = World::new();
    let mut a = foe(1, 22, 10, parts);
    for tick in 0..want {
        w.tick(
            1000 + tick,
            std::slice::from_ref(&t),
            std::slice::from_ref(&a),
            &[],
        );
        assert_eq!(w.orders(), [(100, "attack", 1)], "tick {tick}");
        let fires = w
            .control
            .events()
            .iter()
            .filter(|e| matches!(e, TowerEvent::Fire { .. }))
            .count();
        assert_eq!(
            fires,
            usize::from(tick == 0),
            "one Fire event, on the first tick"
        );
        if tick == 0 {
            let TowerEvent::Fire {
                ticks,
                damage: d,
                heal,
                towers,
                ..
            } = w.control.events()[0]
            else {
                panic!("a Fire event");
            };
            assert_eq!((ticks, d, heal, towers), (want, damage, 0, 1));
        }
        a.hits = a.hits.saturating_sub(damage);
    }
    assert_eq!(a.hits, 0, "dead after the predicted ticks");
}

/// P7 gate scenario: a ranged attacker against two towers at different ranges: both fire at it, for
/// their summed damage.
#[test]
fn scenario_ranged_attacker_against_two_towers() {
    let parts = body(&[(Part::RangedAttack, 15), (Part::Move, 15)]);
    let hits = full(&parts);
    let towers = [tower(100, 5, 5), tower(101, 40, 40)];
    let pos = (25u8, 25u8);
    let damage = falloff(TOWER_POWER_ATTACK, 20) + falloff(TOWER_POWER_ATTACK, 15);
    let mut w = World::new();
    w.tick(1000, &towers, &[foe(1, pos.0, pos.1, parts)], &[]);
    let mut orders = w.orders();
    orders.sort_unstable();
    assert_eq!(orders, [(100, "attack", 1), (101, "attack", 1)]);
    let TowerEvent::Fire {
        ticks, damage: d, ..
    } = w.control.events()[0]
    else {
        panic!("a Fire event");
    };
    assert_eq!((d, ticks), (damage, hits.div_ceil(damage)));
}

/// P7 gate scenario: a healer duo (unboosted HEAL and MOVE, next to each other) heals each target with
/// both its HEAL parts and its partner's. One tower far away cannot beat that heal: it holds fire,
/// with one `Hold` event and no order. Three towers close in kill one in the ticks the engine's
/// order gives, healed all the while.
#[test]
fn scenario_healer_duo_against_the_towers() {
    let heal_parts = 10u32;
    let parts = body(&[(Part::Heal, heal_parts), (Part::Move, heal_parts)]);
    let hits = full(&parts);
    let heal = 2 * heal_parts * HEAL_POWER;
    let (a, b) = (foe(1, 30, 30, parts.clone()), foe(2, 31, 30, parts));
    // A healer is not dangerous: add an attacker beside them, out of their heal (range 4), so the
    // room has a dangerous hostile and holding fire is logged.
    let striker = foe(3, 35, 30, body(&[(Part::Attack, 1), (Part::Move, 1)]));
    let far = [tower(100, 10, 30)];
    let damage_far = falloff(TOWER_POWER_ATTACK, 20);
    assert!(
        damage_far <= heal,
        "the scenario needs heal to match one far tower"
    );
    // Every event is checked here, so no rate limit (`fire_lines_are_rate_limited` covers it).
    let mut w = World::new();
    w.cfg.threat_log_ticks = 1;
    w.tick(1000, &far, &[a.clone(), b.clone()], &[]);
    assert!(w.orders().is_empty(), "no shot into heal it cannot beat");
    // The striker alone is killable: the tower shoots it, not the healers.
    w.tick(1001, &far, &[a.clone(), b.clone(), striker.clone()], &[]);
    assert_eq!(w.orders(), [(100, "attack", 3)]);
    // Without a killable target and with a dangerous one held: one Hold event, then none.
    let mut tough_striker = striker.clone();
    tough_striker.pos = at(31, 31); // next to both healers: their heal covers it too
    w.tick(
        1002,
        &far,
        &[a.clone(), b.clone(), tough_striker.clone()],
        &[],
    );
    let striker_heal = heal_on(
        at(31, 31),
        &[
            (
                a.pos,
                Power {
                    heal: heal_parts,
                    ..Power::default()
                },
            ),
            (
                b.pos,
                Power {
                    heal: heal_parts,
                    ..Power::default()
                },
            ),
        ],
    );
    assert!(damage_far <= striker_heal);
    assert!(w.orders().is_empty());
    assert_eq!(
        w.control.events(),
        [TowerEvent::Hold {
            room: room(),
            dangerous: 1
        }]
    );
    w.tick(1003, &far, &[a.clone(), b.clone(), tough_striker], &[]);
    assert!(w.control.events().is_empty(), "the hold is logged once");

    // Three towers within the optimal range: damage beats the heal, and the duo's first dies in
    // the ticks the engine's tick order gives.
    let near = [tower(100, 28, 28), tower(101, 32, 28), tower(102, 30, 33)];
    let damage = 3 * TOWER_POWER_ATTACK;
    let want = unboosted_ticks(hits, hits, damage, heal, DEFENSE_MMO.max_kill_ticks).expect("dies");
    let mut w = World::new();
    w.tick(1000, &near, &[a.clone(), b.clone()], &[]);
    let TowerEvent::Fire {
        ticks,
        heal: h,
        damage: d,
        towers,
        ..
    } = w.control.events()[0]
    else {
        panic!("a Fire event");
    };
    assert_eq!((ticks, h, d, towers), (want, heal, damage, 3));
    let targets: Vec<u128> = w.orders().iter().map(|o| o.2).collect();
    assert!(
        targets.iter().all(|&t| t == targets[0]),
        "focus fire: one target"
    );
}

/// Dangerous hostiles come before harmless ones, and with the same ticks the one with more heal;
/// last tick's target is kept while it still dies within the limit.
#[test]
fn focus_prefers_danger_then_speed_then_heal_and_keeps_its_target() {
    let t = [tower(100, 25, 25)];
    let scout = foe(1, 26, 25, body(&[(Part::Move, 1)]));
    let attacker = foe(2, 24, 25, body(&[(Part::Attack, 5), (Part::Move, 5)]));
    let mut w = World::new();
    w.tick(1000, &t, &[scout.clone(), attacker.clone()], &[]);
    assert_eq!(
        w.orders(),
        [(100, "attack", 2)],
        "the attacker, though the scout dies sooner"
    );
    // Two attackers of one size, one with a HEAL part: the healer first.
    let plain_a = foe(3, 25, 24, body(&[(Part::Attack, 2), (Part::Move, 3)]));
    let healing_a = foe(
        4,
        25,
        26,
        body(&[(Part::Attack, 2), (Part::Heal, 1), (Part::Move, 2)]),
    );
    let mut w = World::new();
    w.tick(1000, &t, &[plain_a.clone(), healing_a.clone()], &[]);
    assert_eq!(w.orders(), [(100, "attack", 4)]);
    // Kept: a new, quicker kill does not take the towers off the target.
    let mut big = foe(5, 25, 20, body(&[(Part::Attack, 10), (Part::Move, 10)]));
    w = World::new();
    // Every event is checked here, so no rate limit (`fire_lines_are_rate_limited` covers it).
    w.cfg.threat_log_ticks = 1;
    w.tick(1000, &t, std::slice::from_ref(&big), &[]);
    assert_eq!(w.orders(), [(100, "attack", 5)]);
    big.hits -= TOWER_POWER_ATTACK;
    let quick = foe(6, 26, 26, body(&[(Part::Attack, 1), (Part::Move, 1)]));
    w.tick(1001, &t, &[big.clone(), quick.clone()], &[]);
    assert_eq!(w.orders(), [(100, "attack", 5)], "the target is kept");
    assert!(w.control.events().is_empty(), "no new Fire event");
    // Gone: the next one is taken, with a Fire event.
    w.tick(1002, &t, std::slice::from_ref(&quick), &[]);
    assert_eq!(w.orders(), [(100, "attack", 6)]);
    assert_eq!(w.control.events().len(), 1);
}

/// With nothing to shoot, towers holding `heal_reserve` heal the most damaged own creeps, each
/// tower the creep with the most hits still missing; a tower below it does not, and an empty tower
/// gets no order at all.
#[test]
fn heal_priority_spreads_towers_over_the_damaged() {
    let max = 10 * BODYPART_HITS;
    let owns = [
        Own {
            key: 10,
            pos: at(20, 20),
            hits: max - 3 * TOWER_POWER_HEAL / 2,
            max,
        },
        Own {
            key: 11,
            pos: at(22, 20),
            hits: max - TOWER_POWER_HEAL / 2,
            max,
        },
        Own {
            key: 12,
            pos: at(24, 20),
            hits: max,
            max,
        },
    ];
    let mut low = tower(102, 21, 22);
    low.energy = DEFENSE_MMO.heal_reserve - 1;
    let mut empty = tower(103, 21, 23);
    empty.energy = TOWER_ENERGY_COST - 1;
    let towers = [tower(100, 21, 21), tower(101, 21, 19), low, empty];
    let mut w = World::new();
    w.tick(1000, &towers, &[], &owns);
    let mut orders = w.orders();
    orders.sort_unstable();
    // Creep 10 misses 1.5 heals: the first tower takes it, then it still misses half a heal, as
    // much as creep 11; the tie goes to the lower id (10), and 11 is left for no tower.
    assert_eq!(orders, [(100, "heal", 10), (101, "heal", 10)]);
    // With heal reserve 0 the low tower heals too (11), and the empty one never acts.
    w.cfg.heal_reserve = 0;
    w.tick(1001, &towers, &[], &owns);
    let mut orders = w.orders();
    orders.sort_unstable();
    assert_eq!(
        orders,
        [(100, "heal", 10), (101, "heal", 10), (102, "heal", 11)]
    );
    // Nothing damaged and no hostile: the room is not planned.
    let whole = [Own {
        hits: max,
        ..owns[0].clone()
    }];
    w.tick(1002, &towers, &[], &whole);
    assert_eq!(w.control.unit_count(), 0);
    assert!(w.orders().is_empty());
}

/// The same snapshot gives the same orders and events, whatever happened before (the state aside).
#[test]
fn plans_are_deterministic() {
    let towers = [tower(100, 10, 10), tower(101, 40, 40), tower(102, 10, 40)];
    let foes = [
        foe(
            1,
            25,
            25,
            body(&[(Part::Attack, 4), (Part::Heal, 2), (Part::Move, 6)]),
        ),
        foe(2, 26, 25, body(&[(Part::Heal, 6), (Part::Move, 6)])),
        foe(3, 30, 30, body(&[(Part::RangedAttack, 4), (Part::Move, 4)])),
    ];
    let run = || {
        let mut w = World::new();
        w.tick(1000, &towers, &foes, &[]);
        (w.orders(), w.control.events().to_vec())
    };
    assert_eq!(run(), run());
}

/// Only the fewest towers that kill in the ticks all of them would take fire, strongest at the
/// target first; a spare tower heals a damaged own creep meanwhile.
#[test]
fn the_fewest_towers_fire_and_the_rest_heal() {
    let max = 10 * BODYPART_HITS;
    let hurt = Own {
        key: 10,
        pos: at(20, 20),
        hits: max - TOWER_POWER_HEAL,
        max,
    };
    // Three towers at ranges 1, 10 and 20 from a scout of one MOVE part: the nearest kills it
    // alone in one tick, as all three would.
    let towers = [tower(100, 31, 30), tower(101, 30, 40), tower(102, 30, 10)];
    let scout = foe(1, 30, 30, body(&[(Part::Move, 1)]));
    assert!(
        falloff(TOWER_POWER_ATTACK, 20) >= scout.hits,
        "any one tower kills it"
    );
    let mut w = World::new();
    w.tick(1000, &towers, std::slice::from_ref(&scout), &[]);
    assert_eq!(w.orders(), [(100, "attack", 1)]);
    let TowerEvent::Fire {
        towers: n,
        damage,
        ticks,
        ..
    } = w.control.events()[0]
    else {
        panic!("a Fire event");
    };
    assert_eq!((n, damage, ticks), (1, TOWER_POWER_ATTACK, 1));
    // With a damaged own creep, the spare towers heal it.
    let mut w = World::new();
    w.tick(
        1000,
        &towers,
        std::slice::from_ref(&scout),
        std::slice::from_ref(&hurt),
    );
    let mut orders = w.orders();
    orders.sort_unstable();
    assert_eq!(orders[0], (100, "attack", 1));
    assert!(
        orders[1..].iter().all(|o| o.1 == "heal" && o.2 == 10),
        "{orders:?}"
    );
    assert!(!orders[1..].is_empty());
    // A creep of 2000 hits at range 5 from two towers and 20 from a third: the two near ones kill it
    // in the ticks all three take, `ceil(hits / (2 × TOWER_POWER_ATTACK))`; the far one does not
    // shorten it.
    let big = foe(2, 30, 30, body(&[(Part::Attack, 10), (Part::Move, 10)]));
    let towers = [tower(100, 35, 30), tower(101, 25, 30), tower(102, 30, 10)];
    let all = 2 * TOWER_POWER_ATTACK + falloff(TOWER_POWER_ATTACK, 20);
    let two = 2 * TOWER_POWER_ATTACK;
    assert_eq!(
        big.hits.div_ceil(all),
        big.hits.div_ceil(two),
        "the scenario's premise"
    );
    let mut w = World::new();
    w.tick(1000, &towers, std::slice::from_ref(&big), &[]);
    let mut orders = w.orders();
    orders.sort_unstable();
    assert_eq!(orders, [(100, "attack", 2), (101, "attack", 2)]);
}

/// A new target is logged at most once per `threat_log_ticks` per room, but the first dangerous
/// one after a harmless one always is; a hostile stepping out and back within the window, with its
/// threat still open, logs no second line.
#[test]
fn fire_lines_are_rate_limited() {
    let t = [tower(100, 25, 25)];
    let scout = foe(1, 26, 25, body(&[(Part::Move, 1)]));
    let other_scout = foe(2, 24, 25, body(&[(Part::Move, 1)]));
    let attacker = foe(3, 25, 24, body(&[(Part::Attack, 5), (Part::Move, 5)]));
    let fires = |w: &World| {
        w.control
            .events()
            .iter()
            .filter(|e| matches!(e, TowerEvent::Fire { .. }))
            .count()
    };
    let mut w = World::new();
    w.tick(1000, &t, std::slice::from_ref(&scout), &[]);
    assert_eq!(fires(&w), 1);
    // A second harmless target inside the window: shot, not logged.
    w.tick(1001, &t, std::slice::from_ref(&other_scout), &[]);
    assert_eq!(w.orders(), [(100, "attack", 2)]);
    assert_eq!(fires(&w), 0);
    // The first dangerous one: logged at once.
    w.tick(1002, &t, std::slice::from_ref(&attacker), &[]);
    assert_eq!(fires(&w), 1);
    // It steps out for a tick and back (a new handle, the threat open): no second line.
    w.tick(1003, &t, &[], &[]);
    w.tick(1004, &t, std::slice::from_ref(&attacker), &[]);
    assert_eq!(w.orders(), [(100, "attack", 3)]);
    assert_eq!(fires(&w), 0, "inside the window");
    // After the window, a new target is logged again.
    let later = 1004 + w.cfg.threat_log_ticks;
    w.tick(later, &t, std::slice::from_ref(&scout), &[]);
    assert_eq!(fires(&w), 1);
}

/// An own creep's `hitsMax` is kept for its handle once read; its hits rows are listed in order.
#[test]
fn own_hits_max_is_kept_by_handle() {
    let mut snap = Snapshot::default();
    let row = CreepRow {
        pos: at(10, 10),
        my: true,
        spawning: false,
        fatigue: 0,
    };
    for (time, read) in [(1u32, true), (2, false)] {
        snap.begin(TickContext::from_raw(time, 20, 500.0, 10_000));
        snap.creeps.push(ObjectKey(7), row).expect("own");
        if read {
            assert_eq!(snap.creeps.known_hits_max(0), None);
            snap.creeps.set_own_hits(0, 50, 300);
        } else {
            assert_eq!(snap.creeps.known_hits_max(0), Some(300));
            snap.creeps.set_own_hits(0, 60, 300);
            // Set twice, or a row out of order: nothing changes.
            snap.creeps.set_own_hits(0, 1, 1);
            assert_eq!(snap.creeps.hits()[0], 60);
        }
        assert_eq!(snap.creeps.own_hits_rows(), [0]);
        snap.finish();
    }
}

/// The sim against an oracle that plays every tick to the limit with no early exit (V287's order,
/// through `damage_taken`, itself pinned to the engine's formula), over random boosted bodies,
/// hits, towers and heal; more hits never kill sooner.
#[test]
fn kill_ticks_agrees_with_brute_force() {
    let toughs = [
        ResourceType::GhodiumOxide,
        ResourceType::GhodiumAlkalide,
        ResourceType::CatalyzedGhodiumAlkalide,
    ];
    let mut state = 0x7041_2002u64;
    let mut next = |n: u32| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        u32::try_from(state >> 33).expect("31 bits") % n
    };
    let oracle =
        |parts: &[BodyPartRead], hits: u32, max: u32, towers: &[Shot], heal: u32, limit: u32| {
            let mut h = i64::from(hits);
            let mut dead = None;
            for t in 1..=limit {
                let d: u32 = towers
                    .iter()
                    .filter(|s| s.shots >= t)
                    .map(|s| s.damage)
                    .sum();
                let hh = u32::try_from(h.max(0)).expect("fits");
                let after = h - i64::from(super::damage_taken(parts, hh, d)) + i64::from(heal);
                if after <= 0 && dead.is_none() {
                    dead = Some(t);
                }
                h = after.min(i64::from(max));
                if dead.is_some() {
                    break;
                }
            }
            dead
        };
    for case in 0..400 {
        let len = 1 + usize::try_from(next(50)).expect("small");
        let parts: Vec<BodyPartRead> = (0..len)
            .map(|_| match next(4) {
                0 => BodyPartRead {
                    part: Part::Tough,
                    boost: Some(toughs[usize::try_from(next(3)).expect("small")]),
                },
                1 => plain(Part::Heal),
                2 => plain(Part::Attack),
                _ => plain(Part::Move),
            })
            .collect();
        let max = full(&parts);
        let hits = 1 + next(max);
        let towers: Vec<Shot> = (0..1 + next(6))
            .map(|_| Shot {
                damage: tower_amount(TOWER_POWER_ATTACK, next(25)),
                shots: 1 + next(60),
            })
            .collect();
        let heal = next(3 * TOWER_POWER_ATTACK);
        let limit = DEFENSE_MMO.max_kill_ticks;
        let got = kill_ticks(&parts, hits, max, &towers, heal, limit);
        assert_eq!(
            got,
            oracle(&parts, hits, max, &towers, heal, limit),
            "case {case}"
        );
        if hits < max {
            let more = kill_ticks(&parts, hits + 1, max, &towers, heal, limit);
            if let (Some(a), Some(b)) = (got, more) {
                assert!(b >= a, "case {case}: one more hit killed sooner");
            }
            assert!(got.is_some() || more.is_none(), "case {case}");
        }
    }
}

/// A boosted pair: an attacker behind XGHO2 TOUGH and a healer of XLHO2 HEAL parts next to it.
/// The towers take the healer first when its kill and then the attacker's, without its heal, take
/// fewer ticks than the attacker's through it, as the sim gives both.
#[test]
fn scenario_boosted_pair_healer_first() {
    let tough = |n: u32| {
        (0..n)
            .map(|_| BodyPartRead {
                part: Part::Tough,
                boost: Some(ResourceType::CatalyzedGhodiumAlkalide),
            })
            .collect::<Vec<_>>()
    };
    let mut attacker_parts = tough(10);
    attacker_parts.extend(body(&[(Part::Attack, 10), (Part::Move, 10)]));
    let healer_parts: Vec<BodyPartRead> = (0..10)
        .map(|_| BodyPartRead {
            part: Part::Heal,
            boost: Some(ResourceType::CatalyzedLemergiumAlkalide),
        })
        .chain(body(&[(Part::Move, 10)]))
        .collect();
    let attacker = foe(1, 30, 30, attacker_parts.clone());
    let healer = foe(2, 31, 30, healer_parts);
    let towers = [tower(100, 28, 28), tower(101, 33, 28), tower(102, 30, 33)];
    let mut w = World::new();
    w.tick(1000, &towers, &[attacker.clone(), healer.clone()], &[]);
    let targets: Vec<u128> = w.orders().iter().map(|o| o.2).collect();
    // The rule's premise, from the sim itself on each order of kills.
    let damage = 3 * TOWER_POWER_ATTACK;
    let shot = Shot {
        damage: TOWER_POWER_ATTACK,
        shots: TOWER_CAPACITY / TOWER_ENERGY_COST,
    };
    let Some(Boost::Heal(m)) = ResourceType::CatalyzedLemergiumAlkalide.boost() else {
        panic!("XLHO2 boosts heal");
    };
    let heal = 10 * m * HEAL_POWER;
    let limit = DEFENSE_MMO.max_kill_ticks;
    let through = kill_ticks(
        &attacker.parts,
        attacker.hits,
        attacker.hits,
        &[shot; 3],
        heal,
        limit,
    );
    let healer_first = kill_ticks(
        &healer.parts,
        healer.hits,
        healer.hits,
        &[shot; 3],
        heal,
        limit,
    )
    .zip(kill_ticks(
        &attacker.parts,
        attacker.hits,
        attacker.hits,
        &[shot; 3],
        0,
        limit,
    ))
    .map(|(a, b)| a + b);
    assert!(damage > 0);
    match (through, healer_first) {
        (Some(t), Some(h)) if h < t => assert!(targets.iter().all(|&k| k == 2), "{targets:?}"),
        (None, Some(_)) => assert!(targets.iter().all(|&k| k == 2), "{targets:?}"),
        _ => assert!(targets.iter().all(|&k| k == 1), "{targets:?}"),
    }
    assert!(!targets.is_empty(), "the pair is killable");
}

/// A tower with k shots does not start on a target that needs k + 1; with k + 1 it does, for
/// those ticks.
#[test]
fn scenario_shots_run_out() {
    let parts = body(&[(Part::Attack, 6), (Part::Move, 6)]);
    let hits = full(&parts);
    let need = hits.div_ceil(TOWER_POWER_ATTACK);
    let mut short = tower(100, 25, 25);
    short.energy = (need - 1) * TOWER_ENERGY_COST;
    let mut w = World::new();
    w.tick(
        1000,
        std::slice::from_ref(&short),
        &[foe(1, 26, 26, parts.clone())],
        &[],
    );
    assert!(w.orders().is_empty());
    assert!(matches!(w.control.events(), [TowerEvent::Hold { .. }]));
    short.energy = need * TOWER_ENERGY_COST;
    let mut w = World::new();
    w.tick(
        1000,
        std::slice::from_ref(&short),
        &[foe(1, 26, 26, parts)],
        &[],
    );
    let TowerEvent::Fire { ticks, .. } = w.control.events()[0] else {
        panic!("a Fire event");
    };
    assert_eq!(ticks, need);
}

/// A healer at range 3 adds its `rangedHeal`, at range 4 nothing, as the Fire event shows; an empty
/// tower is left out of the fire and its event.
#[test]
fn ranged_heal_and_empty_towers_in_the_fire_event() {
    let heal_parts = 4u32;
    let target = foe(1, 25, 25, body(&[(Part::Attack, 8), (Part::Move, 8)]));
    let healer = |x| {
        foe(
            2,
            x,
            25,
            body(&[(Part::Heal, heal_parts), (Part::Move, heal_parts)]),
        )
    };
    let mut empty = tower(101, 20, 20);
    empty.energy = 0;
    let towers = [tower(100, 25, 20), empty];
    for (x, want) in [(28u8, heal_parts * RANGED_HEAL_POWER), (29, 0)] {
        let mut w = World::new();
        w.tick(1000, &towers, &[target.clone(), healer(x)], &[]);
        let TowerEvent::Fire {
            heal,
            towers: n,
            damage,
            ..
        } = w.control.events()[0]
        else {
            panic!("a Fire event");
        };
        assert_eq!(
            (heal, n, damage),
            (want, 1, TOWER_POWER_ATTACK),
            "healer at x {x}"
        );
        assert!(w.orders().iter().all(|o| o.0 == 100));
    }
}

/// A kept target that a healer then covers beyond the towers' damage is dropped: the towers hold.
#[test]
fn a_kept_target_is_dropped_once_it_cannot_die() {
    let t = [tower(100, 10, 25)];
    let target = foe(1, 30, 25, body(&[(Part::Attack, 5), (Part::Move, 5)]));
    let far = falloff(TOWER_POWER_ATTACK, 20);
    let heal_parts = far.div_ceil(HEAL_POWER);
    let healer = foe(
        2,
        31,
        25,
        body(&[(Part::Heal, heal_parts), (Part::Move, 1)]),
    );
    let mut w = World::new();
    w.cfg.threat_log_ticks = 1;
    w.tick(1000, &t, std::slice::from_ref(&target), &[]);
    assert_eq!(w.orders(), [(100, "attack", 1)]);
    w.tick(1001, &t, &[target.clone(), healer], &[]);
    assert!(w.orders().iter().all(|o| o.2 != 1), "{:?}", w.orders());
}

/// A tower at `TOWER_FALLOFF_RANGE` or more heals `floor(TOWER_POWER_HEAL × (1 − TOWER_FALLOFF))`,
/// so a second tower goes to the creep still missing more.
#[test]
fn heal_priority_at_falloff_range() {
    let max = 10 * BODYPART_HITS;
    let far_heal = falloff(TOWER_POWER_HEAL, u32::from(TOWER_FALLOFF_RANGE));
    let owns = [
        Own {
            key: 10,
            pos: at(5, 5),
            hits: max - far_heal - 1,
            max,
        },
        Own {
            key: 11,
            pos: at(5, 7),
            hits: max - far_heal,
            max,
        },
    ];
    // The first tower is far from both: creep 10 then misses 1, so the second goes to creep 11.
    let towers = [tower(100, 30, 30), tower(101, 6, 6)];
    let mut w = World::new();
    w.tick(1000, &towers, &[], &owns);
    let mut orders = w.orders();
    orders.sort_unstable();
    assert_eq!(orders, [(100, "heal", 10), (101, "heal", 11)]);
}

/// The shot check: a target whose hits do not fall as the sim expects (a rampart takes the shots)
/// is given up on after `miss_ticks` ticks in a row, with one `Distrust` event, and not shot again.
#[test]
fn the_shot_check_gives_up_on_a_target_that_does_not_fall() {
    let t = [tower(100, 25, 25)];
    let target = foe(1, 26, 25, body(&[(Part::Attack, 20), (Part::Move, 20)]));
    let mut w = World::new();
    let miss = w.cfg.miss_ticks;
    for k in 0..miss {
        // Its hits stay full: the shots land elsewhere.
        w.tick(1000 + k, &t, std::slice::from_ref(&target), &[]);
        assert_eq!(w.orders(), [(100, "attack", 1)], "tick {k}");
    }
    w.tick(1000 + miss, &t, std::slice::from_ref(&target), &[]);
    assert!(
        w.control
            .events()
            .iter()
            .any(|e| matches!(e, TowerEvent::Distrust { misses, .. } if *misses == miss))
    );
    assert!(w.orders().is_empty(), "{:?}", w.orders());
    w.tick(1001 + miss, &t, std::slice::from_ref(&target), &[]);
    assert!(w.orders().is_empty());
    // A target whose hits fall as expected is never given up on.
    let mut w = World::new();
    let mut fine = target.clone();
    for k in 0..miss + 2 {
        w.tick(2000 + k, &t, std::slice::from_ref(&fine), &[]);
        assert_eq!(w.orders(), [(100, "attack", 1)]);
        fine.hits -= TOWER_POWER_ATTACK;
    }
}

/// A one-tick kill leaves the spare towers for the other one-tick kills: two scouts, three towers,
/// one tower each; the third has nothing to do.
#[test]
fn spare_towers_take_other_one_tick_kills() {
    let towers = [tower(100, 25, 25), tower(101, 25, 30), tower(102, 25, 35)];
    let a = foe(1, 26, 25, body(&[(Part::Move, 1)]));
    let b = foe(2, 26, 30, body(&[(Part::Move, 1)]));
    let mut w = World::new();
    w.tick(1000, &towers, &[a, b], &[]);
    let mut targets: Vec<u128> = w.orders().iter().map(|o| o.2).collect();
    targets.sort_unstable();
    assert_eq!(targets, [1, 2], "{:?}", w.orders());
}
