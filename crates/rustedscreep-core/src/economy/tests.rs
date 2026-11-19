//! Tests for the opening (P2.4): sizing and watchdog units, and native scenarios (TEST_PLAN §7)
//! in which a small world model drives the snapshot, movement, the spawner, the opening and the
//! intent buffer over thousands of ticks. Expected values derive from constants.
//!
//! The world model follows the verified engine rules: harvest takes `min(HARVEST_POWER × WORK,
//! energy)` and drops what does not fit on the creep's tile, and a harvest that finds the source
//! emptied earlier in the tick does nothing (V182, V183); transfer moves `min(stock, free space)`
//! (V191); upgrading spends `min(UPGRADE_CONTROLLER_POWER × WORK, energy)` and restores the
//! downgrade timer by `CONTROLLER_DOWNGRADE_RESTORE + 1`, capped (V188, V189); a level needs
//! `CONTROLLER_LEVELS[level]` progress (V188); building spends `min(BUILD_POWER × WORK, energy,
//! left)` and is refused while a creep stands on an obstacle site (V185, V186); a source refills to
//! its capacity `ENERGY_REGEN_TIME` ticks after it first drops below it (V184); a spawn gains 1
//! energy a tick while it and the room hold less than `SPAWN_ENERGY_CAPACITY` (V174); spawning
//! drains spawns first, then extensions (V173) and takes `CREEP_SPAWN_TIME` per part (V170); a creep
//! dies `CREEP_LIFE_TIME` ticks after it is born and leaves a tombstone with its store (V194, the
//! body reclaim left out: about 0 at old age); piles lose `ceil(amount / ENERGY_DECAY)` a tick and
//! merge on a tile (V193). Moves follow V51, V52 and the fatigue rules (V54, V55) on plain and swamp
//! terrain (no roads).
//! The game side accepts the bot's plan and the processor then runs each creep's intents in its own
//! order (`creepActions`, V45), creep by creep, so a transfer lands before a harvest.
//! Containers (P3.1, ADR-023): energy made on a tile (a harvest's overflow, a decayed tombstone)
//! goes into a container there first, up to `CONTAINER_CAPACITY`, and the rest onto a pile (V193);
//! `withdraw` takes from one (V192); a finished container site becomes one; a container refuses
//! another site on its tile (V205). A new container has `CONTAINER_HITS` and loses `CONTAINER_DECAY`
//! hits `CONTAINER_DECAY_TIME` − 1 ticks after it is built, then every `CONTAINER_DECAY_TIME_OWNED`
//! − 1 (an owned room); at 0 hits its energy goes onto a pile and it is gone (V214). A repair (P3.5)
//! is refused out of range 3 or with no energy (V221); at full hits it is billed and does nothing;
//! otherwise it adds `min(REPAIR_POWER × WORK, energy / REPAIR_COST, hits missing)` for
//! `ceil(hits × REPAIR_COST)` energy (V222), in the processor's order after harvest (V45).
//! Links (P3.2, ADR-024): a finished link site becomes a link holding up to `LINK_CAPACITY`; a
//! creep's transfer into one moves `min(stock, free space)` and `withdraw` takes `min(free space,
//! energy)`, refused when it is empty (V220); a send is checked against the tick's start (cooldown,
//! energy, the receiver's room; V216), then clamped to the receiver's room, which gets the amount
//! less [`link_loss`], and the sender's cooldown becomes `LINK_COOLDOWN` per tile of range, counted
//! down at the end of every tick (V217). Sends run after the creep intents, so a withdraw sees the
//! link's energy at the tick's start, as the game side computes it.

use super::placement::{
    CONTROLLER_CLEAR, EDGE_MARGIN, Placement, PlanView, SPAWN_CLEAR, extensions_in, next_extension,
};
use super::sizing::{
    Round, Trip, WorkerScratch, WorkerSource, WorkerTargets, build_round, carry_round,
    container_upkeep, haul_round, hauler_plan, haulers_for, miner_body, miner_work,
    smallest_worker, source_wants, upgraded, upgrader_carry, upgrader_plan, upgrader_upgrade,
    walk_ticks, worker_plan, worker_round, worker_value, workers_for_source, yield_per_life,
};
use super::*;
use crate::config::{
    CACHE_MMO, HAULING_MMO, HaulingConfig, LINK_MMO, LinkConfig, MINING_MMO, MMO, MOVEMENT_MMO,
    MiningConfig, OPENING_MMO, OpeningConfig, PLANNER_MMO, REPAIR_MMO, RepairConfig, SPAWN_MMO,
    UPGRADING_MMO, UpgradingConfig,
};
use crate::intents::{ActLog, CreepIntent, IntentBuffer, Outcome, Target, TargetRow};
use crate::movement::{
    CostGrid, Dial, FlowField, Footprint, Movement, ShoveCaps, TerrainGrid, Tile,
};
use crate::scheduler::Commit;
use crate::snapshot::{
    Controller, ControllerState, CreepRow, CreepStore, ObjectKey, ObjectKind, ObjectRow,
    ObjectStatic, RoomEnergy, Snapshot, StructureStatic, TickContext,
};
use crate::spawning::{Body, EnergyBudget, Identity, Role, Spawner};
use rustedscreep_consts::{
    BUILD_POWER, CARRY_CAPACITY, CONTAINER_CAPACITY, CONTAINER_DECAY, CONTAINER_DECAY_TIME,
    CONTAINER_DECAY_TIME_OWNED, CONTAINER_HITS, CONTROLLER_DOWNGRADE_RESTORE,
    CONTROLLER_MAX_UPGRADE_PER_TICK, CREEP_LIFE_TIME, CREEP_SPAWN_TIME, ENERGY_DECAY,
    ENERGY_REGEN_TIME, ErrorCode, HARVEST_POWER, LINK_CAPACITY, LINK_COOLDOWN, MAX_CREEP_SIZE,
    MOVE_COST_PLAIN, MOVE_COST_ROAD, MOVE_COST_SWAMP, MOVE_POWER, Part, Position, REPAIR_COST,
    REPAIR_POWER, RoomCoordinate, RoomName, SOURCE_ENERGY_CAPACITY, SOURCE_ENERGY_KEEPER_CAPACITY,
    SOURCE_ENERGY_NEUTRAL_CAPACITY, SPAWN_ENERGY_CAPACITY, SPAWN_ENERGY_START, StructureType,
    TOMBSTONE_DECAY_PER_PART, Terrain, UPGRADE_CONTROLLER_POWER, controller_downgrade,
    controller_levels, extension_energy_capacity,
};

// ---------------------------------------------------------------------------------------------
// Sizing

/// `n` repeats of the smallest worker's parts, each repeated in place (a fixture for the round
/// formulas; no job spawns it since P3.6).
fn repeats(n: u32) -> Body {
    let unit = smallest_worker().expect("smallest worker");
    Body::scaled(unit.parts(), unit.cost() * n, u8::MAX).expect("body")
}

/// Uses: MOVE_POWER, MOVE_COST_PLAIN, MOVE_COST_SWAMP. The walk takes at least the tiles and at
/// least the fatigue over what the MOVE parts remove a tick.
#[test]
fn walk_ticks_take_the_tiles_or_the_fatigue() {
    let plain = Trip {
        tiles: 10,
        cost: 10 * MOVE_COST_PLAIN,
    };
    // Weight 1 per MOVE part: the fatigue of a plain tile is gone in one tick.
    assert_eq!(walk_ticks(plain, 1, 1), Some(10));
    // Weight 2 per MOVE part: two ticks a tile on plain.
    assert_eq!(
        walk_ticks(plain, 2, 1),
        Some((10 * MOVE_COST_PLAIN * 2).div_ceil(MOVE_POWER))
    );
    let swamp = Trip {
        tiles: 4,
        cost: 4 * MOVE_COST_SWAMP,
    };
    assert_eq!(
        walk_ticks(swamp, 1, 1),
        Some((4 * MOVE_COST_SWAMP).div_ceil(MOVE_POWER))
    );
    assert_eq!(walk_ticks(plain, 3, 0), None, "no MOVE part, no walk");
}

/// Uses: CARRY_CAPACITY, HARVEST_POWER, UPGRADE_CONTROLLER_POWER, MOVE_POWER, MOVE_COST_PLAIN.
#[test]
fn a_worker_round_from_constants() {
    let trip = Trip {
        tiles: 12,
        cost: 12 * MOVE_COST_PLAIN,
    };
    for n in 1..=4 {
        let body = repeats(n);
        let round = worker_round(&body, trip).expect("round");
        let load = n * CARRY_CAPACITY;
        assert_eq!(round.harvest, load.div_ceil(HARVEST_POWER * n));
        assert_eq!(round.upgrade, load.div_ceil(UPGRADE_CONTROLLER_POWER * n));
        // Loaded: WORK and CARRY weigh, per MOVE part; empty: only WORK.
        let loaded = trip.tiles.max((trip.cost * 2 * n).div_ceil(MOVE_POWER * n));
        let empty = trip.tiles.max((trip.cost * n).div_ceil(MOVE_POWER * n));
        assert_eq!(round.walk, loaded + empty, "{n} repeats");
    }
    let no_carry = Body::new(&[Part::Work, Part::Move]).expect("body");
    assert_eq!(worker_round(&no_carry, trip), None);
}

/// Uses: SOURCE_ENERGY_CAPACITY, ENERGY_REGEN_TIME, CARRY_CAPACITY. A pinned example: one repeat,
/// a round of `ticks`, so the source yields `capacity × ticks / ENERGY_REGEN_TIME` per round, each
/// worker carries one load, and the open tiles bound the workers harvesting at once.
#[test]
fn workers_for_a_source_drain_it_within_its_tiles() {
    let body = repeats(1);
    let round = Round {
        harvest: 25,
        walk: 30,
        upgrade: 50,
    };
    let ticks = round.ticks();
    let drain = (SOURCE_ENERGY_CAPACITY * ticks).div_ceil(ENERGY_REGEN_TIME * CARRY_CAPACITY);
    let slots = |open: u32| open * ticks / round.harvest;
    for open in 0..=8 {
        let want = workers_for_source(SOURCE_ENERGY_CAPACITY, open, &body, round);
        if open == 0 {
            assert_eq!(want, 0);
        } else {
            assert_eq!(want, drain.min(slots(open)).max(1), "{open} open tiles");
        }
    }
    assert_eq!(workers_for_source(0, 3, &body, round), 0);
}

/// Monotonic: more capacity, more open tiles or a longer trip never asks for fewer workers; a
/// larger body (more repeats of the same pattern) never asks for more.
#[test]
fn workers_for_a_source_are_monotonic() {
    let caps = [1500, SOURCE_ENERGY_CAPACITY, 4000];
    for tiles in [1u32, 5, 15, 30] {
        for open in 1..=8 {
            let mut last_by_reps = u32::MAX;
            for n in 1..=8 {
                let body = repeats(n);
                let trip = Trip {
                    tiles,
                    cost: tiles * MOVE_COST_PLAIN,
                };
                let round = worker_round(&body, trip).expect("round");
                let want = workers_for_source(SOURCE_ENERGY_CAPACITY, open, &body, round);
                assert!(want <= last_by_reps, "{n} repeats");
                last_by_reps = want;
                let mut last = 0;
                for &c in &caps {
                    let w = workers_for_source(c, open, &body, round);
                    assert!(w >= last);
                    last = w;
                }
                let more_open = workers_for_source(SOURCE_ENERGY_CAPACITY, open + 1, &body, round);
                assert!(more_open >= want);
                let longer = Trip {
                    tiles: tiles + 5,
                    cost: (tiles + 5) * MOVE_COST_PLAIN,
                };
                let r2 = worker_round(&body, longer).expect("round");
                assert!(workers_for_source(SOURCE_ENERGY_CAPACITY, open, &body, r2) >= want);
            }
        }
    }
}

/// The spawns' and extensions' capacity at `rcl` (`SPAWN_ENERGY_CAPACITY` and the level's full set
/// of extensions). Uses: SPAWN_ENERGY_CAPACITY, CONTROLLER_STRUCTURES, EXTENSION_ENERGY_CAPACITY.
fn capacity_at(rcl: u32) -> u32 {
    SPAWN_ENERGY_CAPACITY
        + StructureType::Extension.controller_structures(rcl) * extension_energy_capacity(rcl)
}

/// The body of `work` WORK, `carry` CARRY and `moves` MOVE parts, in that order.
fn wcm(work: u32, carry: u32, moves: u32) -> Body {
    let mut parts = Vec::new();
    parts.extend(std::iter::repeat_n(Part::Work, work as usize));
    parts.extend(std::iter::repeat_n(Part::Carry, carry as usize));
    parts.extend(std::iter::repeat_n(Part::Move, moves as usize));
    Body::new(&parts).expect("body")
}

/// A trip of `tiles` tiles at `rate` each (a fatigue rate, `MOVE_COST_*`).
fn trip_of(tiles: u32, rate: u32) -> Trip {
    Trip {
        tiles,
        cost: tiles * rate,
    }
}

/// Every worker body of WORK, CARRY and MOVE parts (one each at least, at most `t.max_work` WORK
/// and `MAX_CREEP_SIZE` parts) that costs at most `energy`, with its value and workers
/// (`worker_value`), counted again through the `Body` functions.
fn worker_options(t: &WorkerTargets<'_>, energy: u32) -> Vec<(Body, i64, u32)> {
    let mut out = Vec::new();
    let mut wants = Vec::new();
    for work in 1..=t.max_work.min(MAX_CREEP_SIZE) {
        for carry in 1..MAX_CREEP_SIZE {
            for moves in 1..MAX_CREEP_SIZE {
                if work + carry + moves > MAX_CREEP_SIZE {
                    continue;
                }
                let cost = work * Part::Work.cost()
                    + carry * Part::Carry.cost()
                    + moves * Part::Move.cost();
                if cost > energy {
                    continue;
                }
                let body = wcm(work, carry, moves);
                source_wants(t, &body, &mut wants);
                let (value, n) = worker_value(t, &body, &wants);
                out.push((body, value, n));
            }
        }
    }
    out
}

/// The solver's order among the bodies within the slack of the best value: the fewest workers,
/// then the most value, then the least energy for all of them, then the cheapest body, then the
/// fewest MOVE parts.
fn worker_key(body: &Body, value: i64, n: u32) -> (u32, i64, u64, u32, u32) {
    (
        n,
        -value,
        u64::from(n) * u64::from(body.cost()),
        body.cost(),
        count(body, Part::Move),
    )
}

/// The slack of `t`: `slack_pct` of the yield over a creep life of the sources it sizes workers for
/// (`yield_per_life`) and, with upgraders, the build work. Uses: CREEP_LIFE_TIME,
/// ENERGY_REGEN_TIME.
fn slack_of(t: &WorkerTargets<'_>) -> i64 {
    let stake: u64 = t
        .sources
        .iter()
        .filter(|s| s.trip.is_some() && !(t.upgraders && s.spent))
        .map(|s| yield_per_life(s.capacity))
        .sum::<u64>()
        + if t.upgraders { u64::from(t.build) } else { 0 };
    (stake * u64::from(t.slack_pct) / 100) as i64
}

/// Whether a body of `t` may be picked: without upgraders, while a linked source wants workers,
/// it walks empty at a tile a tick on plain (each WORK part's `MOVE_COST_PLAIN` within what its MOVE
/// parts remove, `MOVE_POWER` each). Uses: MOVE_COST_PLAIN, MOVE_POWER.
fn walks_its_duties(t: &WorkerTargets<'_>, body: &Body) -> bool {
    t.upgraders
        || !t.duties()
        || count(body, Part::Work) * MOVE_COST_PLAIN <= MOVE_POWER * count(body, Part::Move)
}

/// Two sources of `SOURCE_ENERGY_CAPACITY` with three open tiles each, `trip` from the controller,
/// unmined, for the scenario room's sizes.
fn two_sources(trip: Trip, mined: bool, linked: bool) -> [WorkerSource; 2] {
    let s = WorkerSource {
        capacity: SOURCE_ENERGY_CAPACITY,
        open: 3,
        trip: Some(trip),
        mined,
        linked,
        container: mined,
        spent: linked,
    };
    [s, s]
}

fn targets(
    sources: &[WorkerSource],
    upgraders: bool,
    build: u32,
    build_trip: Trip,
) -> WorkerTargets<'_> {
    WorkerTargets {
        sources,
        upgraders,
        min: OPENING_MMO.min_workers,
        max: OPENING_MMO.max_workers,
        max_work: if upgraders {
            UPGRADING_MMO.worker_repeats
        } else {
            u32::MAX
        },
        build,
        build_trip: Some(build_trip),
        fill: None,
        unload: extension_energy_capacity(3),
        slack_pct: OPENING_MMO.body_slack_pct,
    }
}

/// Uses: BODYPART_COST, MAX_CREEP_SIZE, MOVE_POWER, the level capacities. Property test over trips
/// (plain, swamp, road, short and long), mined, linked and plain sources, rooms with and without
/// upgraders, build work and energies: the solved body costs at most the energy, has one WORK,
/// CARRY and MOVE part at least, at most `max_work` WORK parts and `MAX_CREEP_SIZE` parts, in WORK,
/// CARRY, MOVE order; by brute force over every body (those that walk their duties' trips empty at
/// a tile a tick where that applies) it is the first in the solver's order among the bodies within
/// `body_slack_pct` of the best value; and with more energy its value never falls below an earlier
/// energy's floor (the best less the slack). RCL5 to RCL8's capacities on one trip reach the
/// 50-part limit.
#[test]
fn worker_plans_fit_and_put_the_most_to_use() {
    let mut energies: Vec<u32> = (1..=4).map(capacity_at).collect();
    energies.extend([199, 200, 250, 450, 1000]);
    energies.sort_unstable();
    let mut wants = Vec::new();
    for trip in [
        trip_of(3, MOVE_COST_PLAIN),
        trip_of(15, MOVE_COST_PLAIN),
        trip_of(30, MOVE_COST_PLAIN),
        trip_of(10, MOVE_COST_SWAMP),
        trip_of(12, MOVE_COST_ROAD),
    ] {
        for (mined, linked) in [(false, false), (true, false), (true, true)] {
            let sources = two_sources(trip, mined, linked);
            for (upgraders, build, fill) in [
                (false, 0, None),
                (false, 0, Some(1000)),
                (true, 0, None),
                (true, 20_000, None),
            ] {
                let t = WorkerTargets {
                    fill,
                    ..targets(&sources, upgraders, build, trip)
                };
                // The highest floor so far: the best value only rises with more energy.
                let mut last: Option<i64> = None;
                let high = trip == trip_of(15, MOVE_COST_PLAIN) && !upgraders;
                let more: Vec<u32> = if high {
                    (5..=8).map(capacity_at).collect()
                } else {
                    Vec::new()
                };
                for &energy in energies.iter().chain(&more) {
                    let options: Vec<(Body, i64, u32)> = worker_options(&t, energy)
                        .into_iter()
                        .filter(|o| walks_its_duties(&t, &o.0))
                        .collect();
                    let tag = format!(
                        "{trip:?} mined {mined} linked {linked} upgraders {upgraders} build {build} fill {fill:?} at {energy}"
                    );
                    let Some(body) = worker_plan(&t, energy, &mut WorkerScratch::default()) else {
                        assert!(options.is_empty(), "{tag}: a worker fits");
                        continue;
                    };
                    let tag = format!("{tag}: {body:?}");
                    assert!(body.cost() <= energy, "{tag}");
                    assert!(body.len() <= MAX_CREEP_SIZE as usize, "{tag}");
                    let (w, c, m) = (
                        count(&body, Part::Work),
                        count(&body, Part::Carry),
                        count(&body, Part::Move),
                    );
                    assert!(w >= 1 && c >= 1 && m >= 1, "{tag}");
                    assert!(w <= t.max_work, "{tag}");
                    assert_eq!(body, wcm(w, c, m), "{tag}: WORK, CARRY, MOVE order");
                    assert!(walks_its_duties(&t, &body), "{tag}: too slow empty");
                    let best = options.iter().map(|o| o.1).max().expect("options");
                    let floor = best - slack_of(&t);
                    source_wants(&t, &body, &mut wants);
                    let (value, n) = worker_value(&t, &body, &wants);
                    assert!(value >= floor, "{tag}: {value} below {floor}");
                    let key = worker_key(&body, value, n);
                    for (other, v, k) in options.iter().filter(|o| o.1 >= floor) {
                        assert!(
                            worker_key(other, *v, *k) >= key,
                            "{tag}: {other:?} comes first"
                        );
                    }
                    assert!(
                        last.is_none_or(|l| value >= l),
                        "{tag}: below an earlier energy's floor"
                    );
                    last = Some(last.map_or(floor, |l| l.max(floor)));
                }
            }
        }
    }
    let none = targets(&[], false, 0, trip_of(1, MOVE_COST_PLAIN));
    assert_eq!(
        worker_plan(&none, capacity_at(8), &mut WorkerScratch::default()),
        smallest_worker(),
        "nothing to size for"
    );
    let smallest = smallest_worker().expect("smallest").cost();
    assert_eq!(
        worker_plan(&none, smallest - 1, &mut WorkerScratch::default()),
        None
    );
}

/// Uses: MOVE_COST_PLAIN, MOVE_POWER, the level capacities. Without upgraders, with linked sources,
/// a duty trip whose range-based tiles are 0 (the spawn or a mined source within 4 of the
/// controller) or a detour (more cost than its tiles' plain cost) still solves to a body at every
/// capacity, and that body walks empty at a tile a tick on plain (one MOVE part a WORK part at
/// least), as the floor states; a floor measured against the trip's tiles once emptied the search.
#[test]
fn worker_plans_survive_short_and_detoured_duty_trips() {
    for trip in [
        Trip {
            tiles: 0,
            cost: MOVE_COST_PLAIN,
        },
        Trip {
            tiles: 2,
            cost: 8 * MOVE_COST_PLAIN,
        },
    ] {
        let sources = two_sources(trip, true, true);
        let t = targets(&sources, false, 0, trip);
        assert!(t.duties());
        for rcl in 2..=8 {
            let body = worker_plan(&t, capacity_at(rcl), &mut WorkerScratch::default())
                .unwrap_or_else(|| panic!("{trip:?} RCL{rcl}: a body"));
            let (w, m) = (count(&body, Part::Work), count(&body, Part::Move));
            assert!(
                w * MOVE_COST_PLAIN <= MOVE_POWER * m,
                "{trip:?} RCL{rcl}: {body:?}"
            );
        }
    }
}

/// Uses: CARRY_CAPACITY, BUILD_POWER, MOVE_POWER, MOVE_COST_PLAIN. A builder's round: the take, the
/// loaded and the empty walk ([`walk_ticks`]), and a load built at `BUILD_POWER` per WORK part.
#[test]
fn a_build_round_from_constants() {
    let trip = trip_of(12, MOVE_COST_PLAIN);
    for (w, c, m) in [(1, 1, 1), (2, 4, 3), (5, 2, 1)] {
        let body = wcm(w, c, m);
        let load = c * CARRY_CAPACITY;
        let loaded = walk_ticks(trip, w + c, m).expect("walk");
        let empty = walk_ticks(trip, w, m).expect("walk");
        assert_eq!(
            build_round(&body, trip),
            Some(1 + loaded + empty + load.div_ceil(BUILD_POWER * w)),
            "{body:?}"
        );
    }
    assert_eq!(
        build_round(&Body::new(&[Part::Work, Part::Move]).expect("body"), trip),
        None
    );
}

/// Uses: CREEP_LIFE_TIME, CONTAINER_DECAY, CONTAINER_DECAY_TIME_OWNED, REPAIR_COST. A mining
/// container of an owned room loses `CONTAINER_DECAY` hits every `CONTAINER_DECAY_TIME_OWNED` − 1
/// ticks (V214, as the world model decays it), each repaired for `REPAIR_COST` energy (V222): its
/// repairs over a creep life.
#[test]
fn a_container_upkeep_from_constants() {
    let hits = CREEP_LIFE_TIME * CONTAINER_DECAY / (CONTAINER_DECAY_TIME_OWNED - 1);
    assert_eq!(container_upkeep(), hits.div_ceil(hits_per_energy()));
}

/// Millionths of a worker, as `worker_value` counts them.
const MICRO: u128 = 1_000_000;

/// Uses: SOURCE_ENERGY_CAPACITY, CREEP_LIFE_TIME, ENERGY_REGEN_TIME, CARRY_CAPACITY, BUILD_POWER,
/// REPAIR_POWER, REPAIR_COST, BODYPART_COST. A pinned example of the value, with workers counted as
/// fractions: each source keeps `capacity × round / (ENERGY_REGEN_TIME × load)` workers (or what its
/// open tiles allow), which carry its yield over a life (`yield_per_life`); `max_workers` keeps
/// fewer here, which carry that share; every worker kept costs its body. With upgraders spending
/// both sources, the room keeps `min_workers` builders, counted whole, which spend the build work
/// in the time their build rounds and the containers' repair rounds leave them (all of it when
/// that time fits in their lives, else that share).
#[test]
fn a_worker_value_from_constants() {
    let trip = trip_of(15, MOVE_COST_PLAIN);
    let sources = two_sources(trip, false, false);
    let t = targets(&sources, false, 0, trip);
    let body = wcm(2, 2, 2);
    let mut wants = Vec::new();
    source_wants(&t, &body, &mut wants);
    let round = worker_round(&body, trip).expect("round");
    let n = workers_for_source(SOURCE_ENERGY_CAPACITY, 3, &body, round);
    assert_eq!(wants, [n, n]);
    let load = u128::from(2 * CARRY_CAPACITY);
    let life = u128::from(CREEP_LIFE_TIME);
    let ticks = u128::from(round.ticks());
    let drain =
        u128::from(SOURCE_ENERGY_CAPACITY) * ticks * MICRO / (u128::from(ENERGY_REGEN_TIME) * load);
    let slots = 3 * ticks * MICRO / u128::from(round.harvest);
    let each = drain.min(slots);
    let carried = (each * load * life / (ticks * MICRO))
        .min(u128::from(yield_per_life(SOURCE_ENERGY_CAPACITY)));
    let need = 2 * each;
    let kept = need.clamp(
        u128::from(OPENING_MMO.min_workers) * MICRO,
        u128::from(OPENING_MMO.max_workers) * MICRO,
    );
    assert!(kept < need, "the example is capped by max_workers");
    let delivered = 2 * carried * kept / need;
    let expected = delivered as i64 - (kept * u128::from(body.cost()) / MICRO) as i64;
    let whole = (2 * n).clamp(OPENING_MMO.min_workers, OPENING_MMO.max_workers);
    assert_eq!(worker_value(&t, &body, &wants), (expected, whole));

    // Upgraders spend both (linked, mined) sources: the `min_workers` builders build and repair.
    let spent = two_sources(trip, true, true);
    let w = count(&body, Part::Work);
    let (loaded, empty) = (
        walk_ticks(trip, w + 2, count(&body, Part::Move)).expect("walk"),
        walk_ticks(trip, w, count(&body, Part::Move)).expect("walk"),
    );
    let repair =
        1 + loaded + empty + (2 * CARRY_CAPACITY).div_ceil(w * REPAIR_POWER / hits_per_energy());
    let duty = |energy: u128, round: u32| energy * u128::from(round) * MICRO / (load * life);
    let repairs = 2 * duty(u128::from(container_upkeep()), repair);
    for build in [0u32, 500, 1_000_000] {
        let t = targets(&spent, true, build, trip);
        source_wants(&t, &body, &mut wants);
        assert_eq!(wants, [0, 0]);
        let r = build_round(&body, trip).expect("round");
        let need = repairs
            + if build > 0 {
                duty(u128::from(build), r)
            } else {
                0
            };
        // The room keeps `min_workers`, counted whole: a build they cannot finish in a life is
        // built in that share.
        let kept = u128::from(OPENING_MMO.min_workers) * MICRO;
        let built = if kept >= need {
            u128::from(build)
        } else {
            u128::from(build) * kept / need
        };
        let expected = built as i64 - (kept * u128::from(body.cost()) / MICRO) as i64;
        assert_eq!(
            worker_value(&t, &body, &wants),
            (expected, OPENING_MMO.min_workers),
            "build {build}"
        );
    }
    // Without upgraders, two linked mined sources share one pool of workers, which take next to
    // the controller (no walk) and walk for their duties: the containers' repairs on the sources'
    // trips, and the fills along the build trip (the workers' own bodies and 1000 for the others),
    // each a fill of at most `unload` taking a transfer and a step.
    let linked = two_sources(trip, true, true);
    let t = WorkerTargets {
        fill: Some(1000),
        ..targets(&linked, false, 0, trip)
    };
    source_wants(&t, &body, &mut wants);
    let pool = carry_round(&body, Trip { tiles: 0, cost: 0 }).expect("round");
    let total = workers_for_source(2 * SOURCE_ENERGY_CAPACITY, 6, &body, pool);
    assert_eq!(
        wants,
        [total.div_ceil(2), total / 2],
        "the pool, split in order"
    );
    let ticks = u128::from(pool.ticks());
    let workers = (u128::from(2 * SOURCE_ENERGY_CAPACITY) * ticks * MICRO
        / (u128::from(ENERGY_REGEN_TIME) * load))
        .min(6 * ticks * MICRO / u128::from(pool.harvest));
    let delivered = (workers * load * life / (ticks * MICRO))
        .min(2 * u128::from(yield_per_life(SOURCE_ENERGY_CAPACITY)));
    let fill_round = loaded + empty + 2 * (2 * CARRY_CAPACITY).div_ceil(t.unload);
    let own = (workers + repairs) * u128::from(body.cost()) / MICRO;
    let need = workers + repairs + duty(own + 1000, fill_round);
    let kept = need.clamp(
        u128::from(OPENING_MMO.min_workers) * MICRO,
        u128::from(OPENING_MMO.max_workers) * MICRO,
    );
    assert_eq!(kept, need, "the example is within the counts");
    let expected = delivered as i64 - (kept * u128::from(body.cost()) / MICRO) as i64;
    let whole = (wants[0] + wants[1]).clamp(OPENING_MMO.min_workers, OPENING_MMO.max_workers);
    assert_eq!(worker_value(&t, &body, &wants), (expected, whole));

    // With no build work the builders are the smallest worker.
    let t = targets(&spent, true, 0, trip);
    assert_eq!(
        worker_plan(&t, capacity_at(8), &mut WorkerScratch::default()),
        smallest_worker()
    );
}

/// Uses: MOVE_COST_PLAIN, MOVE_COST_SWAMP, the level capacities. Bodies follow the trip: across
/// swamp the solved worker has at least the MOVE parts per other part it has on plain, and the
/// solved body is never worth less than the old fixed pattern's largest repeat (P2.4's worker).
#[test]
fn worker_bodies_follow_the_trip() {
    let mut wants = Vec::new();
    for rcl in 1..=4 {
        let energy = capacity_at(rcl);
        let ratio = |rate: u32, scratch: &mut WorkerScratch| {
            let trip = trip_of(12, rate);
            let sources = two_sources(trip, false, false);
            let t = targets(&sources, false, 0, trip);
            let b = worker_plan(&t, energy, scratch).expect("body");
            let m = count(&b, Part::Move);
            (m, b.len() as u32 - m, b)
        };
        let mut scratch = WorkerScratch::default();
        let (pm, po, plain) = ratio(MOVE_COST_PLAIN, &mut scratch);
        let (sm, so, swamp) = ratio(MOVE_COST_SWAMP, &mut scratch);
        assert!(
            sm * po >= pm * so,
            "RCL{rcl}: swamp {swamp:?}, plain {plain:?}"
        );
        for rate in [MOVE_COST_PLAIN, MOVE_COST_SWAMP] {
            let trip = trip_of(12, rate);
            let sources = two_sources(trip, false, false);
            let t = targets(&sources, false, 0, trip);
            let solved = worker_plan(&t, energy, &mut WorkerScratch::default()).expect("body");
            source_wants(&t, &solved, &mut wants);
            let (value, _) = worker_value(&t, &solved, &wants);
            let unit = smallest_worker().expect("smallest");
            let old = Body::scaled(unit.parts(), energy, u8::MAX).expect("pattern");
            source_wants(&t, &old, &mut wants);
            let (old_value, _) = worker_value(&t, &old, &wants);
            assert!(
                value >= old_value,
                "RCL{rcl} rate {rate}: {solved:?} against {old:?}"
            );
        }
    }
}

/// Uses: HARVEST_POWER, ENERGY_REGEN_TIME, SOURCE_ENERGY_CAPACITY, SOURCE_ENERGY_NEUTRAL_CAPACITY,
/// SOURCE_ENERGY_KEEPER_CAPACITY. A miner's WORK parts take a source's whole capacity within one
/// regeneration window, and one part fewer would not.
#[test]
fn a_miners_work_parts_drain_its_source() {
    for capacity in [
        SOURCE_ENERGY_NEUTRAL_CAPACITY,
        SOURCE_ENERGY_CAPACITY,
        SOURCE_ENERGY_KEEPER_CAPACITY,
    ] {
        let work = miner_work(capacity);
        let per_window = HARVEST_POWER * ENERGY_REGEN_TIME;
        assert!(work * per_window >= capacity, "{capacity}");
        assert!((work - 1) * per_window < capacity, "{capacity}");
    }
    assert_eq!(miner_work(0), 0);
}

/// Every loss a miner body can have, by brute force: (MOVE parts, loss in energy ×
/// ENERGY_REGEN_TIME) for each MOVE count that fits `energy`. Uses: BODYPART_COST,
/// ENERGY_REGEN_TIME, MAX_CREEP_SIZE.
fn miner_losses(capacity: u32, trip: Trip, energy: u32) -> Vec<(u32, u64)> {
    let work = miner_work(capacity);
    (1..=MAX_CREEP_SIZE - work)
        .filter(|&m| work * Part::Work.cost() + m * Part::Move.cost() <= energy)
        .map(|m| {
            let walk = walk_ticks(trip, work, m).expect("walk");
            let loss = u64::from(m * Part::Move.cost()) * u64::from(ENERGY_REGEN_TIME)
                + u64::from(walk) * u64::from(capacity);
            (m, loss)
        })
        .collect()
}

/// Property test over source capacities, trips (plain, swamp and road tiles) and the room
/// capacities of every level: a miner body costs at most the energy, has `miner_work` WORK parts
/// and at least one MOVE, at most MAX_CREEP_SIZE parts, the least loss of every MOVE count that
/// fits (the fewest MOVE among equal losses), and more energy never makes the loss larger.
/// Uses: BODYPART_COST, MOVE_COST_PLAIN, MOVE_COST_SWAMP, MOVE_COST_ROAD, MAX_CREEP_SIZE,
/// SPAWN_ENERGY_CAPACITY, CONTROLLER_STRUCTURES, EXTENSION_ENERGY_CAPACITY.
#[test]
fn miner_bodies_fit_and_lose_the_least() {
    let energies: Vec<u32> = (1..=8u32)
        .map(|rcl| {
            SPAWN_ENERGY_CAPACITY
                + StructureType::Extension.controller_structures(rcl)
                    * extension_energy_capacity(rcl)
        })
        .chain([0, 299, 549, 551, 649, 1000])
        .collect();
    for capacity in [
        SOURCE_ENERGY_NEUTRAL_CAPACITY,
        SOURCE_ENERGY_CAPACITY,
        SOURCE_ENERGY_KEEPER_CAPACITY,
    ] {
        let work = miner_work(capacity);
        for tiles in [1u32, 5, 13, 30, 60] {
            for rate in [MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP] {
                let trip = Trip {
                    tiles,
                    cost: tiles * rate,
                };
                let mut sorted = energies.clone();
                sorted.sort_unstable();
                let mut last: Option<u64> = None;
                for &energy in &sorted {
                    let losses = miner_losses(capacity, trip, energy);
                    let Some(body) = miner_body(capacity, trip, energy) else {
                        assert!(losses.is_empty(), "{capacity} {tiles} {rate} {energy}");
                        continue;
                    };
                    assert!(body.cost() <= energy);
                    assert!(body.len() <= MAX_CREEP_SIZE as usize);
                    assert_eq!(count(&body, Part::Work), work);
                    assert_eq!(count(&body, Part::Carry), 0);
                    let moves = count(&body, Part::Move);
                    assert!(moves >= 1);
                    assert_eq!(body.len() as u32, work + moves, "only WORK and MOVE");
                    let least = losses.iter().map(|l| l.1).min().expect("a loss");
                    let fewest = losses.iter().find(|l| l.1 == least).expect("least").0;
                    assert_eq!(moves, fewest, "{capacity} {tiles} {rate} {energy}");
                    if let Some(prev) = last {
                        assert!(least <= prev, "more energy, no larger loss");
                    }
                    last = Some(least);
                }
            }
        }
    }
}

/// At RCL2's full capacity a source of SOURCE_ENERGY_CAPACITY gets its miner, with as many MOVE
/// parts as the energy left after the WORK parts pays for (one, at 550); at RCL1's spawn alone it
/// gets none. Uses: BODYPART_COST, SPAWN_ENERGY_CAPACITY, CONTROLLER_STRUCTURES,
/// EXTENSION_ENERGY_CAPACITY, MOVE_COST_PLAIN.
#[test]
fn miner_bodies_by_level() {
    let trip = Trip {
        tiles: 15,
        cost: 15 * MOVE_COST_PLAIN,
    };
    let work = miner_work(SOURCE_ENERGY_CAPACITY);
    let rcl2 = SPAWN_ENERGY_CAPACITY
        + StructureType::Extension.controller_structures(2) * extension_energy_capacity(2);
    let left = rcl2 - work * Part::Work.cost();
    let body = miner_body(SOURCE_ENERGY_CAPACITY, trip, rcl2).expect("a miner at RCL2");
    assert!(count(&body, Part::Move) <= left / Part::Move.cost());
    assert!(work * Part::Work.cost() + Part::Move.cost() > SPAWN_ENERGY_CAPACITY);
    assert_eq!(
        miner_body(SOURCE_ENERGY_CAPACITY, trip, SPAWN_ENERGY_CAPACITY),
        None
    );
}

/// Uses: CARRY_CAPACITY, UPGRADE_CONTROLLER_POWER, MOVE_COST_PLAIN. At a mined source a worker's
/// round takes one tick there; its walk and upgrade are the harvesting round's.
#[test]
fn a_carry_round_takes_one_tick_at_the_source() {
    let trip = Trip {
        tiles: 12,
        cost: 12 * MOVE_COST_PLAIN,
    };
    for n in 1..=4 {
        let body = repeats(n);
        let harvest = worker_round(&body, trip).expect("round");
        let carry = carry_round(&body, trip).expect("round");
        assert_eq!(carry.harvest, 1);
        assert_eq!(carry.walk, harvest.walk);
        assert_eq!(
            carry.upgrade,
            (n * CARRY_CAPACITY).div_ceil(UPGRADE_CONTROLLER_POWER * n)
        );
    }
}

/// Uses: MOVE_POWER, MOVE_COST_PLAIN, MOVE_COST_SWAMP, CARRY_CAPACITY, EXTENSION_ENERGY_CAPACITY. A
/// hauler's round: one tick to take, the walk loaded with every CARRY part weighing, a transfer a
/// tick of at most a fill's capacity with a step between two, and the walk back empty with none
/// (V54).
#[test]
fn a_haul_round_from_constants() {
    for tiles in [1u32, 8, 20] {
        for rate in [MOVE_COST_PLAIN, MOVE_COST_SWAMP] {
            let trip = Trip {
                tiles,
                cost: tiles * rate,
            };
            for (carry, moves) in [(5u32, 5u32), (10, 5), (4, 1)] {
                let mut parts = vec![Part::Carry; carry as usize];
                parts.extend(vec![Part::Move; moves as usize]);
                let body = Body::new(&parts).expect("body");
                let loaded = tiles.max((trip.cost * carry).div_ceil(MOVE_POWER * moves));
                for rcl in [2u32, 7, 8] {
                    let unload = extension_energy_capacity(rcl);
                    let fills = (carry * CARRY_CAPACITY).div_ceil(unload);
                    // Empty, nothing weighs: one tile a tick.
                    assert_eq!(
                        haul_round(&body, trip, unload),
                        Some(loaded + tiles + 1 + (2 * fills - 1)),
                        "{carry} CARRY {moves} MOVE, {tiles} tiles at {rate}, RCL{rcl}"
                    );
                }
                // A sink that takes a whole load: one transfer.
                assert_eq!(
                    haul_round(&body, trip, carry * CARRY_CAPACITY),
                    Some(loaded + tiles + 2)
                );
            }
        }
    }
    let trip = Trip {
        tiles: 10,
        cost: 10 * MOVE_COST_PLAIN,
    };
    let no_carry = Body::new(&[Part::Move, Part::Move]).expect("body");
    assert_eq!(
        haul_round(&no_carry, trip, CARRY_CAPACITY),
        None,
        "no CARRY"
    );
    let no_move = Body::new(&[Part::Carry]).expect("body");
    assert_eq!(haul_round(&no_move, trip, CARRY_CAPACITY), None, "no MOVE");
    // A WORK part weighs both ways.
    let worker = Body::new(&[Part::Work, Part::Carry, Part::Move]).expect("body");
    assert_eq!(
        haul_round(&worker, trip, CARRY_CAPACITY),
        Some(
            trip.tiles.max((trip.cost * 2).div_ceil(MOVE_POWER))
                + trip.tiles.max(trip.cost.div_ceil(MOVE_POWER))
                + 2
        )
    );
}

/// Uses: CARRY_CAPACITY, ENERGY_REGEN_TIME, SOURCE_ENERGY_CAPACITY. The least count whose loads per
/// round cover the source's yield over a round with the margin: `n` covers it and `n - 1` does not.
#[test]
fn haulers_carry_a_sources_yield_with_the_margin() {
    let covers = |n: u32, capacity: u32, carry: u32, round: u32, margin: u32| {
        u128::from(n) * u128::from(carry * CARRY_CAPACITY) * u128::from(ENERGY_REGEN_TIME) * 100
            >= u128::from(capacity) * u128::from(round) * u128::from(100 + margin)
    };
    for capacity in [
        SOURCE_ENERGY_NEUTRAL_CAPACITY,
        SOURCE_ENERGY_CAPACITY,
        SOURCE_ENERGY_KEEPER_CAPACITY,
    ] {
        for carry in [1u32, 5, 16, 25] {
            for round in [3u32, 42, 120, 400] {
                for margin in [0u32, 10, 100] {
                    let n = haulers_for(capacity, carry, round, margin);
                    assert!(n >= 1);
                    assert!(covers(n, capacity, carry, round, margin));
                    assert!(!covers(n - 1, capacity, carry, round, margin));
                }
            }
        }
    }
    assert_eq!(haulers_for(0, 5, 42, 10), 0, "an empty source");
    assert_eq!(
        haulers_for(SOURCE_ENERGY_CAPACITY, 0, 42, 10),
        0,
        "no CARRY"
    );
}

/// Every hauler body that fits `energy`, by brute force: (haulers, energy for all of them, MOVE
/// parts, CARRY parts). Uses: BODYPART_COST, MAX_CREEP_SIZE, MOVE_POWER, CARRY_CAPACITY.
fn hauler_options(
    capacity: u32,
    trip: Trip,
    energy: u32,
    margin: u32,
    unload: u32,
) -> Vec<(u32, u64, u32, u32)> {
    let mut out = Vec::new();
    // A hauler holds at most a container's whole load.
    for carry in 1..=(CONTAINER_CAPACITY / CARRY_CAPACITY).min(MAX_CREEP_SIZE - 1) {
        for moves in 1..=MAX_CREEP_SIZE - carry {
            let cost = carry * Part::Carry.cost() + moves * Part::Move.cost();
            if cost > energy {
                continue;
            }
            let loaded = trip
                .tiles
                .max((trip.cost * carry).div_ceil(MOVE_POWER * moves));
            let round = loaded + trip.tiles + 2 * (carry * CARRY_CAPACITY).div_ceil(unload).max(1);
            let n = haulers_for(capacity, carry, round, margin);
            out.push((n, u64::from(n) * u64::from(cost), moves, carry));
        }
    }
    out
}

/// Property test over source capacities, trips (road, plain and swamp tiles), margins and the room
/// capacities of every level: a hauler plan's body is CARRY parts then MOVE parts, costs at most the
/// energy and holds at most MAX_CREEP_SIZE parts; its count carries the yield (`haulers_for` of its
/// own round); it has the fewest haulers of every body that fits, then the least energy for all of
/// them, then the fewest MOVE parts; and more energy never asks for more haulers.
/// Uses: BODYPART_COST, MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP, MAX_CREEP_SIZE,
/// SPAWN_ENERGY_CAPACITY, CONTROLLER_STRUCTURES, EXTENSION_ENERGY_CAPACITY.
#[test]
fn hauler_plans_fit_and_take_the_fewest() {
    let mut energies: Vec<u32> = (1..=8u32)
        .map(|rcl| {
            SPAWN_ENERGY_CAPACITY
                + StructureType::Extension.controller_structures(rcl)
                    * extension_energy_capacity(rcl)
        })
        .chain([0, 99, 100, 101, 449, 551, 1000])
        .collect();
    energies.sort_unstable();
    for capacity in [
        SOURCE_ENERGY_NEUTRAL_CAPACITY,
        SOURCE_ENERGY_CAPACITY,
        SOURCE_ENERGY_KEEPER_CAPACITY,
    ] {
        for tiles in [0u32, 1, 7, 20, 45] {
            for rate in [MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP] {
                let trip = Trip {
                    tiles,
                    cost: tiles * rate,
                };
                for (margin, unload) in [
                    (0u32, extension_energy_capacity(2)),
                    (HAULING_MMO.margin_pct, extension_energy_capacity(7)),
                    (50, extension_energy_capacity(8)),
                ] {
                    let mut last: Option<u32> = None;
                    for &energy in &energies {
                        let options = hauler_options(capacity, trip, energy, margin, unload);
                        let Some((body, n)) = hauler_plan(capacity, trip, energy, margin, unload)
                        else {
                            assert!(options.is_empty(), "{capacity} {tiles} {rate} {energy}");
                            continue;
                        };
                        let carry = count(&body, Part::Carry);
                        let moves = count(&body, Part::Move);
                        assert!(body.cost() <= energy);
                        assert!(body.len() <= MAX_CREEP_SIZE as usize);
                        assert!(carry >= 1 && moves >= 1);
                        assert!(
                            carry * CARRY_CAPACITY <= CONTAINER_CAPACITY,
                            "a whole load fits"
                        );
                        assert_eq!(body.len() as u32, carry + moves, "only CARRY and MOVE");
                        let first_move = body.parts().iter().position(|&p| p == Part::Move);
                        assert_eq!(first_move, Some(carry as usize), "CARRY first");
                        let round = haul_round(&body, trip, unload).expect("round");
                        assert_eq!(n, haulers_for(capacity, carry, round, margin));
                        let best = options
                            .iter()
                            .map(|o| (o.0, o.1, o.2))
                            .min()
                            .expect("an option");
                        let got = (n, u64::from(n) * u64::from(body.cost()), moves);
                        assert_eq!(got, best, "{capacity} {tiles} {rate} {energy} {margin}");
                        if let Some(prev) = last {
                            assert!(n <= prev, "more energy, no more haulers");
                        }
                        last = Some(n);
                    }
                }
            }
        }
    }
    let trip = Trip {
        tiles: 10,
        cost: 10 * MOVE_COST_PLAIN,
    };
    assert_eq!(
        hauler_plan(0, trip, 1000, 10, CARRY_CAPACITY),
        None,
        "an empty source"
    );
}

/// The counts TUNING cites for `HaulingConfig::max_per_source`, against brute force: at RCL2's full
/// capacity a source of SOURCE_ENERGY_CAPACITY 20 plain tiles from its sink takes two haulers and 40
/// tiles away four (the cap's default), and at RCL4's 10 tiles away one.
/// Uses: SPAWN_ENERGY_CAPACITY, CONTROLLER_STRUCTURES, EXTENSION_ENERGY_CAPACITY, MOVE_COST_PLAIN.
#[test]
fn hauler_counts_by_level() {
    let energy = |rcl: u32| {
        SPAWN_ENERGY_CAPACITY
            + StructureType::Extension.controller_structures(rcl) * extension_energy_capacity(rcl)
    };
    let margin = HAULING_MMO.margin_pct;
    let plain = |tiles: u32| Trip {
        tiles,
        cost: tiles * MOVE_COST_PLAIN,
    };
    let fewest = |trip: Trip, e: u32, unload: u32| {
        hauler_options(SOURCE_ENERGY_CAPACITY, trip, e, margin, unload)
            .iter()
            .map(|o| o.0)
            .min()
            .expect("an option")
    };
    for (rcl, tiles, want) in [(2, 20, 2), (2, 40, HAULING_MMO.max_per_source), (4, 10, 1)] {
        let unload = extension_energy_capacity(rcl);
        let (_, n) = hauler_plan(
            SOURCE_ENERGY_CAPACITY,
            plain(tiles),
            energy(rcl),
            margin,
            unload,
        )
        .expect("a plan");
        assert_eq!(n, fewest(plain(tiles), energy(rcl), unload));
        assert_eq!(n, want, "RCL{rcl}, {tiles} tiles");
    }
}

// ---------------------------------------------------------------------------------------------
// Watchdog

fn observation(time: u32, alive: u32, working: u32) -> Observation {
    Observation {
        time,
        alive,
        working,
        want: 12,
        level: 2,
        controller: ControllerState {
            progress: 0,
            downgrade_ticks: 9000,
            upgrade_blocked: false,
        },
        energy: 0,
        capacity: 550,
        sources: 6000,
        building: 0,
        requesting: false,
    }
}

#[test]
fn the_watchdog_starts_and_ends_a_recovery() {
    let cfg = OPENING_MMO;
    let mut w = Watchdog::default();
    let mut events = Vec::new();
    w.observe(&observation(10, 0, 0), &cfg, &mut events);
    assert_eq!(events, [Event::RecoveryStarted]);
    assert!(w.recovering());
    events.clear();
    // One worker is not enough while min_workers is 2.
    w.observe(&observation(20, 1, 1), &cfg, &mut events);
    assert!(events.is_empty() && w.recovering());
    let mut o = observation(30, cfg.min_workers, cfg.min_workers);
    o.energy = 5;
    w.observe(&o, &cfg, &mut events);
    assert_eq!(events, [Event::Recovered { ticks: 20 }]);
    assert!(!w.recovering());
}

/// Uses: CONTROLLER_DOWNGRADE_RESTORE. On below the threshold; off only one upgrade tick's restore
/// above it (V189), so the first upgrade does not turn it off again.
#[test]
fn the_watchdog_guards_the_controller_with_hysteresis() {
    let cfg = OPENING_MMO;
    let g = cfg.downgrade_guard_ticks;
    let mut w = Watchdog::default();
    let mut events = Vec::new();
    let mut o = observation(10, 3, 3);
    o.controller.downgrade_ticks = g;
    w.observe(&o, &cfg, &mut events);
    assert!(!w.guard() && events.is_empty(), "at the threshold: off");
    o.time = 11;
    o.controller.downgrade_ticks = g - 1;
    w.observe(&o, &cfg, &mut events);
    assert_eq!(
        events,
        [Event::GuardOn {
            downgrade_ticks: g - 1
        }]
    );
    events.clear();
    // One upgrade tick later it is above the threshold but below the release: still on.
    o.time = 12;
    o.controller.downgrade_ticks = g - 2 + CONTROLLER_DOWNGRADE_RESTORE;
    w.observe(&o, &cfg, &mut events);
    assert!(w.guard() && events.is_empty());
    o.time = 13;
    o.controller.downgrade_ticks = g + CONTROLLER_DOWNGRADE_RESTORE;
    w.observe(&o, &cfg, &mut events);
    assert_eq!(events, [Event::GuardOff]);
    // An undefined timer is read as a full one by the snapshot, so it never turns the guard on;
    // a room with no level never has it.
    o.level = 0;
    o.controller.downgrade_ticks = 0;
    events.clear();
    w.observe(&o, &cfg, &mut events);
    assert!(!w.guard() && events.is_empty());
}

/// After `wait_ticks` with no new worker, bodies are capped at the most energy the room held; a new
/// worker keeps the cap, more energy raises it, and the room seen full drops it.
#[test]
fn a_request_falls_back_to_what_the_room_holds() {
    let cfg = OPENING_MMO;
    let mut w = Watchdog::default();
    let mut events = Vec::new();
    let mut o = observation(100, 3, 3);
    o.requesting = true;
    let peak = 450;
    w.observe(&o, &cfg, &mut events);
    for t in 101..100 + cfg.wait_ticks {
        o.time = t;
        o.energy = if t == 150 { peak } else { 300 };
        w.observe(&o, &cfg, &mut events);
    }
    assert!(w.cap().is_none() && events.is_empty());
    o.time = 100 + cfg.wait_ticks;
    w.observe(&o, &cfg, &mut events);
    assert_eq!(
        events,
        [Event::WaitFallback {
            ticks: cfg.wait_ticks,
            energy: peak
        }]
    );
    assert_eq!(w.cap(), Some(peak));
    events.clear();
    // A new worker does not start the wait over: the cap stays.
    o.time += 1;
    o.alive += 1;
    w.observe(&o, &cfg, &mut events);
    assert_eq!(w.cap(), Some(peak));
    // More energy raises it.
    o.time += 1;
    o.energy = peak + 50;
    w.observe(&o, &cfg, &mut events);
    assert_eq!(w.cap(), Some(peak + 50));
    // The room full: no cap, and a later wait starts over.
    o.time += 1;
    o.energy = o.capacity;
    w.observe(&o, &cfg, &mut events);
    assert_eq!(w.cap(), None);
    assert!(events.is_empty());
}

/// A room whose workers move nothing is reported once, even while its spawn regenerates (its
/// energy changes); a harvest, controller progress or construction progress resets the clock.
#[test]
fn a_room_that_stands_still_is_reported_once() {
    let cfg = OPENING_MMO;
    let mut w = Watchdog::default();
    let mut events = Vec::new();
    let mut o = observation(0, 3, 3);
    let mut replans = 0;
    for t in 0..=cfg.stall_ticks * 2 {
        o.time = t;
        // The spawn's own regeneration (V174) is not the workers' doing.
        o.energy = t % SPAWN_ENERGY_CAPACITY;
        if w.observe(&o, &cfg, &mut events) {
            replans += 1;
        }
    }
    assert_eq!(replans, 1);
    assert_eq!(
        events,
        [Event::Stalled {
            ticks: cfg.stall_ticks
        }]
    );
    events.clear();
    o.time += 1;
    o.sources -= HARVEST_POWER;
    w.observe(&o, &cfg, &mut events);
    assert_eq!(
        events,
        [Event::StallEnded {
            ticks: cfg.stall_ticks + 1
        }]
    );
    // No workers that can work is a recovery, not a stall.
    let mut w = Watchdog::default();
    events.clear();
    let o = observation(0, 0, 0);
    for t in 0..=cfg.stall_ticks * 2 {
        assert!(!w.observe(&Observation { time: t, ..o }, &cfg, &mut events));
    }
    assert_eq!(events, [Event::RecoveryStarted]);
    // A room that wants no workers (no source) never starts one.
    let mut w = Watchdog::default();
    events.clear();
    w.observe(&Observation { want: 0, ..o }, &cfg, &mut events);
    assert!(events.is_empty() && !w.recovering());
}

// ---------------------------------------------------------------------------------------------
// The world model

const ROOM: &str = "W1N1";

/// Haulers and the controller container off (P3.3): the economy as it was before them.
const HAULING_OFF: HaulingConfig = HaulingConfig {
    enabled: false,
    place_container: false,
    ..HAULING_MMO
};

fn tile(x: u8, y: u8) -> Tile {
    Tile::new(x, y).expect("in room")
}

#[derive(Clone, Debug)]
struct SimCreep {
    key: u128,
    identity: Identity,
    at: Tile,
    body: Body,
    energy: u32,
    fatigue: u32,
    /// Ticks of spawning left (0: spawned).
    spawning: u32,
    born: u32,
}

impl SimCreep {
    fn capacity(&self) -> u32 {
        count(&self.body, Part::Carry) * CARRY_CAPACITY
    }
}

#[derive(Clone, Debug)]
struct SimStore {
    key: u128,
    kind: StructureType,
    at: Tile,
    energy: u32,
    capacity: u32,
    /// A link's cooldown (P3.2); 0 for the others.
    cooldown: u32,
}

impl SimStore {
    /// Spawns and extensions: what `Room.energyAvailable` sums and spawning drains (V173, V175).
    fn spawn_energy(&self) -> bool {
        matches!(self.kind, StructureType::Spawn | StructureType::Extension)
    }
}

#[derive(Clone, Debug)]
struct SimSource {
    key: u128,
    at: Tile,
    energy: u32,
    regen_at: Option<u32>,
}

#[derive(Clone, Debug)]
struct SimSite {
    key: u128,
    kind: StructureType,
    at: Tile,
    progress: u32,
    total: u32,
}

#[derive(Clone, Debug)]
enum Loose {
    Pile {
        key: u128,
        at: Tile,
        amount: u32,
    },
    Tomb {
        key: u128,
        at: Tile,
        energy: u32,
        decay_at: u32,
    },
    /// A container (ADR-023): it holds up to `CONTAINER_CAPACITY`, and loses `CONTAINER_DECAY` hits
    /// once the tick reaches `decay_at` − 1 (V214).
    Container {
        key: u128,
        at: Tile,
        energy: u32,
        hits: u32,
        decay_at: u32,
    },
}

/// What the world adds when the controller levels up: the level's extensions built at once (the
/// default), placed as sites for the workers to build (what ADR-020's placement would do), or
/// nothing (no placement: the bot places no site before ADR-020 is decided).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LevelUp {
    Built,
    Sites,
    Nothing,
}

/// What a row of this tick's snapshot was.
#[derive(Clone, Copy, Debug)]
enum StructRow {
    Controller,
    Store(usize),
}
#[derive(Clone, Copy, Debug)]
enum ObjRow {
    Source(usize),
    Site(usize),
    Loose(usize),
}

struct World {
    room: RoomName,
    time: u32,
    raw: Vec<u8>,
    level: u8,
    progress: u32,
    downgrade_at: u32,
    controller_at: Tile,
    stores: Vec<SimStore>,
    sources: Vec<SimSource>,
    sites: Vec<SimSite>,
    loose: Vec<Loose>,
    creeps: Vec<SimCreep>,
    next_key: u128,
    // The bot.
    snap: Snapshot,
    buffer: IntentBuffer,
    movement: Movement,
    spawner: Spawner,
    opening: Opening,
    mining: Mining,
    hauling: Hauling,
    upgrading: Upgrading,
    placement: Placement,
    cfg: OpeningConfig,
    mining_cfg: MiningConfig,
    link_cfg: LinkConfig,
    repair_cfg: RepairConfig,
    hauling_cfg: HaulingConfig,
    upgrading_cfg: UpgradingConfig,
    /// Haulers (P3.3): transfers into a container and the energy they moved; takes that found
    /// nothing left (an earlier take this tick emptied the target: billed for nothing).
    stored: u32,
    stored_energy: u64,
    empty_takes: u32,
    /// Takes by haulers and the energy they moved.
    hauler_takes: u32,
    hauler_taken: u64,
    /// Repairs (P3.5): calls that repaired, hits added, energy spent, and calls at full hits (billed
    /// for nothing); containers that decayed away.
    repairs: u32,
    repaired: u64,
    repair_energy: u64,
    repair_noops: u32,
    containers_lost: u32,
    /// The container tile of every repair call that repaired.
    repair_log: Vec<Tile>,
    /// Sites the bot's placement asked for: extensions accepted, containers accepted, links accepted
    /// (in the order placed), refused by the model.
    placed: u32,
    placed_containers: u32,
    placed_links: Vec<Tile>,
    /// Link sends: accepted, refused, clamped by the processor to less than asked, and the energy
    /// the receivers got; workers' takes from links.
    sends: u32,
    refused_sends: u32,
    clamped_sends: u32,
    received: u32,
    link_takes: u32,
    /// Each accepted send's sender.
    senders: Vec<Tile>,
    /// Orders dropped because a later order of the same tick came from the same link (the engine
    /// keeps the last call per object, V34, V216); the tick and the sender of every order; tiles the
    /// model refuses a site on; a tick before which every send is refused (`ERR_NOT_OWNER`).
    dup_sends: u32,
    orders_at: Vec<(u32, Tile)>,
    refuse_tiles: Vec<Tile>,
    refuse_sends_until: u32,
    /// Every take of the tick from a store (P3.4): the tick, the taker's role, the target's row,
    /// the amount asked (`None`: all that fits), the taker's free room and the target's energy at
    /// the tick's start. Kept only while `log_takes` is on.
    takes_log: Vec<(u32, Role, TargetRow, Option<u32>, u32, u32)>,
    log_takes: bool,
    /// Every transfer into a structure while `log_takes` is on: the tick, the role, the target.
    fills_log: Vec<(u32, Role, TargetRow)>,
    /// The tick's start, for the game side's checks (V187, V192): each creep's energy, each
    /// store's and each loose object's.
    start_creeps: Vec<u32>,
    start_stores: Vec<u32>,
    start_loose: Vec<u32>,
    /// Billed intents and calls since the start (V34): creep intents, spawn orders, sites, sends.
    billed: u64,
    refused_sites: u32,
    /// Whether the Standard tier runs (`opening work`); off, as below `standard_bucket_min`.
    standard: bool,
    /// The processor drops every site the game side accepted (V204's silent checks).
    drop_sites: bool,
    /// Walls built after the start (as structures: movement learns them on a rescan).
    extra_walls: Vec<Tile>,
    // What happened.
    events: Vec<(u32, Event)>,
    failed: u32,
    spawned: Vec<(u32, Body)>,
    /// Each worker spawned (P3.6): the tick, its body, the budget the opening requested it with
    /// that tick, and the plan's body then.
    worker_spawns: Vec<(u32, Body, Option<EnergyBudget>, Option<Body>)>,
    levels: Vec<(u32, u8)>,
    /// The progress of the levels completed since the start (`CONTROLLER_LEVELS` each), so a
    /// measure across a level-up counts all the upgrading ([`World::upgraded`]).
    leveled: u64,
    /// Each source refill: the tick, the source's index, and the energy it still held (none taken
    /// that cycle is wasted).
    regens: Vec<(u32, usize, u32)>,
    full_at: Option<u32>,
    struct_rows: Vec<StructRow>,
    obj_rows: Vec<ObjRow>,
    at_level_up: LevelUp,
}

/// Uses: REPAIR_COST. The hits a repair adds per energy: the engine divides the creep's energy by
/// `REPAIR_COST` in doubles (V222), which gives exactly 100 per energy for every store up to 2000.
fn hits_per_energy() -> u32 {
    (1.0 / f64::from(REPAIR_COST)).round() as u32
}

fn count(body: &Body, part: Part) -> u32 {
    u32::try_from(body.count(part)).expect("small")
}

fn place(room: RoomName, t: Tile) -> Position {
    Position::new(
        RoomCoordinate::new(t.x()).expect("x"),
        RoomCoordinate::new(t.y()).expect("y"),
        room,
    )
}

impl World {
    /// A room at `level`: walls on the border ring, plain inside, a spawn at (25,25) holding
    /// `spawn_energy`, the level's extensions (empty) in a row below it, two full sources and the
    /// controller. No creeps.
    fn new(level: u8, spawn_energy: u32) -> Self {
        let wall = Terrain::Wall as u8;
        let mut raw = Vec::with_capacity(2500);
        for y in 0..50u8 {
            for x in 0..50u8 {
                raw.push(if x == 0 || y == 0 || x == 49 || y == 49 {
                    wall
                } else {
                    0
                });
            }
        }
        let mut w = Self {
            room: ROOM.parse().expect("room"),
            time: 10_000,
            raw,
            level,
            progress: 0,
            downgrade_at: 0,
            controller_at: tile(25, 8),
            stores: Vec::new(),
            sources: Vec::new(),
            sites: Vec::new(),
            loose: Vec::new(),
            creeps: Vec::new(),
            next_key: 1,
            snap: Snapshot::default(),
            buffer: IntentBuffer::default(),
            movement: Movement::default(),
            spawner: Spawner::default(),
            opening: Opening::default(),
            mining: Mining::default(),
            hauling: Hauling::default(),
            upgrading: Upgrading::default(),
            placement: Placement::default(),
            cfg: OPENING_MMO,
            mining_cfg: MINING_MMO,
            link_cfg: LINK_MMO,
            repair_cfg: REPAIR_MMO,
            hauling_cfg: HAULING_MMO,
            upgrading_cfg: UPGRADING_MMO,
            stored: 0,
            stored_energy: 0,
            empty_takes: 0,
            hauler_takes: 0,
            hauler_taken: 0,
            repairs: 0,
            repaired: 0,
            repair_energy: 0,
            repair_noops: 0,
            containers_lost: 0,
            repair_log: Vec::new(),
            placed: 0,
            placed_containers: 0,
            placed_links: Vec::new(),
            sends: 0,
            refused_sends: 0,
            clamped_sends: 0,
            received: 0,
            link_takes: 0,
            senders: Vec::new(),
            dup_sends: 0,
            orders_at: Vec::new(),
            refuse_tiles: Vec::new(),
            refuse_sends_until: 0,
            takes_log: Vec::new(),
            log_takes: false,
            fills_log: Vec::new(),
            start_creeps: Vec::new(),
            start_stores: Vec::new(),
            start_loose: Vec::new(),
            billed: 0,
            refused_sites: 0,
            standard: true,
            drop_sites: false,
            extra_walls: Vec::new(),
            events: Vec::new(),
            failed: 0,
            spawned: Vec::new(),
            worker_spawns: Vec::new(),
            levels: Vec::new(),
            leveled: 0,
            regens: Vec::new(),
            full_at: None,
            struct_rows: Vec::new(),
            obj_rows: Vec::new(),
            at_level_up: LevelUp::Built,
        };
        w.downgrade_at = w.time + controller_downgrade(level).expect("level") + 1;
        let key = w.key();
        w.stores.push(SimStore {
            key,
            kind: StructureType::Spawn,
            at: tile(25, 25),
            energy: spawn_energy,
            capacity: SPAWN_ENERGY_CAPACITY,
            cooldown: 0,
        });
        w.add_extensions(0);
        for at in [tile(8, 30), tile(42, 20)] {
            let key = w.key();
            w.sources.push(SimSource {
                key,
                at,
                energy: SOURCE_ENERGY_CAPACITY,
                regen_at: None,
            });
        }
        w
    }

    fn key(&mut self) -> u128 {
        self.next_key += 1;
        self.next_key
    }

    /// Extensions up to the level's count, with the level's capacity, in rows of ten from y 28,
    /// two tiles apart (the first `from` already exist).
    fn add_extensions(&mut self, from: u32) {
        let rcl = u32::from(self.level);
        let n = StructureType::Extension.controller_structures(rcl);
        for i in from..n {
            let key = self.key();
            let i = u8::try_from(i).expect("small");
            let (x, y) = (15 + 2 * (i % 10), 28 + 2 * (i / 10));
            self.stores.push(SimStore {
                key,
                kind: StructureType::Extension,
                at: tile(x, y),
                energy: 0,
                capacity: extension_energy_capacity(rcl),
                cooldown: 0,
            });
        }
        for s in &mut self.stores {
            if s.kind == StructureType::Extension {
                s.capacity = extension_energy_capacity(rcl);
            }
        }
    }

    fn pos(&self, t: Tile) -> Position {
        Position::new(
            RoomCoordinate::new(t.x()).expect("x"),
            RoomCoordinate::new(t.y()).expect("y"),
            self.room,
        )
    }

    fn room_energy(&self) -> RoomEnergy {
        let stores = || self.stores.iter().filter(|s| s.spawn_energy());
        RoomEnergy {
            available: stores().map(|s| s.energy).sum(),
            capacity: stores().map(|s| s.capacity).sum(),
        }
    }

    fn blocked(&self, t: Tile) -> bool {
        self.raw[usize::from(t.y()) * 50 + usize::from(t.x())] & (Terrain::Wall as u8) != 0
            || self.stores.iter().any(|s| s.at == t)
            || self.sources.iter().any(|s| s.at == t)
            || self.controller_at == t
            || self.extra_walls.contains(&t)
            || self.sites.iter().any(|s| s.at == t && s.kind.is_obstacle())
    }

    fn footprints(&self) -> Vec<(Tile, Footprint)> {
        let mut out: Vec<(Tile, Footprint)> = self
            .stores
            .iter()
            .map(|s| (s.at, Footprint::Blocked))
            .collect();
        out.push((self.controller_at, Footprint::Blocked));
        out.extend(self.extra_walls.iter().map(|&t| (t, Footprint::Blocked)));
        out.extend(
            self.sites
                .iter()
                .filter(|s| s.kind.is_obstacle())
                .map(|s| (s.at, Footprint::Blocked)),
        );
        out
    }

    fn take_snapshot(&mut self) {
        let time = self.time;
        self.snap
            .begin(TickContext::from_raw(time, 20, 500.0, 10_000));
        for (i, c) in self.creeps.iter().enumerate() {
            let row = CreepRow {
                pos: self.pos(c.at),
                my: true,
                spawning: c.spawning > 0,
                fatigue: c.fatigue,
            };
            let id = c.identity;
            self.snap
                .creeps
                .push_named(ObjectKey(c.key), row, || Some(id))
                .expect("creep");
            if c.spawning == 0 {
                self.snap.creeps.set_store(
                    i,
                    CreepStore {
                        energy: c.energy,
                        capacity: c.capacity(),
                    },
                );
            }
        }
        let energy = self.room_energy();
        let row = self
            .snap
            .rooms
            .push(self.room, Controller::Mine { level: self.level });
        self.snap.rooms.set_energy(row, energy);
        self.snap.rooms.set_controller_state(
            row,
            ControllerState {
                progress: self.progress,
                downgrade_ticks: self.downgrade_at.saturating_sub(time),
                upgrade_blocked: false,
            },
        );
        self.struct_rows.clear();
        let cpos = self.pos(self.controller_at);
        self.snap
            .structures
            .push(ObjectKey(1), || {
                Ok(StructureStatic {
                    kind: StructureType::Controller,
                    pos: cpos,
                })
            })
            .expect("controller");
        self.struct_rows.push(StructRow::Controller);
        let not_full = energy.available < energy.capacity;
        for (i, s) in self.stores.iter().enumerate() {
            let (kind, pos) = (s.kind, self.pos(s.at));
            let h = self
                .snap
                .structures
                .push(ObjectKey(s.key), || Ok(StructureStatic { kind, pos }))
                .expect("structure");
            let row = self.snap.structures.len() - 1;
            if not_full && s.spawn_energy() {
                self.snap
                    .structures
                    .set_free_energy(row, s.capacity - s.energy);
            }
            if kind == StructureType::Spawn {
                let busy = self.creeps.iter().any(|c| c.spawning > 0);
                self.snap.spawns.push(h, row as u32, busy);
            }
            if kind == StructureType::Link {
                self.snap.links.push(h, row as u32, s.energy, s.cooldown);
            }
            self.struct_rows.push(StructRow::Store(i));
        }
        self.obj_rows.clear();
        let room = self.room;
        let push = |snap: &mut Snapshot, key: u128, kind, at: Position, amount, total, hits| {
            snap.objects
                .push(
                    ObjectKey(key),
                    || Ok(ObjectStatic { kind, pos: at }),
                    ObjectRow {
                        amount,
                        total,
                        hits,
                    },
                )
                .expect("object");
        };
        for (i, s) in self.sources.iter().enumerate() {
            push(
                &mut self.snap,
                s.key,
                ObjectKind::Source,
                place(room, s.at),
                s.energy,
                SOURCE_ENERGY_CAPACITY,
                0,
            );
            self.obj_rows.push(ObjRow::Source(i));
        }
        for (i, s) in self.sites.iter().enumerate() {
            push(
                &mut self.snap,
                s.key,
                ObjectKind::Site(s.kind),
                place(room, s.at),
                s.progress,
                s.total,
                0,
            );
            self.obj_rows.push(ObjRow::Site(i));
        }
        for (i, l) in self.loose.iter().enumerate() {
            let (key, kind, at, amount, hits) = match *l {
                Loose::Pile { key, at, amount } => (key, ObjectKind::Energy, at, amount, 0),
                Loose::Tomb {
                    key, at, energy, ..
                } => (key, ObjectKind::Tombstone, at, energy, 0),
                Loose::Container {
                    key,
                    at,
                    energy,
                    hits,
                    ..
                } => (key, ObjectKind::Container, at, energy, hits),
            };
            push(&mut self.snap, key, kind, place(room, at), amount, 0, hits);
            self.obj_rows.push(ObjRow::Loose(i));
        }
        self.snap.finish();
    }

    /// One tick: the snapshot, the bot's decide and act steps, then the engine model.
    fn step(&mut self) {
        self.take_snapshot();
        let mcfg = MOVEMENT_MMO;
        self.movement.begin(&self.snap, &mcfg);
        self.movement.set_cache(&mcfg, &CACHE_MMO);
        let mut rooms = Vec::new();
        self.movement.first_reads(&mcfg, &mut rooms);
        let statics: Vec<Tile> = self.sources.iter().map(|s| s.at).collect();
        for &room in &rooms {
            assert!(self.movement.set_terrain(room, &self.raw));
            self.movement.set_statics(room, statics.clone());
            self.movement.set_objects(room, self.footprints());
        }
        rooms.clear();
        self.movement.rescans(&mcfg, &mut rooms);
        for &room in &rooms {
            self.movement.set_objects(room, self.footprints());
        }
        self.buffer.begin(&self.snap);
        self.spawner.begin(&self.snap);
        self.opening.begin(&self.snap);
        self.mining.begin(&self.snap);
        self.hauling.begin(&self.snap);
        self.upgrading.begin(&self.snap);
        let mut commit = Commit::default();
        let cfg = self.cfg;
        let mining_cfg = self.mining_cfg;
        let link_cfg = self.link_cfg;
        let hauling_cfg = self.hauling_cfg;
        let upgrading_cfg = self.upgrading_cfg;
        let controller_container = (hauling_cfg.enabled && hauling_cfg.place_container)
            .then_some(hauling_cfg.container_level);
        // The Critical `mining` job, then `hauling`, then `opening` (rustedscreep-wasm's order).
        for unit in 0..self.mining.room_count() {
            let room = self.mining.room_name(unit);
            let cap = room.and_then(|room| self.opening.cap(room));
            let refused = room.map_or(Vec::new(), |room| {
                self.placement.refused_tiles(room).to_vec()
            });
            self.mining
                .plan_room(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.spawner,
                    &mut self.buffer,
                    &mining_cfg,
                    &link_cfg,
                    &mcfg,
                    cap,
                    &refused,
                    controller_container,
                    &mut commit,
                )
                .expect("mining");
        }
        for unit in 0..self.hauling.room_count() {
            let room = self.hauling.room_name(unit).expect("room");
            let cap = self.opening.cap(room);
            let mined = self.mining.mined(room).to_vec();
            self.hauling
                .plan_room(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.spawner,
                    &mut self.buffer,
                    &hauling_cfg,
                    &mcfg,
                    self.mining.plan(room),
                    &mined,
                    cap,
                    &mut commit,
                )
                .expect("hauling");
        }
        // The Critical `upgrading` job (P3.4), for the rooms with a controller store or upgraders.
        {
            let mining = &self.mining;
            self.upgrading.select(|room| {
                mining.plan(room).is_some_and(|p| p.controller_store)
                    || !mining.receivers(room).is_empty()
            });
        }
        for unit in 0..self.upgrading.unit_count() {
            let room = self.upgrading.unit_room(unit).expect("room");
            let cap = self.opening.cap(room);
            let receivers = self.mining.receivers(room).to_vec();
            self.upgrading
                .plan_room(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.spawner,
                    &upgrading_cfg,
                    &mcfg,
                    self.mining.plan(room),
                    &receivers,
                    self.hauling.plan(room),
                    Crew {
                        miners: self.mining.stats(room).map_or(0, |s| s.miners),
                        haulers: self.hauling.stats(room).map_or(0, |s| s.haulers),
                    },
                    self.opening.plan(room),
                    cap,
                    &mut commit,
                )
                .expect("upgrading");
        }
        for unit in 0..self.opening.room_count() {
            let room = self.opening.room_name(unit).expect("room");
            let mined = self.mining.mined(room).to_vec();
            let tiles = self.mining.mine_tiles(room).to_vec();
            let receivers = self.mining.receivers(room).to_vec();
            let haul = self.hauling.view(room);
            let upgrade = self.upgrading.view(room);
            self.opening
                .plan_room(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.spawner,
                    &mut self.buffer,
                    &cfg,
                    &self.repair_cfg,
                    &mcfg,
                    &mined,
                    &tiles,
                    &receivers,
                    &haul,
                    &upgrade,
                    &mut commit,
                )
                .expect("plan");
        }
        for unit in 0..self.spawner.close(&self.snap) {
            self.spawner
                .plan_room(&self.snap, unit, &mut commit)
                .expect("spawn plan");
        }
        for unit in 0..self.opening.room_count() * u32::from(self.standard) {
            self.opening
                .work_room(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.buffer,
                    &mcfg,
                    &mut commit,
                )
                .expect("work");
        }
        // The Standard `hauling work` job (P3.3).
        for unit in 0..self.hauling.work_count() * u32::from(self.standard) {
            self.hauling
                .work_room(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.buffer,
                    &mcfg,
                    &mut commit,
                )
                .expect("hauling work");
        }
        // The Standard `upgrading work` job (P3.4).
        for unit in 0..self.upgrading.work_count() * u32::from(self.standard) {
            let room = self.upgrading.work_room_name(unit).expect("room");
            let taken = self.opening.taken(room).to_vec();
            self.upgrading
                .work_room(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.buffer,
                    &mcfg,
                    &taken,
                    &mut commit,
                )
                .expect("upgrading work");
        }
        // The Standard `links` job (P3.2).
        for unit in 0..self.mining.send_count() * u32::from(self.standard) {
            self.mining
                .send_room(unit, &link_cfg, &mut commit)
                .expect("links");
        }
        // The Optional placement job (ADR-020, ADR-023, ADR-024).
        self.placement.begin(&self.snap);
        if cfg.place_extensions
            || mining_cfg.place_containers
            || link_cfg.place_links
            || controller_container.is_some()
        {
            for unit in 0..self.placement.room_count() {
                let room = self.placement.room_name(unit).expect("room");
                let containers = self.mining.container_needs(room).to_vec();
                let links = self.mining.link_needs(room).to_vec();
                self.placement
                    .plan_room(
                        unit,
                        &self.snap,
                        &mut self.movement,
                        &mcfg,
                        cfg.place_extensions,
                        &containers,
                        &links,
                        PlanView::Off,
                        &PLANNER_MMO,
                        &mut commit,
                    )
                    .expect("placement");
            }
        }
        let caps = ShoveCaps::from_cpu_left([100.0; 3]);
        self.movement
            .resolve(&self.snap, &mut self.buffer, &mcfg, caps, &mut commit);
        let time = self.time;
        self.events
            .extend(self.opening.drain_events().map(|(_, e)| (time, e)));
        // The act step: spawn orders first, then the creep intents.
        for i in 0..self.spawner.orders().len() {
            let order = self.spawner.orders()[i];
            let outcome = self.spawn(order.body, order.identity);
            self.spawner.record(i, outcome, &SPAWN_MMO);
        }
        for i in 0..self.placement.orders().len() {
            let order = self.placement.orders()[i];
            let outcome = self.create_site(order.tile, order.kind);
            self.placement.record(i, outcome);
        }
        let start: Vec<(u32, u32)> = self.stores.iter().map(|s| (s.energy, s.cooldown)).collect();
        let mut dest: Vec<Option<Tile>> = vec![None; self.creeps.len()];
        let mut upgraded = false;
        let mut log = ActLog::default();
        let mut buffer = std::mem::take(&mut self.buffer);
        // The game side accepts the plan; the processor then runs each creep's intents in its own
        // order (`creepActions`, V45: transfer before harvest), creep by creep.
        let mut issued = Vec::new();
        buffer.issue(&mut log, |issue| {
            issued.push((issue.actor as usize, issue.target, issue.intent));
            Some(Outcome::Ok)
        });
        issued.sort_by_key(|&(actor, _, intent)| (actor, intent.action() as u8));
        self.start_creeps = self.creeps.iter().map(|c| c.energy).collect();
        self.start_stores = self.stores.iter().map(|s| s.energy).collect();
        self.start_loose = self
            .loose
            .iter()
            .map(|l| match l {
                Loose::Pile { amount, .. } => *amount,
                Loose::Tomb { energy, .. } | Loose::Container { energy, .. } => *energy,
            })
            .collect();
        if self.log_takes {
            for &(actor, target, intent) in &issued {
                if let (CreepIntent::Transfer { .. }, Some(t @ TargetRow::Structure(_))) =
                    (intent, target)
                {
                    self.fills_log
                        .push((self.time, self.creeps[actor].identity.role, t));
                }
                let asked = match intent {
                    CreepIntent::Withdraw { amount, .. }
                    | CreepIntent::WithdrawObject { amount, .. } => amount,
                    CreepIntent::Pickup { .. } => None,
                    _ => continue,
                };
                let Some(t) = target else {
                    continue;
                };
                let held = match t {
                    TargetRow::Structure(row) => match self.struct_rows[row as usize] {
                        StructRow::Store(i) => self.stores[i].energy,
                        StructRow::Controller => 0,
                    },
                    TargetRow::Object(row) => match self.obj_rows[row as usize] {
                        ObjRow::Loose(i) => match &self.loose[i] {
                            Loose::Pile { amount, .. } => *amount,
                            Loose::Tomb { energy, .. } | Loose::Container { energy, .. } => *energy,
                        },
                        _ => 0,
                    },
                    TargetRow::Creep(_) => 0,
                };
                let c = &self.creeps[actor];
                self.takes_log.push((
                    self.time,
                    c.identity.role,
                    t,
                    asked,
                    c.capacity() - c.energy,
                    held,
                ));
            }
        }
        for (actor, target, intent) in issued {
            let outcome = self.apply(actor, target, intent, &mut dest, &mut upgraded);
            if !outcome.recorded() {
                self.failed += 1;
            }
        }
        self.buffer = buffer;
        self.billed += u64::from(log.issues().billed);
        // The link sends, against the links as they were at the tick's start; of two calls from one
        // link the engine keeps the last (V34, V216).
        let orders = self.mining.link_orders().to_vec();
        for (i, &order) in orders.iter().enumerate() {
            if orders[i + 1..].iter().any(|o| o.from_row == order.from_row) {
                self.dup_sends += 1;
                continue;
            }
            let at = match self.struct_rows[order.from_row as usize] {
                StructRow::Store(k) => self.stores[k].at,
                StructRow::Controller => self.controller_at,
            };
            self.orders_at.push((self.time, at));
            let outcome = if self.time < self.refuse_sends_until {
                Outcome::Failed(ErrorCode::NotOwner)
            } else {
                self.send(order, &start)
            };
            if outcome == Outcome::Ok {
                self.billed += 1;
            }
            if !outcome.recorded() {
                self.refused_sends += 1;
            }
            self.mining.record_send(i, outcome, &link_cfg);
        }
        self.movement.moves_refused(&self.snap, Vec::new(), &mcfg);
        self.resolve_moves(&dest);
        self.end_of_tick(upgraded);
        self.time += 1;
    }

    /// `createConstructionSite` (V200): refused on a wall, the edge, an occupied tile or past the
    /// level's count; the site appears at the end of the tick.
    fn create_site(&mut self, at: Tile, kind: StructureType) -> Outcome {
        let wall =
            self.raw[usize::from(at.y()) * 50 + usize::from(at.x())] & (Terrain::Wall as u8) != 0;
        let container_at = |t: Tile| {
            self.loose
                .iter()
                .any(|l| matches!(*l, Loose::Container { at, .. } if at == t))
        };
        let taken = self.blocked(at)
            || self.sites.iter().any(|s| s.at == at)
            || container_at(at)
            || self.refuse_tiles.contains(&at);
        let containers = self
            .loose
            .iter()
            .filter(|l| matches!(l, Loose::Container { .. }))
            .count();
        let have = self.stores.iter().filter(|s| s.kind == kind).count()
            + self.sites.iter().filter(|s| s.kind == kind).count()
            + if kind == StructureType::Container {
                containers
            } else {
                0
            };
        let allowed = kind.controller_structures(u32::from(self.level)) as usize;
        if wall || taken || at.is_edge() || have >= allowed {
            self.refused_sites += 1;
            return Outcome::Failed(ErrorCode::InvalidTarget);
        }
        if kind == StructureType::Container {
            self.placed_containers += 1;
        } else if kind == StructureType::Link {
            self.placed_links.push(at);
        } else {
            self.placed += 1;
        }
        if self.drop_sites {
            return Outcome::Ok;
        }
        let key = self.key();
        self.sites.push(SimSite {
            key,
            kind,
            at,
            progress: 0,
            total: kind.construction_cost().expect("cost"),
        });
        Outcome::Ok
    }

    /// `spawnCreep` (V171, V173): one at a time, paid from the spawn first, then the extensions.
    fn spawn(&mut self, body: Body, identity: Identity) -> Outcome {
        if self.creeps.iter().any(|c| c.spawning > 0) {
            return Outcome::Failed(ErrorCode::Busy);
        }
        let cost = body.cost();
        if self.room_energy().available < cost {
            return Outcome::Failed(ErrorCode::NotEnough);
        }
        let mut left = cost;
        for s in self.stores.iter_mut().filter(|s| s.spawn_energy()) {
            let take = s.energy.min(left);
            s.energy -= take;
            left -= take;
        }
        let key = self.key();
        let at = self.stores[0].at;
        self.creeps.push(SimCreep {
            key,
            identity,
            at,
            body,
            energy: 0,
            fatigue: 0,
            spawning: body.spawn_time(),
            born: 0,
        });
        self.spawned.push((self.time, body));
        if identity.role == Role::Worker {
            let (budget, plan_body) = self
                .opening
                .room_states()
                .next()
                .map_or((None, None), |(_, plan, _, stats)| {
                    (stats.request.map(|r| r.0), plan.and_then(|p| p.body))
                });
            self.worker_spawns
                .push((self.time, body, budget, plan_body));
        }
        Outcome::Ok
    }

    fn apply(
        &mut self,
        actor: usize,
        target: Option<TargetRow>,
        intent: CreepIntent,
        dest: &mut [Option<Tile>],
        upgraded: &mut bool,
    ) -> Outcome {
        let c = self.creeps[actor].clone();
        let near = |a: Tile, b: Tile, r: u8| a.range(b) <= r;
        let work = count(&c.body, Part::Work);
        let fail = Outcome::Failed(ErrorCode::InvalidTarget);
        match (intent, target) {
            (CreepIntent::Move { direction }, None) => {
                dest[actor] = c.at.step(direction);
                Outcome::Ok
            }
            (CreepIntent::Harvest { .. }, Some(TargetRow::Object(row))) => {
                let ObjRow::Source(i) = self.obj_rows[row as usize] else {
                    return fail;
                };
                let s = &mut self.sources[i];
                if !near(c.at, s.at, 1) {
                    return Outcome::Failed(ErrorCode::NotInRange);
                }
                // The game side saw the energy of the tick's start; the processor re-checks it and
                // does nothing when an earlier harvest this tick emptied the source (billed).
                if s.energy == 0 {
                    return Outcome::Ok;
                }
                let amount = s.energy.min(HARVEST_POWER * work);
                s.energy -= amount;
                let cap = c.capacity();
                let total = c.energy + amount;
                self.creeps[actor].energy = total.min(cap);
                if total > cap {
                    self.drop_at(c.at, total - cap);
                }
                Outcome::Ok
            }
            (
                CreepIntent::Transfer {
                    target: Target::Structure(_),
                    ..
                },
                Some(TargetRow::Structure(row)),
            ) => {
                let StructRow::Store(i) = self.struct_rows[row as usize] else {
                    return fail;
                };
                let s = &mut self.stores[i];
                if !near(c.at, s.at, 1) {
                    return Outcome::Failed(ErrorCode::NotInRange);
                }
                let free = s.capacity - s.energy;
                if free == 0 {
                    return Outcome::Failed(ErrorCode::Full);
                }
                let amount = c.energy.min(free);
                s.energy += amount;
                self.creeps[actor].energy -= amount;
                Outcome::Ok
            }
            (
                CreepIntent::Transfer {
                    target: Target::Object(_),
                    ..
                },
                Some(TargetRow::Object(row)),
            ) => {
                // Into a container (P3.3): the processor clamps to its room (V191).
                let ObjRow::Loose(i) = self.obj_rows[row as usize] else {
                    return fail;
                };
                let Loose::Container { at, energy, .. } = &mut self.loose[i] else {
                    return fail;
                };
                if !near(c.at, *at, 1) {
                    return Outcome::Failed(ErrorCode::NotInRange);
                }
                let free = CONTAINER_CAPACITY - *energy;
                if free == 0 {
                    return Outcome::Failed(ErrorCode::Full);
                }
                let amount = c.energy.min(free);
                *energy += amount;
                self.creeps[actor].energy -= amount;
                self.stored += 1;
                self.stored_energy += u64::from(amount);
                Outcome::Ok
            }
            (CreepIntent::Withdraw { amount: asked, .. }, Some(TargetRow::Structure(row))) => {
                let StructRow::Store(i) = self.struct_rows[row as usize] else {
                    return fail;
                };
                let s = &mut self.stores[i];
                if !near(c.at, s.at, 1) {
                    return Outcome::Failed(ErrorCode::NotInRange);
                }
                let free = c.capacity() - c.energy;
                if free == 0 {
                    return Outcome::Failed(ErrorCode::Full);
                }
                if s.energy == 0 {
                    return Outcome::Failed(ErrorCode::NotEnough);
                }
                // An explicit amount above the free space is refused, or above the stock at the
                // tick's start (V192, V220).
                if asked.is_some_and(|a| a > free) {
                    return Outcome::Failed(ErrorCode::Full);
                }
                if asked.is_some_and(|a| a > self.start_stores[i]) {
                    return Outcome::Failed(ErrorCode::NotEnough);
                }
                let amount = s.energy.min(asked.unwrap_or(free));
                s.energy -= amount;
                self.creeps[actor].energy += amount;
                self.link_takes += 1;
                Outcome::Ok
            }
            (CreepIntent::UpgradeController { .. }, Some(TargetRow::Structure(_))) => {
                // The game side refuses a creep that held no energy at the tick's start (V187).
                if self.start_creeps[actor] == 0 {
                    return Outcome::Failed(ErrorCode::NotEnough);
                }
                if !near(c.at, self.controller_at, 3) || c.energy == 0 {
                    return Outcome::Failed(ErrorCode::NotInRange);
                }
                let amount = c.energy.min(UPGRADE_CONTROLLER_POWER * work);
                self.creeps[actor].energy -= amount;
                self.progress += amount;
                *upgraded = true;
                Outcome::Ok
            }
            (CreepIntent::Build { .. }, Some(TargetRow::Object(row))) => {
                let ObjRow::Site(i) = self.obj_rows[row as usize] else {
                    return fail;
                };
                let site_at = self.sites[i].at;
                if !near(c.at, site_at, 3) || c.energy == 0 {
                    return Outcome::Failed(ErrorCode::NotInRange);
                }
                let blocked = self.sites[i].kind.is_obstacle()
                    && self
                        .creeps
                        .iter()
                        .any(|o| o.spawning == 0 && o.at == site_at);
                if blocked {
                    // The game side refuses it (ERR_INVALID_TARGET).
                    return fail;
                }
                let s = &mut self.sites[i];
                let amount = c.energy.min(BUILD_POWER * work).min(s.total - s.progress);
                s.progress += amount;
                self.creeps[actor].energy -= amount;
                Outcome::Ok
            }
            (CreepIntent::Pickup { .. }, Some(TargetRow::Object(row)))
            | (CreepIntent::WithdrawObject { .. }, Some(TargetRow::Object(row))) => {
                let asked = match intent {
                    CreepIntent::WithdrawObject { amount, .. } => amount,
                    _ => None,
                };
                let ObjRow::Loose(i) = self.obj_rows[row as usize] else {
                    return fail;
                };
                let free = c.capacity() - c.energy;
                let (at, have) = match &mut self.loose[i] {
                    Loose::Pile { at, amount, .. } => (*at, amount),
                    Loose::Tomb { at, energy, .. } | Loose::Container { at, energy, .. } => {
                        (*at, energy)
                    }
                };
                if !near(c.at, at, 1) || free == 0 {
                    return Outcome::Failed(ErrorCode::NotInRange);
                }
                // An explicit amount above the free space is refused, or above the stock at the
                // tick's start (V192).
                if asked.is_some_and(|a| a > free) {
                    return Outcome::Failed(ErrorCode::Full);
                }
                if asked.is_some_and(|a| a > self.start_loose[i]) {
                    return Outcome::Failed(ErrorCode::NotEnough);
                }
                // Emptied by an earlier take this tick: the processor clamps to what is left.
                let amount = (*have).min(asked.unwrap_or(free));
                *have -= amount;
                self.creeps[actor].energy += amount;
                if amount == 0 {
                    self.empty_takes += 1;
                }
                if c.identity.role == Role::Hauler {
                    self.hauler_takes += 1;
                    self.hauler_taken += u64::from(amount);
                }
                Outcome::Ok
            }
            (CreepIntent::RepairObject { .. }, Some(TargetRow::Object(row))) => {
                let ObjRow::Loose(i) = self.obj_rows[row as usize] else {
                    return fail;
                };
                let Loose::Container { at, hits, .. } = &mut self.loose[i] else {
                    return fail;
                };
                // The game side (V221): energy, then range.
                if c.energy == 0 {
                    return Outcome::Failed(ErrorCode::NotEnough);
                }
                if !near(c.at, *at, 3) {
                    return Outcome::Failed(ErrorCode::NotInRange);
                }
                // The processor (V222): billed for nothing at full hits.
                if *hits >= CONTAINER_HITS {
                    self.repair_noops += 1;
                    return Outcome::Ok;
                }
                let per_energy = hits_per_energy();
                let effect = (REPAIR_POWER * work)
                    .min(c.energy * per_energy)
                    .min(CONTAINER_HITS - *hits);
                let cost = c.energy.min(effect.div_ceil(per_energy));
                *hits += effect;
                let at = *at;
                self.creeps[actor].energy -= cost;
                self.repair_log.push(at);
                self.repairs += 1;
                self.repaired += u64::from(effect);
                self.repair_energy += u64::from(cost);
                Outcome::Ok
            }
            other => panic!("the opening queued {other:?}"),
        }
    }

    /// `transferEnergy(target, amount)`: the game side's checks on the tick's start (`start`, each
    /// store's energy and cooldown; V216), then links/transfer.js (V217).
    fn send(&mut self, order: LinkOrder, start: &[(u32, u32)]) -> Outcome {
        let row = |r: u32| match self.struct_rows[r as usize] {
            StructRow::Store(i) => Some(i),
            StructRow::Controller => None,
        };
        let (Some(from), Some(to)) = (row(order.from_row), row(order.to_row)) else {
            return Outcome::Failed(ErrorCode::InvalidTarget);
        };
        let link = |i: usize| self.stores[i].kind == StructureType::Link;
        if from == to || !link(from) || !link(to) {
            return Outcome::Failed(ErrorCode::InvalidTarget);
        }
        let (energy, cooldown) = start[from];
        let (target, _) = start[to];
        if cooldown > 0 {
            return Outcome::Failed(ErrorCode::Tired);
        }
        // `checkStructureAgainstController` (V218): the level's count of links nearest the
        // controller stay active, ties in the engine's object order (here the world's).
        let allowed = StructureType::Link.controller_structures(u32::from(self.level)) as usize;
        let d = |i: usize| self.stores[i].at.range(self.controller_at);
        let ahead = (0..self.stores.len())
            .filter(|&i| i != from && link(i) && (d(i) < d(from) || (d(i) == d(from) && i < from)))
            .count();
        if ahead >= allowed {
            return Outcome::Failed(ErrorCode::RclNotEnough);
        }
        if energy == 0 || energy < order.amount {
            return Outcome::Failed(ErrorCode::NotEnough);
        }
        if target + order.amount > LINK_CAPACITY {
            return Outcome::Failed(ErrorCode::Full);
        }
        // The processor: clamp to the receiver's room now, the loss off what arrives.
        let room = LINK_CAPACITY - self.stores[to].energy;
        if room == 0 {
            return Outcome::Ok;
        }
        assert!(
            order.amount >= self.link_cfg.send_min,
            "a send below send_min"
        );
        let amount = order.amount.min(room).min(self.stores[from].energy);
        if amount < order.amount {
            self.clamped_sends += 1;
        }
        let lost = link_loss(amount);
        self.stores[to].energy += amount - lost;
        self.stores[from].energy -= amount;
        let dist = u32::from(self.stores[from].at.range(self.stores[to].at));
        self.stores[from].cooldown += LINK_COOLDOWN * dist;
        self.sends += 1;
        self.received += amount - lost;
        let at = self.stores[from].at;
        if !self.senders.contains(&at) {
            self.senders.push(at);
        }
        Outcome::Ok
    }

    fn drop_at(&mut self, at: Tile, amount: u32) {
        let mut amount = amount;
        for l in &mut self.loose {
            if let Loose::Container {
                at: a, energy: e, ..
            } = l
                && *a == at
            {
                let into = amount.min(CONTAINER_CAPACITY - *e);
                *e += into;
                amount -= into;
            }
        }
        if amount == 0 {
            return;
        }
        for l in &mut self.loose {
            if let Loose::Pile {
                at: a, amount: n, ..
            } = l
                && *a == at
            {
                *n += amount;
                return;
            }
        }
        let key = self.key();
        self.loose.push(Loose::Pile { key, at, amount });
    }

    /// Moves (V51, V52) and their fatigue (V54: `MOVE_COST_SWAMP` on a swamp tile, else
    /// `MOVE_COST_PLAIN`; no roads; V55).
    fn resolve_moves(&mut self, dest: &[Option<Tile>]) {
        let mut dest = dest.to_vec();
        loop {
            let mut changed = false;
            for i in 0..self.creeps.len() {
                let Some(d) = dest[i] else { continue };
                let two = (0..self.creeps.len()).any(|j| j != i && dest[j] == Some(d));
                assert!(!two, "two moves into {d:?}");
                let stays = (0..self.creeps.len()).any(|j| {
                    j != i
                        && self.creeps[j].spawning == 0
                        && self.creeps[j].at == d
                        && dest[j].is_none()
                });
                if self.blocked(d) || stays || d.is_edge() {
                    dest[i] = None;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let raw = &self.raw;
        for (c, d) in self.creeps.iter_mut().zip(&dest) {
            if let Some(d) = *d {
                c.at = d;
                let loaded = c
                    .energy
                    .div_ceil(CARRY_CAPACITY)
                    .min(count(&c.body, Part::Carry));
                let weight = count(&c.body, Part::Work) + loaded;
                let swamp =
                    raw[usize::from(d.y()) * 50 + usize::from(d.x())] & (Terrain::Swamp as u8) != 0;
                let rate = if swamp {
                    MOVE_COST_SWAMP
                } else {
                    MOVE_COST_PLAIN
                };
                c.fatigue += rate * weight;
            }
            c.fatigue = c
                .fatigue
                .saturating_sub(MOVE_POWER * count(&c.body, Part::Move));
        }
    }

    fn end_of_tick(&mut self, upgraded: bool) {
        let now = self.time;
        // The controller.
        if upgraded {
            let full = now + controller_downgrade(self.level).expect("level") + 1;
            self.downgrade_at = (self.downgrade_at + CONTROLLER_DOWNGRADE_RESTORE + 1).min(full);
        }
        assert!(now < self.downgrade_at - 1, "the controller downgraded");
        if let Some(total) = controller_levels(u32::from(self.level))
            && self.progress >= total
        {
            self.progress -= total;
            self.leveled += u64::from(total);
            self.level += 1;
            self.levels.push((now, self.level));
            let have = u32::try_from(
                self.stores
                    .iter()
                    .filter(|s| s.kind == StructureType::Extension)
                    .count(),
            )
            .expect("small");
            if self.at_level_up == LevelUp::Sites {
                let rcl = u32::from(self.level);
                let n = StructureType::Extension.controller_structures(rcl);
                let placed = have + u32::try_from(self.sites.len()).expect("small");
                for i in placed..n {
                    let key = self.key();
                    let x = 15 + 2 * u8::try_from(i).expect("small");
                    self.sites.push(SimSite {
                        key,
                        kind: StructureType::Extension,
                        at: tile(x, 32),
                        progress: 0,
                        total: StructureType::Extension.construction_cost().expect("cost"),
                    });
                }
            } else if self.at_level_up == LevelUp::Built {
                self.add_extensions(have);
            }
        }
        // Sites finished.
        let done: Vec<SimSite> = self
            .sites
            .iter()
            .filter(|s| s.progress >= s.total)
            .cloned()
            .collect();
        self.sites.retain(|s| s.progress < s.total);
        for s in done {
            if s.kind == StructureType::Container {
                // A new object with its own id (the snapshot reads an object's kind once per id).
                let key = self.key();
                self.loose.push(Loose::Container {
                    key,
                    at: s.at,
                    energy: 0,
                    hits: CONTAINER_HITS,
                    decay_at: now + CONTAINER_DECAY_TIME,
                });
                continue;
            }
            let rcl = u32::from(self.level);
            let capacity = match s.kind {
                StructureType::Link => LINK_CAPACITY,
                _ => extension_energy_capacity(rcl),
            };
            self.stores.push(SimStore {
                key: s.key,
                kind: s.kind,
                at: s.at,
                energy: 0,
                capacity,
                cooldown: 0,
            });
        }
        // Links' cooldowns (links/tick.js, after the intents).
        for s in &mut self.stores {
            s.cooldown = s.cooldown.saturating_sub(1);
        }
        // Sources (sources/tick.js).
        for (i, s) in self.sources.iter_mut().enumerate() {
            if s.energy < SOURCE_ENERGY_CAPACITY && s.regen_at.is_none() {
                s.regen_at = Some(now + ENERGY_REGEN_TIME);
            }
            if s.regen_at.is_some_and(|at| now >= at - 1) {
                self.regens.push((now, i, s.energy));
                s.energy = SOURCE_ENERGY_CAPACITY;
                s.regen_at = None;
            }
        }
        // The spawn's regeneration (V174).
        let room = self.room_energy().available;
        if let Some(spawn) = self.stores.first_mut()
            && room < SPAWN_ENERGY_CAPACITY
            && spawn.energy < SPAWN_ENERGY_CAPACITY
        {
            spawn.energy += 1;
        }
        // Spawning, then ageing.
        let spawn_at = self.stores[0].at;
        let mut placed = Vec::new();
        for (i, c) in self.creeps.iter_mut().enumerate() {
            if c.spawning > 0 {
                c.spawning -= 1;
                if c.spawning == 0 {
                    c.born = now;
                    placed.push(i);
                }
            }
        }
        for i in placed {
            let open = spawn_at.neighbors().map(|(_, t)| t).find(|&t| {
                !self.blocked(t) && !self.creeps.iter().any(|c| c.spawning == 0 && c.at == t)
            });
            match open {
                Some(t) => self.creeps[i].at = t,
                None => self.creeps[i].spawning = 1,
            }
        }
        let mut dead = Vec::new();
        self.creeps.retain(|c| {
            let alive = c.spawning > 0 || now < c.born + CREEP_LIFE_TIME;
            if !alive {
                dead.push(c.clone());
            }
            alive
        });
        for c in dead {
            let key = self.key();
            let parts = u32::try_from(c.body.len()).expect("small");
            self.loose.push(Loose::Tomb {
                key,
                at: c.at,
                energy: c.energy,
                decay_at: now + TOMBSTONE_DECAY_PER_PART * parts,
            });
        }
        // Piles decay; tombstones drop what they hold.
        let mut dropped = Vec::new();
        for l in &mut self.loose {
            match l {
                Loose::Pile { amount, .. } => *amount -= amount.div_ceil(ENERGY_DECAY),
                Loose::Tomb {
                    at,
                    energy,
                    decay_at,
                    ..
                } => {
                    if now >= *decay_at - 1 {
                        dropped.push((*at, *energy));
                        *energy = 0;
                        *decay_at = 0;
                    }
                }
                // containers/tick.js (V214): the owned room's wait after each step.
                Loose::Container {
                    at,
                    energy,
                    hits,
                    decay_at,
                    ..
                } => {
                    if now >= *decay_at - 1 {
                        *hits = hits.saturating_sub(CONTAINER_DECAY);
                        *decay_at = now + CONTAINER_DECAY_TIME_OWNED;
                        if *hits == 0 {
                            dropped.push((*at, *energy));
                            *energy = 0;
                            self.containers_lost += 1;
                        }
                    }
                }
            }
        }
        self.loose.retain(|l| match l {
            Loose::Pile { amount, .. } => *amount > 0,
            Loose::Tomb { decay_at, .. } => *decay_at > 0,
            Loose::Container { hits, .. } => *hits > 0,
        });
        for (at, energy) in dropped {
            if energy > 0 {
                self.drop_at(at, energy);
            }
        }
        let e = self.room_energy();
        if self.full_at.is_none() && e.available == e.capacity && !self.creeps.is_empty() {
            self.full_at = Some(now);
        }
    }

    fn run(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.step();
        }
    }

    /// Every upgrade's progress since the start, across level-ups.
    fn upgraded(&self) -> u64 {
        self.leveled + u64::from(self.progress)
    }

    fn workers(&self) -> usize {
        self.creeps
            .iter()
            .filter(|c| c.identity.role == Role::Worker)
            .count()
    }

    fn miners(&self) -> usize {
        self.creeps
            .iter()
            .filter(|c| c.identity.role == Role::Miner)
            .count()
    }

    fn haulers(&self) -> usize {
        self.creeps
            .iter()
            .filter(|c| c.identity.role == Role::Hauler)
            .count()
    }

    fn upgraders(&self) -> usize {
        self.creeps
            .iter()
            .filter(|c| c.identity.role == Role::Upgrader)
            .count()
    }

    fn event_at(&self, want: impl Fn(&Event) -> bool) -> Option<u32> {
        self.events.iter().find(|(_, e)| want(e)).map(|&(t, _)| t)
    }

    fn want(&self) -> u32 {
        self.opening
            .room_states()
            .next()
            .and_then(|(_, p, _, _)| p.map(|p| p.want))
            .unwrap_or(0)
    }
}

/// The time a zero-creep room needs to field `min_workers`: the spawn regenerates 1 energy a tick
/// from empty (V174) until it holds one repeat of the pattern, and each recovery worker spawns in
/// `CREEP_SPAWN_TIME` per part; a source's regeneration period covers the first workers' first
/// trips. Uses: `BODYPART_COST`, `CREEP_SPAWN_TIME` (through `Body`), `ENERGY_REGEN_TIME`.
fn recovery_bound(cfg: &OpeningConfig) -> u32 {
    let unit = smallest_worker().expect("smallest worker");
    cfg.min_workers * (unit.cost() + unit.spawn_time()) + ENERGY_REGEN_TIME
}

fn assert_recovers(level: u8) {
    let mut w = World::new(level, 0);
    let start = w.time;
    let bound = recovery_bound(&w.cfg);
    w.run(bound + 1);
    let started = w
        .event_at(|e| *e == Event::RecoveryStarted)
        .expect("started");
    assert_eq!(
        started, start,
        "a zero-creep room starts its recovery at once"
    );
    let recovered = w
        .event_at(|e| matches!(e, Event::Recovered { .. }))
        .unwrap_or_else(|| panic!("RCL{level}: not recovered in {bound} ticks: {:?}", w.events));
    assert!(
        recovered - start <= bound,
        "RCL{level}: {} ticks",
        recovered - start
    );
    // The first workers spawn with what the spawn regenerated: the smallest worker each (the
    // solver's body at its cost, the least energy a worker can spawn with).
    let unit = smallest_worker().expect("smallest worker");
    assert!(w.spawned.len() >= w.cfg.min_workers as usize);
    for (_, body) in w.spawned.iter().take(w.cfg.min_workers as usize) {
        assert_eq!(body.cost(), unit.cost(), "RCL{level}: {body:?}");
    }
    assert_eq!(w.failed, 0, "no intent fails in the model");
    assert!(w.event_at(|e| matches!(e, Event::Stalled { .. })).is_none());
}

/// Zero-creep recovery at RCL2 (TEST_PLAN §7, the P2 gate): no creeps, an empty spawn and empty
/// extensions. The room fields `min_workers` within the bound from constants.
#[test]
fn a_zero_creep_room_recovers_at_rcl2() {
    assert_recovers(2);
}

/// Zero-creep recovery at RCL3 (TEST_PLAN §7, the P2 gate).
#[test]
fn a_zero_creep_room_recovers_at_rcl3() {
    assert_recovers(3);
}

/// After a recovery the room keeps going for two creep lifetimes: it fills to its full capacity,
/// it reaches the count its plan wants, workers that die of old age are replaced (by the body the
/// plan solved at the full capacity), their tombstones are emptied, and nothing stalls or fails.
#[test]
fn a_recovered_room_runs_two_lifetimes_without_a_deadlock() {
    let mut w = World::new(3, 0);
    w.run(2 * CREEP_LIFE_TIME + 500);
    assert!(
        w.full_at.is_some(),
        "the room filled its spawns and extensions"
    );
    let want = w.want();
    assert!(
        want >= w.cfg.min_workers && want <= w.cfg.max_workers,
        "want {want}"
    );
    assert!(
        w.workers() >= want as usize - 1,
        "{} of {want}",
        w.workers()
    );
    // Workers requested at the room's capacity get the body the plan solved for it that tick
    // (P3.6), and some of them are larger than the recovery's smallest worker.
    let at_capacity: Vec<_> = w
        .worker_spawns
        .iter()
        .filter(|s| s.2 == Some(EnergyBudget::Capacity))
        .collect();
    assert!(!at_capacity.is_empty(), "workers spawned at capacity");
    for s in &at_capacity {
        assert_eq!(Some(s.1), s.3, "tick {}: the plan's body", s.0);
    }
    assert!(
        at_capacity.iter().any(|s| Some(s.1) != smallest_worker()),
        "a worker at capacity larger than the smallest: {:?}",
        w.spawned
            .iter()
            .map(|(t, b)| (t, b.len()))
            .collect::<Vec<_>>()
    );
    let deaths = w
        .spawned
        .iter()
        .filter(|(t, _)| *t + CREEP_LIFE_TIME < w.time)
        .count();
    assert!(deaths > 0, "some workers died of old age");
    assert_eq!(w.failed, 0);
    assert!(
        w.event_at(|e| matches!(e, Event::Stalled { .. })).is_none(),
        "{:?}",
        w.events
    );
    assert!(w.progress > 0 || w.level > 3);
}

/// A fresh room (RCL1, a full spawn of `SPAWN_ENERGY_START`, no creeps) reaches RCL2 with no manual
/// action: `CONTROLLER_LEVELS[1]` progress within a regeneration period of sources per load the
/// level needs.
#[test]
fn a_fresh_room_reaches_rcl2() {
    let mut w = World::new(1, SPAWN_ENERGY_START);
    let start = w.time;
    let need = controller_levels(1).expect("level 1");
    let bound = ENERGY_REGEN_TIME * need.div_ceil(CARRY_CAPACITY);
    for _ in 0..bound {
        w.step();
        if w.level >= 2 {
            break;
        }
    }
    let (at, level) = *w.levels.first().expect("levelled up");
    assert_eq!(level, 2);
    assert!(at - start <= bound, "{} ticks", at - start);
    assert_eq!(w.failed, 0);
}

/// Building: sites placed at RCL2 are built (spawn energy kept first), and the extensions they
/// make are filled.
#[test]
fn sites_are_built_and_the_extensions_filled() {
    let mut w = World::new(2, SPAWN_ENERGY_CAPACITY);
    // Drop the level's extensions and place them as sites instead.
    w.stores.retain(|s| s.kind == StructureType::Spawn);
    let n = StructureType::Extension.controller_structures(2);
    let total = StructureType::Extension
        .construction_cost()
        .expect("extension cost");
    for i in 0..n {
        let key = w.key();
        let x = 15 + 2 * u8::try_from(i).expect("small");
        w.sites.push(SimSite {
            key,
            kind: StructureType::Extension,
            at: tile(x, 32),
            progress: 0,
            total,
        });
    }
    let limit = 4 * CREEP_LIFE_TIME;
    for _ in 0..limit {
        w.step();
        if w.sites.is_empty() && w.room_energy().available == w.room_energy().capacity {
            break;
        }
    }
    assert!(w.sites.is_empty(), "sites left: {:?}", w.sites);
    let e = w.room_energy();
    assert_eq!(
        e.capacity,
        SPAWN_ENERGY_CAPACITY + n * extension_energy_capacity(2)
    );
    assert_eq!(e.available, e.capacity, "the new extensions are filled");
    assert_eq!(w.failed, 0);
}

/// A room whose capacity it can never fill does not wait for it forever: at RCL3 the workers' body
/// at capacity (800, four repeats) needs every extension full, and one extension walled in leaves
/// 750 reachable. After `wait_ticks` the request falls back once, its bodies are capped at the most
/// the room held (three repeats), new workers do not start the wait over, the walled extension is
/// skipped as unreachable, and the room reaches its count.
#[test]
fn an_unreachable_extension_does_not_stop_spawning() {
    let mut w = World::new(3, 0);
    let at = w
        .stores
        .iter()
        .find(|s| s.kind == StructureType::Extension)
        .expect("extension")
        .at;
    for (_, t) in at.neighbors() {
        w.raw[usize::from(t.y()) * 50 + usize::from(t.x())] = Terrain::Wall as u8;
    }
    w.run(recovery_bound(&w.cfg) + 3 * w.cfg.wait_ticks + CREEP_LIFE_TIME);
    let fallbacks: Vec<_> = w
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::WaitFallback { .. }))
        .collect();
    assert_eq!(fallbacks.len(), 1, "{:?}", w.events);
    let want = w.want();
    assert!(
        w.workers() + 1 >= want as usize,
        "{} of {want}: {:?}",
        w.workers(),
        w.spawned
            .iter()
            .map(|(t, b)| (t, b.len()))
            .collect::<Vec<_>>()
    );
    // Bodies after the fallback cost at most what the room can hold.
    let reachable = w.room_energy().capacity - extension_energy_capacity(3);
    let after = fallbacks[0].0;
    assert!(
        w.spawned
            .iter()
            .filter(|(t, _)| *t > after)
            .all(|(_, b)| b.cost() <= reachable)
    );
    assert_eq!(w.failed, 0);
}

/// A tombstone full of energy in a walled pocket, nearer than the sources: the workers that pick it
/// find no way to it, it goes on the room's unreachable list and nobody waits on it, so the room
/// reaches its count and its controller progresses.
#[test]
fn an_unreachable_tombstone_does_not_trap_the_workers() {
    let mut w = World::new(2, 0);
    let pocket = tile(20, 29);
    for (_, t) in pocket.neighbors() {
        w.raw[usize::from(t.y()) * 50 + usize::from(t.x())] = Terrain::Wall as u8;
    }
    let key = w.key();
    w.loose.push(Loose::Tomb {
        key,
        at: pocket,
        energy: 100_000,
        decay_at: u32::MAX,
    });
    let ticks = recovery_bound(&w.cfg) + CREEP_LIFE_TIME;
    // Worker-ticks spent on the tombstone: a NoPath drops the task at once and the target is
    // retried only every `movement.route_retry_ticks`.
    let mut held = 0u32;
    for _ in 0..ticks {
        w.step();
        let Some(tomb) = w.snap.objects.handle_of(ObjectKey(key)) else {
            continue;
        };
        for c in &w.creeps {
            let h = w.snap.creeps.handle_of(ObjectKey(c.key));
            if h.and_then(|h| w.opening.task(h)) == Some(Task::Take(tomb)) {
                held += 1;
            }
        }
    }
    let retries = ticks / MOVEMENT_MMO.route_retry_ticks + 1;
    assert!(
        held <= 2 * retries,
        "{held} worker-ticks on the unreachable tombstone ({retries} retries)"
    );
    let want = w.want();
    assert!(
        w.workers() + 1 >= want as usize,
        "{} of {want}",
        w.workers()
    );
    assert!(w.progress > 0, "the controller progressed");
    assert_eq!(w.failed, 0);
}

/// With the Standard tier off (a low bucket), builders and upgraders do not work, but they make way:
/// harvesting and filling (Critical) go on, and the room still fills its spawn and extensions, even
/// with one harvesting tile per source.
#[test]
fn critical_work_goes_on_with_the_standard_tier_off() {
    let mut w = World::new(2, 0);
    // One open tile per source, so a worker that stands on it and does not make way stops the
    // harvest.
    let sources: Vec<Tile> = w.sources.iter().map(|s| s.at).collect();
    for src in sources {
        let keep = src
            .step(rustedscreep_consts::Direction::Right)
            .expect("in room");
        for (_, t) in src.neighbors() {
            if t != keep {
                w.raw[usize::from(t.y()) * 50 + usize::from(t.x())] = Terrain::Wall as u8;
            }
        }
    }
    w.run(recovery_bound(&w.cfg) + ENERGY_REGEN_TIME);
    w.standard = false;
    // Empty the room so the fill is visible.
    for st in &mut w.stores {
        st.energy = 0;
    }
    w.full_at = None;
    let mut standard_tasks = 0;
    for _ in 0..2 * ENERGY_REGEN_TIME {
        w.step();
        // Every worker whose (Standard) task did not run was given way, so Critical movers can
        // shove it: none is pinned in place.
        for (row, &h) in w.snap.creeps.handles().iter().enumerate() {
            if let Some(task) = w.opening.task(h)
                && task.tier() == crate::scheduler::Tier::Standard
                && !w.snap.creeps.spawning()[row]
            {
                standard_tasks += 1;
                assert!(w.movement.planned(row), "{task:?} at row {row} is pinned");
            }
        }
    }
    assert!(standard_tasks > 0, "some workers held Standard tasks");
    assert!(
        w.full_at.is_some(),
        "the room filled with only Critical work"
    );
    assert_eq!(w.failed, 0);
}

/// Uses: LOST_MAX, APPEAR_TICKS. Sites the game accepts but the processor drops are picked again
/// on other tiles, `APPEAR_TICKS` apart, until `LOST_MAX` in a row are lost; then the room gets no
/// more calls (each is billed, V203).
#[test]
fn picks_that_never_appear_stop_after_lost_max() {
    use super::placement::{APPEAR_TICKS, LOST_MAX};
    let mut w = World::new(2, SPAWN_ENERGY_CAPACITY);
    w.stores.retain(|s| s.kind == StructureType::Spawn);
    w.drop_sites = true;
    w.run(APPEAR_TICKS * (LOST_MAX + 5));
    assert_eq!(w.placed, LOST_MAX, "calls before the room gives up");
    assert_eq!(w.placement.totals().lost, LOST_MAX);
    assert!(w.sites.is_empty());
}

/// One worker walled in (walls built around it, which movement learns on its next rescan) can reach
/// nothing: its failures are its own, so the other workers keep harvesting, filling and upgrading,
/// and the room still fills and progresses.
#[test]
fn one_walled_in_worker_does_not_close_the_room() {
    let mut w = World::new(2, 0);
    w.run(recovery_bound(&w.cfg) + ENERGY_REGEN_TIME);
    // An empty worker far from every target, walled in.
    let mut victim = w
        .creeps
        .iter()
        .find(|c| c.spawning == 0)
        .expect("a worker")
        .clone();
    victim.key = w.key();
    victim.identity.serial += 1;
    victim.at = tile(35, 40);
    victim.energy = 0;
    w.extra_walls = victim.at.neighbors().map(|(_, t)| t).collect();
    w.creeps.push(victim);
    w.movement.invalidate(w.room);
    let progress = w.progress;
    for st in &mut w.stores {
        st.energy = 0;
    }
    w.full_at = None;
    w.run(CREEP_LIFE_TIME / 2);
    assert!(
        w.full_at.is_some(),
        "the room filled around the walled-in worker"
    );
    assert!(w.progress > progress, "the controller progressed");
    assert_eq!(w.failed, 0);
}

/// The same snapshots give the same intents: two worlds run in lockstep stay identical.
#[test]
fn the_opening_is_deterministic() {
    let mut a = World::new(2, 0);
    let mut b = World::new(2, 0);
    for _ in 0..800 {
        a.step();
        b.step();
        let at = |w: &World| {
            w.creeps
                .iter()
                .map(|c| (c.key, c.at, c.energy))
                .collect::<Vec<_>>()
        };
        assert_eq!(at(&a), at(&b));
    }
    assert_eq!(a.progress, b.progress);
    let _ = MMO;
}

/// Ticks until a fresh room (RCL1, a full spawn, no creeps) is at RCL3, with `at_level_up`.
fn ticks_to_rcl3(at_level_up: LevelUp, place: bool, limit: u32) -> Option<u32> {
    let mut w = World::new(1, SPAWN_ENERGY_START);
    w.at_level_up = at_level_up;
    w.cfg.place_extensions = place;
    let start = w.time;
    for _ in 0..limit {
        w.step();
        if w.level >= 3 {
            assert_eq!(w.failed, 0);
            assert_eq!(
                w.refused_sites, 0,
                "the model refused a site the bot placed"
            );
            assert!(w.event_at(|e| matches!(e, Event::Stalled { .. })).is_none());
            if place {
                assert_eq!(
                    w.placed,
                    StructureType::Extension.controller_structures(2),
                    "the bot placed RCL2's extensions"
                );
            }
            // A room whose capacity pays for a miner places a container per source (ADR-023);
            // the spawn's capacity alone pays for none.
            let one = Trip {
                tiles: 1,
                cost: MOVE_COST_PLAIN,
            };
            let pays = miner_body(SOURCE_ENERGY_CAPACITY, one, w.room_energy().capacity).is_some();
            let want = if pays { w.sources.len() } else { 0 };
            assert_eq!(w.placed_containers as usize, want, "containers");
            return Some(w.time - start);
        }
    }
    None
}

/// The least time from RCL1 to RCL3: `CONTROLLER_LEVELS[1] + CONTROLLER_LEVELS[2]` progress at the
/// most two owned sources yield (`SOURCE_ENERGY_CAPACITY` every `ENERGY_REGEN_TIME` each), with
/// nothing spent on spawning. Uses: `CONTROLLER_LEVELS`, `SOURCE_ENERGY_CAPACITY`,
/// `ENERGY_REGEN_TIME`.
fn rcl3_floor() -> u32 {
    let need = controller_levels(1).expect("1") + controller_levels(2).expect("2");
    (need * ENERGY_REGEN_TIME).div_ceil(2 * SOURCE_ENERGY_CAPACITY)
}

/// With `opening.place_extensions` off and no site placed by hand, a fresh room reaches RCL3 on
/// its spawn's capacity alone, within 4 times the source-limited floor: 6874 ticks in the scenario
/// room with P3.6's solved worker bodies (2026-11-17), 8022 with the 1:1:1 pattern (2026-11-15).
#[test]
fn a_fresh_room_reaches_rcl3_without_extensions() {
    let bound = 4 * rcl3_floor();
    let ticks = ticks_to_rcl3(LevelUp::Nothing, false, bound + 1).expect("RCL3 within the bound");
    assert!(ticks >= rcl3_floor());
}

/// The level's extension sites placed by hand at each level-up (what ADR-020 measured): the
/// workers build and fill them, and a fresh room reaches RCL3 within 3 times the floor: 5892 ticks
/// in the scenario room (2026-11-15), 6146 with P3.1's miners and their containers (2026-11-16),
/// 6264 with P3.6's solved worker bodies (2026-11-17).
#[test]
fn a_fresh_room_reaches_rcl3_building_its_extensions() {
    let bound = 3 * rcl3_floor();
    let ticks = ticks_to_rcl3(LevelUp::Sites, false, bound + 1).expect("RCL3 within the bound");
    assert!(ticks >= rcl3_floor());
}

/// The P2 gate's path (ROADMAP P2: a fresh room reaches RCL3 with no manual action): the bot places
/// RCL2's extensions itself (ADR-020), its workers build and fill them, and the room reaches RCL3
/// within 3 times the source-limited floor, with no site refused, no intent failed and no stall:
/// 6029 ticks before P3.1, 6258 with its miners and containers, which pay off by RCL4
/// (`static_mining_reaches_rcl4_sooner`; 2026-11-16), 6272 with P3.6's solved worker bodies
/// (2026-11-17).
#[test]
fn a_fresh_room_reaches_rcl3_with_no_manual_action() {
    let bound = 3 * rcl3_floor();
    let ticks = ticks_to_rcl3(LevelUp::Nothing, true, bound + 1).expect("RCL3 within the bound");
    assert!(ticks >= rcl3_floor());
}

/// Swamp on every walkable tile of rows 12 to 18 of `w`'s room: a band between the controller at
/// (25,8) and the spawn at (25,25), and between the controller and both sources.
fn swamp_band(w: &mut World) {
    for y in 12..=18u8 {
        for x in 1..49u8 {
            let i = usize::from(y) * 50 + usize::from(x);
            if w.raw[i] & (Terrain::Wall as u8) == 0 {
                w.raw[i] = Terrain::Swamp as u8;
            }
        }
    }
}

/// Uses: MOVE_COST_SWAMP (through the model's fatigue), the level progress and source capacity of
/// `rcl3_floor`. A fresh room with a swamp band between the spawn and the controller (every
/// worker's way crosses it) reaches RCL3 with no manual action within 4 times the source-limited
/// floor: its workers are solved for the swamp's fatigue (P3.6; 7481 ticks in the scenario room,
/// 2026-11-17, against 9604 with the 1:1:1 pattern of P2.4, which this bound would refuse).
#[test]
fn a_fresh_swampy_room_reaches_rcl3() {
    let mut w = World::new(1, SPAWN_ENERGY_START);
    w.at_level_up = LevelUp::Nothing;
    swamp_band(&mut w);
    let start = w.time;
    let bound = 4 * rcl3_floor();
    while w.level < 3 && w.time - start <= bound {
        w.step();
    }
    assert!(w.level >= 3, "RCL3 within {bound} ticks");
    assert_eq!(w.failed, 0);
    assert!(w.event_at(|e| matches!(e, Event::Stalled { .. })).is_none());
}

// ---------------------------------------------------------------------------------------------
// Placement (ADR-020)

/// An open room with walls on the border ring, its grid, the spawn at (25,25), the controller at
/// (25,8) and sources at (8,30) and (42,20).
fn placement_room(raw: Vec<u8>) -> (CostGrid, Tile, Tile, Vec<Tile>) {
    let terrain = TerrainGrid::from_row_major(&raw).expect("terrain");
    let spawn = tile(25, 25);
    let controller = tile(25, 8);
    let sources = vec![tile(8, 30), tile(42, 20)];
    let mut objects: Vec<(Tile, Footprint)> = vec![
        (spawn, Footprint::Blocked),
        (controller, Footprint::Blocked),
    ];
    objects.extend(sources.iter().map(|&s| (s, Footprint::Blocked)));
    let mut grid = CostGrid::default();
    grid.build(&terrain, objects, crate::movement::profile(&MOVEMENT_MMO));
    (grid, spawn, controller, sources)
}

fn bordered(f: impl Fn(u8, u8) -> bool) -> Vec<u8> {
    let wall = Terrain::Wall as u8;
    let mut raw = Vec::with_capacity(2500);
    for y in 0..50u8 {
        for x in 0..50u8 {
            let edge = x == 0 || y == 0 || x == 49 || y == 49;
            raw.push(if edge || f(x, y) { wall } else { 0 });
        }
    }
    raw
}

/// Picks one by one (each pick then counts as a blocking site) until none is left or `max`.
fn place_all(
    grid: &CostGrid,
    spawn: Tile,
    controller: Tile,
    sources: &[Tile],
    max: usize,
) -> Vec<Tile> {
    let profile = crate::movement::profile(&MOVEMENT_MMO);
    let mut occupied: Vec<(Tile, bool)> = vec![(spawn, true), (controller, true)];
    occupied.extend(sources.iter().map(|&s| (s, true)));
    let (mut field, mut dial) = (FlowField::default(), Dial::default());
    let mut out = Vec::new();
    while out.len() < max {
        let Some(t) = next_extension(
            grid,
            profile,
            spawn,
            Some(controller),
            sources,
            &occupied,
            &[],
            &mut field,
            &mut dial,
        ) else {
            break;
        };
        assert!(!out.contains(&t), "{t:?} picked twice");
        occupied.push((t, true));
        out.push(t);
    }
    out
}

/// Uses: EDGE_MARGIN, SPAWN_CLEAR, CONTROLLER_CLEAR, the spawn's parity. Every pick follows the
/// pattern's rules, and picks come nearest the spawn first.
#[test]
fn extensions_follow_the_pattern() {
    let (grid, spawn, controller, sources) = placement_room(bordered(|_, _| false));
    let n = StructureType::Extension.controller_structures(8) as usize;
    let picks = place_all(&grid, spawn, controller, &sources, n);
    assert_eq!(picks.len(), n, "an open room holds RCL8's extensions");
    let parity = (spawn.x() + spawn.y()) % 2;
    let mut last = 0;
    for &t in &picks {
        assert_eq!((t.x() + t.y()) % 2, parity, "{t:?}");
        assert!(t.range(spawn) >= SPAWN_CLEAR, "{t:?}");
        assert!(t.range(controller) > CONTROLLER_CLEAR, "{t:?}");
        assert!(sources.iter().all(|&s| t.range(s) > 1), "{t:?}");
        let (lo, hi) = (EDGE_MARGIN, 50 - EDGE_MARGIN);
        assert!(
            (lo..hi).contains(&t.x()) && (lo..hi).contains(&t.y()),
            "{t:?}"
        );
        let r = t.range(spawn);
        assert!(r >= last, "nearest first: {t:?} at range {r} after {last}");
        last = r;
    }
    // RCL2's five are the nearest same-parity tiles beyond the spawn's ring.
    for &t in picks
        .iter()
        .take(StructureType::Extension.controller_structures(2) as usize)
    {
        assert_eq!(t.range(spawn), SPAWN_CLEAR, "{t:?}");
    }
}

/// However many are placed, every free tile the spawn reached before still reaches it, and every
/// extension has a free orthogonal neighbour to be filled from.
#[test]
fn extensions_never_cut_the_room() {
    // A wall with a one-tile gap at (30, 25) and an L-shaped wall near the spawn.
    let raw = bordered(|x, y| (x == 30 && y != 25) || (y == 20 && (18..=28).contains(&x)));
    let (grid, spawn, controller, sources) = placement_room(raw);
    let picks = place_all(&grid, spawn, controller, &sources, 400);
    assert!(picks.len() > 100, "{} picks", picks.len());
    let reach = |blocked: &dyn Fn(Tile) -> bool| {
        // A BFS over free tiles, from the spawn's neighbours.
        let mut seen = vec![false; 2500];
        let mut queue: Vec<Tile> = spawn
            .neighbors()
            .map(|(_, t)| t)
            .filter(|&t| grid.walkable(t) && !blocked(t))
            .collect();
        for &t in &queue {
            seen[t.index()] = true;
        }
        while let Some(t) = queue.pop() {
            for (_, n) in t.neighbors() {
                if grid.walkable(n) && !blocked(n) && !seen[n.index()] {
                    seen[n.index()] = true;
                    queue.push(n);
                }
            }
        }
        seen
    };
    let before = reach(&|_| false);
    let after = reach(&|t| picks.contains(&t));
    for i in 0..2500 {
        let t = Tile::from_index(i).expect("tile");
        if before[i] && !picks.contains(&t) {
            assert!(after[i], "{t:?} is cut off by the extensions");
        }
    }
    for &t in &picks {
        let fillable = [
            rustedscreep_consts::Direction::Top,
            rustedscreep_consts::Direction::Right,
            rustedscreep_consts::Direction::Bottom,
            rustedscreep_consts::Direction::Left,
        ]
        .into_iter()
        .filter_map(|d| t.step(d))
        .any(|n| after[n.index()]);
        assert!(fillable, "{t:?} cannot be filled");
    }
    // The gap in the wall stays open.
    assert!(!picks.contains(&tile(30, 25)));
}

/// A road, a site, a refused tile and a container's tile are never picked; a room with no free
/// tile gives none.
#[test]
fn extensions_skip_taken_and_refused_tiles() {
    let (grid, spawn, controller, sources) = placement_room(bordered(|_, _| false));
    let profile = crate::movement::profile(&MOVEMENT_MMO);
    let (mut field, mut dial) = (FlowField::default(), Dial::default());
    let first = next_extension(
        &grid,
        profile,
        spawn,
        Some(controller),
        &sources,
        &[],
        &[],
        &mut field,
        &mut dial,
    )
    .expect("a pick");
    // The same tile taken by a container (not blocking) or refused: another one.
    for (occupied, refused) in [(vec![(first, false)], vec![]), (vec![], vec![first])] {
        let next = next_extension(
            &grid,
            profile,
            spawn,
            Some(controller),
            &sources,
            &occupied,
            &refused,
            &mut field,
            &mut dial,
        )
        .expect("another pick");
        assert_ne!(next, first);
    }
    // A road on it.
    let terrain = TerrainGrid::from_row_major(&bordered(|_, _| false)).expect("terrain");
    let mut road = CostGrid::default();
    road.build(
        &terrain,
        vec![(spawn, Footprint::Blocked), (first, Footprint::Road)],
        profile,
    );
    let next = next_extension(
        &road,
        profile,
        spawn,
        Some(controller),
        &sources,
        &[],
        &[],
        &mut field,
        &mut dial,
    )
    .expect("another pick");
    assert_ne!(next, first);
    // A spawn walled in on every side: nothing reachable, no pick.
    let raw =
        bordered(|x, y| (24..=26).contains(&x) && (24..=26).contains(&y) && (x, y) != (25, 25));
    let (boxed, ..) = placement_room(raw);
    assert_eq!(
        next_extension(
            &boxed,
            profile,
            spawn,
            Some(controller),
            &sources,
            &[],
            &[],
            &mut field,
            &mut dial,
        ),
        None
    );
}

/// The job in the scenario world: one pick per tick up to the level's count, none once the count
/// is reached, and the count includes sites and built extensions.
#[test]
fn placement_picks_one_site_per_tick_up_to_the_level() {
    let mut w = World::new(2, SPAWN_ENERGY_CAPACITY);
    w.stores.retain(|s| s.kind == StructureType::Spawn);
    let allowed = StructureType::Extension.controller_structures(2);
    let mut per_tick = Vec::new();
    for _ in 0..(allowed + 5) {
        let before = w.placed;
        w.step();
        per_tick.push(w.placed - before);
    }
    assert!(per_tick.iter().all(|&n| n <= 1), "{per_tick:?}");
    assert_eq!(w.placed, allowed);
    assert_eq!(w.refused_sites, 0);
    w.take_snapshot();
    assert_eq!(extensions_in(&w.snap, w.room), allowed);
}

// ---------------------------------------------------------------------------------------------
// Static mining (P3.1, ADR-023)

/// The tile of `p` in the scenario room.
fn tile_of(p: Position) -> Tile {
    Tile::of(p)
}

/// The miner of each source: its creep's index in the world, by plan order (none before the room
/// has a plan).
fn miners_on(w: &World) -> Vec<Option<usize>> {
    let Some((_, Some(plan), _)) = w.mining.room_states().next() else {
        return Vec::new();
    };
    plan.sources
        .iter()
        .map(|s| {
            w.creeps.iter().position(|c| {
                c.identity.role == Role::Miner
                    && c.spawning == 0
                    && w.snap
                        .creeps
                        .handle_of(ObjectKey(c.key))
                        .and_then(|h| w.mining.source_of(h))
                        == Some(s.source)
            })
        })
        .collect()
}

/// Uses: SOURCE_ENERGY_CAPACITY, ENERGY_REGEN_TIME, HARVEST_POWER, BODYPART_COST, CREEP_LIFE_TIME,
/// CONSTRUCTION_COST, CONTAINER_CAPACITY. At RCL2 with its extensions built (550 capacity), each
/// source gets one miner of `miner_body` for its plan's trip (`miner_work` WORK parts), standing on
/// a walkable tile next to it, off the room's edge. Once both stand there, every regeneration cycle
/// of each source ends with the source empty (all of `SOURCE_ENERGY_CAPACITY` harvested) until the
/// first miner ages out, and no worker harvests a source whose miner is next to it. Within a second
/// creep life the workers have built a container on each tile (2 × `CONSTRUCTION_COST` of
/// building on top of the upgrading).
#[test]
fn miners_drain_their_sources_from_their_containers() {
    let mut w = World::new(2, SPAWN_ENERGY_CAPACITY);
    let capacity = w.room_energy().capacity;
    let mut settled: Option<u32> = None;
    for _ in 0..CREEP_LIFE_TIME {
        w.step();
        // No worker harvests a mined source.
        for c in &w.creeps {
            let Some(h) = w.snap.creeps.handle_of(ObjectKey(c.key)) else {
                continue;
            };
            if let Some(Task::Harvest(src)) = w.opening.task(h) {
                let row = w.snap.objects.row_of(src).expect("source row");
                let pos = w.snap.objects.positions()[row];
                assert!(
                    w.mining.mined(w.room).iter().all(|m| m.source != pos),
                    "a worker harvests mined source {pos:?} at {}",
                    w.time
                );
            }
        }
        let on = miners_on(&w);
        let Some((_, Some(plan), _)) = w.mining.room_states().next() else {
            continue;
        };
        let all_on = !on.is_empty()
            && on
                .iter()
                .zip(&plan.sources)
                .all(|(m, s)| m.is_some_and(|i| Some(w.pos(w.creeps[i].at)) == s.tile));
        if all_on && settled.is_none() {
            settled = Some(w.time);
        }
    }
    let settled = settled.expect("both miners on their tiles within a creep's life");
    let (_, plan, stats) = w.mining.room_states().next().expect("room");
    let plan = plan.expect("plan").clone();
    assert_eq!(stats.miners as usize, w.sources.len());
    for s in &plan.sources {
        let tile = tile_of(s.tile.expect("a tile"));
        assert_eq!(tile.range(tile_of(s.source)), 1, "next to its source");
        assert!(!tile.is_edge());
        let body = miner_body(SOURCE_ENERGY_CAPACITY, s.trip.expect("trip"), capacity);
        assert_eq!(s.body, body);
        assert_eq!(
            count(&body.expect("body"), Part::Work),
            miner_work(SOURCE_ENERGY_CAPACITY)
        );
    }
    let miners: Vec<&(u32, Body)> = w
        .spawned
        .iter()
        .filter(|(_, b)| b.count(Part::Carry) == 0)
        .collect();
    assert_eq!(miners.len(), w.sources.len(), "one miner per source");
    let first_death = miners
        .iter()
        .map(|(t, b)| t + b.spawn_time() + CREEP_LIFE_TIME)
        .min();
    let window = |t: u32| t > settled + ENERGY_REGEN_TIME && first_death.is_none_or(|d| t < d);
    let cycles: Vec<_> = w.regens.iter().filter(|r| window(r.0)).collect();
    for i in 0..w.sources.len() {
        assert!(
            cycles.iter().any(|r| r.1 == i),
            "a cycle of source {i}: {cycles:?}"
        );
    }
    assert!(
        cycles.iter().all(|r| r.2 == 0),
        "energy left at a refill: {cycles:?}"
    );
    // The containers, within a second creep life.
    let containers = |w: &World| {
        plan.sources.iter().all(|s| {
            let tile = s.tile.map(tile_of);
            w.loose
                .iter()
                .any(|l| matches!(*l, Loose::Container { at, .. } if Some(at) == tile))
        })
    };
    for _ in 0..CREEP_LIFE_TIME {
        if containers(&w) {
            break;
        }
        w.step();
    }
    assert!(containers(&w), "a container on each mining tile");
    assert_eq!(w.placed_containers as usize, w.sources.len());
    assert_eq!(w.failed, 0);
    assert!(w.event_at(|e| matches!(e, Event::Stalled { .. })).is_none());
}

/// After a global reset (the heap state of mining and the opening lost), each miner takes the
/// source of the mining tile it stands on again, so no second miner is ordered for a mined source,
/// and the run is the same for two identical worlds (determinism).
#[test]
fn a_reset_keeps_one_miner_per_source() {
    let run = || {
        let mut w = World::new(2, SPAWN_ENERGY_CAPACITY);
        w.run(900);
        let before = miners_on(&w);
        assert!(before.iter().all(Option::is_some), "{before:?}");
        let spawned = w.spawned.len();
        w.mining = Mining::default();
        w.opening = Opening::default();
        w.run(300);
        let after = miners_on(&w);
        assert_eq!(before, after, "each miner keeps its source");
        let new_miners = w.spawned[spawned..]
            .iter()
            .filter(|(_, b)| b.count(Part::Carry) == 0)
            .count();
        assert_eq!(new_miners, 0, "no second miner");
        w
    };
    let (a, b) = (run(), run());
    let at = |w: &World| {
        w.creeps
            .iter()
            .map(|c| (c.key, c.at, c.energy))
            .collect::<Vec<_>>()
    };
    assert_eq!(at(&a), at(&b));
    assert_eq!(a.progress, b.progress);
}

/// With `mining.enabled` and `mining.place_containers` off the room runs the opening alone: no
/// miner and no container site.
#[test]
fn mining_off_orders_no_miner_and_no_container() {
    let mut w = World::new(2, SPAWN_ENERGY_CAPACITY);
    w.mining_cfg = MiningConfig {
        enabled: false,
        place_containers: false,
        ..MINING_MMO
    };
    w.run(1500);
    assert_eq!(w.miners(), 0);
    assert_eq!(w.placed_containers, 0);
    assert!(w.sites.iter().all(|s| s.kind != StructureType::Container));
    assert!(w.workers() > 0);
}

/// Ticks from a fresh room (RCL1, a full spawn, no creeps) to RCL4, with `mining`.
fn ticks_to_rcl4(mining: MiningConfig, limit: u32) -> Option<u32> {
    let mut w = World::new(1, SPAWN_ENERGY_START);
    w.at_level_up = LevelUp::Nothing;
    w.mining_cfg = mining;
    let start = w.time;
    for _ in 0..limit {
        w.step();
        if w.level >= 4 {
            assert_eq!(w.failed, 0);
            assert!(w.event_at(|e| matches!(e, Event::Stalled { .. })).is_none());
            return Some(w.time - start);
        }
    }
    None
}

/// The P2 gate's fresh room, on to RCL4 (`CONTROLLER_LEVELS[1..=3]`), with no manual action: with
/// static miners and their containers (the default) it gets there sooner than the opening alone,
/// after paying for the containers' `CONSTRUCTION_COST` and the miners at RCL2. 17334 ticks
/// against 18643 in the scenario room (2026-11-16, P3.1); 17407 with the containers' decay and
/// repairs (P3.5); 17105 against 18925 with P3.6's solved worker bodies (2026-11-17).
#[test]
fn static_mining_reaches_rcl4_sooner() {
    let need: u32 = (1..=3).map(|l| controller_levels(l).expect("level")).sum();
    let floor = (need * ENERGY_REGEN_TIME).div_ceil(2 * SOURCE_ENERGY_CAPACITY);
    let limit = 3 * floor;
    let off = MiningConfig {
        enabled: false,
        place_containers: false,
        ..MINING_MMO
    };
    let without = ticks_to_rcl4(off, limit).expect("RCL4 without miners");
    let with = ticks_to_rcl4(MINING_MMO, limit).expect("RCL4 with miners");
    assert!(with >= floor);
    assert!(with < without, "with miners {with}, without {without}");
}

/// Two sources whose nearest free neighbour to the spawn is the same tile get two mining tiles,
/// each next to its own source, so both miners reach a tile and both sources are mined.
#[test]
fn neighbouring_sources_get_their_own_mining_tiles() {
    let mut w = World::new(2, SPAWN_ENERGY_CAPACITY);
    // (25,29) is next to both and the nearest of their neighbours to the spawn at (25,25).
    w.sources[0].at = tile(24, 30);
    w.sources[1].at = tile(26, 30);
    w.run(900);
    let (_, plan, stats) = w.mining.room_states().next().expect("room");
    let plan = plan.expect("plan");
    let tiles: Vec<Tile> = plan
        .sources
        .iter()
        .map(|s| {
            let t = tile_of(s.tile.expect("a tile"));
            assert_eq!(t.range(tile_of(s.source)), 1);
            t
        })
        .collect();
    assert_ne!(tiles[0], tiles[1], "{tiles:?}");
    assert!(tiles.contains(&tile(25, 29)));
    assert_eq!(stats.mined as usize, w.sources.len());
}

// ---------------------------------------------------------------------------------------------
// The link network (P3.2, ADR-024)

/// links/transfer.js L46 at a90d26f takes `Math.ceil(amount * C.LINK_LOSS_RATIO)` with the engine's
/// double, 0.03 (engine common constants.js; a pinned value from that source, V134, V217). Over every
/// amount one send can move, [`link_loss`] from the crate's f32 constant gives the same.
#[test]
fn link_loss_matches_the_engines_double() {
    let engine = 0.03_f64;
    assert_eq!(engine as f32, rustedscreep_consts::LINK_LOSS_RATIO);
    for amount in 1..=LINK_CAPACITY {
        assert_eq!(
            link_loss(amount),
            (f64::from(amount) * engine).ceil() as u32,
            "{amount}"
        );
        assert!(link_loss(amount) >= 1 && link_loss(amount) < amount.max(2));
    }
    assert_eq!(link_loss(0), 0);
}

/// A linked miner's body (P3.2) is the plain miner's plus one CARRY: an empty CARRY makes no
/// fatigue (V54), so whenever the CARRY's cost fits, the WORK and MOVE parts are the same; when it
/// does not, it has no more MOVE parts, and never a body where the plain miner has none.
#[test]
fn a_linked_miner_adds_one_carry() {
    let carry = Part::Carry.cost();
    for capacity in [
        SOURCE_ENERGY_NEUTRAL_CAPACITY,
        SOURCE_ENERGY_CAPACITY,
        SOURCE_ENERGY_KEEPER_CAPACITY,
    ] {
        for tiles in [1, 4, 8, 20, 45] {
            for cost in [tiles, MOVE_COST_PLAIN * tiles, MOVE_COST_SWAMP * tiles] {
                let trip = Trip { tiles, cost };
                for energy in (300..=12_900).step_by(50) {
                    let plain = miner_body(capacity, trip, energy);
                    let linked = super::sizing::miner_body_with(capacity, trip, energy, 1);
                    match (plain, linked) {
                        (Some(p), Some(l)) => {
                            assert_eq!(count(&l, Part::Carry), 1);
                            assert_eq!(count(&l, Part::Work), count(&p, Part::Work));
                            assert!(l.cost() <= energy && l.len() <= MAX_CREEP_SIZE as usize);
                            if p.cost() + carry <= energy && p.len() < MAX_CREEP_SIZE as usize {
                                assert_eq!(count(&l, Part::Move), count(&p, Part::Move));
                                assert_eq!(l.cost(), p.cost() + carry);
                            } else {
                                assert!(count(&l, Part::Move) <= count(&p, Part::Move));
                            }
                        }
                        (None, Some(l)) => panic!("linked {l:?} where the plain miner has none"),
                        (Some(p), None) => assert!(
                            count(&p, Part::Work) * Part::Work.cost() + carry + Part::Move.cost()
                                > energy
                        ),
                        (None, None) => {}
                    }
                }
            }
        }
    }
}

/// Uses: SOURCE_ENERGY_CAPACITY, HARVEST_POWER, ENERGY_REGEN_TIME, CREEP_LIFE_TIME. A source's
/// harvest cycles: a rise over the last tick's energy is a refill, which ends a cycle that left the
/// last tick's energy less that tick's miner harvest (at most `WORK × HARVEST_POWER`, none without
/// one); a cycle its miner left before the end counts with what the last tick held; a cycle no
/// miner harvested in, or one ended after a skipped tick, is kept out; a full source starts no
/// cycle; the last `CREEP_LIFE_TIME / ENERGY_REGEN_TIME` cycles are kept, oldest first.
#[test]
fn source_harvest_cycles_from_constants() {
    let room: RoomName = ROOM.parse().expect("room");
    let at = place(room, tile(10, 10));
    let cap = SOURCE_ENERGY_CAPACITY;
    let power = 5 * HARVEST_POWER;
    assert_eq!(LIFE_CYCLES as u32, CREEP_LIFE_TIME / ENERGY_REGEN_TIME);
    let mut h = SourceHarvest::new(at);
    // Full and unharvested: no cycle.
    h.observe(100, cap, cap);
    h.observe(101, cap, cap);
    assert_eq!((h.cycles, h.unmined, h.missed), (0, 0, 0));
    // A cycle harvested down to 25 before its last harvest: it leaves 25 - power (at least 0).
    let mut t = 102;
    let mut energy = cap;
    while energy > 25 + power {
        h.observe(t, energy, cap);
        h.harvest(power);
        energy -= power;
        t += 1;
    }
    let before = 25;
    h.observe(t, before, cap);
    h.harvest(power);
    t += 1;
    h.observe(t, cap, cap);
    let left = before - power.min(before);
    assert_eq!(h.cycles, 1);
    assert_eq!(h.recent(), &[(cap - left, cap)][..]);
    assert_eq!(
        (h.harvested, h.capacity),
        (u64::from(cap - left), u64::from(cap))
    );
    // Its miner gone before the end: the cycle counts with what the last tick held.
    h.observe(t + 1, cap - power, cap);
    h.harvest(power);
    h.observe(t + 2, 400, cap);
    h.observe(t + 3, cap, cap);
    assert_eq!(h.cycles, 2);
    assert_eq!(h.recent().last(), Some(&(cap - 400, cap)));
    // A cycle no miner harvested in, and one ended after a skipped tick, are kept out.
    let t = t + 4;
    h.observe(t, cap - power, cap);
    h.observe(t + 1, cap, cap);
    assert_eq!(h.unmined, 1);
    h.observe(t + 2, cap - power, cap);
    h.harvest(power);
    h.observe(t + 4, cap, cap);
    assert_eq!(h.missed, 1);
    assert_eq!(h.cycles, 2);
    // The ring keeps the last LIFE_CYCLES cycles, oldest first.
    let mut t = t + 5;
    for k in 0..(LIFE_CYCLES as u32 + 2) {
        h.observe(t, k + 1, cap);
        h.harvest(0);
        h.observe(t + 1, cap, cap);
        t += 2;
    }
    let want: Vec<(u32, u32)> = (2..LIFE_CYCLES as u32 + 2)
        .map(|k| (cap - (k + 1), cap))
        .collect();
    assert_eq!(h.recent(), &want[..]);
    assert_eq!(h.cycles, 2 + LIFE_CYCLES as u32 + 2);
    // The life's figures over that ring: what it harvested of what it held, and its worst cycle.
    let got: u64 = want.iter().map(|w| u64::from(w.0)).sum();
    let held = u64::from(cap) * LIFE_CYCLES as u64;
    let worst = u64::from(cap - (LIFE_CYCLES as u32 + 2)) * 1000 / u64::from(cap);
    assert_eq!(h.life(), Some((got * 1000 / held, worst)));
    assert_eq!(h.overall(), Some(h.harvested * 1000 / h.capacity));
    assert_eq!(SourceHarvest::new(at).life(), None);
    // After a missed refill, a cycle no miner harvests in is kept out as unmined.
    let (cycles, unmined) = (h.cycles, h.unmined);
    h.observe(t, cap - power, cap);
    h.harvest(power);
    h.observe(t + 2, cap, cap);
    h.observe(t + 3, cap - power, cap);
    h.observe(t + 4, cap, cap);
    assert_eq!((h.cycles, h.unmined), (cycles, unmined + 1));
    assert_eq!(permille(999, 1000), Some(999));
    assert_eq!(permille(1, 3), Some(333));
    assert_eq!(permille(1, 0), None);
}

/// The recorder against the world model's own ledger of refills (the energy each source held when
/// it refilled, after that tick's harvests): in a room that starts at RCL4, every refill after the
/// first tick is counted once, as a cycle or kept out, and each cycle a miner harvested to the end
/// left what the model says. Uses: SOURCE_ENERGY_CAPACITY.
#[test]
fn source_harvest_matches_the_worlds_refills() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    let start = w.time;
    w.run(6000);
    let room = w.room;
    let records = w.mining.harvest(room);
    assert_eq!(records.len(), w.sources.len());
    let mut counted = 0;
    for (i, s) in w.sources.iter().enumerate() {
        let rec = records
            .iter()
            .find(|r| r.source == place(room, s.at))
            .expect("a record per source");
        // The recorder sees a refill on the tick after it; the last tick's refill is still unseen.
        let refills: Vec<u32> = w
            .regens
            .iter()
            .filter(|r| r.1 == i && r.0 > start && r.0 < w.time - 1)
            .map(|r| r.2)
            .collect();
        assert_eq!(
            (rec.cycles + rec.unmined + rec.missed) as usize,
            refills.len(),
            "source {i}"
        );
        assert_eq!(rec.missed, 0, "the model skips no tick");
        assert!(rec.cycles > 0, "source {i}: mined cycles");
        // The last cycles, newest last, against the model's last refills: the refills kept out
        // come before the first miner harvests, so the last ones are all counted (the cycle a
        // miner died in among them).
        let recent = rec.recent();
        assert!(refills.len() >= recent.len());
        let tail = &refills[refills.len() - recent.len()..];
        let mut differ = 0;
        for (&(harvested, cap), &left) in recent.iter().zip(tail) {
            assert_eq!(cap, SOURCE_ENERGY_CAPACITY);
            // Never higher than it was: a worker's harvest of a source whose miner is gone, on a
            // cycle's last tick, is not seen; at most the one cycle a life the miner dies in.
            assert!(
                harvested <= cap - left,
                "source {i}: {recent:?} against {tail:?}"
            );
            if harvested != cap - left {
                differ += 1;
            }
        }
        assert!(differ <= 1, "source {i}: {recent:?} against {tail:?}");
        counted += rec.cycles;
    }
    assert!(counted > 0);
}

/// One source's miner gone for good (mining off): its record counts no cycle but the one its miner
/// was in and keeps every later refill out as unmined, while the other source's cycles go on and
/// match the world model's ledger (lower by at most one miner tick's take), so each harvest is
/// credited to its own source. Uses: HARVEST_POWER, ENERGY_REGEN_TIME.
#[test]
fn source_harvest_follows_each_source() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.run(4000);
    let room = w.room;
    let at = |w: &World, i: usize| place(room, w.sources[i].at);
    let rec = |w: &World, i: usize| {
        *w.mining
            .harvest(room)
            .iter()
            .find(|r| r.source == at(w, i))
            .expect("a record")
    };
    // Every miner but source 1's goes, spawning ones included (one not yet given a source would
    // take source 0 on coming out).
    let at1 = at(&w, 1);
    let mining1 = |w: &World| -> Vec<u128> {
        w.creeps
            .iter()
            .filter(|c| c.identity.role == Role::Miner && c.spawning == 0)
            .filter(|c| {
                w.snap
                    .creeps
                    .handle_of(ObjectKey(c.key))
                    .and_then(|h| w.mining.source_of(h))
                    == Some(at1)
            })
            .map(|c| c.key)
            .collect()
    };
    // A tick where source 1 has its miner (not between two of them).
    let mut waited = 0;
    while mining1(&w).is_empty() {
        assert!(waited < CREEP_LIFE_TIME, "source 1 gets a miner");
        w.step();
        waited += 1;
    }
    let keep = mining1(&w);
    let (gone, kept) = (rec(&w, 0), rec(&w, 1));
    w.mining_cfg.enabled = false;
    w.creeps
        .retain(|c| c.identity.role != Role::Miner || keep.contains(&c.key));
    let removed_at = w.time;
    w.run(4 * ENERGY_REGEN_TIME);
    let (gone2, kept2) = (rec(&w, 0), rec(&w, 1));
    // Source 0: the cycle its miner was in counts, and every later refill (if a worker's harvest
    // started one; else the source stays full) is kept out as unmined.
    let refills0 = w
        .regens
        .iter()
        .filter(|r| r.1 == 0 && r.0 > removed_at && r.0 < w.time - 1)
        .count() as u32;
    assert!(gone2.cycles <= gone.cycles + 1, "{gone:?} then {gone2:?}");
    assert_eq!(
        (gone2.cycles - gone.cycles) + (gone2.unmined - gone.unmined),
        refills0,
        "{gone:?} then {gone2:?}"
    );
    assert!(kept2.cycles >= kept.cycles + 3, "{kept:?} then {kept2:?}");
    let lefts: Vec<u32> = w
        .regens
        .iter()
        .filter(|r| r.1 == 1 && r.0 > removed_at && r.0 < w.time - 1)
        .map(|r| r.2)
        .collect();
    let recent = kept2.recent();
    let n = lefts.len().min(recent.len());
    for (&(harvested, cap), &left) in recent[recent.len() - n..]
        .iter()
        .zip(&lefts[lefts.len() - n..])
    {
        assert!(harvested <= cap - left);
        assert!(cap - left - harvested <= HARVEST_POWER * miner_work(cap));
    }
}

/// The mining plan of room `w` (after a run).
fn mine_plan(w: &World) -> MinePlan {
    w.mining
        .room_states()
        .next()
        .and_then(|(_, p, _)| p.cloned())
        .expect("plan")
}

/// ADR-024's tiles in the scenario room at RCL5: the controller link 2 from the controller, each
/// source link next to its mining tile, all inside the margin, off the mining tiles and each other,
/// and none closes a way (its free neighbours stay connected); then the sites go in order, the
/// controller link first and the farthest source's next, as many as the level allows
/// (`CONTROLLER_STRUCTURES`) and `links.source_links` leaves.
#[test]
fn links_are_planned_and_placed_by_the_rules() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(3);
    let plan = mine_plan(&w);
    let controller = w.pos(w.controller_at);
    let c = plan.controller_link.expect("a controller link tile");
    assert_eq!(
        c.x()
            .u8()
            .abs_diff(controller.x().u8())
            .max(c.y().u8().abs_diff(controller.y().u8())),
        CONTROLLER_LINK_RANGE
    );
    let high = rustedscreep_consts::ROOM_SIZE - 1 - LINK_MARGIN;
    let mining: Vec<Tile> = plan
        .sources
        .iter()
        .filter_map(|s| s.tile.map(tile_of))
        .collect();
    let mut links = vec![tile_of(c)];
    for s in &plan.sources {
        let (Some(t), Some(l)) = (s.tile, s.link) else {
            panic!("source {s:?} has no link tile");
        };
        assert_eq!(tile_of(l).range(tile_of(t)), 1, "next to the mining tile");
        links.push(tile_of(l));
    }
    for (i, &l) in links.iter().enumerate() {
        assert!((LINK_MARGIN..=high).contains(&l.x()) && (LINK_MARGIN..=high).contains(&l.y()));
        assert!(!mining.contains(&l), "{l:?} is a mining tile");
        assert!(!links[..i].contains(&l), "{l:?} twice");
        assert!(!w.blocked(l));
        let others: Vec<Tile> = links.iter().copied().filter(|&o| o != l).collect();
        assert!(super::placement::keeps_neighbours_connected(l, |n| {
            w.blocked(n) || others.contains(&n)
        }));
    }
    w.run(3000);
    let allowed = StructureType::Link.controller_structures(5) as usize;
    let want = allowed.min(1 + LINK_MMO.source_links as usize);
    assert_eq!(w.placed_links.len(), want, "{:?}", w.placed_links);
    assert_eq!(w.placed_links[0], tile_of(c), "the controller link first");
    let farthest = plan
        .sources
        .iter()
        .max_by_key(|s| {
            (
                s.trip.map_or(0, |t| t.cost),
                std::cmp::Reverse(tile_key_of(s.source)),
            )
        })
        .and_then(|s| s.link)
        .expect("a source link");
    assert_eq!(
        w.placed_links[1],
        tile_of(farthest),
        "then the farthest source's"
    );
    assert_eq!(w.refused_sites, 0);
}

fn tile_key_of(p: Position) -> (u8, u8) {
    (p.x().u8(), p.y().u8())
}

/// The network at RCL5 (ADR-024), over four miner lives: both links stand, the linked source's
/// next miner carries one CARRY part, its harvest goes miner → source link → controller link →
/// workers, every send moves at least `links.send_min` and loses [`link_loss`] of it, none is
/// refused or clamped (each fits the receiver's room at the tick's start, V216, V217), no intent
/// fails, and the sends keep going to the end (a controller link no send fits is drained, so the
/// pipe never stalls).
#[test]
fn links_carry_a_sources_energy_to_the_controller() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(4 * CREEP_LIFE_TIME);
    let links = w
        .stores
        .iter()
        .filter(|s| s.kind == StructureType::Link)
        .count();
    assert_eq!(links, 2);
    assert!(
        w.spawned.iter().any(|(_, b)| count(b, Part::Carry) == 1
            && count(b, Part::Work) == miner_work(SOURCE_ENERGY_CAPACITY)),
        "a linked miner"
    );
    let totals = *w.mining.link_totals();
    assert!(w.sends > 0 && totals.sent == w.sends && totals.failed == 0);
    assert_eq!(w.refused_sends, 0);
    assert_eq!(w.clamped_sends, 0);
    assert_eq!(w.dup_sends, 0);
    assert_eq!(w.failed, 0);
    assert!(w.link_takes > 0, "workers take from the controller link");
    // What arrived is what was sent less the loss of each send.
    assert!(
        u64::from(w.received) < totals.energy && u64::from(w.received) * 100 >= totals.energy * 96
    );
    let before = w.sends;
    w.run(CREEP_LIFE_TIME);
    assert!(w.sends > before, "the sends go on");
    assert_eq!(w.failed, 0);
}

/// Upgrade progress from tick 6000 to 18000 of a room that starts at the level, with the defaults
/// (`LINK_MMO`) or with the network off.
fn steady_progress(level: u8, links: LinkConfig) -> (u64, u64) {
    let mut w = World::new(level, SPAWN_ENERGY_CAPACITY);
    w.link_cfg = links;
    w.run(6000);
    let (p, b) = (w.upgraded(), w.billed);
    w.run(12_000);
    assert_eq!(w.failed, 0);
    (w.upgraded() - p, w.billed - b)
}

/// The network pays at RCL5 and RCL6 in the scenario room (ADR-024 "As built"): more upgrade
/// progress from tick 6000 to 18000 than with it off, for fewer billed intents. Measured
/// 2026-11-16: RCL5 162285 against 154665 progress (+4.9%), 4.83 against 5.13 billed a tick; RCL6
/// 164133 against 156211 (+5.1%), 4.32 against 4.50. Counted from tick 0, the links' build cost
/// (`CONSTRUCTION_COST` 5000 each) is paid back by tick 24000 at RCL5 (286491 against 286604), not
/// yet at RCL6 (279027 against 281219). With the containers' decay and repairs (P3.5): RCL5 160537
/// against 152910 (+5.0%), RCL6 161762 against 151348 (+6.9%). With P3.6's solved worker bodies
/// (2026-11-17, six starts, ticks 6000 to 30000, progress counted across level-ups): RCL5 +4.8%
/// (+4.0% to +5.2%), RCL6 +2.2% (+1.7% to +2.7%), billed intents −2.8% and −5.4%; the 1:1:1
/// pattern gave +4.5% and +5.2% the same way: at RCL6 the room with links is within 0.4% of the
/// pattern's (`OpeningConfig::body_slack_pct`'s measurement), the one without gained about 2.5%.
#[test]
fn links_raise_the_upgrade_rate() {
    let off = LinkConfig {
        enabled: false,
        place_links: false,
        ..LINK_MMO
    };
    for level in [5, 6] {
        let (with, with_billed) = steady_progress(level, LINK_MMO);
        let (without, without_billed) = steady_progress(level, off);
        assert!(with > without, "RCL{level}: {with} against {without}");
        assert!(
            with_billed < without_billed,
            "RCL{level}: {with_billed} against {without_billed}"
        );
    }
}

/// With `links.enabled` and `links.place_links` off, no link site is placed, no miner gets a
/// CARRY part and nothing is sent.
#[test]
fn links_off_place_nothing_and_add_no_carry() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.link_cfg = LinkConfig {
        enabled: false,
        place_links: false,
        ..LINK_MMO
    };
    w.run(2 * CREEP_LIFE_TIME);
    assert!(w.placed_links.is_empty());
    assert!(w.sites.iter().all(|s| s.kind != StructureType::Link));
    assert!(w.spawned.iter().all(|(_, b)| count(b, Part::Carry) == 0
        || count(b, Part::Work) != miner_work(SOURCE_ENERGY_CAPACITY)
        || b.len() > 12));
    assert_eq!(w.sends, 0);
    assert!(w.mining.link_orders().is_empty());
}

/// Two source links into one controller link (RCL6, `links.source_links` 2): each tick's sends
/// share the receiver's room at the tick's start, so none is refused or clamped, and both send.
#[test]
fn two_source_links_share_the_receivers_room() {
    let mut w = World::new(6, SPAWN_ENERGY_CAPACITY);
    w.link_cfg = LinkConfig {
        source_links: 2,
        ..LINK_MMO
    };
    // Five lives: with haulers the room keeps fewer workers, which build its sites later.
    w.run(5 * CREEP_LIFE_TIME);
    assert_eq!(w.placed_links.len(), 3);
    let plan = mine_plan(&w);
    let senders: Vec<Tile> = plan
        .sources
        .iter()
        .filter_map(|s| s.link.map(tile_of))
        .collect();
    assert_eq!(senders.len(), 2);
    let sent: Vec<bool> = senders.iter().map(|t| w.senders.contains(t)).collect();
    assert_eq!(sent, [true, true], "both source links send");
    assert!(w.sends > 0);
    assert_eq!(w.refused_sends, 0);
    assert_eq!(w.clamped_sends, 0);
    assert_eq!(w.dup_sends, 0);
    assert_eq!(w.failed, 0);
}

/// Links past the level's count (V218): at RCL5, three links placed by hand (the planned tiles of
/// the controller link and both source links), so the source link farther from the controller is
/// inactive. The bot adopts all three, never orders a send from the inactive one (none is refused),
/// and the active one sends; no miner fills the inactive one, and its source's miners get no CARRY
/// part for it.
#[test]
fn an_inactive_link_never_sends() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(3);
    let plan = mine_plan(&w);
    let mut tiles = vec![tile_of(plan.controller_link.expect("controller link"))];
    tiles.extend(plan.sources.iter().filter_map(|s| s.link.map(tile_of)));
    assert_eq!(tiles.len(), 3);
    for &at in &tiles {
        let key = w.key();
        w.stores.push(SimStore {
            key,
            kind: StructureType::Link,
            at,
            energy: 0,
            capacity: LINK_CAPACITY,
            cooldown: 0,
        });
    }
    let (near, far) = tiles[1].min_by_range(tiles[2], w.controller_at);
    w.run(4 * CREEP_LIFE_TIME);
    assert!(w.placed_links.is_empty(), "the level's links stand already");
    assert_eq!(w.refused_sends, 0);
    assert!(w.senders.contains(&near), "the active source link sends");
    assert!(!w.senders.contains(&far), "the inactive one never does");
    let far_link = w.stores.iter().find(|s| s.at == far).expect("far link");
    assert_eq!(far_link.energy, 0, "no miner fills an inactive link");
    let plan = mine_plan(&w);
    let far_source = plan
        .sources
        .iter()
        .find(|s| s.tile.is_some_and(|t| tile_of(t).range(far) <= 1))
        .expect("the far source");
    assert_eq!(far_source.body.map(|b| count(&b, Part::Carry)), Some(0));
    assert_eq!(w.dup_sends, 0);
    assert_eq!(w.failed, 0);
}

/// One link next to two mining tiles (two sources three tiles apart, so both plan sources take the
/// link the planner places between their tiles): it is one sender, so it gets at most one order a
/// tick, and none is clamped (the reviewers' layout, 2026-11-16).
#[test]
fn a_link_two_mining_tiles_share_sends_once_a_tick() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.sources[0].at = tile(5, 5);
    w.sources[1].at = tile(5, 8);
    w.run(3);
    let plan = mine_plan(&w);
    let links: Vec<Option<Position>> = plan.sources.iter().map(|s| s.link).collect();
    w.run(4 * CREEP_LIFE_TIME);
    let plan = mine_plan(&w);
    let shared = plan
        .sources
        .iter()
        .filter_map(|s| s.link)
        .collect::<Vec<_>>();
    assert_eq!(shared.len(), 2);
    assert_eq!(
        shared[0], shared[1],
        "both sources name one link (planned {links:?})"
    );
    assert!(w.sends > 0);
    assert_eq!(w.dup_sends, 0, "one order a tick per link");
    assert_eq!(w.clamped_sends, 0);
    assert_eq!(w.refused_sends, 0);
    assert_eq!(w.failed, 0);
}

/// A link tile the game refuses (here the planned controller link's) is never picked again: the
/// plan takes another tile 2 from the controller, and both links the level allows are placed.
#[test]
fn a_refused_link_tile_is_not_picked_again() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(3);
    let first = tile_of(mine_plan(&w).controller_link.expect("controller link"));
    w.refuse_tiles.push(first);
    w.run(3000);
    let plan = mine_plan(&w);
    let c = tile_of(plan.controller_link.expect("another controller link tile"));
    assert_ne!(c, first);
    assert_eq!(c.range(w.controller_at), CONTROLLER_LINK_RANGE);
    assert_eq!(w.placed_links.len(), 2, "{:?}", w.placed_links);
    assert_eq!(w.placed_links[0], c);
    assert_eq!(w.refused_sites, 1, "one refusal, then another tile");
}

/// A send the game refuses holds its link for `links.hold_ticks`: no order from it until then.
#[test]
fn a_refused_send_holds_its_link() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(3 * CREEP_LIFE_TIME);
    assert!(w.sends > 0, "the network runs");
    let start = w.time;
    w.refuse_sends_until = start + 2 * LINK_MMO.hold_ticks;
    w.run(4 * LINK_MMO.hold_ticks);
    let during: Vec<(u32, Tile)> = w
        .orders_at
        .iter()
        .copied()
        .filter(|o| o.0 >= start)
        .collect();
    assert!(!during.is_empty());
    for pair in during.windows(2) {
        if pair[0].1 == pair[1].1 && pair[0].0 < w.refuse_sends_until {
            assert!(pair[1].0 - pair[0].0 >= LINK_MMO.hold_ticks, "{pair:?}");
        }
    }
    assert!(
        w.refused_sends >= 1 && w.refused_sends <= 2,
        "{}",
        w.refused_sends
    );
    assert!(
        during.iter().any(|o| o.0 >= w.refuse_sends_until),
        "it sends again after the hold"
    );
}

trait NearerOf {
    /// `(nearer, farther)` of two tiles by range to `to` (they must differ).
    fn min_by_range(self, other: Tile, to: Tile) -> (Tile, Tile);
}

impl NearerOf for Tile {
    fn min_by_range(self, other: Tile, to: Tile) -> (Tile, Tile) {
        assert_ne!(self.range(to), other.range(to));
        if self.range(to) < other.range(to) {
            (self, other)
        } else {
            (other, self)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Repairs (P3.5)

/// Uses: CONTAINER_HITS. A container is repaired below `start_pct` of its `hitsMax`, and once under
/// repair up to full; never at full hits.
#[test]
fn a_container_needs_repair_below_the_share_until_full() {
    let pct = REPAIR_MMO.start_pct;
    let start = u32::try_from(u64::from(CONTAINER_HITS) * u64::from(pct) / 100).expect("fits");
    assert!(needs_repair(start - 1, pct, false));
    assert!(!needs_repair(start, pct, false), "at the share it waits");
    assert!(needs_repair(start, pct, true), "under repair it goes on");
    assert!(needs_repair(CONTAINER_HITS - 1, pct, true));
    assert!(!needs_repair(CONTAINER_HITS, pct, true), "full is done");
    assert!(!needs_repair(CONTAINER_HITS, 99, false));
    for pct in [10, 50, 99] {
        let start = u32::try_from(u64::from(CONTAINER_HITS) * u64::from(pct) / 100).expect("fits");
        assert!(needs_repair(start - 1, pct, false), "{pct}");
        assert!(!needs_repair(start, pct, false), "{pct}");
    }
}

/// The tile next to source 0 (8,30) that is nearest the spawn: its mining tile, where a container
/// wins (P3.1).
const WORN_TILE: (u8, u8) = (9, 29);

/// A room at RCL3 with a full spawn and no creeps, and a container on source 0's mining tile with
/// `hits`, its next decay step `CONTAINER_DECAY_TIME_OWNED` − 1 ticks away (V214).
fn worn_container_world(hits: u32) -> World {
    let mut w = World::new(3, SPAWN_ENERGY_CAPACITY);
    // P3.5's rules where the workers gather at the mining tiles: haulers off (with them a mining
    // tile's container is repaired as a linked source's is, `a_hauled_rooms_containers_are_kept`).
    w.hauling_cfg = HAULING_OFF;
    let key = w.key();
    let at = tile(WORN_TILE.0, WORN_TILE.1);
    let decay_at = w.time + CONTAINER_DECAY_TIME_OWNED;
    w.loose.push(Loose::Container {
        key,
        at,
        energy: 0,
        hits,
        decay_at,
    });
    w
}

/// Uses: CONTAINER_HITS, CONTAINER_DECAY, CONTAINER_DECAY_TIME_OWNED, CREEP_LIFE_TIME, and
/// `repair.start_pct`. [`worn_container_world`] at full hits after a creep life: workers settled at
/// both sources (the placement built source 1's container meanwhile, ADR-023), and the worn
/// container still above the share (it took fewer than `CREEP_LIFE_TIME` /
/// `CONTAINER_DECAY_TIME_OWNED` + 1 steps), so nothing was repaired yet.
fn settled_world() -> World {
    let mut w = worn_container_world(CONTAINER_HITS);
    w.run(CREEP_LIFE_TIME);
    let steps = CREEP_LIFE_TIME / CONTAINER_DECAY_TIME_OWNED + 1;
    assert!(!below_share(
        CONTAINER_HITS - steps * CONTAINER_DECAY,
        REPAIR_MMO.start_pct
    ));
    assert_eq!(w.repairs, 0, "above the share: no repair");
    w
}

/// The hits of the container at `at`, or `None` once it is gone.
fn hits_at(w: &World, at: Tile) -> Option<u32> {
    w.loose.iter().find_map(|l| match *l {
        Loose::Container { at: a, hits, .. } if a == at => Some(hits),
        _ => None,
    })
}

/// The worn container's hits, or `None` once it is gone.
fn worn_hits(w: &World) -> Option<u32> {
    hits_at(w, tile(WORN_TILE.0, WORN_TILE.1))
}

/// Sets the energy of the container at `at`.
fn set_container_energy(w: &mut World, at: Tile, to: u32) {
    for l in &mut w.loose {
        if let Loose::Container { at: a, energy, .. } = l
            && *a == at
        {
            *energy = to;
        }
    }
}

/// Sets the hits of the container at `at`, and its next decay step `CONTAINER_DECAY_TIME_OWNED` − 1
/// ticks from now (V214).
fn set_container(w: &mut World, at: Tile, to: u32) {
    let next = w.time + CONTAINER_DECAY_TIME_OWNED;
    for l in &mut w.loose {
        if let Loose::Container {
            at: a,
            hits,
            decay_at,
            ..
        } = l
            && *a == at
        {
            *hits = to;
            *decay_at = next;
        }
    }
}

/// The share `pct` of `CONTAINER_HITS` (`repair.start_pct`, `repair.walk_pct`). Uses: CONTAINER_HITS.
fn share(pct: u32) -> u32 {
    u32::try_from(u64::from(CONTAINER_HITS) * u64::from(pct) / 100).expect("fits")
}

/// Uses: CONTAINER_HITS, CONTAINER_DECAY, REPAIR_POWER, CREEP_LIFE_TIME. In a settled RCL3 room
/// ([`settled_world`]), the container on source 0's mining tile set one decay step below
/// `repair.start_pct` of its `hitsMax` is repaired up to full within half a creep life, by one
/// worker at a time, each within repair range of it when it is given the task (the container stays
/// above `repair.walk_pct`); no repair is billed at full hits (V222); the repairs add at least the
/// hits that were missing, at most `REPAIR_POWER` per WORK part of the largest worker per call;
/// once it is full the room has nothing under repair.
#[test]
fn a_worn_container_is_repaired_to_full() {
    let start = share(REPAIR_MMO.start_pct);
    let mut w = settled_world();
    let worn = tile(WORN_TILE.0, WORN_TILE.1);
    set_container(&mut w, worn, start - CONTAINER_DECAY);
    let from = w.time;
    let mut full_at = None;
    let mut most_repairers = 0;
    let repair = Task::KINDS
        .iter()
        .position(|&k| k == "repair")
        .expect("kind");
    for _ in 0..CREEP_LIFE_TIME / 2 {
        w.step();
        let (_, _, _, stats) = w.opening.room_states().next().expect("room");
        most_repairers = most_repairers.max(stats.tasks[repair]);
        assert!(!below_share(
            worn_hits(&w).expect("there"),
            REPAIR_MMO.walk_pct
        ));
        assert_eq!(
            repairers_out_of_range(&w),
            0,
            "a worker walked at {}",
            w.time
        );
        if worn_hits(&w) == Some(CONTAINER_HITS) {
            full_at = Some(w.time - from);
            break;
        }
    }
    assert!(
        full_at.is_some(),
        "full within half a creep life: {:?}",
        worn_hits(&w)
    );
    assert_eq!(most_repairers, 1, "one worker per container");
    assert_eq!(w.repair_noops, 0, "no repair billed at full hits");
    assert_eq!(w.failed, 0);
    assert!(
        w.repair_log.iter().all(|&t| t == worn),
        "only that container"
    );
    // What the repairs added: the hits missing plus any decay step since.
    assert!(w.repaired >= u64::from(CONTAINER_HITS - (start - CONTAINER_DECAY)));
    // A call repairs at most `REPAIR_POWER` per WORK part of the largest worker.
    let max_work = w
        .creeps
        .iter()
        .filter(|c| c.identity.role == Role::Worker)
        .map(|c| u64::from(count(&c.body, Part::Work)))
        .max()
        .expect("workers");
    assert!(w.repaired <= u64::from(w.repairs) * max_work * u64::from(REPAIR_POWER));
    // Full now: nothing under repair, and no repair task on the next tick.
    w.step();
    let (_, _, _, stats) = w.opening.room_states().next().expect("room");
    assert_eq!(stats.repairing, 0);
    // Both mining tiles' containers, source 1's built meanwhile (ADR-023).
    let tiles = w.mining.mine_tiles(w.room).to_vec();
    let on_tiles = w
        .loose
        .iter()
        .filter(|l| matches!(**l, Loose::Container { at, .. } if tiles.contains(&w.pos(at))))
        .count();
    assert_eq!(stats.containers as usize, on_tiles);
    assert_eq!(stats.tasks[repair], 0);
}

/// Workers given a repair this tick (as the snapshot placed them, before their moves) farther than
/// repair range (3, V221) from the container at `at`, or from any container with `None`.
fn walking_repairers(w: &World, at: Option<Tile>) -> usize {
    let snap = &w.snap;
    (0..snap.creeps.len())
        .filter(|&row| {
            let h = snap.creeps.handles()[row];
            let Some(Task::Repair(o)) = w.opening.task(h) else {
                return false;
            };
            let from = tile_of(snap.creeps.positions()[row]);
            let to = tile_of(snap.objects.positions()[snap.objects.row_of(o).expect("row")]);
            at.is_none_or(|t| t == to) && from.range(to) > 3
        })
        .count()
}

fn repairers_out_of_range(w: &World) -> usize {
    walking_repairers(w, None)
}

/// Uses: CONTAINER_HITS, CONTAINER_DECAY, CREEP_LIFE_TIME. A container off the mining plan's tiles
/// (a previous owner's, say) is never repaired, even below `repair.walk_pct`, and the room's
/// summary counts only the containers on its mining tiles (the user's choice, 2026-11-16: "Mining
/// tiles only").
#[test]
fn a_container_off_the_mining_tiles_is_never_repaired() {
    let mut w = settled_world();
    let far = tile(44, 44);
    let key = w.key();
    let decay_at = w.time + CONTAINER_DECAY_TIME_OWNED;
    w.loose.push(Loose::Container {
        key,
        at: far,
        energy: 0,
        hits: share(REPAIR_MMO.walk_pct) - CONTAINER_DECAY,
        decay_at,
    });
    w.run(CREEP_LIFE_TIME);
    assert!(
        !w.repair_log.contains(&far),
        "repaired off the mining tiles"
    );
    assert!(hits_at(&w, far).expect("there") < share(REPAIR_MMO.walk_pct));
    let (_, _, _, stats) = w.opening.room_states().next().expect("room");
    let tiles = w.mining.mine_tiles(w.room).to_vec();
    assert!(!tiles.contains(&w.pos(far)));
    let on_tiles = w
        .loose
        .iter()
        .filter(|l| matches!(**l, Loose::Container { at, .. } if tiles.contains(&w.pos(at))))
        .count();
    assert_eq!(stats.containers as usize, on_tiles);
}

/// Uses: CONTAINER_HITS, CONTAINER_DECAY, CONTAINER_DECAY_TIME_OWNED, CREEP_LIFE_TIME. At RCL5 with
/// the link network (one source link, `links.source_links`), the linked source's container, whose
/// gatherers wait at the controller link instead (P3.2), set between `repair.walk_pct` and
/// `repair.start_pct`: under repair, but over a creep life (three decay steps, still above the walk
/// share) no worker is sent to it from outside repair range. Set below `repair.walk_pct`, it is
/// repaired back above that share within a creep life. Upgraders off (P3.4): with them the workers
/// take from the nearest container, this one too, and repair it from there.
#[test]
fn a_linked_container_waits_for_the_walk_share() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.upgrading_cfg = UPGRADING_OFF;
    let mut linked = None;
    for _ in 0..24 {
        w.run(500);
        // The source a link carries (the opening's `linked` also marks a hauled one, P3.3).
        let (_, mine, _) = w.mining.room_states().next().expect("room");
        linked = mine
            .and_then(|m| m.sources.iter().find(|s| s.linked))
            .and_then(|s| s.tile);
        if linked.is_some_and(|t| hits_at(&w, tile_of(t)).is_some()) {
            break;
        }
    }
    let at = tile_of(linked.expect("a linked source within 12000 ticks"));
    let walk = share(REPAIR_MMO.walk_pct);
    // Three steps in a creep life, each `CONTAINER_DECAY`: still above the walk share.
    set_container(&mut w, at, walk + 4 * CONTAINER_DECAY);
    for _ in 0..CREEP_LIFE_TIME {
        w.step();
        assert_eq!(walking_repairers(&w, Some(at)), 0, "walked at {}", w.time);
    }
    let hits = hits_at(&w, at).expect("there");
    assert!(below_share(hits, REPAIR_MMO.start_pct) && !below_share(hits, REPAIR_MMO.walk_pct));
    let (_, _, _, stats) = w.opening.room_states().next().expect("room");
    assert!(stats.repairing >= 1, "under repair all the same");
    // Below the walk share: repaired back above it.
    set_container(&mut w, at, walk - CONTAINER_DECAY);
    let mut back = false;
    for _ in 0..CREEP_LIFE_TIME {
        w.step();
        if !below_share(hits_at(&w, at).expect("there"), REPAIR_MMO.walk_pct) {
            back = true;
            break;
        }
    }
    assert!(back, "repaired above the walk share: {:?}", hits_at(&w, at));
    assert!(w.repair_log.contains(&at));
    assert_eq!(w.repair_noops, 0);
    assert_eq!(w.failed, 0);
}

/// Uses: CONTAINER_HITS, CARRY_CAPACITY. With one free worker (the spawn kept busy, nothing to fill,
/// build or upgrade by rule) and both mining tiles' containers below `repair.walk_pct` with equal
/// hits, the worker keeps the container it started on until its store is empty, though its own
/// repairs make the other one the fewest (no swapping between them).
#[test]
fn a_repairer_keeps_its_container() {
    let mut w = World::new(3, SPAWN_ENERGY_CAPACITY);
    w.hauling_cfg = HAULING_OFF;
    w.cfg = OpeningConfig {
        min_upgraders: 0,
        place_extensions: false,
        ..OPENING_MMO
    };
    w.mining_cfg = MiningConfig {
        place_containers: false,
        ..MINING_MMO
    };
    for s in &mut w.stores {
        if s.spawn_energy() {
            s.energy = s.capacity;
        }
    }
    let home = w.room;
    // The spawn stays busy: a creep spawning for the whole test.
    let key = w.key();
    w.creeps.push(SimCreep {
        key,
        identity: Identity {
            role: Role::Worker,
            home,
            serial: 1,
        },
        at: tile(25, 25),
        body: repeats(1),
        energy: 0,
        fatigue: 0,
        spawning: u32::MAX / 2,
        born: 0,
    });
    w.step();
    let tiles: Vec<Position> = mine_plan(&w)
        .sources
        .iter()
        .filter_map(|s| s.tile)
        .collect();
    assert_eq!(tiles.len(), 2, "two mining tiles");
    let hits = share(REPAIR_MMO.walk_pct) - CONTAINER_DECAY;
    for &t in &tiles {
        let key = w.key();
        w.loose.push(Loose::Container {
            key,
            at: tile_of(t),
            energy: 0,
            hits,
            decay_at: u32::MAX / 2,
        });
    }
    let key = w.key();
    let body = repeats(4);
    let energy = count(&body, Part::Carry) * CARRY_CAPACITY;
    w.creeps.push(SimCreep {
        key,
        identity: Identity {
            role: Role::Worker,
            home,
            serial: 2,
        },
        at: tile(25, 20),
        body,
        energy,
        fatigue: 0,
        spawning: 0,
        born: w.time,
    });
    let mut targets = Vec::new();
    for _ in 0..200 {
        w.step();
        let Some(h) = w.snap.creeps.handle_of(ObjectKey(key)) else {
            continue;
        };
        match w.opening.task(h) {
            Some(Task::Repair(o)) => targets.push(o),
            _ if !targets.is_empty() => break,
            _ => {}
        }
    }
    assert!(!targets.is_empty(), "it repaired");
    assert!(
        targets.iter().all(|&o| o == targets[0]),
        "it swapped containers: {targets:?}"
    );
    assert!(!w.repair_log.is_empty());
    assert!(w.repair_log.iter().all(|&t| t == w.repair_log[0]));
}

/// Uses: CONTAINER_HITS, CONTAINER_DECAY, CONTAINER_DECAY_TIME_OWNED, CREEP_LIFE_TIME. In a settled
/// room ([`settled_world`]), a container above `repair.start_pct` is not repaired, though workers
/// with energy stand within repair range of it (its gatherers take their loads there), until a
/// decay step takes it below the share; then it is repaired up to full.
#[test]
fn a_container_above_the_share_waits_for_it() {
    let start = share(REPAIR_MMO.start_pct);
    let mut w = settled_world();
    let worn = tile(WORN_TILE.0, WORN_TILE.1);
    set_container(&mut w, worn, start + CONTAINER_DECAY / 2);
    // Its next step comes on the tick `CONTAINER_DECAY_TIME_OWNED` − 1 on (V214).
    let mut near = 0;
    for _ in 0..CONTAINER_DECAY_TIME_OWNED - 1 {
        w.step();
        let snap = &w.snap;
        near += (0..snap.creeps.len())
            .filter(|&row| {
                snap.creeps.stores()[row].energy > 0
                    && tile_of(snap.creeps.positions()[row]).range(worn) <= 3
                    && snap.creeps.identities()[row].is_some_and(|id| id.role == Role::Worker)
            })
            .count();
    }
    assert!(near > 0, "workers with energy came within range");
    assert_eq!(worn_hits(&w), Some(start + CONTAINER_DECAY / 2));
    assert!(!w.repair_log.contains(&worn), "above the share: no repair");
    w.run(1);
    assert_eq!(worn_hits(&w), Some(start - CONTAINER_DECAY / 2), "the step");
    let mut full = false;
    for _ in 0..CREEP_LIFE_TIME {
        w.step();
        if worn_hits(&w) == Some(CONTAINER_HITS) {
            full = true;
            break;
        }
    }
    assert!(full, "repaired to full once below: {:?}", worn_hits(&w));
    assert_eq!(w.repair_noops, 0);
}

/// Uses: CONTAINER_DECAY, CONTAINER_DECAY_TIME_OWNED. In a settled room ([`settled_world`]), the
/// container on source 0's mining tile set one decay step from gone is lost on that step with
/// `repair.enabled` off (no repair call), and kept with it on: its gatherers come within range of
/// it within the step's `CONTAINER_DECAY_TIME_OWNED` − 1 ticks.
#[test]
fn repairs_keep_a_container_that_would_decay_away() {
    let run = |enabled: bool| {
        let mut w = settled_world();
        w.repair_cfg = RepairConfig {
            enabled,
            ..REPAIR_MMO
        };
        set_container(&mut w, tile(WORN_TILE.0, WORN_TILE.1), CONTAINER_DECAY);
        w.run(CONTAINER_DECAY_TIME_OWNED);
        w
    };
    let off = run(false);
    assert_eq!(worn_hits(&off), None, "gone without repairs");
    assert_eq!(off.containers_lost, 1);
    assert_eq!(off.repairs, 0);
    let on = run(true);
    assert!(worn_hits(&on).is_some(), "kept with repairs");
    assert_eq!(on.containers_lost, 0);
    assert!(on.repairs > 0);
    assert_eq!(on.repair_noops, 0);
}

// ---------------------------------------------------------------------------------------------
// Haulers (P3.3, ADR-025)

/// Upgrade progress from tick 6000 to 18000 of a room that starts at `level`, and the billed
/// intents over the same ticks, with `hauling`.
fn steady_haul(level: u8, hauling: HaulingConfig) -> (u64, u64) {
    let mut w = World::new(level, SPAWN_ENERGY_CAPACITY);
    w.hauling_cfg = hauling;
    // Haulers alone, as measured before P3.4's upgraders (`upgraders_raise_the_upgrade_rate`).
    w.upgrading_cfg = UPGRADING_OFF;
    w.run(6000);
    let (p, b) = (w.upgraded(), w.billed);
    w.run(12_000);
    assert_eq!(w.failed, 0);
    (w.upgraded() - p, w.billed - b)
}

/// Haulers pay in the scenario room with upgrading off (ADR-025 "As built"): more upgrade progress
/// from tick 6000 to 18000 than with them off at RCL3, RCL4 and RCL6, and at RCL5 this start within
/// 2% of them off; fewer billed intents at RCL3 and RCL4, where the workers walked most. Measured
/// 2026-11-17 with P3.6's solved worker bodies, over six starts (the spawn's energy at the start 300
/// to 50) and ticks 6000 to 30000, progress counted across level-ups: RCL3 +9.2% (+8.6% to
/// +10.2%), RCL4 +6.9% (+6.0% to +8.0%), RCL5 +5.0% (−10.3% to +8.9%), RCL6 +10.3% (+7.6% to
/// +11.1%); billed intents −17.7%, −19.3%, −5.8%, +8.4%. This start, ticks 6000 to 18000: RCL3
/// 166141 against 155939, RCL4 170302 against 153261, RCL5 158871 against 161761 (−1.8%: with
/// upgrading off the two linked-source workers do all the upgrading, and one spawned small in a
/// hauler's gap upgrades less for its life), RCL6 166990 against 162459. Before P3.6 (the 1:1:1
/// pattern, progress not counted across level-ups): +5.1%, +2.8%, +6.1%, +10.4%; billed intents
/// −13.4%, −13.8%, +0.3%, +4.9%.
#[test]
fn haulers_raise_the_upgrade_rate() {
    for level in [3, 4, 5, 6] {
        let (with, with_billed) = steady_haul(level, HAULING_MMO);
        let (without, without_billed) = steady_haul(level, HAULING_OFF);
        if level == 5 {
            assert!(
                with * 50 >= without * 49,
                "RCL{level}: {with} against {without}"
            );
        } else {
            assert!(with > without, "RCL{level}: {with} against {without}");
        }
        if level <= 4 {
            assert!(
                with_billed < without_billed,
                "RCL{level}: {with_billed} against {without_billed}"
            );
        }
    }
}

/// Uses: CONTAINER_CAPACITY, CARRY_CAPACITY, `CONTROLLER_LINK_RANGE`. A room that
/// starts at RCL4 places the controller container on a free tile 2 from the controller (off the
/// controller link's tile, not next to a source), builds it, and gets the plan's haulers: CARRY and
/// MOVE parts only, at most `max_per_source` a source. They fill the spawns and extensions and
/// carry the rest into the container; the workers (from P3.4 the upgraders) take it there, the
/// workers are sized with no walk, and fill nothing while a hauler hauls a mined source.
#[test]
fn haulers_carry_a_sources_energy_to_the_controller_container() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.run(4000);
    let plan = mine_plan(&w);
    let at = plan
        .controller_container
        .expect("a controller container tile");
    let ct = w.controller_at;
    assert_eq!(tile_of(at).range(ct), CONTROLLER_LINK_RANGE);
    assert!(Some(at) != plan.controller_link);
    assert!(w.sources.iter().all(|s| tile_of(at).range(s.at) > 1));
    assert!(hits_at(&w, tile_of(at)).is_some(), "built");
    assert!(
        w.mining.mine_tiles(w.room).contains(&at),
        "repaired with the mining tiles"
    );
    let (_, haul, stats) = w.hauling.room_states().next().expect("room");
    let haul = haul.expect("a hauling plan");
    assert_eq!(haul.container, at);
    assert_eq!(haul.sources.len(), 2, "both sources hauled at RCL4");
    for s in &haul.sources {
        assert!(s.want >= 1 && s.want <= HAULING_MMO.max_per_source);
        let body = s.body.expect("a body");
        assert_eq!(
            body.len(),
            body.count(Part::Carry) + body.count(Part::Move),
            "CARRY and MOVE only"
        );
    }
    let want: u32 = haul.sources.iter().map(|s| s.want).sum();
    assert_eq!(stats.want, want);
    let (stored, energy) = (w.stored, w.stored_energy);
    let mut takes_there = 0;
    for _ in 0..2000 {
        w.step();
        let (_, _, _, os) = w.opening.room_states().next().expect("room");
        let (_, _, hs) = w.hauling.room_states().next().expect("room");
        assert!(hs.haulers <= want, "no more haulers than the plan wants");
        if hs.hauled > 0 {
            assert_eq!(os.tasks[2], 0, "no worker fills while a hauler hauls");
        }
        takes_there += w
            .creeps
            .iter()
            .filter(|c| {
                // The workers, or from P3.4 the upgraders, that take the container's energy.
                matches!(c.identity.role, Role::Worker | Role::Upgrader)
                    && c.spawning == 0
                    && c.at.range(tile_of(at)) <= 1
            })
            .count();
    }
    assert!(w.stored > stored, "the haulers stored into the container");
    assert_eq!(
        w.empty_takes, 0,
        "no take found its energy taken (the haulers' claims)"
    );
    assert!(w.stored_energy - energy >= u64::from(CARRY_CAPACITY));
    assert!(
        takes_there > 0,
        "workers or upgraders came to the container"
    );
    let (_, plan, _, _) = w.opening.room_states().next().expect("room");
    let plan = plan.expect("plan");
    assert!(
        plan.sources.iter().all(|s| s.linked),
        "hauled: sized with no walk"
    );
    assert!(w.loose.iter().all(|l| match l {
        Loose::Container { energy, .. } => *energy <= CONTAINER_CAPACITY,
        _ => true,
    }));
    assert_eq!(w.failed, 0);
}

/// Uses: CARRY_CAPACITY. An empty hauler and an empty worker both next to a hauled source's
/// container that holds exactly the hauler's load: the hauler takes it, and the worker is not
/// offered what the hauler claimed (`HaulView::taken`), so no take finds its energy gone (a billed
/// no-op in the engine).
#[test]
fn a_hauler_and_a_worker_never_take_the_same_energy() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.run(4000);
    let (_, haul, _) = w.hauling.room_states().next().expect("room");
    let tile = tile_of(haul.expect("a hauling plan").sources[0].tile);
    let h = w
        .creeps
        .iter()
        .position(|c| c.identity.role == Role::Hauler && c.spawning == 0)
        .expect("a hauler");
    let k = w
        .creeps
        .iter()
        .position(|c| c.identity.role == Role::Worker && c.spawning == 0)
        .expect("a worker");
    let free: Vec<Tile> = tile
        .neighbors()
        .map(|(_, t)| t)
        .filter(|&t| {
            !w.blocked(t)
                && w.sources.iter().all(|s| s.at != t)
                && w.creeps.iter().all(|c| c.at != t)
        })
        .take(2)
        .collect();
    assert_eq!(free.len(), 2, "two free tiles next to the mining tile");
    let load = w.creeps[h].capacity();
    w.creeps[h].at = free[0];
    w.creeps[h].energy = 0;
    w.creeps[k].at = free[1];
    w.creeps[k].energy = 0;
    for l in &mut w.loose {
        match l {
            Loose::Container { at, energy, .. } if *at == tile => *energy = load,
            _ => {}
        }
    }
    w.loose
        .retain(|l| !matches!(l, Loose::Pile { at, .. } if *at == tile));
    let empty = w.empty_takes;
    w.step();
    assert_eq!(w.creeps[h].energy, load, "the hauler took its load");
    assert_eq!(w.empty_takes, empty, "the worker was not sent for it");
    assert_eq!(w.failed, 0);
}

/// Uses: CONTAINER_CAPACITY. Behind a full mining container the miner's overflow makes a small pile
/// on the tile every tick (V183, V193): the haulers still take whole loads (the container's, or a
/// pile's once it holds one), not one billed pickup a tick of each new pile. Over a creep life with
/// both containers full at the start, every hauler take moves at least half the smallest hauler's
/// load on average.
#[test]
fn haulers_take_whole_loads_behind_a_full_container() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.run(4000);
    let tiles: Vec<Tile> = mine_plan(&w)
        .sources
        .iter()
        .filter_map(|s| s.tile.map(tile_of))
        .collect();
    for &t in &tiles {
        set_container_energy(&mut w, t, CONTAINER_CAPACITY);
    }
    let smallest = w
        .creeps
        .iter()
        .filter(|c| c.identity.role == Role::Hauler)
        .map(SimCreep::capacity)
        .min()
        .expect("a hauler");
    let (takes, taken) = (w.hauler_takes, w.hauler_taken);
    w.run(CREEP_LIFE_TIME);
    let n = u64::from(w.hauler_takes - takes);
    assert!(n > 0);
    assert!(
        (w.hauler_taken - taken) / n >= u64::from(smallest / 2),
        "{} energy in {n} takes",
        w.hauler_taken - taken
    );
    assert_eq!(w.failed, 0);
}

/// A hauled source whose miner is gone leaves the haulers nothing to fill with: from a tick where
/// the haulers fill (a miner is replaced only at its death, so its turnover leaves ticks with no
/// hauled source mined, when the workers fill too), the view no longer says the haulers fill once
/// the miners are gone, so the workers fill the drained spawns and extensions again.
#[test]
fn workers_fill_again_when_no_hauler_can() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.run(4000);
    let mut waited = 0;
    while !w.hauling.view(w.room).fills {
        assert!(
            waited < CREEP_LIFE_TIME,
            "the haulers fill again within a creep's life"
        );
        w.step();
        waited += 1;
    }
    w.mining_cfg.enabled = false;
    w.creeps.retain(|c| c.identity.role != Role::Miner);
    for s in &mut w.stores {
        if s.spawn_energy() {
            s.energy = 0;
        }
    }
    w.step();
    assert!(w.haulers() > 0, "the haulers are still there");
    assert!(!w.hauling.view(w.room).fills, "no source is mined");
    let mut fills = 0;
    for _ in 0..100 {
        w.step();
        let (_, _, _, os) = w.opening.room_states().next().expect("room");
        fills += os.tasks[2];
    }
    assert!(fills > 0, "the workers filled");
    assert_eq!(w.failed, 0);
}

/// With `hauling.enabled` and `hauling.place_container` off, no container is placed by the
/// controller and no hauler is ordered.
#[test]
fn haulers_off_place_no_container_and_order_none() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.hauling_cfg = HAULING_OFF;
    w.run(4000);
    let at = mine_plan(&w)
        .controller_container
        .expect("the tile is planned all the same");
    assert!(hits_at(&w, tile_of(at)).is_none(), "no container there");
    assert!(w.sites.iter().all(|s| s.at != tile_of(at)), "no site there");
    assert_eq!(w.haulers(), 0);
    assert!(
        w.spawned.iter().all(|(_, b)| b.count(Part::Work) > 0),
        "no CARRY-and-MOVE body"
    );
    assert_eq!(w.stored, 0);
}

/// Uses: CONTAINER_HITS, CONTAINER_DECAY, CONTAINER_DECAY_TIME_OWNED, CREEP_LIFE_TIME, and
/// `repair.walk_pct`. In a hauled room no gatherer comes to the mining tiles' containers, so they
/// are repaired as a linked source's is (P3.5: one walking worker below `repair.walk_pct`), and the
/// controller container, which the workers stand by, within range: over ten creep lives none is
/// lost and none falls a decay step below the walk share.
#[test]
fn a_hauled_rooms_containers_are_kept() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    let mut lowest = CONTAINER_HITS;
    for _ in 0..10 * CREEP_LIFE_TIME {
        w.step();
        for l in &w.loose {
            if let Loose::Container { hits, .. } = l {
                lowest = lowest.min(*hits);
            }
        }
    }
    assert!(w.haulers() > 0);
    assert_eq!(w.containers_lost, 0);
    assert!(
        lowest + CONTAINER_DECAY >= share(REPAIR_MMO.walk_pct),
        "lowest {lowest}"
    );
    assert!(w.repairs > 0);
    assert_eq!(w.failed, 0);
}

/// A global reset forgets every hauler's source and mode; the next ticks give them back from their
/// stores and the plan's wants, so no hauler is ordered beyond the plan and they keep storing.
#[test]
fn a_reset_keeps_the_haulers_hauling() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.run(4000);
    let before = w.haulers();
    assert!(before > 0);
    w.hauling = Hauling::default();
    let stored = w.stored;
    w.run(1000);
    let (_, plan, _) = w.hauling.room_states().next().expect("room");
    let want: u32 = plan.expect("plan").sources.iter().map(|s| s.want).sum();
    assert!(w.haulers() as u32 <= want);
    assert!(w.stored > stored, "storing again after the reset");
    assert_eq!(w.failed, 0);
}

/// With the Standard tier off (a low bucket), haulers still take and fill (Critical): the room's
/// spawns and extensions fill again after a spawn drained them; nothing goes into the controller
/// container, whose transfers are hauling (Standard).
#[test]
fn critical_hauling_goes_on_with_the_standard_tier_off() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.run(4000);
    assert!(w.haulers() > 0);
    w.standard = false;
    for s in &mut w.stores {
        if s.spawn_energy() {
            s.energy = 0;
        }
    }
    let stored = w.stored;
    let mut full = false;
    for _ in 0..CREEP_LIFE_TIME {
        w.step();
        let e = w.room_energy();
        if e.available == e.capacity {
            full = true;
            break;
        }
    }
    assert!(full, "refilled with Standard off: {:?}", w.room_energy());
    assert_eq!(w.stored, stored, "no transfer into the container");
    assert_eq!(w.failed, 0);
}

/// A hauled source's energy goes to the controller container for the opening; a linked one keeps
/// its link, and a source with no working hauler its tile.
#[test]
fn the_view_redirects_only_hauled_sources() {
    let room: RoomName = ROOM.parse().expect("room");
    let pos = |x, y| place(room, tile(x, y));
    let hauled = [pos(8, 30)];
    let view = HaulView {
        container: Some(pos(24, 10)),
        hauled: &hauled,
        fills: true,
        taken: &[],
        filled: &[],
    };
    let mined = |source, link| Mined {
        source,
        tile: pos(9, 29),
        link,
    };
    assert_eq!(
        view.redirect(mined(pos(8, 30), None)).link,
        Some(pos(24, 10))
    );
    assert_eq!(
        view.redirect(mined(pos(8, 30), Some(pos(25, 10)))).link,
        Some(pos(25, 10)),
        "a link carries it"
    );
    assert_eq!(
        view.redirect(mined(pos(42, 20), None)).link,
        None,
        "not hauled"
    );
    assert_eq!(
        HaulView::default().redirect(mined(pos(8, 30), None)).link,
        None
    );
}

// ---------------------------------------------------------------------------------------------
// Upgraders (P3.4)

const UPGRADING_OFF: UpgradingConfig = UpgradingConfig {
    enabled: false,
    ..UPGRADING_MMO
};

/// Uses: SOURCE_ENERGY_CAPACITY, CREEP_LIFE_TIME, ENERGY_REGEN_TIME. A source's yield over a creep
/// life: its capacity once every regeneration window (V184), for a creep's life (V170).
#[test]
fn a_sources_yield_per_life_from_constants() {
    let windows = CREEP_LIFE_TIME / ENERGY_REGEN_TIME;
    assert_eq!(
        yield_per_life(SOURCE_ENERGY_CAPACITY),
        u64::from(SOURCE_ENERGY_CAPACITY * windows)
    );
    assert_eq!(yield_per_life(0), 0);
}

/// Uses: CARRY_CAPACITY, UPGRADE_CONTROLLER_POWER, MAX_CREEP_SIZE. An upgrader's load is more than
/// one tick's upgrade, with the fewest CARRY parts that do it.
#[test]
fn an_upgraders_carry_outlasts_one_upgrade() {
    for work in 1..MAX_CREEP_SIZE {
        let carry = upgrader_carry(work);
        let tick = work * UPGRADE_CONTROLLER_POWER;
        assert!(carry * CARRY_CAPACITY > tick, "{work} WORK");
        assert!(
            (carry - 1) * CARRY_CAPACITY <= tick,
            "{work} WORK: the fewest"
        );
    }
}

/// Uses: CREEP_LIFE_TIME, CREEP_SPAWN_TIME, UPGRADE_CONTROLLER_POWER, MOVE_POWER, MOVE_COST_PLAIN.
/// What one upgrader upgrades over a life: its WORK parts' upgrade for the life left after its
/// spawning and its walk out, which only the WORK parts weigh (V54, V55); and what `n` of them
/// upgrade of a budget once their bodies are paid.
#[test]
fn an_upgraders_upgrade_from_constants() {
    let trip = Trip {
        tiles: 13,
        cost: 13 * MOVE_COST_PLAIN,
    };
    for (work, carry, moves) in [(1, 1, 1), (9, 1, 1), (15, 1, 3), (40, 1, 9)] {
        let walk = trip
            .tiles
            .max((trip.cost * work).div_ceil(MOVE_POWER * moves));
        let spawn = (work + carry + moves) * CREEP_SPAWN_TIME;
        let want = u64::from(work * UPGRADE_CONTROLLER_POWER * (CREEP_LIFE_TIME - walk - spawn));
        assert_eq!(
            upgrader_upgrade(work, carry, moves, trip),
            Some(want),
            "{work}/{carry}/{moves}"
        );
    }
    assert_eq!(upgrader_upgrade(5, 1, 0, trip), None, "no MOVE part");
    // Bodies first, then up to what they upgrade.
    assert_eq!(upgraded(10_000, 2, 1000, 3000), 6000);
    assert_eq!(upgraded(10_000, 2, 1000, 5000), 8000);
    assert_eq!(upgraded(1500, 2, 1000, 5000), 0);
}

/// Every upgrader body that fits `energy`, `MAX_CREEP_SIZE` and `max_work` with every count within
/// the caps, by brute force: (energy not upgraded, upgraders, energy for all of them, MOVE parts).
fn upgrader_options(
    budget: u64,
    trip: Trip,
    energy: u32,
    max_creeps: u32,
    max_work: Option<u32>,
) -> Vec<(u64, u32, u64, u32)> {
    let mut out = Vec::new();
    for work in 1..MAX_CREEP_SIZE {
        let carry = upgrader_carry(work);
        for moves in 1..=MAX_CREEP_SIZE.saturating_sub(work + carry) {
            let cost =
                work * Part::Work.cost() + carry * Part::Carry.cost() + moves * Part::Move.cost();
            if cost > energy || max_work.is_some_and(|m| work > m) {
                continue;
            }
            let upgrade = upgrader_upgrade(work, carry, moves, trip).expect("upgrade");
            let most = max_work.map_or(max_creeps, |m| (m / work).min(max_creeps));
            for n in 1..=most {
                out.push((
                    budget - upgraded(budget, n, cost, upgrade),
                    n,
                    u64::from(n) * u64::from(cost),
                    moves,
                ));
            }
        }
    }
    out
}

/// The room's spawn and extension energy at `rcl` with every spawn and extension the level allows
/// (V175).
/// Uses: SPAWN_ENERGY_CAPACITY, CONTROLLER_STRUCTURES, EXTENSION_ENERGY_CAPACITY.
fn level_energy(rcl: u32) -> u32 {
    StructureType::Spawn.controller_structures(rcl) * SPAWN_ENERGY_CAPACITY
        + StructureType::Extension.controller_structures(rcl) * extension_energy_capacity(rcl)
}

/// Property test over budgets (up to two keeper sources' yield), trips (road, plain and swamp
/// tiles), post counts and the room capacities of every level: an upgrader plan's body is WORK, then
/// `upgrader_carry` CARRY, then MOVE parts, costs at most the energy and holds at most
/// MAX_CREEP_SIZE parts; its count is within the posts; of every body and count that fit it
/// upgrades the most of the budget (`upgraded`), then has the fewest upgraders, then the least
/// energy for all of them, then the fewest MOVE parts; uncapped it leaves no more of the budget
/// than one body's worth unspent; at level 8 its WORK parts stay within
/// CONTROLLER_MAX_UPGRADE_PER_TICK; and an empty budget, no post, too little energy or a budget
/// below one body gives none.
/// Uses: BODYPART_COST, MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP, MAX_CREEP_SIZE,
/// CONTROLLER_MAX_UPGRADE_PER_TICK, UPGRADE_CONTROLLER_POWER, SOURCE_ENERGY_CAPACITY,
/// SOURCE_ENERGY_KEEPER_CAPACITY, SPAWN_ENERGY_CAPACITY, CONTROLLER_STRUCTURES,
/// EXTENSION_ENERGY_CAPACITY.
#[test]
fn upgrader_plans_fit_upgrade_the_most_and_take_the_fewest() {
    let mut energies: Vec<u32> = (1..=8u32)
        .map(level_energy)
        .chain([0, 199, 200, 201, 650])
        .collect();
    energies.sort_unstable();
    let cap = CONTROLLER_MAX_UPGRADE_PER_TICK / UPGRADE_CONTROLLER_POWER;
    let budgets = [
        1,
        yield_per_life(SOURCE_ENERGY_NEUTRAL_CAPACITY),
        yield_per_life(SOURCE_ENERGY_CAPACITY),
        2 * yield_per_life(SOURCE_ENERGY_CAPACITY),
        2 * yield_per_life(SOURCE_ENERGY_KEEPER_CAPACITY),
    ];
    for budget in budgets {
        for tiles in [0u32, 5, 20, 45] {
            for rate in [MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP] {
                let trip = Trip {
                    tiles,
                    cost: tiles * rate,
                };
                for (max_creeps, max_work) in [(1, None), (7, None), (7, Some(cap))] {
                    for &energy in &energies {
                        let options = upgrader_options(budget, trip, energy, max_creeps, max_work);
                        let case = format!("{budget} {tiles} {rate} {max_creeps} {energy}");
                        let Some((body, n)) =
                            upgrader_plan(budget, trip, energy, max_creeps, max_work)
                        else {
                            assert!(
                                options.iter().all(|o| o.0 == budget),
                                "{case}: none upgrades anything"
                            );
                            continue;
                        };
                        let work = count(&body, Part::Work);
                        let carry = count(&body, Part::Carry);
                        let moves = count(&body, Part::Move);
                        assert!(body.cost() <= energy, "{case}");
                        assert!(body.len() <= MAX_CREEP_SIZE as usize, "{case}");
                        assert_eq!(carry, upgrader_carry(work), "{case}");
                        assert!(work >= 1 && moves >= 1, "{case}");
                        assert_eq!(body.parts()[0], Part::Work, "WORK first");
                        assert_eq!(body.parts()[work as usize], Part::Carry, "then CARRY");
                        assert!(n >= 1 && n <= max_creeps, "{case}");
                        if let Some(m) = max_work {
                            assert!(n * work <= m, "{case}: level 8's cap");
                        }
                        let upgrade = upgrader_upgrade(work, carry, moves, trip).expect("upgrade");
                        let done = upgraded(budget, n, body.cost(), upgrade);
                        assert!(done > 0, "{case}");
                        let paid = u64::from(n) * u64::from(body.cost());
                        if n < max_creeps && max_work.is_none() {
                            assert!(
                                budget - paid - done <= u64::from(body.cost()) + upgrade,
                                "{case}: less than one body's worth unspent"
                            );
                        }
                        let best = options.iter().copied().min().expect("an option");
                        let got = (budget - done, n, paid, moves);
                        assert_eq!(got, best, "{case}");
                    }
                }
            }
        }
    }
    let trip = Trip {
        tiles: 10,
        cost: 10 * MOVE_COST_PLAIN,
    };
    let unit = Part::Work.cost() + Part::Carry.cost() + Part::Move.cost();
    assert_eq!(upgrader_plan(0, trip, 1000, 3, None), None, "no budget");
    assert_eq!(upgrader_plan(1000, trip, 1000, 0, None), None, "no post");
    assert_eq!(
        upgrader_plan(1000, trip, unit - 1, 3, None),
        None,
        "too little energy"
    );
    assert_eq!(
        upgrader_plan(u64::from(unit), trip, 1000, 3, None),
        None,
        "a budget that only pays for a body"
    );
    assert!(upgrader_plan(u64::from(unit) + 1, trip, unit, 3, None).is_some());
}

/// The budget the scenario room's plan holds at RCL5 (one source sent by links, one hauled), from
/// constants: both sources' yield over a creep life (`yield_per_life`), less the links' loss on the
/// sent one (`link_loss`, LINK_LOSS_RATIO); and every other creep's body: its miners and haulers as
/// their plans give them, and `min_workers` workers of the opening plan's capped body (at most
/// `upgrading.worker_repeats` WORK parts). Without a miner nothing is counted.
/// Uses: SOURCE_ENERGY_CAPACITY, CREEP_LIFE_TIME, ENERGY_REGEN_TIME, LINK_LOSS_RATIO, BODYPART_COST.
#[test]
fn the_budget_counts_the_flowing_sources_less_every_other_body() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(8000);
    let room = w.room;
    let (_, plan, stats) = w.upgrading.room_states().next().expect("room");
    let plan = plan.expect("a plan").clone();
    assert!(stats.takes);
    let mine = w.mining.plan(room).expect("mining plan");
    let haul = w.hauling.plan(room).expect("hauling plan");
    assert_eq!(
        mine.sources.iter().filter(|s| s.linked).count(),
        1,
        "one sent"
    );
    let y = yield_per_life(SOURCE_ENERGY_CAPACITY);
    assert_eq!(plan.delivered, 2 * y - u64::from(link_loss(y as u32)));
    let miners: u64 = mine
        .sources
        .iter()
        .filter_map(|s| s.body)
        .map(|b| u64::from(b.cost()))
        .sum();
    let haulers: u64 = haul
        .sources
        .iter()
        .filter_map(|s| s.body.map(|b| u64::from(b.cost()) * u64::from(s.want)))
        .sum();
    // The workers' body as the opening solved it while the upgraders take (P3.6): at most
    // `worker_repeats` WORK parts.
    let worker = w
        .opening
        .plan(room)
        .and_then(|p| p.capped)
        .expect("a capped worker");
    assert!(count(&worker, Part::Work) <= UPGRADING_MMO.worker_repeats);
    let workers = u64::from(OPENING_MMO.min_workers) * u64::from(worker.cost());
    assert_eq!(plan.upkeep, miners + haulers + workers);
    assert_eq!(plan.sources.len(), 2);
    let sources = [
        (place(room, w.sources[0].at), SOURCE_ENERGY_CAPACITY),
        (place(room, w.sources[1].at), SOURCE_ENERGY_CAPACITY),
    ];
    let ways = Ways {
        container: true,
        link: true,
        crew: Crew {
            miners: 2,
            haulers: 1,
        },
    };
    assert_eq!(
        delivered(Some(mine), Some(haul), &sources, ways),
        plan.delivered
    );
    let none = Ways {
        crew: Crew::default(),
        ..ways
    };
    assert_eq!(
        delivered(Some(mine), Some(haul), &sources, none),
        0,
        "no miner"
    );
    assert!(delivering(Some(mine), Some(haul), none).is_empty());
    let unhauled = Ways {
        crew: Crew {
            miners: 2,
            haulers: 0,
        },
        ..ways
    };
    assert_eq!(
        delivered(Some(mine), Some(haul), &sources, unhauled),
        y - u64::from(link_loss(y as u32)),
        "no hauler: only the sent source"
    );
}

/// Uses: UPGRADE_CONTROLLER_POWER, CARRY_CAPACITY, `RECEIVER_RANGE`. A room at RCL5 (its controller
/// container, then its links): the plan's upgraders come, WORK parts first and `upgrader_carry`
/// CARRY parts, and stand each on its own post, a walkable tile within upgrading range of the
/// controller next to a store; the posts are every such tile but the `free_tiles` nearest the spawn;
/// the opening keeps `min_workers` workers of at most `worker_repeats` WORK parts; no intent fails.
/// Uses also MAX_CREEP_SIZE, CREEP_SPAWN_TIME, SOURCE_ENERGY_CAPACITY, ENERGY_REGEN_TIME: the count
/// is read once a replacement in progress has spawned and settled (no pre-spawn before P5), within
/// the allowance the test derives.
#[test]
fn upgraders_take_the_controllers_energy_from_their_posts() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(9000);
    // No pre-spawn before P5: an upgrader that died of old age is asked for again the next tick and
    // ordered once the spawn's stores hold its body's cost (`EnergyBudget::Capacity`). A fixed tick
    // can fall in that window, or before the replacement settles. Step until the room holds `want`
    // upgraders and one has settled, within an allowance: the longest order that can hold the spawn
    // (`MAX_CREEP_SIZE` parts), the body's cost at the hauled sources' yield (a linked source's
    // energy goes to the controller), and the replacement's spawn and walk there and back.
    let plan_now = |w: &World| {
        w.upgrading
            .room_states()
            .next()
            .and_then(|(_, p, _)| p.cloned())
            .expect("a plan")
    };
    let settled = |w: &World| {
        let p = plan_now(w);
        w.creeps
            .iter()
            .filter(|c| c.identity.role == Role::Upgrader && c.spawning == 0)
            .any(|c| {
                p.trip
                    .and_then(|t| {
                        walk_ticks(t, count(&c.body, Part::Work), count(&c.body, Part::Move))
                    })
                    .is_some_and(|walk| w.time - c.born > 2 * walk + 10)
            })
    };
    let first = plan_now(&w);
    let b = first.body.expect("a body");
    let walk = first
        .trip
        .and_then(|t| walk_ticks(t, count(&b, Part::Work), count(&b, Part::Move)))
        .expect("a walk");
    let hauled = w
        .mining
        .plan(w.room)
        .expect("mining plan")
        .sources
        .iter()
        .filter(|s| !s.linked)
        .count() as u32;
    let refill = (b.cost() * ENERGY_REGEN_TIME).div_ceil(hauled.max(1) * SOURCE_ENERGY_CAPACITY);
    let bound = MAX_CREEP_SIZE * CREEP_SPAWN_TIME + refill + b.spawn_time() + 2 * walk + 10;
    let mut waited = 0;
    while (w.upgraders() as u32) < plan_now(&w).want || !settled(&w) {
        assert!(
            waited < bound,
            "no settled upgrader or one short for {bound} ticks"
        );
        w.step();
        waited += 1;
    }
    assert_eq!(w.failed, 0);
    let room = w.room;
    let (_, plan, stats) = w.upgrading.room_states().next().expect("room");
    let plan = plan.expect("a plan").clone();
    assert!(stats.takes && plan.want >= 1);
    let body = plan.body.expect("a body");
    assert!(body.cost() <= plan.capacity);
    // The posts, by brute force over the room: every walkable tile within 3 of the controller
    // next to a store, off the mining and link tiles, but the `free_tiles` next to the first store
    // nearest the spawn.
    let controller = place(room, w.controller_at);
    let mine = w.mining.plan(room).expect("mining plan");
    let spawn = mine.spawn.expect("a spawn");
    let reserved: Vec<Position> = mine
        .sources
        .iter()
        .flat_map(|s| [s.tile, s.link])
        .chain([mine.controller_link])
        .flatten()
        .collect();
    let mut candidates = Vec::new();
    for x in 1..=48u8 {
        for y in 1..=48u8 {
            let t = tile(x, y);
            let p = place(room, t);
            let near_store = plan.stores.iter().any(|&s| tile_of(s).range(t) <= 1);
            if t.range(w.controller_at) > 3 || w.blocked(t) || !near_store {
                continue;
            }
            if reserved.contains(&p) && !plan.stores.contains(&p) {
                continue;
            }
            assert!(p != controller);
            candidates.push(p);
        }
    }
    let access = tile_of(plan.stores[0]);
    let mut by_access: Vec<Position> = candidates
        .iter()
        .copied()
        .filter(|&p| tile_of(p).range(access) <= 1)
        .collect();
    by_access.sort_by_key(|&p| (tile_of(p).range(tile_of(spawn)), tile_key_of(p)));
    let free: Vec<Position> = by_access
        .iter()
        .copied()
        .take(UPGRADING_MMO.free_tiles as usize)
        .collect();
    assert_eq!(free.len(), UPGRADING_MMO.free_tiles as usize);
    let mut want: Vec<Position> = candidates
        .into_iter()
        .filter(|p| !free.contains(p))
        .collect();
    let mut got = plan.posts.clone();
    want.sort_by_key(|&p| tile_key_of(p));
    got.sort_by_key(|&p| tile_key_of(p));
    assert_eq!(got, want, "the posts");
    assert!(
        free.iter().all(|p| !plan.posts.contains(p)),
        "free next to the store"
    );
    // Its upgraders: the plan's bodies (or an earlier plan's), each settled one on its own post.
    let upgraders: Vec<&SimCreep> = w
        .creeps
        .iter()
        .filter(|c| c.identity.role == Role::Upgrader)
        .collect();
    assert_eq!(upgraders.len() as u32, plan.want);
    let mut seen = Vec::new();
    for c in &upgraders {
        let work = count(&c.body, Part::Work);
        assert_eq!(count(&c.body, Part::Carry), upgrader_carry(work));
        assert!(count(&c.body, Part::Move) >= 1);
        let walk = plan
            .trip
            .and_then(|t| walk_ticks(t, work, count(&c.body, Part::Move)))
            .expect("a walk");
        if c.spawning == 0 && w.time - c.born > 2 * walk + 10 {
            let at = place(room, c.at);
            assert!(plan.posts.contains(&at), "on a post: {:?}", c.at);
            assert!(!seen.contains(&at), "one each");
            seen.push(at);
        }
    }
    assert!(!seen.is_empty());
    // The opening's workers: `min_workers`, small.
    assert_eq!(w.want(), OPENING_MMO.min_workers);
    for c in w.creeps.iter().filter(|c| c.identity.role == Role::Worker) {
        assert!(
            count(&c.body, Part::Work) <= UPGRADING_MMO.worker_repeats,
            "{:?}",
            c.body
        );
    }
}

/// Upgraders pay at every level the scenario room is run at: more upgrade progress from tick 6000
/// to 18000 than with them off, and fewer billed intents at RCL3. Measured 2026-11-17 with P3.6's
/// solved worker bodies, over six starts (the spawn's energy at the start 300 to 50) and ticks 6000
/// to 30000, progress counted across level-ups: RCL3 +4.9% (+3.1% to +5.5%), RCL4 +5.8% (+2.5% to
/// +7.3%), RCL5 +11.8% (+7.7% to +30.7%), RCL6 +4.4% (+3.4% to +6.5%); billed intents −3.0%,
/// +2.0%, +20.2%, +0.8% (this start at RCL4: 67552 against 66320). Before P3.6 (the 1:1:1 pattern):
/// +8.8%, +6.0%, +9.2%, +6.4%; billed intents −9.8%, −7.7%, +10.1%, +1.4%; a fresh room reached
/// RCL4 in 16935 ticks against 17537 and RCL5 in 47156 against 49804.
#[test]
fn upgraders_raise_the_upgrade_rate() {
    let steady = |level: u8, cfg: UpgradingConfig| {
        let mut w = World::new(level, SPAWN_ENERGY_CAPACITY);
        w.upgrading_cfg = cfg;
        w.run(6000);
        let (p, b) = (w.upgraded(), w.billed);
        w.run(12_000);
        assert_eq!(w.failed, 0);
        (w.upgraded() - p, w.billed - b)
    };
    for level in [3, 4, 5, 6] {
        let (with, with_billed) = steady(level, UPGRADING_MMO);
        let (without, without_billed) = steady(level, UPGRADING_OFF);
        assert!(with > without, "RCL{level}: {with} against {without}");
        if level == 3 {
            assert!(
                with_billed < without_billed,
                "RCL{level}: {with_billed} against {without_billed}"
            );
        }
    }
}

/// With `upgrading.enabled` off no upgrader is ordered (none on any tick, spawning ones counted), the
/// view never gives the controller's energy away, and the workers are never solved as under
/// upgraders: `worker_repeats` does not cap their WORK parts (their proportions come from the
/// solver, P3.6).
#[test]
fn upgraders_off_order_none() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.upgrading_cfg = UPGRADING_OFF;
    let (mut seen, mut workers) = (Vec::new(), Vec::new());
    for _ in 0..6000 {
        w.step();
        assert!(!w.upgrading.view(w.room).takes);
        assert_eq!(w.upgraders(), 0);
        let (_, plan, _, _) = w.opening.room_states().next().expect("room");
        assert!(plan.is_none_or(|p| !p.upgraders));
        for c in &w.creeps {
            if !seen.contains(&c.key) {
                seen.push(c.key);
                if c.identity.role == Role::Worker {
                    workers.push(c.body);
                }
            }
        }
    }
    assert!(
        workers
            .iter()
            .any(|b| b.count(Part::Work) > UPGRADING_MMO.worker_repeats as usize)
    );
    assert_eq!(w.failed, 0);
}

/// With mining off no energy reaches the controller, so no upgrader is ordered and the workers keep
/// upgrading (a room whose stores stand but whose miners never come must not shrink its workers).
#[test]
fn a_room_without_miners_keeps_its_workers() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.mining_cfg = MiningConfig {
        enabled: false,
        ..MINING_MMO
    };
    for _ in 0..6000 {
        w.step();
        assert!(!w.upgrading.view(w.room).takes);
    }
    assert_eq!(w.upgraders(), 0);
    assert!(w.want() > OPENING_MMO.min_workers);
    assert_eq!(w.failed, 0);
}

/// Two runs that once gave a billed no-op: a worker's fill (no amount) took the room a hauler had
/// claimed in the same extension (RCL4 with 2-repeat workers, a start of 50 energy); an upgrader's
/// withdraw (no amount) emptied the controller link before a worker's claimed take (RCL6, a start of
/// 200). The workers now leave a hauler's fills alone, and an upgrader withdraws only what it claims.
#[test]
fn workers_haulers_and_upgraders_never_take_the_same_energy() {
    for (level, energy, ticks) in [(4u8, 50, 11_000), (6, 200, 26_000)] {
        let mut w = World::new(level, energy);
        w.run(ticks);
        assert_eq!(w.failed, 0, "RCL{level}");
        assert!(w.upgraders() >= 1);
    }
}

/// After a global reset (upgrading's state lost) the upgraders take posts again, one each, those on
/// a post the one they stand on (none moves), and go on upgrading.
#[test]
fn a_reset_keeps_the_upgraders_at_their_posts() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(8000);
    let (_, plan, _) = w.upgrading.room_states().next().expect("room");
    let posts = plan.expect("a plan").posts.clone();
    let settled: Vec<(u128, Tile)> = w
        .creeps
        .iter()
        .filter(|c| c.identity.role == Role::Upgrader && c.spawning == 0)
        .filter(|c| posts.contains(&place(w.room, c.at)))
        .map(|c| (c.key, c.at))
        .collect();
    assert!(settled.len() >= 2, "two upgraders at RCL5");
    // Two of them swap posts: after the reset each keeps the one it stands on, whatever the ids.
    let (a, b) = (settled[0], settled[1]);
    for c in w.creeps.iter_mut() {
        if c.key == a.0 {
            c.at = b.1;
        } else if c.key == b.0 {
            c.at = a.1;
        }
    }
    let settled: Vec<(u128, Tile)> = [(a.0, b.1), (b.0, a.1)]
        .into_iter()
        .chain(settled[2..].iter().copied())
        .collect();
    w.upgrading = Upgrading::default();
    let before = w.progress;
    w.step();
    for &(key, at) in &settled {
        let c = w.creeps.iter().find(|c| c.key == key).expect("alive");
        assert_eq!(c.at, at, "it keeps the post it stands on");
    }
    w.run(100);
    let (_, plan, stats) = w.upgrading.room_states().next().expect("room");
    let posts = plan.expect("a plan").posts.clone();
    assert!(stats.upgrades >= 1);
    assert!(w.progress > before);
    let mut seen = Vec::new();
    for c in w
        .creeps
        .iter()
        .filter(|c| c.identity.role == Role::Upgrader && c.spawning == 0)
    {
        let at = place(w.room, c.at);
        if posts.contains(&at) {
            assert!(!seen.contains(&at), "one each");
            seen.push(at);
        }
    }
    assert!(!seen.is_empty());
    assert_eq!(w.failed, 0);
}

/// With the Standard tier off the Critical `upgrading` job still plans and orders upgraders (one
/// that dies is replaced), and none of them withdraws, upgrades or moves (that is the Standard
/// job's): the controller gains nothing and the settled ones stay put.
#[test]
fn critical_upgrading_goes_on_with_the_standard_tier_off() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(6000);
    w.standard = false;
    w.log_takes = true;
    let mut settled: Vec<(u128, Tile)> = w
        .creeps
        .iter()
        .filter(|c| c.identity.role == Role::Upgrader && c.spawning == 0)
        .map(|c| (c.key, c.at))
        .collect();
    assert!(!settled.is_empty());
    // One dies: its replacement is ordered all the same.
    let (gone, _) = settled.remove(0);
    w.creeps.retain(|c| c.key != gone);
    let ordered = w.spawned.len();
    let progress = w.progress;
    w.run(300);
    assert!(
        w.spawned[ordered..]
            .iter()
            .any(|(_, b)| b.count(Part::Work) > b.count(Part::Carry)),
        "a replacement upgrader"
    );
    let (_, plan, stats) = w.upgrading.room_states().next().expect("room");
    assert!(stats.takes);
    assert_eq!(stats.upgraders, plan.expect("a plan").want);
    assert_eq!(w.progress, progress, "no upgrade");
    assert!(
        w.takes_log.iter().all(|t| t.1 != Role::Upgrader),
        "no upgrader withdraw"
    );
    for c in w
        .creeps
        .iter()
        .filter(|c| c.identity.role == Role::Upgrader)
    {
        if let Some(&(_, at)) = settled.iter().find(|b| b.0 == c.key) {
            assert_eq!(c.at, at, "no Standard move");
        }
    }
    assert_eq!(w.failed, 0);
}

/// Uses: MAX_CREEP_SIZE, CREEP_SPAWN_TIME (`upgrading.crew_grace_ticks`). With upgraders at work,
/// the room's only hauler dying, then both its miners, hands nothing back: until the replacements
/// show (the tick after their orders), the upgraders still take the controller's energy, both plans
/// stay as they were, and no worker beyond `min_workers` is wanted.
#[test]
fn a_crew_gap_keeps_the_upgraders_taking() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(8000);
    let room = w.room;
    assert_eq!(w.haulers(), 1, "one hauled source at RCL5");
    let before = w.upgrading.room_states().next().expect("room").1.cloned();
    assert!(before.is_some() && w.upgrading.view(room).takes);
    let workers_spawned = |w: &World| {
        w.spawned
            .iter()
            .filter(|(_, b)| b.count(Part::Work) == b.count(Part::Carry))
            .count()
    };
    for role in [Role::Hauler, Role::Miner] {
        w.creeps.retain(|c| c.identity.role != role);
        let spawned = workers_spawned(&w);
        for _ in 0..20 {
            w.step();
            assert!(w.upgrading.view(room).takes, "{role:?} gone at {}", w.time);
            assert_eq!(w.want(), OPENING_MMO.min_workers);
            let now = w.upgrading.room_states().next().expect("room").1.cloned();
            assert_eq!(now, before, "the plan stays");
        }
        assert_eq!(workers_spawned(&w), spawned, "no worker for the gap");
    }
    assert_eq!(w.failed, 0);
}

/// Once mining stops for good (switched off, its miners gone), the grace runs out and the
/// controller's energy goes back to the workers.
#[test]
fn the_crew_grace_runs_out() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(8000);
    assert!(w.upgrading.view(w.room).takes);
    w.mining_cfg = MiningConfig {
        enabled: false,
        ..MINING_MMO
    };
    w.creeps.retain(|c| c.identity.role != Role::Miner);
    w.run(UPGRADING_MMO.crew_grace_ticks);
    assert!(w.upgrading.view(w.room).takes, "within the grace");
    w.run(2);
    assert!(!w.upgrading.view(w.room).takes, "after it");
    assert_eq!(w.failed, 0);
}

/// The plan wanting upgraders hands the controller's energy over only once the room has one
/// (spawning counts): until then the workers keep their sources and their upgrading.
#[test]
fn the_energy_is_handed_over_with_the_first_upgrader() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    let mut waited = 0;
    for _ in 0..8000 {
        w.step();
        let (_, plan, stats) = w.upgrading.room_states().next().expect("room");
        let wants = plan.is_some_and(|p| p.want > 0 && p.body.is_some());
        if stats.upgraders > 0 {
            assert!(wants && stats.takes, "taken with the first one");
            assert!(
                waited > 0,
                "the plan wanted upgraders before one was ordered"
            );
            assert!(w.upgrading.view(w.room).takes);
            return;
        }
        assert!(!stats.takes, "no upgrader yet at {}", w.time);
        assert!(!w.upgrading.view(w.room).takes);
        if wants {
            waited += 1;
        }
    }
    panic!("no upgrader within 8000 ticks");
}

/// Posts stand only around the stores a counted source feeds: with the link network off at RCL5
/// both sources are hauled, so a controller link standing by the controller gets no post of its
/// own, and every post is next to the container.
#[test]
fn posts_stand_only_by_fed_stores() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.link_cfg = LinkConfig {
        enabled: false,
        ..LINK_MMO
    };
    w.run(9000);
    let room = w.room;
    let mine = w.mining.plan(room).expect("mining plan");
    let container = mine.controller_container.expect("a container tile");
    assert!(
        !w.mining.receivers(room).is_empty(),
        "a controller link stands"
    );
    let (_, plan, _) = w.upgrading.room_states().next().expect("room");
    let plan = plan.expect("a plan");
    assert_eq!(plan.stores, vec![container]);
    assert!(!plan.posts.is_empty());
    for &p in &plan.posts {
        assert!(tile_of(p).range(tile_of(container)) <= 1, "{p:?}");
    }
    assert_eq!(w.failed, 0);
    // Both sources sent through links at RCL6: the container is fed by no one and gets no post.
    let mut w = World::new(6, SPAWN_ENERGY_CAPACITY);
    w.link_cfg = LinkConfig {
        source_links: 2,
        ..LINK_MMO
    };
    let mut linked = false;
    for _ in 0..30 {
        w.run(1000);
        linked = w
            .mining
            .plan(w.room)
            .is_some_and(|m| m.sources.iter().all(|s| s.linked));
        if linked {
            break;
        }
    }
    assert!(linked, "both sources linked within 30000 ticks");
    w.run(50);
    let room = w.room;
    let container = w.mining.plan(room).and_then(|m| m.controller_container);
    let (_, plan, _) = w.upgrading.room_states().next().expect("room");
    let plan = plan.expect("a plan");
    assert!(!plan.stores.is_empty());
    assert!(
        container.is_none_or(|c| !plan.stores.contains(&c)),
        "no post by a dry container"
    );
    let links: Vec<Position> = w.mining.receivers(room).iter().map(|r| r.pos).collect();
    assert!(plan.stores.iter().all(|s| links.contains(s)));
    assert_eq!(w.failed, 0);
}

/// Uses: CARRY_CAPACITY (the stores' sizes). Every upgrader withdraw asks for an explicit amount,
/// at most its free room and what its store held at the tick's start less the free room of every
/// worker that took from that store this tick (`Opening::taken`), so whichever the engine runs
/// first, no take finds the store emptied (RCL5 and RCL6 runs: controller links and the container).
#[test]
fn an_upgrader_never_takes_a_workers_claim() {
    for (level, energy) in [(5u8, 300), (6, 200)] {
        let mut w = World::new(level, energy);
        w.log_takes = true;
        w.run(12_000);
        let mut checked = 0;
        for &(time, role, target, asked, free, held) in &w.takes_log {
            if role != Role::Upgrader {
                continue;
            }
            let amount = asked.expect("an explicit amount");
            assert!(
                amount >= 1 && amount <= free,
                "t{time}: {amount} of {free} free"
            );
            let workers: u32 = w
                .takes_log
                .iter()
                .filter(|t| t.0 == time && t.1 == Role::Worker && t.2 == target)
                .map(|t| t.4)
                .sum();
            assert!(
                amount + workers <= held,
                "t{time}: {amount} and the workers' {workers} from {held}"
            );
            checked += 1;
        }
        assert!(checked > 0, "RCL{level}: upgraders withdrew");
        assert_eq!(w.failed, 0);
    }
}

/// An upgrader withdraws from the first store next to it whose energy left covers its free room
/// (the links come first), else from the one with the most: a link with a few energy left does not
/// cost it a small withdraw now and another next tick while the container holds plenty.
#[test]
fn an_upgrader_picks_a_store_that_covers_its_take() {
    use super::upgrading::{Store, pick_store};
    // Any handles: the pick reads only the places and the energy.
    let mut w = World::new(1, SPAWN_ENERGY_CAPACITY);
    w.step();
    let link = w.snap.structures.handles()[0];
    let container = w.snap.objects.handles()[0];
    let room = w.room;
    let at = place(room, tile(25, 10));
    let near_link = (Store::Link(link), place(room, tile(24, 10)), 0u32);
    let near_box = (Store::Container(container), place(room, tile(26, 11)), 0u32);
    let far_box = (
        Store::Container(container),
        place(room, tile(28, 10)),
        CONTAINER_CAPACITY,
    );
    let with = |link: u32, container: u32| {
        [
            (near_link.0, near_link.1, link),
            (near_box.0, near_box.1, container),
            far_box,
        ]
    };
    let free = CARRY_CAPACITY;
    assert_eq!(
        pick_store(&with(free, 500), at, free),
        Some(0),
        "the link covers it"
    );
    assert_eq!(
        pick_store(&with(10, 500), at, free),
        Some(1),
        "the container does"
    );
    assert_eq!(
        pick_store(&with(10, 30), at, free),
        Some(1),
        "neither: the most"
    );
    assert_eq!(
        pick_store(&with(30, 30), at, free),
        Some(0),
        "a tie: the link"
    );
    assert_eq!(pick_store(&with(0, 0), at, free), None, "nothing near");
}

/// With hauling off at RCL5 the linked source alone engages the upgraders; the other source's
/// workers, whose energy the upgraders do not count, keep upgrading with it (they would otherwise
/// sit on full stores), so the room upgrades about as much as with upgraders off (within 5%).
/// Measured 2026-11-17 with P3.6's solved worker bodies (ticks 6000 to 18000, starts of 300 and
/// 50): RCL5 −0.3% and −1.5%, RCL6 +0.8% and +3.5%, with fewer billed intents than the 1:1:1
/// pattern (121262 against 126269 at RCL5 with upgraders). Before P3.6: RCL5 −1.3% and −2.9%,
/// RCL6 +0.6% and +3.3%.
#[test]
fn an_uncounted_sources_workers_keep_upgrading() {
    let run = |cfg: UpgradingConfig| {
        let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
        w.hauling_cfg = HAULING_OFF;
        w.upgrading_cfg = cfg;
        w.run(6000);
        let p = w.upgraded();
        let mut upgrades = 0;
        for _ in 0..12_000 {
            w.step();
            let (_, _, _, stats) = w.opening.room_states().next().expect("room");
            upgrades += stats.tasks[6];
        }
        assert_eq!(w.failed, 0);
        (w.upgraded() - p, upgrades, w.upgrading.view(w.room).takes)
    };
    let (with, upgrades, takes) = run(UPGRADING_MMO);
    let (without, _, _) = run(UPGRADING_OFF);
    assert!(takes, "the linked source engages the upgraders");
    assert!(upgrades > 0, "the workers still upgrade");
    assert!(with * 20 >= without * 19, "{with} against {without}");
}

/// An upgrader withdraws by its own body, not the plan's: each one's WORK parts are taken from the
/// plan's body when it is first seen, and stay when the plan's body changes under it (here the room
/// loses extensions until its capacity pays for no upgrader of the plan's WORK parts, so the plan
/// takes fewer; before
/// P3.6 a larger `worker_repeats` raised the workers' bodies in the budget instead, a lever the
/// solved worker bodies no longer give).
#[test]
fn an_upgrader_keeps_its_own_work_parts() {
    let mut w = World::new(5, SPAWN_ENERGY_CAPACITY);
    w.run(8000);
    let own = |w: &World| -> Vec<(u32, Option<u32>)> {
        w.creeps
            .iter()
            .filter(|c| c.identity.role == Role::Upgrader)
            .filter_map(|c| {
                let h = w.snap.creeps.handle_of(ObjectKey(c.key))?;
                Some((count(&c.body, Part::Work), w.upgrading.work_parts(h)))
            })
            .collect()
    };
    let before = own(&w);
    assert!(!before.is_empty());
    for &(work, seen) in &before {
        assert_eq!(seen, Some(work), "as ordered");
    }
    let plan_work = |w: &World| {
        let (_, plan, _) = w.upgrading.room_states().next().expect("room");
        plan.and_then(|p| p.body).map(|b| count(&b, Part::Work))
    };
    // The room loses extensions until its capacity is below the cheapest upgrader of the plan's
    // WORK parts (those WORK, their `upgrader_carry` CARRY and one MOVE part): the plan must take
    // fewer WORK parts (`upgrader_plan` takes bodies within the capacity). Uses: BODYPART_COST.
    let was = plan_work(&w);
    let work = was.expect("a body");
    let cost =
        work * Part::Work.cost() + upgrader_carry(work) * Part::Carry.cost() + Part::Move.cost();
    while w.room_energy().capacity >= cost {
        let i = w
            .stores
            .iter()
            .rposition(|s| s.kind == StructureType::Extension)
            .expect("an extension to remove");
        w.stores.remove(i);
    }
    w.run(2);
    let now = plan_work(&w);
    assert_ne!(now, was, "the plan's body changed");
    let mut checked = 0;
    for (work, seen) in own(&w) {
        if before.iter().any(|b| b.0 == work) {
            assert_eq!(seen, Some(work), "still its own");
            if Some(work) != now {
                checked += 1;
            }
        }
    }
    assert!(
        checked > 0,
        "an upgrader of the old plan's body was checked"
    );
}

/// Uses: EXTENSION_ENERGY_CAPACITY. No worker transfers into a spawn or extension a hauler's
/// transfer goes into on the same tick: with the miners gone (and none ordered, so the workers fill
/// too and nothing drains the room), every spawn and extension full but one, every hauler holding a
/// fifth of its room, one of them and a full worker next to it, the worker leaves it to the hauler
/// (its no-amount transfer would fill all the room and leave the hauler's billed for nothing).
#[test]
fn a_worker_never_fills_where_a_hauler_transfers() {
    let mut w = World::new(4, SPAWN_ENERGY_CAPACITY);
    w.run(8000);
    w.mining_cfg = MiningConfig {
        enabled: false,
        ..MINING_MMO
    };
    w.creeps.retain(|c| c.identity.role != Role::Miner);
    let target = w
        .stores
        .iter()
        .position(|s| s.kind == StructureType::Extension)
        .expect("an extension");
    let at = w.stores[target].at;
    for (k, s) in w.stores.iter_mut().enumerate() {
        if s.spawn_energy() {
            s.energy = if k == target { 0 } else { s.capacity };
        }
    }
    let mut free_tiles = at
        .neighbors()
        .map(|(_, t)| t)
        .filter(|&t| !w.blocked(t) && !w.creeps.iter().any(|c| c.at == t));
    let (ht, wt) = (
        free_tiles.next().expect("a tile"),
        free_tiles.next().expect("another"),
    );
    let part = extension_energy_capacity(4) / 5;
    for c in w
        .creeps
        .iter_mut()
        .filter(|c| c.identity.role == Role::Hauler)
    {
        c.energy = part;
    }
    let hauler = w
        .creeps
        .iter()
        .position(|c| c.identity.role == Role::Hauler && c.spawning == 0)
        .expect("a hauler");
    let worker = w
        .creeps
        .iter()
        .position(|c| c.identity.role == Role::Worker && c.spawning == 0)
        .expect("a worker");
    w.creeps[hauler].at = ht;
    w.creeps[worker].at = wt;
    w.creeps[worker].energy = w.creeps[worker].capacity();
    w.log_takes = true;
    w.step();
    let into = |role: Role| {
        w.fills_log
            .iter()
            .filter(|f| f.1 == role)
            .map(|f| f.2)
            .collect::<Vec<_>>()
    };
    assert!(!into(Role::Hauler).is_empty(), "the hauler filled");
    for t in into(Role::Worker) {
        assert!(
            !into(Role::Hauler).contains(&t),
            "a worker and a hauler into {t:?}"
        );
    }
    assert_eq!(w.failed, 0);
}

// ---------------------------------------------------------------------------------------------
// The plan path (P4.3, ADR-026)

/// The placement's plan path (`Placement::plan_room` with [`PlanView::Ready`]) on a small bench:
/// one owned room's snapshot, movement's terrain and scans, and a hand-made plan, with no world
/// model. The bench plays the game side: an accepted site stands from the next tick (V204), and
/// an accepted destroy is either done by the next tick or dropped by the processor (V251), as each
/// test says.
mod plan_path {
    use super::{ROOM, place, tile};
    use crate::config::{MOVEMENT_MMO, PLANNER_MMO, PlannerConfig};
    use crate::economy::placement::{
        APPEAR_TICKS, DestroyOrder, DestroyTarget, LOST_MAX, Placement, PlanView, SiteOrder,
    };
    use crate::intents::Outcome;
    use crate::movement::{
        Footprint, Movement, TerrainGrid, Tile, site_footprint, structure_footprint,
    };
    use crate::planner::build::Census;
    use crate::planner::job::{Facts, Plan};
    use crate::planner::rules::{allowed, earliest_level};
    use crate::planner::{Item, Layout, Roles};
    use crate::scheduler::Commit;
    use crate::snapshot::{
        Controller, CreepRow, ObjectKey, ObjectKind, ObjectRow, ObjectStatic, Snapshot,
        StructureStatic, TickContext,
    };
    use rustedscreep_consts::{
        ErrorCode, ROOM_SIZE, RoomName, SOURCE_ENERGY_CAPACITY, StructureType, Terrain,
    };

    use StructureType::{Container, Extension, Link, Road, Spawn, Storage, Tower};

    const CONTROLLER_KEY: u128 = 1;
    const SPAWN_KEY: u128 = 2;
    const SOURCE_KEY: u128 = 3;
    const OWN_CREEP_KEY: u128 = 4;
    const HOSTILE_KEY: u128 = 5;

    /// The plan's anchor: the room's standing spawn.
    fn anchor() -> Tile {
        tile(25, 25)
    }

    fn controller() -> Tile {
        tile(25, 8)
    }

    fn source() -> Tile {
        tile(10, 10)
    }

    /// The planned storage's tile, next to the anchor's ring.
    fn storage_slot() -> Tile {
        tile(27, 25)
    }

    fn item(kind: StructureType, t: Tile, level: u8) -> Item {
        Item {
            kind,
            tile: t,
            level,
        }
    }

    /// The first controller level that allows a `kind`. Uses: `CONTROLLER_STRUCTURES` (V234).
    fn level_of(kind: StructureType) -> u8 {
        earliest_level(kind, 0).expect("allowed at some level")
    }

    /// Whether the room's plan is ready or still being built, or the interim placement runs (the
    /// planner off, or no valid plan).
    #[derive(Clone, Copy, Debug)]
    enum View {
        Ready,
        Waiting,
        Off,
        Fallback,
    }

    struct Bench {
        room: RoomName,
        time: u32,
        level: u8,
        raw: Vec<u8>,
        /// Own structures, the controller and the anchor spawn first: key, type, tile.
        structures: Vec<(u128, StructureType, Tile)>,
        /// Own construction sites: key, type, tile.
        sites: Vec<(u128, StructureType, Tile)>,
        /// Built roads: they have no owner, so only movement's scan shows them, never the
        /// snapshot.
        roads: Vec<Tile>,
        hostile: bool,
        next_key: u128,
        snap: Snapshot,
        movement: Movement,
        placement: Placement,
        plan: Plan,
        pcfg: PlannerConfig,
        /// `opening.place_extensions`, for the interim path.
        extensions: bool,
        container_needs: Vec<crate::economy::ContainerNeed>,
        link_needs: Vec<crate::economy::LinkNeed>,
    }

    impl Bench {
        /// An owned room at `level`: walls on the border ring and plain inside, the controller, the
        /// anchor spawn, one source and one own creep (movement tracks the rooms its creeps are
        /// in). Its plan holds the anchor spawn and `items`, with `roles`.
        fn new(level: u8, items: Vec<Item>, roles: Roles) -> Self {
            let wall = Terrain::Wall as u8;
            let edge = ROOM_SIZE - 1;
            let raw: Vec<u8> = (0..ROOM_SIZE)
                .flat_map(|y| {
                    (0..ROOM_SIZE).map(move |x| {
                        if x == 0 || y == 0 || x == edge || y == edge {
                            wall
                        } else {
                            0
                        }
                    })
                })
                .collect();
            let terrain = TerrainGrid::from_row_major(&raw).expect("terrain");
            let mut all = vec![item(Spawn, anchor(), level_of(Spawn))];
            all.extend(items);
            let mut layout = Layout {
                items: all,
                roles,
                needs: Vec::new(),
            };
            layout.sort();
            let plan = Plan {
                layout,
                facts: Facts {
                    terrain,
                    controller: controller(),
                    sources: vec![source()],
                    mineral: None,
                    spawns: vec![anchor()],
                    anchors: vec![anchor()],
                    standing: Vec::new(),
                    own: vec![(Spawn, anchor())],
                    prior: None,
                },
                anchor: anchor(),
                ready_at: 0,
                steps: 0,
            };
            Self {
                room: ROOM.parse().expect("room"),
                time: 10_000,
                level,
                raw,
                structures: vec![
                    (CONTROLLER_KEY, StructureType::Controller, controller()),
                    (SPAWN_KEY, Spawn, anchor()),
                ],
                sites: Vec::new(),
                roads: Vec::new(),
                hostile: false,
                next_key: 100,
                snap: Snapshot::default(),
                movement: Movement::default(),
                placement: Placement::default(),
                plan,
                pcfg: PLANNER_MMO,
                extensions: false,
                container_needs: Vec::new(),
                link_needs: Vec::new(),
            }
        }

        fn key(&mut self) -> u128 {
            self.next_key += 1;
            self.next_key
        }

        /// Moves the room's controller to `t`, in the plan's facts and in the snapshot.
        fn move_controller(&mut self, t: Tile) {
            self.plan.facts.controller = t;
            for s in &mut self.structures {
                if s.0 == CONTROLLER_KEY {
                    s.2 = t;
                }
            }
        }

        /// An own structure stands from the next snapshot.
        fn build(&mut self, kind: StructureType, t: Tile) {
            let key = self.key();
            self.structures.push((key, kind, t));
        }

        /// An own site stands from the next snapshot.
        fn site(&mut self, kind: StructureType, t: Tile) {
            let key = self.key();
            self.sites.push((key, kind, t));
        }

        fn take_snapshot(&mut self) {
            let room = self.room;
            let snap = &mut self.snap;
            snap.begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
            snap.rooms
                .push(room, Controller::Mine { level: self.level });
            for &(key, kind, t) in &self.structures {
                let pos = place(room, t);
                snap.structures
                    .push(ObjectKey(key), || Ok(StructureStatic { kind, pos }))
                    .expect("structure");
            }
            let creep = |at: Tile, my: bool| CreepRow {
                pos: place(room, at),
                my,
                spawning: false,
                fatigue: 0,
            };
            snap.creeps
                .push(ObjectKey(OWN_CREEP_KEY), creep(tile(40, 40), true))
                .expect("own creep");
            if self.hostile {
                snap.creeps
                    .push(ObjectKey(HOSTILE_KEY), creep(tile(30, 40), false))
                    .expect("hostile creep");
            }
            let mut object = |key: u128, kind: ObjectKind, t: Tile, row: ObjectRow| {
                let pos = place(room, t);
                snap.objects
                    .push(ObjectKey(key), || Ok(ObjectStatic { kind, pos }), row)
                    .expect("object");
            };
            object(
                SOURCE_KEY,
                ObjectKind::Source,
                source(),
                ObjectRow {
                    amount: SOURCE_ENERGY_CAPACITY,
                    total: SOURCE_ENERGY_CAPACITY,
                    hits: 0,
                },
            );
            for &(key, kind, t) in &self.sites {
                let row = ObjectRow {
                    amount: 0,
                    total: kind.construction_cost().unwrap_or(0),
                    hits: 0,
                };
                object(key, ObjectKind::Site(kind), t, row);
            }
            snap.finish();
        }

        /// Movement's scan of the room as the glue reads it: every structure's footprint (the
        /// built roads' too) and the own sites' (`structure_footprint`, `site_footprint`).
        fn footprints(&self) -> Vec<(Tile, Footprint)> {
            let built = self
                .structures
                .iter()
                .filter_map(|&(_, kind, t)| structure_footprint(kind, true, false).map(|f| (t, f)));
            let roads = self
                .roads
                .iter()
                .filter_map(|&t| structure_footprint(Road, false, false).map(|f| (t, f)));
            let sited = self
                .sites
                .iter()
                .filter_map(|&(_, kind, t)| site_footprint(kind).map(|f| (t, f)));
            built.chain(roads).chain(sited).collect()
        }

        /// One tick's decide step: the snapshot, movement's tick (the terrain on the first, and a
        /// scan when `scan`), then the Optional placement job. Returns the tick's orders, which
        /// stay in the placement for the act step.
        fn step(&mut self, view: View, scan: bool) -> (Vec<SiteOrder>, Vec<DestroyOrder>) {
            self.time += 1;
            self.take_snapshot();
            self.movement.begin(&self.snap, &MOVEMENT_MMO);
            if self.movement.room_terrain(self.room).is_none() {
                assert!(self.movement.set_terrain(self.room, &self.raw), "terrain");
            }
            if scan {
                let footprints = self.footprints();
                self.movement.set_objects(self.room, footprints);
            }
            self.placement.begin(&self.snap);
            let mut commit = Commit::default();
            let plan = match view {
                View::Ready => PlanView::Ready(&self.plan),
                View::Waiting => PlanView::Waiting,
                View::Off => PlanView::Off,
                View::Fallback => PlanView::Fallback,
            };
            for unit in 0..self.placement.room_count() {
                self.placement
                    .plan_room(
                        unit,
                        &self.snap,
                        &mut self.movement,
                        &MOVEMENT_MMO,
                        self.extensions,
                        &self.container_needs,
                        &self.link_needs,
                        plan,
                        &self.pcfg,
                        &mut commit,
                    )
                    .expect("placement");
            }
            (
                self.placement.orders().to_vec(),
                self.placement.destroys().to_vec(),
            )
        }

        /// The act step for this tick's sites: each call returns `outcome`; an accepted site
        /// stands from the next tick (V204).
        fn act_sites(&mut self, outcome: Outcome) {
            let orders = self.placement.orders().to_vec();
            for (i, o) in orders.iter().enumerate() {
                self.placement.record(i, outcome);
                if outcome == Outcome::Ok {
                    self.site(o.kind, o.tile);
                }
            }
        }

        /// The act step for this tick's destroys: each call returns `outcome`; an accepted one is
        /// gone by the next tick when `done`, else the processor dropped it (V251).
        fn act_destroys(&mut self, outcome: Outcome, done: bool) {
            let orders = self.placement.destroys().to_vec();
            for (i, d) in orders.iter().enumerate() {
                self.placement.record_destroy(i, outcome);
                if outcome == Outcome::Ok && done {
                    self.structures
                        .retain(|&(_, k, t)| !(k == d.kind && t == d.tile));
                }
            }
        }

        /// A tick with a scan and the plan ready, every call accepted and every destroy done.
        fn tick(&mut self) -> (Vec<SiteOrder>, Vec<DestroyOrder>) {
            let out = self.step(View::Ready, true);
            self.act_sites(Outcome::Ok);
            self.act_destroys(Outcome::Ok, true);
            out
        }

        fn looks(&self) -> u32 {
            self.placement.totals().looks
        }

        fn census(&self) -> Option<Census> {
            self.placement.census(self.room).copied()
        }

        fn site_order(&self, kind: StructureType, t: Tile) -> SiteOrder {
            SiteOrder {
                room: self.room,
                room_row: 0,
                tile: t,
                kind,
            }
        }

        /// The destroy of the own `kind` on `t`, by its row in this tick's snapshot.
        fn destroy_of(&self, kind: StructureType, t: Tile) -> DestroyOrder {
            let st = &self.snap.structures;
            let at = place(self.room, t);
            let row = st
                .kinds()
                .iter()
                .zip(st.positions())
                .position(|(&k, p)| k == kind && *p == at)
                .expect("it stands");
            DestroyOrder {
                room: self.room,
                tile: t,
                kind,
                target: DestroyTarget::Structure(row),
            }
        }
    }

    /// A room at `level` whose planned storage tile holds an own off-plan `blocker`, under `pcfg`,
    /// with `extra` planned items.
    fn storage_bench(
        level: u8,
        blocker: StructureType,
        pcfg: PlannerConfig,
        extra: Vec<Item>,
    ) -> Bench {
        let mut items = vec![item(Storage, storage_slot(), level_of(Storage))];
        items.extend(extra);
        let mut b = Bench::new(level, items, Roles::default());
        b.pcfg = pcfg;
        b.build(blocker, storage_slot());
        b
    }

    /// The storage's gate on (the user's choice keeps it off on the MMO until P5).
    fn storage_on() -> PlannerConfig {
        PlannerConfig {
            storage: true,
            ..PLANNER_MMO
        }
    }

    fn roads() -> [Tile; 2] {
        [tile(22, 22), tile(23, 22)]
    }

    fn road_items() -> Vec<Item> {
        roads()
            .iter()
            .map(|&t| item(Road, t, level_of(Road)))
            .collect()
    }

    /// A free planned extension due at the room's level gets a site from the plan, and the look's
    /// census counts the spawn built and the extension missing, then sited.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`: the room is at the first level
    /// that allows an extension).
    #[test]
    fn a_free_due_item_gets_a_planned_site() {
        let level = level_of(Extension);
        let slot = tile(27, 25);
        let mut b = Bench::new(level, vec![item(Extension, slot, level)], Roles::default());
        let (orders, destroys) = b.tick();
        assert_eq!(orders, vec![b.site_order(Extension, slot)]);
        assert!(destroys.is_empty());
        let t = *b.placement.totals();
        assert_eq!(
            (
                t.picked,
                t.planned,
                t.containers,
                t.links,
                t.looks,
                t.destroys
            ),
            (1, 1, 0, 0, 1, 0)
        );
        let due = Census {
            due: 2,
            built: 1,
            ..Census::default()
        };
        assert_eq!(b.census(), Some(Census { missing: 1, ..due }));
        // The site stands on the next tick (V204): placed, sited, nothing more to do.
        let (orders, destroys) = b.tick();
        assert!(orders.is_empty() && destroys.is_empty());
        assert_eq!(b.placement.totals().placed, 1);
        assert_eq!(b.census(), Some(Census { sited: 1, ..due }));
    }

    /// An own off-plan extension on the tile of a planned storage that is due and placed (its gate
    /// on) is destroyed; never with the storage's gate off, never a spawn, never with a hostile
    /// creep in the room, never with replacing off.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level` and `allowed`: the storage's first
    /// level, which allows the extension too; the first level that allows two spawns).
    #[test]
    fn an_off_plan_blocker_is_destroyed_only_when_allowed() {
        let level = level_of(Storage);
        assert!(allowed(Extension, level) > 0);
        let mut b = storage_bench(level, Extension, storage_on(), Vec::new());
        let (orders, destroys) = b.step(View::Ready, true);
        assert!(orders.is_empty());
        assert_eq!(destroys, vec![b.destroy_of(Extension, storage_slot())]);
        assert_eq!(b.placement.totals().destroys, 1);

        // The storage's gate off (the MMO's default): nothing is due on the tile.
        const { assert!(!PLANNER_MMO.storage) };
        let mut b = storage_bench(level, Extension, PLANNER_MMO, Vec::new());
        let (orders, destroys) = b.step(View::Ready, true);
        assert!(orders.is_empty() && destroys.is_empty());

        // A spawn is never destroyed (at a level that allows a second one).
        let two = earliest_level(Spawn, 1).expect("two spawns at some level");
        let mut b = storage_bench(two.max(level), Spawn, storage_on(), Vec::new());
        let (orders, destroys) = b.step(View::Ready, true);
        assert!(orders.is_empty() && destroys.is_empty());
        assert_eq!(b.census().map(|c| c.blocked), Some(1));

        // Not with a hostile creep in the room; at once when it leaves.
        let mut b = storage_bench(level, Extension, storage_on(), Vec::new());
        b.hostile = true;
        let (orders, destroys) = b.step(View::Ready, true);
        assert!(orders.is_empty() && destroys.is_empty());
        assert_eq!(b.census().map(|c| c.blocked), Some(1));
        b.hostile = false;
        let (_, destroys) = b.step(View::Ready, true);
        assert_eq!(destroys, vec![b.destroy_of(Extension, storage_slot())]);

        // Not with replacing off.
        let pcfg = PlannerConfig {
            replace: false,
            ..storage_on()
        };
        let mut b = storage_bench(level, Extension, pcfg, Vec::new());
        let (orders, destroys) = b.step(View::Ready, true);
        assert!(orders.is_empty() && destroys.is_empty());
        assert_eq!(b.placement.totals().destroys, 0);
    }

    /// `ERR_BUSY` from a destroy (a hostile creep or power creep the snapshot may not see, V248)
    /// holds the room's destroys for `verify_ticks` while its sites still go out; the destroy comes
    /// again on the tick the hold ends.
    ///
    /// Uses: `PLANNER_MMO.verify_ticks`, `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn a_busy_destroy_holds_destroys_but_not_sites() {
        let level = level_of(Storage);
        let mut b = storage_bench(level, Extension, storage_on(), road_items());
        let verify = b.pcfg.verify_ticks;
        assert!(
            u32::try_from(roads().len()).expect("small") + 1 < verify,
            "the hold outlasts the road sites"
        );
        let (orders, destroys) = b.step(View::Ready, true);
        assert!(
            orders.is_empty(),
            "the storage's destroy goes before the roads"
        );
        assert_eq!(destroys, vec![b.destroy_of(Extension, storage_slot())]);
        b.act_destroys(Outcome::Failed(ErrorCode::Busy), false);
        let busy_at = b.time;
        let mut sited = Vec::new();
        while b.time + 1 < busy_at + verify {
            let (orders, destroys) = b.tick();
            assert!(destroys.is_empty(), "held at {}", b.time);
            sited.extend(orders.iter().map(|o| (o.kind, o.tile)));
        }
        let want: Vec<_> = roads().iter().map(|&t| (Road, t)).collect();
        assert_eq!(sited, want, "the sites went on meanwhile");
        let (orders, destroys) = b.step(View::Ready, true);
        assert_eq!(b.time, busy_at + verify);
        assert!(orders.is_empty());
        assert_eq!(destroys, vec![b.destroy_of(Extension, storage_slot())]);
        let t = *b.placement.totals();
        assert_eq!((t.destroys, t.destroyed, t.lost), (2, 0, 0));
    }

    /// An accepted destroy whose structure still stands `APPEAR_TICKS` later (the processor
    /// dropped it, V251) counts as dropped, apart from the sites' `lost`; after `LOST_MAX` such
    /// drops the room gets no more destroys, but its sites still go out, and nothing is placed
    /// while a destroy is pending.
    ///
    /// Uses: `APPEAR_TICKS`, `LOST_MAX`, `PLANNER_MMO.verify_ticks`, `CONTROLLER_STRUCTURES`
    /// (through `earliest_level`).
    #[test]
    fn dropped_destroys_stop_at_lost_max_and_sites_go_on() {
        let level = level_of(Storage);
        let mut b = storage_bench(level, Extension, storage_on(), road_items());
        for dropped in 0..LOST_MAX {
            let (orders, destroys) = b.step(View::Ready, true);
            assert_eq!(b.placement.totals().destroys_dropped, dropped);
            assert_eq!(b.placement.totals().lost, 0);
            assert!(orders.is_empty());
            assert_eq!(destroys, vec![b.destroy_of(Extension, storage_slot())]);
            b.act_destroys(Outcome::Ok, false);
            for _ in 1..APPEAR_TICKS {
                let (orders, destroys) = b.tick();
                assert!(
                    orders.is_empty() && destroys.is_empty(),
                    "pending at {}",
                    b.time
                );
            }
        }
        // The last loss is judged on this tick: the roads go on, the destroys do not.
        let (orders, destroys) = b.tick();
        let t = *b.placement.totals();
        assert_eq!(
            (t.destroys_dropped, t.lost, t.destroys, t.destroyed),
            (LOST_MAX, 0, LOST_MAX, LOST_MAX)
        );
        assert!(destroys.is_empty());
        assert_eq!(orders, vec![b.site_order(Road, roads()[0])]);
        let mut sited = vec![roads()[0]];
        for _ in 0..=b.pcfg.verify_ticks {
            let (orders, destroys) = b.tick();
            assert!(destroys.is_empty(), "no destroy at {}", b.time);
            sited.extend(orders.iter().map(|o| o.tile));
        }
        assert_eq!(sited, roads().to_vec());
        let t = *b.placement.totals();
        assert_eq!(
            (t.destroys_dropped, t.lost, t.destroys, t.refused),
            (LOST_MAX, 0, LOST_MAX, 0)
        );
        assert_eq!(
            b.census(),
            Some(Census {
                due: 4,
                built: 1,
                sited: 2,
                blocked: 1,
                ..Census::default()
            })
        );
    }

    /// A quiet room: once a look finds nothing to do, no look is made while the snapshot is
    /// unchanged until `verify_ticks` pass; a new site, or a level change, looks again at once.
    ///
    /// Uses: `PLANNER_MMO.verify_ticks`, `CONTROLLER_STRUCTURES` (through `earliest_level` and
    /// `allowed`: the first level with extensions, and the next one that allows more).
    #[test]
    fn a_quiet_room_looks_again_on_a_change_or_after_verify_ticks() {
        let first = level_of(Extension);
        let next = earliest_level(Extension, allowed(Extension, first)).expect("more later");
        let (built, later) = (tile(27, 25), tile(23, 25));
        let items = vec![item(Extension, built, first), item(Extension, later, next)];
        let mut b = Bench::new(first, items, Roles::default());
        b.build(Extension, built);
        let (orders, _) = b.tick();
        assert!(orders.is_empty());
        assert_eq!(b.looks(), 1);
        let idle = b.time;
        let verify = b.pcfg.verify_ticks;
        while b.time + 1 < idle + verify {
            let (orders, destroys) = b.tick();
            assert!(orders.is_empty() && destroys.is_empty());
            assert_eq!(b.looks(), 1, "quiet at {}", b.time);
        }
        b.tick();
        assert_eq!(b.time, idle + verify);
        assert_eq!(b.looks(), 2, "verify_ticks passed");
        b.tick();
        assert_eq!(b.looks(), 2);
        // A site appears (off the plan, a manual one): a look at once, then quiet again.
        b.site(Road, tile(30, 30));
        b.tick();
        assert_eq!(b.looks(), 3);
        b.tick();
        assert_eq!(b.looks(), 3);
        // The level rises: a look at once, and the next level's extension goes out.
        b.level = next;
        let (orders, _) = b.tick();
        assert_eq!(b.looks(), 4);
        assert_eq!(orders, vec![b.site_order(Extension, later)]);
    }

    /// A site that goes (built) changes what only movement's scan shows (roads have no owner, so
    /// the snapshot does not hold them): no look until movement has a scan at or after that tick;
    /// a scan on the change's own tick counts.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn a_built_site_waits_for_a_scan() {
        let level = level_of(Extension);
        let slot = tile(27, 25);
        let mut b = Bench::new(level, vec![item(Extension, slot, level)], Roles::default());
        let (orders, _) = b.tick();
        assert_eq!(orders, vec![b.site_order(Extension, slot)]);
        let (orders, _) = b.tick();
        assert!(orders.is_empty(), "sited");
        let looks = b.looks();
        // Built: the site goes and the extension stands, with no scan on this tick or the next.
        b.sites.clear();
        b.build(Extension, slot);
        for _ in 0..2 {
            let (orders, destroys) = b.step(View::Ready, false);
            assert!(orders.is_empty() && destroys.is_empty());
            assert_eq!(b.looks(), looks, "no look before a scan at {}", b.time);
        }
        let (orders, _) = b.step(View::Ready, true);
        assert!(orders.is_empty());
        assert_eq!(b.looks(), looks + 1, "the scan came");
        assert_eq!(b.census().map(|c| (c.due, c.built)), Some((2, 2)));
        // Lost (destroyed by someone else), scanned on the same tick: a look at once, and its
        // site goes back.
        b.structures
            .retain(|&(_, k, t)| !(k == Extension && t == slot));
        let (orders, _) = b.step(View::Ready, true);
        assert_eq!(b.looks(), looks + 2);
        assert_eq!(orders, vec![b.site_order(Extension, slot)]);
    }

    /// The economy's container need next to a source (ADR-023 keeps its timing) goes on the plan's
    /// mining tile for that source (`roles.mining`), before the plan's own items; once it is
    /// sited the plan's items go on.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `allowed` and `earliest_level`: the level's
    /// containers, the first level with an extension).
    #[test]
    fn a_container_need_goes_on_the_plans_mining_tile() {
        let level = level_of(Extension);
        assert!(allowed(Container, level) > 0);
        // The plan's mining tile and the economy's own pick, both next to the source.
        let (mining, need) = (tile(11, 10), tile(9, 9));
        assert_eq!((source().range(mining), source().range(need)), (1, 1));
        let slot = tile(27, 25);
        let roles = Roles {
            mining: vec![mining],
            source_links: vec![None],
            ..Roles::default()
        };
        let items = vec![
            item(Extension, slot, level),
            item(Container, mining, level_of(Container)),
        ];
        let mut b = Bench::new(level, items, roles);
        b.container_needs = vec![crate::economy::ContainerNeed {
            tile: need,
            source: Some(source()),
        }];
        let (orders, destroys) = b.tick();
        assert_eq!(orders, vec![b.site_order(Container, mining)]);
        assert!(destroys.is_empty());
        let t = *b.placement.totals();
        assert_eq!((t.picked, t.planned, t.containers, t.links), (1, 1, 1, 0));
        // The economy still lists its tile (it holds no container), but the plan's is sited.
        let (orders, _) = b.tick();
        assert_eq!(orders, vec![b.site_order(Extension, slot)]);
        let t = *b.placement.totals();
        assert_eq!((t.picked, t.planned, t.containers), (2, 2, 1));
    }

    /// The economy's needs go by role, not by their own tile: the controller container's need
    /// (`source: None`) goes on the plan's controller container even when its tile is next to the
    /// source, and a mining container's need on the plan's mining tile even when its tile is the
    /// controller container's (two needs whose tiles the old position rule read the other way).
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `allowed` and `earliest_level`).
    #[test]
    fn container_needs_go_on_the_plans_tile_for_their_role() {
        use crate::economy::ContainerNeed;
        let level = level_of(Extension);
        assert!(allowed(Container, level) > 1);
        let (mining, controller_container) = (tile(11, 10), tile(25, 38));
        let roles = Roles {
            mining: vec![mining],
            source_links: vec![None],
            controller_container: Some(controller_container),
            ..Roles::default()
        };
        let items = vec![
            item(Container, mining, level_of(Container)),
            item(Container, controller_container, level_of(Container)),
        ];
        let mut b = Bench::new(level, items, roles);
        b.container_needs = vec![
            ContainerNeed {
                tile: tile(9, 9),
                source: None,
            },
            ContainerNeed {
                tile: controller_container,
                source: Some(source()),
            },
        ];
        let (orders, destroys) = b.tick();
        assert!(destroys.is_empty());
        assert_eq!(orders, vec![b.site_order(Container, controller_container)]);
        let (orders, _) = b.tick();
        assert_eq!(orders, vec![b.site_order(Container, mining)]);
        assert_eq!(b.placement.totals().containers, 2);
    }

    /// While the room's plan is being built (`PlanView::Waiting`) nothing is placed or destroyed
    /// and no look is made, with a free item, a container need and a blocker all due; once the
    /// plan is ready it places at once.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn a_waiting_plan_places_nothing() {
        let level = level_of(Storage);
        let roles = Roles {
            mining: vec![tile(11, 10)],
            source_links: vec![None],
            ..Roles::default()
        };
        let mut b = Bench::new(
            level,
            vec![
                item(Storage, storage_slot(), level),
                item(Extension, tile(23, 25), level_of(Extension)),
            ],
            roles,
        );
        b.pcfg = storage_on();
        b.build(Extension, storage_slot());
        b.container_needs = vec![crate::economy::ContainerNeed {
            tile: tile(9, 9),
            source: Some(source()),
        }];
        for _ in 0..3 {
            let (orders, destroys) = b.step(View::Waiting, true);
            b.act_sites(Outcome::Ok);
            b.act_destroys(Outcome::Ok, true);
            assert!(orders.is_empty() && destroys.is_empty());
        }
        assert_eq!(*b.placement.totals(), Default::default());
        assert_eq!(b.census(), Some(Census::default()));
        let (orders, _) = b.tick();
        assert_eq!(orders, vec![b.site_order(Container, tile(11, 10))]);
    }

    /// A destroy the game carries out opens its replacement: the plan's item for the freed tile
    /// gets the next site, and while that replacement is open (here its site was refused) no other
    /// destroy goes out until `verify_ticks` have passed since.
    ///
    /// Uses: `PLANNER_MMO.verify_ticks`, `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn an_open_replacement_holds_the_next_destroy() {
        let level = level_of(Storage);
        let first = tile(23, 25);
        let extension = vec![item(Extension, first, level_of(Extension))];
        let mut b = storage_bench(level, Extension, storage_on(), extension);
        // An off-plan tower on the planned extension's tile: the extension goes first.
        b.build(Tower, first);
        let (orders, destroys) = b.tick();
        assert!(orders.is_empty());
        assert_eq!(destroys, vec![b.destroy_of(Tower, first)]);
        // Gone: the bench scans on this tick, so the look is made at once.
        let (orders, destroys) = b.step(View::Ready, true);
        let since = b.time;
        assert!(destroys.is_empty());
        assert_eq!(orders, vec![b.site_order(Extension, first)]);
        b.act_sites(Outcome::Failed(ErrorCode::InvalidTarget));
        let verify = b.pcfg.verify_ticks;
        let mut came = None;
        while came.is_none() && b.time < since + 2 * verify {
            let (orders, destroys) = b.tick();
            assert!(orders.is_empty(), "the refused tile is skipped");
            if !destroys.is_empty() {
                assert_eq!(destroys, vec![b.destroy_of(Extension, storage_slot())]);
                came = Some(b.time);
            }
        }
        let came = came.expect("the next destroy came");
        assert!(
            came >= since + verify,
            "held until {}, came at {came}",
            since + verify
        );
        let t = *b.placement.totals();
        assert_eq!((t.destroys, t.destroyed, t.refused), (2, 2, 1));
    }

    /// A destroy holds the next one open only for a replacement the bot places now: the storage
    /// gate turned off while the storage tile's blocker is being destroyed leaves nothing to hold
    /// for, so the tower tile's blocker is destroyed on the next look, not `verify_ticks` later;
    /// with the gate still on the hold stands until the storage is sited.
    ///
    /// Uses: `PLANNER_MMO.verify_ticks`, `CONTROLLER_STRUCTURES` (through `earliest_level` and
    /// `allowed`).
    #[test]
    fn a_destroy_holds_only_for_a_replacement_placed_now() {
        let level = level_of(Storage);
        assert!(level_of(Tower) <= level && allowed(Tower, level) > 0);
        let tower = tile(23, 25);
        for gate_off in [true, false] {
            let tower_item = vec![item(Tower, tower, level_of(Tower))];
            let both = PlannerConfig {
                towers: true,
                ..storage_on()
            };
            let mut b = storage_bench(level, Extension, both, tower_item);
            b.build(Extension, tower);
            // The storage comes first in the plan's order (`KINDS`).
            let (orders, destroys) = b.step(View::Ready, true);
            assert!(orders.is_empty());
            assert_eq!(destroys, vec![b.destroy_of(Extension, storage_slot())]);
            b.act_destroys(Outcome::Ok, true);
            if gate_off {
                b.pcfg.storage = false;
            }
            // Gone: the next look (the bench scans every tick) clears the tower's tile at once
            // when nothing is held, or sites the storage when its gate is still on.
            let (orders, destroys) = b.step(View::Ready, true);
            if gate_off {
                assert!(orders.is_empty());
                assert_eq!(destroys, vec![b.destroy_of(Extension, tower)]);
            } else {
                assert!(destroys.is_empty());
                assert_eq!(orders, vec![b.site_order(Storage, storage_slot())]);
            }
        }
    }

    /// A destroy for one of the economy's links holds the next destroy only while the economy
    /// still asks for that link: with the need gone when the destroy is done (here the need list
    /// is emptied), the storage tile's blocker goes on the next look; with the need still there
    /// the link is sited first.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level` and `allowed`: the first level with
    /// a link and a storage).
    #[test]
    fn an_economy_destroy_holds_only_while_its_need_stands() {
        use crate::economy::LinkNeed;
        let level = level_of(Link).max(level_of(Storage));
        assert!(allowed(Link, level) > 0);
        let m = tile(12, 11);
        for asked in [true, false] {
            let roles = Roles {
                mining: vec![tile(11, 10)],
                source_links: vec![Some(m)],
                ..Roles::default()
            };
            let items = vec![
                item(Storage, storage_slot(), level_of(Storage)),
                item(Link, m, level_of(Link)),
            ];
            let mut b = Bench::new(level, items, roles);
            b.pcfg = storage_on();
            b.build(Extension, storage_slot());
            b.build(Extension, m);
            b.link_needs = vec![LinkNeed {
                tile: tile(9, 11),
                source: Some(source()),
            }];
            // The economy goes first: the blocker on its link's tile.
            let (orders, destroys) = b.step(View::Ready, true);
            assert!(orders.is_empty());
            assert_eq!(destroys, vec![b.destroy_of(Extension, m)]);
            b.act_destroys(Outcome::Ok, true);
            if !asked {
                b.link_needs.clear();
            }
            let (orders, destroys) = b.step(View::Ready, true);
            if asked {
                assert!(destroys.is_empty(), "held for the link");
                assert_eq!(orders, vec![b.site_order(Link, m)]);
            } else {
                assert!(orders.is_empty());
                assert_eq!(
                    destroys,
                    vec![b.destroy_of(Extension, storage_slot())],
                    "nothing asks for the link, so nothing holds"
                );
            }
        }
    }

    /// A destroy whose replacement is a type at the level's count holds nothing: an opening
    /// destroy of an off-plan link on a planned extension slot, in a room at its extension count,
    /// leaves the slot capped, so the next tower's blocker goes out a look or two later, not
    /// `verify_ticks` later.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level` and `allowed`: the first level with
    /// a link, the towers and extensions it allows), `PLANNER_MMO.verify_ticks`.
    #[test]
    fn a_destroy_for_a_capped_type_holds_nothing() {
        let level = level_of(Link).max(level_of(Tower));
        let (towers, extensions) = (allowed(Tower, level), allowed(Extension, level));
        assert!(towers >= 2 && allowed(Link, level) > 0);
        let t = tile(35, 25);
        let t2 = tile(40, 30);
        assert!(t.index() < t2.index(), "the boxed-in tower comes first");
        let ring: Vec<Tile> = t.neighbors().map(|(_, n)| n).collect();
        let m = *ring.first().expect("a ring");
        let mut items = vec![
            item(Tower, t, level_of(Tower)),
            item(Tower, t2, level_of(Tower)),
        ];
        items.extend(
            ring.iter()
                .map(|&r| item(Extension, r, level_of(Extension))),
        );
        let mut b = Bench::new(level, items, Roles::default());
        b.pcfg = PlannerConfig {
            towers: true,
            ..PLANNER_MMO
        };
        // The ring: on-plan extensions but on `m`, an off-plan link there.
        for &r in ring.iter().filter(|&&r| r != m) {
            b.build(Extension, r);
        }
        b.build(Link, m);
        b.build(Extension, t2);
        // The rest of the level's extensions, off the plan on open ground.
        let mut have = ring.len() as u32;
        let mut x = 5;
        while have < extensions {
            b.build(Extension, tile(x, 45));
            x += 1;
            have += 1;
        }
        let (orders, destroys) = b.step(View::Ready, true);
        assert!(orders.is_empty());
        assert_eq!(destroys, vec![b.destroy_of(Link, m)], "the opening");
        b.act_destroys(Outcome::Ok, true);
        let start = b.time;
        let mut came = None;
        while came.is_none() && b.time < start + 5 {
            let (_, destroys) = b.tick();
            if destroys.contains(&b.destroy_of(Extension, t2)) {
                came = Some(b.time);
            }
        }
        let came = came.expect("the second tower's blocker went out within a few looks");
        assert!(came < start + b.pcfg.verify_ticks);
    }

    /// A room whose plan holds only the spawn and the two roads, at the roads' first level.
    fn road_bench() -> Bench {
        Bench::new(level_of(Road), road_items(), Roles::default())
    }

    /// The plan's first road is picked on a scanned tick and the call accepted, but no site is ever
    /// shown to the placement. Returns the pick's tick.
    fn pick_unseen_road(b: &mut Bench) -> u32 {
        let (orders, destroys) = b.step(View::Ready, true);
        assert_eq!(orders, vec![b.site_order(Road, roads()[0])]);
        assert!(destroys.is_empty());
        b.placement.record(0, Outcome::Ok);
        b.time
    }

    /// A road site built before the placement saw it (once built it has no owner, so the snapshot
    /// never holds it): movement's scan showing the road on its tile makes the pick done, so
    /// nothing counts as lost, the tile is not refused, and the next road goes out on that tick;
    /// both when the road shows on the next tick and when the Optional tier skipped past
    /// `APPEAR_TICKS` meanwhile.
    ///
    /// Uses: `APPEAR_TICKS`, `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn a_road_built_unseen_is_done_by_movements_scan() {
        for skipped in [0, APPEAR_TICKS] {
            let mut b = road_bench();
            let at = pick_unseen_road(&mut b);
            b.roads.push(roads()[0]);
            b.time += skipped;
            let (orders, destroys) = b.step(View::Ready, true);
            assert_eq!(b.time, at + skipped + 1);
            assert!(destroys.is_empty());
            assert_eq!(
                orders,
                vec![b.site_order(Road, roads()[1])],
                "skipped {skipped}"
            );
            let t = *b.placement.totals();
            assert_eq!(
                (t.picked, t.placed, t.lost, t.refused),
                (2, 1, 0, 0),
                "skipped {skipped}"
            );
            assert!(b.placement.refused_tiles(b.room).is_empty());
        }
    }

    /// A road pick still unseen at its `APPEAR_TICKS` timeout while the room's last scan is older
    /// than pick + `APPEAR_TICKS` (here one from the tick before the timeout, which does not count):
    /// the room places nothing and asks movement for a rescan each tick until a scan newer than
    /// that. If that scan shows the road the pick is done; if it shows nothing the pick counts as
    /// lost then (the tile refused). Either way the next road goes out on the scan's tick.
    ///
    /// Uses: `APPEAR_TICKS`, `LOST_MAX`, `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn a_road_pick_at_its_timeout_waits_for_a_newer_scan() {
        const { assert!(1 < LOST_MAX) };
        for built in [true, false] {
            let mut b = road_bench();
            let at = pick_unseen_road(&mut b);
            let timeout = at + APPEAR_TICKS;
            // No scan until the tick before the timeout, whose scan shows no road yet.
            while b.time + 1 < timeout {
                let scan = b.time + 2 == timeout;
                let (orders, destroys) = b.step(View::Ready, scan);
                assert!(
                    orders.is_empty() && destroys.is_empty(),
                    "pending at {}",
                    b.time
                );
            }
            assert_eq!(b.movement.room_scanned(b.room), Some(timeout - 1));
            // From the timeout on, the room waits for a newer scan and asks for one.
            let wait = APPEAR_TICKS / 2;
            while b.time + 1 < timeout + wait {
                let (orders, destroys) = b.step(View::Ready, false);
                assert!(
                    orders.is_empty() && destroys.is_empty(),
                    "waiting at {}",
                    b.time
                );
                assert_ne!(b.movement.tick().stale, 0, "no rescan asked at {}", b.time);
                assert_eq!(b.placement.totals().lost, 0);
                assert!(b.placement.refused_tiles(b.room).is_empty());
            }
            assert!(
                b.time + 1 < at + 2 * APPEAR_TICKS,
                "the scan comes inside the wait's cap"
            );
            if built {
                b.roads.push(roads()[0]);
            }
            let (orders, destroys) = b.step(View::Ready, true);
            assert!(destroys.is_empty());
            assert_eq!(
                orders,
                vec![b.site_order(Road, roads()[1])],
                "built {built}"
            );
            assert_eq!(
                b.placement.totals().lost,
                u32::from(!built),
                "built {built}"
            );
            let refused: Vec<Tile> = if built { Vec::new() } else { vec![roads()[0]] };
            assert_eq!(
                b.placement.refused_tiles(b.room).to_vec(),
                refused,
                "built {built}"
            );
        }
    }

    /// With no scan of the room after an unseen road pick, the pick waits from the tick its site is
    /// first found gone (`APPEAR_TICKS` after the pick) for `APPEAR_TICKS` plus
    /// `MovementConfig::rescan_min_ticks` more, then counts as lost on that tick (the tile
    /// refused), and the room goes on with the next road.
    ///
    /// Uses: `APPEAR_TICKS`, `LOST_MAX`, `MOVEMENT_MMO.rescan_min_ticks`, `CONTROLLER_STRUCTURES`
    /// (through `earliest_level`).
    #[test]
    fn an_unseen_road_pick_with_no_scan_is_lost_once_its_wait_runs_out() {
        const { assert!(1 < LOST_MAX) };
        let mut b = road_bench();
        let at = pick_unseen_road(&mut b);
        let cap = at + APPEAR_TICKS + APPEAR_TICKS + MOVEMENT_MMO.rescan_min_ticks;
        while b.time + 1 < cap {
            let (orders, destroys) = b.step(View::Ready, false);
            assert!(
                orders.is_empty() && destroys.is_empty(),
                "pending at {}",
                b.time
            );
            assert_eq!(b.placement.totals().lost, 0);
        }
        assert_eq!(b.movement.room_scanned(b.room), Some(at));
        let (orders, destroys) = b.step(View::Ready, false);
        assert_eq!(b.time, cap);
        assert!(destroys.is_empty());
        assert_eq!(b.placement.totals().lost, 1);
        assert_eq!(b.placement.refused_tiles(b.room).to_vec(), vec![roads()[0]]);
        assert_eq!(orders, vec![b.site_order(Road, roads()[1])]);
    }

    /// A road built during a gap in the placement longer than `2 * APPEAR_TICKS` (the Optional
    /// tier off, say), with movement's last scan from before it was built: the first look after
    /// the gap waits for a scan instead of counting the pick lost, then the scan judges it: the
    /// road shown makes it done; nothing shown counts it lost.
    ///
    /// Uses: `APPEAR_TICKS`, `MOVEMENT_MMO.rescan_min_ticks`, `CONTROLLER_STRUCTURES` (through
    /// `earliest_level`).
    #[test]
    fn a_road_built_in_a_long_gap_waits_for_a_scan_after_it() {
        for built in [true, false] {
            let mut b = road_bench();
            let at = pick_unseen_road(&mut b);
            if built {
                b.roads.push(roads()[0]);
            }
            b.time += 3 * APPEAR_TICKS;
            let (orders, destroys) = b.step(View::Ready, false);
            assert!(orders.is_empty() && destroys.is_empty(), "built {built}");
            assert_eq!(b.placement.totals().lost, 0, "built {built}");
            assert_eq!(b.movement.room_scanned(b.room), Some(at));
            const { assert!(1 < APPEAR_TICKS + MOVEMENT_MMO.rescan_min_ticks) };
            let (orders, destroys) = b.step(View::Ready, true);
            assert!(destroys.is_empty());
            let t = *b.placement.totals();
            if built {
                assert_eq!(t.lost, 0);
                assert!(b.placement.refused_tiles(b.room).is_empty());
            } else {
                assert_eq!(t.lost, 1);
                assert_eq!(b.placement.refused_tiles(b.room).to_vec(), vec![roads()[0]]);
            }
            assert_eq!(
                orders,
                vec![b.site_order(Road, roads()[1])],
                "built {built}"
            );
        }
    }

    /// The extension slot of the `ERR_RCL_NOT_ENOUGH` tests.
    fn full_slot() -> Tile {
        tile(27, 25)
    }

    /// A plan-path site refused for the level's count (`ERR_RCL_NOT_ENOUGH`: another owner's
    /// structures count too, V238) holds its type, not the room: nothing counts as lost and the
    /// tile is not refused, the room's roads go out meanwhile, and the type is offered again on
    /// the tick `verify_ticks` have passed since the refusal.
    ///
    /// Uses: `PLANNER_MMO.verify_ticks`, `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn a_full_type_is_held_while_the_room_places_on() {
        let level = level_of(Extension);
        let mut items = vec![item(Extension, full_slot(), level)];
        items.extend(road_items());
        let mut b = Bench::new(level, items, Roles::default());
        let verify = b.pcfg.verify_ticks;
        let roads_n = u32::try_from(roads().len()).expect("small");
        assert!(roads_n + 1 < verify, "the hold outlasts the road sites");
        let (orders, _) = b.step(View::Ready, true);
        assert_eq!(orders, vec![b.site_order(Extension, full_slot())]);
        b.act_sites(Outcome::Failed(ErrorCode::RclNotEnough));
        let full_at = b.time;
        let t = *b.placement.totals();
        assert_eq!((t.full, t.refused, t.lost, t.placed), (1, 1, 0, 0));
        assert!(b.placement.refused_tiles(b.room).is_empty());
        let mut sited = Vec::new();
        while b.time + 1 < full_at + verify {
            let (orders, destroys) = b.tick();
            assert!(destroys.is_empty());
            assert!(
                orders.iter().all(|o| o.kind != Extension),
                "held at {}",
                b.time
            );
            sited.extend(orders.iter().map(|o| o.tile));
        }
        assert_eq!(sited, roads().to_vec(), "the roads went out meanwhile");
        assert_eq!(b.census().map(|c| c.capped), Some(1));
        let (orders, _) = b.step(View::Ready, true);
        assert_eq!(b.time, full_at + verify);
        assert_eq!(orders, vec![b.site_order(Extension, full_slot())]);
        b.act_sites(Outcome::Ok);
        let t = *b.placement.totals();
        assert_eq!((t.full, t.lost, t.placed), (1, 0, roads_n + 1));
    }

    /// `LOST_MAX` refusals for the level's count in a row, with no site between them, do not stop
    /// a planned room: after each hold of `verify_ticks` the type is offered again.
    ///
    /// Uses: `LOST_MAX`, `PLANNER_MMO.verify_ticks`, `CONTROLLER_STRUCTURES` (through
    /// `earliest_level`).
    #[test]
    fn full_refusals_never_stop_a_planned_room() {
        let level = level_of(Extension);
        let items = vec![item(Extension, full_slot(), level)];
        let mut b = Bench::new(level, items, Roles::default());
        let verify = b.pcfg.verify_ticks;
        for round in 1..=LOST_MAX {
            let (orders, _) = b.step(View::Ready, true);
            assert_eq!(
                orders,
                vec![b.site_order(Extension, full_slot())],
                "round {round}"
            );
            b.act_sites(Outcome::Failed(ErrorCode::RclNotEnough));
            assert_eq!(b.placement.totals().full, round);
            let full_at = b.time;
            while b.time + 1 < full_at + verify {
                let (orders, destroys) = b.tick();
                assert!(
                    orders.is_empty() && destroys.is_empty(),
                    "held at {}",
                    b.time
                );
            }
        }
        let (orders, _) = b.step(View::Ready, true);
        assert_eq!(
            orders,
            vec![b.site_order(Extension, full_slot())],
            "not stopped"
        );
        let t = *b.placement.totals();
        assert_eq!((t.full, t.refused, t.lost), (LOST_MAX, LOST_MAX, 0));
    }

    /// A level change ends a full type's hold at once: the refused extension is offered on the
    /// first tick at the next level, long before `verify_ticks`.
    ///
    /// Uses: `PLANNER_MMO.verify_ticks`, `CONTROLLER_STRUCTURES` (through `earliest_level` and
    /// `allowed`: the first level with extensions, and the next one that allows more).
    #[test]
    fn a_level_change_offers_a_full_type_again_at_once() {
        let first = level_of(Extension);
        let next = earliest_level(Extension, allowed(Extension, first)).expect("more later");
        let mut items = vec![item(Extension, full_slot(), first)];
        items.extend(road_items());
        let mut b = Bench::new(first, items, Roles::default());
        let (orders, _) = b.step(View::Ready, true);
        assert_eq!(orders, vec![b.site_order(Extension, full_slot())]);
        b.act_sites(Outcome::Failed(ErrorCode::RclNotEnough));
        let full_at = b.time;
        let (orders, _) = b.tick();
        assert_eq!(orders, vec![b.site_order(Road, roads()[0])], "held");
        b.level = next;
        let (orders, _) = b.tick();
        assert!(b.time < full_at + b.pcfg.verify_ticks);
        assert_eq!(orders, vec![b.site_order(Extension, full_slot())]);
        assert_eq!(b.placement.totals().full, 1);
    }

    /// On the interim path (the planner off, or no valid plan) `ERR_RCL_NOT_ENOUGH` still counts
    /// toward `LOST_MAX`: after that many refused extension picks in a row the room gets no more
    /// calls, and none of them counts as full.
    ///
    /// Uses: `LOST_MAX`, `APPEAR_TICKS`, `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn the_interim_path_counts_a_full_refusal_toward_lost_max() {
        for view in [View::Off, View::Fallback] {
            let mut b = Bench::new(level_of(Extension), Vec::new(), Roles::default());
            b.extensions = true;
            for pick in 0..LOST_MAX {
                let (orders, destroys) = b.step(view, true);
                assert!(destroys.is_empty());
                assert_eq!(orders.len(), 1, "{view:?} pick {pick}");
                assert!(orders.iter().all(|o| o.kind == Extension));
                b.act_sites(Outcome::Failed(ErrorCode::RclNotEnough));
            }
            for _ in 0..APPEAR_TICKS {
                let (orders, destroys) = b.step(view, true);
                assert!(
                    orders.is_empty() && destroys.is_empty(),
                    "{view:?} stopped at {}",
                    b.time
                );
            }
            let t = *b.placement.totals();
            assert_eq!(
                (t.picked, t.refused, t.full, t.lost),
                (LOST_MAX, LOST_MAX, 0, 0),
                "{view:?}"
            );
        }
    }

    /// The economy's link needs go on the plan's link of their role, whatever their own tile: with
    /// the source near the controller, a source link's need whose tile is within `RECEIVER_RANGE`
    /// of the controller goes on the plan's link for that source (`roles.source_links`), and the
    /// controller link's need (`source: None`) on `roles.controller_link`.
    ///
    /// Uses: `RECEIVER_RANGE`, `NEAR`, `CONTROLLER_STRUCTURES` (through `earliest_level`: the
    /// first level that allows two links).
    #[test]
    fn a_link_need_goes_on_the_plans_link_for_its_role() {
        use crate::economy::mining::NEAR;
        use crate::economy::{LinkNeed, RECEIVER_RANGE};
        let level = earliest_level(Link, 1).expect("two links at some level");
        let controller = tile(14, 10);
        let mining = tile(11, 10);
        let (source_link, controller_link) = (tile(12, 11), tile(15, 11));
        // The economy's own pick for the source's link, next to the mining tile.
        let need = tile(12, 9);
        assert_eq!(source().range(mining), NEAR);
        assert!(need.range(mining) <= NEAR && source_link.range(mining) <= NEAR);
        for t in [need, source_link, controller_link] {
            assert!(t.range(controller) <= RECEIVER_RANGE, "{t:?}");
        }
        let roles = Roles {
            mining: vec![mining],
            source_links: vec![Some(source_link)],
            controller_link: Some(controller_link),
            ..Roles::default()
        };
        let items = vec![
            item(Link, source_link, level),
            item(Link, controller_link, level),
        ];
        let source_need = LinkNeed {
            tile: need,
            source: Some(source()),
        };
        let controller_need = LinkNeed {
            tile: tile(15, 9),
            source: None,
        };
        // The source's link alone: on the plan's source link, not the controller link.
        let mut b = Bench::new(level, items.clone(), roles.clone());
        b.move_controller(controller);
        b.link_needs = vec![source_need];
        let (orders, destroys) = b.tick();
        assert!(destroys.is_empty());
        assert_eq!(orders, vec![b.site_order(Link, source_link)]);
        let t = *b.placement.totals();
        assert_eq!((t.picked, t.planned, t.links), (1, 1, 1));
        // Both, the controller's first (`Mining::link_needs`' order), each on its role's link; the
        // sited controller link's need, still listed, is passed over.
        let mut b = Bench::new(level, items, roles);
        b.move_controller(controller);
        b.link_needs = vec![controller_need, source_need];
        let (orders, _) = b.tick();
        assert_eq!(orders, vec![b.site_order(Link, controller_link)]);
        let (orders, _) = b.tick();
        assert_eq!(orders, vec![b.site_order(Link, source_link)]);
        assert_eq!(b.placement.totals().links, 2);
    }

    /// A change this tick's movement scan already shows asks for no early rescan: a site built
    /// (the site gone, its structure standing) or a destroy done, seen on a tick the room was
    /// scanned, leaves movement's tick counters with no rescan asked (`Counts::stale`); the same
    /// change on a tick with no scan asks for one.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn a_change_this_ticks_scan_shows_asks_for_no_rescan() {
        let level = level_of(Extension);
        let slot = tile(27, 25);
        for scan in [true, false] {
            let mut b = Bench::new(level, vec![item(Extension, slot, level)], Roles::default());
            let (orders, _) = b.tick();
            assert_eq!(orders, vec![b.site_order(Extension, slot)]);
            b.tick();
            b.sites.clear();
            b.build(Extension, slot);
            let (orders, destroys) = b.step(View::Ready, scan);
            assert!(orders.is_empty() && destroys.is_empty());
            assert_eq!(
                b.movement.tick().stale == 0,
                scan,
                "built, scanned {scan}: {} asked",
                b.movement.tick().stale
            );
        }
        let level = level_of(Storage);
        for scan in [true, false] {
            let mut b = storage_bench(level, Extension, storage_on(), Vec::new());
            let (orders, destroys) = b.tick();
            assert!(orders.is_empty());
            assert_eq!(destroys, vec![b.destroy_of(Extension, storage_slot())]);
            let (orders, _) = b.step(View::Ready, scan);
            assert_eq!(
                b.movement.tick().stale == 0,
                scan,
                "destroyed, scanned {scan}: {} asked",
                b.movement.tick().stale
            );
            if scan {
                assert_eq!(orders, vec![b.site_order(Storage, storage_slot())]);
            }
        }
    }

    /// A destroy the game refuses for a reason other than `ERR_BUSY` (here `ERR_NOT_OWNER`) counts
    /// toward the room's destroy limit and never toward the sites' `lost` or `destroys_dropped`:
    /// after `LOST_MAX` in a row the room gets no more destroys while its roads go out. A dropped
    /// destroy (accepted, the structure still there after `APPEAR_TICKS`) shares that limit with
    /// refused ones, and only it counts as dropped.
    ///
    /// Uses: `LOST_MAX`, `APPEAR_TICKS`, `PLANNER_MMO.verify_ticks`, `CONTROLLER_STRUCTURES`
    /// (through `earliest_level`).
    #[test]
    fn refused_destroys_count_toward_the_destroy_limit_only() {
        let level = level_of(Storage);
        for dropped in [0, 1] {
            let mut b = storage_bench(level, Extension, storage_on(), road_items());
            for _ in 0..dropped {
                let (orders, destroys) = b.step(View::Ready, true);
                assert!(orders.is_empty());
                assert_eq!(destroys, vec![b.destroy_of(Extension, storage_slot())]);
                b.act_destroys(Outcome::Ok, false);
                for _ in 1..APPEAR_TICKS {
                    let (orders, destroys) = b.tick();
                    assert!(
                        orders.is_empty() && destroys.is_empty(),
                        "pending at {}",
                        b.time
                    );
                }
            }
            for refused in 0..LOST_MAX - dropped {
                let (orders, destroys) = b.step(View::Ready, true);
                assert!(orders.is_empty(), "the storage's destroy goes first");
                assert_eq!(
                    destroys,
                    vec![b.destroy_of(Extension, storage_slot())],
                    "dropped {dropped}, refused {refused}"
                );
                b.act_destroys(Outcome::Failed(ErrorCode::NotOwner), false);
                let t = *b.placement.totals();
                assert_eq!((t.lost, t.destroys_dropped), (0, dropped));
            }
            // The limit reached: no more destroys, and the roads go out.
            let mut sited = Vec::new();
            for _ in 0..=b.pcfg.verify_ticks {
                let (orders, destroys) = b.tick();
                assert!(destroys.is_empty(), "no destroy at {}", b.time);
                sited.extend(orders.iter().map(|o| o.tile));
            }
            assert_eq!(sited, roads().to_vec(), "dropped {dropped}");
            let t = *b.placement.totals();
            assert_eq!(
                (
                    t.destroys,
                    t.destroyed,
                    t.destroys_dropped,
                    t.lost,
                    t.refused
                ),
                (LOST_MAX, dropped, dropped, 0, 0),
                "dropped {dropped}"
            );
            assert_eq!(b.census().map(|c| c.blocked), Some(1));
        }
    }
}
