//! Safe-mode trigger tests (P7.4, ADR-035), among them the P7 gate's safe-mode trigger tests.
//! Expected values come from the constants (`ATTACK_POWER`, `RANGED_ATTACK_POWER`,
//! `DISMANTLE_POWER`, the structures' hits, `CONTROLLER_DOWNGRADE`,
//! `CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD`, the towers' constants, `BODYPART_HITS`) and the
//! engine's rules as the verification log reads them (V325 to V330), never from the code under
//! test.

use rustedscreep_consts::extra::BODYPART_HITS;
use rustedscreep_consts::{
    ATTACK_POWER, Boost, CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD, DISMANTLE_POWER, HEAL_POWER,
    Part, Position, RANGED_ATTACK_POWER, ResourceType, RoomCoordinate, RoomName,
    SAFE_MODE_DURATION, SPAWN_HITS, STORAGE_HITS, StructureType, TOWER_CAPACITY, TOWER_FALLOFF,
    TOWER_FALLOFF_RANGE, TOWER_HITS, TOWER_OPTIMAL_RANGE, TOWER_POWER_ATTACK, controller_downgrade,
};

use super::safe_mode::{
    Blocked, Reason, SafeModeEvent, SafeModes, blocked, claim_wins, falls_first, is_core,
    least_downgrade, reach_damage,
};
use super::threat::{Power, Threats};
use super::towers::TowerControl;
use crate::config::{DEFENSE_MMO, DefenseConfig};
use crate::snapshot::{
    BodyPartRead, BodyRead, Controller, ControllerState, CreepRow, ObjectKey, SafeModeState,
    Snapshot, StructureStatic, TickContext,
};

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

fn named(name: &str) -> RoomName {
    name.parse().expect("valid room name")
}

fn room() -> RoomName {
    named("W1N1")
}

fn at_in(r: RoomName, x: u8, y: u8) -> Position {
    Position::new(
        RoomCoordinate::new(x).expect("in room"),
        RoomCoordinate::new(y).expect("in room"),
        r,
    )
}

fn at(x: u8, y: u8) -> Position {
    at_in(room(), x, y)
}

/// The engine's tower falloff, operations in its order (V239), written out from the constants.
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

/// The game side's downgrade check in doubles, as `game/structures.js` writes it (V325): refused when
/// `ticksToDowngrade < CONTROLLER_DOWNGRADE[level]/2 - CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD`.
fn js_downgrade_refuses(level: u8, ticks: u32) -> bool {
    let full = f64::from(controller_downgrade(level).expect("level"));
    f64::from(ticks) < full / 2.0 - f64::from(CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD)
}

fn state(level: u8, safe: SafeModeState) -> ControllerState {
    ControllerState {
        progress: 0,
        downgrade_ticks: controller_downgrade(level).expect("level"),
        upgrade_blocked: false,
        safe_mode: Some(safe),
    }
}

fn charges(n: u32) -> SafeModeState {
    SafeModeState {
        ticks: 0,
        available: n,
        cooldown: 0,
    }
}

#[test]
fn the_downgrade_bound_is_the_game_sides() {
    for level in 1..=8u8 {
        let least = least_downgrade(level);
        for ticks in least.saturating_sub(3)..=least + 3 {
            let mut s = state(level, charges(1));
            s.downgrade_ticks = ticks;
            let refused = matches!(blocked(level, &s), Some(Blocked::Downgrade { .. }));
            assert_eq!(
                refused,
                js_downgrade_refuses(level, ticks),
                "level {level} ticks {ticks}"
            );
        }
    }
}

#[test]
fn blocked_follows_the_game_sides_order() {
    let level = 5;
    let mut s = state(level, charges(0));
    s.upgrade_blocked = true;
    s.safe_mode = Some(SafeModeState {
        ticks: 0,
        available: 0,
        cooldown: 7,
    });
    // ERR_NOT_ENOUGH_RESOURCES comes before ERR_TIRED (V325).
    assert_eq!(blocked(level, &s), Some(Blocked::NoCharges));
    s.safe_mode = Some(SafeModeState {
        ticks: 0,
        available: 1,
        cooldown: 7,
    });
    assert_eq!(blocked(level, &s), Some(Blocked::Cooldown { ticks: 7 }));
    s.safe_mode = Some(charges(1));
    assert_eq!(blocked(level, &s), Some(Blocked::UpgradeBlocked));
    s.upgrade_blocked = false;
    assert_eq!(blocked(level, &s), None);
    s.safe_mode = Some(SafeModeState {
        ticks: 12,
        available: 1,
        cooldown: 0,
    });
    // An active safe mode is ERR_BUSY, the last check.
    assert_eq!(blocked(level, &s), Some(Blocked::Active { ticks: 12 }));
    s.safe_mode = None;
    assert_eq!(blocked(level, &s), Some(Blocked::NotRead));
}

#[test]
fn the_core_is_spawns_towers_storage_and_terminal() {
    for kind in [
        StructureType::Spawn,
        StructureType::Tower,
        StructureType::Storage,
        StructureType::Terminal,
    ] {
        assert!(is_core(kind), "{kind:?}");
    }
    for kind in [
        StructureType::Extension,
        StructureType::Rampart,
        StructureType::Controller,
        StructureType::Link,
        StructureType::Container,
    ] {
        assert!(!is_core(kind), "{kind:?}");
    }
}

#[test]
fn reach_damage_counts_each_action_at_its_reach_plus_the_lead() {
    let melee = Power {
        attack: 4,
        ..Power::default()
    };
    let ranged = Power {
        ranged: 3,
        ..Power::default()
    };
    let breaker = Power {
        attack: 2,
        dismantle: 5,
        ..Power::default()
    };
    for lead in [1u32, 2] {
        // Melee: range 1, plus the lead.
        assert_eq!(reach_damage(melee, 1 + lead, lead), 4 * ATTACK_POWER);
        assert_eq!(reach_damage(melee, 2 + lead, lead), 0);
        // Ranged: range 3, plus the lead.
        assert_eq!(
            reach_damage(ranged, 3 + lead, lead),
            3 * RANGED_ATTACK_POWER
        );
        assert_eq!(reach_damage(ranged, 4 + lead, lead), 0);
        // `dismantle` blocks `attack` on the same creep (V44): the larger of the two.
        assert_eq!(
            reach_damage(breaker, 1, lead),
            (2 * ATTACK_POWER).max(5 * DISMANTLE_POWER)
        );
        // Neither blocks `rangedAttack`: both add.
        let both = melee.plus(ranged);
        assert_eq!(
            reach_damage(both, 1, lead),
            4 * ATTACK_POWER + 3 * RANGED_ATTACK_POWER
        );
        // At range 3 the melee counts only with a lead of 2 or more.
        let at3 = if lead >= 2 {
            4 * ATTACK_POWER + 3 * RANGED_ATTACK_POWER
        } else {
            3 * RANGED_ATTACK_POWER
        };
        assert_eq!(reach_damage(both, 3, lead), at3);
    }
    // A healer or a claimer deals no damage to a structure.
    let other = Power {
        heal: 5,
        claim: 2,
        ..Power::default()
    };
    assert_eq!(reach_damage(other, 1, 1), 0);
}

#[test]
fn falls_first_compares_the_damage_until_the_kills_with_the_hits() {
    let hits = SPAWN_HITS;
    assert!(!falls_first(hits, 0, None), "no hostile in reach");
    assert!(falls_first(hits, 1, None), "the towers never win");
    let d = 10 * ATTACK_POWER;
    let k = hits.div_ceil(d);
    assert!(falls_first(hits, d, Some(k)));
    assert!(!falls_first(hits, d, Some(k - 1)));
    // No overflow on extreme inputs.
    assert!(falls_first(u32::MAX, u32::MAX, Some(u32::MAX)));
}

#[test]
fn a_claim_creep_wins_when_it_can_hit_after_this_tick() {
    // At range 1 it may hit this tick, which cancels an activation (V326); one it survives into
    // the next tick is worth stopping.
    assert!(!claim_wins(1, Some(1)));
    assert!(claim_wins(1, Some(2)));
    // At range 2 it first hits next tick.
    assert!(!claim_wins(2, Some(1)));
    assert!(claim_wins(2, Some(2)));
    assert!(!claim_wins(3, Some(2)));
    assert!(claim_wins(3, Some(3)));
    assert!(claim_wins(2, None));
}

/// One tick's scenario world: owned rooms with a controller, core structures, towers, and hostiles.
#[derive(Clone)]
struct Site {
    key: u128,
    kind: StructureType,
    pos: Position,
    hits: u32,
    energy: u32,
}

#[derive(Clone)]
struct Foe {
    key: u128,
    pos: Position,
    parts: Vec<BodyPartRead>,
    hits: u32,
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

fn site(key: u128, kind: StructureType, x: u8, y: u8, hits: u32) -> Site {
    Site {
        key,
        kind,
        pos: at(x, y),
        hits,
        energy: TOWER_CAPACITY,
    }
}

/// The controller at (25, 25), a spawn at (10, 10).
fn base() -> Vec<Site> {
    vec![
        site(900, StructureType::Controller, 25, 25, 0),
        site(901, StructureType::Spawn, 10, 10, SPAWN_HITS),
    ]
}

struct World {
    snap: Snapshot,
    threats: Threats,
    towers: TowerControl,
    safe: SafeModes,
    cfg: DefenseConfig,
    level: u8,
    /// Per room, the controller's safe-mode state as the snapshot would read it.
    states: Vec<(RoomName, SafeModeState)>,
}

impl World {
    fn new() -> Self {
        Self {
            snap: Snapshot::default(),
            threats: Threats::default(),
            towers: TowerControl::default(),
            safe: SafeModes::default(),
            cfg: DEFENSE_MMO,
            level: 5,
            states: vec![(room(), charges(2))],
        }
    }

    /// Takes the tick's snapshot as the wasm glue does, then runs the assessment, the towers, and
    /// the safe-mode triggers, as `DefenseGlue::plan_towers` does.
    fn tick(&mut self, time: u32, sites: &[Site], foes: &[Foe]) {
        let snap = &mut self.snap;
        snap.begin(TickContext::from_raw(time, 20, 500.0, 10_000));
        for &(r, safe) in &self.states {
            let row = snap.rooms.push(r, Controller::Mine { level: self.level });
            let mut s = state(self.level, safe);
            s.downgrade_ticks = controller_downgrade(self.level).expect("level");
            snap.rooms.set_controller_state(row, s);
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
        for s in sites {
            let handle = snap
                .structures
                .push(ObjectKey(s.key), || {
                    Ok(StructureStatic {
                        kind: s.kind,
                        pos: s.pos,
                    })
                })
                .expect("structure");
            let row = snap.structures.len() - 1;
            snap.structures.set_hits(row, s.hits);
            if s.kind == StructureType::Tower {
                let r = u32::try_from(row).expect("small");
                snap.towers.push(handle, r, s.energy);
            }
        }
        snap.finish();
        self.threats.assess(&self.snap, &self.cfg);
        self.towers.begin(&self.snap, &self.threats, &self.cfg);
        for unit in 0..self.towers.unit_count() {
            self.towers
                .plan_room(unit, &self.snap, &self.threats, &self.cfg)
                .expect("towers");
        }
        self.towers.finish(time, &self.threats);
        self.safe.begin(&self.snap, &self.threats, &self.cfg);
        for unit in 0..self.safe.unit_count() {
            self.safe
                .plan_room(unit, &self.snap, &self.threats, &self.towers, &self.cfg)
                .expect("safe mode");
        }
        self.safe.finish(time, &self.threats);
    }

    fn reasons(&self) -> Vec<Reason> {
        self.safe.orders().iter().map(|o| o.reason).collect()
    }

    /// The structure key of this tick's order's controller row.
    fn ordered_controller(&self) -> Option<u128> {
        let st = &self.snap.structures;
        self.safe.orders().first().map(|o| {
            let h = st.handles()[usize::try_from(o.controller).expect("small")];
            st.key_of(h).expect("live").0
        })
    }
}

fn melee(attack: u32) -> Vec<BodyPartRead> {
    body(&[(Part::Attack, attack), (Part::Move, attack)])
}

/// P7 gate: with no tower, a melee hostile one step beyond reach of the spawn triggers safe mode on
/// the room's controller, and the reason names the spawn, its hits, and the damage.
#[test]
fn scenario_no_tower_and_a_melee_hostile_near_the_spawn() {
    let mut w = World::new();
    let parts = melee(5);
    // Range 2 from the spawn at (10, 10): melee reach 1 plus the lead of 1.
    w.tick(100, &base(), &[foe(1, 12, 10, parts.clone())]);
    assert_eq!(w.ordered_controller(), Some(900));
    assert_eq!(
        w.reasons(),
        [Reason::Core {
            kind: StructureType::Spawn,
            at: at(10, 10),
            hits: SPAWN_HITS,
            hostiles: 1,
            damage: 5 * ATTACK_POWER,
            towers: 0,
            kill: None,
            wait: 0,
        }]
    );
    // One step farther, nothing is in reach.
    let mut w = World::new();
    w.tick(100, &base(), &[foe(1, 13, 10, parts)]);
    assert!(w.safe.orders().is_empty());
    assert!(w.safe.events().is_empty());
}

/// P7 gate: a tower that kills a light melee hostile before it can bring the spawn down spends no
/// safe mode; the same tower against a heavy one beside a tower (`TOWER_HITS`) does.
#[test]
fn scenario_the_towers_win_or_lose_the_race() {
    let tower_at = (10u8, 14u8);
    let mut sites = base();
    sites.push(site(
        902,
        StructureType::Tower,
        tower_at.0,
        tower_at.1,
        TOWER_HITS,
    ));
    // A light attacker beside the spawn, at range 4 of the tower: the sim's kill is its hits over
    // the tower's damage there, and its damage over those ticks is far below SPAWN_HITS.
    let light = melee(2);
    let hits = full(&light);
    let d = falloff(4);
    let k = hits.div_ceil(d);
    assert!(2 * ATTACK_POWER * k < SPAWN_HITS, "the premise");
    let mut w = World::new();
    w.tick(100, &sites, &[foe(1, 11, 10, light)]);
    assert!(w.safe.orders().is_empty(), "the tower wins");
    // A heavy attacker (TOUGH first) beside the tower, at range 1 of it.
    let heavy = body(&[(Part::Tough, 10), (Part::Attack, 20), (Part::Move, 20)]);
    let hits = full(&heavy);
    let k = hits.div_ceil(falloff(1));
    let damage = 20 * ATTACK_POWER;
    assert!(damage * k >= TOWER_HITS, "the premise");
    let mut w = World::new();
    w.tick(100, &sites, &[foe(2, 11, 14, heavy)]);
    assert_eq!(
        w.reasons(),
        [Reason::Core {
            kind: StructureType::Tower,
            at: at(tower_at.0, tower_at.1),
            hits: TOWER_HITS,
            hostiles: 1,
            damage,
            towers: 1,
            kill: Some(k),
            wait: 0,
        }]
    );
}

/// P7 gate: a structure's hits read this tick decide it. A damaged spawn falls first to an attacker
/// the towers would beat at full hits.
#[test]
fn scenario_a_damaged_spawn_falls_first() {
    let parts = melee(4);
    let hits = full(&parts);
    let d = falloff(4);
    let k = hits.div_ceil(d);
    let damage = 4 * ATTACK_POWER;
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 10, 14, TOWER_HITS));
    let mut w = World::new();
    w.tick(100, &sites, &[foe(1, 11, 10, parts.clone())]);
    assert!(w.safe.orders().is_empty(), "full hits: the tower wins");
    sites[1].hits = damage * k;
    let mut w = World::new();
    w.tick(100, &sites, &[foe(1, 11, 10, parts)]);
    assert!(matches!(
        w.reasons().as_slice(),
        [Reason::Core {
            kind: StructureType::Spawn,
            ..
        }]
    ));
}

/// P7 gate: a CLAIM creep within 2 tiles of the controller triggers safe mode unless the towers kill
/// it this tick; at 3 tiles it does not yet.
#[test]
fn scenario_a_claim_creep_near_the_controller() {
    let claimer = body(&[(Part::Claim, 1), (Part::Move, 1)]);
    let mut w = World::new();
    w.tick(100, &base(), &[foe(1, 27, 25, claimer.clone())]);
    assert_eq!(
        w.reasons(),
        [Reason::Claim {
            row: 0,
            claim: 1,
            range: 2,
            towers: 0,
            kill: None,
        }]
    );
    let mut w = World::new();
    w.tick(100, &base(), &[foe(1, 28, 25, claimer.clone())]);
    assert!(w.safe.orders().is_empty(), "range 3");
    // A tower at range 1 of the claimer kills it this tick (2 parts' hits under TOWER_POWER_ATTACK).
    assert!(full(&claimer) <= TOWER_POWER_ATTACK, "the premise");
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 28, 25, TOWER_HITS));
    let mut w = World::new();
    w.tick(100, &sites, &[foe(1, 27, 25, claimer)]);
    assert!(w.safe.orders().is_empty(), "killed this tick");
}

/// P7 gate: a trigger the room cannot meet is reported once per threat, with the reason, and makes
/// no call.
#[test]
fn scenario_no_charges_left() {
    let mut w = World::new();
    w.states = vec![(room(), charges(0))];
    let f = foe(1, 11, 10, melee(5));
    w.tick(100, &base(), std::slice::from_ref(&f));
    assert!(w.safe.orders().is_empty());
    assert!(matches!(
        w.safe.events(),
        [SafeModeEvent::Unavailable {
            blocked: Blocked::NoCharges,
            ..
        }]
    ));
    w.tick(101, &base(), std::slice::from_ref(&f));
    assert!(w.safe.events().is_empty(), "once per threat");
}

/// P7 gate: one call per room per threat, whatever it returns; a new threat may call again.
#[test]
fn scenario_one_call_per_threat() {
    let mut w = World::new();
    let f = foe(1, 11, 10, melee(5));
    w.tick(100, &base(), std::slice::from_ref(&f));
    assert_eq!(w.safe.orders().len(), 1);
    // The game refused it: the room reads no safe mode on the next tick.
    let cfg = w.cfg;
    w.safe.not_accepted(room(), false, &cfg);
    w.tick(101, &base(), std::slice::from_ref(&f));
    assert!(w.safe.orders().is_empty(), "the same threat");
    // The threat ends after `threat_end_ticks` without a hostile; a new one starts.
    let gone = 101 + w.cfg.threat_end_ticks;
    w.tick(gone, &base(), &[]);
    assert!(w.threats.under_threat(room()).is_none(), "the premise");
    w.tick(gone + 1, &base(), std::slice::from_ref(&f));
    assert_eq!(w.safe.orders().len(), 1, "a new threat");
}

/// P7 gate: a threat in a room already in its safe mode makes no call and says so once.
#[test]
fn scenario_already_in_safe_mode() {
    let mut w = World::new();
    w.states = vec![(
        room(),
        SafeModeState {
            ticks: SAFE_MODE_DURATION - 10,
            available: 1,
            cooldown: 0,
        },
    )];
    let f = foe(1, 11, 10, melee(5));
    w.tick(100, &base(), std::slice::from_ref(&f));
    assert!(w.safe.orders().is_empty());
    assert_eq!(
        w.safe.events(),
        [SafeModeEvent::Protected {
            room: room(),
            ticks: SAFE_MODE_DURATION - 10
        }]
    );
    w.tick(101, &base(), std::slice::from_ref(&f));
    assert!(w.safe.events().is_empty(), "once per threat");
}

/// P7 gate: only one owned room may be in safe mode (V325's ERR_BUSY): a room read in safe mode
/// blocks another's trigger, and of two rooms that trigger on one tick only the first is ordered.
#[test]
fn scenario_another_room_in_safe_mode() {
    let other = named("W2N1");
    let mut w = World::new();
    w.states = vec![
        (room(), charges(1)),
        (
            other,
            SafeModeState {
                ticks: 500,
                available: 1,
                cooldown: 0,
            },
        ),
    ];
    let mut f = foe(2, 30, 30, melee(1));
    f.pos = at_in(other, 30, 30);
    // The other room's threat, so its state is read and remembered.
    w.tick(100, &base(), std::slice::from_ref(&f));
    assert!(matches!(
        w.safe.events(),
        [SafeModeEvent::Protected { room, .. }] if *room == other
    ));
    let g = foe(1, 11, 10, melee(5));
    w.tick(101, &base(), &[f, g]);
    assert!(w.safe.orders().is_empty());
    assert!(w.safe.events().iter().any(|e| matches!(
        e,
        SafeModeEvent::Unavailable {
            blocked: Blocked::OtherRoom { room },
            ..
        } if *room == other
    )));
    // Two rooms that trigger on the same tick: only the first judged (room order) is ordered, and
    // the other is told the first has it.
    let mut w = World::new();
    w.states = vec![(room(), charges(1)), (other, charges(1))];
    let g = foe(1, 11, 10, melee(5));
    let mut h = foe(3, 11, 10, melee(5));
    h.pos = at_in(other, 11, 10);
    let mut sites = base();
    sites.push(Site {
        key: 911,
        kind: StructureType::Spawn,
        pos: at_in(other, 10, 10),
        hits: SPAWN_HITS,
        energy: 0,
    });
    sites.push(Site {
        key: 910,
        kind: StructureType::Controller,
        pos: at_in(other, 25, 25),
        hits: 0,
        energy: 0,
    });
    w.tick(100, &sites, &[g, h]);
    let first = w.threats.rooms()[0].room;
    let rooms: Vec<RoomName> = w.safe.orders().iter().map(|o| o.room).collect();
    assert_eq!(rooms, [first]);
    assert!(w.safe.events().iter().any(|e| matches!(
        e,
        SafeModeEvent::Unavailable {
            blocked: Blocked::OtherRoom { room: r },
            ..
        } if *r == first
    )));
}

/// `defense.safe_mode` off: no room is judged.
#[test]
fn scenario_turned_off() {
    let mut w = World::new();
    w.cfg.safe_mode = false;
    w.tick(100, &base(), &[foe(1, 11, 10, melee(5))]);
    assert!(w.safe.orders().is_empty());
    assert!(w.safe.events().is_empty());
}

/// A core structure whose hits were not read (0) cannot be judged and fires nothing.
#[test]
fn scenario_unread_hits_are_not_judged() {
    let mut sites = base();
    sites[1].hits = 0;
    let mut w = World::new();
    w.tick(100, &sites, &[foe(1, 11, 10, melee(5))]);
    assert!(w.safe.orders().is_empty());
}

/// In the bot's own safe mode no hostile heals (V300), so the towers fire at a pair whose heal
/// would otherwise make them hold.
#[test]
fn scenario_towers_fire_through_heal_in_own_safe_mode() {
    // An attacker with a healer beside it whose heal beats one far tower's damage.
    let attacker = melee(5);
    let healer = body(&[(Part::Heal, 20), (Part::Move, 20)]);
    let far = 30u32;
    assert!(
        20 * rustedscreep_consts::HEAL_POWER > falloff(far),
        "the premise"
    );
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 5, 5, TOWER_HITS));
    let foes = [foe(1, 35, 5, attacker), foe(2, 36, 5, healer)];
    let mut w = World::new();
    w.tick(100, &sites, &foes);
    assert!(w.towers.orders().is_empty(), "the heal wins: hold fire");
    let mut w = World::new();
    w.states = vec![(
        room(),
        SafeModeState {
            ticks: 100,
            available: 0,
            cooldown: 0,
        },
    )];
    w.tick(100, &sites, &foes);
    assert!(!w.towers.orders().is_empty(), "no heal in own safe mode");
}

/// A dismantler's WORK parts boosted for `dismantle` deal the compound's multiplier times
/// `DISMANTLE_POWER` each (V288; the multiplier from the `BOOSTS` table).
#[test]
fn a_boosted_dismantler_deals_its_multiplier() {
    let Some(Boost::Dismantle(m)) = ResourceType::CatalyzedZynthiumAcid.boost() else {
        panic!("XZH2O boosts dismantle");
    };
    let mut power = Power::default();
    for _ in 0..10 {
        power.add_part(BodyPartRead {
            part: Part::Work,
            boost: Some(ResourceType::CatalyzedZynthiumAcid),
        });
    }
    assert_eq!(reach_damage(power, 1, 1), 10 * m * DISMANTLE_POWER);
    assert_eq!(reach_damage(power, 3, 1), 0);
}

/// The formulas' directions: more damage or a longer kill never saves a structure that fell, more
/// hits never makes one fall; a slower kill never stops a CLAIM trigger; damage never grows with
/// range and never shrinks with the lead.
#[test]
fn the_formulas_move_the_right_way() {
    let hits = SPAWN_HITS;
    for d in (0..=600u32).step_by(30) {
        for k in 0..=20u32 {
            if falls_first(hits, d, Some(k)) {
                assert!(falls_first(hits, d + 30, Some(k)), "d {d} k {k}");
                assert!(falls_first(hits, d, Some(k + 1)), "d {d} k {k}");
                assert!(falls_first(hits, d, None), "d {d} k {k}");
                assert!(falls_first(hits - 1, d, Some(k)), "d {d} k {k}");
            }
        }
    }
    for r in 1..=5u32 {
        for k in 1..=6u32 {
            if claim_wins(r, Some(k)) {
                assert!(claim_wins(r, Some(k + 1)), "r {r} k {k}");
            }
        }
    }
    let p = Power {
        attack: 3,
        ranged: 2,
        dismantle: 4,
        ..Power::default()
    };
    for lead in 1..=4u32 {
        for r in 0..=10u32 {
            assert!(
                reach_damage(p, r + 1, lead) <= reach_damage(p, r, lead),
                "r {r}"
            );
            assert!(
                reach_damage(p, r, lead + 1) >= reach_damage(p, r, lead),
                "r {r}"
            );
        }
    }
}

/// Audit H1: a CLAIM creep the towers could kill this tick alone, while their focus stays on
/// another dangerous hostile, still triggers: only the towers that fire at it count.
#[test]
fn scenario_a_claim_creep_the_towers_do_not_shoot() {
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 40, 40, TOWER_HITS));
    // A heavy attacker at range 5 of the tower (out of every core structure's reach) takes the
    // towers' focus first.
    let heavy = foe(
        1,
        45,
        45,
        body(&[(Part::Tough, 10), (Part::Attack, 20), (Part::Move, 20)]),
    );
    let claimer = foe(2, 27, 25, body(&[(Part::Claim, 1), (Part::Move, 1)]));
    // Alone, the claimer dies in one tick at range 15 of the tower.
    assert!(claimer.hits <= falloff(15), "the premise");
    let mut w = World::new();
    w.tick(100, &sites, std::slice::from_ref(&heavy));
    assert!(w.safe.orders().is_empty());
    w.tick(101, &sites, &[heavy.clone(), claimer.clone()]);
    assert_eq!(
        w.reasons(),
        [Reason::Claim {
            row: 1,
            claim: 1,
            range: 2,
            towers: 0,
            kill: None,
        }]
    );
    // When the towers do fire at it and kill it this tick, nothing triggers.
    let mut w = World::new();
    w.tick(101, &sites, std::slice::from_ref(&claimer));
    assert!(w.safe.orders().is_empty());
}

/// Audit H2: an accepted call that does not take effect (the cooldown's last tick, V338, or a
/// same-tick `attackController`, V326) is made again while a read shows no safe mode and the same
/// activations left, up to `defense.safe_mode_calls` calls in the threat.
#[test]
fn scenario_a_call_that_does_not_take_effect() {
    let mut w = World::new();
    let f = foe(1, 11, 10, melee(5));
    let calls = w.cfg.safe_mode_calls;
    for k in 0..calls {
        w.tick(100 + k, &base(), std::slice::from_ref(&f));
        assert_eq!(w.safe.orders().len(), 1, "call {k}");
        let not_applied = w
            .safe
            .events()
            .iter()
            .filter(|e| matches!(e, SafeModeEvent::NotApplied { again: true, .. }))
            .count();
        assert_eq!(not_applied, usize::from(k > 0), "tick {k}");
    }
    w.tick(100 + calls, &base(), std::slice::from_ref(&f));
    assert!(w.safe.orders().is_empty(), "no call left");
    assert_eq!(
        w.safe.events(),
        [SafeModeEvent::NotApplied {
            room: room(),
            again: false
        }]
    );
    // A call that took effect is not made again.
    let mut w = World::new();
    w.tick(100, &base(), std::slice::from_ref(&f));
    assert_eq!(w.safe.orders().len(), 1);
    w.states = vec![(
        room(),
        SafeModeState {
            ticks: SAFE_MODE_DURATION - 1,
            available: 1,
            cooldown: 0,
        },
    )];
    w.tick(101, &base(), std::slice::from_ref(&f));
    assert!(w.safe.orders().is_empty());
    assert!(matches!(w.safe.events(), [SafeModeEvent::Protected { .. }]));
}

/// A call the game refused is not made again in the threat; one that threw is.
#[test]
fn scenario_refused_and_thrown_calls() {
    let f = foe(1, 11, 10, melee(5));
    let mut w = World::new();
    w.tick(100, &base(), std::slice::from_ref(&f));
    assert_eq!(w.safe.orders().len(), 1);
    let cfg = w.cfg;
    w.safe.not_accepted(room(), false, &cfg);
    w.tick(101, &base(), std::slice::from_ref(&f));
    assert!(w.safe.orders().is_empty(), "refused");
    assert!(w.safe.events().is_empty());
    let mut w = World::new();
    w.tick(100, &base(), std::slice::from_ref(&f));
    w.safe.not_accepted(room(), true, &cfg);
    w.tick(101, &base(), std::slice::from_ref(&f));
    assert_eq!(w.safe.orders().len(), 1, "threw: again");
}

/// Two attackers in reach of one spawn: their damage a tick adds up, and so do their kill times
/// (one after another). Hits of exactly the product fall first; one more does not.
#[test]
fn scenario_two_attackers_sum_their_damage_and_kills() {
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 10, 14, TOWER_HITS));
    let parts = melee(2);
    let (a, b) = (foe(1, 11, 10, parts.clone()), foe(2, 9, 10, parts.clone()));
    // Both at range 4 of the tower.
    let k = full(&parts).div_ceil(falloff(4));
    let d = 2 * ATTACK_POWER;
    let bound = (d + d) * (k + k);
    sites[1].hits = bound;
    let mut w = World::new();
    w.tick(100, &sites, &[a.clone(), b.clone()]);
    assert_eq!(
        w.reasons(),
        [Reason::Core {
            kind: StructureType::Spawn,
            at: at(10, 10),
            hits: bound,
            hostiles: 2,
            damage: 2 * d,
            towers: 1,
            kill: Some(2 * k),
            wait: 0,
        }]
    );
    sites[1].hits = bound + 1;
    let mut w = World::new();
    w.tick(100, &sites, &[a, b]);
    assert!(w.safe.orders().is_empty());
}

/// A hostile the towers stop trusting (the shot check: its hits do not fall) counts as never
/// killed, so the spawn it reaches falls first once the towers give up on it.
#[test]
fn scenario_a_distrusted_attacker() {
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 10, 14, TOWER_HITS));
    let f = foe(1, 11, 10, melee(10));
    let k = f.hits.div_ceil(falloff(4));
    assert!(
        10 * ATTACK_POWER * k < SPAWN_HITS,
        "the premise: the tower wins while it shoots"
    );
    let mut w = World::new();
    let miss = w.cfg.miss_ticks;
    for t in 0..miss {
        // Its hits stay full: the shots land elsewhere.
        w.tick(1000 + t, &sites, std::slice::from_ref(&f));
        assert!(w.safe.orders().is_empty(), "tick {t}");
    }
    w.tick(1000 + miss, &sites, std::slice::from_ref(&f));
    assert!(matches!(
        w.reasons().as_slice(),
        [Reason::Core {
            towers: 1,
            kill: None,
            ..
        }]
    ));
}

/// The engine's order on one creep with no TOUGH part (V287): `damage` lands, then `heal`, capped at
/// `hits_max`; it dies once its hits come to 0 or less. The ticks until it dies from `hits`, or
/// `None` within `limit`.
fn kill_by_hand(hits: u32, hits_max: u32, damage: u32, heal: u32, limit: u32) -> Option<u32> {
    let mut h = i64::from(hits);
    for tick in 1..=limit {
        h = h - i64::from(damage) + i64::from(heal);
        if h <= 0 {
            return Some(tick);
        }
        h = h.min(i64::from(hits_max));
    }
    None
}

/// The P7 gate audit's high finding: the towers keep a dangerous focus they kill within
/// `defense.max_kill_ticks` (P7.2), so a dismantler beside the spawn that one tower would kill in
/// one tick is not shot while they finish a healed tank out of the spawn's reach. The spawn falls
/// before the tank dies, so the trigger waits for the tank: its kill time by the towers' plan comes
/// first in K. A quick tank does not make it fire.
#[test]
fn scenario_core_falls_while_the_towers_keep_another_focus() {
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 10, 14, TOWER_HITS));
    // The tank: one RANGED_ATTACK part, last so that damage (which takes the front parts first,
    // V283) leaves it armed, makes it dangerous; at range 5 of the tower (full power) and 5 of the
    // spawn it is out of reach of both (3 plus the lead).
    let tank_parts = body(&[(Part::Move, 8), (Part::Heal, 40), (Part::RangedAttack, 1)]);
    let tank_at = (15u8, 14u8);
    let heal = 40 * HEAL_POWER;
    let shot = falloff(5);
    let hits_max = full(&tank_parts);
    assert!(shot > heal, "the premise: the tower beats the tank's heal");
    // The dismantler beside the spawn, at range 4 of the tower: one shot kills it.
    let dismantler = body(&[(Part::Work, 3), (Part::Move, 3)]);
    let d = 3 * DISMANTLE_POWER;
    assert!(full(&dismantler) <= falloff(4), "the premise: one shot");
    assert!(
        d < SPAWN_HITS,
        "the premise: the old bound (a one-tick kill) never fired"
    );
    // Tick 1: the tank alone becomes the towers' focus. Tick 2: its hits as the sim expected (no
    // miss for the shot check), and the dismantler arrives.
    let mut tank = foe(1, tank_at.0, tank_at.1, tank_parts);
    let mut w = World::new();
    w.tick(100, &sites, std::slice::from_ref(&tank));
    assert!(
        w.safe.orders().is_empty(),
        "the tank reaches no core structure"
    );
    tank.hits = hits_max - shot + heal;
    let limit = w.cfg.max_kill_ticks;
    let wait = kill_by_hand(tank.hits, hits_max, shot, heal, limit).expect("the premise: killable");
    assert!(
        SPAWN_HITS.div_ceil(d) < wait,
        "the premise: the spawn falls before the tank dies"
    );
    w.tick(
        101,
        &sites,
        &[tank.clone(), foe(2, 11, 10, dismantler.clone())],
    );
    // The towers' plan: one tower on the tank, none on the dismantler.
    assert_eq!(w.towers.firing_at(0).count(), 1, "the tank is shot");
    assert_eq!(w.towers.firing_at(1).count(), 0, "the dismantler is not");
    assert_eq!(
        w.reasons(),
        [Reason::Core {
            kind: StructureType::Spawn,
            at: at(10, 10),
            hits: SPAWN_HITS,
            hostiles: 1,
            damage: d,
            towers: 1,
            // The tank's kill first, then the dismantler's one shot.
            kill: Some(wait + 1),
            wait,
        }]
    );
    // A tank with no heal dies in a few ticks: the spawn holds, and nothing fires.
    let quick_parts = body(&[(Part::Move, 8), (Part::RangedAttack, 1)]);
    let quick_max = full(&quick_parts);
    let mut quick = foe(1, tank_at.0, tank_at.1, quick_parts);
    let mut w = World::new();
    w.tick(100, &sites, std::slice::from_ref(&quick));
    quick.hits = quick_max - shot;
    let wait = kill_by_hand(quick.hits, quick_max, shot, 0, limit).expect("killable");
    assert!(d * (wait + 1) < SPAWN_HITS, "the premise");
    w.tick(101, &sites, &[quick, foe(2, 11, 10, dismantler)]);
    assert!(w.safe.orders().is_empty(), "{:?}", w.reasons());
}

/// The audit of the fix (a new medium): when the towers shoot a healer first (P7.2's attacker and
/// healer pair), the hostiles in reach are killed after it, without its heal. Counting that heal as
/// well as the wait spent a safe mode on a storage that holds. Hits of exactly the towers' order
/// (the healer, then the dismantler without its heal) still fall first.
#[test]
fn scenario_a_healer_shot_first_counts_once() {
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 10, 14, TOWER_HITS));
    sites.push(site(903, StructureType::Storage, 13, 14, STORAGE_HITS));
    // A dismantler beside the storage, at range 4 of the tower, and a healer of XLHO2-boosted HEAL
    // parts beside it at range 5: both in the tower's full power.
    let dismantler = body(&[(Part::Work, 11), (Part::Move, 9)]);
    let Some(Boost::Heal(m)) = ResourceType::CatalyzedLemergiumAlkalide.boost() else {
        panic!("XLHO2 boosts heal");
    };
    let healer: Vec<BodyPartRead> = (0..10)
        .map(|_| BodyPartRead {
            part: Part::Heal,
            boost: Some(ResourceType::CatalyzedLemergiumAlkalide),
        })
        .chain(body(&[(Part::Move, 3)]))
        .collect();
    let heal = 10 * m * HEAL_POWER;
    let (d_max, h_max) = (full(&dismantler), full(&healer));
    let limit = DEFENSE_MMO.max_kill_ticks;
    let first = kill_by_hand(h_max, h_max, falloff(5), heal, limit).expect("killable");
    let after = d_max.div_ceil(falloff(4));
    let through = kill_by_hand(d_max, d_max, falloff(4), heal, limit).expect("killable");
    assert!(
        first + after < through,
        "the premise: the towers shoot the healer first"
    );
    let d = 11 * DISMANTLE_POWER;
    assert!(
        d * (first + after) < STORAGE_HITS,
        "the premise: the storage holds"
    );
    assert!(
        d * (first + through) >= STORAGE_HITS,
        "the premise: counting the heal twice would fire"
    );
    let foes = [foe(1, 14, 14, dismantler), foe(2, 15, 14, healer)];
    let mut w = World::new();
    w.tick(100, &sites, &foes);
    assert_eq!(w.towers.firing_at(1).count(), 1, "the healer is shot");
    assert_eq!(w.towers.firing_at(0).count(), 0, "the dismantler is not");
    assert!(w.safe.orders().is_empty(), "{:?}", w.reasons());
    let hits = d * (first + after);
    if let Some(s) = sites.iter_mut().find(|s| s.kind == StructureType::Storage) {
        s.hits = hits;
    }
    let mut w = World::new();
    w.tick(100, &sites, &foes);
    assert_eq!(
        w.reasons(),
        [Reason::Core {
            kind: StructureType::Storage,
            at: at(13, 14),
            hits,
            hostiles: 1,
            damage: d,
            towers: 1,
            kill: Some(first + after),
            wait: first,
        }]
    );
}

/// The audit of the fix (a low): when the focus dies in one tick, a spare tower may kill a hostile
/// in reach in the same tick (P7.2's `split`), so it does not wait. A spawn with one tick of its
/// damage and one hit more holds.
#[test]
fn scenario_a_spare_tower_kills_in_reach_this_tick() {
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 10, 14, TOWER_HITS));
    sites.push(site(903, StructureType::Tower, 12, 15, TOWER_HITS));
    // The focus: a ranged scout one shot kills, out of reach (more than 3 plus the lead) of the
    // spawn and both towers, with the lower id. The dismantler beside the spawn: one shot each.
    let scout = body(&[(Part::RangedAttack, 1), (Part::Move, 1)]);
    let dismantler = body(&[(Part::Work, 3), (Part::Move, 3)]);
    assert!(
        full(&scout) <= falloff(6),
        "the premise: one shot from either tower"
    );
    assert!(full(&dismantler) <= falloff(4), "the premise: one shot");
    let d = 3 * DISMANTLE_POWER;
    let hits = d + 1;
    sites[1].hits = hits;
    assert!(2 * d >= hits, "the premise: a wait of one tick would fire");
    let mut w = World::new();
    w.tick(
        100,
        &sites,
        &[foe(1, 16, 10, scout), foe(2, 11, 10, dismantler)],
    );
    assert_eq!(w.towers.firing_at(0).count(), 1, "the scout");
    assert_eq!(
        w.towers.firing_at(1).count(),
        1,
        "the dismantler, by the spare tower"
    );
    assert!(w.safe.orders().is_empty(), "{:?}", w.reasons());
}

/// The third audit of the fix (a medium from P7.4): a dangerous healer the towers focus in reach
/// dies first, so its partner's kill leaves out its heal. Two ranged healers side by side beside a
/// storage: K is the focus's kill through both heals plus the partner's through its own.
#[test]
fn scenario_a_dangerous_healer_focus_in_reach_counts_once() {
    let mut sites = base();
    sites.push(site(902, StructureType::Tower, 10, 14, TOWER_HITS));
    // At range 5 of the tower (full power) and 2 of the storage; 5 from the spawn and the tower,
    // out of their reach.
    sites.push(site(903, StructureType::Storage, 13, 14, STORAGE_HITS));
    let pair = body(&[(Part::Move, 5), (Part::Heal, 10), (Part::RangedAttack, 5)]);
    let own = 10 * HEAL_POWER;
    let hp = full(&pair);
    let limit = DEFENSE_MMO.max_kill_ticks;
    let shot = falloff(5);
    // The first (lower id) is the focus: both heal it at range 1 or on itself.
    let k_first = kill_by_hand(hp, hp, shot, 2 * own, limit).expect("killable");
    let k_then = kill_by_hand(hp, hp, shot, own, limit).expect("killable");
    let k_through = kill_by_hand(hp, hp, shot, 2 * own, limit).expect("killable");
    assert!(k_then < k_through, "the premise: the focus's heal matters");
    let d = 2 * 5 * RANGED_ATTACK_POWER;
    let foes = [foe(1, 15, 14, pair.clone()), foe(2, 15, 15, pair)];
    let bound = d * (k_first + k_then);
    for (hits, fires) in [(bound + 1, false), (bound, true)] {
        if let Some(s) = sites.iter_mut().find(|s| s.kind == StructureType::Storage) {
            s.hits = hits;
        }
        let mut w = World::new();
        w.tick(100, &sites, &foes);
        assert_eq!(w.towers.firing_at(0).count(), 1, "the focus");
        if fires {
            assert_eq!(
                w.reasons(),
                [Reason::Core {
                    kind: StructureType::Storage,
                    at: at(13, 14),
                    hits,
                    hostiles: 2,
                    damage: d,
                    towers: 1,
                    kill: Some(k_first + k_then),
                    wait: 0,
                }]
            );
        } else {
            assert!(w.safe.orders().is_empty(), "{:?}", w.reasons());
        }
    }
}
