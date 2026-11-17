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
//! merge on a tile (V193). Moves follow V51, V52 and the fatigue rules (V54, V55) on plain terrain.
//! The game side accepts the bot's plan and the processor then runs each creep's intents in its own
//! order (`creepActions`, V45), creep by creep, so a transfer lands before a harvest.
//! Containers (P3.1, ADR-023): energy made on a tile (a harvest's overflow, a decayed tombstone)
//! goes into a container there first, up to `CONTAINER_CAPACITY`, and the rest onto a pile (V193);
//! `withdraw` takes from one (V192); a finished container site becomes one; a container refuses
//! another site on its tile (V205). Their decay is left out (a new one lasts about 24550 ticks
//! unrepaired in an owned room, V214, beyond every scenario here).
//! Links (P3.2, ADR-024): a finished link site becomes a link holding up to `LINK_CAPACITY`; a
//! creep's transfer into one moves `min(stock, free space)` and `withdraw` takes `min(free space,
//! energy)`, refused when it is empty (V220); a send is checked against the tick's start (cooldown,
//! energy, the receiver's room; V216), then clamped to the receiver's room, which gets the amount
//! less [`link_loss`], and the sender's cooldown becomes `LINK_COOLDOWN` per tile of range, counted
//! down at the end of every tick (V217). Sends run after the creep intents, so a withdraw sees the
//! link's energy at the tick's start, as the game side computes it.

use super::placement::{
    CONTROLLER_CLEAR, EDGE_MARGIN, Placement, SPAWN_CLEAR, extensions_in, next_extension,
};
use super::sizing::{
    Round, Trip, carry_round, miner_body, miner_work, walk_ticks, worker_body, worker_round,
    workers_for_source,
};
use super::*;
use crate::config::{
    CACHE_MMO, LINK_MMO, LinkConfig, MINING_MMO, MMO, MOVEMENT_MMO, MiningConfig, OPENING_MMO,
    OpeningConfig, SPAWN_MMO,
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
use crate::spawning::{Body, Identity, Role, Spawner, WORKER_PATTERN};
use rustedscreep_consts::{
    BUILD_POWER, CARRY_CAPACITY, CONTAINER_CAPACITY, CONTROLLER_DOWNGRADE_RESTORE, CREEP_LIFE_TIME,
    ENERGY_DECAY, ENERGY_REGEN_TIME, ErrorCode, HARVEST_POWER, LINK_CAPACITY, LINK_COOLDOWN,
    MAX_CREEP_SIZE, MOVE_COST_PLAIN, MOVE_COST_ROAD, MOVE_COST_SWAMP, MOVE_POWER, Part, Position,
    RoomCoordinate, RoomName, SOURCE_ENERGY_CAPACITY, SOURCE_ENERGY_KEEPER_CAPACITY,
    SOURCE_ENERGY_NEUTRAL_CAPACITY, SPAWN_ENERGY_CAPACITY, SPAWN_ENERGY_START, StructureType,
    TOMBSTONE_DECAY_PER_PART, Terrain, UPGRADE_CONTROLLER_POWER, controller_downgrade,
    controller_levels, extension_energy_capacity,
};

// ---------------------------------------------------------------------------------------------
// Sizing

fn repeats(n: u32) -> Body {
    let cost = Body::new(&WORKER_PATTERN).expect("pattern").cost();
    worker_body(cost * n).expect("body")
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

/// Uses: SPAWN_ENERGY_CAPACITY, extension counts and capacities per level. The opening's bodies at
/// RCL1, 2 and 3 are the whole repeats of the pattern the room's capacity holds.
#[test]
fn worker_bodies_by_level() {
    let unit = Body::new(&WORKER_PATTERN).expect("pattern").cost();
    for rcl in 1..=3u32 {
        let capacity = SPAWN_ENERGY_CAPACITY
            + StructureType::Extension.controller_structures(rcl) * extension_energy_capacity(rcl);
        let body = worker_body(capacity).expect("body");
        let n = capacity / unit;
        assert_eq!(body.len(), WORKER_PATTERN.len() * n as usize, "RCL{rcl}");
        assert!(body.cost() <= capacity);
    }
    assert_eq!(worker_body(unit - 1), None);
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
    /// A container (ADR-023): it holds up to `CONTAINER_CAPACITY` and never decays here.
    Container {
        key: u128,
        at: Tile,
        energy: u32,
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
    placement: Placement,
    cfg: OpeningConfig,
    mining_cfg: MiningConfig,
    link_cfg: LinkConfig,
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
    levels: Vec<(u32, u8)>,
    /// Each source refill: the tick, the source's index, and the energy it still held (none taken
    /// that cycle is wasted).
    regens: Vec<(u32, usize, u32)>,
    full_at: Option<u32>,
    struct_rows: Vec<StructRow>,
    obj_rows: Vec<ObjRow>,
    at_level_up: LevelUp,
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
            placement: Placement::default(),
            cfg: OPENING_MMO,
            mining_cfg: MINING_MMO,
            link_cfg: LINK_MMO,
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
            billed: 0,
            refused_sites: 0,
            standard: true,
            drop_sites: false,
            extra_walls: Vec::new(),
            events: Vec::new(),
            failed: 0,
            spawned: Vec::new(),
            levels: Vec::new(),
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
        let push = |snap: &mut Snapshot, key: u128, kind, at: Position, amount, total| {
            snap.objects
                .push(
                    ObjectKey(key),
                    || Ok(ObjectStatic { kind, pos: at }),
                    ObjectRow { amount, total },
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
            );
            self.obj_rows.push(ObjRow::Site(i));
        }
        for (i, l) in self.loose.iter().enumerate() {
            let (key, kind, at, amount) = match *l {
                Loose::Pile { key, at, amount } => (key, ObjectKind::Energy, at, amount),
                Loose::Tomb {
                    key, at, energy, ..
                } => (key, ObjectKind::Tombstone, at, energy),
                Loose::Container { key, at, energy } => (key, ObjectKind::Container, at, energy),
            };
            push(&mut self.snap, key, kind, place(room, at), amount, 0);
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
        let mut commit = Commit::default();
        let cfg = self.cfg;
        let mining_cfg = self.mining_cfg;
        let link_cfg = self.link_cfg;
        // The Critical `mining` job, then `opening` (rustedscreep-wasm's order).
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
                    &mut commit,
                )
                .expect("mining");
        }
        for unit in 0..self.opening.room_count() {
            let room = self.opening.room_name(unit).expect("room");
            let mined = self.mining.mined(room).to_vec();
            let receivers = self.mining.receivers(room).to_vec();
            self.opening
                .plan_room(
                    unit,
                    &self.snap,
                    &mut self.movement,
                    &mut self.spawner,
                    &mut self.buffer,
                    &cfg,
                    &mcfg,
                    &mined,
                    &receivers,
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
        // The Standard `links` job (P3.2).
        for unit in 0..self.mining.send_count() * u32::from(self.standard) {
            self.mining
                .send_room(unit, &link_cfg, &mut commit)
                .expect("links");
        }
        // The Optional placement job (ADR-020, ADR-023, ADR-024).
        self.placement.begin(&self.snap);
        if cfg.place_extensions || mining_cfg.place_containers || link_cfg.place_links {
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
            (CreepIntent::Withdraw { .. }, Some(TargetRow::Structure(row))) => {
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
                let amount = s.energy.min(free);
                s.energy -= amount;
                self.creeps[actor].energy += amount;
                self.link_takes += 1;
                Outcome::Ok
            }
            (CreepIntent::UpgradeController { .. }, Some(TargetRow::Structure(_))) => {
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
                // Emptied by an earlier take this tick: the processor clamps to what is left.
                let amount = (*have).min(free);
                *have -= amount;
                self.creeps[actor].energy += amount;
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

    /// Moves (V51, V52) and their fatigue (V54 on plain, V55).
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
        for (c, d) in self.creeps.iter_mut().zip(&dest) {
            if let Some(d) = *d {
                c.at = d;
                let loaded = c
                    .energy
                    .div_ceil(CARRY_CAPACITY)
                    .min(count(&c.body, Part::Carry));
                let weight = count(&c.body, Part::Work) + loaded;
                c.fatigue += MOVE_COST_PLAIN * weight;
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
                Loose::Container { .. } => {}
            }
        }
        self.loose.retain(|l| match l {
            Loose::Pile { amount, .. } => *amount > 0,
            Loose::Tomb { decay_at, .. } => *decay_at > 0,
            Loose::Container { .. } => true,
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
    let unit = Body::new(&WORKER_PATTERN).expect("pattern");
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
    // The first workers spawn with what the spawn regenerated: one repeat each.
    let unit = Body::new(&WORKER_PATTERN).expect("pattern");
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
/// it reaches the count its plan wants, workers that die of old age are replaced (by bodies of the
/// full capacity), their tombstones are emptied, and nothing stalls or fails.
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
    let capacity = w.room_energy().capacity;
    let big = worker_body(capacity).expect("body");
    assert!(
        w.spawned.iter().any(|(_, b)| *b == big),
        "a full-capacity worker spawned: {:?}",
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
/// its spawn's capacity alone, within 4 times the source-limited floor: 8022 ticks in the scenario
/// room (2026-11-15).
#[test]
fn a_fresh_room_reaches_rcl3_without_extensions() {
    let bound = 4 * rcl3_floor();
    let ticks = ticks_to_rcl3(LevelUp::Nothing, false, bound + 1).expect("RCL3 within the bound");
    assert!(ticks >= rcl3_floor());
}

/// The level's extension sites placed by hand at each level-up (what ADR-020 measured): the
/// workers build and fill them, and a fresh room reaches RCL3 within 3 times the floor: 5892 ticks
/// in the scenario room (2026-11-15), 6146 with P3.1's miners and their containers (2026-11-16).
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
/// (`static_mining_reaches_rcl4_sooner`; 2026-11-16).
#[test]
fn a_fresh_room_reaches_rcl3_with_no_manual_action() {
    let bound = 3 * rcl3_floor();
    let ticks = ticks_to_rcl3(LevelUp::Nothing, true, bound + 1).expect("RCL3 within the bound");
    assert!(ticks >= rcl3_floor());
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
/// against 18643 in the scenario room (2026-11-16, P3.1).
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
fn steady_progress(level: u8, links: LinkConfig) -> (u32, u64) {
    let mut w = World::new(level, SPAWN_ENERGY_CAPACITY);
    w.link_cfg = links;
    w.run(6000);
    let (p, b) = (w.progress, w.billed);
    w.run(12_000);
    assert_eq!(w.failed, 0);
    (w.progress - p, w.billed - b)
}

/// The network pays at RCL5 and RCL6 in the scenario room (ADR-024 "As built"): more upgrade
/// progress from tick 6000 to 18000 than with it off, for fewer billed intents. Measured
/// 2026-11-16: RCL5 162285 against 154665 progress (+4.9%), 4.83 against 5.13 billed a tick; RCL6
/// 164133 against 156211 (+5.1%), 4.32 against 4.50. Counted from tick 0, the links' build cost
/// (`CONSTRUCTION_COST` 5000 each) is paid back by tick 24000 at RCL5 (286491 against 286604), not
/// yet at RCL6 (279027 against 281219).
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
    w.run(4 * CREEP_LIFE_TIME);
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
