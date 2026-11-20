//! The queue over scripted snapshots: priority, the room's energy shared by its spawns, waiting,
//! unfit requests, holds and probation after a refused call, names, the per-tick order cap,
//! determinism, and the global queue of P5.1 (`room_distance`, the serving room of a home without a
//! spawn, help only from a full room while allowed, own requests first, `spawn_side`). Expectations
//! derive from `BODYPART_COST`, `SPAWN_ENERGY_CAPACITY`, `CONTROLLER_STRUCTURES`,
//! `EXTENSION_ENERGY_CAPACITY`, `INTENT_CPU_COST`, `ROOM_SIZE`, `CREEP_LIFE_TIME` and the config.

use rustedscreep_consts::{
    CREEP_LIFE_TIME, ErrorCode, INTENT_CPU_COST, Part, Position, ROOM_SIZE, RoomCoordinate,
    RoomName, SPAWN_ENERGY_CAPACITY, StructureType, extension_energy_capacity,
};

use super::*;

/// A pattern for the spawner's scaled bodies (`BodySpec::Scaled`): one each of WORK, CARRY and
/// MOVE. No job spawns a fixed pattern since P3.6 (`economy::sizing`); the spawner still scales
/// one when asked.
const WORKER_PATTERN: [Part; 3] = [Part::Work, Part::Carry, Part::Move];
use crate::config::{SPAWN_MMO, SpawnConfig};
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
    let rooms: Vec<(&str, RoomEnergy, Controller)> = rooms
        .iter()
        .map(|&(name, e)| (name, e, Controller::Mine { level: 3 }))
        .collect();
    fill_rooms(s, time, &rooms, spawns);
}

/// [`fill`] with each room's controller given (a visible room the bot does not own).
fn fill_rooms(
    s: &mut Snapshot,
    time: u32,
    rooms: &[(&str, RoomEnergy, Controller)],
    spawns: &[SpawnAt],
) {
    s.begin(TickContext::from_raw(time, 20, 500.0, 10_000));
    for &(name, energy, controller) in rooms {
        let row = s.rooms.push(room(name), controller);
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

/// `req` with at most `max_repeats` repeats of its pattern.
fn capped(req: SpawnRequest, max_repeats: u8) -> SpawnRequest {
    let BodySpec::Scaled { pattern, .. } = req.body else {
        panic!("a scaled request")
    };
    SpawnRequest {
        body: BodySpec::Scaled {
            pattern,
            max_repeats,
        },
        ..req
    }
}

/// `req` with `pattern` in place of its own.
fn patterned(req: SpawnRequest, pattern: &'static [Part]) -> SpawnRequest {
    let BodySpec::Scaled { max_repeats, .. } = req.body else {
        panic!("a scaled request")
    };
    SpawnRequest {
        body: BodySpec::Scaled {
            pattern,
            max_repeats,
        },
        ..req
    }
}

fn worker(home: &str, budget: EnergyBudget, priority: u8) -> SpawnRequest {
    SpawnRequest {
        home: room(home),
        role: Role::Worker,
        body: BodySpec::Scaled {
            pattern: &WORKER_PATTERN,
            max_repeats: u8::MAX,
        },
        budget,
        priority,
    }
}

/// Runs a tick: begin, the requests, close, then every room planned in order.
fn plan(sp: &mut Spawner, snap: &Snapshot, requests: &[SpawnRequest]) -> Commit {
    plan_with(sp, snap, requests, &SPAWN_MMO, true)
}

/// [`plan`] with `cfg` in place of the profile's spawn values, and `help` for whether rooms may
/// serve others (the Standard tier runs).
fn plan_with(
    sp: &mut Spawner,
    snap: &Snapshot,
    requests: &[SpawnRequest],
    cfg: &SpawnConfig,
    help: bool,
) -> Commit {
    sp.begin(snap, cfg);
    for &r in requests {
        sp.request(r).expect("open");
    }
    let mut commit = Commit::default();
    for unit in 0..sp.close(snap, help) {
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
    let top = capped(worker("W1N1", EnergyBudget::Capacity, 9), 2);
    let cheap = capped(worker("W1N1", EnergyBudget::Available, 0), 1);
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

/// Uses: BODYPART_COST (through the pattern's cost). `UpTo(cap)` sizes to whole repeats within the
/// cap and the capacity and waits for that cost; below one repeat it is the one-repeat body.
#[test]
fn an_up_to_request_sizes_within_its_cap_and_waits_for_it() {
    let unit = unit_cost(&WORKER_PATTERN);
    let cap = 4 * unit;
    for (limit, repeats) in [(3 * unit + unit / 2, 3), (9 * unit, 4), (unit / 2, 1)] {
        let req = worker("W1N1", EnergyBudget::UpTo(limit), 1);
        let cost = repeats * unit;
        // One energy short: it waits.
        let mut snap = snap_at(
            T0,
            &[("W1N1", energy(cost - 1, cap))],
            &[spawn(1, "W1N1", 20)],
        );
        let mut sp = Spawner::default();
        plan(&mut sp, &snap, &[req]);
        assert!(sp.orders().is_empty(), "cap {limit}: {:?}", sp.orders());
        assert_eq!(sp.stats().waiting, 1);
        fill(
            &mut snap,
            T0 + 1,
            &[("W1N1", energy(cost, cap))],
            &[spawn(1, "W1N1", 20)],
        );
        plan(&mut sp, &snap, &[req]);
        let [order] = sp.orders() else {
            panic!("cap {limit}: one order: {:?}", sp.orders())
        };
        assert_eq!(order.body.cost(), cost, "cap {limit}");
    }
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
    let two = capped(worker("W1N1", EnergyBudget::Available, 1), 2);
    let one = capped(worker("W1N1", EnergyBudget::Available, 1), 1);
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
    // W9N1 is owned, has no spawn and is beyond help range of both spawn rooms.
    for r in ["W1N1", "W2N1"] {
        assert!(room_distance(room("W9N1"), room(r)) > SPAWN_MMO.help_range);
    }
    let busy = SpawnAt {
        busy: true,
        ..spawn(1, "W1N1", 20)
    };
    let snap = snap_at(
        T0,
        &[
            ("W1N1", energy(unit, unit)),
            ("W2N1", energy(unit, unit)),
            ("W9N1", CLAIMED),
        ],
        &[busy, spawn(2, "W2N1", 20)],
    );
    let mut sp = Spawner::default();
    plan(
        &mut sp,
        &snap,
        &[
            worker("W1N1", EnergyBudget::Available, 1),
            worker("W9N1", EnergyBudget::Available, 1),
        ],
    );
    assert!(
        sp.orders().is_empty(),
        "W1N1's spawn is busy, W2N1 serves no request"
    );
    assert_eq!((sp.stats().no_spawn, sp.stats().helped), (1, 0));
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
    let claimer = patterned(worker("W1N1", EnergyBudget::Capacity, 9), &CLAIMER);
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

/// An exact body (P3.1's miners) is that body whatever the budget: it waits until the room holds
/// its cost, and is skipped as unfit, without blocking the requests below it, when it costs more
/// than the room's capacity. Uses: BODYPART_COST (through `Body::cost`).
#[test]
fn an_exact_body_waits_for_its_cost_or_is_unfit() {
    let body = Body::new(&[Part::Work, Part::Work, Part::Move]).expect("body");
    let cost = body.cost();
    let exact = |budget| SpawnRequest {
        body: BodySpec::Exact(body),
        ..worker("W1N1", budget, 9)
    };
    for budget in [
        EnergyBudget::Capacity,
        EnergyBudget::Available,
        EnergyBudget::UpTo(cost - 1),
    ] {
        // It waits for its cost, and holds the cheaper request below it back.
        let snap = snap_at(
            T0,
            &[("W1N1", energy(cost - 1, cost))],
            &[spawn(1, "W1N1", 20)],
        );
        let mut sp = Spawner::default();
        plan(
            &mut sp,
            &snap,
            &[exact(budget), worker("W1N1", EnergyBudget::Available, 1)],
        );
        assert!(sp.orders().is_empty(), "{budget:?}: {:?}", sp.orders());
        assert_eq!(sp.stats().waiting, 1, "{budget:?}");
        // At its cost it spawns as it is.
        let snap = snap_at(T0, &[("W1N1", energy(cost, cost))], &[spawn(1, "W1N1", 20)]);
        let mut sp = Spawner::default();
        plan(&mut sp, &snap, &[exact(budget)]);
        let [order] = sp.orders() else {
            panic!("{budget:?}: one order: {:?}", sp.orders())
        };
        assert_eq!(order.body, body, "{budget:?}");
        // Above the room's capacity it is unfit and the next request spawns.
        let unit = unit_cost(&WORKER_PATTERN);
        let snap = snap_at(
            T0,
            &[("W1N1", energy(unit, cost - 1))],
            &[spawn(1, "W1N1", 20)],
        );
        let mut sp = Spawner::default();
        plan(
            &mut sp,
            &snap,
            &[exact(budget), worker("W1N1", EnergyBudget::Available, 1)],
        );
        let [order] = sp.orders() else {
            panic!("{budget:?}: one order: {:?}", sp.orders())
        };
        assert_eq!(order.request, 1, "{budget:?}");
        assert_eq!(sp.stats().unfit, 1, "{budget:?}");
    }
}

#[test]
fn a_refused_call_holds_the_spawn_for_hold_ticks() {
    let unit = unit_cost(&WORKER_PATTERN);
    let cfg = SPAWN_MMO;
    let spawns = [spawn(1, "W1N1", 20), spawn(2, "W1N1", 30)];
    let req = capped(worker("W1N1", EnergyBudget::Available, 1), 1);
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
        patterned(worker("W1N1", EnergyBudget::Available, 9), &CLAIMER),
        capped(worker("W1N1", EnergyBudget::Available, 8), 0),
        capped(worker("W1N1", EnergyBudget::Capacity, 7), 0),
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

/// Requests whose (owned) home has no own spawn and no spawn room within help range are counted,
/// and no unit is made for them.
#[test]
fn requests_for_a_room_without_a_spawn_are_counted() {
    let unit = unit_cost(&WORKER_PATTERN);
    assert!(room_distance(room("W5N5"), room("W1N1")) > SPAWN_MMO.help_range);
    let snap = snap_at(
        T0,
        &[("W1N1", energy(unit, unit)), ("W5N5", CLAIMED)],
        &[spawn(1, "W1N1", 20)],
    );
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
    sp.begin(&snap, &SPAWN_MMO);
    let req = capped(worker("W1N1", EnergyBudget::Available, 1), 1);
    for _ in 0..count {
        sp.request(req).expect("open");
    }
    assert_eq!(sp.close(&snap, true), 1);
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
    sp.begin(&stale, &SPAWN_MMO);
    sp.request(worker("W1N1", EnergyBudget::Available, 1))
        .expect("open");
    assert!(
        sp.plan_room(&stale, 0, &mut commit).is_err(),
        "planned before close"
    );
    assert_eq!(sp.close(&stale, true), 1);
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
            ("W3N1", CLAIMED),
        ],
        &[
            spawn(5, "W2N1", 20),
            spawn(4, "W1N1", 20),
            spawn(6, "W1N1", 22),
            spawn(7, "W2N1", 22),
        ],
    );
    // W3N1 (owned) has no spawn: W2N1, 1 away and full, serves it after its own request, on its
    // second spawn with what its own order leaves.
    let requests = [
        worker("W1N1", EnergyBudget::Available, 2),
        capped(worker("W2N1", EnergyBudget::Capacity, 2), 4),
        capped(worker("W1N1", EnergyBudget::Available, 2), 2),
        worker("W3N1", EnergyBudget::Available, 9),
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
    assert_eq!(
        rooms,
        [room("W2N1"), room("W3N1"), room("W1N1")],
        "rooms by packed name, each room's own first"
    );
    let helped = a.orders()[1];
    assert_eq!(
        (helped.request, helped.spawn, helped.body.cost()),
        (3, handle(&snap, 7), 5 * unit),
        "W2N1's second spawn, with the 9 - 4 repeats its own order left"
    );
    assert_eq!(a.orders()[2].request, 0, "the earlier of equal priorities");
    assert_eq!(
        a.orders()[2].body.cost(),
        5 * unit,
        "W1N1 spends what it holds"
    );
    assert_eq!(
        a.stats().waiting,
        1,
        "W1N1's second spawn finds nothing left"
    );
    assert_eq!(a.stats().helped, 1);
}

/// At the safe range's minimum, a hold still keeps the spawn out for one tick.
#[test]
fn the_shortest_hold_lasts_one_tick() {
    let unit = unit_cost(&WORKER_PATTERN);
    let cfg = SpawnConfig {
        hold_ticks: 1,
        ..SPAWN_MMO
    };
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

/// `room_distance` over V169's coordinates: each of the eight neighbours is 1 away, the rooms either
/// side of the W/E and N/S seams are neighbours, the larger difference counts, and it is symmetric.
#[test]
fn room_distance_is_the_larger_coordinate_difference() {
    let c = room("E5N5");
    for dx in -1..=1 {
        for dy in -1..=1 {
            let n = c.checked_add((dx, dy)).expect("a neighbour");
            assert_eq!(room_distance(c, n), u32::from(dx != 0 || dy != 0));
        }
    }
    assert_eq!(room_distance(room("W0N0"), room("E0N0")), 1);
    assert_eq!(room_distance(room("W0N0"), room("W0S0")), 1);
    assert_eq!(room_distance(room("W0N0"), room("E0S0")), 1);
    let (a, b) = (room("W2N1"), room("E3N4"));
    let want = a
        .x_coord()
        .abs_diff(b.x_coord())
        .max(a.y_coord().abs_diff(b.y_coord()));
    assert_eq!(
        want, 6,
        "W2 is x -3 and E3 is x 3; N1 is y -2 and N4 is y -5 (V169)"
    );
    assert_eq!((room_distance(a, b), room_distance(b, a)), (want, want));
}

/// An owned room with no spawn, as a claimed room before its spawn stands: no energy of its own.
const CLAIMED: RoomEnergy = RoomEnergy {
    available: 0,
    capacity: 0,
};

/// A request whose (owned) home has no own spawn goes to the nearest spawn room within help range
/// whose capacity fits its body (equal distances to the lower packed name), is sized to that
/// room's capacity or to what it holds, and its creep keeps its home.
#[test]
fn a_room_without_a_spawn_is_served_by_the_nearest_spawn_room() {
    let unit = unit_cost(&WORKER_PATTERN);
    let (near, far) = (4 * unit, 9 * unit);
    // W3N1 is 1 from W2N1 and 2 from W1N1 (room x: W3 -4, W2 -3, W1 -2; V169).
    assert_eq!(room_distance(room("W3N1"), room("W2N1")), 1);
    assert_eq!(room_distance(room("W3N1"), room("W1N1")), 2);
    let rooms = [
        ("W1N1", energy(far, far)),
        ("W2N1", energy(near, near)),
        ("W3N1", CLAIMED),
    ];
    let spawns = [spawn(1, "W1N1", 20), spawn(2, "W2N1", 20)];
    let snap = snap_at(T0, &rooms, &spawns);
    let mut sp = Spawner::default();
    plan(&mut sp, &snap, &[worker("W3N1", EnergyBudget::Capacity, 1)]);
    assert_eq!(sp.server_of(room("W3N1")), Some(room("W2N1")));
    assert_eq!(
        sp.server_of(room("W1N1")),
        Some(room("W1N1")),
        "a spawn room serves itself"
    );
    let [order] = sp.orders() else {
        panic!("one order: {:?}", sp.orders())
    };
    assert_eq!(order.spawn, handle(&snap, 2), "the nearer room's spawn");
    assert_eq!(
        order.body.cost(),
        near,
        "sized to the serving room's capacity"
    );
    assert_eq!(
        order.identity.home,
        room("W3N1"),
        "the creep keeps its home"
    );
    assert_eq!(
        CreepName::parse(order.name.as_bytes()),
        Some(order.identity)
    );
    assert_eq!((sp.stats().helped, sp.stats().no_spawn), (1, 0));
    assert_eq!(sp.rooms(), [room("W2N1")]);

    // An exact body above the nearer room's capacity goes on to the farther one, which fits it;
    // an `Available` request spends what its room's own order leaves. A body no room in range fits
    // has no serving room.
    let spawns = [
        spawn(1, "W1N1", 20),
        spawn(2, "W2N1", 20),
        spawn(3, "W2N1", 22),
    ];
    let snap = snap_at(T0 + 1, &rooms, &spawns);
    let big = Body::scaled(&WORKER_PATTERN, near + unit, u8::MAX).expect("a body");
    assert!(big.cost() > near && big.cost() <= far);
    let huge = Body::scaled(&WORKER_PATTERN, far + unit, u8::MAX).expect("a body");
    assert!(huge.cost() > far);
    let exact = |body: Body| SpawnRequest {
        body: BodySpec::Exact(body),
        ..worker("W3N1", EnergyBudget::Capacity, 5)
    };
    let requests = [
        exact(big),
        capped(worker("W2N1", EnergyBudget::Available, 1), 1),
        worker("W3N1", EnergyBudget::Available, 1),
        exact(huge),
    ];
    plan(&mut sp, &snap, &requests);
    let got: Vec<(StructureHandle, u32, u32)> = sp
        .orders()
        .iter()
        .map(|o| (o.spawn, o.request, o.body.cost()))
        .collect();
    assert_eq!(
        got,
        [
            (handle(&snap, 2), 1, unit),
            (handle(&snap, 3), 2, near - unit),
            (handle(&snap, 1), 0, big.cost())
        ],
        "rooms planned by packed name: W2N1 (x -3) before W1N1 (x -2)"
    );
    let servers: Vec<Option<RoomName>> = (0..4).map(|i| sp.server_at(i)).collect();
    assert_eq!(
        servers,
        [
            Some(room("W1N1")),
            Some(room("W2N1")),
            Some(room("W2N1")),
            None
        ]
    );
    assert_eq!((sp.stats().helped, sp.stats().no_spawn), (2, 1));
    assert_eq!(sp.stats().unfit, 0, "no room takes a body it cannot fit");

    // Equal distances: W2N2 is 1 from W1N1 and from W3N1; W3N1 has the lower packed name.
    let snap = snap_at(
        T0 + 1,
        &[
            ("W1N1", energy(far, far)),
            ("W3N1", energy(near, near)),
            ("W2N2", CLAIMED),
        ],
        &[spawn(1, "W1N1", 20), spawn(3, "W3N1", 20)],
    );
    assert_eq!(room_distance(room("W2N2"), room("W1N1")), 1);
    assert_eq!(room_distance(room("W2N2"), room("W3N1")), 1);
    assert!(room("W3N1").packed_repr() < room("W1N1").packed_repr());
    plan(&mut sp, &snap, &[worker("W2N2", EnergyBudget::Capacity, 1)]);
    assert_eq!(sp.server_of(room("W2N2")), Some(room("W3N1")));
    assert_eq!(sp.orders().first().map(|o| o.spawn), Some(handle(&snap, 3)));
}

/// Only an owned room is served by another's spawns: nothing drives a creep whose home the bot
/// does not own (`economy` takes members of owned rooms), so such a request has no serving room,
/// whether the room is visible (neutral, or another player's) or not.
#[test]
fn only_owned_rooms_are_served_by_others() {
    let unit = unit_cost(&WORKER_PATTERN);
    let mut snap = Snapshot::default();
    fill_rooms(
        &mut snap,
        T0,
        &[
            (
                "W1N1",
                energy(2 * unit, 2 * unit),
                Controller::Mine { level: 3 },
            ),
            ("W2N2", CLAIMED, Controller::Mine { level: 1 }),
            ("W2N1", CLAIMED, Controller::Neutral),
            ("W1N2", CLAIMED, Controller::Foreign { level: 2 }),
        ],
        &[spawn(1, "W1N1", 20), spawn(2, "W1N1", 22)],
    );
    for r in ["W2N2", "W2N1", "W1N2", "W2N0"] {
        assert!(room_distance(room(r), room("W1N1")) <= SPAWN_MMO.help_range);
    }
    let ask = |home: &str| capped(worker(home, EnergyBudget::Available, 1), 1);
    let mut sp = Spawner::default();
    plan(
        &mut sp,
        &snap,
        &[ask("W2N1"), ask("W1N2"), ask("W2N0"), ask("W2N2")],
    );
    for r in ["W2N1", "W1N2", "W2N0"] {
        assert_eq!(sp.server_of(room(r)), None, "{r} is not owned");
    }
    let homes: Vec<RoomName> = sp.orders().iter().map(|o| o.identity.home).collect();
    assert_eq!(homes, [room("W2N2")]);
    assert_eq!((sp.stats().helped, sp.stats().no_spawn), (1, 3));
}

/// A helped request goes past the nearest room it fits when that room cannot help this tick (not
/// full, or no ready spawn) to the nearest in range that can; with none, it stays with the nearest
/// it fits by capacity (whatever that room holds now), and nothing spawns it.
#[test]
fn a_helped_request_goes_past_a_room_that_cannot_help() {
    let unit = unit_cost(&WORKER_PATTERN);
    let cap = 4 * unit;
    // W3N1 (owned, no spawn) is 1 from W2N1 and 2 from W1N1.
    let req = capped(worker("W3N1", EnergyBudget::Available, 1), 1);
    let busy = |key: u128, room: &'static str| SpawnAt {
        busy: true,
        ..spawn(key, room, 20)
    };
    let mut sp = Spawner::default();
    let world = |near: RoomEnergy, near_spawn: SpawnAt, far: RoomEnergy, far_spawn: SpawnAt| {
        snap_at(
            T0,
            &[("W2N1", near), ("W1N1", far), ("W3N1", CLAIMED)],
            &[near_spawn, far_spawn],
        )
    };

    // The nearer room is not full: the farther, full one serves.
    let snap = world(
        energy(cap - 1, cap),
        spawn(2, "W2N1", 20),
        energy(cap, cap),
        spawn(1, "W1N1", 20),
    );
    plan(&mut sp, &snap, &[req]);
    assert_eq!(sp.server_at(0), Some(room("W1N1")));
    assert_eq!(
        sp.server_of(room("W3N1")),
        Some(room("W2N1")),
        "the nearest"
    );
    assert_eq!(sp.orders().first().map(|o| o.spawn), Some(handle(&snap, 1)));

    // The nearer room is full but its spawn is busy: the farther one serves.
    let snap = world(
        energy(cap, cap),
        busy(2, "W2N1"),
        energy(cap, cap),
        spawn(1, "W1N1", 20),
    );
    plan(&mut sp, &snap, &[req]);
    assert_eq!(sp.server_at(0), Some(room("W1N1")));
    assert_eq!(sp.orders().len(), 1);

    // Neither can help: it stays with the nearer room, fitted by capacity though it holds less
    // than the body costs, and no room is a unit.
    let snap = world(
        energy(0, cap),
        spawn(2, "W2N1", 20),
        energy(cap, cap),
        busy(1, "W1N1"),
    );
    plan(&mut sp, &snap, &[req]);
    assert_eq!(sp.server_at(0), Some(room("W2N1")));
    assert!(sp.orders().is_empty() && sp.rooms().is_empty());
    assert_eq!((sp.stats().helped, sp.stats().no_spawn), (1, 0));

    // The nearer room's only spawn is held after a refused call: not ready, so it cannot help, and
    // with the farther one busy the request stays with it; its held spawn is counted.
    let snap = world(
        energy(cap, cap),
        spawn(2, "W2N1", 20),
        energy(cap, cap),
        busy(1, "W1N1"),
    );
    plan(&mut sp, &snap, &[req]);
    let [order] = sp.orders() else {
        panic!("one order: {:?}", sp.orders())
    };
    assert_eq!(order.spawn, handle(&snap, 2));
    sp.record(0, Outcome::Threw, &SPAWN_MMO);
    let mut snap = snap;
    fill(
        &mut snap,
        T0 + 1,
        &[
            ("W2N1", energy(cap, cap)),
            ("W1N1", energy(cap, cap)),
            ("W3N1", CLAIMED),
        ],
        &[spawn(2, "W2N1", 20), busy(1, "W1N1")],
    );
    plan(&mut sp, &snap, &[req]);
    assert_eq!(sp.server_at(0), Some(room("W2N1")));
    assert!(sp.orders().is_empty() && sp.rooms().is_empty());
    assert_eq!(sp.stats().held, 1, "a serving room's held spawn");
}

/// `spawn_side` names the room whose spawns would spawn a home's creep, with that room's energy and
/// whether one of its spawns is idle: the home itself, the room that would serve an owned home
/// without a spawn, or none.
#[test]
fn spawn_side_names_the_spawning_room() {
    let unit = unit_cost(&WORKER_PATTERN);
    let busy = SpawnAt {
        busy: true,
        ..spawn(1, "W1N1", 20)
    };
    let mut snap = Snapshot::default();
    fill_rooms(
        &mut snap,
        T0,
        &[
            (
                "W1N1",
                energy(unit, 3 * unit),
                Controller::Mine { level: 3 },
            ),
            (
                "W2N1",
                energy(2 * unit, 2 * unit),
                Controller::Mine { level: 2 },
            ),
            (
                "W3N1",
                energy(5 * unit, 6 * unit),
                Controller::Mine { level: 6 },
            ),
            ("W5N1", CLAIMED, Controller::Neutral),
        ],
        &[busy, spawn(2, "W2N1", 20)],
    );
    let mut sp = Spawner::default();
    sp.begin(&snap, &SPAWN_MMO);
    let side = |home: &str| sp.spawn_side(room(home));
    assert_eq!(
        side("W1N1"),
        Some(SpawnSide {
            room: room("W1N1"),
            energy: energy(unit, 3 * unit),
            idle: false
        }),
        "its own, busy"
    );
    // W3N1 lost its spawn but keeps extensions: it is sized at W2N1, 1 away.
    assert_eq!(
        side("W3N1"),
        Some(SpawnSide {
            room: room("W2N1"),
            energy: energy(2 * unit, 2 * unit),
            idle: true
        })
    );
    assert_eq!(side("W5N1"), None, "not owned");
}
/// A room serves others only on a tick it starts full and help is allowed (the Standard tier
/// runs); its own requests spawn either way. A room whose only requests are others' is no unit on
/// a tick it does not help, so it reads no clock.
#[test]
fn a_room_helps_only_while_full_and_allowed() {
    let unit = unit_cost(&WORKER_PATTERN);
    let cap = 3 * unit;
    let spawns = [spawn(1, "W1N1", 20), spawn(2, "W1N1", 22)];
    let helped = capped(worker("W2N2", EnergyBudget::Available, 9), 1);
    let own = capped(worker("W1N1", EnergyBudget::Available, 1), 1);
    let at = |available: u32| {
        snap_at(
            T0,
            &[("W1N1", energy(available, cap)), ("W2N2", CLAIMED)],
            &spawns,
        )
    };
    let homes =
        |sp: &Spawner| -> Vec<RoomName> { sp.orders().iter().map(|o| o.identity.home).collect() };
    let mut sp = Spawner::default();

    // Not full: only its own request; with no own request, no unit.
    let snap = at(cap - 1);
    plan(&mut sp, &snap, &[helped, own]);
    assert_eq!(homes(&sp), [room("W1N1")]);
    plan(&mut sp, &snap, &[helped]);
    assert!(sp.rooms().is_empty() && sp.orders().is_empty());
    assert_eq!(
        sp.stats().helped,
        1,
        "given to W1N1, which does not plan it"
    );

    // Full but help not allowed: the same.
    let snap = at(cap);
    plan_with(&mut sp, &snap, &[helped, own], &SPAWN_MMO, false);
    assert_eq!(homes(&sp), [room("W1N1")]);
    plan_with(&mut sp, &snap, &[helped], &SPAWN_MMO, false);
    assert!(sp.rooms().is_empty() && sp.orders().is_empty());

    // Full and allowed: both.
    plan(&mut sp, &snap, &[helped, own]);
    assert_eq!(homes(&sp), [room("W1N1"), room("W2N2")]);
}

/// Help reaches exactly `spawn.help_range` (straight or diagonal): a home that far is served, one a
/// room farther is not, and at 0 no room serves another.
#[test]
fn help_reaches_exactly_help_range() {
    let unit = unit_cost(&WORKER_PATTERN);
    let range = SPAWN_MMO.help_range;
    let step = i32::try_from(range).expect("range");
    let server = room("E1N1");
    let at = server.checked_add((step, 0)).expect("in the world");
    let diagonal = server.checked_add((-step, step)).expect("in the world");
    let beyond = server.checked_add((0, step + 1)).expect("in the world");
    assert_eq!(room_distance(at, server), range);
    assert_eq!(room_distance(diagonal, server), range);
    assert_eq!(room_distance(beyond, server), range + 1);
    let names: Vec<String> = [at, diagonal, beyond]
        .iter()
        .map(|r| r.to_string())
        .collect();
    let mut rooms = vec![("E1N1", energy(3 * unit, 3 * unit))];
    rooms.extend(names.iter().map(|n| (n.as_str(), CLAIMED)));
    let spawns = [
        spawn(1, "E1N1", 20),
        spawn(2, "E1N1", 22),
        spawn(3, "E1N1", 24),
    ];
    let snap = snap_at(T0, &rooms, &spawns);
    let ask = |home: RoomName| SpawnRequest {
        home,
        ..capped(worker("E1N1", EnergyBudget::Available, 1), 1)
    };
    let requests = [ask(beyond), ask(at), ask(diagonal)];
    let mut sp = Spawner::default();
    plan(&mut sp, &snap, &requests);
    let homes: Vec<RoomName> = sp.orders().iter().map(|o| o.identity.home).collect();
    assert_eq!(
        homes,
        [at, diagonal],
        "in request order; the farther home gets none"
    );
    assert_eq!((sp.stats().helped, sp.stats().no_spawn), (2, 1));
    assert_eq!(sp.server_of(beyond), None);

    let off = SpawnConfig {
        help_range: 0,
        ..SPAWN_MMO
    };
    plan_with(&mut sp, &snap, &requests, &off, true);
    assert!(sp.orders().is_empty() && sp.rooms().is_empty());
    assert_eq!((sp.stats().helped, sp.stats().no_spawn), (0, 3));
    assert_eq!(sp.server_of(at), None);
    assert_eq!(
        sp.server_of(server),
        Some(server),
        "a spawn room still serves itself"
    );
}

/// A serving room takes its own requests before those it serves for others, whatever their
/// priority, on its first spawns by id; what its own orders leave is all the helped ones may spend.
#[test]
fn own_requests_come_before_helped_ones() {
    let unit = unit_cost(&WORKER_PATTERN);
    let cap = 2 * unit;
    // W1N1 serves W2N1, 1 away, owned, with no spawn of its own.
    assert!(room_distance(room("W2N1"), room("W1N1")) <= SPAWN_MMO.help_range);
    let spawns = [spawn(1, "W1N1", 20), spawn(2, "W1N1", 22)];
    let rooms = [("W1N1", energy(cap, cap)), ("W2N1", CLAIMED)];
    let snap = snap_at(T0, &rooms, &spawns);
    let helped = capped(worker("W2N1", EnergyBudget::Available, u8::MAX), 1);
    let own = capped(worker("W1N1", EnergyBudget::Available, 1), 1);
    let mut sp = Spawner::default();

    plan(&mut sp, &snap, &[helped, own]);
    let got: Vec<(RoomName, StructureHandle)> = sp
        .orders()
        .iter()
        .map(|o| (o.identity.home, o.spawn))
        .collect();
    assert_eq!(
        got,
        [
            (room("W1N1"), handle(&snap, 1)),
            (room("W2N1"), handle(&snap, 2))
        ],
        "own first, on the first spawn by id; then the helped one"
    );

    // An own request at the room's capacity takes all it holds: the helped one waits.
    plan(
        &mut sp,
        &snap,
        &[helped, worker("W1N1", EnergyBudget::Capacity, 0)],
    );
    let [order] = sp.orders() else {
        panic!("one order: {:?}", sp.orders())
    };
    assert_eq!(order.identity.home, room("W1N1"));
    assert_eq!(order.body.cost(), cap);
    assert_eq!(sp.stats().waiting, 1, "nothing left for the helped one");
}

/// `spawn.help_range`'s default (TUNING): a room is `ROOM_SIZE` tiles across and a creep walks at
/// most one tile a tick, so a helped creep spends about `ROOM_SIZE` ticks a room of distance on
/// the way at best (more with fatigue and detours); the default is the largest range whose best
/// walk is within a tenth of `CREEP_LIFE_TIME`.
#[test]
fn the_default_help_ranges_best_walk_is_a_tenth_of_a_life() {
    let walk = |range: u32| range * u32::from(ROOM_SIZE);
    assert!(walk(SPAWN_MMO.help_range) * 10 <= CREEP_LIFE_TIME);
    assert!(walk(SPAWN_MMO.help_range + 1) * 10 > CREEP_LIFE_TIME);
}
