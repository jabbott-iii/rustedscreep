//! Worker sizing for the opening (ROADMAP P2.4): how long one worker's round takes between a source
//! and the controller, and how many workers keep a source busy; and the static miner's body
//! (P3.1). The P3 body solver and hauler math replace the worker sizing; the formulas stay simple
//! on purpose and are lower bounds where noted.

use rustedscreep_consts::{
    CARRY_CAPACITY, ENERGY_REGEN_TIME, HARVEST_POWER, MAX_CREEP_SIZE, MOVE_POWER, Part,
    UPGRADE_CONTROLLER_POWER,
};

use crate::spawning::{Body, WORKER_PATTERN};

/// One way between a source and the controller, measured on movement's cost grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trip {
    /// Tiles stepped, at least (a creep moves at most one tile a tick). Unit: tiles.
    pub tiles: u32,
    /// The sum of the step costs on the cheapest way, in movement's cost units. With the default
    /// cost profile these are the engine's fatigue rates (`MOVE_COST_*`): the fatigue each part that
    /// makes fatigue adds per tile entered (V54, V154).
    pub cost: u32,
}

/// Ticks to walk `trip` with `weight` parts that make fatigue and `moves` MOVE parts, or `None`
/// with no MOVE part. A creep moves only while its fatigue is 0; each tile adds `rate × weight` and
/// each tick takes `MOVE_POWER × moves` away (V54, V55), so the walk takes at least the tiles and at
/// least the fatigue over the removal rate. A lower bound: rounding per tile can add a tick.
///
/// Uses: `MOVE_POWER`.
pub fn walk_ticks(trip: Trip, weight: u32, moves: u32) -> Option<u32> {
    if moves == 0 {
        return None;
    }
    let fatigue = trip.cost.saturating_mul(weight);
    let per_tick = MOVE_POWER.saturating_mul(moves);
    Some(trip.tiles.max(fatigue.div_ceil(per_tick)))
}

/// One worker's round: harvest a full load at a source, carry it to the controller, upgrade it all
/// and walk back. Upgrading is the opening's slowest way to spend a load, so the round is the
/// longest one a worker makes (filling a spawn is one tick, building takes `BUILD_POWER` times less
/// time than upgrading). Unit: ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Round {
    /// Ticks at the source: harvesting a load, or one take at a mined source ([`carry_round`]).
    pub harvest: u32,
    pub walk: u32,
    pub upgrade: u32,
}

impl Round {
    pub const fn ticks(self) -> u32 {
        self.harvest
            .saturating_add(self.walk)
            .saturating_add(self.upgrade)
    }
}

/// One worker's round at a mined source (P3.1): as [`worker_round`], but its load comes off the
/// miner's tile in one `withdraw` or `pickup` (V190, V192) instead of being harvested, so it spends
/// one tick at the source. `None` as for [`worker_round`].
pub fn carry_round(body: &Body, trip: Trip) -> Option<Round> {
    worker_round(body, trip).map(|round| Round {
        harvest: 1,
        ..round
    })
}

/// The round of a worker of `body` on `trip`, or `None` for a body that cannot harvest, carry or
/// move. Loaded, every CARRY part holds energy and weighs; empty, only the parts that are neither
/// MOVE nor CARRY do (V54).
///
/// Uses: `CARRY_CAPACITY`, `HARVEST_POWER`, `UPGRADE_CONTROLLER_POWER` (per WORK part, unboosted),
/// [`walk_ticks`].
pub fn worker_round(body: &Body, trip: Trip) -> Option<Round> {
    let work = count(body, Part::Work);
    let carry = count(body, Part::Carry);
    let moves = count(body, Part::Move);
    if work == 0 || carry == 0 {
        return None;
    }
    let len = u32::try_from(body.len()).ok()?;
    let others = len.saturating_sub(moves).saturating_sub(carry);
    let load = carry.saturating_mul(CARRY_CAPACITY);
    let loaded = walk_ticks(trip, others.saturating_add(carry), moves)?;
    let empty = walk_ticks(trip, others, moves)?;
    Some(Round {
        harvest: load.div_ceil(HARVEST_POWER.saturating_mul(work)),
        walk: loaded.saturating_add(empty),
        upgrade: load.div_ceil(UPGRADE_CONTROLLER_POWER.saturating_mul(work)),
    })
}

/// Workers of `body` that one source of `capacity` keeps busy, each making `round`: enough to carry
/// away what the source yields while a worker makes its round (the source refills to `capacity`
/// every `ENERGY_REGEN_TIME` ticks), but no more than `open_tiles` can harvest at once (each worker
/// spends `round.harvest` of its round next to the source). At least one while the source has an
/// open tile; 0 without one. Unit: workers.
///
/// Uses: `ENERGY_REGEN_TIME`, `CARRY_CAPACITY`.
pub fn workers_for_source(capacity: u32, open_tiles: u32, body: &Body, round: Round) -> u32 {
    if open_tiles == 0 || capacity == 0 {
        return 0;
    }
    let load = u64::from(count(body, Part::Carry)) * u64::from(CARRY_CAPACITY);
    let ticks = u64::from(round.ticks().max(1));
    let yielded = u64::from(capacity) * ticks;
    let drain = yielded.div_ceil((u64::from(ENERGY_REGEN_TIME) * load).max(1));
    let slots = u64::from(open_tiles) * ticks / u64::from(round.harvest.max(1));
    u32::try_from(drain.min(slots).max(1)).unwrap_or(u32::MAX)
}

/// The opening's worker body for a room whose spawns and extensions can hold `energy`: the most
/// whole repeats of [`WORKER_PATTERN`] that fit (`Body::scaled`), or `None` below one repeat.
pub fn worker_body(energy: u32) -> Option<Body> {
    Body::scaled(&WORKER_PATTERN, energy, u8::MAX)
}

fn count(body: &Body, part: Part) -> u32 {
    u32::try_from(body.count(part)).unwrap_or(u32::MAX)
}

/// WORK parts that harvest a source of `capacity` empty in each regeneration window: the source
/// refills to `capacity` `ENERGY_REGEN_TIME` ticks after it first drops below it, and harvests on
/// all of those ticks count (V184), each WORK part taking `HARVEST_POWER` a tick (V183). 0 for an
/// empty capacity. Unit: parts.
///
/// Uses: `ENERGY_REGEN_TIME`, `HARVEST_POWER`.
pub fn miner_work(capacity: u32) -> u32 {
    capacity.div_ceil(ENERGY_REGEN_TIME.saturating_mul(HARVEST_POWER).max(1))
}

/// A static miner's body for a source of `capacity` whose mining tile is `trip` from the spawn, in a
/// room that holds at most `energy` (P3.1): [`miner_work`] WORK parts, then the MOVE parts that
/// waste the least energy over its life. Each MOVE costs `BODYPART_COST[MOVE]`; each tick of the
/// walk out is a tick its source is not harvested, a loss of the source's yield `capacity /
/// ENERGY_REGEN_TIME` (the miner's WORK parts take at least that). The walk is [`walk_ticks`] with
/// the WORK parts as the weight (no CARRY). Ties go to fewer MOVE parts; MOVE parts past the one
/// that brings the walk down to `trip.tiles` save nothing. `None` when the WORK parts and one MOVE
/// cost more than `energy` or pass `MAX_CREEP_SIZE`, or for an empty capacity.
///
/// Uses: `BODYPART_COST` (`Part::cost`), `ENERGY_REGEN_TIME`, `MAX_CREEP_SIZE`, [`miner_work`],
/// [`walk_ticks`].
pub fn miner_body(capacity: u32, trip: Trip, energy: u32) -> Option<Body> {
    let work = miner_work(capacity);
    if work == 0 {
        return None;
    }
    let work_cost = Part::Work.cost().saturating_mul(work);
    let move_cost = Part::Move.cost();
    let mut best: Option<(u64, u32)> = None;
    for moves in 1..=MAX_CREEP_SIZE.saturating_sub(work) {
        let cost = work_cost.saturating_add(move_cost.saturating_mul(moves));
        if cost > energy {
            break;
        }
        let walk = walk_ticks(trip, work, moves)?;
        // In energy × ENERGY_REGEN_TIME, so the yield per tick needs no division.
        let loss = u64::from(move_cost) * u64::from(moves) * u64::from(ENERGY_REGEN_TIME)
            + u64::from(walk) * u64::from(capacity);
        if best.is_none_or(|(least, _)| loss < least) {
            best = Some((loss, moves));
        }
        if walk <= trip.tiles {
            break;
        }
    }
    let (_, moves) = best?;
    let mut parts = [Part::Move; MAX_CREEP_SIZE as usize];
    let len = usize::try_from(work.saturating_add(moves)).ok()?;
    for part in parts.iter_mut().take(usize::try_from(work).ok()?) {
        *part = Part::Work;
    }
    Body::new(parts.get(..len)?)
}
