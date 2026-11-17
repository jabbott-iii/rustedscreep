//! Worker sizing for the opening (ROADMAP P2.4): how long one worker's round takes between a source
//! and the controller, and how many workers keep a source busy; the static miner's body (P3.1);
//! the haulers' bodies and counts from a source's yield and trip (P3.3); and the upgraders' bodies
//! and counts from the energy that reaches the controller (P3.4). The P3 body solver replaces the
//! worker sizing; the formulas stay simple on purpose and are lower bounds where noted.

use rustedscreep_consts::{
    CARRY_CAPACITY, CONTAINER_CAPACITY, CREEP_LIFE_TIME, CREEP_SPAWN_TIME, ENERGY_REGEN_TIME,
    HARVEST_POWER, MAX_CREEP_SIZE, MOVE_POWER, Part, UPGRADE_CONTROLLER_POWER,
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
    miner_body_with(capacity, trip, energy, 0)
}

/// A static miner's body as [`miner_body`] gives it, with `carry` CARRY parts between the WORK and
/// the MOVE parts (P3.2: a miner beside a link holds its harvest and transfers it). An empty CARRY
/// part makes no fatigue (V54), so the walk out and the MOVE count stay those of [`miner_body`];
/// the CARRY parts add their cost and their share of `MAX_CREEP_SIZE`. `None` when the parts with
/// one MOVE cost more than `energy` or pass `MAX_CREEP_SIZE`, or for an empty capacity.
///
/// Uses: `BODYPART_COST` (`Part::cost`), `ENERGY_REGEN_TIME`, `MAX_CREEP_SIZE`, [`miner_work`],
/// [`walk_ticks`].
pub fn miner_body_with(capacity: u32, trip: Trip, energy: u32, carry: u32) -> Option<Body> {
    let work = miner_work(capacity);
    if work == 0 {
        return None;
    }
    let fixed = Part::Work
        .cost()
        .saturating_mul(work)
        .saturating_add(Part::Carry.cost().saturating_mul(carry));
    let move_cost = Part::Move.cost();
    let mut best: Option<(u64, u32)> = None;
    for moves in 1..=MAX_CREEP_SIZE.saturating_sub(work).saturating_sub(carry) {
        let cost = fixed.saturating_add(move_cost.saturating_mul(moves));
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
    let work = usize::try_from(work).ok()?;
    let carry = usize::try_from(carry).ok()?;
    let len = work
        .checked_add(carry)?
        .checked_add(usize::try_from(moves).ok()?)?;
    for (i, part) in parts.iter_mut().enumerate().take(work + carry) {
        *part = if i < work { Part::Work } else { Part::Carry };
    }
    Body::new(parts.get(..len)?)
}

/// A hauler's round (P3.3): one tick next to the mining tile to take a load, `trip` with every CARRY
/// part full, the ticks to hand the load over, and `trip` back empty. Loaded, each full CARRY part
/// weighs one; empty, only the parts that are neither MOVE nor CARRY do (V54). Handing over is one
/// `transfer` a tick (V191) into fills that each take at most `unload` (the level's extension
/// capacity, `EXTENSION_ENERGY_CAPACITY`), `ceil(load / unload)` of them (at least one), with a step
/// to the next fill between two: `2 × ceil(load / unload) − 1` ticks, the most a load can take (a
/// load the controller container takes goes in one tick; fills side by side need no step). The tick
/// at each end is the act step's: a hauler's task is chosen again each tick, so it walks on from
/// the tick after its take or its last transfer. `None` without a CARRY or a MOVE part. The walks
/// are lower bounds, as [`walk_ticks`] is; the unloading an upper one. Unit: ticks.
///
/// Uses: `CARRY_CAPACITY`, [`walk_ticks`].
pub fn haul_round(body: &Body, trip: Trip, unload: u32) -> Option<u32> {
    let carry = count(body, Part::Carry);
    let moves = count(body, Part::Move);
    if carry == 0 {
        return None;
    }
    let len = u32::try_from(body.len()).ok()?;
    let others = len.saturating_sub(moves).saturating_sub(carry);
    round_ticks(trip, others, carry, moves, unload)
}

/// [`haul_round`] for a body of `others` parts that make fatigue, `carry` CARRY and `moves` MOVE
/// parts.
fn round_ticks(trip: Trip, others: u32, carry: u32, moves: u32, unload: u32) -> Option<u32> {
    let loaded = walk_ticks(trip, others.saturating_add(carry), moves)?;
    let empty = walk_ticks(trip, others, moves)?;
    let fills = carry
        .saturating_mul(CARRY_CAPACITY)
        .div_ceil(unload.max(1))
        .max(1);
    // The take, then a transfer a fill and a step between two.
    Some(
        loaded
            .saturating_add(empty)
            .saturating_add(fills.saturating_mul(2)),
    )
}

/// Haulers of `carry` CARRY parts each, every one making a round of `round` ticks, that carry what a
/// source of `capacity` yields with `margin_pct` to spare: the least `n` with `n × carry ×
/// CARRY_CAPACITY × ENERGY_REGEN_TIME × 100 ≥ capacity × round × (100 + margin_pct)` (the source
/// yields `capacity` every `ENERGY_REGEN_TIME` ticks, V184, and each hauler moves one full load a
/// round). 0 for an empty capacity or no CARRY part. Unit: creeps.
///
/// Uses: `CARRY_CAPACITY`, `ENERGY_REGEN_TIME`.
pub fn haulers_for(capacity: u32, carry: u32, round: u32, margin_pct: u32) -> u32 {
    if capacity == 0 || carry == 0 {
        return 0;
    }
    // In u64: a source's capacity, a round and the margin stay far below its range.
    let need = u64::from(capacity)
        .saturating_mul(u64::from(round.max(1)))
        .saturating_mul(100u64.saturating_add(u64::from(margin_pct)));
    let per = u64::from(carry)
        .saturating_mul(u64::from(CARRY_CAPACITY))
        .saturating_mul(u64::from(ENERGY_REGEN_TIME))
        .saturating_mul(100);
    u32::try_from(need.div_ceil(per.max(1))).unwrap_or(u32::MAX)
}

/// The haulers for a source of `capacity` whose loads travel `trip` and are handed over at most
/// `unload` a transfer ([`haul_round`]), in a room whose spawns and extensions hold at most
/// `energy` (P3.3): a body of CARRY then MOVE parts and how many of it carry the source's yield with
/// `margin_pct` to spare ([`haulers_for`]). Among the bodies that cost
/// at most `energy` and hold at most `MAX_CREEP_SIZE` parts it takes the fewest haulers (each step a
/// hauler takes and each load it takes and hands over is a billed intent, and the steps per energy
/// carried fall as its CARRY parts grow: fewer, larger haulers carry the same energy for fewer
/// intents), then the least energy for all of them, then the fewest MOVE parts. MOVE parts past
/// the ones that bring the loaded walk down to `trip.tiles` save nothing. `None` for an empty
/// capacity, or when one CARRY and one MOVE cost more than `energy`. A hauler holds at most what a
/// container does (`CONTAINER_CAPACITY`), since it takes whole loads.
///
/// Uses: `BODYPART_COST` (`Part::cost`), `MAX_CREEP_SIZE`, `CONTAINER_CAPACITY`, `CARRY_CAPACITY`,
/// [`haul_round`], [`haulers_for`].
pub fn hauler_plan(
    capacity: u32,
    trip: Trip,
    energy: u32,
    margin_pct: u32,
    unload: u32,
) -> Option<(Body, u32)> {
    if capacity == 0 {
        return None;
    }
    let carry_cost = Part::Carry.cost();
    let move_cost = Part::Move.cost();
    // (haulers, energy for all of them, MOVE parts, CARRY parts)
    let mut best: Option<(u32, u64, u32, u32)> = None;
    let most = CONTAINER_CAPACITY
        .checked_div(CARRY_CAPACITY)
        .unwrap_or(0)
        .min(MAX_CREEP_SIZE.saturating_sub(1));
    for carry in 1..=most {
        let fixed = carry_cost.saturating_mul(carry);
        if fixed.saturating_add(move_cost) > energy {
            break;
        }
        for moves in 1..=MAX_CREEP_SIZE.saturating_sub(carry) {
            let cost = fixed.saturating_add(move_cost.saturating_mul(moves));
            if cost > energy {
                break;
            }
            let Some(round) = round_ticks(trip, 0, carry, moves, unload) else {
                break;
            };
            let n = haulers_for(capacity, carry, round, margin_pct);
            let key = (n, u64::from(n) * u64::from(cost), moves, carry);
            if best.is_none_or(|b| (key.0, key.1, key.2) < (b.0, b.1, b.2)) {
                best = Some(key);
            }
            if walk_ticks(trip, carry, moves).is_some_and(|w| w <= trip.tiles) {
                break;
            }
        }
    }
    let (n, _, moves, carry) = best?;
    let mut parts = [Part::Move; MAX_CREEP_SIZE as usize];
    let carry_n = usize::try_from(carry).ok()?;
    let len = carry_n.checked_add(usize::try_from(moves).ok()?)?;
    for part in parts.iter_mut().take(carry_n) {
        *part = Part::Carry;
    }
    Some((Body::new(parts.get(..len)?)?, n))
}

/// What a source of `capacity` yields over one creep life: `capacity` every `ENERGY_REGEN_TIME`
/// ticks (V184), for `CREEP_LIFE_TIME` ticks (V170). The upgraders' budget is counted per creep
/// life, so every creep's body cost (paid once a life) adds up with it in whole energy. An upper
/// bound on what a mined source gives: each miner life loses the replacement's spawning and walk
/// out (no pre-spawn before P5). Unit: energy.
///
/// Uses: `CREEP_LIFE_TIME`, `ENERGY_REGEN_TIME`.
pub fn yield_per_life(capacity: u32) -> u64 {
    u64::from(capacity) * u64::from(CREEP_LIFE_TIME) / u64::from(ENERGY_REGEN_TIME.max(1))
}

/// CARRY parts an upgrader of `work` WORK parts takes (P3.4): the fewest whose load is more than one
/// tick's upgrade, `floor(work × UPGRADE_CONTROLLER_POWER / CARRY_CAPACITY) + 1`. Its `withdraw`
/// runs before its `upgradeController` in the same tick (V45, V46), so it refills on the tick its
/// store would run dry and upgrades every tick; a CARRY part more saves a withdraw now and then (a
/// billed intent) for `BODYPART_COST[CARRY]` a life, energy the economy keeps. Unit: parts.
///
/// Uses: `UPGRADE_CONTROLLER_POWER`, `CARRY_CAPACITY`.
pub fn upgrader_carry(work: u32) -> u32 {
    work.saturating_mul(UPGRADE_CONTROLLER_POWER)
        .checked_div(CARRY_CAPACITY)
        .unwrap_or(0)
        .saturating_add(1)
}

/// What one upgrader of `work` WORK, `carry` CARRY and `moves` MOVE parts upgrades over a life
/// when its post is `trip` from the spawn (P3.4): `work × UPGRADE_CONTROLLER_POWER` a tick for the
/// life left after its spawning (`CREEP_SPAWN_TIME` a part, V170: its replacement is ordered once
/// it is gone, so the room waits that long) and its walk out (empty, so only the WORK parts weigh,
/// V54). A lower bound: spawning lengthens its cycle rather than shortening its life. `None`
/// without a MOVE part. Unit: energy.
///
/// Uses: `CREEP_LIFE_TIME`, `CREEP_SPAWN_TIME`, `UPGRADE_CONTROLLER_POWER`, [`walk_ticks`].
pub fn upgrader_upgrade(work: u32, carry: u32, moves: u32, trip: Trip) -> Option<u64> {
    let walk = walk_ticks(trip, work, moves)?;
    let parts = work.saturating_add(carry).saturating_add(moves);
    let spawn = parts.saturating_mul(CREEP_SPAWN_TIME);
    let active = CREEP_LIFE_TIME.saturating_sub(walk).saturating_sub(spawn);
    Some(u64::from(work) * u64::from(UPGRADE_CONTROLLER_POWER) * u64::from(active))
}

/// What `n` upgraders that each cost `cost` and upgrade `upgrade` a life (P3.4) upgrade of `budget`
/// a life: their bodies come out of it first, and they upgrade the rest up to what they can. The
/// energy neither upgraded nor paid for piles up in the stores (lost once they are full). Unit:
/// energy.
pub fn upgraded(budget: u64, n: u32, cost: u32, upgrade: u64) -> u64 {
    let paid = u64::from(n).saturating_mul(u64::from(cost));
    budget
        .saturating_sub(paid)
        .min(u64::from(n).saturating_mul(upgrade))
}

/// The upgraders for a room whose controller gets `budget` energy a creep life (P3.4: what reaches
/// its controller container and controller link, less every other creep's body), whose posts are
/// `trip` from the spawn, whose spawns and extensions hold at most `energy`, with at most
/// `max_creeps` posts and, at level 8, at most `max_work` WORK parts in all
/// (`CONTROLLER_MAX_UPGRADE_PER_TICK`, V188). Each body is WORK parts, [`upgrader_carry`] CARRY
/// parts and MOVE parts. For each body that costs at most `energy` and holds at most
/// `MAX_CREEP_SIZE` parts, the count is the one of `floor` and `ceil(budget / (cost + upgrade))`
/// (within the caps) that upgrades more ([`upgraded`]). It takes the body and count that upgrade
/// the most of the budget, then the fewest upgraders (each is a billed upgrade a tick, and holds
/// a post), then the least energy, then the fewest MOVE parts. A body with too few MOVE parts walks
/// out longer and upgrades less ([`upgrader_upgrade`]); MOVE parts past the ones that bring the
/// walk down to `trip.tiles` only cost. `None` for an empty budget, no post, when one WORK, its
/// CARRY and one MOVE cost more than `energy`, or when the best plan upgrades nothing (the budget
/// does not pay for one body).
///
/// Uses: `BODYPART_COST` (`Part::cost`), `MAX_CREEP_SIZE`, [`upgrader_carry`],
/// [`upgrader_upgrade`], [`upgraded`], [`walk_ticks`].
pub fn upgrader_plan(
    budget: u64,
    trip: Trip,
    energy: u32,
    max_creeps: u32,
    max_work: Option<u32>,
) -> Option<(Body, u32)> {
    if budget == 0 || max_creeps == 0 {
        return None;
    }
    let work_cost = Part::Work.cost();
    let carry_cost = Part::Carry.cost();
    let move_cost = Part::Move.cost();
    // (not upgraded, upgraders, energy for all of them, MOVE parts, WORK parts, CARRY parts)
    let mut best: Option<(u64, u32, u64, u32, u32, u32)> = None;
    for work in 1..MAX_CREEP_SIZE {
        if max_work.is_some_and(|m| work > m) {
            break;
        }
        let carry = upgrader_carry(work);
        let fixed = work_cost
            .saturating_mul(work)
            .saturating_add(carry_cost.saturating_mul(carry));
        if fixed.saturating_add(move_cost) > energy
            || work.saturating_add(carry).saturating_add(1) > MAX_CREEP_SIZE
        {
            break;
        }
        let most = max_work
            .map_or(max_creeps, |m| {
                m.checked_div(work).unwrap_or(0).min(max_creeps)
            })
            .max(1);
        for moves in 1..=MAX_CREEP_SIZE.saturating_sub(work).saturating_sub(carry) {
            let cost = fixed.saturating_add(move_cost.saturating_mul(moves));
            if cost > energy {
                break;
            }
            let Some(upgrade) = upgrader_upgrade(work, carry, moves, trip) else {
                break;
            };
            let per = u64::from(cost).saturating_add(upgrade).max(1);
            let floor = u32::try_from(budget / per).unwrap_or(u32::MAX);
            let ceil = u32::try_from(budget.div_ceil(per)).unwrap_or(u32::MAX);
            for n in [floor, ceil] {
                let n = n.clamp(1, most);
                let key = (
                    budget.saturating_sub(upgraded(budget, n, cost, upgrade)),
                    n,
                    u64::from(n) * u64::from(cost),
                    moves,
                    work,
                    carry,
                );
                if best.is_none_or(|b| (key.0, key.1, key.2, key.3) < (b.0, b.1, b.2, b.3)) {
                    best = Some(key);
                }
            }
            if walk_ticks(trip, work, moves).is_some_and(|w| w <= trip.tiles) {
                break;
            }
        }
    }
    if best.is_some_and(|b| b.0 >= budget) {
        return None;
    }
    let (_, n, _, moves, work, carry) = best?;
    let mut parts = [Part::Move; MAX_CREEP_SIZE as usize];
    let work = usize::try_from(work).ok()?;
    let carry = usize::try_from(carry).ok()?;
    let len = work
        .checked_add(carry)?
        .checked_add(usize::try_from(moves).ok()?)?;
    for (i, part) in parts.iter_mut().enumerate().take(work + carry) {
        *part = if i < work { Part::Work } else { Part::Carry };
    }
    Some((Body::new(parts.get(..len)?)?, n))
}
