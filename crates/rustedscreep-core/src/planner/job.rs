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
//! A run owns a copy of its facts (the terrain is 2500 bytes), so nothing it borrows has to live
//! across ticks. Rooms are planned one at a time, in name order.

use rustedscreep_consts::{RoomName, StructureType};

use super::flood::{self, Params};
use super::layout::{Layout, PlanError, PlanInput};
use super::scratch::Scratch;
use super::validate::validate;
use crate::movement::{Movement, TerrainGrid, Tile};
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
    /// The tiles a plan may be anchored on: the standing spawns and the own spawn sites, in tile
    /// order (the run tries them from the last).
    pub anchors: Vec<Tile>,
    /// The standing containers and links and the own sites of both (ADR-026: the economy counts a
    /// site as its store or link already), in kind then tile order.
    pub standing: Vec<(StructureType, Tile)>,
    /// What stands in the room that a plan may hold: the own structures but the controller, the
    /// containers and the own sites, in kind then tile order (the anchor rule's measure).
    pub own: Vec<(StructureType, Tile)>,
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
        })
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
    /// The tile the plan was anchored on: a standing spawn, or an own spawn site.
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
    /// How many of `facts.anchors` were tried, from the last.
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
}

impl Planner {
    /// Starts a tick: the owned rooms with a spawn and read terrain, by name. A new one starts a
    /// run; a ready one where a spawn stands off its spawn tiles that was not standing when it was
    /// made starts again (a lost anchor keeps the plan); rooms no longer owned are dropped.
    pub fn begin(&mut self, snap: &Snapshot, movement: &Movement) {
        self.time = snap.tick.time;
        for r in &mut self.rooms {
            r.seen = false;
        }
        let rooms = &snap.rooms;
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
                    room.state = State::Ready(Box::new(Plan {
                        layout,
                        facts,
                        anchor,
                        ready_at: time,
                        steps,
                    }));
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
        Self {
            facts,
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
        let anchor = self
            .facts
            .anchors
            .len()
            .checked_sub(self.spawn.saturating_add(1))
            .and_then(|i| self.facts.anchors.get(i))
            .copied();
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
                // Nothing tried later can fit better, and a tie goes to the anchor tried first.
                if fit == (true, self.facts.own.len()) {
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
        if self.spawn < self.facts.anchors.len() {
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
    use crate::config::{MOVEMENT_MMO, PLANNER_MMO};
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
    /// `validate` alone: the anchors from the highest tile down; of the valid plans the one whose
    /// spawn tiles hold every standing spawn, then the one holding the most of `facts.own` on tiles
    /// of their type, the first tried on a tie; a plan holding everything ends the search.
    /// Uses: PLANNER_MMO.
    fn anchor_rule(facts: &Facts) -> Option<(Layout, Tile)> {
        let mut best: Option<(Layout, Tile, (bool, usize))> = None;
        let mut anchors = facts.anchors.clone();
        anchors.sort_unstable();
        for &sp in anchors.iter().rev() {
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
            if (all, held) == (true, facts.own.len()) {
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
        fn new() -> Self {
            Self {
                snap: Snapshot::default(),
                movement: Movement::default(),
                planner: Planner::default(),
                time: 0,
                raw: open_raw(),
            }
        }

        /// One tick: the snapshot of `scenes` (ids stable per room, kind and tile), movement's
        /// begin and first reads, then the planner's begin.
        /// Uses: CPU_TICK_LIMIT_MAX, CPU_BUCKET_MAX (a full bucket), SOURCE_ENERGY_CAPACITY,
        /// CONTAINER_HITS, CONSTRUCTION_COST (the object rows), MOVEMENT_MMO.
        fn tick(&mut self, scenes: &[Scene]) {
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
            self.planner.begin(&self.snap, &self.movement);
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
}
