//! Tests for remote mining's profit model (P6.1, `economy::remote`). Expected values derive from
//! constants; the solvers are checked against brute force over their whole search space.

use rustedscreep_consts::{
    CARRY_CAPACITY, CONSTRUCTION_COST_ROAD_SWAMP_RATIO, CONSTRUCTION_COST_ROAD_WALL_RATIO,
    CONTAINER_DECAY, CONTAINER_DECAY_TIME, CONTROLLER_RESERVE, CONTROLLER_RESERVE_MAX,
    CREEP_CLAIM_LIFE_TIME, CREEP_LIFE_TIME, CREEP_SPAWN_TIME, ENERGY_REGEN_TIME, HARVEST_POWER,
    INTENT_CPU_COST, MAX_CREEP_SIZE, MOVE_COST_PLAIN, MOVE_COST_ROAD, MOVE_COST_SWAMP, MOVE_POWER,
    Part, REPAIR_COST, REPAIR_POWER, SOURCE_ENERGY_CAPACITY, SOURCE_ENERGY_KEEPER_CAPACITY,
    SOURCE_ENERGY_NEUTRAL_CAPACITY, SPAWN_ENERGY_CAPACITY, STORAGE_CAPACITY, StructureType,
    extension_energy_capacity,
};

use super::sizing::{
    Trip, haul_round, hauler_plan, hauler_plan_with, haulers_for, miner_body_with, miner_body_work,
    miner_work, walk_ticks, yield_per_life,
};
use super::*;
use crate::config::{HAULING_MMO, REMOTE_MMO, RemoteConfig};
use crate::spawning::Body;

const LIFE: u64 = CREEP_LIFE_TIME as u64;

/// What a room at `rcl` holds at most: its spawn and its level's extensions.
/// Uses: SPAWN_ENERGY_CAPACITY, CONTROLLER_STRUCTURES, EXTENSION_ENERGY_CAPACITY.
fn energy_at(rcl: u32) -> u32 {
    SPAWN_ENERGY_CAPACITY
        + StructureType::Extension.controller_structures(rcl) * extension_energy_capacity(rcl)
}

fn count(body: &Body, part: Part) -> u32 {
    body.count(part) as u32
}

/// A way of `tiles` tiles at fatigue rate `rate`, and the same way paved.
fn way(tiles: u32, rate: u32) -> RemoteWay {
    RemoteWay {
        bare: Trip {
            tiles,
            cost: tiles * rate,
        },
        paved: Trip {
            tiles,
            cost: tiles * MOVE_COST_ROAD,
        },
    }
}

/// A source `tiles` away at `rate`, half of its way needing a new road (the rest is the home's).
fn source(tiles: u32, rate: u32, container: bool) -> RemoteSource {
    let half = tiles - tiles / 2;
    let road = if rate == MOVE_COST_SWAMP {
        RoadTiles {
            swamp: half,
            ..RoadTiles::NONE
        }
    } else {
        RoadTiles {
            plain: half,
            ..RoadTiles::NONE
        }
    };
    RemoteSource {
        way: way(tiles, rate),
        road,
        road_new: road,
        container,
    }
}

fn home(rcl: u32) -> RemoteHome {
    RemoteHome {
        energy: energy_at(rcl),
        unload: STORAGE_CAPACITY,
        margin_pct: HAULING_MMO.margin_pct,
    }
}

const NO_ROADS: RemotePlan = RemotePlan {
    reserve: false,
    roads: false,
};

/// Road tiles weigh by their terrain's construction ratio, and cost `CONSTRUCTION_COST[road]` a
/// plain-road tile (V186, V235, V242).
/// Uses: CONSTRUCTION_COST, CONSTRUCTION_COST_ROAD_SWAMP_RATIO, CONSTRUCTION_COST_ROAD_WALL_RATIO.
#[test]
fn road_tiles_weigh_by_terrain() {
    let road = u64::from(StructureType::Road.construction_cost().expect("road cost"));
    let t = RoadTiles {
        plain: 7,
        swamp: 3,
        wall: 2,
    };
    let weight = 7
        + 3 * u64::from(CONSTRUCTION_COST_ROAD_SWAMP_RATIO)
        + 2 * u64::from(CONSTRUCTION_COST_ROAD_WALL_RATIO);
    assert_eq!(t.tiles(), 12);
    assert_eq!(t.weight(), weight);
    assert_eq!(t.build(), weight * road);
    assert_eq!(RoadTiles::NONE.build(), 0);
}

/// A remote container loses `CONTAINER_DECAY` every `CONTAINER_DECAY_TIME − 1` ticks (V214).
/// Uses: CONTAINER_DECAY, CONTAINER_DECAY_TIME.
#[test]
fn remote_containers_decay_on_the_unowned_schedule() {
    let step = u64::from(CONTAINER_DECAY_TIME - 1);
    assert_eq!(remote_container_hits(step), u64::from(CONTAINER_DECAY));
    assert_eq!(
        remote_container_hits(step * 3),
        3 * u64::from(CONTAINER_DECAY)
    );
    assert_eq!(
        remote_container_hits(step + 1),
        (u64::from(CONTAINER_DECAY) * (step + 1)).div_ceil(step)
    );
    assert_eq!(remote_container_hits(0), 0);
}

/// The remote miner's WORK parts harvest its source empty and repair what its container loses in
/// one regeneration window within the window (a repair blocks the tick's harvest, V44), and one
/// fewer does not; at a capacity [`miner_work`] drains in exactly the window (no tick to spare),
/// it takes more than [`miner_work`].
/// Uses: ENERGY_REGEN_TIME, HARVEST_POWER, REPAIR_POWER, MAX_CREEP_SIZE, SOURCE_ENERGY_*.
#[test]
fn remote_miners_harvest_and_repair_within_the_window() {
    let window = u64::from(ENERGY_REGEN_TIME);
    let hits = remote_container_hits(window);
    let fits = |capacity: u32, w: u32| {
        u64::from(capacity).div_ceil(u64::from(w * HARVEST_POWER))
            + hits.div_ceil(u64::from(w * REPAIR_POWER))
            <= window
    };
    for capacity in [
        SOURCE_ENERGY_NEUTRAL_CAPACITY,
        SOURCE_ENERGY_CAPACITY,
        SOURCE_ENERGY_KEEPER_CAPACITY,
        1,
        ENERGY_REGEN_TIME * HARVEST_POWER,
    ] {
        let w = remote_miner_work(capacity).expect("a count");
        assert!(w >= miner_work(capacity), "{capacity}");
        assert!(w + 2 <= MAX_CREEP_SIZE, "room for a CARRY and a MOVE");
        assert!(fits(capacity, w), "{capacity}: {w}");
        if w > miner_work(capacity) {
            assert!(!fits(capacity, w - 1), "{capacity}: {w} is the fewest");
        }
        if capacity % (ENERGY_REGEN_TIME * HARVEST_POWER) == 0 {
            assert!(w > miner_work(capacity), "no tick to spare at {capacity}");
        }
    }
    assert_eq!(remote_miner_work(0), None);
}

/// `miner_body_work` with [`miner_work`]'s count is [`miner_body_with`].
/// Uses: SOURCE_ENERGY_*, MOVE_COST_*.
#[test]
fn miner_body_work_generalizes_miner_body_with() {
    for capacity in [SOURCE_ENERGY_NEUTRAL_CAPACITY, SOURCE_ENERGY_CAPACITY] {
        for (tiles, rate) in [
            (0, MOVE_COST_PLAIN),
            (20, MOVE_COST_PLAIN),
            (40, MOVE_COST_SWAMP),
        ] {
            let trip = way(tiles, rate).bare;
            for rcl in 1..=8 {
                for carry in [0, 1] {
                    assert_eq!(
                        miner_body_work(
                            miner_work(capacity),
                            carry,
                            capacity,
                            trip,
                            energy_at(rcl)
                        ),
                        miner_body_with(capacity, trip, energy_at(rcl), carry),
                    );
                }
            }
        }
    }
    assert_eq!(
        miner_body_work(
            0,
            1,
            SOURCE_ENERGY_CAPACITY,
            way(5, MOVE_COST_PLAIN).bare,
            10_000
        ),
        None
    );
}

/// Property test of [`hauler_plan_with`]: with no WORK part it is [`hauler_plan`]; with WORK parts
/// the body is WORK, then CARRY, then MOVE parts, fits the energy and `MAX_CREEP_SIZE`, and its
/// count carries the yield over its own round, in which the WORK parts weigh both ways
/// (`haul_round`, V54); and it takes the fewest haulers of every such body (brute force), then the
/// least energy for all of them, then the fewest MOVE parts.
/// Uses: BODYPART_COST, MAX_CREEP_SIZE, CARRY_CAPACITY, MOVE_POWER, MOVE_COST_*,
/// SOURCE_ENERGY_*, STORAGE_CAPACITY.
#[test]
fn haulers_with_work_parts_weigh_and_take_the_fewest() {
    for capacity in [SOURCE_ENERGY_NEUTRAL_CAPACITY, SOURCE_ENERGY_CAPACITY] {
        for (tiles, rate) in [
            (10, MOVE_COST_ROAD),
            (45, MOVE_COST_ROAD),
            (30, MOVE_COST_PLAIN),
            (25, MOVE_COST_SWAMP),
        ] {
            let trip = Trip {
                tiles,
                cost: tiles * rate,
            };
            for rcl in 1..=8 {
                let energy = energy_at(rcl);
                let unload = STORAGE_CAPACITY;
                assert_eq!(
                    hauler_plan_with(capacity, trip, energy, 0, unload, 0),
                    hauler_plan(capacity, trip, energy, 0, unload),
                );
                let work = 1;
                let Some((body, n)) = hauler_plan_with(capacity, trip, energy, 0, unload, work)
                else {
                    continue;
                };
                let (carry, moves) = (count(&body, Part::Carry), count(&body, Part::Move));
                assert!(body.cost() <= energy);
                assert!(body.len() <= MAX_CREEP_SIZE as usize);
                assert_eq!(count(&body, Part::Work), work);
                assert_eq!(body.parts().first(), Some(&Part::Work), "WORK first");
                assert_eq!(
                    body.parts().iter().position(|&p| p == Part::Move),
                    Some((work + carry) as usize),
                    "then CARRY, then MOVE"
                );
                let round = haul_round(&body, trip, unload).expect("a round");
                assert_eq!(n, haulers_for(capacity, carry, round, 0));
                // Brute force over the same bodies.
                let mut best: Option<(u32, u64, u32)> = None;
                for c in 1..MAX_CREEP_SIZE - work {
                    for m in 1..=MAX_CREEP_SIZE - work - c {
                        let cost = work * Part::Work.cost()
                            + c * Part::Carry.cost()
                            + m * Part::Move.cost();
                        if cost > energy
                            || c * CARRY_CAPACITY > rustedscreep_consts::CONTAINER_CAPACITY
                        {
                            continue;
                        }
                        let loaded = tiles.max((trip.cost * (work + c)).div_ceil(MOVE_POWER * m));
                        let empty = tiles.max((trip.cost * work).div_ceil(MOVE_POWER * m));
                        let r = loaded + empty + 2 * (c * CARRY_CAPACITY).div_ceil(unload).max(1);
                        let k = haulers_for(capacity, c, r, 0);
                        let key = (k, u64::from(k) * u64::from(cost), m);
                        if best.is_none_or(|b| key < b) {
                            best = Some(key);
                        }
                    }
                }
                let got = (n, u64::from(n) * u64::from(body.cost()), moves);
                assert_eq!(Some(got), best, "{capacity} {tiles} {rate} RCL{rcl}");
            }
        }
    }
}

/// One reserver body, as the brute force weighs it.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    cost: u32,
    claim: u32,
    moves: u32,
    stay: u32,
}

/// Property test of [`reserver_body`] over trips and every level's capacity: CLAIM parts then MOVE
/// parts, within the energy and `MAX_CREEP_SIZE`, staying a positive time, its whole stay's growth
/// under `CONTROLLER_RESERVE_MAX`; and no other such body (brute force) gives a reservation tick
/// for less energy, or for as little energy with fewer intents, or with both equal in fewer parts.
/// `None` below one CLAIM and one MOVE.
/// Uses: BODYPART_COST, CREEP_CLAIM_LIFE_TIME, CONTROLLER_RESERVE, CONTROLLER_RESERVE_MAX,
/// MAX_CREEP_SIZE, MOVE_POWER, MOVE_COST_*.
#[test]
fn reserver_bodies_buy_the_cheapest_reservation() {
    let floor = Part::Claim.cost() + Part::Move.cost();
    let mut energies: Vec<u32> = (1..=8).map(energy_at).collect();
    energies.extend([floor - 1, floor, 2 * floor - 1, 2 * floor]);
    for tiles in [0u32, 5, 30, 80, 200] {
        for rate in [MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP] {
            let trip = Trip {
                tiles,
                cost: tiles * rate,
            };
            for &energy in &energies {
                let mut options = Vec::new();
                for claim in 1..MAX_CREEP_SIZE {
                    for moves in 1..=MAX_CREEP_SIZE - claim {
                        let cost = claim * Part::Claim.cost() + moves * Part::Move.cost();
                        let walk = tiles.max((trip.cost * claim).div_ceil(MOVE_POWER * moves));
                        if cost > energy || walk >= CREEP_CLAIM_LIFE_TIME {
                            continue;
                        }
                        let stay = CREEP_CLAIM_LIFE_TIME - walk;
                        if u64::from(claim * CONTROLLER_RESERVE - 1) * u64::from(stay)
                            > u64::from(CONTROLLER_RESERVE_MAX)
                        {
                            continue;
                        }
                        options.push(Candidate {
                            cost,
                            claim,
                            moves,
                            stay,
                        });
                    }
                }
                let Some(body) = reserver_body(trip, energy) else {
                    assert!(options.is_empty(), "{tiles} {rate} {energy}");
                    continue;
                };
                let (claim, moves) = (count(&body, Part::Claim), count(&body, Part::Move));
                assert_eq!(body.len() as u32, claim + moves, "only CLAIM and MOVE");
                assert_eq!(
                    body.parts().iter().position(|&p| p == Part::Move),
                    Some(claim as usize),
                    "CLAIM first"
                );
                assert!(body.cost() <= energy);
                assert_eq!(body.life(), CREEP_CLAIM_LIFE_TIME);
                let walk = walk_ticks(trip, claim, moves).expect("a walk");
                let me = Candidate {
                    cost: body.cost(),
                    claim,
                    moves,
                    stay: CREEP_CLAIM_LIFE_TIME - walk,
                };
                assert!(
                    u64::from(claim * CONTROLLER_RESERVE - 1) * u64::from(me.stay)
                        <= u64::from(CONTROLLER_RESERVE_MAX)
                );
                let adds = |o: &Candidate| u128::from(o.claim) * u128::from(o.stay);
                for o in &options {
                    // Energy a reservation tick: cost / (claim × stay), cross-multiplied.
                    let e_me = u128::from(me.cost) * adds(o);
                    let e_o = u128::from(o.cost) * adds(&me);
                    assert!(e_me <= e_o, "{o:?} is cheaper than {me:?}");
                    if e_me == e_o {
                        let i_me = u128::from(me.stay + tiles) * adds(o);
                        let i_o = u128::from(o.stay + tiles) * adds(&me);
                        assert!(i_me <= i_o, "{o:?} bills fewer than {me:?}");
                        if i_me == i_o {
                            assert!(me.claim + me.moves <= o.claim + o.moves);
                        }
                    }
                }
            }
        }
    }
}

/// Per creep life the figures add up: the yield is each source's at the plan's capacity
/// ([`yield_per_life`]); the net is the yield less bodies, upkeep and the builds' share; the CPU is
/// the intents at `INTENT_CPU_COST` and the creep-ticks at `creep_ucpu`; and the ratios follow.
/// Uses: CREEP_LIFE_TIME, INTENT_CPU_COST, SOURCE_ENERGY_*, STORAGE_CAPACITY, MOVE_COST_*.
#[test]
fn the_figures_add_up() {
    assert_eq!(intent_ucpu(), (INTENT_CPU_COST * 1e6).round() as u64);
    let cfg = REMOTE_MMO;
    for (tiles, rate) in [
        (25, MOVE_COST_PLAIN),
        (70, MOVE_COST_PLAIN),
        (50, MOVE_COST_SWAMP),
    ] {
        let sources = [source(tiles, rate, false), source(tiles + 9, rate, true)];
        let room = RemoteRoom {
            sources: &sources,
            controller: Some(way(tiles + 4, rate)),
        };
        for rcl in 4..=8 {
            for plan in RemotePlan::ALL {
                let p = remote_profit(&room, plan, home(rcl), &cfg).expect("a profit");
                assert_eq!(p.plan, plan);
                assert_eq!(p.gross, 2 * yield_per_life(plan.capacity()));
                assert_eq!(
                    i128::from(p.net),
                    i128::from(p.gross) - i128::from(p.bodies + p.upkeep + p.amortized)
                );
                assert_eq!(
                    p.cpu_ucpu,
                    p.intents * intent_ucpu() + p.creep_ticks * u64::from(cfg.creep_ucpu)
                );
                assert_eq!(
                    p.per_cpu(),
                    Some((i128::from(p.net) * 1_000_000 / i128::from(p.cpu_ucpu)) as i64)
                );
                assert_eq!(p.spent(), p.bodies + p.upkeep + p.amortized);
                assert_eq!(
                    p.per_energy_permille(),
                    Some((i128::from(p.net) * 1000 / i128::from(p.spent())) as i64)
                );
                // One container to build (the second stands), and the roads plan's new tiles.
                let container = u64::from(StructureType::Container.construction_cost().unwrap());
                let roads = if plan.roads {
                    sources[0].road_new.build() + sources[1].road_new.build()
                } else {
                    0
                };
                assert_eq!(p.build, container + roads);
                assert_eq!(
                    p.amortized,
                    (p.build * LIFE).div_ceil(u64::from(cfg.horizon_ticks))
                );
                assert!(p.haulers >= 2, "a hauler a source at least");
            }
        }
    }
}

/// A pinned derivation of the plan without a reserver or roads, one source on plain with its
/// container standing, from the model's stated rules: the miner's lives `LIFE / (LIFE − walk)`
/// (its body, spawn time and walk's moves with them), its harvests and repairs, the container's
/// repairs in energy, the haulers' bodies and each load's withdraw, transfer and moves.
/// Uses: CREEP_LIFE_TIME, CREEP_SPAWN_TIME, CARRY_CAPACITY, ENERGY_REGEN_TIME, HARVEST_POWER,
/// REPAIR_POWER, REPAIR_COST, SOURCE_ENERGY_NEUTRAL_CAPACITY, STORAGE_CAPACITY, MOVE_COST_PLAIN.
#[test]
fn one_neutral_source_by_hand() {
    let tiles = 40;
    let sources = [source(tiles, MOVE_COST_PLAIN, true)];
    let room = RemoteRoom {
        sources: &sources,
        controller: None,
    };
    let h = home(6);
    let cfg = REMOTE_MMO;
    let p = remote_profit(&room, NO_ROADS, h, &cfg).expect("a profit");
    let capacity = SOURCE_ENERGY_NEUTRAL_CAPACITY;
    let trip = sources[0].way.bare;
    let up = |x: u64, need: u64, stay: u64| (x * need).div_ceil(stay);

    let work = remote_miner_work(capacity).unwrap();
    let miner = miner_body_work(work, 1, capacity, trip, h.energy).unwrap();
    assert_eq!(count(&miner, Part::Carry), 1);
    let walk = u64::from(walk_ticks(trip, work, count(&miner, Part::Move)).unwrap());
    let stay = LIFE - walk;
    let hits = remote_container_hits(LIFE);
    let per_hit = (1.0 / f64::from(REPAIR_COST)).round() as u64;
    let repaired = hits.div_ceil(per_hit);
    let harvests = up(
        u64::from(capacity).div_ceil(u64::from(work * HARVEST_POWER)),
        LIFE,
        u64::from(ENERGY_REGEN_TIME),
    );
    let repairs = hits.div_ceil(u64::from(work * REPAIR_POWER));

    let (hauler, n) =
        hauler_plan(capacity, trip, h.energy, h.margin_pct, h.unload).expect("haulers");
    let load = u64::from(count(&hauler, Part::Carry) * CARRY_CAPACITY);
    let loads = (yield_per_life(capacity) - repaired).div_ceil(load);
    let per_load = 1 + 1 + 2 * u64::from(tiles); // withdraw, one transfer into the storage, moves

    let bodies = up(u64::from(miner.cost()), LIFE, stay) + u64::from(n * hauler.cost());
    let intents = up(u64::from(tiles), LIFE, stay) + harvests + repairs + loads * per_load;
    let spawn = up(u64::from(miner.len() as u32 * CREEP_SPAWN_TIME), LIFE, stay)
        + u64::from(n * hauler.len() as u32 * CREEP_SPAWN_TIME);
    let creep_ticks = up(LIFE, LIFE, stay) + u64::from(n) * LIFE;

    assert_eq!(p.gross, yield_per_life(capacity));
    assert_eq!(p.bodies, bodies);
    assert_eq!(p.upkeep, repaired);
    assert_eq!(
        (p.build, p.amortized),
        (0, 0),
        "the container stands, no roads"
    );
    assert_eq!(p.intents, intents);
    assert_eq!(p.spawn_ticks, spawn);
    assert_eq!(p.creep_ticks, creep_ticks);
    assert_eq!(p.haulers, n);
}

/// A reserver holds the reservation over the whole life: its creeps' CLAIM parts at the controller
/// add at least `CREEP_LIFE_TIME` reservation ticks, `CONTROLLER_RESERVE` a part a tick, and
/// reserving raises the yield by `SOURCE_ENERGY_CAPACITY / SOURCE_ENERGY_NEUTRAL_CAPACITY`.
/// Uses: CREEP_LIFE_TIME, CREEP_CLAIM_LIFE_TIME, CONTROLLER_RESERVE, SOURCE_ENERGY_*.
#[test]
fn a_reserver_holds_the_reservation() {
    let sources = [source(30, MOVE_COST_PLAIN, true)];
    for (tiles, rate) in [
        (10, MOVE_COST_PLAIN),
        (40, MOVE_COST_PLAIN),
        (40, MOVE_COST_SWAMP),
    ] {
        let ctrl = way(tiles, rate);
        let room = RemoteRoom {
            sources: &sources,
            controller: Some(ctrl),
        };
        for rcl in 3..=8 {
            let plan = RemotePlan {
                reserve: true,
                roads: false,
            };
            let Some(p) = remote_profit(&room, plan, home(rcl), &REMOTE_MMO) else {
                assert!(
                    energy_at(rcl) < Part::Claim.cost() + Part::Move.cost(),
                    "RCL{rcl}"
                );
                continue;
            };
            let bare = remote_profit(&room, NO_ROADS, home(rcl), &REMOTE_MMO).unwrap();
            assert_eq!(
                u128::from(p.gross) * u128::from(SOURCE_ENERGY_NEUTRAL_CAPACITY),
                u128::from(bare.gross) * u128::from(SOURCE_ENERGY_CAPACITY)
            );
            let body = reserver_body(ctrl.bare, home(rcl).energy).unwrap();
            let claim = u64::from(count(&body, Part::Claim));
            let need = LIFE.div_ceil(claim * u64::from(CONTROLLER_RESERVE));
            assert!(need * claim * u64::from(CONTROLLER_RESERVE) >= LIFE);
        }
    }
    // No controller, no reserver plan.
    let room = RemoteRoom {
        sources: &sources,
        controller: None,
    };
    let plan = RemotePlan {
        reserve: true,
        roads: false,
    };
    assert_eq!(remote_profit(&room, plan, home(8), &REMOTE_MMO), None);
}

/// Longer ways cost more: across a sweep of way lengths every plan's net energy falls (more MOVE
/// parts or haulers), and the longest way costs more CPU than the shortest (more moves a load).
/// The CPU need not rise at every step: the hauler solver takes the fewest haulers, then the
/// least energy, so at one count a longer way can take larger bodies, fewer loads and fewer
/// moves. A way so long the miner would walk its whole life is no plan at all.
/// Uses: CREEP_LIFE_TIME, MOVE_COST_*, STORAGE_CAPACITY.
#[test]
fn longer_ways_cost_more() {
    for rate in [MOVE_COST_PLAIN, MOVE_COST_SWAMP] {
        for rcl in [4, 6, 8] {
            for plan in RemotePlan::ALL {
                let mut seen: Vec<RemoteProfit> = Vec::new();
                for tiles in (10..=150).step_by(20) {
                    let sources = [source(tiles, rate, false), source(tiles, rate, false)];
                    let room = RemoteRoom {
                        sources: &sources,
                        controller: Some(way(tiles, rate)),
                    };
                    let p = remote_profit(&room, plan, home(rcl), &REMOTE_MMO).expect("a plan");
                    if let Some(prev) = seen.last() {
                        assert!(p.net < prev.net, "{rate} RCL{rcl} {plan:?} {tiles}");
                    }
                    seen.push(p);
                }
                let (first, last) = (seen.first().unwrap(), seen.last().unwrap());
                assert!(last.cpu_ucpu > first.cpu_ucpu, "{rate} RCL{rcl} {plan:?}");
            }
        }
    }
    let far = CREEP_LIFE_TIME;
    let sources = [source(far, MOVE_COST_PLAIN, true)];
    let room = RemoteRoom {
        sources: &sources,
        controller: None,
    };
    assert_eq!(remote_profit(&room, NO_ROADS, home(8), &REMOTE_MMO), None);
}

/// A home too small for the remote miner's body plans nothing.
/// Uses: BODYPART_COST, SOURCE_ENERGY_NEUTRAL_CAPACITY.
#[test]
fn a_small_home_plans_nothing() {
    let sources = [source(20, MOVE_COST_PLAIN, true)];
    let room = RemoteRoom {
        sources: &sources,
        controller: None,
    };
    let work = remote_miner_work(SOURCE_ENERGY_NEUTRAL_CAPACITY).unwrap();
    let least = work * Part::Work.cost() + Part::Carry.cost() + Part::Move.cost();
    let mut h = home(8);
    h.energy = least - 1;
    assert_eq!(remote_profit(&room, NO_ROADS, h, &REMOTE_MMO), None);
    assert_eq!(best_plan(&room, h, &REMOTE_MMO), None);
    h.energy = least;
    assert!(remote_profit(&room, NO_ROADS, h, &REMOTE_MMO).is_some());
}

/// On a swamp way roads pay: the paved haulers need a fraction of the MOVE parts
/// (`MOVE_COST_ROAD` against `MOVE_COST_SWAMP`), which outweighs the roads' build over the
/// default horizon and their upkeep, so the reserved plan with roads nets more and costs less CPU
/// than the one without, and [`best_plan`] takes it.
/// Uses: MOVE_COST_ROAD, MOVE_COST_SWAMP, CONSTRUCTION_COST_ROAD_SWAMP_RATIO, STORAGE_CAPACITY.
#[test]
fn roads_pay_on_swamp() {
    let sources = [
        source(60, MOVE_COST_SWAMP, false),
        source(60, MOVE_COST_SWAMP, false),
    ];
    let room = RemoteRoom {
        sources: &sources,
        controller: Some(way(60, MOVE_COST_SWAMP)),
    };
    for rcl in 5..=8 {
        let paved = RemotePlan {
            reserve: true,
            roads: true,
        };
        let bare = RemotePlan {
            reserve: true,
            roads: false,
        };
        let p = remote_profit(&room, paved, home(rcl), &REMOTE_MMO).unwrap();
        let b = remote_profit(&room, bare, home(rcl), &REMOTE_MMO).unwrap();
        assert!(p.net > b.net, "RCL{rcl}");
        assert!(p.cpu_ucpu < b.cpu_ucpu, "RCL{rcl}");
        assert!(p.spawn_ticks < b.spawn_ticks, "RCL{rcl}");
        assert_eq!(
            best_plan(&room, home(rcl), &REMOTE_MMO).map(|r| r.plan),
            Some(paved)
        );
    }
}

/// [`best_plan`] over a sweep of rooms: its plan nets more than 0 and within the slack of the best
/// net, and no other plan that does both costs less CPU, or as little CPU with more net. With no
/// slack it is the best net. With every plan at a loss it is `None`.
/// Uses: MOVE_COST_*, STORAGE_CAPACITY, the config's slack.
#[test]
fn the_best_plan_takes_the_least_cpu_within_the_slack() {
    for slack_pct in [0, REMOTE_MMO.slack_pct, 10] {
        let cfg = RemoteConfig {
            slack_pct,
            ..REMOTE_MMO
        };
        for rate in [MOVE_COST_PLAIN, MOVE_COST_SWAMP] {
            for tiles in [15, 45, 90] {
                for rcl in [4, 6, 8] {
                    let sources = [source(tiles, rate, false), source(tiles + 7, rate, true)];
                    let room = RemoteRoom {
                        sources: &sources,
                        controller: Some(way(tiles, rate)),
                    };
                    let all: Vec<RemoteProfit> = RemotePlan::ALL
                        .iter()
                        .filter_map(|&plan| remote_profit(&room, plan, home(rcl), &cfg))
                        .filter(|p| p.net > 0)
                        .collect();
                    let best = best_plan(&room, home(rcl), &cfg).expect("a plan");
                    let top = all.iter().max_by_key(|p| p.net).unwrap();
                    let floor = top.net - (top.gross * u64::from(slack_pct) / 100) as i64;
                    assert!(best.net > 0 && best.net >= floor);
                    for p in all.iter().filter(|p| p.net >= floor) {
                        assert!(
                            (best.cpu_ucpu, -best.net) <= (p.cpu_ucpu, -p.net),
                            "{p:?} beats {best:?}"
                        );
                    }
                    if slack_pct == 0 {
                        assert_eq!(best.net, top.net);
                    }
                }
            }
        }
    }
    // At a loss: a costly horizon (every build charged in one life) and a long swamp way at RCL4.
    let cfg = RemoteConfig {
        horizon_ticks: CREEP_LIFE_TIME,
        ..REMOTE_MMO
    };
    let sources = [source(140, MOVE_COST_SWAMP, false)];
    let room = RemoteRoom {
        sources: &sources,
        controller: Some(way(140, MOVE_COST_SWAMP)),
    };
    for plan in RemotePlan::ALL {
        if let Some(p) = remote_profit(&room, plan, home(4), &cfg) {
            assert!(p.net <= 0, "{plan:?} nets {}", p.net);
        }
    }
    assert_eq!(best_plan(&room, home(4), &cfg), None);
}

/// Standing builds are sunk: with the container standing and every road tile built, nothing is
/// charged for builds; each new road tile adds its terrain's `CONSTRUCTION_COST`.
/// Uses: CONSTRUCTION_COST, CONSTRUCTION_COST_ROAD_SWAMP_RATIO.
#[test]
fn standing_builds_are_not_charged() {
    let mut s = source(40, MOVE_COST_SWAMP, true);
    s.road_new = RoadTiles::NONE;
    let roads = RemotePlan {
        reserve: true,
        roads: true,
    };
    let room = RemoteRoom {
        sources: core::slice::from_ref(&s),
        controller: Some(way(40, MOVE_COST_SWAMP)),
    };
    let p = remote_profit(&room, roads, home(7), &REMOTE_MMO).unwrap();
    assert_eq!((p.build, p.amortized), (0, 0));
    s.road_new = RoadTiles {
        swamp: 3,
        ..RoadTiles::NONE
    };
    let room = RemoteRoom {
        sources: core::slice::from_ref(&s),
        controller: Some(way(40, MOVE_COST_SWAMP)),
    };
    let q = remote_profit(&room, roads, home(7), &REMOTE_MMO).unwrap();
    let road = u64::from(StructureType::Road.construction_cost().unwrap());
    assert_eq!(
        q.build,
        3 * road * u64::from(CONSTRUCTION_COST_ROAD_SWAMP_RATIO)
    );
    assert!(q.net < p.net);
}
