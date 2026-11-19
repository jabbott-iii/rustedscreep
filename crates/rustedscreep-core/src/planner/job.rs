//! The planner's job (ROADMAP P4.3, ADR-007): each owned room with a spawn is planned once a global
//! by [`flood`], one phase a step, so the Optional tier slices it across ticks
//! (`TickCpu::run_steps`), then judged by [`validate`]. The plan stays in the heap only: it is
//! deterministic in the room's terrain, controller, sources, mineral, standing spawns and sites and
//! the standing containers and links it adopts (ADR-026), so a global reset rebuilds it from the
//! same facts for the cost of one more run (more with several spawns, below).
//!
//! The anchor: the plan's input spawn. A room with one spawn and no spawn site has one candidate.
//! Otherwise each standing spawn and own spawn site is tried, from the highest tile down (a spawn
//! lost and sited again still anchors the plan the room was built from), and of the valid plans
//! the room keeps the one whose spawn tiles hold every standing spawn and, of those, the one that
//! holds the most of what stands in the room (its own structures and sites and its containers on
//! tiles the plan gives their type); the first tried on a tie. A plan that holds all of it ends
//! the search at once. So a global reset at RCL7 or later finds the plan the room was built from
//! while its anchor stands or is sited, not one anchored on a later spawn that happens to hold the
//! first (ADR-026). The order is for CPU only: the room's first spawn sorts above the plan's later
//! spawns in 223 of the corpus's 284 room-levels at RCL7 and RCL8 (a standing spawn off the core,
//! whose core spawn arm is a later spawn, is the usual exception), so a room that holds nothing off
//! its plan is usually planned in one run. Otherwise every anchor runs in full: up to
//! `allowed(Spawn, MAX_LEVEL)` runs of `flood` and their validations a reset. A ready plan is kept
//! while every standing spawn is on one of its spawn tiles or was standing when it was made (a
//! plan no anchor fits fully is kept too, since a new run would make it again); a spawn that
//! stands off its tiles and is new since plans the room again. A room with no valid plan is marked
//! failed for the rest of the global; `economy::placement` then runs the interim placement there.
//! A room with no spawn waits, and reads nothing but its spawn count until one stands.
//!
//! The record (ADR-027, [`record`]): per owned room, the tile its plan was anchored on and the
//! containers and links it adopted, kept in a segment across global resets. No room is planned
//! until the record's load has finished (it comes in the Standard tier, before the Optional
//! planner runs, on the global's first or second tick); a room's entry is tried first, even with
//! nothing standing on its anchor, with its adopted tiles standing, and its plan is kept without
//! trying the others when it validates and its spawn tiles hold every standing spawn. A missing or
//! refused record leaves the rule above; an entry whose plan fails those checks stays a
//! candidate of that rule (tried first, so it wins a tie). An entry is set when a room's plan is
//! ready and dropped when the room is no longer owned (also while the planner is switched off,
//! [`Planner::forget_unowned`]); either change marks the record for writing
//! ([`Planner::take_dirty`]). A record this build refuses is written again at once; a segment
//! whose text never arrived is left as it is, and this global writes nothing.
//!
//! A run owns a copy of its facts (the terrain is 2500 bytes), so nothing it borrows has to live
//! across ticks. Rooms are planned one at a time, in name order.

use rustedscreep_consts::{RoomName, StructureType};

use super::flood::{self, Params};
use super::layout::{Layout, PlanError, PlanInput};
use super::record::{self, Entry, Record, RecordError};
use super::scratch::Scratch;
use super::validate::validate;
use crate::movement::{Movement, TerrainGrid, Tile};
use crate::persist::{DecodeError, Frame, LoadError, kind};
use crate::scheduler::Step;
use crate::snapshot::{Controller, ObjectKind, Snapshot};

/// A room's facts as the planner reads them, owned.
#[derive(Clone)]
pub struct Facts {
    pub terrain: TerrainGrid,
    pub controller: Tile,
    /// In tile order (the snapshot's and the corpus's (x, y) order).
    pub sources: Vec<Tile>,
    pub mineral: Option<Tile>,
    /// The standing spawns, in tile order.
    pub spawns: Vec<Tile>,
    /// The tiles a plan may be anchored on: the standing spawns, the own spawn sites and the
    /// record's anchor even with nothing on it ([`Facts::with_record`]), in tile order (the run
    /// tries the record's first, then the rest from the last).
    pub anchors: Vec<Tile>,
    /// The standing containers and links and the own sites of both (ADR-026: the economy counts a
    /// site as its store or link already), in kind then tile order.
    pub standing: Vec<(StructureType, Tile)>,
    /// What stands in the room that a plan may hold: the own structures but the controller, the
    /// containers and the own sites, in kind then tile order (the anchor rule's measure).
    pub own: Vec<(StructureType, Tile)>,
    /// The anchor the room's record names (ADR-027): tried first, and its plan kept when it
    /// validates and holds every standing spawn.
    pub prior: Option<Tile>,
}

impl Facts {
    /// The input for the plan anchored on `spawn`.
    pub fn input(&self, spawn: Option<Tile>) -> PlanInput<'_> {
        PlanInput {
            terrain: &self.terrain,
            controller: self.controller,
            sources: &self.sources,
            mineral: self.mineral,
            spawn,
        }
    }

    /// The facts of owned room `name` this tick, or what is missing: its terrain (not read yet) or
    /// a spawn (none yet; both are waited for), or a controller or a source (nothing to plan).
    pub fn read(snap: &Snapshot, movement: &Movement, name: RoomName) -> Result<Self, Missing> {
        let st = &snap.structures;
        let mut controller = None;
        let mut spawns = Vec::new();
        let mut anchors = Vec::new();
        let mut standing = Vec::new();
        let mut own = Vec::new();
        for (&kind, &pos) in st.kinds().iter().zip(st.positions()) {
            if pos.room_name() != name {
                continue;
            }
            let t = Tile::of(pos);
            match kind {
                StructureType::Controller => {
                    controller = Some(t);
                    continue;
                }
                StructureType::Spawn => {
                    spawns.push(t);
                    anchors.push(t);
                }
                StructureType::Link => standing.push((kind, t)),
                _ => {}
            }
            own.push((kind, t));
        }
        let ob = &snap.objects;
        let mut sources = Vec::new();
        for (&kind, &pos) in ob.kinds().iter().zip(ob.positions()) {
            if pos.room_name() != name {
                continue;
            }
            let t = Tile::of(pos);
            let held = match kind {
                ObjectKind::Source => {
                    sources.push(t);
                    continue;
                }
                ObjectKind::Container => StructureType::Container,
                ObjectKind::Site(k) => k,
                _ => continue,
            };
            match held {
                StructureType::Container | StructureType::Link => standing.push((held, t)),
                StructureType::Spawn => anchors.push(t),
                _ => {}
            }
            own.push((held, t));
        }
        sources.sort_unstable();
        sources.dedup();
        spawns.sort_unstable();
        anchors.sort_unstable();
        anchors.dedup();
        let by_kind = |&(k, t): &(StructureType, Tile)| (super::layout::kind_rank(k), t.index());
        standing.sort_unstable_by_key(by_kind);
        standing.dedup();
        own.sort_unstable_by_key(by_kind);
        own.dedup();
        // The statics are the sources and minerals read with the terrain (a room has one mineral).
        let mineral = movement
            .room_statics(name)
            .filter(|t| !sources.contains(t))
            .min();
        let controller = controller.ok_or(Missing::Unplannable("no controller"))?;
        if sources.is_empty() {
            return Err(Missing::Unplannable("no source"));
        }
        if spawns.is_empty() {
            return Err(Missing::Spawn);
        }
        // Copied last, so a room that is waiting copies nothing.
        let terrain = movement.room_terrain(name).ok_or(Missing::Terrain)?.clone();
        Ok(Self {
            terrain,
            controller,
            sources,
            mineral,
            spawns,
            anchors,
            standing,
            own,
            prior: None,
        })
    }

    /// These facts with room's record `entry` (ADR-027): its anchor tried first (added to the
    /// anchors if nothing stands on it) and its adopted containers and links standing.
    pub fn with_record(mut self, entry: Option<&Entry>) -> Self {
        let Some(e) = entry else {
            return self;
        };
        self.prior = Some(e.anchor);
        if let Err(i) = self.anchors.binary_search(&e.anchor) {
            self.anchors.insert(i, e.anchor);
        }
        self.standing.extend(e.adopted.iter().copied());
        self.standing
            .sort_unstable_by_key(|&(k, t)| (super::layout::kind_rank(k), t.index()));
        self.standing.dedup();
        self
    }
}

/// Why [`Facts::read`] has no facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// The room's terrain is not read yet (movement reads it).
    Terrain,
    /// No spawn stands yet (the planner plans from a standing spawn; a fresh room is P8's).
    Spawn,
    /// The room can never be planned this way.
    Unplannable(&'static str),
}

/// A finished, valid plan and what it was made from.
pub struct Plan {
    pub layout: Layout,
    pub facts: Facts,
    /// The tile the plan was anchored on: a standing spawn, an own spawn site, or the record's
    /// anchor (ADR-027).
    pub anchor: Tile,
    /// `Game.time` it was ready, and the steps it took (validation included).
    pub ready_at: u32,
    pub steps: u32,
}

/// How well a valid plan fits what stands ([`Running::advance`]): whether its spawn tiles hold
/// every standing spawn, then how many of [`Facts::own`] it holds.
type Fit = (bool, usize);

struct Running {
    facts: Facts,
    /// The anchors in the order they are tried: the record's first, then the rest from the
    /// highest tile down.
    order: Vec<Tile>,
    /// How many of `order` were tried.
    spawn: usize,
    run: Option<flood::Run>,
    /// The finished layout, waiting for its validation step.
    done: Option<Layout>,
    /// The best valid plan so far, with its anchor and fit.
    best: Option<(Layout, Tile, Fit)>,
    steps: u32,
}

enum State {
    Run(Box<Running>),
    Ready(Box<Plan>),
    Failed(&'static str),
}

struct Room {
    name: RoomName,
    state: State,
    /// Seen as owned with a spawn this tick.
    seen: bool,
}

/// What a room's planning has come to, for `economy::placement` and the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status<'a> {
    /// Not planned: the planner is off, or the room has no spawn or no terrain yet.
    None,
    /// Planning (the phase the next step does).
    Planning(&'static str),
    Ready(&'a Plan),
    Failed(&'static str),
}

impl PartialEq for Plan {
    fn eq(&self, other: &Self) -> bool {
        self.layout == other.layout && self.anchor == other.anchor
    }
}

impl Eq for Plan {}

impl core::fmt::Debug for Plan {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Plan")
            .field("items", &self.layout.items.len())
            .field("anchor", &self.anchor)
            .field("ready_at", &self.ready_at)
            .finish()
    }
}

/// Counters since the global started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub started: u32,
    pub ready: u32,
    pub failed: u32,
    pub steps: u32,
}

/// The planner's state for one global.
#[derive(Default)]
pub struct Planner {
    rooms: Vec<Room>,
    scratch: Scratch,
    totals: Totals,
    time: u32,
    /// The record (ADR-027): `None` until its load finished, and no room is planned before.
    record: Option<Record>,
    /// How the record's load ended, for the console.
    loaded: Option<Loaded>,
    /// The record changed since it was last written.
    dirty: bool,
    /// The record may be written: false for a global whose load never got the segment's text
    /// (`LoadError::Unavailable`), which leaves the segment as it is for the next global.
    readonly: bool,
}

/// How the record's load ended (ADR-027).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// A record of `rooms` entries, written on tick `written`.
    Record { rooms: usize, written: u32 },
    /// The segment was empty (never written, or cleared).
    Empty,
    /// No segment is configured for it.
    Off,
    /// The load failed, or the record was refused: plans come from what stands.
    Refused(Refusal),
}

/// Why the record was not used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Load(LoadError),
    Record(RecordError),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Load(e) => write!(f, "{e}"),
            Self::Record(e) => write!(f, "{e}"),
        }
    }
}

impl Planner {
    /// Starts a tick: the owned rooms with a spawn and read terrain, by name. A new one starts a
    /// run; a ready one where a spawn stands off its spawn tiles that was not standing when it was
    /// made starts again (a lost anchor keeps the plan); rooms no longer owned are dropped.
    pub fn begin(&mut self, snap: &Snapshot, movement: &Movement) {
        self.time = snap.tick.time;
        self.forget_unowned(snap);
        let Some(record) = self.record.as_mut() else {
            // Nothing is planned before the record's load finished.
            return;
        };
        let rooms = &snap.rooms;
        for r in &mut self.rooms {
            r.seen = false;
        }
        for row in rooms.owned() {
            let Some(&name) = rooms.names().get(row) else {
                continue;
            };
            if !matches!(rooms.controllers().get(row), Some(Controller::Mine { .. })) {
                continue;
            }
            let pos = self
                .rooms
                .binary_search_by_key(&name.packed_repr(), |r| r.name.packed_repr());
            match pos {
                Ok(i) => {
                    let Some(r) = self.rooms.get_mut(i) else {
                        continue;
                    };
                    r.seen = true;
                    if let State::Ready(plan) = &r.state
                        && new_spawn_off_plan(snap, name, plan)
                        && let Ok(facts) = Facts::read(snap, movement, name)
                    {
                        let facts = facts.with_record(record.get(name));
                        r.state = State::Run(Box::new(Running::new(facts)));
                        self.totals.started = self.totals.started.saturating_add(1);
                    }
                }
                Err(i) => {
                    if !any_spawn(snap, name) || movement.room_terrain(name).is_none() {
                        continue;
                    }
                    let state = match Facts::read(snap, movement, name) {
                        Ok(facts) => {
                            self.totals.started = self.totals.started.saturating_add(1);
                            let facts = facts.with_record(record.get(name));
                            State::Run(Box::new(Running::new(facts)))
                        }
                        Err(Missing::Unplannable(why)) => {
                            self.totals.failed = self.totals.failed.saturating_add(1);
                            State::Failed(why)
                        }
                        Err(Missing::Terrain | Missing::Spawn) => continue,
                    };
                    self.rooms.insert(
                        i,
                        Room {
                            name,
                            state,
                            seen: true,
                        },
                    );
                }
            }
        }
        self.rooms.retain(|r| r.seen);
    }

    /// Drops the record's entries of rooms not owned this tick (marking it dirty), so a room lost
    /// and claimed again starts from what stands. [`Planner::begin`] does it; the glue calls it
    /// alone while the planner is switched off, so the record never outlives a claim.
    pub fn forget_unowned(&mut self, snap: &Snapshot) {
        let rooms = &snap.rooms;
        let owned = |name: RoomName| {
            rooms.owned().any(|row| {
                rooms.names().get(row) == Some(&name)
                    && matches!(rooms.controllers().get(row), Some(Controller::Mine { .. }))
            })
        };
        if let Some(record) = self.record.as_mut()
            && record.retain(owned)
        {
            self.dirty = true;
        }
    }

    /// Whether some room is planning (the job has a step to run).
    pub fn busy(&self) -> bool {
        self.rooms.iter().any(|r| matches!(r.state, State::Run(_)))
    }

    /// The phase the next step does, for the profiler's and the console's timings.
    pub fn next_phase(&self) -> Option<&'static str> {
        self.rooms.iter().find_map(|r| match &r.state {
            State::Run(run) => Some(run.phase()),
            _ => None,
        })
    }

    /// One step of the first room that is planning (a phase of its run, or its validation).
    /// `Step::Done` once no room is planning.
    pub fn step(&mut self, params: Params) -> Step {
        let time = self.time;
        let Self {
            rooms,
            scratch,
            totals,
            record,
            dirty,
            ..
        } = self;
        let Some(room) = rooms.iter_mut().find(|r| matches!(r.state, State::Run(_))) else {
            return Step::Done;
        };
        totals.steps = totals.steps.saturating_add(1);
        let outcome = match &mut room.state {
            State::Run(running) => {
                running.steps = running.steps.saturating_add(1);
                running.advance(params, scratch)
            }
            _ => Advance::More,
        };
        match outcome {
            Advance::More => {}
            Advance::Ready(layout, anchor) => {
                if let State::Run(running) =
                    std::mem::replace(&mut room.state, State::Failed("taken"))
                {
                    let Running { facts, steps, .. } = *running;
                    let plan = Plan {
                        layout,
                        facts,
                        anchor,
                        ready_at: time,
                        steps,
                    };
                    if let Some(rec) = record.as_mut()
                        && rec.set(plan.entry(room.name))
                    {
                        *dirty = true;
                    }
                    room.state = State::Ready(Box::new(plan));
                    totals.ready = totals.ready.saturating_add(1);
                }
            }
            Advance::Failed(why) => {
                room.state = State::Failed(why);
                totals.failed = totals.failed.saturating_add(1);
            }
        }
        if self.busy() { Step::More } else { Step::Done }
    }

    /// Room `name`'s planning.
    pub fn status(&self, name: RoomName) -> Status<'_> {
        self.rooms
            .iter()
            .find(|r| r.name == name)
            .map_or(Status::None, |r| match &r.state {
                State::Run(run) => Status::Planning(run.phase()),
                State::Ready(plan) => Status::Ready(plan),
                State::Failed(why) => Status::Failed(why),
            })
    }

    /// Every room's planning, by name.
    pub fn rooms(&self) -> impl Iterator<Item = (RoomName, Status<'_>)> + '_ {
        self.rooms.iter().map(|r| (r.name, self.status(r.name)))
    }

    pub fn totals(&self) -> &Totals {
        &self.totals
    }

    /// The record's load finished: its frame, or why there is none (ADR-027). Returns whether
    /// the segment held a usable record or none at all (an empty segment), for the segment
    /// manager's counters. Ignored once a record is in use. A segment that held a record this
    /// build refuses is written again at once (with what this global plans); one whose text never
    /// arrived is left as it is for the next global, and nothing is written in this one.
    pub fn loaded(&mut self, frame: Result<Frame<'_>, LoadError>) -> bool {
        if self.record.is_some() {
            return true;
        }
        let frame = frame.and_then(|f| {
            f.header
                .expect(kind::PLANNER, record::SCHEMA)
                .map_err(LoadError::Decode)
                .map(|()| f)
        });
        let (rec, loaded, ok) = match frame {
            Ok(f) => match Record::decode(f.payload) {
                Ok(r) => {
                    let rooms = r.entries().len();
                    let written = f.header.written;
                    (r, Loaded::Record { rooms, written }, true)
                }
                Err(e) => (
                    Record::default(),
                    Loaded::Refused(Refusal::Record(e)),
                    false,
                ),
            },
            Err(LoadError::Decode(DecodeError::Empty)) => (Record::default(), Loaded::Empty, true),
            Err(e) => (Record::default(), Loaded::Refused(Refusal::Load(e)), false),
        };
        match loaded {
            Loaded::Refused(Refusal::Load(LoadError::Unavailable { .. })) => self.readonly = true,
            Loaded::Refused(_) => self.dirty = true,
            _ => {}
        }
        self.record = Some(rec);
        self.loaded = Some(loaded);
        ok
    }

    /// No segment keeps the record: plan from what stands at once.
    pub fn no_record(&mut self) {
        if self.record.is_none() {
            self.record = Some(Record::default());
            self.loaded = Some(Loaded::Off);
        }
    }

    /// How the record's load ended (`None` while it is still loading).
    pub fn load_state(&self) -> Option<Loaded> {
        self.loaded
    }

    /// The record in use (`None` while it is still loading).
    pub fn record(&self) -> Option<&Record> {
        self.record.as_ref()
    }

    /// Whether the record changed since the last call (the glue marks its segment dirty then);
    /// never in a global whose load did not get the segment's text.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty) && !self.readonly
    }

    /// Whether this global writes the record (module docs).
    pub fn writes_record(&self) -> bool {
        !self.readonly
    }

    /// The record's payload ([`record`]) into `out`.
    pub fn payload(&self, out: &mut Vec<u8>) {
        if let Some(r) = &self.record {
            r.encode(out);
        }
    }
}

impl Plan {
    /// The record entry of room `name`'s plan: its anchor and the containers and links of its
    /// facts' standing ones (built, sited or from the record) that it holds on their tiles.
    pub fn entry(&self, name: RoomName) -> Entry {
        let adopted = self
            .facts
            .standing
            .iter()
            .copied()
            .filter(|&(k, t)| self.layout.items.iter().any(|i| i.kind == k && i.tile == t))
            .collect();
        Entry {
            room: name,
            anchor: self.anchor,
            adopted,
        }
    }
}

/// Every step of a room's run at once, as the job spreads them over ticks: the plan the job would
/// make from `facts` (its anchor choice included, module docs) and its anchor, or why there is
/// none. For tests and tools; the tick path runs [`Planner::step`].
pub fn plan_whole(
    facts: Facts,
    params: Params,
    s: &mut Scratch,
) -> Result<(Layout, Tile), &'static str> {
    let mut running = Running::new(facts);
    loop {
        match running.advance(params, s) {
            Advance::More => {}
            Advance::Ready(layout, anchor) => return Ok((layout, anchor)),
            Advance::Failed(why) => return Err(why),
        }
    }
}

/// Whether a spawn stands in `name`.
fn any_spawn(snap: &Snapshot, name: RoomName) -> bool {
    let st = &snap.structures;
    st.kinds()
        .iter()
        .zip(st.positions())
        .any(|(&k, &p)| k == StructureType::Spawn && p.room_name() == name)
}

/// Whether a spawn stands in `name` on a tile `plan` holds no spawn on and was not standing when
/// the plan was made (a plan chosen with a spawn off its tiles, the best no anchor fits fully, is
/// what a new run would make again from the same facts).
fn new_spawn_off_plan(snap: &Snapshot, name: RoomName, plan: &Plan) -> bool {
    let st = &snap.structures;
    st.kinds()
        .iter()
        .zip(st.positions())
        .filter(|&(&k, &p)| k == StructureType::Spawn && p.room_name() == name)
        .any(|(_, &p)| {
            let t = Tile::of(p);
            !plan.layout.of(StructureType::Spawn).any(|i| i.tile == t)
                && plan.facts.spawns.binary_search(&t).is_err()
        })
}

/// How many of `own` stand on a tile `layout` gives their type ([`Fit`]).
fn holds(layout: &Layout, own: &[(StructureType, Tile)]) -> usize {
    let mut planned: Vec<(usize, usize)> = layout
        .items
        .iter()
        .map(|i| (super::layout::kind_rank(i.kind), i.tile.index()))
        .collect();
    planned.sort_unstable();
    own.iter()
        .filter(|&&(k, t)| {
            planned
                .binary_search(&(super::layout::kind_rank(k), t.index()))
                .is_ok()
        })
        .count()
}

enum Advance {
    More,
    Ready(Layout, Tile),
    Failed(&'static str),
}

fn why(e: PlanError) -> &'static str {
    match e {
        PlanError::NoRoom(what) | PlanError::BadInput(what) => what,
    }
}

impl Running {
    fn new(facts: Facts) -> Self {
        let prior = facts.prior.filter(|t| facts.anchors.contains(t));
        let order = prior
            .into_iter()
            .chain(
                facts
                    .anchors
                    .iter()
                    .rev()
                    .copied()
                    .filter(|&t| Some(t) != prior),
            )
            .collect();
        Self {
            facts,
            order,
            spawn: 0,
            run: None,
            done: None,
            best: None,
            steps: 0,
        }
    }

    fn phase(&self) -> &'static str {
        if self.done.is_some() {
            "validate"
        } else {
            self.run.as_ref().map_or("core", flood::Run::phase)
        }
    }

    /// The next piece of work: a phase of the run for the anchor being tried, or the validation
    /// of its finished layout (and its fit, module docs); then the next anchor, or the outcome.
    fn advance(&mut self, params: Params, s: &mut Scratch) -> Advance {
        let anchor = self.order.get(self.spawn).copied();
        let Some(anchor) = anchor else {
            return match self.best.take() {
                Some((layout, at, _)) => Advance::Ready(layout, at),
                None => Advance::Failed("no valid plan"),
            };
        };
        if let Some(layout) = self.done.take() {
            let input = self.facts.input(Some(anchor));
            let valid = validate(&input, &layout).valid();
            if valid {
                let holds_all = self.facts.spawns.iter().all(|&sp| {
                    sp == anchor || layout.of(StructureType::Spawn).any(|i| i.tile == sp)
                });
                let fit = (holds_all, holds(&layout, &self.facts.own));
                // Nothing tried later can fit better, and a tie goes to the anchor tried first;
                // the record's anchor is kept once its plan holds every standing spawn (ADR-027).
                if fit == (true, self.facts.own.len())
                    || (holds_all && Some(anchor) == self.facts.prior)
                {
                    return Advance::Ready(layout, anchor);
                }
                if self.best.as_ref().is_none_or(|&(_, _, b)| fit > b) {
                    self.best = Some((layout, anchor, fit));
                }
            }
            return self.next_spawn(if valid {
                "inconsistent"
            } else {
                "invalid plan"
            });
        }
        let facts = &self.facts;
        let input = facts.input(Some(anchor));
        let run = self.run.get_or_insert_with(|| {
            let mut run = flood::Run::with(&input, params);
            run.adopt(&facts.standing);
            run
        });
        match run.step(&input, s) {
            Ok(Step::More) => Advance::More,
            Ok(Step::Done) => {
                if let Some(run) = self.run.take() {
                    self.done = Some(run.finish());
                }
                Advance::More
            }
            Err(e) => self.next_spawn(why(e)),
        }
    }

    fn next_spawn(&mut self, last: &'static str) -> Advance {
        self.run = None;
        self.done = None;
        self.spawn += 1;
        if self.spawn < self.order.len() {
            Advance::More
        } else {
            match self.best.take() {
                Some((layout, at, _)) => Advance::Ready(layout, at),
                None => Advance::Failed(last),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MOVEMENT_MMO, PERSIST_MMO, PLANNER_MMO};
    use crate::persist::frame;
    use crate::planner::layout::{MAX_LEVEL, kind_rank};
    use crate::planner::rules::{allowed, earliest_level};
    use crate::snapshot::{
        CreepRow, ObjectKey, ObjectRow, ObjectStatic, StructureStatic, TickContext,
    };
    use rustedscreep_consts::{
        CONTAINER_HITS, CPU_BUCKET_MAX, CPU_TICK_LIMIT_MAX, Position, ROOM_AREA, ROOM_SIZE,
        RoomCoordinate, SOURCE_ENERGY_CAPACITY, Terrain,
    };

    fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).expect("in room")
    }

    fn room(name: &str) -> RoomName {
        name.parse().expect("room name")
    }

    /// The open room's controller.
    fn controller_tile() -> Tile {
        tile(25, 41)
    }

    /// The open room's sources, in tile order.
    fn source_tiles() -> Vec<Tile> {
        vec![tile(10, 10), tile(40, 12)]
    }

    /// The open room's mineral.
    fn mineral_tile() -> Tile {
        tile(12, 40)
    }

    /// The open room's first spawn, in the middle of the room.
    fn spawn_tile() -> Tile {
        tile(25, 25)
    }

    /// Where the creep that keeps a room in movement's view stands (the planner reads no creep).
    fn creep_tile() -> Tile {
        tile(30, 30)
    }

    /// The first level that allows a spawn: the level of the rooms the tests own.
    /// Uses: CONTROLLER_STRUCTURES (via `rules::earliest_level`).
    fn spawn_level() -> u8 {
        earliest_level(StructureType::Spawn, 0).expect("a level allows a spawn")
    }

    /// The open room's terrain, row-major as `Room.Terrain.getRawBuffer()` gives it (V156): walls
    /// on the border but two exits, as the flood tests' open room, and under the controller, the
    /// sources and the mineral. Uses: ROOM_SIZE, ROOM_AREA, the `Terrain::Wall` mask.
    fn open_raw() -> Vec<u8> {
        let size = usize::from(ROOM_SIZE);
        let last = size - 1;
        let exits = 20..30;
        let mut raw = vec![0u8; ROOM_AREA];
        for y in 0..size {
            for x in 0..size {
                let border = x == 0 || y == 0 || x == last || y == last;
                let exit = (y == 0 && exits.contains(&x)) || (x == last && exits.contains(&y));
                if border && !exit {
                    raw[y * size + x] = Terrain::Wall as u8;
                }
            }
        }
        let fixed = source_tiles()
            .into_iter()
            .chain([controller_tile(), mineral_tile()]);
        for t in fixed {
            raw[usize::from(t.y()) * size + usize::from(t.x())] = Terrain::Wall as u8;
        }
        raw
    }

    fn open_facts(spawns: Vec<Tile>) -> Facts {
        Facts {
            terrain: TerrainGrid::from_row_major(&open_raw()).expect("terrain"),
            controller: controller_tile(),
            sources: source_tiles(),
            mineral: Some(mineral_tile()),
            anchors: spawns.clone(),
            own: spawns.iter().map(|&t| (StructureType::Spawn, t)).collect(),
            spawns,
            standing: Vec::new(),
            prior: None,
        }
    }

    /// The tunables the Optional job passes (`PlannerConfig` → `Params`). Uses: PLANNER_MMO.
    fn params() -> Params {
        Params::from_config(&PLANNER_MMO)
    }

    /// What `flood` plans for `facts` anchored on `spawn`, every phase at once, with the job's
    /// tunables and the standing containers and links adopted. Uses: PLANNER_MMO.
    fn flood_plan(facts: &Facts, spawn: Tile) -> Result<Layout, PlanError> {
        let input = facts.input(Some(spawn));
        let mut run = flood::Run::with(&input, params());
        run.adopt(&facts.standing);
        let mut s = Scratch::default();
        while run.step(&input, &mut s)? == Step::More {}
        Ok(run.finish())
    }

    /// The plan the module's anchor rules pick for `facts`, worked out from `flood` and
    /// `validate` alone: the record's anchor first, then the anchors from the highest tile down; of
    /// the valid plans the one whose spawn tiles hold every standing spawn, then the one holding
    /// the most of `facts.own` on tiles of their type, the first tried on a tie; a plan holding
    /// everything ends the search, and so does the record anchor's plan holding every spawn.
    /// Uses: PLANNER_MMO.
    fn anchor_rule(facts: &Facts) -> Option<(Layout, Tile)> {
        let mut best: Option<(Layout, Tile, (bool, usize))> = None;
        let mut anchors = facts.anchors.clone();
        anchors.sort_unstable();
        // The record's anchor first (ADR-027), then the rest from the highest tile down.
        let prior = facts.prior.filter(|t| anchors.contains(t));
        let order: Vec<Tile> = prior
            .into_iter()
            .chain(anchors.iter().rev().copied().filter(|&t| Some(t) != prior))
            .collect();
        for &sp in &order {
            let Ok(layout) = flood_plan(facts, sp) else {
                continue;
            };
            if !validate(&facts.input(Some(sp)), &layout).valid() {
                continue;
            }
            let all = facts
                .spawns
                .iter()
                .all(|&o| o == sp || holds_spawn(&layout, o));
            let held = facts
                .own
                .iter()
                .filter(|&&(k, t)| layout.items.iter().any(|i| i.kind == k && i.tile == t))
                .count();
            if (all, held) == (true, facts.own.len()) || (all && Some(sp) == prior) {
                return Some((layout, sp));
            }
            if best.as_ref().is_none_or(|b| (all, held) > b.2) {
                best = Some((layout, sp, (all, held)));
            }
        }
        best.map(|(layout, sp, _)| (layout, sp))
    }

    /// Whether `layout` holds a spawn on `t`.
    fn holds_spawn(layout: &Layout, t: Tile) -> bool {
        layout.of(StructureType::Spawn).any(|i| i.tile == t)
    }

    /// One room as a tick's snapshot and movement show it.
    #[derive(Clone)]
    struct Scene {
        name: RoomName,
        /// The controller is the bot's, at `level` (nobody's otherwise).
        owned: bool,
        level: u8,
        /// The controller's structure row (none: the snapshot lists no controller).
        controller: Option<Tile>,
        sources: Vec<Tile>,
        mineral: Option<Tile>,
        spawns: Vec<Tile>,
        containers: Vec<Tile>,
        links: Vec<Tile>,
        /// Other own structures (`Game.structures` rows): extensions, towers, ramparts and so on.
        built: Vec<(StructureType, Tile)>,
        /// Own construction sites (`ObjectKind::Site` rows).
        sites: Vec<(StructureType, Tile)>,
        /// An own creep stands in the room, so movement tracks it and reads its terrain (the
        /// open room's) and statics (the sources and the mineral).
        read: bool,
    }

    impl Scene {
        /// Room `name` as the open room, owned at [`spawn_level`], with `spawns` standing.
        fn open(name: &str, spawns: &[Tile]) -> Self {
            Self {
                name: room(name),
                owned: true,
                level: spawn_level(),
                controller: Some(controller_tile()),
                sources: source_tiles(),
                mineral: Some(mineral_tile()),
                spawns: spawns.to_vec(),
                containers: Vec::new(),
                links: Vec::new(),
                built: Vec::new(),
                sites: Vec::new(),
                read: true,
            }
        }
    }

    /// The snapshot, movement and the planner over ticks, as `rustedscreep-wasm` drives them.
    struct World {
        snap: Snapshot,
        movement: Movement,
        planner: Planner,
        time: u32,
        raw: Vec<u8>,
    }

    /// The ready plan's layout, anchor, `ready_at` and steps, owned.
    type Ready = (Layout, Tile, u32, u32);

    impl World {
        /// A global whose planner record segment was empty (no record: plans from what stands).
        fn new() -> Self {
            let mut planner = Planner::default();
            assert!(planner.loaded(Err(LoadError::Decode(DecodeError::Empty))));
            Self {
                snap: Snapshot::default(),
                movement: Movement::default(),
                planner,
                time: 0,
                raw: open_raw(),
            }
        }

        /// One tick: the snapshot of `scenes` (ids stable per room, kind and tile), movement's
        /// begin and first reads, then the planner's begin.
        /// Uses: CPU_TICK_LIMIT_MAX, CPU_BUCKET_MAX (a full bucket), SOURCE_ENERGY_CAPACITY,
        /// CONTAINER_HITS, CONSTRUCTION_COST (the object rows), MOVEMENT_MMO.
        fn tick(&mut self, scenes: &[Scene]) {
            self.tick_without_begin(scenes);
            self.planner.begin(&self.snap, &self.movement);
        }

        /// As [`World::tick`], without the planner's begin (the planner switched off).
        fn tick_without_begin(&mut self, scenes: &[Scene]) {
            self.time += 1;
            let snap = &mut self.snap;
            snap.begin(TickContext::from_raw(
                self.time,
                CPU_TICK_LIMIT_MAX,
                f64::from(CPU_TICK_LIMIT_MAX),
                i32::try_from(CPU_BUCKET_MAX).expect("bucket"),
            ));
            for s in scenes {
                let base = u128::from(s.name.packed_repr()) << 32;
                let key = |code: u128, t: Tile| ObjectKey(base | (code << 16) | t.index() as u128);
                let at = |t: Tile| {
                    Position::new(
                        RoomCoordinate::new(t.x()).expect("x"),
                        RoomCoordinate::new(t.y()).expect("y"),
                        s.name,
                    )
                };
                if s.read {
                    let row = CreepRow {
                        pos: at(creep_tile()),
                        my: true,
                        spawning: false,
                        fatigue: 0,
                    };
                    snap.creeps.push(key(0, creep_tile()), row).expect("creep");
                }
                let controller = if s.owned {
                    Controller::Mine { level: s.level }
                } else {
                    Controller::Neutral
                };
                snap.rooms.push(s.name, controller);
                let structures = s
                    .controller
                    .map(|t| (1, StructureType::Controller, t))
                    .into_iter()
                    .chain(s.spawns.iter().map(|&t| (2, StructureType::Spawn, t)))
                    .chain(s.links.iter().map(|&t| (3, StructureType::Link, t)))
                    .chain(
                        s.built
                            .iter()
                            .map(|&(k, t)| (0x100 + kind_rank(k) as u128, k, t)),
                    );
                for (code, kind, t) in structures {
                    let pos = at(t);
                    snap.structures
                        .push(key(code, t), || Ok(StructureStatic { kind, pos }))
                        .expect("structure");
                }
                let source = ObjectRow {
                    amount: SOURCE_ENERGY_CAPACITY,
                    total: SOURCE_ENERGY_CAPACITY,
                    hits: 0,
                };
                let container = ObjectRow {
                    amount: 0,
                    total: 0,
                    hits: CONTAINER_HITS,
                };
                let objects = s
                    .sources
                    .iter()
                    .map(|&t| (4, ObjectKind::Source, t, source))
                    .chain(
                        s.containers
                            .iter()
                            .map(|&t| (5, ObjectKind::Container, t, container)),
                    )
                    .chain(s.sites.iter().map(|&(k, t)| {
                        // A fresh site: no progress of the cost its type takes.
                        let site = ObjectRow {
                            amount: 0,
                            total: k.construction_cost().expect("a buildable type"),
                            hits: 0,
                        };
                        (0x200 + kind_rank(k) as u128, ObjectKind::Site(k), t, site)
                    }));
                for (code, kind, t, row) in objects {
                    let pos = at(t);
                    snap.objects
                        .push(key(code, t), || Ok(ObjectStatic { kind, pos }), row)
                        .expect("object");
                }
            }
            snap.finish();
            self.movement.begin(&self.snap, &MOVEMENT_MMO);
            for s in scenes.iter().filter(|s| s.read) {
                if self.movement.room_terrain(s.name).is_none() {
                    assert!(self.movement.set_terrain(s.name, &self.raw), "tracked");
                }
                self.movement
                    .set_statics(s.name, s.sources.iter().copied().chain(s.mineral));
            }
        }

        /// Steps the planner until `Step::Done`, as the Optional tier slices it; the phase of
        /// each step, in order. Uses: PLANNER_MMO.
        fn plan(&mut self) -> Vec<&'static str> {
            let mut phases = Vec::new();
            while self.planner.busy() {
                phases.push(self.planner.next_phase().expect("a phase while busy"));
                let step = self.planner.step(params());
                assert_eq!(step == Step::More, self.planner.busy(), "More while busy");
                assert!(phases.len() < 100, "a plan ends");
            }
            phases
        }

        /// Room `name`'s ready plan, owned so the world can tick on.
        fn ready(&self, name: RoomName) -> Ready {
            match self.planner.status(name) {
                Status::Ready(p) => (p.layout.clone(), p.anchor, p.ready_at, p.steps),
                other => panic!("{name:?} is not ready: {other:?}"),
            }
        }
    }

    fn run_to_end(mut r: Running) -> (Advance, u32) {
        let mut s = Scratch::default();
        let mut n = 0;
        loop {
            n += 1;
            match r.advance(Params::default(), &mut s) {
                Advance::More => {}
                other => return (other, n),
            }
            assert!(n < 100, "a run ends");
        }
    }

    #[test]
    fn a_run_steps_to_the_plan_the_whole_planner_gives() {
        let facts = open_facts(vec![tile(25, 25)]);
        let whole =
            flood::plan(&facts.input(Some(tile(25, 25))), &mut Scratch::default()).expect("plan");
        let (out, steps) = run_to_end(Running::new(facts));
        let Advance::Ready(layout, anchor) = out else {
            panic!("a plan");
        };
        assert_eq!(layout, whole);
        assert_eq!(anchor, tile(25, 25));
        // Every phase of the run, then the validation.
        assert!(steps > 2);
    }

    #[test]
    fn a_later_spawn_on_the_plan_keeps_its_anchor() {
        // Plan on (25, 25), then stand a spawn on one of the plan's later spawn tiles that comes
        // first in tile order: the planner must find the plan anchored on (25, 25) again. The
        // anchor is tried first here (the higher tile); a later spawn tried before the anchor is
        // the corpus test's case (`tests/planner_build.rs`, 366 of its reset cases).
        let first = tile(25, 25);
        let facts = open_facts(vec![first]);
        let whole = flood::plan(&facts.input(Some(first)), &mut Scratch::default()).expect("plan");
        let later = whole
            .of(StructureType::Spawn)
            .map(|i| i.tile)
            .filter(|&t| t != first)
            .min()
            .expect("a second spawn");
        let mut spawns = vec![first, later];
        spawns.sort_unstable();
        let facts = open_facts(spawns);
        let (out, _) = run_to_end(Running::new(facts));
        let Advance::Ready(layout, anchor) = out else {
            panic!("a plan");
        };
        assert_eq!(anchor, first);
        assert_eq!(layout, whole);
    }

    /// `Facts::read` names what a room lacks, in its order: a controller, then a source (the room
    /// can never be planned), then a spawn, then the terrain (both waited for; the terrain is
    /// checked and copied last, so a room with neither is told it lacks a spawn).
    /// Uses: CONTROLLER_STRUCTURES (the rooms' level, via `rules::earliest_level`), MOVEMENT_MMO.
    #[test]
    fn facts_read_names_what_a_room_lacks() {
        let spawn = spawn_tile();
        let mut no_controller = Scene::open("W1N1", &[spawn]);
        no_controller.controller = None;
        no_controller.sources.clear();
        let mut no_source = Scene::open("W1N2", &[spawn]);
        no_source.sources.clear();
        let mut no_source_no_spawn = Scene::open("W1N3", &[]);
        no_source_no_spawn.sources.clear();
        let no_spawn = Scene::open("W1N4", &[]);
        let mut no_terrain = Scene::open("W1N5", &[spawn]);
        no_terrain.read = false;
        let mut neither = Scene::open("W1N6", &[]);
        neither.read = false;
        let complete = Scene::open("W1N7", &[spawn]);
        let scenes = [
            no_controller,
            no_source,
            no_source_no_spawn,
            no_spawn,
            no_terrain,
            neither,
            complete,
        ];
        let mut w = World::new();
        w.tick(&scenes);
        let missing: Vec<_> = scenes
            .iter()
            .map(|s| Facts::read(&w.snap, &w.movement, s.name).err())
            .collect();
        assert_eq!(
            missing,
            [
                Some(Missing::Unplannable("no controller")),
                Some(Missing::Unplannable("no source")),
                Some(Missing::Unplannable("no source")),
                Some(Missing::Spawn),
                Some(Missing::Terrain),
                Some(Missing::Spawn),
                None,
            ]
        );
    }

    /// `Facts::read` keeps only the room's own objects, puts its sources and spawns in tile order
    /// and its standing containers and links in kind order (`kind_rank`: containers first), then
    /// tile order, whatever order the snapshot read them in; it takes the mineral from movement's
    /// statics (the one that is not a source) and copies movement's terrain, so it reads the open
    /// room exactly as `open_facts` builds it by hand.
    /// Uses: ROOM_AREA (every tile's terrain), `kind_rank` (layout::KINDS), MOVEMENT_MMO.
    #[test]
    fn facts_read_sorts_the_rooms_own_objects() {
        let (a, b) = (spawn_tile(), tile(30, 20));
        let mut scene = Scene::open("W1N1", &[b, a]);
        scene.sources.reverse();
        scene.containers = vec![tile(11, 11), tile(9, 11)];
        scene.links = vec![tile(26, 39), tile(24, 39)];
        // Another owned room's spawn, link and container are not this room's.
        let mut other = Scene::open("W2N1", &[tile(20, 20)]);
        other.containers = vec![tile(8, 8)];
        other.links = vec![tile(3, 3)];
        let mut w = World::new();
        w.tick(&[other, scene.clone()]);
        let Ok(facts) = Facts::read(&w.snap, &w.movement, scene.name) else {
            panic!("the room's facts");
        };
        let mut spawns = vec![a, b];
        spawns.sort_unstable();
        let hand = open_facts(spawns);
        assert_eq!(facts.controller, hand.controller);
        assert_eq!(facts.sources, hand.sources);
        assert_eq!(facts.mineral, hand.mineral);
        assert_eq!(facts.spawns, hand.spawns);
        assert!(kind_rank(StructureType::Container) < kind_rank(StructureType::Link));
        assert_eq!(
            facts.standing,
            [
                (StructureType::Container, tile(9, 11)),
                (StructureType::Container, tile(11, 11)),
                (StructureType::Link, tile(24, 39)),
                (StructureType::Link, tile(26, 39)),
            ]
        );
        assert!(
            (0..ROOM_AREA)
                .filter_map(Tile::from_index)
                .all(|t| facts.terrain.kind(t) == hand.terrain.kind(t))
        );
    }

    /// A room with no spawn is not planned, and the cheap spawn check keeps `Facts::read` from
    /// running at all: the room has no source either, which a read would mark failed for the rest
    /// of the global, yet tick after tick it stays `Status::None` and is counted nowhere. Once a
    /// spawn and the sources stand, the next `begin` starts a run at its first phase.
    /// Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level).
    #[test]
    fn a_room_with_no_spawn_reads_nothing_until_one_stands() {
        let mut bare = Scene::open("W1N1", &[]);
        bare.sources.clear();
        let name = bare.name;
        let mut w = World::new();
        for _ in 0..3 {
            w.tick(std::slice::from_ref(&bare));
            assert_eq!(w.planner.status(name), Status::None);
            assert_eq!(*w.planner.totals(), Totals::default());
            assert_eq!(w.planner.rooms().count(), 0);
            assert!(!w.planner.busy());
            assert_eq!(w.planner.next_phase(), None);
            assert_eq!(w.planner.step(params()), Step::Done);
            assert_eq!(w.planner.totals().steps, 0);
        }
        // Had the room's facts been read, it would have failed for the global.
        assert_eq!(
            Facts::read(&w.snap, &w.movement, name).err(),
            Some(Missing::Unplannable("no source"))
        );
        // Sources alone: still a room that waits for its spawn.
        w.tick(&[Scene::open("W1N1", &[])]);
        assert_eq!(w.planner.status(name), Status::None);
        assert_eq!(*w.planner.totals(), Totals::default());
        w.tick(&[Scene::open("W1N1", &[spawn_tile()])]);
        assert_eq!(w.planner.status(name), Status::Planning("core"));
        assert_eq!(w.planner.next_phase(), Some("core"));
        assert!(w.planner.busy());
        assert_eq!(
            *w.planner.totals(),
            Totals {
                started: 1,
                ..Totals::default()
            }
        );
    }

    /// A room whose terrain movement has not read yet waits (`Status::None`, counted nowhere),
    /// even with a spawn; the tick movement reads it, the room starts a run.
    /// Uses: MOVEMENT_MMO, PLANNER_MMO, CONTROLLER_STRUCTURES (the room's level).
    #[test]
    fn a_room_waits_for_its_terrain() {
        let mut unread = Scene::open("W1N1", &[spawn_tile()]);
        unread.read = false;
        let name = unread.name;
        let mut w = World::new();
        w.tick(std::slice::from_ref(&unread));
        assert_eq!(w.planner.status(name), Status::None);
        assert_eq!(*w.planner.totals(), Totals::default());
        assert!(!w.planner.busy());
        w.tick(&[Scene::open("W1N1", &[spawn_tile()])]);
        assert_eq!(w.planner.status(name), Status::Planning("core"));
        assert_eq!(w.planner.totals().started, 1);
    }

    /// A room with a spawn but no source fails in `begin` itself, with no run and no step, is
    /// counted once in `totals().failed`, and stays failed for the rest of the global, sources or
    /// not. Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level).
    #[test]
    fn a_room_with_a_spawn_and_no_source_fails_at_once() {
        let mut dry = Scene::open("W1N1", &[spawn_tile()]);
        dry.sources.clear();
        let name = dry.name;
        let failed = Totals {
            failed: 1,
            ..Totals::default()
        };
        let mut w = World::new();
        w.tick(std::slice::from_ref(&dry));
        assert_eq!(w.planner.status(name), Status::Failed("no source"));
        assert_eq!(*w.planner.totals(), failed);
        assert!(!w.planner.busy());
        assert_eq!(w.planner.step(params()), Step::Done);
        assert_eq!(*w.planner.totals(), failed);
        assert_eq!(
            w.planner.rooms().collect::<Vec<_>>(),
            [(name, Status::Failed("no source"))]
        );
        for scene in [dry, Scene::open("W1N1", &[spawn_tile()])] {
            w.tick(&[scene]);
            assert_eq!(w.planner.status(name), Status::Failed("no source"));
            assert_eq!(*w.planner.totals(), failed);
        }
    }

    /// Stepping the job until `Step::Done` makes the room `Ready` with exactly the plan `flood`
    /// gives for the same facts and tunables, anchored on the standing spawn: a valid plan whose
    /// spawn tiles include the anchor and number what the top level allows. Every phase of the
    /// run, then its validation, is one step, counted in the plan and the totals, and the plan is
    /// ready at the tick of its last step. A later `begin` with the same facts keeps it.
    /// Uses: PLANNER_MMO (the job's and flood's tunables), CONTROLLER_STRUCTURES (via
    /// `rules::allowed` at `MAX_LEVEL`, and the room's level), MOVEMENT_MMO.
    #[test]
    fn a_planned_room_is_ready_with_the_plan_flood_gives() {
        let spawn = spawn_tile();
        let scene = Scene::open("W1N1", &[spawn]);
        let name = scene.name;
        let mut w = World::new();
        w.tick(std::slice::from_ref(&scene));
        let phases = w.plan();
        assert_eq!(phases.first(), Some(&"core"));
        assert_eq!(phases.last(), Some(&"validate"));
        let steps = u32::try_from(phases.len()).expect("steps");
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        let facts = open_facts(vec![spawn]);
        assert_eq!(plan.layout, flood_plan(&facts, spawn).expect("flood plans"));
        assert_eq!(
            plan.layout,
            flood::plan(&facts.input(Some(spawn)), &mut Scratch::default()).expect("plan")
        );
        assert_eq!(plan.anchor, spawn);
        assert_eq!(plan.facts.spawns, [spawn]);
        assert_eq!((plan.ready_at, plan.steps), (w.time, steps));
        assert!(validate(&plan.facts.input(Some(spawn)), &plan.layout).valid());
        assert!(holds_spawn(&plan.layout, spawn));
        assert_eq!(
            plan.layout.count(StructureType::Spawn, MAX_LEVEL),
            allowed(StructureType::Spawn, MAX_LEVEL)
        );
        let done = Totals {
            started: 1,
            ready: 1,
            failed: 0,
            steps,
        };
        assert_eq!(*w.planner.totals(), done);
        assert!(!w.planner.busy());
        assert_eq!(w.planner.next_phase(), None);
        assert_eq!(w.planner.step(params()), Step::Done);
        assert_eq!(*w.planner.totals(), done);
        let ready = w.ready(name);
        for _ in 0..3 {
            w.tick(std::slice::from_ref(&scene));
            assert_eq!(w.ready(name), ready);
            assert!(!w.planner.busy());
            assert_eq!(*w.planner.totals(), done);
        }
    }

    /// Once a room is ready, losing its anchor spawn keeps the same plan while every standing
    /// spawn is on one of the plan's spawn tiles: another spawn on a later plan spawn tile, no
    /// spawn at all, or the anchor back beside it. The status stays `Ready` with the plan made at
    /// the first run, and no run starts. Uses: CONTROLLER_STRUCTURES (via `rules::allowed` at
    /// `MAX_LEVEL`: the plan holds more than one spawn), PLANNER_MMO, MOVEMENT_MMO.
    #[test]
    fn a_lost_anchor_keeps_the_plan_while_spawns_stand_on_its_tiles() {
        assert!(allowed(StructureType::Spawn, MAX_LEVEL) > 1);
        let anchor = spawn_tile();
        let name = room("W1N1");
        let mut w = World::new();
        w.tick(&[Scene::open("W1N1", &[anchor])]);
        w.plan();
        let ready = w.ready(name);
        let other = ready
            .0
            .of(StructureType::Spawn)
            .map(|i| i.tile)
            .find(|&t| t != anchor)
            .expect("a later spawn tile");
        let totals = *w.planner.totals();
        for spawns in [vec![other], vec![], vec![anchor, other], vec![anchor]] {
            w.tick(&[Scene::open("W1N1", &spawns)]);
            assert_eq!(w.ready(name), ready, "spawns {spawns:?}");
            assert!(!w.planner.busy());
            assert_eq!(*w.planner.totals(), totals);
        }
    }

    /// A spawn standing on a tile the ready plan holds no spawn on starts a new run at once (the
    /// status is `Planning` from that `begin`), and the run ends in a valid plan anchored on one
    /// of the standing spawns: the one the module's anchor rules pick, worked out from `flood` and
    /// `validate` alone. Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level).
    #[test]
    fn a_spawn_off_the_plan_plans_the_room_again() {
        let anchor = spawn_tile();
        let name = room("W1N1");
        let mut w = World::new();
        w.tick(&[Scene::open("W1N1", &[anchor])]);
        w.plan();
        let (layout, ..) = w.ready(name);
        let off = [tile(40, 40), tile(8, 30), tile(30, 8)]
            .into_iter()
            .find(|&t| !holds_spawn(&layout, t))
            .expect("a tile off the plan's spawns");
        w.tick(&[Scene::open("W1N1", &[anchor, off])]);
        assert_eq!(w.planner.status(name), Status::Planning("core"));
        assert!(w.planner.busy());
        assert_eq!(w.planner.totals().started, 2);
        w.plan();
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready again");
        };
        let mut spawns = vec![anchor, off];
        spawns.sort_unstable();
        assert_eq!(plan.facts.spawns, spawns);
        assert!(spawns.contains(&plan.anchor));
        assert!(validate(&plan.facts.input(Some(plan.anchor)), &plan.layout).valid());
        let (layout, at) = anchor_rule(&plan.facts).expect("a valid plan");
        assert_eq!((&plan.layout, plan.anchor), (&layout, at));
        assert_eq!(plan.ready_at, w.time);
        let t = w.planner.totals();
        assert_eq!((t.started, t.ready, t.failed), (2, 2, 0));
    }

    /// A ready plan made from facts that no plan fits fully (a standing spawn none of its spawn
    /// tiles holds: the anchor rules' "else the first valid one") is kept while those facts stand.
    /// The plan is deterministic in them (module doc), so a new run would make the same plan;
    /// the room is planned once, not again on every `begin`.
    /// Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level).
    #[test]
    fn a_fallback_plan_is_kept_while_its_facts_stand() {
        let anchor = spawn_tile();
        let name = room("W1N1");
        let mut w = World::new();
        w.tick(&[Scene::open("W1N1", &[anchor])]);
        w.plan();
        let (layout, ..) = w.ready(name);
        let off = [tile(40, 40), tile(8, 30), tile(30, 8)]
            .into_iter()
            .find(|&t| !holds_spawn(&layout, t))
            .expect("a tile off the plan's spawns");
        let scene = Scene::open("W1N1", &[anchor, off]);
        w.tick(std::slice::from_ref(&scene));
        w.plan();
        let ready = w.ready(name);
        // The scenario: no plan holds both spawns, so the room took the first valid one.
        let spawns = [anchor, off];
        assert!(
            !spawns
                .iter()
                .all(|&sp| sp == ready.1 || holds_spawn(&ready.0, sp)),
            "a fallback plan"
        );
        let totals = *w.planner.totals();
        for _ in 0..3 {
            w.tick(std::slice::from_ref(&scene));
            assert_eq!(w.ready(name), ready);
            assert_eq!(*w.planner.totals(), totals);
        }
    }

    /// A room whose standing spawn no plan can use fails once its run ends, counted once in
    /// `totals().failed` after one start, and stays failed. The spawn stands on a wall (the
    /// border), which `flood` cannot plan from. Uses: PLANNER_MMO, MOVEMENT_MMO,
    /// CONTROLLER_STRUCTURES (the room's level), the `Terrain::Wall` mask (the border).
    #[test]
    fn a_room_no_plan_fits_fails_for_the_global() {
        let walled = tile(0, 10);
        let raw = open_raw();
        assert_eq!(
            raw[usize::from(walled.y()) * usize::from(ROOM_SIZE) + usize::from(walled.x())],
            Terrain::Wall as u8
        );
        let scene = Scene::open("W1N1", &[walled]);
        let name = scene.name;
        let mut w = World::new();
        w.tick(std::slice::from_ref(&scene));
        assert_eq!(w.planner.status(name), Status::Planning("core"));
        w.plan();
        assert!(matches!(w.planner.status(name), Status::Failed(_)));
        let t = *w.planner.totals();
        assert_eq!((t.started, t.ready, t.failed), (1, 0, 1));
        w.tick(std::slice::from_ref(&scene));
        assert!(matches!(w.planner.status(name), Status::Failed(_)));
        assert!(!w.planner.busy());
        assert_eq!(*w.planner.totals(), t);
    }

    /// Rooms are planned one at a time, in the job's name order whatever order the snapshot read
    /// them in: each step goes to the first room still planning, so the second starts its first
    /// phase only once the first is ready, and the totals count both runs' steps.
    /// Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the rooms' level).
    #[test]
    fn rooms_are_planned_one_at_a_time_in_name_order() {
        let spawn = spawn_tile();
        let scenes = [Scene::open("W1N1", &[spawn]), Scene::open("W2N3", &[spawn])];
        let order = |scenes: &[Scene]| {
            let mut w = World::new();
            w.tick(scenes);
            w.planner.rooms().map(|(n, _)| n).collect::<Vec<_>>()
        };
        let forward = order(&scenes);
        let backward = order(&[scenes[1].clone(), scenes[0].clone()]);
        assert_eq!(forward, backward);
        assert_eq!(forward.len(), 2);
        let (first, second) = (forward[0], forward[1]);
        let mut w = World::new();
        w.tick(&scenes);
        assert_eq!(w.planner.totals().started, 2);
        assert_eq!(w.planner.step(params()), Step::More);
        assert_ne!(w.planner.status(first), Status::Planning("core"));
        assert_eq!(w.planner.status(second), Status::Planning("core"));
        let mut steps = 1;
        while matches!(w.planner.status(first), Status::Planning(_)) {
            assert_eq!(w.planner.status(second), Status::Planning("core"));
            w.planner.step(params());
            steps += 1;
        }
        let (layout, _, _, first_steps) = w.ready(first);
        assert_eq!(first_steps, steps);
        w.plan();
        let (other, _, _, second_steps) = w.ready(second);
        assert_eq!(other, layout, "the same facts give the same plan");
        let t = *w.planner.totals();
        assert_eq!(
            (t.started, t.ready, t.steps),
            (2, 2, first_steps + second_steps)
        );
    }

    /// A room no longer owned is dropped, its run or plan with it (`Status::None`, not listed);
    /// owned again, it starts a new run. Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES
    /// (the room's level).
    #[test]
    fn a_room_no_longer_owned_is_dropped() {
        let scene = Scene::open("W1N1", &[spawn_tile()]);
        let mut lost = scene.clone();
        lost.owned = false;
        let name = scene.name;
        let mut w = World::new();
        w.tick(std::slice::from_ref(&scene));
        assert_eq!(w.planner.step(params()), Step::More);
        w.tick(std::slice::from_ref(&lost));
        assert_eq!(w.planner.status(name), Status::None);
        assert!(!w.planner.busy());
        assert_eq!(w.planner.rooms().count(), 0);
        w.tick(std::slice::from_ref(&scene));
        assert_eq!(w.planner.status(name), Status::Planning("core"));
        w.plan();
        let ready = w.ready(name);
        w.tick(std::slice::from_ref(&lost));
        assert_eq!(w.planner.status(name), Status::None);
        w.tick(std::slice::from_ref(&scene));
        assert_eq!(w.planner.status(name), Status::Planning("core"));
        w.plan();
        assert_eq!(w.ready(name).0, ready.0);
        let t = w.planner.totals();
        assert_eq!((t.started, t.ready), (3, 2));
    }

    /// The room's standing containers and links reach the run (ADR-026): the plan is the one
    /// `flood` makes with them adopted, and the plan's facts keep them.
    /// Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level), CONTAINER_HITS
    /// (the container's row).
    #[test]
    fn standing_containers_reach_the_run() {
        let spawn = spawn_tile();
        let name = room("W1N1");
        let mut w = World::new();
        w.tick(&[Scene::open("W1N1", &[spawn])]);
        w.plan();
        let (layout, ..) = w.ready(name);
        let mining = *layout.roles.mining.first().expect("a mining tile");
        let mut scene = Scene::open("W1N1", &[spawn]);
        scene.containers = vec![mining];
        let mut w = World::new();
        w.tick(&[scene]);
        w.plan();
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        assert_eq!(plan.facts.standing, [(StructureType::Container, mining)]);
        assert_eq!(plan.layout, flood_plan(&plan.facts, spawn).expect("plan"));
        assert_eq!(plan.layout.roles.mining.first(), Some(&mining));
    }

    /// Whether `layout` gives `kind` the tile `t` (the anchor rule's "holds", by hand).
    fn gives(layout: &Layout, kind: StructureType, t: Tile) -> bool {
        layout.items.iter().any(|i| i.kind == kind && i.tile == t)
    }

    /// `own` in the order `Facts` keeps it: kind (`kind_rank`), then tile.
    fn by_kind(mut own: Vec<(StructureType, Tile)>) -> Vec<(StructureType, Tile)> {
        own.sort_unstable_by_key(|&(k, t)| (kind_rank(k), t));
        own
    }

    /// `Facts::read` takes the room's own sites with its structures (C7, ADR-026): own container
    /// and link sites join the standing containers and links (kind then tile order, a site on a
    /// built one's tile counted once), own spawn sites join the standing spawns in `anchors` (tile
    /// order) but not `spawns`, and `own` holds every own structure but the controller, the
    /// containers and every own site, in kind then tile order (`kind_rank`, then the column-major
    /// tile). Another owned room's structures, containers and sites stay out.
    /// Uses: `kind_rank` (layout::KINDS), CONSTRUCTION_COST (the site rows), MOVEMENT_MMO,
    /// CONTROLLER_STRUCTURES (the rooms' level, via `rules::earliest_level`).
    #[test]
    fn facts_read_takes_the_rooms_sites_and_own_structures() {
        use StructureType::{Container, Extension, Link, Rampart, Road, Spawn, Storage, Tower};
        let (a, b, site) = (spawn_tile(), tile(30, 20), tile(20, 25));
        let mut scene = Scene::open("W1N1", &[b, a]);
        scene.containers = vec![tile(11, 11), tile(9, 11)];
        scene.links = vec![tile(26, 39), tile(24, 39)];
        scene.built = vec![
            (Rampart, a),
            (Tower, tile(27, 23)),
            (Storage, tile(22, 25)),
            (Extension, tile(24, 27)),
        ];
        scene.sites = vec![
            (Road, tile(25, 30)),
            (Link, tile(27, 38)),
            (Spawn, site),
            (Extension, tile(26, 27)),
            // On the built link's tile: one link there.
            (Link, tile(24, 39)),
            (Container, tile(10, 12)),
        ];
        let mut other = Scene::open("W2N1", &[tile(20, 20)]);
        other.containers = vec![tile(8, 8)];
        other.links = vec![tile(3, 3)];
        other.built = vec![(Extension, tile(5, 5)), (Tower, tile(6, 5))];
        other.sites = vec![
            (Spawn, tile(21, 21)),
            (Container, tile(7, 7)),
            (Link, tile(4, 4)),
            (Extension, tile(6, 6)),
        ];
        let mut w = World::new();
        w.tick(&[other, scene.clone()]);
        let facts = Facts::read(&w.snap, &w.movement, scene.name).expect("the room's facts");
        // The kinds in `KINDS` order, the tiles in column-major (x, then y) order.
        let ranks = [
            Spawn, Extension, Container, Storage, Link, Tower, Road, Rampart,
        ]
        .map(kind_rank);
        assert!(ranks.is_sorted(), "{ranks:?}");
        assert!(tile(20, 25) < tile(25, 25) && tile(25, 25) < tile(30, 20));
        assert!(tile(9, 11) < tile(10, 12) && tile(10, 12) < tile(11, 11));
        assert_eq!(facts.spawns, [a, b]);
        assert_eq!(facts.anchors, [site, a, b]);
        assert_eq!(
            facts.standing,
            [
                (Container, tile(9, 11)),
                (Container, tile(10, 12)),
                (Container, tile(11, 11)),
                (Link, tile(24, 39)),
                (Link, tile(26, 39)),
                (Link, tile(27, 38)),
            ]
        );
        let own = [
            (Spawn, site),
            (Spawn, a),
            (Spawn, b),
            (Extension, tile(24, 27)),
            (Extension, tile(26, 27)),
            (Container, tile(9, 11)),
            (Container, tile(10, 12)),
            (Container, tile(11, 11)),
            (Storage, tile(22, 25)),
            (Link, tile(24, 39)),
            (Link, tile(26, 39)),
            (Link, tile(27, 38)),
            (Tower, tile(27, 23)),
            (Road, tile(25, 30)),
            (Rampart, a),
        ];
        assert_eq!(facts.own, own);
        assert!(
            !facts
                .own
                .iter()
                .any(|&(k, _)| k == StructureType::Controller)
        );
        // The other room reads its own.
        let theirs = Facts::read(&w.snap, &w.movement, room("W2N1")).expect("the other's facts");
        assert_eq!(theirs.anchors, [tile(20, 20), tile(21, 21)]);
        assert_eq!(
            theirs.standing,
            [
                (Container, tile(7, 7)),
                (Container, tile(8, 8)),
                (Link, tile(3, 3)),
                (Link, tile(4, 4)),
            ]
        );
        assert_eq!(theirs.own.len(), 9);
    }

    /// After a global reset at the level of a second spawn (C5), a room whose anchor spawn was
    /// lost and sited again on its tile, with a second spawn on the plan's first later spawn tile
    /// due there (tried first only when it sorts after the anchor, which it does not here: the
    /// corpus test `a_global_reset_with_later_spawns_keeps_the_plan_the_room_was_built_from` covers
    /// that order) and the plan's due extensions and
    /// towers standing, is planned from the site again: the fresh planner readies exactly the
    /// layout the room was built from, anchored on the site, and `plan_whole` on the same facts
    /// gives the same plan and anchor (the job and the whole run agree). Without the site the run
    /// has only the standing spawn to anchor on, and the plan moves (the site is what keeps it).
    /// Uses: CONTROLLER_STRUCTURES (via `rules::earliest_level` for the second spawn's level and
    /// `rules::allowed` for the extensions there), PLANNER_MMO, MOVEMENT_MMO, CONSTRUCTION_COST
    /// (the site row).
    #[test]
    fn a_spawn_site_anchors_the_plan_the_room_was_built_from() {
        use StructureType::{Extension, Spawn, Tower};
        let a = spawn_tile();
        let name = room("W1N1");
        let level = earliest_level(Spawn, 1).expect("a level allows a second spawn");
        let mut built_from = Scene::open("W1N1", &[a]);
        built_from.level = level;
        let mut w = World::new();
        w.tick(&[built_from]);
        w.plan();
        let (layout, anchor, ..) = w.ready(name);
        assert_eq!(anchor, a);
        let b = layout
            .of(Spawn)
            .filter(|i| i.tile != a && i.level <= level)
            .map(|i| i.tile)
            .min()
            .expect("a later spawn due at the level");
        let due: Vec<(StructureType, Tile)> = layout
            .items
            .iter()
            .filter(|i| i.level <= level && matches!(i.kind, Extension | Tower))
            .map(|i| (i.kind, i.tile))
            .collect();
        let extensions = due.iter().filter(|&&(k, _)| k == Extension).count();
        assert_eq!(
            u32::try_from(extensions).expect("count"),
            allowed(Extension, level)
        );
        let mut reset = Scene::open("W1N1", &[b]);
        reset.level = level;
        reset.sites = vec![(Spawn, a)];
        reset.built = due.clone();
        let mut w = World::new();
        w.tick(std::slice::from_ref(&reset));
        let facts = Facts::read(&w.snap, &w.movement, name).expect("the room's facts");
        let mut anchors = vec![a, b];
        anchors.sort_unstable();
        assert_eq!(facts.spawns, [b]);
        assert_eq!(facts.anchors, anchors);
        assert_eq!(
            facts.own,
            by_kind(
                due.iter()
                    .copied()
                    .chain([(Spawn, a), (Spawn, b)])
                    .collect()
            )
        );
        w.plan();
        let (again, at, ..) = w.ready(name);
        assert_eq!(at, a, "anchored on the site");
        assert!(again == layout, "the plan the room was built from");
        let whole = plan_whole(facts, params(), &mut Scratch::default()).expect("a plan");
        assert!(whole == (again, at), "plan_whole and the job agree");
        // Without the site the room has one candidate, the standing spawn.
        reset.sites.clear();
        let mut w = World::new();
        w.tick(&[reset]);
        w.plan();
        let (moved, on, ..) = w.ready(name);
        assert_eq!(on, b);
        assert!(
            moved != layout,
            "the scenario: the site is what keeps the plan"
        );
    }

    /// The fit (C5): of the anchors whose valid plans hold every standing spawn, the one holding
    /// the most of `facts.own` on tiles of their type wins, whichever is tried first; with `own`
    /// empty (both fit fully, so the first tried ends the search) or equal counts, the anchor
    /// tried first (the higher tile) wins; and a plan holding every standing spawn beats one that
    /// holds more of `own` but misses a spawn. The open room's plans put their later spawns on one
    /// side of their anchor, so no two standing spawns' plans hold each other: the anchors are a
    /// standing spawn S and an own spawn site T whose plan holds S (S is the first of T's later
    /// spawn tiles, a lower tile, so T is tried first). In each case `plan_whole`, the stepped run and the anchor rules worked
    /// out from `flood` and `validate` alone agree. Uses: PLANNER_MMO, CONTROLLER_STRUCTURES (via
    /// `rules::allowed` at `MAX_LEVEL`: a plan holds more than one spawn).
    #[test]
    fn the_anchor_whose_plan_holds_the_most_of_what_stands_wins() {
        use StructureType::{Extension, Spawn};
        assert!(allowed(Spawn, MAX_LEVEL) > 1);
        let t = spawn_tile();
        let lt = flood_plan(&open_facts(vec![t]), t).expect("a plan on T");
        let s = lt
            .of(Spawn)
            .map(|i| i.tile)
            .filter(|&x| x != t)
            .min()
            .expect("a later spawn tile");
        let ls = flood_plan(&open_facts(vec![s]), s).expect("a plan on S");
        for (sp, l) in [(s, &ls), (t, &lt)] {
            assert!(validate(&open_facts(vec![sp]).input(Some(sp)), l).valid());
        }
        // The scenario: T is tried first (the higher tile), T's plan holds S, S's does not hold T.
        assert!(s < t);
        assert!(holds_spawn(&lt, s) && !holds_spawn(&ls, t));
        let only = |l: &Layout, other: &Layout| -> Vec<(StructureType, Tile)> {
            l.of(Extension)
                .filter(|i| !gives(other, Extension, i.tile))
                .map(|i| (Extension, i.tile))
                .collect()
        };
        let (ls_only, lt_only) = (only(&ls, &lt), only(&lt, &ls));
        assert!(ls_only.len() >= 2 && !lt_only.is_empty());
        let stray = (0..ROOM_AREA)
            .filter_map(Tile::from_index)
            .filter(|x| !x.is_edge())
            .find(|&x| !gives(&ls, Extension, x) && !gives(&lt, Extension, x))
            .expect("a tile neither plan gives an extension");
        let with = |spawns: &[Tile], own: Vec<(StructureType, Tile)>| {
            let mut f = open_facts(spawns.to_vec());
            f.anchors = vec![s, t];
            f.own = by_kind(own);
            f
        };
        let pick = |f: Facts| -> Tile {
            let (rule, at) = anchor_rule(&f).expect("a valid plan");
            let (stepped, _) = run_to_end(Running::new(f.clone()));
            let Advance::Ready(run, run_at) = stepped else {
                panic!("the run readies a plan");
            };
            let (whole, whole_at) =
                plan_whole(f, params(), &mut Scratch::default()).expect("a plan");
            assert!(
                whole == rule && run == rule,
                "the job and the anchor rules agree"
            );
            assert_eq!((whole_at, run_at), (at, at));
            let want = if at == s { &ls } else { &lt };
            assert!(&rule == want, "the plan of the anchor picked");
            at
        };
        let spawn_s = (Spawn, s);
        // Nothing stands but the spawn both plans hold: T's fit is full, so T, tried first, wins.
        assert_eq!(pick(with(&[s], Vec::new())), t);
        // T holds more (its extensions), fully or with a stray extension neither holds: it wins.
        let more_t: Vec<_> = [spawn_s].into_iter().chain(lt_only.clone()).collect();
        assert_eq!(pick(with(&[s], more_t.clone())), t);
        let stray_t: Vec<_> = more_t.into_iter().chain([(Extension, stray)]).collect();
        assert_eq!(pick(with(&[s], stray_t)), t);
        // Equal counts, short of full: the anchor tried first, T, wins.
        assert_eq!(pick(with(&[s], vec![spawn_s, (Extension, stray)])), t);
        let even = vec![spawn_s, ls_only[0], lt_only[0]];
        assert_eq!(pick(with(&[s], even)), t);
        // S, tried later, holds more: it wins.
        let more_s: Vec<_> = [spawn_s].into_iter().chain(ls_only.clone()).collect();
        assert_eq!(pick(with(&[s], more_s)), s);
        // Both spawns stand: S's plan holds more of `own` but not T, T's holds both: T wins.
        let both = [spawn_s, (Spawn, t)].into_iter().chain(ls_only.clone());
        let f = with(&[s, t], both.collect());
        assert!(1 + ls_only.len() > 2, "S's plan holds more");
        assert_eq!(pick(f), t);
    }

    /// After a fallback plan is ready (C0: no plan held both standing spawns), the room is not
    /// planned again for a spawn it knew, a new spawn on one of the plan's spawn tiles, or an own
    /// spawn site off them (a site is no standing spawn); a new spawn standing off the plan's
    /// spawn tiles plans it again at once, and the run ends in the anchor rules' plan for the new
    /// facts. Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level, and via
    /// `rules::allowed` at `MAX_LEVEL`: the plan holds more than one spawn), CONSTRUCTION_COST
    /// (the site row).
    #[test]
    fn a_new_spawn_off_a_fallback_plan_plans_the_room_again() {
        assert!(allowed(StructureType::Spawn, MAX_LEVEL) > 1);
        let anchor = spawn_tile();
        let name = room("W1N1");
        let mut w = World::new();
        w.tick(&[Scene::open("W1N1", &[anchor])]);
        w.plan();
        let (layout, ..) = w.ready(name);
        let offs: Vec<Tile> = [tile(40, 40), tile(8, 30), tile(30, 8), tile(40, 25)]
            .into_iter()
            .filter(|&t| !holds_spawn(&layout, t))
            .collect();
        let off = *offs.first().expect("a tile off the plan's spawns");
        w.tick(&[Scene::open("W1N1", &[anchor, off])]);
        w.plan();
        let fallback = w.ready(name);
        let mut known = vec![anchor, off];
        known.sort_unstable();
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        assert_eq!(plan.facts.spawns, known);
        assert!(
            !known
                .iter()
                .all(|&sp| sp == fallback.1 || holds_spawn(&fallback.0, sp)),
            "the scenario: a fallback plan"
        );
        let on = fallback
            .0
            .of(StructureType::Spawn)
            .map(|i| i.tile)
            .find(|t| !known.contains(t))
            .expect("a plan spawn tile no spawn stands on");
        let new = offs
            .iter()
            .copied()
            .find(|&t| t != off && !holds_spawn(&fallback.0, t))
            .expect("a second tile off the plan's spawns");
        let totals = *w.planner.totals();
        let mut sited = Scene::open("W1N1", &[anchor, off]);
        sited.sites = vec![(StructureType::Spawn, new)];
        for scene in [
            Scene::open("W1N1", &[anchor, off]),
            Scene::open("W1N1", &[off]),
            Scene::open("W1N1", &[anchor, off, on]),
            sited,
        ] {
            w.tick(std::slice::from_ref(&scene));
            assert_eq!(w.ready(name), fallback, "{:?}", scene.spawns);
            assert!(!w.planner.busy());
            assert_eq!(*w.planner.totals(), totals);
        }
        w.tick(&[Scene::open("W1N1", &[anchor, off, new])]);
        assert_eq!(w.planner.status(name), Status::Planning("core"));
        assert_eq!(w.planner.totals().started, totals.started + 1);
        w.plan();
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready again");
        };
        let mut spawns = vec![anchor, off, new];
        spawns.sort_unstable();
        assert_eq!(plan.facts.spawns, spawns);
        let (layout, at) = anchor_rule(&plan.facts).expect("a valid plan");
        assert_eq!((&plan.layout, plan.anchor), (&layout, at));
        assert_eq!(plan.ready_at, w.time);
    }

    /// A room planned at the top level with every plan spawn standing and its extensions built
    /// keeps its plan when the anchor spawn is lost and no site replaces it, while the other
    /// spawns stand on the plan's spawn tiles: `begin` does not plan it again (same plan,
    /// `ready_at` and steps; no run; the totals unchanged), though a new run from these facts,
    /// with no anchor on the lost spawn's tile, would plan the room elsewhere. A site on the lost
    /// tile, then the spawn rebuilt, keep it too. Uses: CONTROLLER_STRUCTURES (via
    /// `rules::allowed` at `MAX_LEVEL`), PLANNER_MMO, MOVEMENT_MMO, CONSTRUCTION_COST (the site
    /// row).
    #[test]
    fn a_lost_anchor_with_no_site_keeps_the_plan() {
        use StructureType::{Extension, Spawn};
        assert!(allowed(Spawn, MAX_LEVEL) > 1);
        let a = spawn_tile();
        let name = room("W1N1");
        let layout = flood_plan(&open_facts(vec![a]), a).expect("a plan on the anchor");
        let spawns: Vec<Tile> = layout.of(Spawn).map(|i| i.tile).collect();
        assert_eq!(
            u32::try_from(spawns.len()).expect("count"),
            allowed(Spawn, MAX_LEVEL)
        );
        let mut others: Vec<Tile> = spawns.iter().copied().filter(|&t| t != a).collect();
        others.sort_unstable();
        let extensions: Vec<_> = layout.of(Extension).map(|i| (Extension, i.tile)).collect();
        let scene = |spawns: &[Tile], sites: Vec<(StructureType, Tile)>| {
            let mut s = Scene::open("W1N1", spawns);
            s.level = MAX_LEVEL;
            s.built = extensions.clone();
            s.sites = sites;
            s
        };
        let mut w = World::new();
        w.tick(&[scene(&spawns, Vec::new())]);
        w.plan();
        let ready = w.ready(name);
        assert_eq!((&ready.0, ready.1), (&layout, a), "planned from the anchor");
        let totals = *w.planner.totals();
        let lost = scene(&others, Vec::new());
        for s in [
            lost.clone(),
            lost.clone(),
            scene(&others, vec![(Spawn, a)]),
            scene(&spawns, Vec::new()),
        ] {
            w.tick(std::slice::from_ref(&s));
            assert_eq!(w.ready(name), ready, "{:?} {:?}", s.spawns, s.sites);
            assert!(!w.planner.busy());
            assert_eq!(*w.planner.totals(), totals);
        }
        // What `begin` keeps the room from: a run from the lost anchor's facts moves the plan.
        let mut fresh = World::new();
        fresh.tick(std::slice::from_ref(&lost));
        let facts = Facts::read(&fresh.snap, &fresh.movement, name).expect("the room's facts");
        assert_eq!(facts.anchors, others);
        let (moved, at) = plan_whole(facts, params(), &mut Scratch::default()).expect("a plan");
        assert!(others.contains(&at));
        assert!(
            moved != layout,
            "the scenario: a new run would move the plan"
        );
    }

    /// The frame of `payload` as the planner's segment holds it: owner `kind`, `schema`, written
    /// on tick `written` (`persist::frame`).
    fn framed(kind: u16, schema: u16, written: u32, payload: &[u8]) -> String {
        let mut text = String::new();
        frame::encode(kind, schema, written, payload, &mut text).expect("a payload that fits");
        text
    }

    /// `rec`'s payload (`record`'s layout).
    fn payload_of(rec: &Record) -> Vec<u8> {
        let mut out = Vec::new();
        rec.encode(&mut out);
        out
    }

    /// The tick the tests' records were written on: any tick, echoed by `Loaded::Record`.
    const WRITTEN: u32 = 4_321;

    impl World {
        /// A global whose planner record has not loaded yet (`Planner::default`).
        fn unloaded() -> Self {
            Self {
                snap: Snapshot::default(),
                movement: Movement::default(),
                planner: Planner::default(),
                time: 0,
                raw: open_raw(),
            }
        }

        /// A global whose planner segment held `rec`, written on [`WRITTEN`]: the frame encoded,
        /// decoded and handed to `Planner::loaded`, as the glue does after a reset.
        /// Uses: `persist::kind::PLANNER`, `record::SCHEMA`.
        fn with_record(rec: &Record) -> Self {
            let mut w = Self::unloaded();
            let text = framed(kind::PLANNER, record::SCHEMA, WRITTEN, &payload_of(rec));
            let mut buf = Vec::new();
            let f = frame::decode(&text, &mut buf).expect("a frame");
            assert!(w.planner.loaded(Ok(f)), "a usable record");
            let rooms = rec.entries().len();
            assert_eq!(
                w.planner.load_state(),
                Some(Loaded::Record {
                    rooms,
                    written: WRITTEN
                })
            );
            w
        }
    }

    /// A record of two rooms' entries (an anchor each, one with a container and a link adopted).
    fn two_rooms() -> Record {
        let mut r = Record::default();
        r.set(Entry {
            room: room("W1N1"),
            anchor: spawn_tile(),
            adopted: vec![
                (StructureType::Container, tile(11, 10)),
                (StructureType::Link, tile(26, 39)),
            ],
        });
        r.set(Entry {
            room: room("W2N3"),
            anchor: tile(30, 20),
            adopted: Vec::new(),
        });
        r
    }

    /// What the record's load means for writing it (ADR-027): a segment holding a record this
    /// build refuses (another owner, another schema, a payload it cannot read, not text) is
    /// written again at once with the empty record; one whose text never arrived
    /// (`LoadError::Unavailable`, after `PersistConfig::load_retries` + 1 tries) is left as it is:
    /// the global writes nothing, not even once a plan is ready; a good or empty one is written
    /// only when an entry changes. Uses: PERSIST_MMO (load_retries), PLANNER_MMO, MOVEMENT_MMO,
    /// CONTROLLER_STRUCTURES (the room's level).
    #[test]
    fn a_refused_record_is_written_again_and_an_unread_one_is_left() {
        let bytes = payload_of(&two_rooms());
        let refused = framed(kind::PLANNER, record::SCHEMA + 1, WRITTEN, &bytes);
        let mut buf = Vec::new();
        let mut p = Planner::default();
        p.loaded(frame::decode(&refused, &mut buf).map_err(LoadError::Decode));
        assert!(p.writes_record());
        assert!(p.take_dirty(), "the refused segment is replaced at once");
        assert!(!p.take_dirty());
        let mut p = Planner::default();
        p.loaded(Err(LoadError::NotText));
        assert!(p.take_dirty());
        // Never read: nothing is written in this global.
        let mut w = World::unloaded();
        let given_up = LoadError::Unavailable {
            tries: PERSIST_MMO.load_retries + 1,
        };
        assert!(!w.planner.loaded(Err(given_up)));
        assert!(!w.planner.writes_record());
        w.tick(&[Scene::open("W1N1", &[spawn_tile()])]);
        w.plan();
        let _ = w.ready(room("W1N1"));
        assert!(
            w.planner
                .record()
                .is_some_and(|r| r.get(room("W1N1")).is_some()),
            "the plan's entry is kept in the heap"
        );
        assert!(!w.planner.take_dirty(), "but the segment is not written");
        // A good record: clean until an entry changes.
        let w = World::with_record(&two_rooms());
        let mut planner = w.planner;
        assert!(planner.writes_record());
        assert!(!planner.take_dirty());
    }

    /// The record drops a room no longer owned even while the planner is switched off (the glue
    /// calls `forget_unowned` alone then), so a room lost and claimed again later does not plan
    /// from its earlier claim's entry. Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES.
    #[test]
    fn a_lost_rooms_entry_is_dropped_while_the_planner_is_off() {
        let mut w = World::with_record(&two_rooms());
        assert!(w.planner.record().is_some_and(|r| r.entries().len() == 2));
        // A tick with only W1N1 owned, as the snapshot sees it, without the planner's begin.
        let scene = Scene::open("W1N1", &[spawn_tile()]);
        w.tick(std::slice::from_ref(&scene));
        let _ = w.planner.take_dirty();
        let mut gone = scene.clone();
        gone.owned = false;
        w.tick_without_begin(std::slice::from_ref(&gone));
        w.planner.forget_unowned(&w.snap);
        assert!(w.planner.record().is_some_and(|r| r.entries().is_empty()));
        assert!(w.planner.take_dirty(), "the dropped entries are written");
    }

    /// `Planner::loaded` (ADR-027): every way the record's load fails leaves an empty record in
    /// use, so rooms are planned from what stands, and `load_state` says why: another owner's
    /// frame or another schema (the header check), a payload `record::decode` refuses, a segment
    /// that never loaded or held no string; each returns false (nothing usable). An empty segment
    /// returns true with an empty record too, and a good frame gives its record, with its room
    /// count and written tick. Once a record is in use a later load is ignored, whatever it holds;
    /// `no_record` (no segment configured) gives `Loaded::Off`, and a later load is ignored then.
    /// Uses: `persist::kind::{PLANNER, CONFIG}`, `record::SCHEMA`, PERSIST_MMO (`load_retries`:
    /// a load given up), `persist::frame` (the segment's text).
    #[test]
    fn the_records_load_leaves_a_record_in_use_whatever_the_segment_held() {
        let good = two_rooms();
        let bytes = payload_of(&good);
        let mut long = bytes.clone();
        long.push(0);
        let given_up = LoadError::Unavailable {
            tries: PERSIST_MMO.load_retries + 1,
        };
        let load = |seg: Result<&str, LoadError>| -> (bool, Planner) {
            let mut p = Planner::default();
            let mut buf = Vec::new();
            let f = seg.and_then(|t| frame::decode(t, &mut buf).map_err(LoadError::Decode));
            let ok = p.loaded(f);
            (ok, p)
        };
        let other_kind = framed(kind::CONFIG, record::SCHEMA, WRITTEN, &bytes);
        let other_schema = framed(kind::PLANNER, record::SCHEMA + 1, WRITTEN, &bytes);
        let refused = framed(kind::PLANNER, record::SCHEMA, WRITTEN, &long);
        let failures = [
            (
                Ok(other_kind.as_str()),
                Refusal::Load(LoadError::Decode(DecodeError::Kind {
                    expected: kind::PLANNER,
                    found: kind::CONFIG,
                })),
            ),
            (
                Ok(other_schema.as_str()),
                Refusal::Load(LoadError::Decode(DecodeError::Schema {
                    expected: record::SCHEMA,
                    found: record::SCHEMA + 1,
                })),
            ),
            (Ok(refused.as_str()), Refusal::Record(RecordError::Long)),
            (Err(given_up), Refusal::Load(given_up)),
            (Err(LoadError::NotText), Refusal::Load(LoadError::NotText)),
        ];
        for (seg, why) in failures {
            let (ok, p) = load(seg);
            assert!(!ok, "{why}");
            assert_eq!(p.load_state(), Some(Loaded::Refused(why)));
            assert_eq!(p.record(), Some(&Record::default()), "{why}");
        }
        // An empty segment: never written, or cleared.
        let (ok, p) = load(Ok(""));
        assert!(ok);
        assert_eq!(p.load_state(), Some(Loaded::Empty));
        assert_eq!(p.record(), Some(&Record::default()));
        // A good record.
        let text = framed(kind::PLANNER, record::SCHEMA, WRITTEN, &bytes);
        let (ok, mut p) = load(Ok(text.as_str()));
        assert!(ok);
        let in_use = Some(Loaded::Record {
            rooms: good.entries().len(),
            written: WRITTEN,
        });
        assert_eq!(p.load_state(), in_use);
        assert_eq!(p.record(), Some(&good));
        // Later loads are ignored once a record is in use: a failure, an empty segment, another
        // record, or no segment at all.
        let mut buf = Vec::new();
        let other = framed(
            kind::PLANNER,
            record::SCHEMA,
            WRITTEN + 1,
            &payload_of(&Record::default()),
        );
        p.loaded(Err(given_up));
        p.loaded(Err(LoadError::Decode(DecodeError::Empty)));
        p.loaded(frame::decode(&other, &mut buf).map_err(LoadError::Decode));
        p.no_record();
        assert_eq!(p.load_state(), in_use);
        assert_eq!(p.record(), Some(&good));
        // A refused record stays the empty one in use: a good record later is ignored.
        let (_, mut p) = load(Ok(refused.as_str()));
        p.loaded(frame::decode(&text, &mut buf).map_err(LoadError::Decode));
        assert_eq!(
            p.load_state(),
            Some(Loaded::Refused(Refusal::Record(RecordError::Long)))
        );
        assert_eq!(p.record(), Some(&Record::default()));
        // No segment configured: an empty record at once, and a load after it is ignored.
        let mut p = Planner::default();
        assert_eq!((p.load_state(), p.record()), (None, None));
        p.no_record();
        assert_eq!(p.load_state(), Some(Loaded::Off));
        assert_eq!(p.record(), Some(&Record::default()));
        p.loaded(frame::decode(&text, &mut buf).map_err(LoadError::Decode));
        assert_eq!(p.load_state(), Some(Loaded::Off));
        assert_eq!(p.record(), Some(&Record::default()));
    }

    /// No room is planned before the record's load finished (ADR-027): a fresh planner with an
    /// owned room, its spawn and its terrain starts nothing, tick after tick (no status, no run,
    /// no step, nothing counted, nothing to write); the `begin` after `loaded` starts the run at
    /// its first phase, and so does the `begin` after `no_record`.
    /// Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level).
    #[test]
    fn no_room_is_planned_before_the_record_loads() {
        let scene = Scene::open("W1N1", &[spawn_tile()]);
        let name = scene.name;
        type Unlock = fn(&mut Planner);
        let unlocks: [(&str, Unlock); 2] = [
            ("loaded", |p| {
                assert!(p.loaded(Err(LoadError::Decode(DecodeError::Empty))));
            }),
            ("no_record", Planner::no_record),
        ];
        for (how, unlock) in unlocks {
            let mut w = World::unloaded();
            for _ in 0..3 {
                w.tick(std::slice::from_ref(&scene));
                assert_eq!(w.planner.status(name), Status::None, "{how}");
                assert!(!w.planner.busy());
                assert_eq!(w.planner.next_phase(), None);
                assert_eq!(w.planner.rooms().count(), 0);
                assert_eq!(w.planner.step(params()), Step::Done);
                assert_eq!(*w.planner.totals(), Totals::default());
                assert_eq!(w.planner.load_state(), None);
                assert!(!w.planner.take_dirty());
                let mut out = Vec::new();
                w.planner.payload(&mut out);
                assert!(out.is_empty(), "no record to write before it loads");
            }
            unlock(&mut w.planner);
            w.tick(std::slice::from_ref(&scene));
            assert_eq!(w.planner.status(name), Status::Planning("core"), "{how}");
            assert!(w.planner.busy());
            assert_eq!(w.planner.totals().started, 1);
        }
    }

    /// The record's anchor (ADR-027): after a global reset at the level of a second spawn (C5),
    /// with the anchor spawn A lost and not sited again and the plan's first later spawn B
    /// standing, a planner whose record holds the room's entry plans it on A again: the layout
    /// the room was built from, the facts' prior anchor A, one anchor's run (the steps of the
    /// first plan, in the plan and the totals). A stray extension neither A's nor B's plan holds
    /// stands too, so A's fit is not full and only the record's keep rule ends the search before
    /// B is tried. The same entry is set again, so nothing is to be written. With an empty record
    /// the same scene has only B to anchor on, and the plan moves.
    /// Uses: CONTROLLER_STRUCTURES (via `rules::earliest_level` for the second spawn's level),
    /// PLANNER_MMO, MOVEMENT_MMO, `persist::kind::PLANNER`, `record::SCHEMA`.
    #[test]
    fn the_records_anchor_plans_the_room_it_was_built_from_in_one_run() {
        use StructureType::{Extension, Spawn};
        let a = spawn_tile();
        let name = room("W1N1");
        let level = earliest_level(Spawn, 1).expect("a level allows a second spawn");
        let mut built_from = Scene::open("W1N1", &[a]);
        built_from.level = level;
        let mut w = World::new();
        w.tick(&[built_from]);
        w.plan();
        let (layout, anchor, _, one_run) = w.ready(name);
        assert_eq!(anchor, a);
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        let entry = plan.entry(name);
        assert_eq!((entry.room, entry.anchor), (name, a));
        assert_eq!(w.planner.record().and_then(|r| r.get(name)), Some(&entry));
        let b = layout
            .of(Spawn)
            .filter(|i| i.tile != a && i.level <= level)
            .map(|i| i.tile)
            .min()
            .expect("a later spawn due at the level");
        let on_b = flood_plan(&open_facts(vec![b]), b).expect("a plan on B");
        let stray = (0..ROOM_AREA)
            .filter_map(Tile::from_index)
            .filter(|x| !x.is_edge())
            .find(|&x| !gives(&layout, Extension, x) && !gives(&on_b, Extension, x))
            .expect("a tile neither plan gives an extension");
        let mut reset = Scene::open("W1N1", &[b]);
        reset.level = level;
        reset.built = vec![(Extension, stray)];
        let mut rec = Record::default();
        assert!(rec.set(entry.clone()));
        let mut w = World::with_record(&rec);
        w.tick(std::slice::from_ref(&reset));
        assert_eq!(w.planner.status(name), Status::Planning("core"));
        w.plan();
        let (again, at, _, steps) = w.ready(name);
        assert_eq!(at, a, "anchored on the record's anchor");
        assert!(again == layout, "the plan the room was built from");
        assert_eq!(steps, one_run, "one anchor's run");
        let t = *w.planner.totals();
        assert_eq!((t.started, t.ready, t.failed, t.steps), (1, 1, 0, one_run));
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        let mut anchors = vec![a, b];
        anchors.sort_unstable();
        assert_eq!(plan.facts.prior, Some(a));
        assert_eq!(plan.facts.anchors, anchors);
        assert_eq!(plan.facts.spawns, [b]);
        // The scenario: A's plan holds B but not the stray, so its fit is short of full, and the
        // job and the anchor rules agree on the plan.
        assert!(holds_spawn(&layout, b) && !gives(&layout, Extension, stray));
        assert_eq!(plan.facts.own.len(), 2);
        let (rule, rule_at) = anchor_rule(&plan.facts).expect("a valid plan");
        assert_eq!((&rule, rule_at), (&layout, a));
        assert_eq!(plan.entry(name), entry);
        assert!(!w.planner.take_dirty(), "the same entry: nothing to write");
        // Without the record, the room has one candidate, the standing spawn.
        let mut w = World::new();
        w.tick(&[reset]);
        w.plan();
        let (moved, on, ..) = w.ready(name);
        assert_eq!(on, b);
        assert!(moved == on_b, "the plan on B");
        assert!(
            moved != layout,
            "the scenario: the record is what keeps the plan"
        );
    }

    /// The keep rule (ADR-027): a record anchor A whose valid plan does not hold a standing spawn
    /// (a new spawn standing off A's spawn tiles, with A lost or standing) is not kept at once:
    /// the run goes on to the other anchors (more steps than one anchor's run) and readies what
    /// the anchor rules worked out from `flood` and `validate` give for the facts with the prior
    /// anchor. With A lost, the new spawn's plan holds everything standing and wins.
    /// Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level),
    /// `persist::kind::PLANNER`, `record::SCHEMA`.
    #[test]
    fn a_record_anchor_whose_plan_misses_a_standing_spawn_is_not_kept() {
        let a = spawn_tile();
        let name = room("W1N1");
        let mut w = World::new();
        w.tick(&[Scene::open("W1N1", &[a])]);
        w.plan();
        let (layout, anchor, _, one_run) = w.ready(name);
        assert_eq!(anchor, a);
        let mut rec = Record::default();
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        rec.set(plan.entry(name));
        let off = [tile(40, 40), tile(8, 30), tile(30, 8)]
            .into_iter()
            .find(|&t| !holds_spawn(&layout, t))
            .expect("a tile off the plan's spawns");
        let mut both = vec![a, off];
        both.sort_unstable();
        for spawns in [vec![off], both] {
            let mut w = World::with_record(&rec);
            w.tick(&[Scene::open("W1N1", &spawns)]);
            w.plan();
            let Status::Ready(plan) = w.planner.status(name) else {
                panic!("ready with spawns {spawns:?}");
            };
            assert_eq!(plan.facts.prior, Some(a));
            assert_eq!(plan.facts.spawns, spawns);
            let (rule, at) = anchor_rule(&plan.facts).expect("a valid plan");
            assert_eq!((&plan.layout, plan.anchor), (&rule, at), "{spawns:?}");
            assert!(
                plan.steps > one_run,
                "the run went on past the record's anchor: {spawns:?}"
            );
            assert_eq!(plan.ready_at, w.time);
            if spawns == [off] {
                assert_eq!(plan.anchor, off, "the new spawn's plan holds what stands");
                assert!(plan.layout != layout);
            }
        }
    }

    /// The record's upkeep (ADR-027): a loaded entry of a room this global does not own is
    /// dropped at the first `begin` and marks the record for writing (`take_dirty` true once);
    /// no entry is set while the room plans, and the plan becoming ready sets its entry
    /// (`Plan::entry`) and marks it once. Ticks with the same facts leave it clean; the payload
    /// round-trips through `Record::decode` to the record in use, and a reset planned from it sets
    /// the same entry again (nothing to write). The room no longer owned drops its entry and
    /// marks the record, whose payload is then the empty record's.
    /// Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level),
    /// `persist::kind::PLANNER`, `record::SCHEMA`.
    #[test]
    fn the_record_follows_the_ready_plans_and_the_owned_rooms() {
        let a = spawn_tile();
        let scene = Scene::open("W1N1", &[a]);
        let name = scene.name;
        let stale = room("W9N9");
        let mut rec = Record::default();
        rec.set(Entry {
            room: stale,
            anchor: a,
            adopted: Vec::new(),
        });
        let mut w = World::with_record(&rec);
        assert!(!w.planner.take_dirty(), "a load is no change");
        w.tick(std::slice::from_ref(&scene));
        assert_eq!(w.planner.record().map(Record::entries), Some(&[][..]));
        assert!(w.planner.take_dirty(), "the stale room's entry dropped");
        assert!(!w.planner.take_dirty());
        while w.planner.busy() {
            assert_eq!(w.planner.record().and_then(|r| r.get(name)), None);
            assert!(!w.planner.take_dirty(), "nothing set while the room plans");
            w.planner.step(params());
        }
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        let entry = plan.entry(name);
        assert_eq!(entry.anchor, a);
        let record = w.planner.record().expect("in use");
        assert_eq!(record.entries(), std::slice::from_ref(&entry));
        assert!(w.planner.take_dirty(), "the new entry");
        assert!(!w.planner.take_dirty());
        let (layout, ..) = w.ready(name);
        for _ in 0..3 {
            w.tick(std::slice::from_ref(&scene));
            assert!(!w.planner.take_dirty(), "the same facts: no change");
        }
        let mut out = Vec::new();
        w.planner.payload(&mut out);
        let decoded = Record::decode(&out).expect("the payload decodes");
        assert_eq!(Some(&decoded), w.planner.record());
        // A reset planned from that record sets the same entry again: nothing to write.
        let mut again = World::with_record(&decoded);
        again.tick(std::slice::from_ref(&scene));
        assert!(!again.planner.take_dirty());
        again.plan();
        assert!(again.ready(name).0 == layout);
        assert_eq!(again.planner.record(), Some(&decoded));
        assert!(
            !again.planner.take_dirty(),
            "the same entry: nothing to write"
        );
        // The room no longer owned: its entry is dropped, and the record is written.
        let mut lost = scene.clone();
        lost.owned = false;
        w.tick(&[lost]);
        assert_eq!(w.planner.status(name), Status::None);
        assert_eq!(w.planner.record().and_then(|r| r.get(name)), None);
        assert!(w.planner.take_dirty(), "the entry dropped");
        assert!(!w.planner.take_dirty());
        out.clear();
        w.planner.payload(&mut out);
        assert_eq!(out, payload_of(&Record::default()));
        assert_eq!(Record::decode(&out), Ok(Record::default()));
    }

    /// `Plan::entry` (ADR-027) keeps the plan's anchor and only the facts' standing containers and
    /// links the layout gives their type on their tiles: a container on the plan's mining tile and
    /// a link on its controller link tile are kept; a container and a link on a tile the plan gives
    /// neither, and a link on the plan's other mining tile (its container's, not a link's), are not.
    /// Uses: PLANNER_MMO, `kind_rank` (layout::KINDS: the facts' order).
    #[test]
    fn a_plans_entry_keeps_only_the_standing_it_holds() {
        use StructureType::{Container, Link};
        let a = spawn_tile();
        let name = room("W1N1");
        let layout = flood_plan(&open_facts(vec![a]), a).expect("a plan on the anchor");
        let (m0, m1) = match layout.roles.mining[..] {
            [m0, m1] => (m0, m1),
            _ => panic!("a mining tile per source"),
        };
        let link = layout.roles.controller_link.expect("a controller link");
        let stray = (0..ROOM_AREA)
            .filter_map(Tile::from_index)
            .filter(|x| !x.is_edge())
            .find(|&x| !gives(&layout, Container, x) && !gives(&layout, Link, x))
            .expect("a tile the plan gives no container or link");
        assert!(gives(&layout, Container, m1) && !gives(&layout, Link, m1));
        let mut facts = open_facts(vec![a]);
        facts.standing = by_kind(vec![
            (Container, m0),
            (Container, stray),
            (Link, link),
            (Link, stray),
            (Link, m1),
        ]);
        let plan = Plan {
            layout,
            facts,
            anchor: a,
            ready_at: 0,
            steps: 0,
        };
        assert_eq!(
            plan.entry(name),
            Entry {
                room: name,
                anchor: a,
                adopted: by_kind(vec![(Container, m0), (Link, link)]),
            }
        );
    }

    /// The job's entry is the ready plan's: with a container built on the plan's mining tile, a
    /// container site on its other one and a container standing off the plan, the record's entry
    /// holds the two on the plan and not the stray (the plan gives no container there).
    /// Uses: PLANNER_MMO, MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level), CONTAINER_HITS
    /// and CONSTRUCTION_COST (the container and site rows).
    #[test]
    fn the_records_entry_leaves_out_a_container_off_the_plan() {
        use StructureType::Container;
        let a = spawn_tile();
        let name = room("W1N1");
        let layout = flood_plan(&open_facts(vec![a]), a).expect("a plan on the anchor");
        let (m0, m1) = match layout.roles.mining[..] {
            [m0, m1] => (m0, m1),
            _ => panic!("a mining tile per source"),
        };
        let stray = (0..ROOM_AREA)
            .filter_map(Tile::from_index)
            .filter(|x| !x.is_edge())
            .find(|&x| !gives(&layout, Container, x))
            .expect("a tile the plan gives no container");
        let mut scene = Scene::open("W1N1", &[a]);
        scene.containers = vec![m0, stray];
        scene.sites = vec![(Container, m1)];
        let mut w = World::new();
        w.tick(&[scene]);
        w.plan();
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        let mut standing = vec![(Container, m0), (Container, m1), (Container, stray)];
        standing.sort_unstable_by_key(|&(_, t)| t);
        assert_eq!(plan.facts.standing, standing);
        assert!(!gives(&plan.layout, Container, stray), "the scenario");
        let mut on_plan = vec![(Container, m0), (Container, m1)];
        on_plan.sort_unstable_by_key(|&(_, t)| t);
        let entry = plan.entry(name);
        assert_eq!(entry.adopted, on_plan);
        assert_eq!(w.planner.record().and_then(|r| r.get(name)), Some(&entry));
    }

    /// The record's adopted tiles (ADR-027): a container standing within `RECEIVER_RANGE` of the
    /// controller off the tile a fresh plan gives its container is adopted (ADR-026) and goes in
    /// the entry; after a reset with that container lost and not sited again, the record keeps it
    /// standing for the run, so the plan is the one the room was built from and the entry is
    /// unchanged (nothing to write). With an empty record the plan puts the controller container
    /// back where a fresh plan does.
    /// Uses: RECEIVER_RANGE, CONTROLLER_LINK_RANGE (the controller container's tiles), PLANNER_MMO,
    /// MOVEMENT_MMO, CONTROLLER_STRUCTURES (the room's level), CONTAINER_HITS (the container row),
    /// `persist::kind::PLANNER`, `record::SCHEMA`.
    #[test]
    fn the_records_adopted_container_keeps_the_plan_while_it_is_lost() {
        use crate::economy::{CONTROLLER_LINK_RANGE, RECEIVER_RANGE};
        use StructureType::Container;
        let a = spawn_tile();
        let name = room("W1N1");
        let fresh = flood_plan(&open_facts(vec![a]), a).expect("a plan on the anchor");
        let given = fresh
            .roles
            .controller_container
            .expect("a controller container");
        // A fresh plan tries the link range first, so a tile at the receiver range is a choice
        // only a standing container makes.
        const { assert!(CONTROLLER_LINK_RANGE < RECEIVER_RANGE) };
        let fixed: Vec<Tile> = source_tiles()
            .into_iter()
            .chain([controller_tile(), mineral_tile()])
            .collect();
        let off = (0..ROOM_AREA)
            .filter_map(Tile::from_index)
            .filter(|&t| t.range(controller_tile()) == RECEIVER_RANGE)
            .filter(|&t| t != given && !t.is_edge())
            .filter(|&t| fixed.iter().all(|&f| f.range(t) > 1))
            .find(|&t| fresh.items.iter().all(|i| i.tile != t))
            .expect("a free tile at the receiver range");
        let mut built_from = Scene::open("W1N1", &[a]);
        built_from.containers = vec![off];
        let mut w = World::new();
        w.tick(&[built_from]);
        w.plan();
        let (layout, ..) = w.ready(name);
        assert_eq!(layout.roles.controller_container, Some(off), "adopted");
        assert!(
            layout != fresh,
            "the scenario: the container moves the plan"
        );
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        let entry = plan.entry(name);
        assert_eq!(entry.adopted, [(Container, off)]);
        let mut rec = Record::default();
        rec.set(entry.clone());
        let lost = Scene::open("W1N1", &[a]);
        let mut w = World::with_record(&rec);
        w.tick(std::slice::from_ref(&lost));
        w.plan();
        let Status::Ready(plan) = w.planner.status(name) else {
            panic!("ready");
        };
        assert_eq!(plan.facts.standing, [(Container, off)]);
        assert!(plan.layout == layout, "the plan the room was built from");
        assert_eq!(plan.entry(name), entry);
        assert!(!w.planner.take_dirty(), "the same entry: nothing to write");
        // Without the record the container is gone from the facts, and so is its tile.
        let mut w = World::new();
        w.tick(&[lost]);
        w.plan();
        let (again, ..) = w.ready(name);
        assert!(again == fresh);
        assert_eq!(again.roles.controller_container, Some(given));
    }
}
