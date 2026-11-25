//! Claim scoring (ROADMAP P8.2, SPEC §3.7, ADR-041): which room near the homes the bot should own
//! next. It ranks the rooms of the atlas ([`super::Atlas`]), so it needs no vision of its own, and
//! checks the best few with the base planner, so a room where no base fits is never chosen.
//!
//! # Which rooms are candidates
//!
//! A room of the atlas is a candidate when a claimer could take its controller (the game's
//! `claimController` checks, V374 to V381) and the bot could hold it:
//! - It was read, and the explorer did not find it unreachable on its last try.
//! - Its controller has no owner. `claimController` refuses a controller above level 0, the bot's
//!   own included (V374). No other user's reservation holds it (V375); the bot's own may.
//! - No invader core stands there, and no other user's towers or spawns.
//! - It has the status of a home within `claim.max_distance` rooms. Rooms of another status are
//!   walled off from the home (V168). It is never closed, and never a novice area once the bot owns
//!   `GCL_NOVICE` rooms, where `claimController` returns `ERR_FULL` (V374, V381).
//! - It has at least `claim.min_sources` sources (the user's choice: one source is enough, and the
//!   score decides).
//!
//! The global control level (GCL), which caps how many rooms the bot may own (V378), is not a
//! filter: the ranking holds whether or not a claim is allowed now, and P8.4 keeps to the cap.
//!
//! # The score
//!
//! Each term is in milli-energy a tick (me/t, a thousandth of an energy unit each tick), so the
//! terms add up to one value ([`Terms::total`]). The energy terms come from the constants. The
//! weights that have no constant behind them are [`ClaimConfig`] fields.
//! - **Sources** (+): each source refills to `SOURCE_ENERGY_CAPACITY` every `ENERGY_REGEN_TIME`
//!   ticks in an owned room (V184), so it yields their ratio a tick ([`source_yield`]).
//! - **Remotes** (+): the sources of the rooms across the candidate's exits that the bot could mine
//!   from it, at `claim.remote_pct` percent of a reserved room's yield, for the best
//!   `remote.max_rooms` of them (a home mines no more at once, ADR-029). A room counts only when it
//!   is a room that can have a controller, read, with no owner, no other user's reservation,
//!   towers, spawns or invader core, and with no owned room across one of its own exits (that
//!   room's home would mine it already).
//! - **Distance** (−): `claim.distance_cost` for each room between the candidate and its nearest
//!   home, for the pioneers' and defenders' walks.
//! - **Foreign rooms** (−): for each room another player owns within `claim.threat_range` rooms,
//!   `claim.foreign_cost` times its level over `MAX_LEVEL`, and once more for each room it is
//!   closer than the range's end ([`foreign_penalty`]).
//! - **Walks** (−, from the plan): the haulers that carry each source's yield from its mining tile
//!   to the base's hub and the sources' yield from the hub to the controller's container, over the
//!   plan's roads ([`haul_cost`]).
//! - **Ramparts** (−, from the plan): the energy that repairs what the plan's ramparts lose to decay
//!   ([`rampart_cost`]).
//!
//! # The plan check
//!
//! Before it is planned, a candidate's value is an upper bound: the plan's terms only subtract, and
//! the plan's terrain can only close exits the remotes term assumed open. The ranking is sorted by
//! value, and the first unplanned candidate is planned while fewer than `claim.plan_count`
//! planned candidates rank above it (the user's "Plan the top 3"). So once the top `plan_count` are
//! planned, no unplanned room can beat them. A plan reads the room's terrain without vision
//! (`Game.map.getRoomTerrain`, V156) and runs the flood planner ([`crate::planner::flood`], ADR-007)
//! with no standing spawn, one phase per step in the Optional tier, then its validator. A room whose
//! plan fails or does not validate leaves the ranking until its sources or controller change.
//! A plan depends only on the terrain and the room's natural objects, which do not change, so its
//! outcome is kept for the global (the heap only; a reset plans the top rooms again).
//!
//! The ranking is rebuilt whenever the atlas or the set of owned rooms changes, and at least every
//! `claim.score_ticks` (a reservation that ends changes nothing in the atlas), and the plans'
//! outcomes are applied to it again.

use core::cmp::Reverse;

use rustedscreep_consts::{
    CARRY_CAPACITY, CREEP_LIFE_TIME, ENERGY_REGEN_TIME, GCL_NOVICE, Part, ROOM_SIZE, RoomName,
    SOURCE_ENERGY_CAPACITY, StructureType,
};

use super::atlas::{Atlas, MAX_SOURCES, Reserved, RoomIntel};
use crate::config::ClaimConfig;
use crate::economy::is_remote_room;
use crate::economy::sizing::{rampart_decay, repair_energy};
use crate::movement::{Side, TerrainGrid, TerrainKind, Tile};
use crate::planner::flood::{Params, Run};
use crate::planner::{Layout, MAX_LEVEL, Metrics, PlanError, PlanInput, Scratch, validate};
use crate::scheduler::Step;
use crate::snapshot::{Controller, RoomStatus};
use crate::spawning::room_distance;

/// Milli-energy in one energy unit.
const MILLI: i64 = 1000;

/// How many candidates the console summary lists.
pub const SHOWN: usize = 5;

/// A home claims are scored from: an owned room with a spawn ([`super::Home`]), and its status as
/// `Game.map.getRoomStatus` gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimHome {
    pub room: RoomName,
    pub status: RoomStatus,
}

/// Why a room of the atlas is not a candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// Not read yet, or the explorer found no route there on its last try.
    Unknown,
    /// No controller, or its tile was not read.
    NoController,
    /// Its controller has an owner (V374).
    Owned,
    /// Another user's reservation holds it (V375).
    Reserved,
    /// An invader core, or another user's towers or spawns.
    Hostile,
    /// No home within `claim.max_distance` has its status, it is closed, or it is a novice area
    /// while the bot owns `GCL_NOVICE` rooms (V374, V381).
    Status,
    /// Farther than `claim.max_distance` from every home.
    Far,
    /// Fewer sources than `claim.min_sources`, or none.
    Sources,
    /// The base planner found no valid plan.
    Unfit,
}

impl Skip {
    /// Every reason, in the console's order.
    pub const ALL: [Self; 9] = [
        Self::Unknown,
        Self::NoController,
        Self::Owned,
        Self::Reserved,
        Self::Hostile,
        Self::Status,
        Self::Far,
        Self::Sources,
        Self::Unfit,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NoController => "no controller",
            Self::Owned => "owned",
            Self::Reserved => "reserved",
            Self::Hostile => "hostile",
            Self::Status => "status",
            Self::Far => "far",
            Self::Sources => "sources",
            Self::Unfit => "unfit",
        }
    }

    const fn slot(self) -> usize {
        match self {
            Self::Unknown => 0,
            Self::NoController => 1,
            Self::Owned => 2,
            Self::Reserved => 3,
            Self::Hostile => 4,
            Self::Status => 5,
            Self::Far => 6,
            Self::Sources => 7,
            Self::Unfit => 8,
        }
    }
}

/// A candidate's score, term by term (module docs). Unit: milli-energy a tick (me/t).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Terms {
    pub sources: i64,
    pub remotes: i64,
    pub distance: i64,
    pub foreign: i64,
    pub walks: i64,
    pub ramparts: i64,
}

impl Terms {
    /// The value: every term added. Unit: me/t.
    pub const fn total(&self) -> i64 {
        self.sources
            .saturating_add(self.remotes)
            .saturating_add(self.distance)
            .saturating_add(self.foreign)
            .saturating_add(self.walks)
            .saturating_add(self.ramparts)
    }
}

/// What a valid plan of a candidate gives the score and the claimers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    /// The first spawn's tile: the plan's level-1 spawn, where P8.3's pioneers build.
    pub spawn: Tile,
    /// The plan's ramparts. Unit: structures.
    pub ramparts: u32,
    /// The path costs from the hub to each source's mining tile, summed, and from the hub to the
    /// controller's container, over the plan's roads (`planner::Metrics`). Unit: path cost, a tick
    /// a road tile for a hauler at one MOVE part to two CARRY parts.
    pub source_ways: u32,
    pub controller_way: u32,
    /// The sides whose edge has a tile that is not a wall: the room's exits (`Side::ALL`'s order).
    pub open: [bool; 4],
}

/// One room the bot could claim, with its score.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub room: RoomName,
    /// The nearest home of the room's status, and how far it is by the larger of the two
    /// room-coordinate differences. Unit: rooms.
    pub home: RoomName,
    pub distance: u32,
    /// Unit: sources.
    pub sources: u8,
    pub terms: Terms,
    /// The plan check's outcome, once it ran.
    pub plan: Option<Fit>,
}

impl Candidate {
    /// Unit: me/t.
    pub const fn value(&self) -> i64 {
        self.terms.total()
    }
}

/// What happened this tick, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimEvent {
    /// The best planned candidate changed (or the first one came): the room and its value in me/t,
    /// or `None` when there is no longer one.
    Best { room: Option<RoomName>, value: i64 },
    /// A candidate's plan failed (the planner's reason).
    Unfit { room: RoomName, why: &'static str },
    /// A candidate's terrain could not be read; it is left out until the next timed score.
    NoTerrain { room: RoomName },
}

/// Counters since the global started, for the console.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaimStats {
    pub scores: u32,
    pub plans: u32,
    pub fits: u32,
    pub steps: u32,
}

/// The facts a plan is made from: a plan's outcome is kept while they hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    controller: Tile,
    sources: [Option<Tile>; MAX_SOURCES],
    mineral: Option<Tile>,
}

impl Key {
    fn of(e: &RoomIntel) -> Option<Self> {
        Some(Self {
            controller: e.controller_tile?,
            sources: e.sources,
            mineral: e.mineral.map(|d| d.tile),
        })
    }
}

/// A plan's outcome for one room.
#[derive(Clone, Copy, Debug)]
struct Outcome {
    room: RoomName,
    key: Key,
    fit: Result<Fit, &'static str>,
}

/// The outcome of a room whose terrain could not be read: kept until the next timed score.
const NO_TERRAIN: &str = "no terrain";

/// The plan being made, a phase a step.
struct Active {
    room: RoomName,
    key: Key,
    terrain: TerrainGrid,
    /// In tile order, as the planner wants them.
    sources: Vec<Tile>,
    run: Option<Run>,
    /// The finished layout, waiting for its validation step.
    layout: Option<Layout>,
}

impl Active {
    fn input(&self) -> PlanInput<'_> {
        PlanInput {
            terrain: &self.terrain,
            controller: self.key.controller,
            sources: &self.sources,
            mineral: self.key.mineral,
            spawn: None,
        }
    }
}

/// Claim scoring for one global (module docs).
#[derive(Default)]
pub struct Claims {
    /// The ranking, best first: value, then room.
    candidates: Vec<Candidate>,
    skipped: [u32; Skip::ALL.len()],
    scored_at: Option<u32>,
    /// The atlas's revision ([`super::Scouting::revision`]) and the owned rooms at the last score.
    scored_rev: u32,
    scored_owned: Vec<RoomName>,
    /// `remote.max_rooms` and the claim config at the last score: the remotes term counts this
    /// many neighbors, and a change of either makes a score due.
    remote_rooms: u32,
    scored_cfg: Option<ClaimConfig>,
    /// The planner's tunables the outcomes were made with: a change drops them all.
    params: Option<Params>,
    /// The plans' outcomes, sorted by room.
    outcomes: Vec<Outcome>,
    active: Option<Box<Active>>,
    scratch: Option<Box<Scratch>>,
    /// The best planned candidate last reported.
    best: Option<RoomName>,
    events: Vec<ClaimEvent>,
    stats: ClaimStats,
}

impl Claims {
    /// The ranking, best first.
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// The room to claim: the first of the ranking once it is planned (no unplanned room can beat
    /// it, module docs).
    pub fn best(&self) -> Option<&Candidate> {
        self.candidates.first().filter(|c| c.plan.is_some())
    }

    /// The rooms left out at the last score, by reason ([`Skip::ALL`]'s order), with the unfit
    /// ones since.
    pub fn skipped(&self) -> impl Iterator<Item = (Skip, u32)> + '_ {
        Skip::ALL.into_iter().zip(self.skipped.iter().copied())
    }

    pub fn events(&self) -> &[ClaimEvent] {
        &self.events
    }

    pub fn stats(&self) -> &ClaimStats {
        &self.stats
    }

    /// The room being planned and its next phase, if one is.
    pub fn planning(&self) -> Option<(RoomName, &'static str)> {
        self.active.as_ref().map(|a| {
            let phase = if a.layout.is_some() {
                "validate"
            } else {
                a.run.as_ref().map_or("core", Run::phase)
            };
            (a.room, phase)
        })
    }

    /// Starts the tick: no events yet.
    pub fn begin(&mut self) {
        self.events.clear();
    }

    /// Whether the ranking is due to be rebuilt at `now`, with the atlas at `revision`, `owned` the
    /// rooms the bot owns and `remote_rooms` `remote.max_rooms`: one of them or the claim config
    /// changed since the last score, or `claim.score_ticks` passed.
    pub fn score_due(
        &self,
        now: u32,
        revision: u32,
        owned: &[RoomName],
        remote_rooms: u32,
        cfg: &ClaimConfig,
    ) -> bool {
        cfg.enabled
            && self.scored_at.is_none_or(|t| {
                now >= t.saturating_add(cfg.score_ticks)
                    || revision != self.scored_rev
                    || owned != self.scored_owned.as_slice()
                    || remote_rooms != self.remote_rooms
                    || self.scored_cfg != Some(*cfg)
            })
    }

    /// Whether there is work this tick: a score due, a plan to start, or a plan to step.
    pub fn has_work(
        &self,
        now: u32,
        revision: u32,
        owned: &[RoomName],
        remote_rooms: u32,
        cfg: &ClaimConfig,
    ) -> bool {
        cfg.enabled
            && (self.score_due(now, revision, owned, remote_rooms, cfg)
                || self.active.is_some()
                || self.wanted(cfg).is_some())
    }

    /// Sets the planner's tunables the plans are made with. A change drops every outcome and the
    /// running plan, and makes a score due, so the rooms are planned again.
    pub fn set_params(&mut self, params: Params) {
        if self.params == Some(params) {
            return;
        }
        if self.params.is_some() {
            self.outcomes.clear();
            self.active = None;
            self.scored_at = None;
        }
        self.params = Some(params);
    }

    /// Rebuilds the ranking from `atlas` at `revision` for `homes` (sorted by room), with `owned`
    /// the rooms the bot owns (sorted), at `now`, counting `remote_rooms` (`remote.max_rooms`)
    /// neighbors in the remotes term. The plans' outcomes are applied again, and a plan running for
    /// a room that is no longer a candidate (or whose facts changed) is dropped.
    #[allow(clippy::too_many_arguments)]
    pub fn score(
        &mut self,
        atlas: &Atlas,
        revision: u32,
        homes: &[ClaimHome],
        owned: &[RoomName],
        now: u32,
        remote_rooms: u32,
        cfg: &ClaimConfig,
    ) {
        // A timed score lets a room whose terrain could not be read try again.
        if self
            .scored_at
            .is_none_or(|t| now >= t.saturating_add(cfg.score_ticks))
        {
            self.outcomes.retain(|o| o.fit != Err(NO_TERRAIN));
        }
        self.scored_at = Some(now);
        self.scored_rev = revision;
        self.scored_cfg = Some(*cfg);
        self.scored_owned.clear();
        self.scored_owned.extend_from_slice(owned);
        self.remote_rooms = remote_rooms;
        self.stats.scores = self.stats.scores.saturating_add(1);
        self.candidates.clear();
        self.skipped = [0; Skip::ALL.len()];
        for e in atlas.entries() {
            match candidate(e, atlas, homes, owned, now, remote_rooms, cfg) {
                Ok(mut c) => {
                    let found = Key::of(e).and_then(|k| self.outcome(c.room, k));
                    match found {
                        Some(Err(_)) => self.skip(Skip::Unfit),
                        Some(Ok(fit)) => {
                            apply(&mut c, fit, atlas, owned, now, remote_rooms, cfg);
                            self.candidates.push(c);
                        }
                        None => self.candidates.push(c),
                    }
                }
                Err(why) => self.skip(why),
            }
        }
        // The outcomes of rooms no longer in the atlas are dropped.
        self.outcomes.retain(|o| atlas.get(o.room).is_some());
        let keep = self.active.as_ref().is_some_and(|a| {
            self.candidates.iter().any(|c| c.room == a.room)
                && atlas.get(a.room).and_then(Key::of) == Some(a.key)
        });
        if !keep {
            self.active = None;
        }
        self.rank();
    }

    /// The room whose terrain the next plan needs, when no plan is running: the first unplanned
    /// candidate while fewer than `claim.plan_count` planned ones rank above it.
    pub fn wanted(&self, cfg: &ClaimConfig) -> Option<RoomName> {
        if self.active.is_some() {
            return None;
        }
        let mut planned = 0u32;
        for c in &self.candidates {
            if planned >= cfg.plan_count {
                return None;
            }
            if c.plan.is_some() {
                planned = planned.saturating_add(1);
            } else {
                return Some(c.room);
            }
        }
        None
    }

    /// Starts the plan of `room` on its `terrain` (`None`: it could not be read, and the room is
    /// left out until the next timed score). A room the atlas no longer holds with a controller
    /// tile leaves the ranking, so it is not wanted again.
    pub fn feed_terrain(&mut self, room: RoomName, terrain: Option<TerrainGrid>, atlas: &Atlas) {
        let Some(key) = atlas.get(room).and_then(Key::of) else {
            if let Some(i) = self.candidates.iter().position(|c| c.room == room) {
                self.candidates.remove(i);
                self.skip(Skip::NoController);
                self.rank();
            }
            return;
        };
        let Some(terrain) = terrain else {
            self.events.push(ClaimEvent::NoTerrain { room });
            self.finish(room, key, Err(NO_TERRAIN));
            return;
        };
        let mut sources: Vec<Tile> = key.sources.iter().flatten().copied().collect();
        sources.sort_unstable();
        sources.dedup();
        self.stats.plans = self.stats.plans.saturating_add(1);
        self.active = Some(Box::new(Active {
            room,
            key,
            terrain,
            sources,
            run: None,
            layout: None,
        }));
    }

    /// One piece of the running plan with the planner's `params`: a phase, or the validation of
    /// the finished layout and the outcome. `Step::Done` when no plan is running after it. The
    /// planner's scratch buffers are released once no plan is wanted.
    pub fn step(
        &mut self,
        params: Params,
        atlas: &Atlas,
        owned: &[RoomName],
        cfg: &ClaimConfig,
    ) -> Step {
        self.set_params(params);
        let Some(mut active) = self.active.take() else {
            return Step::Done;
        };
        self.stats.steps = self.stats.steps.saturating_add(1);
        let scratch = self.scratch.get_or_insert_with(Box::default);
        if let Some(layout) = active.layout.take() {
            let report = validate(&active.input(), &layout);
            let fit = if report.valid() {
                fit_of(&layout, &report.metrics, &active.terrain)
            } else {
                Err("invalid plan")
            };
            self.finish(active.room, active.key, fit);
            let now = self.scored_at.unwrap_or(0);
            let rooms = self.remote_rooms;
            if let Ok(fit) = fit
                && let Some(c) = self.candidates.iter_mut().find(|c| c.room == active.room)
            {
                apply(c, fit, atlas, owned, now, rooms, cfg);
            }
            self.rank();
            self.release(cfg);
            return Step::Done;
        }
        let a = &mut *active;
        let input = PlanInput {
            terrain: &a.terrain,
            controller: a.key.controller,
            sources: &a.sources,
            mineral: a.key.mineral,
            spawn: None,
        };
        let run = a.run.get_or_insert_with(|| Run::with(&input, params));
        match run.step(&input, scratch) {
            Ok(Step::More) => {}
            Ok(Step::Done) => {
                if let Some(run) = a.run.take() {
                    a.layout = Some(run.finish());
                }
            }
            Err(e) => {
                let why = match e {
                    PlanError::NoRoom(what) | PlanError::BadInput(what) => what,
                };
                self.finish(active.room, active.key, Err(why));
                self.rank();
                self.release(cfg);
                return Step::Done;
            }
        }
        self.active = Some(active);
        Step::More
    }

    /// Drops the planner's scratch buffers once no plan is wanted.
    fn release(&mut self, cfg: &ClaimConfig) {
        if self.wanted(cfg).is_none() {
            self.scratch = None;
        }
    }

    /// Records a plan's outcome; a failed one leaves the ranking.
    fn finish(&mut self, room: RoomName, key: Key, fit: Result<Fit, &'static str>) {
        match self
            .outcomes
            .binary_search_by_key(&room.packed_repr(), |o| o.room.packed_repr())
        {
            Ok(i) => {
                if let Some(o) = self.outcomes.get_mut(i) {
                    *o = Outcome { room, key, fit };
                }
            }
            Err(i) => self.outcomes.insert(i, Outcome { room, key, fit }),
        }
        match fit {
            Ok(_) => self.stats.fits = self.stats.fits.saturating_add(1),
            Err(why) => {
                if why != NO_TERRAIN {
                    self.events.push(ClaimEvent::Unfit { room, why });
                }
                if let Some(i) = self.candidates.iter().position(|c| c.room == room) {
                    self.candidates.remove(i);
                    self.skip(Skip::Unfit);
                }
            }
        }
    }

    fn outcome(&self, room: RoomName, key: Key) -> Option<Result<Fit, &'static str>> {
        self.outcomes
            .binary_search_by_key(&room.packed_repr(), |o| o.room.packed_repr())
            .ok()
            .and_then(|i| self.outcomes.get(i))
            .filter(|o| o.key == key)
            .map(|o| o.fit)
    }

    fn skip(&mut self, why: Skip) {
        if let Some(n) = self.skipped.get_mut(why.slot()) {
            *n = n.saturating_add(1);
        }
    }

    /// Sorts the ranking (value, then room) and reports a change of the best.
    fn rank(&mut self) {
        self.candidates
            .sort_unstable_by_key(|c| (Reverse(c.value()), c.room));
        let best = self.best().map(|c| (c.room, c.value()));
        if best.map(|b| b.0) != self.best {
            self.best = best.map(|b| b.0);
            self.events.push(ClaimEvent::Best {
                room: best.map(|b| b.0),
                value: best.map_or(0, |b| b.1),
            });
        }
    }
}

/// What one source yields a tick in an owned room (V184). Unit: me/t.
///
/// Uses: `SOURCE_ENERGY_CAPACITY`, `ENERGY_REGEN_TIME`.
pub fn source_yield() -> i64 {
    i64::from(SOURCE_ENERGY_CAPACITY).saturating_mul(MILLI) / i64::from(ENERGY_REGEN_TIME.max(1))
}

/// What hauling `flow` me/t over a way of path cost `cost` costs a tick: the haulers' bodies, renewed
/// every `CREEP_LIFE_TIME`. On roads a hauler with one MOVE part to two CARRY parts steps a tile a
/// tick loaded (V54: one fatigue a loaded CARRY part on a road, `MOVE_POWER` 2 a MOVE part), so it
/// needs `flow × 2 × cost / CARRY_CAPACITY` CARRY parts for the round trip, each with half a MOVE
/// part. A lower bound, as [`crate::economy::sizing::walk_ticks`] is. Unit: me/t.
///
/// Uses: `CARRY_CAPACITY`, `CREEP_LIFE_TIME`, `BODYPART_COST` (CARRY, MOVE).
pub fn haul_cost(flow: i64, cost: u32) -> i64 {
    let pair = i64::from(Part::Carry.cost())
        .saturating_mul(2)
        .saturating_add(i64::from(Part::Move.cost()));
    flow.saturating_mul(i64::from(cost)).saturating_mul(pair)
        / i64::from(CARRY_CAPACITY)
            .saturating_mul(i64::from(CREEP_LIFE_TIME))
            .max(1)
}

/// What repairing `ramparts` ramparts' decay costs a tick: `rampart_decay` hits each a creep life,
/// at `REPAIR_COST` an energy a hit. Unit: me/t.
///
/// Uses: `RAMPART_DECAY_AMOUNT`, `RAMPART_DECAY_TIME`, `REPAIR_COST`, `CREEP_LIFE_TIME`.
pub fn rampart_cost(ramparts: u32) -> i64 {
    let hits = rampart_decay().saturating_mul(u64::from(ramparts));
    i64::from(repair_energy(hits)).saturating_mul(MILLI) / i64::from(CREEP_LIFE_TIME.max(1))
}

/// The penalty for a room another player owns at `level`, `distance` rooms from the candidate:
/// `claim.foreign_cost × level / MAX_LEVEL × (threat_range + 1 − distance)`, and 0 beyond the range.
/// Unit: me/t.
///
/// Uses: [`MAX_LEVEL`] (the controller's highest level).
pub fn foreign_penalty(level: u8, distance: u32, cfg: &ClaimConfig) -> i64 {
    if distance == 0 || distance > cfg.threat_range {
        return 0;
    }
    let near = i64::from(cfg.threat_range.saturating_add(1).saturating_sub(distance));
    i64::from(cfg.foreign_cost)
        .saturating_mul(i64::from(level.min(MAX_LEVEL)))
        .saturating_mul(near)
        / i64::from(MAX_LEVEL)
}

/// Whether two rooms share a side (an exit edge, if neither side is all wall).
fn across(a: RoomName, b: RoomName) -> bool {
    a.x_coord().abs_diff(b.x_coord()) + a.y_coord().abs_diff(b.y_coord()) == 1
}

/// A source's share in the remotes term when room `n` could be mined from the candidate (module
/// docs), else 0. Unit: me/t for all of its sources.
fn remote_value(
    n: RoomName,
    atlas: &Atlas,
    owned: &[RoomName],
    now: u32,
    cfg: &ClaimConfig,
) -> i64 {
    let Some(e) = atlas.get(n) else {
        return 0;
    };
    let free = e.known
        && !e.unreachable
        && is_remote_room(n)
        && e.controller == Controller::Neutral
        && !reserved_by_other(e.reserved, now)
        && e.towers == 0
        && e.spawns == 0
        && e.core.is_none()
        && !owned.iter().any(|&o| o == n || across(o, n));
    if !free {
        return 0;
    }
    let sources = i64::try_from(e.source_count()).unwrap_or(0);
    // A remote room is reserved, so its sources refill to `SOURCE_ENERGY_CAPACITY` (V184).
    sources
        .saturating_mul(source_yield())
        .saturating_mul(i64::from(cfg.remote_pct))
        / 100
}

const fn reserved_by_other(r: Reserved, now: u32) -> bool {
    matches!(r, Reserved::Other { until } if until > now)
}

/// The remotes term of `room` with only the sides in `open` counted: the best `rooms` neighbors.
fn remotes(
    room: RoomName,
    open: [bool; 4],
    atlas: &Atlas,
    owned: &[RoomName],
    now: u32,
    rooms: u32,
    cfg: &ClaimConfig,
) -> i64 {
    let mut values = [0i64; 4];
    for ((slot, side), o) in values.iter_mut().zip(Side::ALL).zip(open) {
        if o && let Some(n) = room.checked_add(side.room_offset()) {
            *slot = remote_value(n, atlas, owned, now, cfg);
        }
    }
    values.sort_unstable_by_key(|&v| Reverse(v));
    values
        .into_iter()
        .take(usize::try_from(rooms).unwrap_or(usize::MAX))
        .fold(0i64, i64::saturating_add)
}

/// The foreign term of `room`: every room another player owns within `claim.threat_range`.
fn foreign(room: RoomName, atlas: &Atlas, cfg: &ClaimConfig) -> i64 {
    let r = i32::try_from(cfg.threat_range).unwrap_or(0);
    let mut sum = 0i64;
    for dx in -r..=r {
        for dy in -r..=r {
            if dx == 0 && dy == 0 {
                continue;
            }
            let Some(n) = room.checked_add((dx, dy)) else {
                continue;
            };
            let Some(e) = atlas.get(n) else {
                continue;
            };
            if let (true, Controller::Foreign { level }) = (e.known, e.controller) {
                sum = sum.saturating_add(foreign_penalty(level, room_distance(room, n), cfg));
            }
        }
    }
    sum
}

/// `e` as a candidate before its plan check, or why it is not one (module docs).
fn candidate(
    e: &RoomIntel,
    atlas: &Atlas,
    homes: &[ClaimHome],
    owned: &[RoomName],
    now: u32,
    rooms: u32,
    cfg: &ClaimConfig,
) -> Result<Candidate, Skip> {
    if !e.known || e.unreachable {
        return Err(Skip::Unknown);
    }
    // A room claimed since its last read is still neutral in the atlas.
    if owned.contains(&e.room) {
        return Err(Skip::Owned);
    }
    match e.controller {
        Controller::Neutral => {}
        Controller::Absent => return Err(Skip::NoController),
        Controller::Mine { .. } | Controller::Foreign { .. } => return Err(Skip::Owned),
    }
    if e.controller_tile.is_none() {
        return Err(Skip::NoController);
    }
    if reserved_by_other(e.reserved, now) {
        return Err(Skip::Reserved);
    }
    if e.core.is_some() || e.towers > 0 || e.spawns > 0 {
        return Err(Skip::Hostile);
    }
    let sources = e.source_count();
    if sources == 0 || u32::try_from(sources).unwrap_or(u32::MAX) < cfg.min_sources {
        return Err(Skip::Sources);
    }
    let in_reach = homes
        .iter()
        .map(|h| (room_distance(h.room, e.room), h))
        .filter(|&(d, _)| d <= cfg.max_distance);
    let mut any = false;
    let mut nearest: Option<(u32, &ClaimHome)> = None;
    for (d, h) in in_reach {
        any = true;
        if h.status == e.status && nearest.is_none_or(|(b, bh)| (d, h.room) < (b, bh.room)) {
            nearest = Some((d, h));
        }
    }
    let novice_full = e.status == RoomStatus::Novice
        && u32::try_from(owned.len()).unwrap_or(u32::MAX) >= GCL_NOVICE;
    let (distance, home) = match nearest {
        Some(n) if e.status != RoomStatus::Closed && !novice_full => n,
        _ if any => return Err(Skip::Status),
        _ => return Err(Skip::Far),
    };
    let n = i64::try_from(sources).unwrap_or(0);
    let terms = Terms {
        sources: n.saturating_mul(source_yield()),
        remotes: remotes(e.room, [true; 4], atlas, owned, now, rooms, cfg),
        distance: i64::from(cfg.distance_cost)
            .saturating_mul(i64::from(distance))
            .saturating_neg(),
        foreign: foreign(e.room, atlas, cfg).saturating_neg(),
        walks: 0,
        ramparts: 0,
    };
    Ok(Candidate {
        room: e.room,
        home: home.room,
        distance,
        sources: u8::try_from(sources).unwrap_or(u8::MAX),
        terms,
        plan: None,
    })
}

/// Adds a plan's terms to `c`: the walks, the ramparts, and the remotes with only its open sides,
/// judged at `now` (the last score's tick).
///
/// Uses: [`source_yield`], [`haul_cost`], [`rampart_cost`].
fn apply(
    c: &mut Candidate,
    fit: Fit,
    atlas: &Atlas,
    owned: &[RoomName],
    now: u32,
    rooms: u32,
    cfg: &ClaimConfig,
) {
    let one = source_yield();
    let all = one.saturating_mul(i64::from(c.sources));
    c.terms.walks = haul_cost(one, fit.source_ways)
        .saturating_add(haul_cost(all, fit.controller_way))
        .saturating_neg();
    c.terms.ramparts = rampart_cost(fit.ramparts).saturating_neg();
    c.terms.remotes = remotes(c.room, fit.open, atlas, owned, now, rooms, cfg);
    c.plan = Some(fit);
}

/// A valid layout's fit, from its validator's `metrics`: its first spawn, ramparts, ways, and the
/// room's open sides.
fn fit_of(layout: &Layout, metrics: &Metrics, terrain: &TerrainGrid) -> Result<Fit, &'static str> {
    let spawn = layout
        .of(StructureType::Spawn)
        .filter(|i| i.level == 1)
        .map(|i| i.tile)
        .min()
        .ok_or("no first spawn")?;
    Ok(Fit {
        spawn,
        ramparts: layout.count(StructureType::Rampart, MAX_LEVEL),
        source_ways: metrics
            .to_sources
            .iter()
            .copied()
            .fold(0u32, u32::saturating_add),
        controller_way: metrics.to_controller.unwrap_or(0),
        open: open_sides(terrain),
    })
}

/// Which sides of the room have an edge tile that is not a wall (`Side::ALL`'s order).
///
/// Uses: `ROOM_SIZE`.
pub fn open_sides(terrain: &TerrainGrid) -> [bool; 4] {
    let last = ROOM_SIZE.saturating_sub(1);
    let open = |x: u8, y: u8| Tile::new(x, y).is_some_and(|t| terrain.kind(t) != TerrainKind::Wall);
    Side::ALL.map(|side| {
        (0..ROOM_SIZE).any(|i| match side {
            Side::Top => open(i, 0),
            Side::Right => open(last, i),
            Side::Bottom => open(i, last),
            Side::Left => open(0, i),
        })
    })
}

#[cfg(test)]
#[path = "claim_tests.rs"]
mod tests;
