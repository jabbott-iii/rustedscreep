//! The body solver (ROADMAP P3.6): every body the economy spawns is computed here from its targets,
//! none is a fixed pattern. How long one worker's round takes between a source and the controller,
//! and how many workers keep a source busy (P2.4); the generalist worker's body from its room's
//! sources, trips and build work ([`worker_plan`], P3.6); the static miner's body (P3.1); the
//! haulers' bodies and counts from a source's yield and trip (P3.3); and the upgraders' bodies and
//! counts from the energy that reaches the controller (P3.4). The formulas stay simple on purpose
//! and are lower bounds where noted.

use rustedscreep_consts::{
    BUILD_POWER, CARRY_CAPACITY, CONTAINER_CAPACITY, CONTAINER_DECAY, CONTAINER_DECAY_TIME_OWNED,
    CREEP_LIFE_TIME, CREEP_SPAWN_TIME, ENERGY_REGEN_TIME, HARVEST_POWER, MAX_CREEP_SIZE,
    MOVE_COST_PLAIN, MOVE_POWER, Part, RAMPART_DECAY_AMOUNT, RAMPART_DECAY_TIME, REPAIR_COST,
    REPAIR_POWER, ROAD_DECAY_AMOUNT, ROAD_DECAY_TIME, ROAD_HITS, UPGRADE_CONTROLLER_POWER,
};

use crate::spawning::Body;

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

/// Ticks to walk `trip` with `weight` parts that make fatigue and `moves` MOVE parts, at most, or
/// `None` with no MOVE part (P5.2: a replacement's lead must not fall short of the walk). A creep
/// moves only while its fatigue is 0 (V50); a move onto a tile of rate `r` adds `r × weight` (V54)
/// and each tick takes `R = MOVE_POWER × moves` away, down to 0 (V55), so each tile takes
/// `max(1, ceil(r × weight / R))` ticks and none carries over to the next. A rate is a whole number
/// of at least 1, so that is at most `r × max(1, ceil(weight / R))`, and the walk at most
/// `trip.cost × max(1, ceil(weight / R))`, with `trip.cost` the sum of the rates on the way. That
/// holds however many tiles the way has (`trip.tiles` is only a lower bound on them, a range). It
/// is exact on road, and on plain and swamp when the weight is a whole multiple of `R`; otherwise
/// it counts up to `r - 1` ticks a tile too many, the most with no weight, where a creep walks a
/// tile a tick and this gives the way's cost (twice its tiles on plain). Never below
/// [`walk_ticks`]. The rates are movement's default cost profile (`MOVE_COST_*`, as for
/// [`walk_ticks`]); a runtime profile below them makes this fall short. The way starts at the
/// trip's start, not wherever the spawn puts the creep.
///
/// Uses: `MOVE_POWER`, [`walk_ticks`].
pub fn walk_ticks_max(trip: Trip, weight: u32, moves: u32) -> Option<u32> {
    let low = walk_ticks(trip, weight, moves)?;
    let per_tile = weight
        .div_ceil(MOVE_POWER.saturating_mul(moves).max(1))
        .max(1);
    Some(trip.cost.saturating_mul(per_tile).max(low))
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
    round_of(Parts::of(body), trip, true)
}

/// The round of a worker of `body` on `trip`, or `None` for a body that cannot harvest, carry or
/// move. Loaded, every CARRY part holds energy and weighs; empty, only the parts that are neither
/// MOVE nor CARRY do (V54).
///
/// Uses: `CARRY_CAPACITY`, `HARVEST_POWER`, `UPGRADE_CONTROLLER_POWER` (per WORK part, unboosted),
/// [`walk_ticks`].
pub fn worker_round(body: &Body, trip: Trip) -> Option<Round> {
    round_of(Parts::of(body), trip, false)
}

/// A body's part counts, as the solver searches them: WORK, CARRY, MOVE, and every part that makes
/// fatigue empty (all but MOVE and CARRY, V54).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Parts {
    work: u32,
    carry: u32,
    moves: u32,
    others: u32,
}

impl Parts {
    fn of(body: &Body) -> Self {
        let (work, carry, moves) = (
            count(body, Part::Work),
            count(body, Part::Carry),
            count(body, Part::Move),
        );
        let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
        Self {
            work,
            carry,
            moves,
            others: len.saturating_sub(moves).saturating_sub(carry),
        }
    }

    /// A worker's parts: only WORK makes fatigue empty.
    const fn worker(work: u32, carry: u32, moves: u32) -> Self {
        Self {
            work,
            carry,
            moves,
            others: work,
        }
    }

    /// What its CARRY parts hold. Unit: energy.
    const fn load(self) -> u32 {
        self.carry.saturating_mul(CARRY_CAPACITY)
    }

    /// Its walk on `trip` loaded (every CARRY part full) and empty ([`walk_ticks`]).
    fn walks(self, trip: Trip) -> Option<(u32, u32)> {
        Some((
            walk_ticks(trip, self.others.saturating_add(self.carry), self.moves)?,
            walk_ticks(trip, self.others, self.moves)?,
        ))
    }
}

/// [`worker_round`] (or [`carry_round`] when `mined`) for the part counts `p`.
fn round_of(p: Parts, trip: Trip, mined: bool) -> Option<Round> {
    if p.work == 0 || p.carry == 0 {
        return None;
    }
    let load = p.load();
    let (loaded, empty) = p.walks(trip)?;
    Some(Round {
        harvest: if mined {
            1
        } else {
            load.div_ceil(HARVEST_POWER.saturating_mul(p.work))
        },
        walk: loaded.saturating_add(empty),
        upgrade: load.div_ceil(UPGRADE_CONTROLLER_POWER.saturating_mul(p.work)),
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
    workers_for_load(capacity, open_tiles, Parts::of(body).load(), round)
}

/// [`workers_for_source`] for workers that each carry `load` a round.
fn workers_for_load(capacity: u32, open_tiles: u32, load: u32, round: Round) -> u32 {
    if open_tiles == 0 || capacity == 0 {
        return 0;
    }
    let ticks = u64::from(round.ticks().max(1));
    let yielded = u64::from(capacity) * ticks;
    let drain = yielded.div_ceil((u64::from(ENERGY_REGEN_TIME) * u64::from(load)).max(1));
    let slots = u64::from(open_tiles) * ticks / u64::from(round.harvest.max(1));
    u32::try_from(drain.min(slots).max(1)).unwrap_or(u32::MAX)
}

fn count(body: &Body, part: Part) -> u32 {
    u32::try_from(body.count(part)).unwrap_or(u32::MAX)
}

/// The smallest body that can do a generalist worker's work: one WORK part (harvest, build,
/// repair, upgrade), one CARRY part (hold the energy) and one MOVE part (V54). What a room spawns
/// when it cannot pay for the solved body ([`worker_plan`]), and the body the solver gives a room
/// with nothing to size its workers for.
pub fn smallest_worker() -> Option<Body> {
    Body::new(&[Part::Work, Part::Carry, Part::Move])
}

/// One source as the worker solver sees it (P3.6): what sizes the workers its plan keeps there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerSource {
    /// It refills to this every `ENERGY_REGEN_TIME` ticks (V184). Unit: energy.
    pub capacity: u32,
    /// Walkable tiles next to it. Unit: tiles.
    pub open: u32,
    /// The way from it to the controller; `None` when none was found, and then it wants no worker.
    pub trip: Option<Trip>,
    /// Its miner was next to it (P3.1): a worker takes a load in one tick ([`carry_round`]).
    pub mined: bool,
    /// A link or haulers carry its energy to the controller (P3.2, P3.3): a worker takes it there
    /// and upgrades from there, so its round has no walk; it walks for its duties
    /// ([`WorkerTargets::duties`]). Without upgraders the linked sources with a trip share one pool.
    pub linked: bool,
    /// A container on its mining tile, which the workers repair (P3.5).
    pub container: bool,
    /// The upgraders spend its energy while they take the controller's (P3.4: linked, or counted
    /// in their budget): then it wants no worker.
    pub spent: bool,
}

/// What a room's generalist workers are sized for (P3.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerTargets<'a> {
    /// The room's sources, in the plan's order.
    pub sources: &'a [WorkerSource],
    /// The room's upgraders take the controller's energy (P3.4): the sources they spend want no
    /// worker, linked sources are not pooled, and the room keeps `min` workers even when no source
    /// wants one (or the workers its repairs take, [`worker_value`]); those beyond the sources'
    /// build ([`build_round`]).
    pub upgraders: bool,
    /// `opening.min_workers` and `opening.max_workers`. Unit: creeps.
    pub min: u32,
    pub max: u32,
    /// The most WORK parts a worker gets. Unit: parts.
    pub max_work: u32,
    /// What the room's own construction sites still need: one energy a progress point (V186).
    /// Unit: energy.
    pub build: u32,
    /// The way between the energy a worker takes by the controller (the upgraders' stores, where a
    /// link or haulers bring a linked source's energy) and the room's first spawn (the spawns and
    /// extensions to fill, and the sites, which are mostly the interim extensions by it, ADR-020):
    /// the walk of its builds and fills ([`WorkerTargets::duties`]). `None` without a spawn or a
    /// way: then no build work or fill counts.
    pub build_trip: Option<Trip>,
    /// When the workers fill the spawns and extensions (no hauler does, P3.3): what the room's
    /// other creeps cost a life (the miners' bodies), on top of the workers' own bodies, which they
    /// fill too. `None` while haulers fill. Unit: energy.
    pub fill: Option<u32>,
    /// What one transfer into a fill takes at most (the level's `EXTENSION_ENERGY_CAPACITY`), for
    /// the transfers a fill load needs ([`haul_round`]'s rule). Unit: energy.
    pub unload: u32,
    /// What the room's roads and own ramparts lose to decay over one creep life, which the workers
    /// repair (P7.3, ADR-032: [`road_decay`], [`rampart_decay`], [`repair_energy`]). Unit: energy.
    pub repair: u32,
    /// The way between the energy a worker takes by the controller and those roads and ramparts:
    /// their ways' mean, each weighted by what it loses. `None` when none of them is reached, and
    /// then no repair counts.
    pub repair_trip: Option<Trip>,
    /// `opening.body_slack_pct`: [`worker_plan`] takes the fewest workers among the bodies whose
    /// value is within this share of what is at stake of the best one. Unit: percent.
    pub slack_pct: u32,
}

/// No walk: a linked source's round (its workers take next to the controller and upgrade there).
const NO_WALK: Trip = Trip { tiles: 0, cost: 0 };

impl WorkerTargets<'_> {
    /// The workers walk for their duties away from their rounds: their energy is by the
    /// controller (a linked source wants workers, or the upgraders take and they build). Then the
    /// value counts the time of their builds, repairs and fills ([`worker_value`]); otherwise every
    /// worker's round runs between a source and the controller, the longest a load takes, and they
    /// do their duties on the way.
    pub fn duties(&self) -> bool {
        self.upgraders || self.sources.iter().any(|s| s.linked && s.trip.is_some())
    }

    /// The linked sources share one pool of workers ([`source_wants`]).
    fn pooled(&self) -> bool {
        !self.upgraders && self.sources.iter().filter(|s| pools(s)).count() > 1
    }

    /// The workers the room keeps for `sum` wanted by its sources: none when no source wants one
    /// (without upgraders), else `sum` within `min..=max`. Unit: creeps.
    pub fn room_want(&self, sum: u32) -> u32 {
        if sum == 0 && !self.upgraders {
            0
        } else {
            sum.clamp(self.min.min(self.max), self.max)
        }
    }
}

/// A source whose workers join the pool when it is pooled.
fn pools(s: &WorkerSource) -> bool {
    s.linked && s.trip.is_some()
}

/// The round a worker of `p` makes for source `s` (none when it wants no worker).
fn source_round(t: &WorkerTargets<'_>, s: &WorkerSource, p: Parts) -> Option<Round> {
    if t.upgraders && s.spent {
        return None;
    }
    let trip = s.trip?;
    let walk = if s.linked { NO_WALK } else { trip };
    round_of(p, walk, s.mined)
}

/// Workers of `body` each source of `t` keeps busy, into `out` in the sources' order: what
/// [`workers_for_source`] gives for the worker's round there ([`worker_round`], or [`carry_round`]
/// at a mined source; no walk at a linked one), 0 for a source
/// with no trip or one the upgraders spend. Without upgraders the linked sources with a trip, when
/// there are two or more, share one pool: their workers make the same round, so it is sized as one
/// source of their summed capacity and tiles, and split over them in order, rather than rounded up
/// per source. Unit: creeps.
pub fn source_wants(t: &WorkerTargets<'_>, body: &Body, out: &mut Vec<u32>) {
    wants_of(t, Parts::of(body), out);
}

fn wants_of(t: &WorkerTargets<'_>, p: Parts, out: &mut Vec<u32>) {
    out.clear();
    let load = p.load();
    out.extend(t.sources.iter().map(|s| {
        source_round(t, s, p).map_or(0, |r| workers_for_load(s.capacity, s.open, load, r))
    }));
    if !t.pooled() {
        return;
    }
    let Some(round) = round_of(p, NO_WALK, true) else {
        return;
    };
    let (cap_sum, open_sum) = t
        .sources
        .iter()
        .filter(|s| pools(s))
        .fold((0u32, 0u32), |(c, o), s| {
            (c.saturating_add(s.capacity), o.saturating_add(s.open))
        });
    let total = workers_for_load(cap_sum, open_sum, load, round);
    let n = u32::try_from(t.sources.iter().filter(|s| pools(s)).count()).unwrap_or(1);
    let (each, extra) = (total / n.max(1), total % n.max(1));
    let mut k = 0u32;
    for (s, want) in t.sources.iter().zip(out.iter_mut()) {
        if pools(s) {
            *want = each + u32::from(k < extra);
            k += 1;
        }
    }
}

/// A builder's round (P3.6): one tick to take a load, `trip` with every CARRY part full, building
/// it all at `BUILD_POWER` per WORK part a tick (one energy a progress point, V186), and `trip` back
/// empty. Building is the work the room keeps its workers for while the upgraders take the
/// controller's energy (P3.4); filling and repairing take fewer ticks a load. `None` for a body that
/// cannot build, carry or move. A lower bound, as [`walk_ticks`] is. Unit: ticks.
///
/// Uses: `CARRY_CAPACITY`, `BUILD_POWER`, [`walk_ticks`].
pub fn build_round(body: &Body, trip: Trip) -> Option<u32> {
    build_ticks(Parts::of(body), trip)
}

fn build_ticks(p: Parts, trip: Trip) -> Option<u32> {
    if p.work == 0 || p.carry == 0 {
        return None;
    }
    let (loaded, empty) = p.walks(trip)?;
    Some(
        1u32.saturating_add(loaded)
            .saturating_add(empty)
            .saturating_add(p.load().div_ceil(BUILD_POWER.saturating_mul(p.work))),
    )
}

/// What keeping one container on a mining tile of an owned room costs in repairs over one creep
/// life: it loses `CONTAINER_DECAY` hits every `CONTAINER_DECAY_TIME_OWNED` − 1 ticks (V214: the
/// next step is set `CONTAINER_DECAY_TIME_OWNED` after the tick it is checked against, which is
/// one before it), and a hit repaired costs `REPAIR_COST` energy (V222). Unit: energy.
///
/// Uses: `CREEP_LIFE_TIME`, `CONTAINER_DECAY`, `CONTAINER_DECAY_TIME_OWNED`, `REPAIR_COST`.
pub fn container_upkeep() -> u32 {
    let hits = u64::from(CREEP_LIFE_TIME) * u64::from(CONTAINER_DECAY)
        / u64::from(CONTAINER_DECAY_TIME_OWNED.saturating_sub(1).max(1));
    u32::try_from(hits.div_ceil(u64::from(hits_per_energy()))).unwrap_or(u32::MAX)
}

/// What one own rampart loses to decay over one creep life, whatever its hits: `RAMPART_DECAY_AMOUNT`
/// a step, one step every `RAMPART_DECAY_TIME` − 1 ticks (V304: the step runs once the tick reaches
/// `nextDecayTime` − 1 and sets the next one `RAMPART_DECAY_TIME` after it, as a container's does,
/// V214). Unit: hits.
///
/// Uses: `CREEP_LIFE_TIME`, `RAMPART_DECAY_AMOUNT`, `RAMPART_DECAY_TIME`.
pub fn rampart_decay() -> u64 {
    u64::from(CREEP_LIFE_TIME).saturating_mul(u64::from(RAMPART_DECAY_AMOUNT))
        / u64::from(RAMPART_DECAY_TIME.saturating_sub(1).max(1))
}

/// What one road of `max` hits loses to decay over one creep life. Its `hitsMax` is `ROAD_HITS`
/// times its terrain's ratio (V186), and each step removes `ROAD_DECAY_AMOUNT` times the same ratio,
/// one step every `ROAD_DECAY_TIME` ticks (V242). A lower bound: each creep's step on it brings the
/// next step forward by `ROAD_WEAROUT` a body part (V54), and that wear is not counted. Unit: hits.
///
/// Uses: `CREEP_LIFE_TIME`, `ROAD_DECAY_AMOUNT`, `ROAD_DECAY_TIME`, `ROAD_HITS`.
pub fn road_decay(max: u32) -> u64 {
    u64::from(CREEP_LIFE_TIME)
        .saturating_mul(u64::from(ROAD_DECAY_AMOUNT))
        .saturating_mul(u64::from(max))
        / u64::from(ROAD_HITS)
            .saturating_mul(u64::from(ROAD_DECAY_TIME))
            .max(1)
}

/// The energy that repairs `hits`, rounded up ([`hits_per_energy`] hits an energy, V222). Unit:
/// energy.
///
/// Uses: `REPAIR_COST`.
pub fn repair_energy(hits: u64) -> u32 {
    u32::try_from(hits.div_ceil(u64::from(hits_per_energy()))).unwrap_or(u32::MAX)
}

/// A repairer's round ([`repair_ticks`]) for `body` on `trip`. `None` for a body that cannot repair,
/// carry or move. A lower bound, as [`walk_ticks`] is. Unit: ticks.
///
/// Uses: `CARRY_CAPACITY`, `REPAIR_POWER`, `REPAIR_COST`, [`walk_ticks`].
pub fn repair_round(body: &Body, trip: Trip) -> Option<u32> {
    repair_ticks(Parts::of(body), trip)
}

/// The hits one energy repairs: `1 / REPAIR_COST` (V222), rounded to a whole number, since the
/// crate's `REPAIR_COST` is an `f32` (0.01 widens to 0.0099999998, which a division would round up
/// a tick or an energy too far). At least 1. Unit: hits.
///
/// Uses: `REPAIR_COST`.
pub(super) fn hits_per_energy() -> u32 {
    let hits = (1.0 / f64::from(REPAIR_COST)).round();
    if hits.is_finite() && hits >= 1.0 {
        hits.min(f64::from(u32::MAX)) as u32
    } else {
        1
    }
}

/// A repairer's round (P3.6): one tick to take a load, `trip` loaded and back empty, and the
/// repairs that spend it: each WORK part repairs `REPAIR_POWER` hits a tick for `REPAIR_COST`
/// energy a hit (V222). `None` for a body that cannot repair, carry or move. Unit: ticks.
///
/// Uses: `CARRY_CAPACITY`, `REPAIR_POWER`, `REPAIR_COST`, [`walk_ticks`].
fn repair_ticks(p: Parts, trip: Trip) -> Option<u32> {
    if p.work == 0 || p.carry == 0 {
        return None;
    }
    let (loaded, empty) = p.walks(trip)?;
    // A load lasts `load × hits_per_energy` hits at `REPAIR_POWER` a WORK part a tick.
    let spend = u64::from(p.load())
        .saturating_mul(u64::from(hits_per_energy()))
        .div_ceil(
            u64::from(p.work)
                .saturating_mul(u64::from(REPAIR_POWER))
                .max(1),
        );
    let spend = u32::try_from(spend).unwrap_or(u32::MAX);
    Some(
        1u32.saturating_add(loaded)
            .saturating_add(empty)
            .saturating_add(spend),
    )
}

/// A filler's round (P3.6): one tick to take a load, `trip` loaded and back empty, and the
/// transfers that hand it over into fills of at most `unload` each, with a step between two
/// ([`haul_round`]'s rule). `None` for a body that cannot carry or move. Unit: ticks.
///
/// Uses: `CARRY_CAPACITY`, [`walk_ticks`].
fn fill_ticks(p: Parts, trip: Trip, unload: u32) -> Option<u32> {
    if p.carry == 0 {
        return None;
    }
    let (loaded, empty) = p.walks(trip)?;
    let fills = p.load().div_ceil(unload.max(1)).max(1);
    Some(
        loaded
            .saturating_add(empty)
            .saturating_add(fills.saturating_mul(2)),
    )
}

/// The fraction of a worker, in millionths, that spends `energy` a life in loads of `load` making
/// `round` each. Uses: `CREEP_LIFE_TIME`.
fn duty(energy: u128, load: u32, round: u32) -> u128 {
    energy
        .saturating_mul(u128::from(round))
        .saturating_mul(MICRO)
        / u128::from(load)
            .saturating_mul(u128::from(CREEP_LIFE_TIME))
            .max(1)
}

/// The fixed point of [`worker_value`]'s fractional workers: millionths of a worker.
const MICRO: u128 = 1_000_000;

/// What workers of `load` making rounds of `round` ticks, `micro` millionths of them, carry over
/// one creep life (`CREEP_LIFE_TIME`, V170). Unit: energy.
///
/// Uses: `CREEP_LIFE_TIME`.
fn carried(micro: u128, load: u32, round: u32) -> u128 {
    micro
        .saturating_mul(u128::from(load))
        .saturating_mul(u128::from(CREEP_LIFE_TIME))
        / u128::from(round.max(1)).saturating_mul(MICRO)
}

/// The fraction of workers of `load` making `round` that keep a source of `capacity` with
/// `open` tiles busy, in millionths: [`workers_for_source`] before its rounding up and its floor of
/// one, so bodies of the same proportions are worth the same whatever their size.
///
/// Uses: `ENERGY_REGEN_TIME`.
fn fraction(capacity: u32, open: u32, load: u32, round: Round) -> u128 {
    let ticks = u128::from(round.ticks().max(1));
    let drain = u128::from(capacity)
        .saturating_mul(ticks)
        .saturating_mul(MICRO)
        / u128::from(ENERGY_REGEN_TIME)
            .saturating_mul(u128::from(load))
            .max(1);
    let slots = u128::from(open).saturating_mul(ticks).saturating_mul(MICRO)
        / u128::from(round.harvest.max(1));
    drain.min(slots)
}

/// What the room's workers of `body` put to use over one creep life, net of their bodies (P3.6),
/// and how many the room keeps for `wants` from [`source_wants`] for that body. The value counts
/// workers as fractions ([`fraction`]): each source (a pool as one source of the pooled capacity
/// and tiles) keeps the fraction of a worker that carries what it yields over the life
/// ([`yield_per_life`]), or what its open tiles let them. When the workers walk for their duties
/// ([`WorkerTargets::duties`]), the time of those duties adds to it: the build work on
/// [`build_round`]s, each mined source's container ([`container_upkeep`]) on repair rounds along
/// its trip, and, while no hauler fills, the fills (the workers' bodies and the others' in
/// [`WorkerTargets::fill`]) on fill rounds along the build trip. Whether or not they walk for
/// those, the decay of the room's roads and own ramparts ([`WorkerTargets::repair`]) adds repair
/// rounds on [`WorkerTargets::repair_trip`] (P7.3). Without upgraders the room keeps the sum within
/// `min..=max` (none when nothing wants a worker); with upgraders it keeps its whole count for
/// `wants` (`min_workers` when the upgraders spend every source), and the builds are what the
/// workers put to use. Either way the count is at least the whole workers that the sources'
/// fractions and the repairs (containers, roads and ramparts) take together, within `min..=max`:
/// a structure whose decay the workers cannot keep up with is lost, so the repairs never wait for
/// the time the builds leave. When the room keeps fewer than the time needs, they carry and
/// build that share. Every worker kept costs its body once a life (`BODYPART_COST`). Counted whole, smaller bodies would look cheaper only by
/// rounding (a source's last worker partly idle); counted as fractions, bodies of the same
/// proportions tie, and [`worker_plan`] takes the larger (fewer creeps, fewer intents). Unit:
/// (energy, creeps: the whole workers the room keeps for `wants`).
///
/// Uses: `BODYPART_COST` (`Part::cost`), `CREEP_LIFE_TIME`, `ENERGY_REGEN_TIME`, [`yield_per_life`],
/// [`build_round`], [`container_upkeep`], [`repair_round`].
pub fn worker_value(t: &WorkerTargets<'_>, body: &Body, wants: &[u32]) -> (i64, u32) {
    value_of(t, Parts::of(body), body.cost(), wants)
}

fn value_of(t: &WorkerTargets<'_>, p: Parts, cost: u32, wants: &[u32]) -> (i64, u32) {
    let load = p.load();
    let sum = wants.iter().fold(0u32, |a, &w| a.saturating_add(w));
    let n = t.room_want(sum);
    let pooled = t.pooled();
    let (mut delivered, mut workers) = (0u128, 0u128);
    let mut add = |capacity: u32, open: u32, round: Round| {
        if capacity == 0 || open == 0 {
            return;
        }
        let f = fraction(capacity, open, load, round);
        workers = workers.saturating_add(f);
        let got = carried(f, load, round.ticks()).min(u128::from(yield_per_life(capacity)));
        delivered = delivered.saturating_add(got);
    };
    let (mut pool_cap, mut pool_open) = (0u32, 0u32);
    for s in t.sources {
        if pooled && pools(s) {
            pool_cap = pool_cap.saturating_add(s.capacity);
            pool_open = pool_open.saturating_add(s.open);
        } else if let Some(r) = source_round(t, s, p) {
            add(s.capacity, s.open, r);
        }
    }
    if pooled && let Some(r) = round_of(p, NO_WALK, true) {
        add(pool_cap, pool_open, r);
    }
    // The duties: a body that cannot make one of them is worth nothing here. `repairs` is their
    // share that keeps structures from decaying away: the room keeps the workers it takes (below).
    let mut duties = 0u128;
    let mut repairs = 0u128;
    // Roads and ramparts (P7.3): away from any round, wherever the workers' energy is.
    if t.repair > 0
        && let Some(trip) = t.repair_trip
    {
        let Some(r) = repair_ticks(p, trip) else {
            return (i64::MIN, n);
        };
        repairs = duty(u128::from(t.repair), load, r);
        duties = repairs;
    }
    if t.duties() {
        if t.build > 0
            && let Some(trip) = t.build_trip
        {
            let Some(r) = build_ticks(p, trip) else {
                return (i64::MIN, n);
            };
            duties = duties.saturating_add(duty(u128::from(t.build), load, r));
        }
        let upkeep = u128::from(container_upkeep());
        for trip in t
            .sources
            .iter()
            .filter(|s| s.container)
            .filter_map(|s| s.trip)
        {
            let Some(r) = repair_ticks(p, trip) else {
                return (i64::MIN, n);
            };
            let d = duty(upkeep, load, r);
            duties = duties.saturating_add(d);
            repairs = repairs.saturating_add(d);
        }
        if let (Some(others), Some(trip)) = (t.fill, t.build_trip) {
            let Some(r) = fill_ticks(p, trip, t.unload) else {
                return (i64::MIN, n);
            };
            // The workers refill their own bodies too, a fraction of a body each.
            let own = workers
                .saturating_add(duties)
                .saturating_mul(u128::from(cost))
                / MICRO;
            let filled = own.saturating_add(u128::from(others));
            duties = duties.saturating_add(duty(filled, load, r));
        }
    }
    // The room keeps the whole workers its sources and repairs take together, when the sources'
    // own rounding leaves too little time for the repairs: a structure the workers cannot keep up
    // with decays away (P7.3).
    let whole = u32::try_from(workers.saturating_add(repairs).div_ceil(MICRO)).unwrap_or(u32::MAX);
    let n = if repairs > 0 {
        t.room_want(sum.max(whole))
    } else {
        n
    };
    let need = workers.saturating_add(duties);
    let kept = if t.upgraders {
        // The room keeps `n` workers (`min_workers` when the upgraders spend every source): no
        // rounding to smooth, so they are counted whole, and a build they cannot finish in a life
        // is built in that share.
        u128::from(n).saturating_mul(MICRO)
    } else if need == 0 {
        0
    } else {
        let max = u128::from(t.max).saturating_mul(MICRO);
        let min = u128::from(t.min.min(t.max)).saturating_mul(MICRO);
        need.clamp(min, max)
    };
    let mut built = if t.upgraders && t.build_trip.is_some() {
        u128::from(t.build)
    } else {
        0
    };
    if kept < need {
        delivered = delivered.saturating_mul(kept) / need.max(1);
        built = built.saturating_mul(kept) / need.max(1);
    }
    let paid = kept.saturating_mul(u128::from(cost)) / MICRO;
    let used = i64::try_from(delivered.saturating_add(built)).unwrap_or(i64::MAX);
    let paid = i64::try_from(paid).unwrap_or(i64::MAX);
    (used.saturating_sub(paid), n)
}

/// The generalist worker's body for a room whose spawns and extensions hold at most `energy`
/// (P3.6), among the bodies of WORK, CARRY and MOVE parts (at least one each, at most
/// `t.max_work` WORK parts and `MAX_CREEP_SIZE` parts) that cost at most `energy`: of those whose
/// workers put to use over a creep life, net of their bodies ([`worker_value`]), at least the most
/// any of them does less the slack ([`WorkerTargets::slack_pct`] of the sources' yield and the
/// build work), the one with the fewest workers (each is a creep's intents every tick: values that
/// close are within the round model's error, its walks being lower bounds), then the most value,
/// then the least energy for all of them, then the cheapest body, then the fewest MOVE parts.
/// Without upgraders, while a linked source wants workers, only bodies that walk empty at one tile
/// a tick on plain count (`MOVE_COST_PLAIN` a WORK part within `MOVE_POWER` a MOVE part): their
/// duties start empty and come to few workers ([`WorkerTargets::duties`]). MOVE parts past the ones that bring every loaded walk the workers
/// make down to its tiles save nothing. The parts go WORK, CARRY, MOVE. One pass over the bodies,
/// keeping those within the slack of the best so far in `scratch`. `None` when no body fits
/// `energy` (the smallest worker costs more, or none walks fast enough) or `t.max_work` is 0.
///
/// Uses: `BODYPART_COST` (`Part::cost`), `MAX_CREEP_SIZE`, `MOVE_COST_PLAIN`, `MOVE_POWER`,
/// [`source_wants`], [`worker_value`], [`walk_ticks`], [`yield_per_life`].
pub fn worker_plan(
    t: &WorkerTargets<'_>,
    energy: u32,
    scratch: &mut WorkerScratch,
) -> Option<Body> {
    // One pass: the bodies within the slack of the best so far, dropped as the best rises.
    let slack = slack(t);
    let WorkerScratch { wants, near } = scratch;
    near.clear();
    let mut top: Option<i64> = None;
    each_worker(t, energy, wants, |p, value, n, cost| {
        if top.is_none_or(|v| value > v) {
            top = Some(value);
            let floor = value.saturating_sub(slack);
            near.retain(|&(key, _)| key.1.saturating_neg() >= floor);
        }
        if top.is_some_and(|v| value >= v.saturating_sub(slack)) {
            let key = (
                n,
                value.saturating_neg(),
                u64::from(n) * u64::from(cost),
                cost,
                p.moves,
            );
            near.push((key, p));
        }
    });
    let (_, p) = near.iter().min_by_key(|&&(key, _)| key).copied()?;
    worker_body(p.work, p.carry, p.moves)
}

/// The worker solver's scratch, kept for its capacity ([`worker_plan`]): the sources' wants of the
/// body being valued, and the bodies within the slack of the best so far.
#[derive(Default)]
pub struct WorkerScratch {
    pub wants: Vec<u32>,
    near: Vec<(PickKey, Parts)>,
}

/// [`worker_plan`]'s order among the bodies within the slack: workers, the value negated, the energy
/// for all of them, the body's cost, MOVE parts (lower first).
type PickKey = (u32, i64, u64, u32, u32);

/// The value a solved worker may give up for fewer workers ([`WorkerTargets::slack_pct`]): that
/// share of what is at stake, the yield over a creep life of the sources it sizes workers for
/// ([`yield_per_life`]) and, with upgraders, the build work. Unit: energy.
fn slack(t: &WorkerTargets<'_>) -> i64 {
    let stake = t
        .sources
        .iter()
        .filter(|s| s.trip.is_some() && !(t.upgraders && s.spent))
        .fold(0u64, |a, s| a.saturating_add(yield_per_life(s.capacity)))
        .saturating_add(if t.upgraders { u64::from(t.build) } else { 0 });
    i64::try_from(stake.saturating_mul(u64::from(t.slack_pct)) / 100).unwrap_or(i64::MAX)
}

/// Calls `visit` with the parts, value, workers and cost of every worker body [`worker_plan`]
/// searches, in a fixed order: WORK, then CARRY, then MOVE ascending, each MOVE count up to the
/// one that brings every loaded walk to its tiles.
fn each_worker(
    t: &WorkerTargets<'_>,
    energy: u32,
    wants: &mut Vec<u32>,
    mut visit: impl FnMut(Parts, i64, u32, u32),
) {
    let (wc, cc, mc) = (Part::Work.cost(), Part::Carry.cost(), Part::Move.cost());
    let price = |w: u32, c: u32, m: u32| {
        wc.saturating_mul(w)
            .saturating_add(cc.saturating_mul(c))
            .saturating_add(mc.saturating_mul(m))
    };
    // The trips whose loaded walk more MOVE parts can shorten.
    let walks = |p: Parts| {
        let floor = |trip: Trip| {
            walk_ticks(trip, p.others.saturating_add(p.carry), p.moves)
                .is_none_or(|w| w <= trip.tiles)
        };
        t.sources
            .iter()
            .filter_map(|s| s.trip)
            .chain(t.build_trip)
            // A way of no tiles (every road and rampart by the controller) has no floor to reach.
            .chain(t.repair_trip.filter(|r| t.repair > 0 && r.tiles > 0))
            .all(floor)
    };
    // Without upgraders, workers whose rounds have no walk (a linked source's) walk their duties'
    // trips empty at one tile a tick: those duties start empty (a repair, a site, a fill) and come
    // to few workers, so a slow one leaves the room short for its whole walk, which the fleet's
    // fractions do not show.
    let duties = !t.upgraders && t.duties();
    // Empty, at a tile a tick on plain: the fatigue a plain tile adds (`MOVE_COST_PLAIN` a part that
    // weighs) within what the MOVE parts remove a tick (`MOVE_POWER` each, V54, V55). The smallest
    // worker always passes, so the floor never empties the search; the duty trips' own terrain is
    // in their time in the value.
    let empty_floor = |p: Parts| {
        !duties
            || u64::from(p.others) * u64::from(MOVE_COST_PLAIN)
                <= u64::from(MOVE_POWER) * u64::from(p.moves)
    };
    let most_work = t.max_work.min(MAX_CREEP_SIZE.saturating_sub(2));
    for work in 1..=most_work {
        if price(work, 1, 1) > energy {
            break;
        }
        for carry in 1..=MAX_CREEP_SIZE.saturating_sub(work).saturating_sub(1) {
            if price(work, carry, 1) > energy {
                break;
            }
            for moves in 1..=MAX_CREEP_SIZE.saturating_sub(work).saturating_sub(carry) {
                let cost = price(work, carry, moves);
                if cost > energy {
                    break;
                }
                let p = Parts::worker(work, carry, moves);
                if !empty_floor(p) {
                    continue;
                }
                wants_of(t, p, wants);
                let (value, n) = value_of(t, p, cost, wants);
                visit(p, value, n, cost);
                if walks(p) {
                    break;
                }
            }
        }
    }
}

/// The body of `work` WORK, `carry` CARRY and `moves` MOVE parts, in that order.
fn worker_body(work: u32, carry: u32, moves: u32) -> Option<Body> {
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
/// ENERGY_REGEN_TIME` (the miner's WORK parts take at least that). That loss is the one without
/// pre-spawn replacement: with `spawn.replace` on (P5.2) the walk overlaps the old miner's harvest
/// and costs only the new miner's life, so this body can carry more MOVE parts than it needs (not
/// re-solved yet). The walk is [`walk_ticks`] with the WORK parts as the weight (no CARRY). Ties go
/// to fewer MOVE parts; MOVE parts past the one
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
    miner_body_work(miner_work(capacity), carry, capacity, trip, energy)
}

/// A static miner's body as [`miner_body_with`] gives it, with `work` WORK parts instead of
/// [`miner_work`]'s (P6.1: a remote miner also repairs its container, so it takes more,
/// [`crate::economy::remote_miner_work`]). The MOVE parts are solved as in [`miner_body`], against
/// the walk loss of a source of `capacity`. `None` for no WORK part, or when the parts with one
/// MOVE cost more than `energy` or pass `MAX_CREEP_SIZE`.
///
/// Uses: `BODYPART_COST` (`Part::cost`), `ENERGY_REGEN_TIME`, `MAX_CREEP_SIZE`, [`walk_ticks`].
pub fn miner_body_work(
    work: u32,
    carry: u32,
    capacity: u32,
    trip: Trip,
    energy: u32,
) -> Option<Body> {
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
    hauler_plan_with(capacity, trip, energy, margin_pct, unload, 0)
}

/// The haulers [`hauler_plan`] gives, each with `work` WORK parts before its CARRY parts (P6.1: a
/// remote hauler repairs the road under it as it walks, and a `repair` and a `move` run in the
/// same tick, V45, V46). A WORK part makes fatigue loaded and empty (V54), so it weighs in both
/// walks of the round ([`haul_round`]), and it costs its `BODYPART_COST` and a share of
/// `MAX_CREEP_SIZE`. With `work` 0 this is [`hauler_plan`]. `None` as for [`hauler_plan`], or when
/// the WORK parts, one CARRY and one MOVE cost more than `energy` or pass `MAX_CREEP_SIZE`.
///
/// Uses: `BODYPART_COST` (`Part::cost`), `MAX_CREEP_SIZE`, `CONTAINER_CAPACITY`, `CARRY_CAPACITY`,
/// [`haul_round`], [`haulers_for`].
pub fn hauler_plan_with(
    capacity: u32,
    trip: Trip,
    energy: u32,
    margin_pct: u32,
    unload: u32,
    work: u32,
) -> Option<(Body, u32)> {
    if capacity == 0 {
        return None;
    }
    let carry_cost = Part::Carry.cost();
    let move_cost = Part::Move.cost();
    let work_cost = Part::Work.cost().saturating_mul(work);
    // (haulers, energy for all of them, MOVE parts, CARRY parts)
    let mut best: Option<(u32, u64, u32, u32)> = None;
    let most = CONTAINER_CAPACITY
        .checked_div(CARRY_CAPACITY)
        .unwrap_or(0)
        .min(MAX_CREEP_SIZE.saturating_sub(1).saturating_sub(work));
    for carry in 1..=most {
        let fixed = carry_cost.saturating_mul(carry).saturating_add(work_cost);
        if fixed.saturating_add(move_cost) > energy {
            break;
        }
        for moves in 1..=MAX_CREEP_SIZE.saturating_sub(carry).saturating_sub(work) {
            let cost = fixed.saturating_add(move_cost.saturating_mul(moves));
            if cost > energy {
                break;
            }
            let Some(round) = round_ticks(trip, work, carry, moves, unload) else {
                break;
            };
            let n = haulers_for(capacity, carry, round, margin_pct);
            let key = (n, u64::from(n) * u64::from(cost), moves, carry);
            if best.is_none_or(|b| (key.0, key.1, key.2) < (b.0, b.1, b.2)) {
                best = Some(key);
            }
            if walk_ticks(trip, work.saturating_add(carry), moves).is_some_and(|w| w <= trip.tiles)
            {
                break;
            }
        }
    }
    let (n, _, moves, carry) = best?;
    let mut parts = [Part::Move; MAX_CREEP_SIZE as usize];
    let work_n = usize::try_from(work).ok()?;
    let carry_n = usize::try_from(carry).ok()?;
    let len = work_n
        .checked_add(carry_n)?
        .checked_add(usize::try_from(moves).ok()?)?;
    for (i, part) in parts.iter_mut().enumerate().take(work_n + carry_n) {
        *part = if i < work_n { Part::Work } else { Part::Carry };
    }
    Some((Body::new(parts.get(..len)?)?, n))
}

/// What a source of `capacity` yields over one creep life: `capacity` every `ENERGY_REGEN_TIME`
/// ticks (V184), for `CREEP_LIFE_TIME` ticks (V170). The upgraders' budget is counted per creep
/// life, so every creep's body cost (paid once a life) adds up with it in whole energy. An upper
/// bound on what a mined source gives: with `spawn.replace` off (P5.2), or a replacement whose
/// order waits until the miner is gone, each miner life loses the replacement's spawning and walk
/// out. Unit: energy.
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
/// life left after its spawning (`CREEP_SPAWN_TIME` a part, V170: with `spawn.replace` off its
/// replacement is ordered once it is gone, so the room waits that long; P5.2's pre-spawn hides it)
/// and its walk out (empty, so only the WORK parts weigh, V54). A lower bound: spawning lengthens
/// its cycle rather than shortening its life. `None`
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
