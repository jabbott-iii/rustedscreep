//! Threat assessment tests (P7.1). Expected values come from the constants (`BODYPART_HITS`, the
//! powers, the `BOOSTS` table through `ResourceType::boost`) and from the engine rules the module
//! cites (V283 for the part hits, V286 to V288 for power and damage).

use rustedscreep_consts::extra::BODYPART_HITS;
use rustedscreep_consts::{
    ATTACK_POWER, Boost, DISMANTLE_POWER, HEAL_POWER, MAX_CREEP_SIZE, Part, Position,
    RANGED_ATTACK_POWER, RANGED_HEAL_POWER, RESOURCES_ALL, ResourceType, RoomCoordinate, RoomName,
    StructureType,
};

use super::threat::{
    HostileThreat, Power, ThreatEvent, Threats, damage_taken, effective_hits, part_hits,
};
use crate::config::DEFENSE_MMO;
use crate::snapshot::{
    BodyPartRead, BodyRead, Controller, CreepRow, ObjectKey, Snapshot, TickContext,
};

/// The engine's eight part types (V131: the same set as the server's `BODYPARTS_ALL`).
const PARTS: [Part; 8] = [
    Part::Move,
    Part::Work,
    Part::Carry,
    Part::Attack,
    Part::RangedAttack,
    Part::Tough,
    Part::Heal,
    Part::Claim,
];

fn plain(part: Part) -> BodyPartRead {
    BodyPartRead { part, boost: None }
}

fn boosted(part: Part, boost: ResourceType) -> BodyPartRead {
    BodyPartRead {
        part,
        boost: Some(boost),
    }
}

fn len_u32(parts: &[BodyPartRead]) -> u32 {
    u32::try_from(parts.len()).expect("small")
}

fn full(parts: &[BodyPartRead]) -> u32 {
    len_u32(parts) * BODYPART_HITS
}

/// The compounds the `BOOSTS` table lists, with their effect.
fn compounds() -> impl Iterator<Item = (ResourceType, Boost)> {
    RESOURCES_ALL
        .iter()
        .filter_map(|&r| r.boost().map(|b| (r, b)))
}

/// The TOUGH compounds with their `damage` ratio, as the engine's double (hundredths over 100).
fn tough_compounds() -> Vec<(ResourceType, f64)> {
    compounds()
        .filter_map(|(r, b)| match b {
            Boost::Tough(f) => Some((r, f64::from((f * 100.0).round()) / 100.0)),
            _ => None,
        })
        .collect()
}

/// A deterministic generator for the property tests.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        u32::try_from(self.0 >> 33).expect("31 bits")
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// V283: the engine fills the parts from the last to the first, up to `BODYPART_HITS` each, so the
/// parts' hits sum to the total, never fall from front to back, and the live parts are the last
/// `ceil(hits / BODYPART_HITS)`: what `getActiveBodyparts` counts walking back from the last part to
/// the first dead one (V269).
#[test]
fn part_hits_fill_from_the_last_part() {
    let max = usize::try_from(MAX_CREEP_SIZE).expect("small");
    for len in 1..=max {
        let cap = u32::try_from(len).expect("small") * BODYPART_HITS;
        for hits in (0..=cap)
            .step_by(7)
            .chain([cap, 1, BODYPART_HITS - 1, BODYPART_HITS])
        {
            let hits = hits.min(cap);
            let each: Vec<u32> = (0..len).map(|i| part_hits(len, hits, i)).collect();
            assert_eq!(each.iter().sum::<u32>(), hits, "{len} parts, {hits} hits");
            assert!(each.iter().all(|&h| h <= BODYPART_HITS));
            assert!(each.windows(2).all(|w| w[0] <= w[1]), "{each:?}");
            let live = each.iter().filter(|&&h| h > 0).count();
            assert_eq!(
                u32::try_from(live).expect("small"),
                hits.div_ceil(BODYPART_HITS),
                "{len} parts, {hits} hits"
            );
            let walked_back = each.iter().rev().take_while(|&&h| h > 0).count();
            assert_eq!(walked_back, live, "the live parts are the last ones");
        }
    }
}

/// Each action's power is its live parts' count at the action's base power (V286, V288), and dead
/// front parts count for nothing.
#[test]
fn power_counts_live_parts_at_base_power() {
    let mut parts = vec![plain(Part::Tough)];
    let counts = [
        (Part::Attack, 2u32),
        (Part::RangedAttack, 3),
        (Part::Heal, 4),
        (Part::Work, 5),
        (Part::Claim, 1),
        (Part::Carry, 1),
        (Part::Move, 6),
    ];
    for (part, n) in counts {
        parts.extend((0..n).map(|_| plain(part)));
    }
    let body = BodyRead::of(&parts).expect("fits");
    let len = parts.len();
    for dead in 0..=len {
        let hits = u32::try_from(len - dead).expect("small") * BODYPART_HITS;
        let live = &parts[dead..];
        let count =
            |p: Part| u32::try_from(live.iter().filter(|x| x.part == p).count()).expect("small");
        let t = HostileThreat::of(0, &body, hits);
        let p = t.power;
        assert_eq!(
            p.melee_damage(),
            count(Part::Attack) * ATTACK_POWER,
            "{dead} dead"
        );
        assert_eq!(
            p.ranged_damage(),
            count(Part::RangedAttack) * RANGED_ATTACK_POWER
        );
        assert_eq!(p.heal_adjacent(), count(Part::Heal) * HEAL_POWER);
        assert_eq!(p.heal_at_range(), count(Part::Heal) * RANGED_HEAL_POWER);
        assert_eq!(p.dismantle_damage(), count(Part::Work) * DISMANTLE_POWER);
        assert_eq!(p.claim, count(Part::Claim));
        assert_eq!(
            p.armed(),
            count(Part::Attack) + count(Part::RangedAttack) > 0
        );
        assert_eq!(
            p.dangerous(),
            p.armed() || count(Part::Work) + count(Part::Claim) > 0
        );
        assert_eq!(t.hits_max, full(&parts));
        assert!(!t.boosted);
        // No boosted TOUGH part: the creep's hits are its effective hits.
        assert_eq!(t.effective_hits, hits);
    }
}

/// Every compound of the `BOOSTS` table on every part type: a part counts the compound's multiplier
/// only for the action the table lists it under, and 1 otherwise (`BOOSTS[type][boost][method]`,
/// V286); CLAIM never changes (V288). Any listed compound marks the creep boosted.
#[test]
fn boost_multipliers_follow_the_boosts_table() {
    let mut seen = 0;
    for (compound, boost) in compounds() {
        seen += 1;
        for part in PARTS {
            let mut p = Power::default();
            p.add_part(boosted(part, compound));
            let base = {
                let mut b = Power::default();
                b.add_part(plain(part));
                b
            };
            let expected = match (part, boost) {
                (Part::Attack, Boost::Attack(m)) => Power { attack: m, ..base },
                (Part::RangedAttack, Boost::RangedAttack(m)) => Power { ranged: m, ..base },
                (Part::Heal, Boost::Heal(m)) => Power { heal: m, ..base },
                (Part::Work, Boost::Dismantle(m)) => Power {
                    dismantle: m,
                    ..base
                },
                _ => base,
            };
            assert_eq!(p, expected, "{part:?} with {compound:?}");
            let body = BodyRead::of(&[boosted(part, compound)]).expect("fits");
            assert!(HostileThreat::of(0, &body, BODYPART_HITS).boosted);
        }
    }
    // V131: the table has 30 compounds.
    assert_eq!(seen, 30);
    // An unboosted body of every type counts one of each action's part.
    let mut all = Power::default();
    for part in PARTS {
        all.add_part(plain(part));
    }
    assert_eq!(
        all,
        Power {
            attack: 1,
            ranged: 1,
            heal: 1,
            dismantle: 1,
            claim: 1
        }
    );
}

/// `rangedMassAttack` (V288, engine rangedMassAttack.js L32 and L53, pinned): the boosted power times
/// `{0: 1, 1: 1, 2: 0.4, 3: 0.1}[range]`, rounded once; the per-part constants give the same.
#[test]
fn mass_damage_by_range_matches_the_engine_rates() {
    let rates = [(0u32, 1.0f64), (1, 1.0), (2, 0.4), (3, 0.1)];
    for ranged in 0..=MAX_CREEP_SIZE * 4 {
        let p = Power {
            ranged,
            ..Power::default()
        };
        for (range, rate) in rates {
            let engine = (f64::from(ranged * RANGED_ATTACK_POWER) * rate).round();
            assert_eq!(
                f64::from(p.mass_damage(range)),
                engine,
                "{ranged} at {range}"
            );
        }
        assert_eq!(p.mass_damage(4), 0);
        assert_eq!(p.mass_damage(1), p.ranged_damage());
    }
}

/// With no boosted TOUGH part the whole damage comes off (V279, V287), so the effective hits are the
/// hits, even with other parts boosted.
#[test]
fn unboosted_toughness_takes_all_damage() {
    let parts = [
        plain(Part::Tough),
        boosted(Part::Attack, ResourceType::CatalyzedUtriumAcid),
        boosted(Part::Move, ResourceType::CatalyzedZynthiumAlkalide),
        plain(Part::Move),
    ];
    let cap = full(&parts);
    for hits in 1..=cap {
        assert_eq!(effective_hits(&parts, hits), hits);
        for damage in [0, 1, hits, cap, cap * 3] {
            assert_eq!(damage_taken(&parts, hits, damage), damage);
        }
    }
}

/// A whole body of `t` boosted TOUGH parts in front of unboosted ones: all of the saving lands once
/// the TOUGH parts are spent, so the least killing damage is the hits plus the rounded saving,
/// `t * BODYPART_HITS / ratio * (1 - ratio)` (V287).
#[test]
fn whole_boosted_tough_front_adds_its_saving() {
    for (compound, ratio) in tough_compounds() {
        for len in [2usize, 5, 10, 25, 50] {
            let mut last = 0;
            for t in 0..len {
                let mut parts = vec![boosted(Part::Tough, compound); t];
                parts.extend(vec![plain(Part::Move); len - t]);
                let hits = full(&parts);
                let per = f64::from(BODYPART_HITS) / ratio * (1.0 - ratio);
                let saving = (f64::from(u32::try_from(t).expect("small")) * per).round();
                let want = hits + saving as u32;
                let got = effective_hits(&parts, hits);
                assert_eq!(got, want, "{compound:?}, {t} of {len}");
                assert!(damage_taken(&parts, hits, got) >= hits);
                assert!(damage_taken(&parts, hits, got - 1) < hits);
                assert!(got >= last, "more TOUGH never lowers it");
                last = got;
            }
        }
    }
}

/// Damage empties the front parts first (V283), so once a creep has lost its front TOUGH parts they
/// save nothing: a part's saving counts only its own hits left.
#[test]
fn spent_front_tough_saves_nothing() {
    for (compound, ratio) in tough_compounds() {
        let mut parts = vec![boosted(Part::Tough, compound); 2];
        parts.extend(vec![plain(Part::Move); 3]);
        let unboosted_hits = 3 * BODYPART_HITS;
        // Both TOUGH parts dead.
        assert_eq!(effective_hits(&parts, unboosted_hits), unboosted_hits);
        // The second TOUGH part has `left` hits: its saving is `left / ratio * (1 - ratio)`.
        for left in [1, BODYPART_HITS / 2, BODYPART_HITS - 1] {
            let hits = unboosted_hits + left;
            let saving = (f64::from(left) / ratio * (1.0 - ratio)).round() as u32;
            assert_eq!(
                effective_hits(&parts, hits),
                hits + saving,
                "{compound:?}, {left}"
            );
        }
    }
}

/// Over random bodies (any parts, any compounds, TOUGH ones often) and hits: the taken damage never
/// falls as the damage rises, never exceeds it, and [`effective_hits`] is the least damage that
/// kills, found by scanning up from the hits.
#[test]
fn effective_hits_is_the_least_killing_damage() {
    let all: Vec<ResourceType> = compounds().map(|(r, _)| r).collect();
    let toughs = tough_compounds();
    let mut rng = Lcg(0x5eed_7001);
    for case in 0..300 {
        let len = 1 + rng.below(MAX_CREEP_SIZE);
        let mut parts = Vec::new();
        for _ in 0..len {
            let part = PARTS[usize::try_from(rng.below(8)).expect("small")];
            let boost = match rng.below(4) {
                0 => None,
                1 => Some(all[usize::try_from(rng.below(len_of(&all))).expect("small")]),
                _ => Some(toughs[usize::try_from(rng.below(3)).expect("small")].0),
            };
            // TOUGH compounds go on TOUGH parts most of the time, as a lab would put them.
            let part = if boost.is_some_and(|b| matches!(b.boost(), Some(Boost::Tough(_))))
                && rng.below(4) > 0
            {
                Part::Tough
            } else {
                part
            };
            parts.push(BodyPartRead { part, boost });
        }
        let hits = 1 + rng.below(full(&parts));
        let ehp = effective_hits(&parts, hits);
        if hits < full(&parts) {
            assert!(
                effective_hits(&parts, hits + 1) >= ehp,
                "case {case}: one more hit never lowers it"
            );
        }
        let mut scan = hits;
        let mut before = 0;
        loop {
            let taken = damage_taken(&parts, hits, scan);
            assert!(taken >= before, "case {case}: taken fell at {scan}");
            assert!(taken <= scan, "case {case}");
            before = taken;
            if taken >= hits {
                break;
            }
            scan += 1;
        }
        assert_eq!(ehp, scan, "case {case}: {parts:?} with {hits} hits");
    }
}

fn len_of<T>(v: &[T]) -> u32 {
    u32::try_from(v.len()).expect("small")
}

/// The least damage that kills, by a scan up from the hits (the definition, for the edge cases).
fn least_kill(parts: &[BodyPartRead], hits: u32) -> u32 {
    let mut d = hits;
    while damage_taken(parts, hits, d) < hits {
        d += 1;
    }
    d
}

/// The engine's `_recalc-body.js` (V283), transliterated: from the last part to the first, each gets
/// `min(BODYPART_HITS, left)` and `left` drops by `BODYPART_HITS`.
#[test]
fn part_hits_match_recalc_body() {
    let max = usize::try_from(MAX_CREEP_SIZE).expect("small");
    for len in 1..=max {
        let cap = u32::try_from(len).expect("small") * BODYPART_HITS;
        for hits in 0..=cap {
            let mut engine = vec![0u32; len];
            let mut left = hits;
            for cell in engine.iter_mut().rev() {
                *cell = left.min(BODYPART_HITS);
                left = left.saturating_sub(BODYPART_HITS);
            }
            let ours: Vec<u32> = (0..len).map(|i| part_hits(len, hits, i)).collect();
            assert_eq!(ours, engine, "{len} parts, {hits} hits");
        }
    }
}

/// A part with 1 hit left still acts (`calcBodyEffectiveness` counts any part whose hits are
/// truthy, V286), and one at 0 does not.
#[test]
fn a_part_with_one_hit_still_acts() {
    let parts = [
        plain(Part::Attack),
        plain(Part::Heal),
        plain(Part::Move),
        plain(Part::Move),
        plain(Part::Move),
    ];
    let body = BodyRead::of(&parts).expect("fits");
    let behind = 3 * BODYPART_HITS;
    let power = |hits| HostileThreat::of(0, &body, hits).power;
    assert_eq!(power(behind).heal_adjacent(), 0);
    assert_eq!(power(behind + 1).heal_adjacent(), HEAL_POWER);
    assert_eq!(power(behind + 1).melee_damage(), 0);
    assert_eq!(power(behind + BODYPART_HITS).melee_damage(), 0);
    assert_eq!(
        power(behind + BODYPART_HITS + 1).melee_damage(),
        ATTACK_POWER
    );
}

/// Boosted parts that die leave the sum with their multiplier, front first (V283, V286).
#[test]
fn dead_boosted_parts_leave_the_sum() {
    let Some(Boost::Attack(m)) = ResourceType::CatalyzedUtriumAcid.boost() else {
        panic!("XUH2O boosts attack");
    };
    let mut parts = vec![boosted(Part::Attack, ResourceType::CatalyzedUtriumAcid); 2];
    parts.extend(vec![plain(Part::Attack); 2]);
    parts.extend(vec![plain(Part::Move); 4]);
    let body = BodyRead::of(&parts).expect("fits");
    for dead in 0..=4u32 {
        let hits = full(&parts) - dead * BODYPART_HITS;
        let live_boosted = 2u32.saturating_sub(dead);
        let live_plain = 2u32.min(4 - dead);
        let melee = HostileThreat::of(0, &body, hits).power.melee_damage();
        assert_eq!(
            melee,
            (m * live_boosted + live_plain) * ATTACK_POWER,
            "{dead} dead"
        );
    }
}

/// Pinned from the engine's `_applyDamage` (creeps/tick.js L9-28, V287), run in Node 26.13.1 on
/// 2026-11-22 for a body of one boosted TOUGH part and one MOVE part at 200 hits: the hits lost to
/// D = 0 to 12. XGHO2 at D = 5 saves exactly 3.5 in doubles, which `Math.round` takes up (1 lost);
/// the ratio widened from the table's `f32` would save 3.4999999 and lose 2.
#[test]
fn damage_taken_rounds_like_the_engine() {
    let pinned: [(ResourceType, [u32; 13]); 3] = [
        (
            ResourceType::GhodiumOxide,
            [0, 1, 1, 2, 3, 3, 4, 5, 6, 6, 7, 8, 8],
        ),
        (
            ResourceType::GhodiumAlkalide,
            [0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6],
        ),
        (
            ResourceType::CatalyzedGhodiumAlkalide,
            [0, 0, 1, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4],
        ),
    ];
    for (compound, lost) in pinned {
        let parts = [boosted(Part::Tough, compound), plain(Part::Move)];
        for (damage, &want) in (0u32..).zip(lost.iter()) {
            assert_eq!(
                damage_taken(&parts, full(&parts), damage),
                want,
                "{compound:?} at {damage}"
            );
        }
    }
}

/// Edge cases against the scan: all-TOUGH bodies at full and partial hits, boosted TOUGH behind
/// unboosted parts, mixed compounds, no hits, and no parts.
#[test]
fn effective_hits_edge_cases() {
    let toughs: Vec<ResourceType> = tough_compounds().into_iter().map(|(r, _)| r).collect();
    let mut bodies: Vec<Vec<BodyPartRead>> = Vec::new();
    for &c in &toughs {
        bodies.push(vec![boosted(Part::Tough, c); 5]);
        bodies.push(vec![boosted(Part::Tough, c); 1]);
        let mut behind = vec![plain(Part::Move); 3];
        behind.extend(vec![boosted(Part::Tough, c); 2]);
        bodies.push(behind);
    }
    bodies.push(toughs.iter().map(|&c| boosted(Part::Tough, c)).collect());
    for parts in &bodies {
        for hits in 0..=full(parts) {
            assert_eq!(
                effective_hits(parts, hits),
                least_kill(parts, hits),
                "{parts:?} at {hits}"
            );
        }
    }
    assert_eq!(effective_hits(&[], 0), 0);
}

fn room(name: &str) -> RoomName {
    name.parse().expect("valid room name")
}

fn at(name: &str, x: u8, y: u8) -> Position {
    Position::new(
        RoomCoordinate::new(x).expect("in room"),
        RoomCoordinate::new(y).expect("in room"),
        room(name),
    )
}

/// One hostile creep for a tick of the scenario world.
#[derive(Clone)]
struct Hostile {
    key: u128,
    pos: Position,
    parts: Vec<BodyPartRead>,
    hits: u32,
}

fn hostile(key: u128, room_name: &str, parts: Vec<BodyPartRead>) -> Hostile {
    let hits = full(&parts);
    Hostile {
        key,
        pos: at(room_name, 25, 25),
        parts,
        hits,
    }
}

/// The least controller level with a lab allowance (`CONTROLLER_STRUCTURES`, V291).
fn lab_level() -> u8 {
    (1..=8u8)
        .find(|&l| StructureType::Lab.controller_structures(u32::from(l)) > 0)
        .expect("labs at some level")
}

/// Rooms another player owns, at the lab level and one below it, by name.
const LAB_ROOM: &str = "W5N5";
const NO_LAB_ROOM: &str = "W6N6";

/// The controller of a room the scenario does not own: another player's for the two foreign rooms
/// above, else nobody's.
fn other_controller(name: &str) -> Controller {
    match name {
        LAB_ROOM => Controller::Foreign { level: lab_level() },
        NO_LAB_ROOM => Controller::Foreign {
            level: lab_level() - 1,
        },
        _ => Controller::Neutral,
    }
}

/// Takes one tick's snapshot as the wasm glue does: the owned rooms, an own creep, and each hostile
/// in an owned room with its hits and, while unknown, its body; a hostile in a room where it may be
/// boosted has its body forgotten. Returns how many bodies were read.
fn take(snap: &mut Snapshot, time: u32, owned: &[&str], hostiles: &[Hostile]) -> u32 {
    snap.begin(TickContext::from_raw(time, 20, 500.0, 10_000));
    for name in owned {
        snap.rooms.push(room(name), Controller::Mine { level: 3 });
    }
    let own = CreepRow {
        pos: at(owned.first().copied().unwrap_or("W1N1"), 10, 10),
        my: true,
        spawning: false,
        fatigue: 0,
    };
    snap.creeps.push(ObjectKey(1), own).expect("own");
    let mut reads = 0;
    for h in hostiles {
        let row = CreepRow {
            pos: h.pos,
            my: false,
            spawning: false,
            fatigue: 0,
        };
        snap.creeps.push(ObjectKey(h.key), row).expect("hostile");
        let index = snap.creeps.len() - 1;
        if owned.iter().any(|&n| room(n) == h.pos.room_name()) {
            snap.creeps.set_assessed(index, h.hits);
            if !snap.creeps.knows_body(index) {
                snap.creeps
                    .set_body(index, BodyRead::of(&h.parts).expect("fits"));
                reads += 1;
            }
        } else {
            let name = h.pos.room_name().to_string();
            if other_controller(&name).boosts_possible() {
                snap.creeps.forget_body(index);
            }
        }
    }
    snap.finish();
    reads
}

fn attacker() -> Vec<BodyPartRead> {
    vec![
        boosted(Part::Tough, ResourceType::CatalyzedGhodiumAlkalide),
        boosted(Part::Attack, ResourceType::UtriumAcid),
        plain(Part::Attack),
        plain(Part::Move),
        plain(Part::Move),
    ]
}

fn healer() -> Vec<BodyPartRead> {
    vec![plain(Part::Heal), plain(Part::Move)]
}

/// Only another player's room at a level with labs lets a creep's boosts change (V290, V291).
#[test]
fn boosts_change_only_in_another_players_lab_room() {
    let lab = lab_level();
    assert!(Controller::Foreign { level: lab }.boosts_possible());
    assert!(Controller::Foreign { level: 8 }.boosts_possible());
    assert!(!Controller::Foreign { level: lab - 1 }.boosts_possible());
    for c in [
        Controller::Absent,
        Controller::Neutral,
        Controller::Mine { level: 8 },
    ] {
        assert!(!c.boosts_possible(), "{c:?}");
    }
}

/// The body cache: read once while the creep stays in the snapshot and out of rooms where it may
/// be boosted, read again after it was seen in one or left the snapshot (a new handle), and its
/// entry dropped with its handle.
#[test]
fn bodies_are_read_once_while_in_owned_rooms() {
    let mut snap = Snapshot::default();
    let owned = ["W1N1"];
    let a = hostile(10, "W1N1", attacker());
    assert_eq!(take(&mut snap, 1, &owned, std::slice::from_ref(&a)), 1);
    assert_eq!(take(&mut snap, 2, &owned, std::slice::from_ref(&a)), 0);
    assert_eq!(snap.creeps.bodies_known(), 1);
    // Seen in a neutral room, or another player's room below the lab level: the body is kept.
    for (t, name) in [(3, "W2N1"), (5, NO_LAB_ROOM)] {
        let mut out = a.clone();
        out.pos = at(name, 25, 25);
        assert_eq!(take(&mut snap, t, &owned, &[out]), 0);
        assert_eq!(snap.creeps.bodies_known(), 1, "{name}");
        assert_eq!(take(&mut snap, t + 1, &owned, std::slice::from_ref(&a)), 0);
    }
    // Seen in another player's room with labs: the body is forgotten, and read again on the way
    // back.
    let mut out = a.clone();
    out.pos = at(LAB_ROOM, 25, 25);
    assert_eq!(take(&mut snap, 7, &owned, &[out]), 0);
    assert_eq!(snap.creeps.bodies_known(), 0);
    assert_eq!(take(&mut snap, 8, &owned, std::slice::from_ref(&a)), 1);
    // Gone for a tick: its handle is released with its body.
    assert_eq!(take(&mut snap, 9, &owned, &[]), 0);
    assert_eq!(snap.creeps.bodies_known(), 0);
    assert_eq!(take(&mut snap, 10, &owned, std::slice::from_ref(&a)), 1);
    // A second `set_assessed` for the same row, or one for a row that does not exist, does nothing.
    let row = snap.creeps.assessed()[0];
    let index = usize::try_from(row).expect("small");
    snap.creeps.set_assessed(index, 1);
    snap.creeps.set_assessed(99, 1);
    assert_eq!(snap.creeps.assessed(), [row]);
    assert_eq!(snap.creeps.hits()[index], a.hits);
}

/// A room's sums are its hostiles' power, hits, and effective hits; a healer alone is no threat; a
/// dangerous hostile opens one, which logs its changes no more often than `threat_log_ticks` and
/// ends `threat_end_ticks` after the last sight, surviving a short step out of the room.
#[test]
fn threats_start_change_and_end() {
    let cfg = DEFENSE_MMO;
    let owned = ["W1N1", "W2N1"];
    let mut snap = Snapshot::default();
    let mut threats = Threats::default();
    let w1 = room("W1N1");
    let heal = hostile(20, "W1N1", healer());
    let a = hostile(21, "W1N1", attacker());
    let b = hostile(22, "W1N1", attacker());

    // A healer alone: the room is listed, but nothing is dangerous and no threat opens.
    let mut t = 1000;
    take(&mut snap, t, &owned, std::slice::from_ref(&heal));
    threats.assess(&snap, &cfg);
    let r = *threats.room(w1).expect("listed");
    assert_eq!((r.hostiles, r.dangerous, r.unread), (1, 0, 0));
    assert_eq!(r.power.heal_adjacent(), HEAL_POWER);
    assert!(threats.events().is_empty());
    assert_eq!(threats.under_threat(w1), None);

    // An attacker arrives: one start, and the sums add both creeps.
    t += 1;
    let start = t;
    take(&mut snap, t, &owned, &[heal.clone(), a.clone()]);
    threats.assess(&snap, &cfg);
    assert_eq!(threats.events(), [ThreatEvent::Started { room: w1 }]);
    let r = *threats.room(w1).expect("listed");
    let body = BodyRead::of(&a.parts).expect("fits");
    let one = HostileThreat::of(0, &body, a.hits);
    let Some(Boost::Attack(m)) = ResourceType::UtriumAcid.boost() else {
        panic!("UH2O boosts attack");
    };
    assert_eq!(one.power.melee_damage(), (m + 1) * ATTACK_POWER);
    assert!(one.boosted);
    assert!(one.effective_hits > one.hits, "its TOUGH part is boosted");
    assert_eq!((r.hostiles, r.dangerous, r.boosted), (2, 1, 1));
    assert_eq!(r.power.melee_damage(), one.power.melee_damage());
    assert_eq!(r.hits, heal.hits + a.hits);
    assert_eq!(r.effective_hits, heal.hits + one.effective_hits);
    assert_eq!(threats.hostiles(&r).len(), 2);
    assert_eq!(threats.under_threat(w1), Some(start));

    // A second attacker inside the log window: no change line until the window has passed.
    for _ in 1..cfg.threat_log_ticks {
        t += 1;
        take(&mut snap, t, &owned, &[heal.clone(), a.clone(), b.clone()]);
        threats.assess(&snap, &cfg);
        assert!(threats.events().is_empty(), "tick {t}");
    }
    t += 1;
    take(&mut snap, t, &owned, &[heal.clone(), a.clone(), b.clone()]);
    threats.assess(&snap, &cfg);
    assert_eq!(
        threats.events(),
        [ThreatEvent::Changed {
            room: w1,
            since: start
        }]
    );
    t += 1;
    take(&mut snap, t, &owned, &[heal.clone(), a.clone(), b.clone()]);
    threats.assess(&snap, &cfg);
    assert!(threats.events().is_empty(), "the change is logged once");

    // Both attackers step out for less than the end window and come back: still one threat.
    for _ in 1..cfg.threat_end_ticks {
        t += 1;
        take(&mut snap, t, &owned, std::slice::from_ref(&heal));
        threats.assess(&snap, &cfg);
        assert!(threats.events().is_empty(), "tick {t}");
    }
    t += 1;
    take(&mut snap, t, &owned, &[a.clone(), b.clone()]);
    threats.assess(&snap, &cfg);
    assert!(threats.events().is_empty());
    assert_eq!(threats.under_threat(w1), Some(start));

    // They leave for good: the threat ends `threat_end_ticks` after the last sight.
    let last = t;
    for _ in 1..cfg.threat_end_ticks {
        t += 1;
        take(&mut snap, t, &owned, &[]);
        threats.assess(&snap, &cfg);
        assert!(threats.events().is_empty(), "tick {t}");
        assert!(threats.rooms().is_empty());
    }
    t += 1;
    take(&mut snap, t, &owned, &[]);
    assert!(threats.needed(&snap), "an open threat still has to end");
    threats.assess(&snap, &cfg);
    assert_eq!(
        threats.events(),
        [ThreatEvent::Ended {
            room: w1,
            since: start,
            last
        }]
    );
    assert_eq!(threats.open(), 0);
    t += 1;
    take(&mut snap, t, &owned, &[]);
    assert!(
        !threats.needed(&snap),
        "nothing to do with no hostile and no threat"
    );
}

/// Two owned rooms keep their own threats, in room order; a hostile the snapshot did not assess (a
/// room not owned) is not counted, and one with no body read is counted as unread.
#[test]
fn rooms_keep_their_own_threats() {
    let cfg = DEFENSE_MMO;
    let owned = ["W1N1", "W2N1"];
    let mut snap = Snapshot::default();
    let mut threats = Threats::default();
    let a = hostile(30, "W2N1", attacker());
    let b = hostile(31, "W1N1", attacker());
    let outside = hostile(32, "W3N1", attacker());
    take(&mut snap, 50, &owned, &[a, b, outside]);
    threats.assess(&snap, &cfg);
    let rooms: Vec<RoomName> = threats.rooms().iter().map(|r| r.room).collect();
    let mut sorted = vec![room("W1N1"), room("W2N1")];
    sorted.sort_unstable();
    assert_eq!(rooms, sorted);
    let starts: Vec<ThreatEvent> = sorted
        .iter()
        .map(|&room| ThreatEvent::Started { room })
        .collect();
    assert_eq!(threats.events(), starts.as_slice());

    // A hostile whose body was never set counts as an unread hostile with no power.
    snap.begin(TickContext::from_raw(51, 20, 500.0, 10_000));
    snap.rooms.push(room("W1N1"), Controller::Mine { level: 3 });
    let row = CreepRow {
        pos: at("W1N1", 5, 5),
        my: false,
        spawning: false,
        fatigue: 0,
    };
    snap.creeps.push(ObjectKey(40), row).expect("hostile");
    snap.creeps.set_assessed(0, 300);
    snap.finish();
    threats.assess(&snap, &cfg);
    let r = threats.room(room("W1N1")).expect("listed");
    assert_eq!((r.hostiles, r.unread, r.dangerous), (1, 1, 0));
    assert!(threats.hostiles(r).is_empty());
}
