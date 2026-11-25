//! Tests of claim scoring (P8.2, ADR-041, `expansion::claim`). Expected values derive from the
//! constants (`SOURCE_ENERGY_CAPACITY`, `ENERGY_REGEN_TIME`, `CARRY_CAPACITY`, `CREEP_LIFE_TIME`,
//! `BODYPART_COST`, `RAMPART_DECAY_AMOUNT`, `RAMPART_DECAY_TIME`, `REPAIR_COST`, `GCL_NOVICE`,
//! `MAX_LEVEL`) and the claim config. The plan check on real rooms is in
//! `tests/claim_corpus.rs`.

use rustedscreep_consts::{
    CARRY_CAPACITY, CREEP_LIFE_TIME, ENERGY_REGEN_TIME, GCL_NOVICE, Part, RAMPART_DECAY_AMOUNT,
    RAMPART_DECAY_TIME, REPAIR_COST, ROOM_AREA, RoomName, SOURCE_ENERGY_CAPACITY,
};

use super::*;
use crate::config::{CLAIM_MMO, ClaimConfig, REMOTE_MMO};
use crate::planner::flood::Params;

fn room(name: &str) -> RoomName {
    name.parse().expect("room")
}

fn tile(x: u8, y: u8) -> Tile {
    Tile::new(x, y).expect("tile")
}

const NOW: u32 = 1_000_000;

/// `remote.max_rooms` at the MMO's value: the remotes term counts this many neighbors.
const REMOTES: u32 = REMOTE_MMO.max_rooms;

/// A neutral room read at `NOW` with `sources` sources and its controller at (25, 25).
fn neutral(name: &str, sources: usize) -> RoomIntel {
    let mut e = RoomIntel::new(room(name), NOW);
    e.known = true;
    e.controller = Controller::Neutral;
    e.controller_tile = Some(tile(25, 25));
    for (i, slot) in e.sources.iter_mut().take(sources).enumerate() {
        let x = u8::try_from(10 + 10 * i).expect("x");
        *slot = Some(tile(x, 10));
    }
    e
}

fn foreign(name: &str, level: u8) -> RoomIntel {
    let mut e = neutral(name, 2);
    e.controller = Controller::Foreign { level };
    e
}

fn atlas(entries: Vec<RoomIntel>) -> Atlas {
    let mut a = Atlas::default();
    for e in entries {
        a.put(e);
    }
    a
}

const HOME: &str = "E5N5";

fn homes() -> Vec<ClaimHome> {
    vec![ClaimHome {
        room: room(HOME),
        status: RoomStatus::Normal,
    }]
}

fn owned() -> Vec<RoomName> {
    vec![room(HOME)]
}

/// The tests' config: no weights but the ones a test names.
fn cfg() -> ClaimConfig {
    ClaimConfig {
        remote_pct: 0,
        distance_cost: 0,
        foreign_cost: 0,
        ..CLAIM_MMO
    }
}

fn scored(a: &Atlas, cfg: &ClaimConfig) -> Claims {
    let mut c = Claims::default();
    c.score(a, 0, &homes(), &owned(), NOW, REMOTES, cfg);
    c
}

fn skipped(c: &Claims, why: Skip) -> u32 {
    c.skipped().find(|&(s, _)| s == why).map_or(0, |(_, n)| n)
}

fn get(c: &Claims, name: &str) -> Option<Candidate> {
    c.candidates()
        .iter()
        .find(|x| x.room == room(name))
        .copied()
}

/// A valid fit with these measures, for the ranking tests (the plan check itself is tested on the
/// corpus).
fn fit(source_ways: u32, controller_way: u32, ramparts: u32) -> Fit {
    Fit {
        spawn: tile(25, 30),
        ramparts,
        source_ways,
        controller_way,
        open: [true; 4],
    }
}

// ---- The energy terms ----

#[test]
fn a_source_yields_its_capacity_over_the_regeneration_time() {
    let want = i64::from(SOURCE_ENERGY_CAPACITY) * 1000 / i64::from(ENERGY_REGEN_TIME);
    assert_eq!(source_yield(), want);
}

#[test]
fn hauling_costs_two_carry_parts_and_a_move_part_per_round_trip_load() {
    // One source's yield over a way of 20: yield × 2 × 20 / CARRY_CAPACITY CARRY parts, each with
    // half a MOVE part, renewed every CREEP_LIFE_TIME.
    let flow = source_yield();
    let carry = i64::from(Part::Carry.cost());
    let mv = i64::from(Part::Move.cost());
    let parts_milli = flow * 2 * 20 / i64::from(CARRY_CAPACITY);
    let want = parts_milli * (carry + mv / 2) / i64::from(CREEP_LIFE_TIME);
    assert_eq!(haul_cost(flow, 20), want);
    assert_eq!(haul_cost(flow, 0), 0);
    assert!(haul_cost(flow, 40) > haul_cost(flow, 20));
}

#[test]
fn a_rampart_costs_its_decay_at_the_repair_cost() {
    // RAMPART_DECAY_AMOUNT hits every RAMPART_DECAY_TIME - 1 ticks (V304), REPAIR_COST energy a hit.
    let per_tick = f64::from(RAMPART_DECAY_AMOUNT) / f64::from(RAMPART_DECAY_TIME - 1)
        * f64::from(REPAIR_COST);
    let got = rampart_cost(100);
    let want = per_tick * 100.0 * 1000.0;
    assert!((got as f64 - want).abs() <= 1.0, "{got} against {want}");
    assert_eq!(rampart_cost(0), 0);
}

#[test]
fn a_foreign_room_weighs_by_level_and_nearness() {
    let c = ClaimConfig {
        foreign_cost: 8000,
        threat_range: 2,
        ..CLAIM_MMO
    };
    let top = MAX_LEVEL;
    // One room inside the range's end: the whole cost at the highest level.
    assert_eq!(foreign_penalty(top, 2, &c), 8000);
    // Next door: once more.
    assert_eq!(foreign_penalty(top, 1, &c), 16000);
    // Half the level, half the cost.
    assert_eq!(foreign_penalty(top / 2, 2, &c), 4000);
    // Beyond the range, nothing.
    assert_eq!(foreign_penalty(top, 3, &c), 0);
}

// ---- Which rooms are candidates ----

#[test]
fn a_neutral_room_in_reach_is_a_candidate_at_its_sources_yield() {
    let a = atlas(vec![neutral("E6N5", 2)]);
    let c = scored(&a, &cfg());
    let x = get(&c, "E6N5").expect("candidate");
    assert_eq!(x.home, room(HOME));
    assert_eq!(x.distance, 1);
    assert_eq!(x.terms.sources, 2 * source_yield());
    assert_eq!(x.value(), 2 * source_yield());
    assert!(x.plan.is_none());
}

#[test]
fn rooms_a_claimer_would_fail_on_are_left_out() {
    let mut owned_room = neutral("E6N5", 2);
    owned_room.controller = Controller::Foreign { level: 1 };
    let mut reserved = neutral("E4N5", 2);
    reserved.reserved = Reserved::Other { until: NOW + 1 };
    let mut cored = neutral("E5N6", 2);
    cored.core = Some(0);
    let mut towered = neutral("E5N4", 2);
    towered.towers = 1;
    let mut unknown = neutral("E6N6", 2);
    unknown.known = false;
    let mut unreachable = neutral("E4N4", 2);
    unreachable.unreachable = true;
    let mut no_controller = neutral("E6N4", 2);
    no_controller.controller = Controller::Absent;
    let none = neutral("E4N6", 0);
    let a = atlas(vec![
        owned_room,
        reserved,
        cored,
        towered,
        unknown,
        unreachable,
        no_controller,
        none,
    ]);
    let c = scored(&a, &cfg());
    assert!(c.candidates().is_empty(), "{:?}", c.candidates());
    assert_eq!(skipped(&c, Skip::Owned), 1);
    assert_eq!(skipped(&c, Skip::Reserved), 1);
    assert_eq!(skipped(&c, Skip::Hostile), 2);
    assert_eq!(skipped(&c, Skip::Unknown), 2);
    assert_eq!(skipped(&c, Skip::NoController), 1);
    assert_eq!(skipped(&c, Skip::Sources), 1);
}

#[test]
fn a_reservation_that_ended_or_is_the_bots_own_does_not_block() {
    let mut ended = neutral("E6N5", 2);
    ended.reserved = Reserved::Other { until: NOW };
    let mut mine = neutral("E4N5", 2);
    mine.reserved = Reserved::Mine { until: NOW + 4000 };
    let c = scored(&atlas(vec![ended, mine]), &cfg());
    assert!(get(&c, "E6N5").is_some());
    assert!(get(&c, "E4N5").is_some());
}

#[test]
fn reach_and_sources_follow_the_config() {
    let near = neutral("E6N5", 1);
    let far = neutral("E9N5", 2);
    let c = ClaimConfig {
        max_distance: 3,
        min_sources: 2,
        ..cfg()
    };
    let got = scored(&atlas(vec![near, far]), &c);
    assert!(got.candidates().is_empty());
    assert_eq!(skipped(&got, Skip::Sources), 1);
    assert_eq!(skipped(&got, Skip::Far), 1);
}

#[test]
fn a_room_needs_a_home_of_its_status_and_no_closed_or_full_novice_area() {
    let mut respawn = neutral("E6N5", 2);
    respawn.status = RoomStatus::Respawn;
    let mut closed = neutral("E4N5", 2);
    closed.status = RoomStatus::Closed;
    let c = scored(&atlas(vec![respawn, closed]), &cfg());
    assert!(c.candidates().is_empty());
    assert_eq!(skipped(&c, Skip::Status), 2);

    // A novice home claims in its novice area until the bot owns GCL_NOVICE rooms (V374, V381).
    let mut novice = neutral("E6N5", 2);
    novice.status = RoomStatus::Novice;
    let a = atlas(vec![novice]);
    let novice_home = [ClaimHome {
        room: room(HOME),
        status: RoomStatus::Novice,
    }];
    let few: Vec<RoomName> = (0..GCL_NOVICE - 1)
        .map(|i| room(&format!("E{}N1", i + 1)))
        .collect();
    let mut ok = Claims::default();
    ok.score(&a, 0, &novice_home, &few, NOW, REMOTES, &cfg());
    assert!(get(&ok, "E6N5").is_some());
    let full: Vec<RoomName> = (0..GCL_NOVICE)
        .map(|i| room(&format!("E{}N1", i + 1)))
        .collect();
    let mut no = Claims::default();
    no.score(&a, 0, &novice_home, &full, NOW, REMOTES, &cfg());
    assert!(no.candidates().is_empty());
    assert_eq!(skipped(&no, Skip::Status), 1);
}

#[test]
fn the_nearest_home_of_the_rooms_status_is_its_home() {
    let a = atlas(vec![neutral("E8N5", 2)]);
    let two = [
        ClaimHome {
            room: room(HOME),
            status: RoomStatus::Normal,
        },
        ClaimHome {
            room: room("E9N5"),
            status: RoomStatus::Respawn,
        },
        ClaimHome {
            room: room("E7N5"),
            status: RoomStatus::Normal,
        },
    ];
    let mut c = Claims::default();
    c.score(&a, 0, &two, &owned(), NOW, REMOTES, &cfg());
    let x = get(&c, "E8N5").expect("candidate");
    assert_eq!(x.home, room("E7N5"));
    assert_eq!(x.distance, 1);
}

// ---- The score's terms ----

#[test]
fn distance_costs_its_weight_a_room() {
    let c = ClaimConfig {
        distance_cost: 1500,
        ..cfg()
    };
    let got = scored(&atlas(vec![neutral("E7N5", 2)]), &c);
    let x = get(&got, "E7N5").expect("candidate");
    assert_eq!(x.terms.distance, -1500 * 2);
}

#[test]
fn free_neighbors_add_their_sources_at_the_remote_share() {
    // E7N5's neighbors: E8N5 free (2 sources), E7N4 reserved by another user, E7N6 owned by
    // another player; E6N5 is across an exit from the home, so the home would mine it.
    let mut reserved = neutral("E7N4", 2);
    reserved.reserved = Reserved::Other { until: NOW + 100 };
    let a = atlas(vec![
        neutral("E7N5", 2),
        neutral("E8N5", 2),
        reserved,
        foreign("E7N6", 3),
        neutral("E6N5", 1),
    ]);
    let c = ClaimConfig {
        remote_pct: 30,
        max_distance: 2,
        ..cfg()
    };
    let got = scored(&a, &c);
    let x = get(&got, "E7N5").expect("candidate");
    assert_eq!(x.terms.remotes, 2 * source_yield() * 30 / 100);
}

#[test]
fn only_the_best_neighbors_a_home_can_mine_count() {
    // Four free neighbors with 2, 1, 2 and 1 sources: with `remote.max_rooms` 2, the two
    // two-source ones count; with 1, one of them.
    let a = atlas(vec![
        neutral("E5N8", 2),
        neutral("E5N7", 2),
        neutral("E6N8", 1),
        neutral("E5N9", 2),
        neutral("E4N8", 1),
    ]);
    let c = ClaimConfig {
        remote_pct: 30,
        max_distance: 4,
        ..cfg()
    };
    let share = |n: i64| n * source_yield() * 30 / 100;
    for (rooms, want) in [(2, share(4)), (1, share(2)), (4, share(6))] {
        let mut got = Claims::default();
        got.score(&a, 0, &homes(), &owned(), NOW, rooms, &c);
        let x = get(&got, "E5N8").expect("candidate");
        assert_eq!(x.terms.remotes, want, "{rooms} rooms");
    }
}

#[test]
fn one_source_with_rich_neighbors_can_beat_two_sources_without() {
    // The user's "Score decides": a one-source room ranks by its value like any other.
    let a = atlas(vec![
        neutral("E5N7", 1),
        neutral("E4N7", 2),
        neutral("E6N7", 2),
        neutral("E5N8", 2),
        neutral("E9N9", 2),
    ]);
    let c = ClaimConfig {
        remote_pct: 30,
        max_distance: 4,
        ..cfg()
    };
    let got = scored(&a, &c);
    let one = get(&got, "E5N7").expect("one source");
    let two = get(&got, "E9N9").expect("two sources");
    assert_eq!(one.sources, 1);
    assert!(
        one.value() > two.value(),
        "{} against {}",
        one.value(),
        two.value()
    );
}

#[test]
fn another_players_rooms_nearby_lower_the_value() {
    let a = atlas(vec![
        neutral("E7N5", 2),
        foreign("E8N5", MAX_LEVEL),
        foreign("E9N5", 4),
    ]);
    let c = ClaimConfig {
        foreign_cost: 4000,
        threat_range: 2,
        ..cfg()
    };
    let got = scored(&a, &c);
    let x = get(&got, "E7N5").expect("candidate");
    let want = foreign_penalty(MAX_LEVEL, 1, &c) + foreign_penalty(4, 2, &c);
    assert_eq!(x.terms.foreign, -want);
}

// ---- The plan check and the ranking ----

#[test]
fn the_first_unplanned_room_is_planned_until_the_top_are_planned() {
    let a = atlas(vec![
        neutral("E6N5", 2),
        neutral("E4N5", 2),
        neutral("E5N6", 1),
        neutral("E5N4", 1),
    ]);
    let c = ClaimConfig {
        plan_count: 2,
        ..cfg()
    };
    let mut got = scored(&a, &c);
    // Ties go by room (`RoomName`'s order): E4N5 before E6N5 (two sources), then the first of
    // the one-source rooms.
    assert!(room("E4N5") < room("E6N5"));
    let one = room("E5N4").min(room("E5N6"));
    let one_name = one.to_string();
    assert_eq!(got.wanted(&c), Some(room("E4N5")));
    let key = |n: &str| Key::of(a.get(room(n)).expect("entry")).expect("key");
    got.finish(room("E4N5"), key("E4N5"), Ok(fit(10, 10, 10)));
    if let Some(x) = got.candidates.iter_mut().find(|x| x.room == room("E4N5")) {
        apply(x, fit(10, 10, 10), &a, &owned(), NOW, REMOTES, &c);
    }
    got.rank();
    // E4N5 now ranks below E6N5's upper bound: E6N5 is next.
    assert_eq!(got.candidates()[0].room, room("E6N5"));
    assert!(got.best().is_none());
    assert_eq!(got.wanted(&c), Some(room("E6N5")));
    got.finish(room("E6N5"), key("E6N5"), Err("no room"));
    got.rank();
    // Unfit, it leaves the ranking; E4N5 is the best and the first planned.
    assert!(get(&got, "E6N5").is_none());
    assert_eq!(skipped(&got, Skip::Unfit), 1);
    assert_eq!(got.best().map(|x| x.room), Some(room("E4N5")));
    // One planned above; plan_count 2 wants the next.
    assert_eq!(got.wanted(&c), Some(one));
    got.finish(one, key(&one_name), Ok(fit(10, 10, 10)));
    if let Some(x) = got.candidates.iter_mut().find(|x| x.room == one) {
        apply(x, fit(10, 10, 10), &a, &owned(), NOW, REMOTES, &c);
    }
    got.rank();
    // Its plan's costs drop it below the other one-source room's upper bound, so that one is
    // planned too before the top two are known.
    let other = room("E5N4").max(room("E5N6"));
    assert_eq!(got.wanted(&c), Some(other));
    got.finish(other, key(&other.to_string()), Ok(fit(10, 10, 10)));
    if let Some(x) = got.candidates.iter_mut().find(|x| x.room == other) {
        apply(x, fit(10, 10, 10), &a, &owned(), NOW, REMOTES, &c);
    }
    got.rank();
    assert_eq!(got.wanted(&c), None);
    // A new score keeps the outcomes: nothing is planned again, and the unfit room stays out.
    got.score(&a, 0, &homes(), &owned(), NOW + c.score_ticks, REMOTES, &c);
    assert_eq!(got.wanted(&c), None);
    assert!(get(&got, "E6N5").is_none());
    assert_eq!(got.best().map(|x| x.room), Some(room("E4N5")));
}

#[test]
fn a_plan_lowers_the_value_by_its_walks_and_ramparts() {
    let a = atlas(vec![neutral("E6N5", 2)]);
    let c = cfg();
    let mut got = scored(&a, &c);
    let before = get(&got, "E6N5").expect("candidate").value();
    let f = fit(30, 12, 40);
    if let Some(x) = got.candidates.iter_mut().find(|x| x.room == room("E6N5")) {
        apply(x, f, &a, &owned(), NOW, REMOTES, &c);
    }
    let x = get(&got, "E6N5").expect("candidate");
    let want_walks = haul_cost(source_yield(), 30) + haul_cost(2 * source_yield(), 12);
    assert_eq!(x.terms.walks, -want_walks);
    assert_eq!(x.terms.ramparts, -rampart_cost(40));
    assert_eq!(x.value(), before - want_walks - rampart_cost(40));
    assert_eq!(x.plan, Some(f));
}

#[test]
fn a_closed_side_drops_its_neighbors_remotes() {
    let a = atlas(vec![neutral("E7N5", 2), neutral("E8N5", 2)]);
    let c = ClaimConfig {
        remote_pct: 30,
        max_distance: 2,
        ..cfg()
    };
    let mut got = scored(&a, &c);
    assert!(get(&got, "E7N5").expect("candidate").terms.remotes > 0);
    let mut f = fit(0, 0, 0);
    f.open = [true, false, true, true];
    if let Some(x) = got.candidates.iter_mut().find(|x| x.room == room("E7N5")) {
        apply(x, f, &a, &owned(), NOW, REMOTES, &c);
    }
    assert_eq!(get(&got, "E7N5").expect("candidate").terms.remotes, 0);
}

#[test]
fn changed_facts_plan_a_room_again() {
    let mut e = neutral("E6N5", 2);
    let a = atlas(vec![e]);
    let c = cfg();
    let mut got = scored(&a, &c);
    let key = Key::of(&e).expect("key");
    got.finish(room("E6N5"), key, Err("no room"));
    got.score(&a, 0, &homes(), &owned(), NOW + c.score_ticks, REMOTES, &c);
    assert!(get(&got, "E6N5").is_none());
    // The controller read at another tile: the old outcome no longer holds.
    e.controller_tile = Some(tile(20, 20));
    let b = atlas(vec![e]);
    got.score(
        &b,
        0,
        &homes(),
        &owned(),
        NOW + 2 * c.score_ticks,
        REMOTES,
        &c,
    );
    assert!(get(&got, "E6N5").is_some());
    assert_eq!(got.wanted(&c), Some(room("E6N5")));
}

#[test]
fn a_room_with_no_space_for_a_base_is_unfit() {
    // Walls but for a 5 x 5 pocket around the controller and the source.
    let mut raw = vec![1u8; ROOM_AREA];
    for y in 20..25u8 {
        for x in 20..25u8 {
            if let Some(b) = raw.get_mut(usize::from(y) * 50 + usize::from(x)) {
                *b = 0;
            }
        }
    }
    let terrain = TerrainGrid::from_row_major(&raw).expect("terrain");
    let mut e = neutral("E6N5", 0);
    e.controller_tile = Some(tile(21, 21));
    e.sources[0] = Some(tile(23, 23));
    let a = atlas(vec![e]);
    let c = cfg();
    let mut got = scored(&a, &c);
    assert_eq!(got.wanted(&c), Some(room("E6N5")));
    got.feed_terrain(room("E6N5"), Some(terrain), &a);
    let mut steps = 0;
    while got.step(Params::default(), &a, &owned(), &c) == Step::More {
        steps += 1;
        assert!(steps < 100, "the plan never ends");
    }
    assert!(get(&got, "E6N5").is_none());
    assert_eq!(skipped(&got, Skip::Unfit), 1);
    assert!(
        got.events()
            .iter()
            .any(|ev| matches!(ev, ClaimEvent::Unfit { room: r, .. } if *r == room("E6N5")))
    );
    assert_eq!(got.wanted(&c), None);
}

#[test]
fn a_room_whose_terrain_cannot_be_read_waits_for_the_next_timed_score() {
    let a = atlas(vec![neutral("E6N5", 2)]);
    let c = cfg();
    let mut got = scored(&a, &c);
    got.feed_terrain(room("E6N5"), None, &a);
    assert!(get(&got, "E6N5").is_none());
    assert!(
        got.events()
            .iter()
            .any(|ev| *ev == ClaimEvent::NoTerrain { room: room("E6N5") })
    );
    assert_eq!(got.wanted(&c), None);
    // A score for a changed atlas keeps it out; a timed one lets it try again.
    got.score(&a, 1, &homes(), &owned(), NOW + 1, REMOTES, &c);
    assert!(get(&got, "E6N5").is_none());
    got.score(
        &a,
        1,
        &homes(),
        &owned(),
        NOW + 1 + c.score_ticks,
        REMOTES,
        &c,
    );
    assert_eq!(got.wanted(&c), Some(room("E6N5")));
}

#[test]
fn a_room_the_atlas_no_longer_holds_leaves_the_ranking_when_its_plan_would_start() {
    let a = atlas(vec![neutral("E6N5", 2)]);
    let c = cfg();
    let mut got = scored(&a, &c);
    assert_eq!(got.wanted(&c), Some(room("E6N5")));
    let empty = Atlas::default();
    let plain = TerrainGrid::from_row_major(&vec![0u8; ROOM_AREA]).expect("terrain");
    got.feed_terrain(room("E6N5"), Some(plain), &empty);
    assert!(got.planning().is_none());
    assert_eq!(got.wanted(&c), None);
}

#[test]
fn new_planner_tunables_plan_every_room_again() {
    let a = atlas(vec![neutral("E6N5", 2)]);
    let c = cfg();
    let mut got = scored(&a, &c);
    got.set_params(Params::default());
    let key = Key::of(a.get(room("E6N5")).expect("entry")).expect("key");
    got.finish(room("E6N5"), key, Ok(fit(10, 10, 10)));
    got.score(&a, 0, &homes(), &owned(), NOW + 1, REMOTES, &c);
    assert!(get(&got, "E6N5").is_some_and(|x| x.plan.is_some()));
    let other = Params {
        hub_clearance: Params::default().hub_clearance.saturating_add(1),
        ..Params::default()
    };
    got.set_params(other);
    assert!(got.score_due(NOW + 2, 0, &owned(), REMOTES, &c));
    got.score(&a, 0, &homes(), &owned(), NOW + 2, REMOTES, &c);
    assert!(get(&got, "E6N5").is_some_and(|x| x.plan.is_none()));
    assert_eq!(got.wanted(&c), Some(room("E6N5")));
}

#[test]
fn nothing_is_done_while_off() {
    let off = ClaimConfig {
        enabled: false,
        ..cfg()
    };
    let got = Claims::default();
    assert!(!got.has_work(NOW, 0, &owned(), REMOTES, &off));
    assert!(!got.score_due(NOW, 0, &owned(), REMOTES, &off));
    assert!(got.has_work(NOW, 0, &owned(), REMOTES, &cfg()));
}

#[test]
fn a_score_is_due_when_the_atlas_or_the_owned_rooms_change_or_its_time_passes() {
    let a = atlas(vec![neutral("E6N5", 2)]);
    let c = cfg();
    let got = scored(&a, &c);
    let rev = 0;
    assert!(!got.score_due(NOW + c.score_ticks - 1, rev, &owned(), REMOTES, &c));
    assert!(got.score_due(NOW + c.score_ticks, rev, &owned(), REMOTES, &c));
    assert!(got.score_due(NOW + 1, rev + 1, &owned(), REMOTES, &c));
    let more = vec![room(HOME), room("E9N9")];
    assert!(got.score_due(NOW + 1, rev, &more, REMOTES, &c));
    // A changed config or `remote.max_rooms` too.
    assert!(got.score_due(NOW + 1, rev, &owned(), REMOTES + 1, &c));
    let other = ClaimConfig {
        remote_pct: c.remote_pct + 1,
        ..c
    };
    assert!(got.score_due(NOW + 1, rev, &owned(), REMOTES, &other));
}

#[test]
fn a_room_claimed_since_its_read_is_left_out() {
    // The atlas still holds it as neutral: it is never read once owned.
    let a = atlas(vec![neutral("E6N5", 2)]);
    let both = vec![room(HOME), room("E6N5")];
    let mut got = Claims::default();
    got.score(&a, 0, &homes(), &both, NOW, REMOTES, &cfg());
    assert!(got.candidates().is_empty());
    assert_eq!(skipped(&got, Skip::Owned), 1);
}

#[test]
fn every_side_of_a_plain_room_is_open_and_a_walled_one_none() {
    let plain = TerrainGrid::from_row_major(&vec![0u8; ROOM_AREA]).expect("terrain");
    assert_eq!(open_sides(&plain), [true; 4]);
    let walls = TerrainGrid::from_row_major(&vec![1u8; ROOM_AREA]).expect("terrain");
    assert_eq!(open_sides(&walls), [false; 4]);
}
