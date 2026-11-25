//! Remote defender tests (P7.6, ADR-037), among them the P7 gate's remote scenarios: a small melee
//! invader, a ranged one with a healer, and a raid beyond the cap, fed to the remote manager's
//! two-room world (`economy::remotes_tests`). Expected values come from the constants
//! (`BODYPART_COST` through `Part::cost`, `BODYPART_HITS`, `RANGED_ATTACK_POWER`, `ATTACK_POWER`,
//! `HEAL_POWER`, the `BOOSTS` table through `ResourceType::boost`, `MAX_CREEP_SIZE`,
//! `CREEP_LIFE_TIME`) and the engine's order within a tick (damage, then heal, then death; V287),
//! through a reference fight written out here apart from the code under test: every armed foe hits
//! every defender, each of which must stay above its floor alone.

use rustedscreep_consts::extra::BODYPART_HITS;
use rustedscreep_consts::{
    ATTACK_POWER, Boost, CREEP_LIFE_TIME, CREEP_RANGED_ACTION_RANGE, HEAL_POWER, MAX_CREEP_SIZE,
    Part, RANGED_ATTACK_POWER, ResourceType,
};

use super::remote::{
    DuelScratch, Fighter, Foe, PRIORITY_REMOTE_DEFENDER, RemoteDefense, RemoteDefenseEvent, Shape,
    SizeScratch, Sizing, duel, fit, pays, size,
};
use super::threat::{Threats, part_hits};
use super::towers::{Shot, kill_ticks};
use crate::config::{DEFENSE_MMO, DefenseConfig, MOVEMENT_MMO};
use crate::economy::remotes_tests::{World, pos, staffed, the_remote, tile};
use crate::economy::{AbandonReason, PRIORITY_NORMAL, PRIORITY_REMOTE_MINER};
use crate::intents::CreepIntent;
use crate::scheduler::Commit;
use crate::snapshot::{BodyPartRead, BodyRead, Combat};
use crate::spawning::{Body, Role};

/// A ranged attack's reach (V339), and the melee reach plus one step a post keeps clear.
const REACH: u8 = CREEP_RANGED_ACTION_RANGE;
const MELEE_LEAD: u8 = 2;

/// The test world's home capacity (`economy::remotes_tests`: RCL5, 1800).
const CAPACITY: u32 = 1800;

fn plain(part: Part) -> BodyPartRead {
    BodyPartRead { part, boost: None }
}

fn parts(spec: &[(Part, u32)]) -> Vec<BodyPartRead> {
    spec.iter()
        .flat_map(|&(p, n)| std::iter::repeat_n(plain(p), usize::try_from(n).expect("small")))
        .collect()
}

fn full(parts: &[BodyPartRead]) -> u32 {
    u32::try_from(parts.len()).expect("small") * BODYPART_HITS
}

/// V344's small melee invader: 2 TOUGH, 5 MOVE, 1 RANGED_ATTACK, 1 WORK, 1 ATTACK.
fn small_melee() -> Vec<BodyPartRead> {
    parts(&[
        (Part::Tough, 2),
        (Part::Move, 5),
        (Part::RangedAttack, 1),
        (Part::Work, 1),
        (Part::Attack, 1),
    ])
}

/// V344's small ranged invader: 2 TOUGH, 5 MOVE, 3 RANGED_ATTACK.
fn small_ranged() -> Vec<BodyPartRead> {
    parts(&[(Part::Tough, 2), (Part::Move, 5), (Part::RangedAttack, 3)])
}

/// V344's small healer: 5 MOVE, 5 HEAL.
fn small_healer() -> Vec<BodyPartRead> {
    parts(&[(Part::Move, 5), (Part::Heal, 5)])
}

/// V344's big ranged invader, about: 6 TOUGH, 25 MOVE, 18 RANGED_ATTACK, 1 WORK.
fn big_ranged() -> Vec<BodyPartRead> {
    parts(&[
        (Part::Tough, 6),
        (Part::Move, 25),
        (Part::RangedAttack, 18),
        (Part::Work, 1),
    ])
}

/// `p` with every part of type `part` boosted with `compound`.
fn boosted(mut p: Vec<BodyPartRead>, part: Part, compound: ResourceType) -> Vec<BodyPartRead> {
    for x in p.iter_mut().filter(|x| x.part == part) {
        x.boost = Some(compound);
    }
    p
}

fn foe(p: &[BodyPartRead]) -> Foe {
    Foe::of(&BodyRead::of(p).expect("fits"), full(p))
}

/// A part's multiplier for its action from the `BOOSTS` table, 1 unboosted (V286).
fn mult(x: &BodyPartRead) -> u32 {
    match (x.part, x.boost.and_then(ResourceType::boost)) {
        (Part::Attack, Some(Boost::Attack(m)))
        | (Part::RangedAttack, Some(Boost::RangedAttack(m)))
        | (Part::Heal, Some(Boost::Heal(m))) => m,
        _ => 1,
    }
}

/// A whole foe's melee and ranged damage a tick, boosts included.
fn foe_damage(p: &[BodyPartRead]) -> u32 {
    p.iter()
        .map(|x| match x.part {
            Part::Attack => ATTACK_POWER * mult(x),
            Part::RangedAttack => RANGED_ATTACK_POWER * mult(x),
            _ => 0,
        })
        .sum()
}

fn foe_heal(p: &[BodyPartRead]) -> u32 {
    p.iter()
        .filter(|x| x.part == Part::Heal)
        .map(|x| HEAL_POWER * mult(x))
        .sum()
}

/// The shape's cost from the part costs, written out.
fn cost_of(ranged: u32, heal: u32) -> u32 {
    ranged * Part::RangedAttack.cost()
        + heal * Part::Heal.cost()
        + (ranged + heal) * Part::Move.cost()
}

/// A defender in the reference fight: damage and self heal a tick, hits, hits when whole, floor.
#[derive(Clone, Copy, Debug)]
struct Def {
    damage: u32,
    heal: u32,
    hits: u32,
    max: u32,
    floor: u32,
}

/// A whole body of `ranged` RANGED_ATTACK and `heal` HEAL parts behind as many MOVE parts, written
/// out: it may lose `loss_pct` of its hits, at most its MOVE parts' hits.
fn def(ranged: u32, heal: u32, loss_pct: u32) -> Def {
    let max = (ranged + heal) * 2 * BODYPART_HITS;
    let allowance = (max * loss_pct / 100).min((ranged + heal) * BODYPART_HITS);
    Def {
        damage: ranged * RANGED_ATTACK_POWER,
        heal: heal * HEAL_POWER,
        hits: max,
        max,
        floor: max - allowance,
    }
}

fn fighter(d: Def) -> Fighter {
    Fighter {
        damage: d.damage,
        heal: d.heal,
        hits: d.hits,
        hits_max: d.max,
        floor: d.floor,
    }
}

/// The reference fight (module docs of `defense::remote`), for foes whose TOUGH parts are not
/// boosted, in kill order: each tick the team's damage hits the first live armed foe, every live
/// foe's heal heals it, and every live armed foe hits every defender, which heals itself; a
/// defender is lost at or below its floor. The ticks to kill every armed foe, or the index of the
/// first defender to fall (`Some`), or `None` for a stall or a fight past the limit.
fn reference_fight(
    foes: &[Vec<BodyPartRead>],
    team: &[Def],
    limit: u32,
) -> Result<u32, Option<usize>> {
    let damage: u32 = team.iter().map(|d| d.damage).sum();
    let mut order: Vec<usize> = (0..foes.len())
        .filter(|&i| foe_damage(&foes[i]) > 0)
        .collect();
    order.sort_by_key(|&i| (full(&foes[i]), i));
    if order.is_empty() {
        return Ok(0);
    }
    if let Some(i) = team.iter().position(|d| d.hits <= d.floor) {
        return Err(Some(i));
    }
    if damage == 0 {
        return Err(None);
    }
    let mut hp: Vec<i64> = foes.iter().map(|f| i64::from(full(f))).collect();
    let mut mine: Vec<i64> = team.iter().map(|d| i64::from(d.hits)).collect();
    let mut next = 0;
    for t in 1..=limit {
        let live = |i: usize, hp: &[i64]| hp[i] > 0;
        let incoming: u32 = (0..foes.len())
            .filter(|&i| live(i, &hp))
            .map(|i| foe_damage(&foes[i]))
            .sum();
        let healing: u32 = (0..foes.len())
            .filter(|&i| live(i, &hp))
            .map(|i| foe_heal(&foes[i]))
            .sum();
        let target = order[next];
        let after = hp[target] - i64::from(damage) + i64::from(healing);
        if after <= 0 {
            hp[target] = 0;
            next += 1;
        } else {
            let after = after.min(i64::from(full(&foes[target])));
            if after >= hp[target] {
                return Err(None);
            }
            hp[target] = after;
        }
        for (i, (h, d)) in mine.iter_mut().zip(team).enumerate() {
            *h = (*h - i64::from(incoming) + i64::from(d.heal)).min(i64::from(d.max));
            if *h <= i64::from(d.floor) {
                return Err(Some(i));
            }
        }
        if next == order.len() {
            return Ok(t);
        }
    }
    Err(None)
}

fn reference(foes: &[Vec<BodyPartRead>], team: &[Def], limit: u32) -> Option<u32> {
    reference_fight(foes, team, limit).ok()
}

/// The reference team that goes in: the whole team, else without the first to fall, again, until
/// the rest win; the ticks and which members are kept.
fn reference_fit(foes: &[Vec<BodyPartRead>], team: &[Def], limit: u32) -> Option<(u32, Vec<bool>)> {
    let mut keep = vec![true; team.len()];
    loop {
        let idx: Vec<usize> = (0..team.len()).filter(|&i| keep[i]).collect();
        if idx.is_empty() {
            return None;
        }
        let sub: Vec<Def> = idx.iter().map(|&i| team[i]).collect();
        match reference_fight(foes, &sub, limit) {
            Ok(t) => return Some((t, keep)),
            Err(Some(j)) => keep[idx[j]] = false,
            Err(None) => return None,
        }
    }
}

/// The cheapest winning team by brute force over every shape and count (least energy in all, then
/// fewest bodies, then fewest parts), with the reference fight, `living` first and counted in
/// `have` toward the cap.
fn cheapest(
    foes: &[Vec<BodyPartRead>],
    living: &[Def],
    have: u32,
    capacity: u32,
    cap: u32,
    cfg: &DefenseConfig,
) -> Option<(u32, u32, u32)> {
    let pairs = MAX_CREEP_SIZE / 2;
    // (energy in all, bodies, parts), then (ranged, heal, bodies).
    type Ranked = ((u32, u32, u32), (u32, u32, u32));
    let mut best: Option<Ranked> = None;
    for count in 1..=cap.saturating_sub(have) {
        for heal in 0..pairs {
            for ranged in 1..=(pairs - heal) {
                if cost_of(ranged, heal) > capacity {
                    continue;
                }
                let mut team = living.to_vec();
                team.extend(std::iter::repeat_n(
                    def(ranged, heal, cfg.remote_loss_pct),
                    count as usize,
                ));
                // Every new body must go in.
                let Some((_, keep)) = reference_fit(foes, &team, cfg.max_kill_ticks) else {
                    continue;
                };
                if !keep[living.len()..].iter().all(|&k| k) {
                    continue;
                }
                let key = (count * cost_of(ranged, heal), count, 2 * (ranged + heal));
                if best.is_none_or(|(k, _)| key < k) {
                    best = Some((key, (ranged, heal, count)));
                }
            }
        }
    }
    best.map(|(_, s)| s)
}

/// The raids the unit tests fight: V344's small bodies alone and together, a big ranged one, and a
/// boosted pair (the `BOOSTS` table's tier-1 compounds on ATTACK, RANGED_ATTACK and HEAL, V344).
fn raids() -> Vec<Vec<Vec<BodyPartRead>>> {
    let uh = ResourceType::UtriumHydride;
    let ko = ResourceType::KeaniumOxide;
    let lo = ResourceType::LemergiumOxide;
    assert!(matches!(uh.boost(), Some(Boost::Attack(_))));
    assert!(matches!(ko.boost(), Some(Boost::RangedAttack(_))));
    assert!(matches!(lo.boost(), Some(Boost::Heal(_))));
    let melee = boosted(
        boosted(small_melee(), Part::Attack, uh),
        Part::RangedAttack,
        ko,
    );
    vec![
        vec![small_melee()],
        vec![small_ranged()],
        vec![small_melee(), small_ranged()],
        vec![small_melee(), small_healer()],
        vec![big_ranged()],
        vec![melee, boosted(small_healer(), Part::Heal, lo)],
    ]
}

#[test]
fn a_shape_is_move_then_ranged_attack_then_heal() {
    let s = Shape { ranged: 4, heal: 2 };
    assert_eq!(s.moves(), 6);
    assert_eq!(s.parts(), 12);
    assert_eq!(s.cost(), cost_of(4, 2));
    assert_eq!(s.hits(), 12 * BODYPART_HITS);
    let body = s.body().expect("body");
    let expected: Vec<Part> = std::iter::repeat_n(Part::Move, 6)
        .chain(std::iter::repeat_n(Part::RangedAttack, 4))
        .chain(std::iter::repeat_n(Part::Heal, 2))
        .collect();
    assert_eq!(body.parts(), &expected[..]);
    assert_eq!(body.cost(), s.cost());
    // The allowance is the share, at most the MOVE parts' hits.
    assert_eq!(s.allowance(40), 12 * BODYPART_HITS * 40 / 100);
    assert_eq!(s.allowance(90), 6 * BODYPART_HITS);
    // No body with no weapon, or over the size cap.
    assert!(Shape { ranged: 0, heal: 3 }.body().is_none());
    assert!(
        Shape {
            ranged: MAX_CREEP_SIZE / 2,
            heal: 1
        }
        .body()
        .is_none()
    );
    assert!(
        Shape {
            ranged: MAX_CREEP_SIZE / 2,
            heal: 0
        }
        .body()
        .is_some()
    );
    let f = s.fighter(40);
    let d = def(4, 2, 40);
    assert_eq!(
        (f.damage, f.heal, f.hits, f.hits_max, f.floor),
        (d.damage, d.heal, d.hits, d.max, d.floor)
    );
    // Read back from a body, live parts or not.
    let read: Vec<BodyPartRead> = body.parts().iter().map(|&p| plain(p)).collect();
    assert_eq!(Shape::of_parts(&read), s);
}

/// Above its floor every RANGED_ATTACK and HEAL part of a remote defender's body is whole (V283:
/// damage takes the first parts first, and its MOVE parts come first), so the sim's damage and heal
/// hold to the floor; for every shape and every loss share in the safe range.
#[test]
fn above_the_floor_every_weapon_and_heal_part_is_whole() {
    for loss in 10..=50 {
        for heal in 0..MAX_CREEP_SIZE / 2 {
            for ranged in 1..=(MAX_CREEP_SIZE / 2 - heal) {
                let s = Shape { ranged, heal };
                let floor = s.fighter(loss).floor;
                let len = s.parts() as usize;
                for i in s.moves() as usize..len {
                    assert_eq!(
                        part_hits(len, floor + 1, i),
                        BODYPART_HITS,
                        "{s:?} at {loss}%: part {i}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_duel_matches_the_reference_fight() {
    let cfg = DEFENSE_MMO;
    let mut s = DuelScratch::default();
    for raid in &raids() {
        let foes: Vec<Foe> = raid.iter().map(|p| foe(p)).collect();
        for count in 1..=2 {
            for heal in 0..6 {
                for ranged in 1..=12 {
                    let d = def(ranged, heal, cfg.remote_loss_pct);
                    let team = vec![fighter(d); count];
                    let got = duel(&foes, &team, cfg.max_kill_ticks, &mut s);
                    let want = reference(raid, &vec![d; count], cfg.max_kill_ticks);
                    assert_eq!(got, want, "{raid:?} {ranged}/{heal} x{count}");
                }
            }
        }
    }
}

/// The combat-math audit's H1: with defenders that differ, every one must survive every foe's
/// damage alone. A living defender of 3 RANGED_ATTACK and 2 HEAL at 730 hits and a new one of 2
/// RANGED_ATTACK kill a small ranged invader in `10 × BODYPART_HITS / (5 × RANGED_ATTACK_POWER)`
/// ticks, but the new one, healing nothing, reaches its floor after
/// `ceil(min(4 × BODYPART_HITS × 40%, 2 × BODYPART_HITS) / (3 × RANGED_ATTACK_POWER))` of them.
#[test]
fn the_duel_holds_for_any_targeting_of_mixed_teams() {
    let cfg = DEFENSE_MMO;
    let mut s = DuelScratch::default();
    let raid = vec![small_ranged()];
    let foes = [foe(&raid[0])];
    let mut living = def(3, 2, cfg.remote_loss_pct);
    living.hits = 730;
    let new = def(2, 0, cfg.remote_loss_pct);
    let kill = full(&raid[0]).div_ceil(5 * RANGED_ATTACK_POWER);
    let falls = (new.max - new.floor).div_ceil(3 * RANGED_ATTACK_POWER);
    assert!(falls < kill, "the new body falls first ({falls} < {kill})");
    let team = [fighter(living), fighter(new)];
    assert_eq!(duel(&foes, &team, cfg.max_kill_ticks, &mut s), None);
    assert_eq!(reference(&raid, &[living, new], cfg.max_kill_ticks), None);
    // Over a grid of mixed teams the duel equals the reference.
    for raid in &raids() {
        let foes: Vec<Foe> = raid.iter().map(|p| foe(p)).collect();
        for (lr, lh, lost) in [(3, 2, 270), (1, 0, 0), (6, 1, 100), (2, 4, 500)] {
            let mut l = def(lr, lh, cfg.remote_loss_pct);
            l.hits = l.max - lost.min(l.max);
            for heal in 0..4 {
                for ranged in 1..=10 {
                    let n = def(ranged, heal, cfg.remote_loss_pct);
                    let got = duel(&foes, &[fighter(l), fighter(n)], cfg.max_kill_ticks, &mut s);
                    let want = reference(raid, &[l, n], cfg.max_kill_ticks);
                    assert_eq!(
                        got, want,
                        "{raid:?} living {lr}/{lh}-{lost}, new {ranged}/{heal}"
                    );
                }
            }
        }
    }
}

/// The team that goes in leaves out the first defender to fall and judges the rest again, as the
/// reference does; against the pinned case of the H1 test, both fall in turn and none goes in, while
/// a weak defender beside a strong one is left out and the strong one wins alone.
#[test]
fn the_fit_team_leaves_out_who_would_fall() {
    let cfg = DEFENSE_MMO;
    let mut s = DuelScratch::default();
    let mut keep = Vec::new();
    for raid in &raids() {
        let foes: Vec<Foe> = raid.iter().map(|p| foe(p)).collect();
        for team in [
            vec![
                def(1, 0, cfg.remote_loss_pct),
                def(10, 5, cfg.remote_loss_pct),
            ],
            vec![
                def(2, 0, cfg.remote_loss_pct),
                def(3, 2, cfg.remote_loss_pct),
                def(6, 1, cfg.remote_loss_pct),
            ],
            vec![def(1, 0, cfg.remote_loss_pct)],
        ] {
            let f: Vec<Fighter> = team.iter().map(|&d| fighter(d)).collect();
            let got = fit(&foes, &f, &mut keep, cfg.max_kill_ticks, &mut s);
            let want = reference_fit(raid, &team, cfg.max_kill_ticks);
            assert_eq!(got, want.as_ref().map(|w| w.0), "{raid:?} {team:?}");
            if let Some((_, k)) = want {
                assert_eq!(keep, k);
            }
        }
    }
    let strong = def(10, 5, cfg.remote_loss_pct);
    let team = [fighter(def(1, 0, cfg.remote_loss_pct)), fighter(strong)];
    assert!(
        fit(
            &[foe(&small_melee())],
            &team,
            &mut keep,
            cfg.max_kill_ticks,
            &mut s
        )
        .is_some()
    );
    assert_eq!(keep, [false, true]);
}

/// With a defender that cannot fall, the duel's kill time against one foe is the towers' sim's
/// with the same damage (P7.2's `kill_ticks`), boosted TOUGH included (all three compounds).
#[test]
fn one_foe_dies_as_the_towers_sim_says() {
    let compounds: Vec<ResourceType> = [
        ResourceType::GhodiumOxide,
        ResourceType::GhodiumAlkalide,
        ResourceType::CatalyzedGhodiumAlkalide,
    ]
    .into_iter()
    .filter(|r| matches!(r.boost(), Some(Boost::Tough(_))))
    .collect();
    assert_eq!(compounds.len(), 3, "the three TOUGH compounds");
    let mut s = DuelScratch::default();
    for boost in [
        None,
        Some(compounds[0]),
        Some(compounds[1]),
        Some(compounds[2]),
    ] {
        let mut p = small_melee();
        for part in p.iter_mut().filter(|x| x.part == Part::Tough) {
            part.boost = boost;
        }
        let f = foe(&p);
        for ranged in 1..=20 {
            let damage = ranged * RANGED_ATTACK_POWER;
            let tank = Fighter {
                damage,
                heal: 0,
                hits: u32::MAX / 2,
                hits_max: u32::MAX / 2,
                floor: 0,
            };
            let got = duel(&[f], &[tank], 200, &mut s);
            let towers = [Shot {
                damage,
                shots: u32::MAX,
            }];
            let want = kill_ticks(&p, full(&p), full(&p), &towers, 0, 200);
            assert_eq!(got, want, "{boost:?} x{ranged}");
        }
    }
}

#[test]
fn the_duel_loses_to_heal_it_cannot_beat_and_to_a_long_fight() {
    let mut s = DuelScratch::default();
    let healed = [foe(&small_ranged()), foe(&small_healer())];
    let tank = |damage| Fighter {
        damage,
        heal: 0,
        hits: u32::MAX / 2,
        hits_max: u32::MAX / 2,
        floor: 0,
    };
    // 5 HEAL parts heal 5 × HEAL_POWER a tick: a team dealing no more never brings the target lower.
    assert_eq!(duel(&healed, &[tank(5 * HEAL_POWER)], 1000, &mut s), None);
    // One RANGED_ATTACK part takes 1000 / RANGED_ATTACK_POWER ticks against an unboosted 1000 hits:
    // a limit one short loses.
    let need = full(&small_ranged()).div_ceil(RANGED_ATTACK_POWER);
    let one = [foe(&small_ranged())];
    let slow = tank(RANGED_ATTACK_POWER);
    assert_eq!(duel(&one, &[slow], need, &mut s), Some(need));
    assert_eq!(duel(&one, &[slow], need - 1, &mut s), None);
    // No armed foe: nothing to fight.
    assert_eq!(duel(&[foe(&small_healer())], &[slow], 10, &mut s), Some(0));
    // A defender already at its floor has lost.
    let mut low = slow;
    low.floor = low.hits;
    assert_eq!(duel(&one, &[low], need, &mut s), None);
}

#[test]
fn the_size_is_the_cheapest_team_that_wins() {
    let cfg = DEFENSE_MMO;
    let mut s = SizeScratch::default();
    let mut weak = def(1, 0, cfg.remote_loss_pct);
    weak.hits -= 20;
    for raid in &raids() {
        let foes: Vec<Foe> = raid.iter().map(|p| foe(p)).collect();
        for living in [vec![], vec![weak]] {
            let lf: Vec<Fighter> = living.iter().map(|&d| fighter(d)).collect();
            let have = u32::try_from(living.len()).expect("small");
            for capacity in [550, 800, 1300, CAPACITY, 2300, 5600] {
                let got = size(
                    &foes,
                    &lf,
                    have,
                    capacity,
                    cfg.max_remote_defenders,
                    cfg.max_kill_ticks,
                    cfg.remote_loss_pct,
                    &mut s,
                );
                if !living.is_empty() && reference_fit(raid, &living, cfg.max_kill_ticks).is_some()
                {
                    assert!(matches!(got, Sizing::Held { .. }), "{got:?}");
                    continue;
                }
                match cheapest(
                    raid,
                    &living,
                    have,
                    capacity,
                    cfg.max_remote_defenders,
                    &cfg,
                ) {
                    Some((ranged, heal, count)) => {
                        let Sizing::Need {
                            shape,
                            count: c,
                            ticks,
                        } = got
                        else {
                            panic!("{raid:?} at {capacity}: {got:?}");
                        };
                        assert_eq!(
                            (shape.ranged, shape.heal, c),
                            (ranged, heal, count),
                            "{capacity}"
                        );
                        assert!(shape.cost() <= capacity && shape.parts() <= MAX_CREEP_SIZE);
                        let mut team = living.clone();
                        team.extend(std::iter::repeat_n(
                            def(ranged, heal, cfg.remote_loss_pct),
                            count as usize,
                        ));
                        let fitted = reference_fit(raid, &team, cfg.max_kill_ticks).map(|f| f.0);
                        assert_eq!(Some(ticks), fitted);
                    }
                    None => assert_eq!(got, Sizing::Beyond, "{raid:?} at {capacity}"),
                }
            }
        }
    }
}

/// More RANGED_ATTACK parts with the same HEAL parts never lose a fight fewer won: the property the
/// size's binary search rests on, over the raids, mixed living teams included.
#[test]
fn more_ranged_attack_parts_never_lose_a_won_fight() {
    let cfg = DEFENSE_MMO;
    let mut s = DuelScratch::default();
    let mut damaged = def(3, 2, cfg.remote_loss_pct);
    damaged.hits -= 300;
    for raid in &raids() {
        let foes: Vec<Foe> = raid.iter().map(|p| foe(p)).collect();
        for living in [vec![], vec![damaged], vec![def(1, 0, cfg.remote_loss_pct)]] {
            for count in 1..=2 {
                for heal in 0..6 {
                    // As the size's search judges a team: it wins with every new body in.
                    let wins = |ranged: u32, s: &mut DuelScratch| {
                        let mut team: Vec<Fighter> = living.iter().map(|&d| fighter(d)).collect();
                        team.extend(std::iter::repeat_n(
                            fighter(def(ranged, heal, cfg.remote_loss_pct)),
                            count,
                        ));
                        let mut keep = Vec::new();
                        fit(&foes, &team, &mut keep, cfg.max_kill_ticks, s).is_some()
                            && keep[living.len()..].iter().all(|&k| k)
                    };
                    for ranged in 1..(MAX_CREEP_SIZE / 2 - heal) {
                        if wins(ranged, &mut s) {
                            assert!(
                                wins(ranged + 1, &mut s),
                                "{raid:?} {ranged}/{heal} x{count}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn a_living_team_that_wins_needs_nothing_and_counts_first() {
    let cfg = DEFENSE_MMO;
    let mut s = SizeScratch::default();
    let foes = [foe(&small_melee())];
    let Sizing::Need { shape, count, .. } = size(&foes, &[], 0, CAPACITY, 2, 50, 40, &mut s) else {
        panic!("a small melee invader is answered");
    };
    let team = vec![shape.fighter(cfg.remote_loss_pct); count as usize];
    assert!(matches!(
        size(&foes, &team, count, CAPACITY, 2, 50, 40, &mut s),
        Sizing::Held { .. }
    ));
    // A weaker living defender counts in the sim and against the cap, as the brute force has it.
    let weak = def(1, 0, cfg.remote_loss_pct);
    let got = size(&foes, &[fighter(weak)], 1, CAPACITY, 2, 50, 40, &mut s);
    let want = cheapest(&[small_melee()], &[weak], 1, CAPACITY, 2, &cfg).expect("beatable");
    let Sizing::Need {
        shape: with,
        count: c,
        ..
    } = got
    else {
        panic!("{got:?}");
    };
    assert_eq!((with.ranged, with.heal, c), want);
    assert_eq!(c, 1, "the cap counts the living one");
    // A member the sim has no fighter for still counts against the cap.
    assert!(matches!(
        size(&foes, &[], 2, CAPACITY, 2, 50, 40, &mut s),
        Sizing::Beyond
    ));
}

#[test]
fn a_team_pays_only_below_the_wait_it_saves() {
    let life = i64::from(CREEP_LIFE_TIME);
    // A room netting one creep life's worth `net` over a full life's wait saves `net`.
    assert!(pays(999, 1000, CREEP_LIFE_TIME, 0));
    assert!(!pays(1000, 1000, CREEP_LIFE_TIME, 0));
    // Half the wait left saves half; so does a full wait with half of it spent spawning and fighting.
    assert!(pays(499, 1000, CREEP_LIFE_TIME / 2, 0));
    assert!(!pays(500, 1000, CREEP_LIFE_TIME / 2, 0));
    assert!(!pays(500, 1000, CREEP_LIFE_TIME, CREEP_LIFE_TIME / 2));
    // A room that nets nothing, or nothing left to wait, never pays.
    assert!(!pays(1, 0, CREEP_LIFE_TIME, 0));
    assert!(!pays(1, -life, CREEP_LIFE_TIME, 0));
    assert!(!pays(1, 1000, 0, 0));
    assert!(!pays(1, 1000, 10, 10));
}

#[test]
fn the_remote_defender_priority_is_between_the_remote_roles_and_the_home() {
    const {
        assert!(PRIORITY_REMOTE_DEFENDER > PRIORITY_REMOTE_MINER);
        assert!(PRIORITY_REMOTE_DEFENDER < PRIORITY_NORMAL);
    }
}

// Scenarios in the remote manager's two-room world (`economy::remotes_tests`).

/// The world with the remote defense driven as the glue drives it: the remote plan, then the
/// remote defense's plan, the remote creeps' work, and the remote defenders' work.
struct Fight {
    w: World,
    defense: RemoteDefense,
    cfg: DefenseConfig,
}

impl Fight {
    fn new() -> (Self, Vec<u128>) {
        let (w, miners, _) = staffed();
        (
            Self {
                w,
                defense: RemoteDefense::default(),
                cfg: DEFENSE_MMO,
            },
            miners,
        )
    }

    /// One tick; `spare` is whether the Standard tier runs. Returns the tick's creep intents other
    /// than moves.
    fn step_with(&mut self, spare: bool) -> Vec<(u128, CreepIntent)> {
        let w = &mut self.w;
        w.time += 1;
        w.begin();
        w.plan(true);
        self.defense.begin(&w.snap, &w.remotes, &self.cfg);
        let mut commit = Commit::default();
        for unit in 0..self.defense.unit_count() {
            self.defense
                .plan_room(
                    unit,
                    &w.snap,
                    &mut w.remotes,
                    &mut w.spawner,
                    &self.cfg,
                    spare,
                    &mut commit,
                )
                .expect("plan");
        }
        // The glue runs the remote defenders' work right after their plan (both Critical).
        if self.defense.idle(&w.snap) {
            self.defense.hold_all(&w.snap, &mut w.movement);
        } else {
            for unit in 0..self.defense.member_count() {
                self.defense
                    .work(
                        unit,
                        &w.snap,
                        &mut w.movement,
                        &mut w.buffer,
                        &MOVEMENT_MMO,
                        &mut commit,
                    )
                    .expect("defender work");
            }
        }
        for unit in 0..w.remotes.creep_count() {
            w.remotes
                .work_creep(
                    unit,
                    &w.snap,
                    &mut w.movement,
                    &mut w.buffer,
                    &w.cfg,
                    &MOVEMENT_MMO,
                    &mut commit,
                )
                .expect("work");
        }
        self.defense.finish();
        w.buffer.resolve();
        w.intents()
    }

    fn step(&mut self) -> Vec<(u128, CreepIntent)> {
        self.step_with(true)
    }

    /// An armed hostile at (x, y) of the remote room, its body assessed.
    fn raid(&mut self, key: u128, p: &[BodyPartRead], x: u8, y: u8) {
        let n = |t: Part| u32::try_from(p.iter().filter(|q| q.part == t).count()).expect("small");
        let combat = Combat {
            attack: n(Part::Attack),
            ranged_attack: n(Part::RangedAttack),
        };
        let at = pos(self.w.remote, tile(x, y));
        self.w.hostiles.push((key, at, combat));
        self.w
            .bodies
            .push((key, BodyRead::of(p).expect("fits"), full(p)));
    }

    /// A remote defender of `shape` at (x, y) of the remote room, whole.
    fn defender(&mut self, shape: Shape, x: u8, y: u8) -> u128 {
        let remote = self.w.remote;
        self.defender_at(shape, remote, x, y, shape.hits())
    }

    /// A remote defender of `shape` at (x, y) of `room`, with `hits`.
    fn defender_at(
        &mut self,
        shape: Shape,
        room: rustedscreep_consts::RoomName,
        x: u8,
        y: u8,
        hits: u32,
    ) -> u128 {
        let at = pos(room, tile(x, y));
        let key = self.w.add(Role::RemoteDefender, at, 0, 0);
        let body = shape.body().expect("body");
        let read: Vec<BodyPartRead> = body.parts().iter().map(|&p| plain(p)).collect();
        self.w
            .bodies
            .push((key, BodyRead::of(&read).expect("fits"), hits));
        key
    }

    fn engaged(&self) -> Option<bool> {
        self.defense
            .engaged(self.w.home, self.w.remote, self.w.time)
    }

    fn defender_requests(&self) -> Vec<(Body, u8)> {
        self.w
            .requests()
            .into_iter()
            .filter(|r| r.0 == Role::RemoteDefender)
            .map(|r| {
                assert_eq!(r.1, Some(self.w.remote), "named for its remote room");
                (r.2, r.3)
            })
            .collect()
    }
}

/// The P7 gate's first remote scenario: one small melee invader (V344, the usual raid, V345) in a
/// mined room. Its creeps evacuate, the home requests the cheapest team the reference fight says
/// wins, a defender in the room shoots it and heals itself, and once the room is seen with no armed
/// hostile the wait ends and the miners go back.
#[test]
fn scenario_a_small_melee_invader_is_answered_and_the_wait_ends() {
    let (mut f, miners) = Fight::new();
    let invader = small_melee();
    f.raid(9000, &invader, 30, 30);
    f.step();
    let r = the_remote(&f.w);
    assert!(r.threat.is_some(), "P6.3's wait starts");
    let (ranged, heal, count) = cheapest(
        std::slice::from_ref(&invader),
        &[],
        0,
        CAPACITY,
        f.cfg.max_remote_defenders,
        &f.cfg,
    )
    .expect("a small invader is beatable");
    let shape = Shape { ranged, heal };
    let requests = f.defender_requests();
    assert_eq!(requests.len(), count as usize, "{requests:?}");
    for (body, priority) in &requests {
        assert_eq!(*body, shape.body().expect("body"));
        assert_eq!(*priority, PRIORITY_REMOTE_DEFENDER);
    }
    // The team costs less than the wait would lose, its spawn time and kill time taken out.
    let net = the_remote(&f.w).profit.expect("priced").net;
    let wait = the_remote(&f.w).threat.expect("threatened").until - f.w.time;
    let kill = reference(
        std::slice::from_ref(&invader),
        &vec![def(ranged, heal, f.cfg.remote_loss_pct); count as usize],
        f.cfg.max_kill_ticks,
    )
    .expect("wins");
    let busy = count * shape.parts() * rustedscreep_consts::CREEP_SPAWN_TIME + kill;
    assert!(pays(count * shape.cost(), net, wait, busy));
    let events = f.defense.events().to_vec();
    assert!(
        events.iter().any(|e| matches!(e, RemoteDefenseEvent::Need { shape: s, count: c, have: 0, foes: 1, .. } if *s == shape && *c == count)),
        "{events:?}"
    );
    // Requested every tick while needed, logged once.
    f.step();
    assert_eq!(f.defender_requests().len(), count as usize);
    assert!(f.defense.events().is_empty(), "{:?}", f.defense.events());
    // The defenders arrive within range 3 of the invader: each heals itself and shoots it.
    let mut keys = Vec::new();
    for i in 0..count {
        keys.push(f.defender(shape, 27, 30 - u8::try_from(i).expect("small")));
    }
    let intents = f.step();
    assert!(
        f.defender_requests().is_empty(),
        "the living team wins: nothing more"
    );
    for &k in &keys {
        assert!(
            intents
                .iter()
                .any(|&(a, i)| a == k && matches!(i, CreepIntent::RangedAttack { .. })),
            "{intents:?}"
        );
        if heal > 0 {
            assert!(
                intents
                    .iter()
                    .any(|&(a, i)| a == k && matches!(i, CreepIntent::Heal { .. })),
                "a foe in reach: it heals itself ({intents:?})"
            );
        }
    }
    // Its posts stay out of the melee invader's reach plus a step, within range 3 of it.
    let invader_tile = tile(30, 30);
    for &(_, post) in f.defense.posts() {
        assert!(post.range(invader_tile) <= REACH && post.range(invader_tile) > MELEE_LEAD);
    }
    // The invader dies: the room is seen with no armed hostile, and the wait ends now.
    f.w.hostiles.clear();
    f.step();
    assert!(the_remote(&f.w).threat.is_none(), "the wait ended");
    assert!(
        f.defense
            .events()
            .iter()
            .any(|e| matches!(e, RemoteDefenseEvent::Cleared { .. })),
        "{:?}",
        f.defense.events()
    );
    let intents = f.step();
    assert!(
        intents
            .iter()
            .any(|&(k, i)| miners.contains(&k) && matches!(i, CreepIntent::Harvest { .. })),
        "back at work: {intents:?}"
    );
    // The guards give way with nothing to do, and need no work unit.
    assert!(f.defense.idle(&f.w.snap));
}

/// The P7 gate's second remote scenario: a ranged invader with a healer behind it. The healer's
/// heal counts against the team's damage, so the team is larger than against the ranged one alone,
/// and a living guard of that team answers the next raid alone.
#[test]
fn scenario_a_healed_ranged_raid_needs_more_and_a_guard_holds_the_next() {
    let (mut f, _) = Fight::new();
    let alone = cheapest(&[small_ranged()], &[], 0, CAPACITY, 2, &f.cfg).expect("beatable");
    let raid = vec![small_ranged(), small_healer()];
    let healed = cheapest(&raid, &[], 0, CAPACITY, 2, &f.cfg).expect("beatable");
    assert!(
        cost_of(healed.0, healed.1) * healed.2 > cost_of(alone.0, alone.1) * alone.2,
        "the healer makes it dearer"
    );
    f.raid(9000, &raid[0], 30, 30);
    f.raid(9001, &raid[1], 31, 31);
    f.step();
    let shape = Shape {
        ranged: healed.0,
        heal: healed.1,
    };
    assert_eq!(f.defender_requests().len(), healed.2 as usize);
    for i in 0..healed.2 {
        f.defender(shape, 20, 20 + u8::try_from(i).expect("small"));
    }
    f.w.hostiles.clear();
    f.step();
    assert!(the_remote(&f.w).threat.is_none());
    // The next raid: the living guards win alone, with one line and no request.
    f.raid(9100, &raid[0], 30, 30);
    f.raid(9101, &raid[1], 31, 31);
    f.step();
    assert!(
        the_remote(&f.w).threat.is_some(),
        "P6.3 still sends the crew home"
    );
    assert!(f.defender_requests().is_empty());
    assert!(
        f.defense
            .events()
            .iter()
            .any(|e| matches!(e, RemoteDefenseEvent::Held { have, .. } if *have == healed.2)),
        "{:?}",
        f.defense.events()
    );
}

/// The P7 gate's third remote scenario: a raid of big ranged invaders that no team within the cap
/// and the home's capacity beats gets one line and no request; the room waits (P6.3).
#[test]
fn scenario_a_raid_beyond_the_cap_waits_with_one_line() {
    let (mut f, _) = Fight::new();
    let raid = vec![big_ranged(), big_ranged(), small_healer()];
    assert!(cheapest(&raid, &[], 0, CAPACITY, f.cfg.max_remote_defenders, &f.cfg).is_none());
    for (i, p) in raid.iter().enumerate() {
        f.raid(
            9000 + i as u128,
            p,
            30,
            28 + u8::try_from(i).expect("small"),
        );
    }
    f.step();
    assert!(the_remote(&f.w).threat.is_some());
    assert!(f.defender_requests().is_empty());
    assert_eq!(
        f.defense.events(),
        [RemoteDefenseEvent::Outmatched {
            home: f.w.home,
            room: f.w.remote,
            foes: 2,
            cap: f.cfg.max_remote_defenders
        }]
    );
    f.step();
    assert!(f.defense.events().is_empty(), "once a threat");
}

/// Creeps that still see the room as a raider steps out do not end the wait: only a remote defender
/// standing in the room does.
#[test]
fn only_a_defender_in_the_room_ends_the_wait() {
    let (mut f, _) = Fight::new();
    f.raid(9000, &small_melee(), 30, 30);
    f.step();
    f.w.hostiles.clear();
    f.step();
    assert!(
        the_remote(&f.w).threat.is_some(),
        "the miners saw it go, the wait stands"
    );
    assert!(
        !f.defense
            .events()
            .iter()
            .any(|e| matches!(e, RemoteDefenseEvent::Cleared { .. }))
    );
    // A defender in another room does not either; one in the room does.
    let k = f.defender(Shape { ranged: 2, heal: 1 }, 25, 25);
    f.w.move_creep(k, pos(f.w.home, tile(20, 25)));
    f.step();
    assert!(the_remote(&f.w).threat.is_some());
    f.w.move_creep(k, pos(f.w.remote, tile(20, 25)));
    f.step();
    assert!(the_remote(&f.w).threat.is_none());
}

#[test]
fn a_team_dearer_than_the_wait_is_not_sent() {
    let (mut f, _) = Fight::new();
    // A wait of a few ticks saves almost nothing.
    f.w.cfg.evacuate_ticks = 5;
    f.raid(9000, &small_melee(), 30, 30);
    f.step();
    assert!(f.defender_requests().is_empty());
    assert!(
        f.defense
            .events()
            .iter()
            .any(|e| matches!(e, RemoteDefenseEvent::NotWorth { .. })),
        "{:?}",
        f.defense.events()
    );
}

#[test]
fn nothing_is_requested_off_or_without_the_standard_tier() {
    let (mut f, _) = Fight::new();
    f.raid(9000, &small_melee(), 30, 30);
    f.step_with(false);
    assert!(f.defender_requests().is_empty(), "no Standard tier");
    f.cfg.remote_defenders = false;
    f.step();
    assert!(f.defender_requests().is_empty(), "turned off");
    f.cfg.remote_defenders = true;
    f.step();
    assert!(!f.defender_requests().is_empty());
}

/// Threats a defender cleared do not count toward `remote.contest_threats`: a room raided again and
/// again, and held each time by its guard, stays mined.
#[test]
fn cleared_threats_do_not_abandon_a_room() {
    let (mut f, _) = Fight::new();
    f.w.cfg.contest_threats = 2;
    let (ranged, heal, _) =
        cheapest(&[small_melee()], &[], 0, CAPACITY, 1, &f.cfg).expect("one body wins");
    f.defender(Shape { ranged, heal }, 20, 20);
    for round in 0..3u128 {
        f.raid(9000 + round, &small_melee(), 30, 30);
        f.step();
        f.w.hostiles.clear();
        f.step();
        let r = the_remote(&f.w);
        assert!(r.threat.is_none() && r.abandoned.is_none(), "round {round}");
    }
    // Without the defense, the same raids abandon it (P6.3's rule stands).
    let (mut w, _, _) = staffed();
    w.cfg.contest_threats = 2;
    w.cfg.evacuate_ticks = 2;
    for round in 0..2u128 {
        let combat = Combat {
            attack: 1,
            ranged_attack: 1,
        };
        w.hostiles = vec![(9000 + round, pos(w.remote, tile(30, 30)), combat)];
        w.time += 1;
        w.begin();
        w.plan(true);
        w.hostiles.clear();
        for _ in 0..3 {
            w.time += 1;
            w.begin();
            w.plan(true);
        }
    }
    assert_eq!(
        the_remote(&w).abandoned.map(|a| a.reason),
        Some(AbandonReason::Threats)
    );
}

/// A remote room's hostiles are assessed for the remote defenders, but the owned rooms' threat
/// assessment (P7.1) leaves them out: no tower, safe mode or home defender acts on them.
#[test]
fn the_owned_threat_assessment_skips_remote_rooms() {
    let (mut f, _) = Fight::new();
    f.raid(9000, &small_melee(), 30, 30);
    f.step();
    assert!(!f.w.snap.creeps.assessed().is_empty());
    let mut threats = Threats::default();
    threats.assess(&f.w.snap, &DEFENSE_MMO);
    assert!(threats.rooms().is_empty(), "{:?}", threats.rooms());
    assert!(threats.events().is_empty());
}

/// A guard on an edge tile steps in (a creep left there is carried into the next room, V151), so it
/// needs a work unit; off the edge, whole, with nothing to fight, it needs none.
#[test]
fn a_guard_needs_a_unit_only_on_the_edge() {
    let (mut f, _) = Fight::new();
    let k = f.defender(Shape { ranged: 2, heal: 1 }, 0, 25);
    f.step();
    assert!(!f.defense.idle(&f.w.snap), "on the edge it steps in");
    f.w.move_creep(k, pos(f.w.remote, tile(20, 25)));
    f.step();
    assert!(f.defense.idle(&f.w.snap));
}

/// The combat-math audit's H2: bodies spawn one after another, so a room's defenders go in only
/// together. Against a small ranged invader with a small healer, one of the two bodies the team
/// needs cannot win alone (`5 × RANGED_ATTACK_POWER` against `5 × HEAL_POWER` of heal): it waits in
/// the home, and one already in the room walks out and shoots nothing; with the second out of its
/// spawn, both go in.
#[test]
fn defenders_go_in_only_together() {
    let (mut f, _) = Fight::new();
    let raid = vec![small_ranged(), small_healer()];
    let (ranged, heal, count) = cheapest(&raid, &[], 0, CAPACITY, 2, &f.cfg).expect("beatable");
    assert_eq!(count, 2, "the healed raid needs two bodies");
    let one = def(ranged, heal, f.cfg.remote_loss_pct);
    assert!(
        reference(&raid, &[one], f.cfg.max_kill_ticks).is_none(),
        "one alone loses"
    );
    f.raid(9000, &raid[0], 30, 30);
    f.raid(9001, &raid[1], 31, 31);
    let shape = Shape { ranged, heal };
    let home = f.w.home;
    let first = f.defender_at(shape, home, 20, 25, shape.hits());
    f.step();
    assert_eq!(f.engaged(), Some(false), "one ready body waits");
    // One in the room with the raid: it shoots nothing.
    f.w.move_creep(first, pos(f.w.remote, tile(27, 30)));
    let intents = f.step();
    assert_eq!(f.engaged(), Some(false));
    assert!(
        !intents
            .iter()
            .any(|&(a, i)| a == first && matches!(i, CreepIntent::RangedAttack { .. })),
        "{intents:?}"
    );
    f.defender_at(shape, home, 21, 25, shape.hits());
    let intents = f.step();
    assert_eq!(f.engaged(), Some(true), "both ready: they go in");
    assert!(
        intents
            .iter()
            .any(|&(a, i)| a == first && matches!(i, CreepIntent::RangedAttack { .. })),
        "{intents:?}"
    );
}

/// A defender at its floor leaves the fight, heals itself, and does not count in the team: the room
/// sizes as if it were not there, but it still counts against the cap.
#[test]
fn a_spent_defender_leaves_and_is_not_counted() {
    let (mut f, _) = Fight::new();
    let raid = vec![small_melee()];
    let (ranged, heal, _) = cheapest(&raid, &[], 0, CAPACITY, 2, &f.cfg).expect("beatable");
    let shape = Shape { ranged, heal };
    let floor = shape.fighter(f.cfg.remote_loss_pct).floor;
    let remote = f.w.remote;
    let spent = f.defender_at(shape, remote, 27, 30, floor);
    f.raid(9000, &raid[0], 30, 30);
    let intents = f.step();
    assert!(
        !intents
            .iter()
            .any(|&(a, i)| a == spent && matches!(i, CreepIntent::RangedAttack { .. })),
        "{intents:?}"
    );
    assert!(
        intents
            .iter()
            .any(|&(a, i)| a == spent && matches!(i, CreepIntent::Heal { target } if f.w.snap.creeps.key_of(target).map(|k| k.0) == Some(spent))),
        "it heals itself: {intents:?}"
    );
    let want =
        cheapest(&raid, &[], 1, CAPACITY, f.cfg.max_remote_defenders, &f.cfg).expect("beatable");
    let requests = f.defender_requests();
    assert_eq!(requests.len(), want.2 as usize);
    let body = Shape {
        ranged: want.0,
        heal: want.1,
    }
    .body()
    .expect("body");
    assert!(requests.iter().all(|(b, _)| *b == body));
}

/// A foe seen damaged with a healer by it is refilled while the team spawns and walks (V352): the
/// team is sized against it whole.
#[test]
fn a_damaged_foe_with_a_healer_is_sized_whole() {
    let (mut f, _) = Fight::new();
    let raid = vec![small_ranged(), small_healer()];
    f.raid(9000, &raid[0], 30, 30);
    f.raid(9001, &raid[1], 31, 31);
    // The ranged one at half its hits: its first five parts (2 TOUGH, 3 MOVE) are gone.
    if let Some(b) = f.w.bodies.iter_mut().find(|b| b.0 == 9000) {
        b.2 = full(&raid[0]) / 2;
    }
    f.step();
    let want = cheapest(&raid, &[], 0, CAPACITY, 2, &f.cfg).expect("beatable");
    let body = Shape {
        ranged: want.0,
        heal: want.1,
    }
    .body()
    .expect("body");
    let requests = f.defender_requests();
    assert_eq!(requests.len(), want.2 as usize);
    assert!(requests.iter().all(|(b, _)| *b == body));
}

/// The room has one target, so two defenders in reach of two armed foes shoot the same one (the
/// fewest effective hits, then the lowest id), never splitting their fire.
#[test]
fn defenders_share_one_target() {
    let (mut f, _) = Fight::new();
    f.raid(9000, &small_ranged(), 30, 30);
    f.raid(9001, &small_ranged(), 30, 31);
    let strong = Shape {
        ranged: 10,
        heal: 5,
    };
    let a = f.defender(strong, 27, 30);
    let b = f.defender(strong, 27, 31);
    let intents = f.step();
    assert_eq!(f.engaged(), Some(true));
    let target_of = |k: u128| {
        intents.iter().find_map(|&(a, i)| match i {
            CreepIntent::RangedAttack { target } if a == k => {
                f.w.snap.creeps.key_of(target).map(|x| x.0)
            }
            _ => None,
        })
    };
    assert_eq!(target_of(a), Some(9000));
    assert_eq!(target_of(b), Some(9000));
}

/// A guard whose room is no longer mined walks home and waits there.
#[test]
fn a_guard_of_a_room_no_longer_mined_goes_home() {
    let (mut f, _) = Fight::new();
    let remote = f.w.remote;
    let k = f.defender_at(
        Shape { ranged: 2, heal: 1 },
        remote,
        20,
        25,
        Shape { ranged: 2, heal: 1 }.hits(),
    );
    f.step();
    assert!(f.defense.idle(&f.w.snap), "guarding a mined room");
    // A foreign reservation contests the room: it is abandoned (P6.3).
    f.w.contest();
    f.step();
    assert!(the_remote(&f.w).abandoned.is_some());
    f.step();
    assert!(!f.defense.idle(&f.w.snap), "it walks home");
    f.w.move_creep(k, pos(f.w.home, tile(20, 25)));
    f.step();
    assert!(f.defense.idle(&f.w.snap), "it waits at home");
}

#[test]
fn plans_are_deterministic() {
    let run = || {
        let (mut f, _) = Fight::new();
        f.raid(9000, &small_melee(), 30, 30);
        f.raid(9001, &small_healer(), 31, 31);
        f.step();
        let shape = Shape { ranged: 6, heal: 2 };
        f.defender(shape, 27, 30);
        let intents = f.step();
        (
            f.defender_requests(),
            f.defense.events().to_vec(),
            intents,
            f.defense.posts().to_vec(),
        )
    };
    assert_eq!(run(), run());
}
