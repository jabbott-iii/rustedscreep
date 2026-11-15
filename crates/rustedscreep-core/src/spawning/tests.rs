//! The planner over scripted snapshots: priority, the room's energy shared by its spawns, waiting,
//! unfit requests, holds and probation after a refused call, names, the per-tick order cap, and
//! determinism. Expectations derive from `BODYPART_COST`, `SPAWN_ENERGY_CAPACITY`,
//! `CONTROLLER_STRUCTURES`, `EXTENSION_ENERGY_CAPACITY`, `INTENT_CPU_COST` and the config.

use rustedscreep_consts::{
    ErrorCode, INTENT_CPU_COST, Part, Position, RoomCoordinate, RoomName, SPAWN_ENERGY_CAPACITY,
    StructureType, extension_energy_capacity,
};

use super::*;
use crate::config::SPAWN_MMO;
use crate::intents::Outcome;
use crate::scheduler::Commit;
use crate::snapshot::{
    Controller, ObjectKey, RoomEnergy, Snapshot, StructureHandle, StructureStatic, TickContext,
};

const T0: u32 = 50_000_000;

fn room(name: &str) -> RoomName {
    name.parse().expect("room name")
}

fn at(name: &str, x: u8, y: u8) -> Position {
    Position::new(
        RoomCoordinate::new(x).expect("x"),
        RoomCoordinate::new(y).expect("y"),
        room(name),
    )
}

fn unit_cost(pattern: &[Part]) -> u32 {
    pattern.iter().map(|p| p.cost()).sum()
}

/// A spawn: its id, room, tile and whether it is spawning.
#[derive(Clone, Copy)]
struct SpawnAt {
    key: u128,
    room: &'static str,
    x: u8,
    busy: bool,
}

const fn spawn(key: u128, room: &'static str, x: u8) -> SpawnAt {
    SpawnAt {
        key,
        room,
        x,
        busy: false,
    }
}

/// Takes `s` at `time`, the way rustedscreep-wasm does: owned rooms with their energy and the
/// spawns (pushed in the given order, which is the engine's object order), plus an extension that
/// is not a spawn. Reusing one snapshot across ticks keeps the handles, as in the game loop.
fn fill(s: &mut Snapshot, time: u32, rooms: &[(&str, RoomEnergy)], spawns: &[SpawnAt]) {
    s.begin(TickContext::from_raw(time, 20, 500.0, 10_000));
    for &(name, energy) in rooms {
        let row = s.rooms.push(room(name), Controller::Mine { level: 3 });
        s.rooms.set_energy(row, energy);
    }
    let ext = StructureStatic {
        kind: StructureType::Extension,
        pos: at(rooms.first().map_or("W1N1", |r| r.0), 5, 5),
    };
    s.structures
        .push(ObjectKey(9_999), || Ok(ext))
        .expect("extension");
    for sp in spawns {
        let st = StructureStatic {
            kind: StructureType::Spawn,
            pos: at(sp.room, sp.x, 20),
        };
        let handle = s
            .structures
            .push(ObjectKey(sp.key), || Ok(st))
            .expect("spawn");
        let row = u32::try_from(s.structures.len() - 1).expect("row");
        s.spawns.push(handle, row, sp.busy);
    }
    s.finish();
}

/// A new snapshot filled at `time` (see [`fill`]).
fn snap_at(time: u32, rooms: &[(&str, RoomEnergy)], spawns: &[SpawnAt]) -> Snapshot {
    let mut s = Snapshot::default();
    fill(&mut s, time, rooms, spawns);
    s
}

fn energy(available: u32, capacity: u32) -> RoomEnergy {
    RoomEnergy {
        available,
        capacity,
    }
}

fn worker(home: &str, budget: EnergyBudget, priority: u8) -> SpawnRequest {
    SpawnRequest {
        home: room(home),
        role: Role::Worker,
        pattern: &WORKER_PATTERN,
        max_repeats: u8::MAX,
        budget,
        priority,
    }
}

/// Runs a tick: begin, the requests, close, then every room planned in order.
fn plan(sp: &mut Spawner, snap: &Snapshot, requests: &[SpawnRequest]) -> Commit {
    sp.begin(snap);
    for &r in requests {
        sp.request(r).expect("open");
    }
    let mut commit = Commit::default();
    for unit in 0..sp.close(snap) {
        sp.plan_room(snap, unit, &mut commit).expect("plan");
    }
    commit
}

fn handle(snap: &Snapshot, key: u128) -> StructureHandle {
    snap.structures
        .handle_of(ObjectKey(key))
        .expect("spawn handle")
}

#[test]
fn an_idle_spawn_takes_the_top_request_sized_to_capacity() {
    let unit = unit_cost(&WORKER_PATTERN);
    // A spawn and RCL3's extensions (CONTROLLER_STRUCTURES, EXTENSION_ENERGY_CAPACITY).
    let cap = SPAWN_ENERGY_CAPACITY
        + StructureType::Extension.controller_structures(3) * extension_energy_capacity(3);
    let snap = snap_at(T0, &[("W1N1", energy(cap, cap))], &[spawn(1, "W1N1", 20)]);
    let mut sp = Spawner::default();
    let low = worker("W1N1", EnergyBudget::Capacity, 1);
    let high = worker("W1N1", EnergyBudget::Capacity, 5);
    let commit = plan(&mut sp, &snap, &[low, high]);
    let [order] = sp.orders() else {
        panic!("one order: {:?}", sp.orders())
    };
    assert_eq!(order.request, 1, "the higher priority first");
    assert_eq!(order.spawn, handle(&snap, 1));
    assert_eq!(order.body.len(), 3 * (cap / unit) as usize);
    assert_eq!(order.body.cost(), unit * (cap / unit));
    assert_eq!(commit.cpu(), INTENT_CPU_COST, "one intent committed");
    let id = CreepName::parse(order.name.as_bytes()).expect("the name parses");
    assert_eq!(
        id,
        Identity {
            role: Role::Worker,
            home: room("W1N1"),
            serial: serial(T0, 0).expect("serial")
        }
    );
    assert_eq!((sp.stats().requests, sp.stats().orders), (2, 1));
}

#[test]
fn a_capacity_request_waits_for_energy_and_blocks_lower_ones() {
    let unit = unit_cost(&WORKER_PATTERN);
    // Room for three repeats; the top request takes at most two (its body costs `cost`).
    let cap = 3 * unit;
    let cost = 2 * unit;
    let mut snap = snap_at(
        T0,
        &[("W1N1", energy(cost - 1, cap))],
        &[spawn(1, "W1N1", 20), spawn(2, "W1N1", 30)],
    );
    let mut sp = Spawner::default();
    let top = SpawnRequest {
        max_repeats: 2,
        ..worker("W1N1", EnergyBudget::Capacity, 9)
    };
    let cheap = SpawnRequest {
        max_repeats: 1,
        ..worker("W1N1", EnergyBudget::Available, 0)
    };
    let commit = plan(&mut sp, &snap, &[cheap, top]);
    assert!(sp.orders().is_empty(), "{:?}", sp.orders());
    assert_eq!(sp.stats().waiting, 1);
    assert_eq!(commit.cpu(), 0.0);

    // Once the room holds the cost, the top request spawns and the next one waits for the rest
    // (one repeat less one energy is left, below the capacity).
    fill(
        &mut snap,
        T0 + 1,
        &[("W1N1", energy(cost + unit - 1, cap))],
        &[spawn(1, "W1N1", 20), spawn(2, "W1N1", 30)],
    );
    plan(&mut sp, &snap, &[cheap, top]);
    let [order] = sp.orders() else {
        panic!("one order: {:?}", sp.orders())
    };
    assert_eq!((order.request, order.body.cost()), (1, cost));
    assert_eq!(sp.stats().waiting, 1, "the cheap one waits for one repeat");
}

#[test]
fn spawns_of_one_room_share_its_energy() {
    let unit = unit_cost(&WORKER_PATTERN);
    let snap = snap_at(
        T0,
        &[("W1N1", energy(3 * unit, 3 * unit))],
        &[
            spawn(3, "W1N1", 40),
            spawn(1, "W1N1", 20),
            spawn(2, "W1N1", 30),
        ],
    );
    let mut sp = Spawner::default();
    let two = SpawnRequest {
        max_repeats: 2,
        ..worker("W1N1", EnergyBudget::Available, 1)
    };
    let one = SpawnRequest {
        max_repeats: 1,
        ..worker("W1N1", EnergyBudget::Available, 1)
    };
    let commit = plan(&mut sp, &snap, &[two, one, one]);
    let orders = sp.orders();
    assert_eq!(orders.len(), 2, "{orders:?}");
    let spent: u32 = orders.iter().map(|o| o.body.cost()).sum();
    assert!(spent <= 3 * unit, "never above what the room holds");
    assert_eq!(spent, 3 * unit);
    // Spawns take requests in id order, whatever the engine's object order.
    assert_eq!(orders[0].spawn, handle(&snap, 1));
    assert_eq!(orders[1].spawn, handle(&snap, 2));
    assert_eq!(sp.stats().waiting, 1, "the third request waits");
    assert_eq!(commit.cpu(), 2.0 * INTENT_CPU_COST);
    assert_ne!(orders[0].name, orders[1].name, "names differ within a tick");
}

#[test]
fn busy_spawns_and_other_rooms_are_left_alone() {
    let unit = unit_cost(&WORKER_PATTERN);
    let busy = SpawnAt {
        busy: true,
        ..spawn(1, "W1N1", 20)
    };
    let snap = snap_at(
        T0,
        &[("W1N1", energy(unit, unit)), ("W2N1", energy(unit, unit))],
        &[busy, spawn(2, "W2N1", 20)],
    );
    let mut sp = Spawner::default();
    plan(
        &mut sp,
        &snap,
        &[
            worker("W1N1", EnergyBudget::Available, 1),
            worker("W3N1", EnergyBudget::Available, 1),
        ],
    );
    assert!(
        sp.orders().is_empty(),
        "W1N1's spawn is busy, W2N1 has no request"
    );
    // By packed name: W2N1's room x (-3) is below W1N1's (-2).
    assert_eq!(sp.spawn_rooms(), [room("W2N1"), room("W1N1")]);
    assert!(
        sp.rooms().is_empty(),
        "W1N1 has a request but no ready spawn, W2N1 a ready spawn but no request: no unit"
    );
    plan(
        &mut sp,
        &snap,
        &[worker("W2N1", EnergyBudget::Available, 1)],
    );
    assert_eq!(sp.rooms(), [room("W2N1")], "a request and a ready spawn");
    plan(&mut sp, &snap, &[]);
    assert!(sp.rooms().is_empty(), "nothing requested, no unit");
}

/// A room whose every spawn is busy or held is no unit of the job (it would only read the clock),
/// and a spawn on probation is ready.
#[test]
fn a_room_with_every_spawn_busy_or_held_is_no_unit() {
    let unit = unit_cost(&WORKER_PATTERN);
    let busy = SpawnAt {
        busy: true,
        ..spawn(2, "W1N1", 30)
    };
    let spawns = [spawn(1, "W1N1", 20), busy];
    let req = worker("W1N1", EnergyBudget::Available, 1);
    let mut sp = Spawner::default();
    let mut snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    assert_eq!(sp.orders().len(), 1);
    sp.record(0, Outcome::Failed(ErrorCode::RclNotEnough), &SPAWN_MMO);
    fill(&mut snap, T0 + 1, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    assert!(sp.rooms().is_empty(), "spawn 1 held, spawn 2 busy");
    assert!(sp.orders().is_empty());
    assert_eq!(sp.stats().held, 1, "counted although the room is no unit");
    let h1 = handle(&snap, 1);
    fill(
        &mut snap,
        T0 + SPAWN_MMO.hold_ticks + 1,
        &[("W1N1", energy(unit, unit))],
        &spawns,
    );
    plan(&mut sp, &snap, &[req]);
    assert!(sp.on_probation(h1) && !sp.is_held(h1));
    assert_eq!(sp.rooms(), [room("W1N1")], "a spawn on probation is ready");
    assert_eq!(sp.orders()[0].spawn, h1, "the only ready spawn takes it");
}

#[test]
fn a_pattern_beyond_the_capacity_is_skipped() {
    let unit = unit_cost(&WORKER_PATTERN);
    let snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &[spawn(1, "W1N1", 20)]);
    let mut sp = Spawner::default();
    static CLAIMER: [Part; 2] = [Part::Claim, Part::Move];
    assert!(
        unit_cost(&CLAIMER) > unit,
        "the test needs a pattern above the capacity"
    );
    let claimer = SpawnRequest {
        pattern: &CLAIMER,
        ..worker("W1N1", EnergyBudget::Capacity, 9)
    };
    plan(
        &mut sp,
        &snap,
        &[claimer, worker("W1N1", EnergyBudget::Capacity, 1)],
    );
    let [order] = sp.orders() else {
        panic!("one order: {:?}", sp.orders())
    };
    assert_eq!(order.request, 1);
    assert_eq!(sp.stats().unfit, 1);
}

#[test]
fn a_refused_call_holds_the_spawn_for_hold_ticks() {
    let unit = unit_cost(&WORKER_PATTERN);
    let cfg = SPAWN_MMO;
    let spawns = [spawn(1, "W1N1", 20), spawn(2, "W1N1", 30)];
    let req = SpawnRequest {
        max_repeats: 1,
        ..worker("W1N1", EnergyBudget::Available, 1)
    };
    let mut sp = Spawner::default();
    let mut snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    assert_eq!(sp.orders()[0].spawn, handle(&snap, 1));
    sp.record(0, Outcome::Failed(ErrorCode::RclNotEnough), &cfg);
    assert_eq!(sp.stats().failed, 1);
    assert!(sp.last_failure().is_some());

    // While held, the other spawn takes the request.
    let (h1, h2) = (handle(&snap, 1), handle(&snap, 2));
    for t in 1..=cfg.hold_ticks {
        fill(&mut snap, T0 + t, &[("W1N1", energy(unit, unit))], &spawns);
        plan(&mut sp, &snap, &[req]);
        assert_eq!(sp.orders()[0].spawn, h2, "tick {t}");
        assert_eq!(sp.stats().held, 1);
        assert!(sp.is_held(h1));
    }
    // Once the hold ends the first spawn is on probation: the working one still goes first, so a
    // spawn that keeps refusing never takes the top request from one that works.
    fill(
        &mut snap,
        T0 + cfg.hold_ticks + 1,
        &[("W1N1", energy(2 * unit, 2 * unit))],
        &spawns,
    );
    plan(&mut sp, &snap, &[req, req]);
    assert!(!sp.is_held(h1) && sp.on_probation(h1));
    let order: Vec<StructureHandle> = sp.orders().iter().map(|o| o.spawn).collect();
    assert_eq!(order, [h2, h1], "the spawn on probation last");
    assert_eq!(sp.stats().held, 0);
    // An accepted call of its own ends the probation: id order again.
    sp.record(0, Outcome::Ok, &cfg);
    sp.record(1, Outcome::Ok, &cfg);
    assert!(!sp.on_probation(h1) && !sp.is_held(h1));
    assert_eq!((sp.totals().ok, sp.totals().failed), (2, 1));
    assert_eq!(sp.last_ok().map(|e| e.name), Some(sp.orders()[1].name));
    fill(
        &mut snap,
        T0 + cfg.hold_ticks + 2,
        &[("W1N1", energy(unit, unit))],
        &spawns,
    );
    plan(&mut sp, &snap, &[req]);
    assert_eq!(sp.orders()[0].spawn, h1);
}

/// A refusal while on probation holds the spawn again for the full hold, from that tick.
#[test]
fn a_refusal_on_probation_holds_again() {
    let unit = unit_cost(&WORKER_PATTERN);
    let cfg = SPAWN_MMO;
    let spawns = [spawn(1, "W1N1", 20)];
    let req = worker("W1N1", EnergyBudget::Available, 1);
    let mut sp = Spawner::default();
    let mut snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    let h = handle(&snap, 1);
    sp.record(0, Outcome::Failed(ErrorCode::RclNotEnough), &cfg);
    let again = T0 + cfg.hold_ticks + 1;
    fill(&mut snap, again, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    assert!(sp.on_probation(h));
    assert_eq!(
        sp.orders().len(),
        1,
        "the only spawn, on probation, takes it"
    );
    sp.record(0, Outcome::Failed(ErrorCode::RclNotEnough), &cfg);
    for t in 1..=cfg.hold_ticks {
        fill(
            &mut snap,
            again + t,
            &[("W1N1", energy(unit, unit))],
            &spawns,
        );
        plan(&mut sp, &snap, &[req]);
        assert!(sp.is_held(h), "held again, tick {t}");
        assert!(sp.orders().is_empty() && sp.rooms().is_empty());
        assert_eq!(sp.stats().held, 1, "close counts the held spawn");
    }
    fill(
        &mut snap,
        again + cfg.hold_ticks + 1,
        &[("W1N1", energy(unit, unit))],
        &spawns,
    );
    plan(&mut sp, &snap, &[req]);
    assert_eq!(sp.orders().len(), 1, "on probation again");
    assert_eq!(sp.totals().failed, 2);
}

/// A request no body of which fits the room's capacity is skipped whatever its budget (one repeat
/// above the capacity, or no repeat allowed), so it never blocks the requests below it.
#[test]
fn a_request_that_can_never_fit_is_skipped_whatever_its_budget() {
    let unit = unit_cost(&WORKER_PATTERN);
    let snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &[spawn(1, "W1N1", 20)]);
    static CLAIMER: [Part; 2] = [Part::Claim, Part::Move];
    assert!(unit_cost(&CLAIMER) > unit);
    let never = [
        SpawnRequest {
            pattern: &CLAIMER,
            ..worker("W1N1", EnergyBudget::Available, 9)
        },
        SpawnRequest {
            max_repeats: 0,
            ..worker("W1N1", EnergyBudget::Available, 8)
        },
        SpawnRequest {
            max_repeats: 0,
            ..worker("W1N1", EnergyBudget::Capacity, 7)
        },
    ];
    for bad in never {
        let mut sp = Spawner::default();
        plan(
            &mut sp,
            &snap,
            &[bad, worker("W1N1", EnergyBudget::Available, 1)],
        );
        let [order] = sp.orders() else {
            panic!("{bad:?}: one order, got {:?}", sp.orders())
        };
        assert_eq!(order.request, 1, "{bad:?} must not block the next request");
        assert_eq!((sp.stats().unfit, sp.stats().waiting), (1, 0), "{bad:?}");
    }
}

/// Requests whose home has no own spawn are counted, and no unit is made for them.
#[test]
fn requests_for_a_room_without_a_spawn_are_counted() {
    let unit = unit_cost(&WORKER_PATTERN);
    let snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &[spawn(1, "W1N1", 20)]);
    let mut sp = Spawner::default();
    plan(
        &mut sp,
        &snap,
        &[
            worker("W5N5", EnergyBudget::Available, 9),
            worker("W5N5", EnergyBudget::Capacity, 9),
        ],
    );
    assert_eq!(sp.stats().no_spawn, 2);
    assert!(sp.rooms().is_empty() && sp.orders().is_empty());
}

/// More than `ORDERS_PER_TICK` orders in one tick cannot all be named: the order past the cap is a
/// fault of its room, and the orders before it stand with distinct names.
#[test]
fn orders_past_the_per_tick_cap_fault() {
    let unit = unit_cost(&WORKER_PATTERN);
    let count = ORDERS_PER_TICK + 1;
    let spawns: Vec<SpawnAt> = (0..count)
        .map(|i| spawn(u128::from(i) + 1, "W1N1", 20))
        .collect();
    let snap = snap_at(T0, &[("W1N1", energy(count * unit, count * unit))], &spawns);
    let mut sp = Spawner::default();
    sp.begin(&snap);
    let req = SpawnRequest {
        max_repeats: 1,
        ..worker("W1N1", EnergyBudget::Available, 1)
    };
    for _ in 0..count {
        sp.request(req).expect("open");
    }
    assert_eq!(sp.close(&snap), 1);
    let mut commit = Commit::default();
    assert!(sp.plan_room(&snap, 0, &mut commit).is_err());
    assert_eq!(sp.orders().len(), ORDERS_PER_TICK as usize);
    let names: std::collections::HashSet<_> = sp.orders().iter().map(|o| o.identity).collect();
    assert_eq!(names.len(), ORDERS_PER_TICK as usize, "distinct names");
    for o in sp.orders() {
        assert_eq!(CreepName::parse(o.name.as_bytes()), Some(o.identity));
    }
}

#[test]
fn a_gone_spawn_loses_its_hold() {
    let unit = unit_cost(&WORKER_PATTERN);
    let mut sp = Spawner::default();
    let mut snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &[spawn(1, "W1N1", 20)]);
    let req = worker("W1N1", EnergyBudget::Available, 1);
    plan(&mut sp, &snap, &[req]);
    let h = handle(&snap, 1);
    sp.record(0, Outcome::Threw, &SPAWN_MMO);
    assert!(sp.is_held(h));
    fill(&mut snap, T0 + 1, &[("W1N1", energy(unit, unit))], &[]);
    plan(&mut sp, &snap, &[req]);
    assert!(!sp.is_held(h), "the spawn is gone");
}

#[test]
fn requests_close_once_planning_starts() {
    let unit = unit_cost(&WORKER_PATTERN);
    let snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &[spawn(1, "W1N1", 20)]);
    let mut sp = Spawner::default();
    assert_eq!(
        sp.request(worker("W1N1", EnergyBudget::Available, 1)),
        None,
        "before begin"
    );
    plan(&mut sp, &snap, &[]);
    assert_eq!(sp.request(worker("W1N1", EnergyBudget::Available, 1)), None);
    let stale = snap_at(
        T0 + 1,
        &[("W1N1", energy(unit, unit))],
        &[spawn(1, "W1N1", 20)],
    );
    let mut commit = Commit::default();
    assert!(
        sp.plan_room(&stale, 0, &mut commit).is_err(),
        "planned for another tick"
    );
    sp.begin(&stale);
    sp.request(worker("W1N1", EnergyBudget::Available, 1))
        .expect("open");
    assert!(
        sp.plan_room(&stale, 0, &mut commit).is_err(),
        "planned before close"
    );
    assert_eq!(sp.close(&stale), 1);
    assert!(sp.plan_room(&stale, 0, &mut commit).is_ok());
}

/// The same snapshot and requests give the same orders, and a request pushed later with the same
/// priority comes later.
#[test]
fn planning_is_deterministic() {
    let unit = unit_cost(&WORKER_PATTERN);
    let snap = snap_at(
        T0,
        &[
            ("W2N1", energy(9 * unit, 9 * unit)),
            ("W1N1", energy(5 * unit, 9 * unit)),
        ],
        &[
            spawn(5, "W2N1", 20),
            spawn(4, "W1N1", 20),
            spawn(6, "W1N1", 22),
        ],
    );
    let requests = [
        worker("W1N1", EnergyBudget::Available, 2),
        worker("W2N1", EnergyBudget::Capacity, 2),
        SpawnRequest {
            max_repeats: 2,
            ..worker("W1N1", EnergyBudget::Available, 2)
        },
    ];
    let mut a = Spawner::default();
    let mut b = Spawner::default();
    plan(&mut a, &snap, &requests);
    plan(&mut b, &snap, &requests);
    assert_eq!(a.orders(), b.orders());
    let rooms: Vec<RoomName> = a
        .orders()
        .iter()
        .map(|o| CreepName::parse(o.name.as_bytes()).expect("name").home)
        .collect();
    assert_eq!(rooms, [room("W2N1"), room("W1N1")], "rooms by packed name");
    assert_eq!(a.orders()[1].request, 0, "the earlier of equal priorities");
    assert_eq!(
        a.orders()[1].body.cost(),
        5 * unit,
        "W1N1 spends what it holds"
    );
    assert_eq!(
        a.stats().waiting,
        1,
        "W1N1's second spawn finds nothing left"
    );
}

/// At the safe range's minimum, a hold still keeps the spawn out for one tick.
#[test]
fn the_shortest_hold_lasts_one_tick() {
    let unit = unit_cost(&WORKER_PATTERN);
    let cfg = crate::config::SpawnConfig { hold_ticks: 1 };
    let spawns = [spawn(1, "W1N1", 20)];
    let req = worker("W1N1", EnergyBudget::Available, 1);
    let mut sp = Spawner::default();
    let mut snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    let h = handle(&snap, 1);
    sp.record(0, Outcome::Failed(ErrorCode::RclNotEnough), &cfg);
    fill(&mut snap, T0 + 1, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    assert!(
        sp.is_held(h) && sp.orders().is_empty(),
        "held on the next tick"
    );
    fill(&mut snap, T0 + 2, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    assert!(sp.on_probation(h));
    assert_eq!(sp.orders().len(), 1, "back the tick after");
}

/// A released spawn takes orders on the next tick, with no hold and no probation.
#[test]
fn a_released_spawn_takes_orders_at_once() {
    let unit = unit_cost(&WORKER_PATTERN);
    let spawns = [spawn(1, "W1N1", 20), spawn(2, "W1N1", 30)];
    let req = worker("W1N1", EnergyBudget::Available, 1);
    let mut sp = Spawner::default();
    let mut snap = snap_at(T0, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    let h1 = handle(&snap, 1);
    sp.record(0, Outcome::Threw, &SPAWN_MMO);
    assert!(sp.is_held(h1));
    sp.release(h1);
    assert!(!sp.is_held(h1) && !sp.on_probation(h1));
    fill(&mut snap, T0 + 1, &[("W1N1", energy(unit, unit))], &spawns);
    plan(&mut sp, &snap, &[req]);
    assert_eq!(sp.orders()[0].spawn, h1, "first by id again");
}
