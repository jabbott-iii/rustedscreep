//! The distance-transform and flood-fill candidate with min-cut ramparts (ADR-007 option b).
//!
//! A run does one phase per [`Run::step`]:
//! 1. Core. [`sites::reserve`], then the hub: among the tiles where the distance transform over
//!    open, not-kept-clear ground reaches [`PlannerConfig::hub_clearance`] (the core's own 3 × 3 where none
//!    does), the lowest [`AnchorCost`], then the larger transform value, then index. The core is
//!    the hub (walkable, under a road: the filler's tile) with an arm on each side: the storage,
//!    the hub link's tile (next to the storage, kept free for [`sites::links`]), a spawn and the
//!    terminal, and a road on each diagonal. With a standing spawn the core is fitted so its
//!    spawn arm is that spawn when it can be; else the spawn stays where it stands, on its own,
//!    and the core goes where a fresh room would put it.
//! 2. to 5. The shared steps: [`sites::containers`], [`sites::links`], [`sites::mineral`] and the
//!    roads to the mining tiles, the controller container and the mineral container
//!    ([`roads::road`]), plus one to a lone standing spawn.
//! 6. Fill. A flood outward from the hub, in steps along a road network grown with it: a tile next
//!    to the network joins it as a road when it lies on the diamond lattice through the hub's
//!    diagonals ([`lattice`]: every other tile is next to one, 5 structures to 3 roads), and
//!    takes the next structure of [`FILL_ORDER`] otherwise, so each structure has a road beside
//!    it, the nearest ones go first, and the base grows as a compact Chebyshev ball. A tile that
//!    may not take a structure (kept clear, the border rule, or one that would cut off an exit
//!    tile or a spawn's free neighbor) joins the network as a bridge instead. Then only the
//!    roads some structure is served by are kept, each from the earliest level a structure it
//!    serves stands.
//! 7. Labs. The 10-lab stamp ([`LAB_SOURCES`], [`LAB_OUTPUTS`], [`LAB_ROADS`]) in the free
//!    ground nearest the hub, joined to the network by a road.
//! 8. Ramparts. The minimum cut ([`defence::plan`]) for every protected structure, at
//!    [`defence::levels`]: the line from the first level ramparts are allowed, the rest over a
//!    structure from that structure's level. The line is constructed walls but for its gates
//!    ([`defence::gates`], ADR-033): ramparts stay where own creeps must pass.
//!
//! Every obstacle the run places stands next to ground that never takes one (a road, a
//! container or the hub) joined to the hub through more such ground. No placement of the fill or
//! the labs may cut off from the hub an exit side the bare room reaches (the standing spawn in
//! place) or an exit tile reached when the fill began, the controller's upgrade range, a spawn's
//! free neighbor that leads out (but a standing spawn given so, which the validator judges; one
//! the run broke before the fill ends it with `NoRoom("spawn exit")`) or anything planned before
//! it ([`Reach`]);
//! the core and the shared steps keep the room one passage around each placement
//! (`keeps_neighbours_connected`). So every level stays reached from the first spawn.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rustedscreep_consts::{ROOM_AREA, StructureType};

use super::board::Board;
use super::defence;
use super::layout::{GATES, Gate, Layout, MAX_LEVEL, PlanError, PlanInput};
use super::roads;
use super::rules::{RANGED_REACH, allowed, blocks, earliest_level, edge_tiles, tile_allows};
use super::score::AnchorCost;
use super::scratch::{Scratch, fatigue};
use super::sites;
use super::transform::distance_transform;
use crate::config::{PLANNER_MMO, PlannerConfig};
use crate::economy::RECEIVER_RANGE;
use crate::economy::placement::keeps_neighbours_connected;
use crate::movement::{Goal, Side, TerrainGrid, TerrainKind, Tile, UNREACHED};
use crate::scheduler::Step;

/// The order the fill hands out its slots, nearest the hub first, to the types the core and the
/// lab stamp do not hold (each up to its count at the top level, V234): the other spawns, then
/// the factory and power spawn by the storage and terminal, the towers near the middle of the
/// base (the most damage on the rampart line, V239), the extensions nearest the hub first (each
/// filled from the road beside it), and last the nuker and observer, which are rarely filled.
/// Rationale: spawns and extensions are refilled after every spawn, so they take the nearest
/// ground after the logistics core. Safe: any order of these types (a type left out is not
/// placed and fails the validator).
pub const FILL_ORDER: [StructureType; 7] = [
    StructureType::Spawn,
    StructureType::Factory,
    StructureType::PowerSpawn,
    StructureType::Tower,
    StructureType::Extension,
    StructureType::Nuker,
    StructureType::Observer,
];

/// The core's 3 × 3 must fit: the distance transform's value for a 3 × 3.
const CORE_FIT: u8 = 2;

/// The hub link's least range from a source: a mining tile is next to its source and the hub
/// link must not stand next to a mining tile (or it reads as a source link), so 1 + 1 + 1.
const HUB_LINK_GAP: u8 = 3;

/// The lattice period: lines of road every 4 tiles in both diagonal directions, which is the
/// widest spacing that leaves every other tile next to a road (see [`lattice`]).
const LATTICE: i16 = 4;

/// The arms of the core, orthogonal to the hub: north, east, south, west.
const ARMS: [(i8, i8); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];

/// The core's road tiles beside the hub (on the lattice, see [`lattice`]).
const DIAGONALS: [(i8, i8); 4] = [(1, -1), (1, 1), (-1, 1), (-1, -1)];

/// The lab stamp in its 4 × 4 box, by (x, y) from the box's top-left corner (the mirrored stamp
/// flips x): the two source labs, within
/// [`LAB_REACTION_RANGE`](rustedscreep_consts::LAB_REACTION_RANGE) of every other lab (V240).
const LAB_SOURCES: [(u8, u8); 2] = [(1, 1), (2, 2)];

/// The stamp's other 8 labs.
const LAB_OUTPUTS: [(u8, u8); 8] = [
    (1, 0),
    (2, 0),
    (0, 1),
    (3, 1),
    (0, 2),
    (3, 2),
    (1, 3),
    (2, 3),
];

/// The stamp's road, on the box's anti-diagonal: every lab is next to it, so a creep can stand
/// next to any lab to be boosted (range 1, V240) and haulers reach them all.
const LAB_ROADS: [(u8, u8); 4] = [(3, 0), (2, 1), (1, 2), (0, 3)];

/// The side of the lab stamp's box.
const LAB_BOX: u8 = 4;

fn tiles() -> impl Iterator<Item = Tile> {
    (0..ROOM_AREA).filter_map(Tile::from_index)
}

/// `t` moved by `(dx, dy)`, or `None` outside the room.
fn at(t: Tile, (dx, dy): (i8, i8)) -> Option<Tile> {
    Tile::new(t.x().checked_add_signed(dx)?, t.y().checked_add_signed(dy)?)
}

/// Whether `t` is on the road lattice anchored at `hub`: the diagonal lines through the hub's
/// diagonal neighbors, every [`LATTICE`] tiles. The tiles off it come in plus shapes of 5, each
/// next to the lattice (the hub's own plus is the core), so every structure in the fill has a road
/// beside it, and the lines cross so the roads join up.
fn lattice(hub: Tile, t: Tile) -> bool {
    let dx = i16::from(t.x()) - i16::from(hub.x());
    let dy = i16::from(t.y()) - i16::from(hub.y());
    let half = LATTICE / 2;
    (dx + dy).rem_euclid(LATTICE) == half || (dx - dy).rem_euclid(LATTICE) == half
}

/// The core's arm assignments, as indices into [`ARMS`]: storage, hub link, spawn, terminal, with
/// the storage and the hub link on neighboring arms (so they touch diagonally).
fn assignments() -> Vec<[usize; 4]> {
    let mut out = Vec::new();
    for storage in 0..ARMS.len() {
        for turn in [1, ARMS.len() - 1] {
            let link = (storage + turn) % ARMS.len();
            let rest: Vec<usize> = (0..ARMS.len())
                .filter(|&a| a != storage && a != link)
                .collect();
            if let [a, b] = rest[..] {
                out.push([storage, link, a, b]);
                out.push([storage, link, b, a]);
            }
        }
    }
    out
}

/// A placed core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Core {
    hub: Tile,
    storage: Tile,
    link: Tile,
    spawn: Tile,
    terminal: Tile,
}

impl Core {
    /// The core around `hub` with the arms of `a` ([`assignments`]), when it fits the board: the
    /// arms take a structure, the hub and the diagonals a road, and the hub link is far enough
    /// from the sources.
    fn fit(board: &Board, input: &PlanInput<'_>, hub: Tile, a: [usize; 4]) -> Option<Self> {
        let arm = |i: usize| ARMS.get(i).and_then(|&o| at(hub, o));
        let [storage, link, spawn, terminal] = a;
        let core = Self {
            hub,
            storage: arm(storage)?,
            link: arm(link)?,
            spawn: arm(spawn)?,
            terminal: arm(terminal)?,
        };
        // Every costed obstacle has the same tile rules (V237), so the storage's stand for all.
        let arms = [core.storage, core.link, core.spawn, core.terminal];
        let fits = arms
            .iter()
            .all(|&t| board.can_place(StructureType::Storage, t))
            && board.can_place(StructureType::Road, hub)
            && DIAGONALS
                .iter()
                .all(|&d| at(hub, d).is_some_and(|t| board.can_place(StructureType::Road, t)))
            && input
                .sources
                .iter()
                .all(|s| s.range(core.link) >= HUB_LINK_GAP);
        fits.then_some(core)
    }
}

/// A flood from the hub over walkable ground: which sides' exits it reaches (the validator's
/// rule: an open edge tile on that side next to a reached tile).
struct Reach {
    seen: Vec<bool>,
    stack: Vec<Tile>,
}

impl Reach {
    fn new() -> Self {
        Self {
            seen: vec![false; ROOM_AREA],
            stack: Vec::new(),
        }
    }

    fn reached(&self, t: Tile) -> bool {
        self.seen.get(t.index()).copied().unwrap_or(false)
    }

    fn mark(&mut self, t: Tile) -> bool {
        match self.seen.get_mut(t.index()) {
            Some(c) if !*c => {
                *c = true;
                true
            }
            _ => false,
        }
    }

    /// Floods from `start` over the tiles `walk` accepts (edge tiles never). Returns the sides
    /// whose exits it reached, indexed as [`Side::ALL`].
    fn run(
        &mut self,
        start: Tile,
        terrain: &TerrainGrid,
        walk: impl Fn(Tile) -> bool,
    ) -> [bool; 4] {
        self.seen.iter_mut().for_each(|c| *c = false);
        self.stack.clear();
        let mut sides = [false; 4];
        if !walk(start) {
            return sides;
        }
        self.mark(start);
        self.stack.push(start);
        while let Some(u) = self.stack.pop() {
            for (_, n) in u.neighbors() {
                if n.is_edge() {
                    if terrain.kind(n) != TerrainKind::Wall
                        && let Some(cell) = n
                            .side()
                            .and_then(|s| Side::ALL.iter().position(|&a| a == s))
                            .and_then(|i| sides.get_mut(i))
                    {
                        *cell = true;
                    }
                    continue;
                }
                if walk(n) && self.mark(n) {
                    self.stack.push(n);
                }
            }
        }
        sides
    }

    /// How many exits (edge tiles on a side that are not walls; corners have no side) are next
    /// to the ground the last [`Reach::run`] reached: the validator's reading of an open exit,
    /// tile by tile.
    fn exit_tiles(&self, terrain: &TerrainGrid) -> u16 {
        let n = edge_tiles()
            .filter(|t| {
                t.side().is_some()
                    && terrain.kind(*t) != TerrainKind::Wall
                    && t.neighbors().any(|(_, n)| self.reached(n))
            })
            .count();
        u16::try_from(n).unwrap_or(u16::MAX)
    }
}

/// Whether `now` still reaches every side `need` does.
fn keeps_sides(need: [bool; 4], now: [bool; 4]) -> bool {
    need.iter().zip(now.iter()).all(|(&n, &h)| !n || h)
}

/// A tile in the lab stamp's box at `o`, mirrored in x when `mirror`.
fn stamp_tile(o: Tile, (bx, by): (u8, u8), mirror: bool) -> Option<Tile> {
    let bx = if mirror {
        (LAB_BOX - 1).checked_sub(bx)?
    } else {
        bx
    };
    Tile::new(o.x().checked_add(bx)?, o.y().checked_add(by)?)
}

/// The stamp at `o`: its labs (the two sources first) and its roads.
fn stamp(o: Tile, mirror: bool) -> Option<([Tile; 10], [Tile; 4])> {
    let mut labs = [o; 10];
    for (slot, &p) in labs
        .iter_mut()
        .zip(LAB_SOURCES.iter().chain(LAB_OUTPUTS.iter()))
    {
        *slot = stamp_tile(o, p, mirror)?;
    }
    let mut roads = [o; 4];
    for (slot, &p) in roads.iter_mut().zip(LAB_ROADS.iter()) {
        *slot = stamp_tile(o, p, mirror)?;
    }
    Some((labs, roads))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Core,
    Containers,
    Links,
    Mineral,
    Roads,
    Fill,
    Labs,
    Ramparts,
    Done,
}

impl Phase {
    fn next(self) -> Self {
        match self {
            Self::Core => Self::Containers,
            Self::Containers => Self::Links,
            Self::Links => Self::Mineral,
            Self::Mineral => Self::Roads,
            Self::Roads => Self::Fill,
            Self::Fill => Self::Labs,
            Self::Labs => Self::Ramparts,
            Self::Ramparts | Self::Done => Self::Done,
        }
    }
}

/// What the fill decided for a tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    /// Not decided yet.
    Open,
    /// A road planned before the fill (the core's, the shared ones).
    Road,
    /// A road the fill may keep (on the lattice, or a bridge).
    Grown,
    /// A structure, or ground that takes nothing.
    Taken,
}

const NO_DIST: u16 = u16::MAX;

/// How a hub ranks: its [`AnchorCost`], the larger transform value, its index.
type HubKey = (u32, Reverse<u8>, usize);

/// How a core on a standing spawn ranks: its hub's [`HubKey`], then the storage arm's
/// [`AnchorCost`] and the assignment's rank.
type CoreKey = (u32, Reverse<u8>, usize, u32, usize);

/// The fill's working state, built and dropped within its step.
struct Grow {
    hub: Tile,
    /// Steps along the network from the hub: a network tile's own, an offered tile's through its
    /// parent (plus [`Params::exposed_penalty`] for a slot in reach of the exits).
    dist: Vec<u16>,
    /// The network tile an offered tile was reached from: a road's way back to the hub, a
    /// structure's road beside it.
    parent: Vec<Option<Tile>>,
    mark: Vec<Mark>,
    exposed: Vec<bool>,
    /// [`Params::exposed_penalty`].
    penalty: u16,
    heap: BinaryHeap<Reverse<(u16, usize)>>,
}

impl Grow {
    /// The network so far (every planned road, joined to the hub through roads), its distances,
    /// and its neighbors offered.
    fn new(board: &Board, input: &PlanInput<'_>, hub: Tile, penalty: u16) -> Self {
        let mut g = Self {
            hub,
            dist: vec![NO_DIST; ROOM_AREA],
            parent: vec![None; ROOM_AREA],
            mark: vec![Mark::Open; ROOM_AREA],
            exposed: exposed(input),
            penalty,
            heap: BinaryHeap::new(),
        };
        g.set_dist(hub, 0);
        g.set_mark(hub, Mark::Road);
        let mut order = vec![hub];
        let mut read = 0;
        while let Some(&u) = order.get(read) {
            read += 1;
            let d = g.dist(u).saturating_add(1);
            for (_, n) in u.neighbors() {
                if board.has_road(n) && g.mark(n) == Mark::Open {
                    g.set_mark(n, Mark::Road);
                    g.set_dist(n, d);
                    if let Some(c) = g.parent.get_mut(n.index()) {
                        *c = Some(u);
                    }
                    order.push(n);
                }
            }
        }
        for &u in &order {
            g.offer(board, u);
        }
        g
    }

    fn dist(&self, t: Tile) -> u16 {
        self.dist.get(t.index()).copied().unwrap_or(NO_DIST)
    }

    fn set_dist(&mut self, t: Tile, d: u16) {
        if let Some(c) = self.dist.get_mut(t.index()) {
            *c = d;
        }
    }

    fn mark(&self, t: Tile) -> Mark {
        self.mark.get(t.index()).copied().unwrap_or(Mark::Taken)
    }

    fn set_mark(&mut self, t: Tile, m: Mark) {
        if let Some(c) = self.mark.get_mut(t.index()) {
            *c = m;
        }
    }

    fn parent(&self, t: Tile) -> Option<Tile> {
        self.parent.get(t.index()).copied().flatten()
    }

    /// Offers the undecided walkable neighbors of the network tile `u`, one step on (a slot in
    /// reach of the exits [`Params::exposed_penalty`] steps more).
    fn offer(&mut self, board: &Board, u: Tile) {
        let p = self.dist(u).saturating_add(1);
        for (_, v) in u.neighbors() {
            if self.mark(v) != Mark::Open || !board.walkable(v) {
                continue;
            }
            let slot = !lattice(self.hub, v) && board.can_place(StructureType::Extension, v);
            let exposed = slot && self.exposed.get(v.index()).copied().unwrap_or(false);
            let d = if exposed {
                p.saturating_add(self.penalty)
            } else {
                p
            };
            if d < self.dist(v) {
                self.set_dist(v, d);
                if let Some(c) = self.parent.get_mut(v.index()) {
                    *c = Some(u);
                }
                self.heap.push(Reverse((d, v.index())));
            }
        }
    }

    /// The roads some structure of `placed` (tile, level, parent, gate) is served by, with the
    /// earliest level each gate's structures it serves belong from, in tile order: the grown roads
    /// kept, and the roads planned before the fill on their way back to the hub (a road planned
    /// for one gate, the mineral's, may serve another's structures too, so it is needed for them
    /// from their level). The farthest structures go first; a structure next to a road already
    /// kept uses it, else its parent's way back.
    fn kept_roads(&self, placed: &[(Tile, u8, Tile, Gate)]) -> Vec<(Tile, [u8; GATES])> {
        let mut level = vec![[u8::MAX; GATES]; ROOM_AREA];
        let kept = |v: &[[u8; GATES]], t: Tile| {
            v.get(t.index())
                .is_some_and(|l| l.iter().any(|&x| x != u8::MAX))
        };
        let lvl = |v: &[[u8; GATES]], t: Tile, g: usize| {
            v.get(t.index())
                .and_then(|l| l.get(g))
                .copied()
                .unwrap_or(u8::MAX)
        };
        for &(t, l, access, gate) in placed.iter().rev() {
            let g = gate.index();
            let served = t
                .neighbors()
                .map(|(_, n)| n)
                .filter(|&n| match self.mark(n) {
                    Mark::Road => true,
                    Mark::Grown => kept(&level, n),
                    _ => false,
                })
                .min_by_key(|&n| (self.dist(n), n.index()));
            let mut r = served.unwrap_or(access);
            // Per gate: a road on the way already needed by that gate from `l` or earlier has
            // its way to the hub needed from then too. The network's roads have their way back
            // to the hub as parents ([`Grow::new`]); the hub has none.
            while matches!(self.mark(r), Mark::Grown | Mark::Road) && lvl(&level, r, g) > l {
                if let Some(c) = level.get_mut(r.index()).and_then(|x| x.get_mut(g)) {
                    *c = l;
                }
                match self.parent(r) {
                    Some(up) => r = up,
                    None => break,
                }
            }
        }
        tiles()
            .filter_map(|t| {
                let from = level.get(t.index()).copied()?;
                (matches!(self.mark(t), Mark::Grown | Mark::Road) && kept(&level, t))
                    .then_some((t, from))
            })
            .collect()
    }
}

/// The tiles within ranged reach (V243) of ground no rampart can close off: the walkable edge
/// tiles, where hostile creeps enter, and the walkable tiles the border rule keeps ramparts off
/// (V237; the cut's sources and fixed tiles, [`defence::plan`]). A protected structure on one
/// needs a rampart of its own.
///
/// Uses: [`RANGED_REACH`], the site rule through [`tile_allows`].
fn exposed(input: &PlanInput<'_>) -> Vec<bool> {
    let terrain = input.terrain;
    let natural =
        |t: Tile| t == input.controller || input.sources.contains(&t) || input.mineral == Some(t);
    let mut out = vec![false; ROOM_AREA];
    let r = i8::try_from(RANGED_REACH).unwrap_or(i8::MAX);
    for h in tiles() {
        if terrain.kind(h) == TerrainKind::Wall || natural(h) {
            continue;
        }
        if !h.is_edge() && tile_allows(terrain, StructureType::Rampart, h) {
            continue;
        }
        for dx in -r..=r {
            for dy in -r..=r {
                if let Some(c) = at(h, (dx, dy)).and_then(|t| out.get_mut(t.index())) {
                    *c = true;
                }
            }
        }
    }
    out
}

/// The fill's tunables, from [`PlannerConfig`] (ADR-007's measurements used `PLANNER_MMO`'s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// [`PlannerConfig::hub_clearance`].
    pub hub_clearance: u8,
    /// [`PlannerConfig::exposed_penalty`].
    pub exposed_penalty: u16,
}

impl Params {
    pub const fn from_config(cfg: &PlannerConfig) -> Self {
        Self {
            hub_clearance: cfg.hub_clearance,
            exposed_penalty: cfg.exposed_penalty as u16,
        }
    }
}

impl Default for Params {
    /// The official MMO's values ([`PLANNER_MMO`]).
    fn default() -> Self {
        Self::from_config(&PLANNER_MMO)
    }
}

/// One planning run for a room, a phase per [`Run::step`]. It holds no borrow of its input, so a
/// job can keep it across ticks; every step must be given the input the run was made from.
pub struct Run {
    params: Params,
    /// The input's controller (the exits check keeps its upgrade range reached).
    controller: Tile,
    board: Board,
    phase: Phase,
    core: Option<Core>,
    /// The sides of the room whose exits the bare room reaches from the hub, the standing spawn in
    /// place.
    sides: [bool; 4],
    /// The exit tiles the hub reached when the fill began: the fill and the labs keep every one.
    exits: u16,
    /// A standing spawn that breaks the spawn rule as given ([`Board::standing_spawn_free`]): the
    /// validator judges it, the fill's checks leave it out.
    exempt: Vec<Tile>,
}

impl Run {
    /// A run with the official MMO's tunables ([`Params::default`]).
    pub fn new(input: &PlanInput<'_>) -> Self {
        Self::with(input, Params::default())
    }

    pub fn with(input: &PlanInput<'_>, params: Params) -> Self {
        Self {
            params,
            controller: input.controller,
            board: Board::new(input),
            phase: Phase::Core,
            core: None,
            sides: [false; 4],
            exits: 0,
            exempt: Vec::new(),
        }
    }

    /// Does the next phase. `Step::Done` once the plan is complete; then [`Run::finish`].
    pub fn step(&mut self, input: &PlanInput<'_>, s: &mut Scratch) -> Result<Step, PlanError> {
        match self.phase {
            Phase::Core => self.place_core(input, s)?,
            Phase::Containers => {
                let hub = self.core()?.hub;
                sites::containers(&mut self.board, input, hub, s)?;
            }
            Phase::Links => {
                let core = self.core()?;
                sites::links(&mut self.board, input, core.hub, core.link, s)?;
            }
            Phase::Mineral => {
                let hub = self.core()?.hub;
                sites::mineral(&mut self.board, input, hub, s)?;
            }
            Phase::Roads => self.shared_roads(input, s)?,
            Phase::Fill => self.fill(input, s)?,
            Phase::Labs => self.labs(input, s)?,
            Phase::Ramparts => self.ramparts(input, s)?,
            Phase::Done => return Ok(Step::Done),
        }
        self.phase = self.phase.next();
        Ok(if self.phase == Phase::Done {
            Step::Done
        } else {
            Step::More
        })
    }

    /// Hands the run the room's standing containers and links (ADR-026), before its first step:
    /// [`sites`] takes a standing one for a role where it is as good as the best free tile by the
    /// role's own rules, so the miner, the haulers and the upgraders keep their tiles.
    pub fn adopt(&mut self, standing: &[(StructureType, Tile)]) {
        self.board.set_standing(standing);
    }

    /// The plan, once a step returned `Step::Done`.
    pub fn finish(self) -> Layout {
        self.board.into_layout()
    }

    /// The phase the next [`Run::step`] does (for timings and logs).
    pub fn phase(&self) -> &'static str {
        match self.phase {
            Phase::Core => "core",
            Phase::Containers => "containers",
            Phase::Links => "links",
            Phase::Mineral => "mineral",
            Phase::Roads => "roads",
            Phase::Fill => "fill",
            Phase::Labs => "labs",
            Phase::Ramparts => "ramparts",
            Phase::Done => "done",
        }
    }

    fn core(&self) -> Result<Core, PlanError> {
        self.core.ok_or(PlanError::NoRoom("core"))
    }

    /// The first level that builds (the first extension's), for the roads every base needs.
    fn road_level() -> Result<u8, PlanError> {
        earliest_level(StructureType::Extension, 0).ok_or(PlanError::NoRoom("road level"))
    }

    /// Phase 1: the economy's ground, the hub and the core.
    fn place_core(&mut self, input: &PlanInput<'_>, s: &mut Scratch) -> Result<(), PlanError> {
        sites::reserve(&mut self.board, input);
        if let Some(t) = input.spawn
            && !self
                .board
                .standing_spawn_free(t, &sites::forced_tiles(&self.board, input.sources, Some(t)))
        {
            self.exempt.push(t);
        }
        let anchor = AnchorCost::new(input);
        let mut core = None;
        if let Some(t) = input.spawn {
            distance_transform(|x| self.board.open(x) && !self.board.is_clear(x), &mut s.dt);
            core = self.standing_core(input, t, &anchor, s);
            if core.is_none() {
                // The standing spawn cannot be a spawn arm: it stays on its own.
                self.board
                    .place_role(StructureType::Spawn, t)
                    .ok_or(PlanError::BadInput("standing spawn"))?;
            }
        }
        let core = match core {
            Some(c) => c,
            None => {
                distance_transform(|x| self.board.open(x) && !self.board.is_clear(x), &mut s.dt);
                self.fresh_core(input, &anchor, s)
                    .ok_or(PlanError::NoRoom("core"))?
            }
        };
        let level = Self::road_level()?;
        let b = &mut self.board;
        // The spawn first: in a fresh room or on a standing spawn it is the level-1 spawn.
        b.place(StructureType::Spawn, core.spawn)
            .ok_or(PlanError::NoRoom("core spawn"))?;
        b.place(StructureType::Storage, core.storage)
            .ok_or(PlanError::NoRoom("storage"))?;
        b.place(StructureType::Terminal, core.terminal)
            .ok_or(PlanError::NoRoom("terminal"))?;
        for t in std::iter::once(core.hub).chain(DIAGONALS.iter().filter_map(|&d| at(core.hub, d)))
        {
            if !b.place_road(t, level, Gate::Base) {
                return Err(PlanError::NoRoom("core road"));
            }
        }
        b.roles.hub = Some(core.hub);
        b.roles.storage = Some(core.storage);
        self.core = Some(core);
        let terrain = input.terrain;
        let natural = |t: Tile| {
            t == input.controller || input.sources.contains(&t) || input.mineral == Some(t)
        };
        // The standing spawn is an obstacle whatever the plan: the sides behind it are not the
        // plan's to keep.
        let bare = |t: Tile| {
            !t.is_edge()
                && terrain.kind(t) != TerrainKind::Wall
                && !natural(t)
                && Some(t) != input.spawn
        };
        self.sides = Reach::new().run(core.hub, terrain, bare);
        Ok(())
    }

    /// The core with its spawn arm on the standing spawn `t`, if one fits.
    fn standing_core(
        &self,
        input: &PlanInput<'_>,
        t: Tile,
        anchor: &AnchorCost,
        s: &Scratch,
    ) -> Option<Core> {
        let dt = |x: Tile| s.dt.get(x.index()).copied().unwrap_or(0);
        let mut best: Option<(CoreKey, Core)> = None;
        for (ai, &(dx, dy)) in ARMS.iter().enumerate() {
            let Some(hub) = at(t, (-dx, -dy)) else {
                continue;
            };
            if dt(hub) < CORE_FIT {
                continue;
            }
            let Some(cost) = anchor.cost(hub) else {
                continue;
            };
            for (rank, a) in assignments().into_iter().enumerate() {
                if a.get(2) != Some(&ai) {
                    continue;
                }
                let Some(core) = Core::fit(&self.board, input, hub, a) else {
                    continue;
                };
                let key = (
                    cost,
                    Reverse(dt(hub)),
                    hub.index(),
                    anchor.cost(core.storage).unwrap_or(u32::MAX),
                    rank,
                );
                if best.as_ref().is_none_or(|(k, _)| key < *k) {
                    best = Some((key, core));
                }
            }
        }
        best.map(|(_, c)| c)
    }

    /// The core of a fresh room: the hub by [`AnchorCost`], then the larger transform value, then
    /// index; its arms with the storage, then the spawn, nearest the room's targets.
    fn fresh_core(&self, input: &PlanInput<'_>, anchor: &AnchorCost, s: &Scratch) -> Option<Core> {
        let all = assignments();
        let dt = |x: Tile| s.dt.get(x.index()).copied().unwrap_or(0);
        for need in [self.params.hub_clearance, CORE_FIT] {
            let mut best: Option<(HubKey, Tile)> = None;
            for hub in tiles() {
                let d = dt(hub);
                if d < need {
                    continue;
                }
                let Some(cost) = anchor.cost(hub) else {
                    continue;
                };
                let key = (cost, Reverse(d), hub.index());
                if best.as_ref().is_some_and(|(k, _)| *k <= key) {
                    continue;
                }
                if all
                    .iter()
                    .any(|&a| Core::fit(&self.board, input, hub, a).is_some())
                {
                    best = Some((key, hub));
                }
            }
            if let Some((_, hub)) = best {
                let arm_cost = |t: Tile| anchor.cost(t).unwrap_or(u32::MAX);
                return all
                    .iter()
                    .enumerate()
                    .filter_map(|(rank, &a)| {
                        Core::fit(&self.board, input, hub, a).map(|c| (c, rank))
                    })
                    .min_by_key(|&(c, rank)| (arm_cost(c.storage), arm_cost(c.spawn), rank))
                    .map(|(c, _)| c);
            }
        }
        None
    }

    /// Phase 5: the roads every base needs, from the hub, and one to a lone standing spawn.
    fn shared_roads(&mut self, input: &PlanInput<'_>, s: &mut Scratch) -> Result<(), PlanError> {
        let hub = self.core()?.hub;
        let level = Self::road_level()?;
        let b = &mut self.board;
        for m in b.roles.mining.clone() {
            roads::road(b, hub, m, 0, level, Gate::Base, s)?;
        }
        let cc = b
            .roles
            .controller_container
            .ok_or(PlanError::NoRoom("controller container"))?;
        roads::road(b, hub, cc, 1, level, Gate::Base, s)?;
        if let Some(mc) = b.roles.mineral_container {
            let extractor = earliest_level(StructureType::Extractor, 0)
                .ok_or(PlanError::NoRoom("extractor level"))?;
            roads::road(b, hub, mc, 0, extractor, Gate::Late, s)?;
        }
        if let Some(t) = input.spawn
            && self.core.is_some_and(|c| c.spawn != t)
        {
            roads::road(b, hub, t, 1, level, Gate::Base, s)?;
        }
        Ok(())
    }

    /// Whether blocking `t` (and the `more` tiles; `t` a new spawn when `spawn`) keeps the room as
    /// the validator reads it from the hub: every side's exits, and every exit tile reached when
    /// the fill began (so no exit corridor is sealed off with cut ramparts in it that no creep
    /// could build), the controller's upgrade range, a free neighbor of every obstacle planned
    /// and of the new ones, every road and container planned, and the spawn rule for every spawn
    /// but the exempt ([`Board::spawn_free`]). At once when `t` is no spawn, is next to no exit
    /// and its free neighbors stay one group: nothing reached before is cut off, so only a spawn
    /// beside it can lose its last free neighbor that is no post ([`Board::leaves_spawn_exit`]).
    /// Else by a flood.
    ///
    /// Uses: `RECEIVER_RANGE` (upgrade range, V187).
    fn keeps_exits(&self, t: Tile, more: &[Tile], spawn: bool, reach: &mut Reach) -> bool {
        let board = &self.board;
        let blocked = |n: Tile| !board.walkable(n) || n == t || more.contains(&n);
        // A corner tile may stand next to exits (V237's corner rule): that takes the flood.
        let by_exit = t
            .neighbors()
            .any(|(_, n)| n.is_edge() && board.kind(n) != TerrainKind::Wall);
        if more.is_empty() && !spawn && !by_exit && keeps_neighbours_connected(t, blocked) {
            return board.leaves_spawn_exit(t, blocked);
        }
        let Ok(core) = self.core() else { return false };
        let sides = reach.run(core.hub, board.terrain(), |n| !blocked(n));
        if !keeps_sides(self.sides, sides) || reach.exit_tiles(board.terrain()) < self.exits {
            return false;
        }
        let spawns_ok = board
            .items()
            .iter()
            .filter(|i| i.kind == StructureType::Spawn && !self.exempt.contains(&i.tile))
            .map(|i| i.tile)
            .chain(spawn.then_some(t))
            .all(|sp| board.spawn_free(sp, Some(core.hub), |n| !blocked(n), |n| reach.reached(n)));
        if !spawns_ok {
            return false;
        }
        let near = |x: Tile| x.neighbors().any(|(_, n)| reach.reached(n));
        let controller = self.controller;
        let r = i8::try_from(RECEIVER_RANGE).unwrap_or(i8::MAX);
        let upgrade = (-r..=r)
            .flat_map(|dx| (-r..=r).map(move |dy| (dx, dy)))
            .filter_map(|d| at(controller, d))
            .any(|x| reach.reached(x));
        let new_ok = std::iter::once(t).chain(more.iter().copied()).all(near);
        let planned_ok = board.items().iter().all(|i| match i.kind {
            StructureType::Rampart | StructureType::Wall => true,
            StructureType::Extractor => near(i.tile),
            k if blocks(k) => near(i.tile),
            _ => reach.reached(i.tile),
        });
        upgrade && new_ok && planned_ok
    }

    /// Phase 6: the flood fill (see the module docs).
    fn fill(&mut self, input: &PlanInput<'_>, _s: &mut Scratch) -> Result<(), PlanError> {
        let hub = self.core()?.hub;
        let mut queue: Vec<StructureType> = Vec::new();
        for kind in FILL_ORDER {
            let want = allowed(kind, MAX_LEVEL).saturating_sub(self.board.count(kind));
            queue.extend(std::iter::repeat_n(kind, want as usize));
        }
        let mut g = Grow::new(&self.board, input, hub, self.params.exposed_penalty);
        let mut reach = Reach::new();
        let board = &self.board;
        reach.run(hub, board.terrain(), |n| board.walkable(n));
        let exits = reach.exit_tiles(board.terrain());
        // A spawn the run broke before the fill (not one given so, which is exempt) would make
        // every placement after it fail: say so at once.
        let broken = board
            .items()
            .iter()
            .filter(|i| i.kind == StructureType::Spawn && !self.exempt.contains(&i.tile))
            .any(|i| {
                !board.spawn_free(
                    i.tile,
                    Some(hub),
                    |n| board.walkable(n),
                    |n| reach.reached(n),
                )
            });
        if broken {
            return Err(PlanError::NoRoom("spawn exit"));
        }
        self.exits = exits;
        let mut placed: Vec<(Tile, u8, Tile, Gate)> = Vec::new();
        let mut next = 0;
        while let Some(&kind) = queue.get(next) {
            let Some(Reverse((p, i))) = g.heap.pop() else {
                return Err(PlanError::NoRoom("fill"));
            };
            let Some(t) = Tile::from_index(i) else {
                continue;
            };
            if g.mark(t) != Mark::Open || g.dist(t) != p {
                continue;
            }
            let road_ok = self.board.can_place(StructureType::Road, t);
            // A lattice tile that takes no road takes no structure either (V237).
            if !lattice(hub, t)
                && self.board.can_place(kind, t)
                && self.keeps_exits(t, &[], kind == StructureType::Spawn, &mut reach)
            {
                let level = self
                    .board
                    .place(kind, t)
                    .ok_or(PlanError::NoRoom("fill slot"))?;
                g.set_mark(t, Mark::Taken);
                let access = g.parent(t).ok_or(PlanError::NoRoom("fill access"))?;
                placed.push((t, level, access, Gate::of(kind, t, &self.board.roles)));
                next += 1;
            } else if road_ok {
                g.set_mark(t, Mark::Grown);
                g.offer(&self.board, t);
            } else {
                g.set_mark(t, Mark::Taken);
            }
        }
        for (t, from) in g.kept_roads(&placed) {
            let Some((first, gate)) = Gate::ALL
                .into_iter()
                .filter_map(|g| from.get(g.index()).map(|&l| (l, g)))
                .min()
            else {
                continue;
            };
            // A road planned before the fill is only told what it serves now.
            if !self.board.has_road(t) && !self.board.place_road(t, first, gate) {
                return Err(PlanError::NoRoom("fill road"));
            }
            for g in Gate::ALL {
                if let Some(&l) = from.get(g.index())
                    && l != u8::MAX
                {
                    self.board.lower_road(t, l, g);
                }
            }
        }
        Ok(())
    }

    /// Phase 7: the lab stamp nearest the hub (counting [`Params::exposed_penalty`] per lab in reach of
    /// the exits), with a road to it.
    fn labs(&mut self, input: &PlanInput<'_>, s: &mut Scratch) -> Result<(), PlanError> {
        let hub = self.core()?.hub;
        self.board.cost_grid(&mut s.grid, fatigue());
        s.field.compute(
            &s.grid,
            Goal {
                tile: hub,
                range: 0,
            },
            &mut s.dial,
        );
        let exposed = exposed(input);
        let mut candidates: Vec<(u16, usize, bool)> = Vec::new();
        for o in tiles() {
            for mirror in [false, true] {
                let Some((labs, roads)) = stamp(o, mirror) else {
                    continue;
                };
                let b = &self.board;
                let fits = labs.iter().all(|&l| b.can_place(StructureType::Lab, l))
                    && roads
                        .iter()
                        .all(|&r| b.has_road(r) || b.can_place(StructureType::Road, r));
                if !fits {
                    continue;
                }
                let d = roads
                    .iter()
                    .map(|&r| s.field.distance(r))
                    .min()
                    .unwrap_or(UNREACHED);
                if d != UNREACHED {
                    let open_to_exits = labs
                        .iter()
                        .filter(|l| exposed.get(l.index()).copied().unwrap_or(false))
                        .count() as u16;
                    let key =
                        d.saturating_add(self.params.exposed_penalty.saturating_mul(open_to_exits));
                    candidates.push((key, o.index(), mirror));
                }
            }
        }
        candidates.sort_unstable();
        let mut reach = Reach::new();
        for &(_, oi, mirror) in &candidates {
            let Some((labs, roads)) = Tile::from_index(oi).and_then(|o| stamp(o, mirror)) else {
                continue;
            };
            let Some(first) = labs.first().copied() else {
                continue;
            };
            if !self.keeps_exits(first, &labs, false, &mut reach) {
                continue;
            }
            // The stamp's road tile nearest the hub that the hub still reaches with the labs in.
            let Some(entry) = roads
                .iter()
                .copied()
                .filter(|&r| reach.reached(r))
                .min_by_key(|&r| (s.field.distance(r), r.index()))
            else {
                continue;
            };
            let mut level = MAX_LEVEL;
            for &l in &labs {
                level = level.min(
                    self.board
                        .place(StructureType::Lab, l)
                        .ok_or(PlanError::NoRoom("lab"))?,
                );
            }
            for &r in &roads {
                if self.board.has_road(r) {
                    self.board.lower_road(r, level, Gate::Late);
                } else if !self.board.place_road(r, level, Gate::Late) {
                    return Err(PlanError::NoRoom("lab road"));
                }
            }
            roads::road(&mut self.board, hub, entry, 0, level, Gate::Late, s)?;
            return Ok(());
        }
        Err(PlanError::NoRoom("labs"))
    }

    /// Phase 8: the minimum cut, its line from the first level ramparts are allowed.
    fn ramparts(&mut self, input: &PlanInput<'_>, s: &mut Scratch) -> Result<(), PlanError> {
        let protect = defence::protected_tiles(&self.board);
        let plan = defence::plan(input, &protect, s)?;
        let level =
            earliest_level(StructureType::Rampart, 0).ok_or(PlanError::NoRoom("rampart level"))?;
        defence::apply(&mut self.board, input, &plan, level)?;
        Ok(())
    }
}

/// Every step at once, with the official MMO's tunables.
pub fn plan(input: &PlanInput<'_>, s: &mut Scratch) -> Result<Layout, PlanError> {
    plan_with(input, Params::default(), s)
}

/// Every step at once.
pub fn plan_with(
    input: &PlanInput<'_>,
    params: Params,
    s: &mut Scratch,
) -> Result<Layout, PlanError> {
    let mut run = Run::with(input, params);
    while run.step(input, s)? == Step::More {}
    Ok(run.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::validate;
    use rustedscreep_consts::{LAB_REACTION_RANGE, ROOM_SIZE, Terrain};

    fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).expect("in room")
    }

    fn terrain(walls: impl Fn(u8, u8) -> bool) -> TerrainGrid {
        let mut raw = vec![0u8; ROOM_AREA];
        for y in 0..ROOM_SIZE {
            for x in 0..ROOM_SIZE {
                if walls(x, y) {
                    raw[usize::from(y) * usize::from(ROOM_SIZE) + usize::from(x)] =
                        Terrain::Wall as u8;
                }
            }
        }
        TerrainGrid::from_row_major(&raw).expect("length")
    }

    /// V240: both source labs are within LAB_REACTION_RANGE of the other 8, every lab has a road
    /// beside it (boosting is range 1), the road is one 8-connected line, and nothing overlaps,
    /// in both orientations.
    #[test]
    fn the_lab_stamp_keeps_its_ranges() {
        let o = tile(20, 20);
        for mirror in [false, true] {
            let (labs, roads) = stamp(o, mirror).expect("in room");
            let total = StructureType::Lab.controller_structures(u32::from(MAX_LEVEL)) as usize;
            assert_eq!(labs.len(), total);
            let (sources, outputs) = labs.split_at(LAB_SOURCES.len());
            for s in sources {
                for o in outputs {
                    assert!(
                        u32::from(s.range(*o)) <= LAB_REACTION_RANGE,
                        "{mirror} {s:?} {o:?}"
                    );
                }
            }
            for l in &labs {
                assert!(roads.iter().any(|r| r.range(*l) == 1), "{mirror} {l:?}");
                assert!(!roads.contains(l));
            }
            let mut all: Vec<Tile> = labs.iter().chain(roads.iter()).copied().collect();
            all.sort();
            all.dedup();
            assert_eq!(all.len(), labs.len() + roads.len());
            assert!(roads.windows(2).all(|w| w[0].range(w[1]) == 1));
            assert!(
                all.iter()
                    .all(|t| t.x() < o.x() + LAB_BOX && t.y() < o.y() + LAB_BOX)
            );
        }
    }

    /// Every tile off the lattice is next to one on it, and the lattice is one 8-connected
    /// network, in an open stretch around the hub.
    #[test]
    fn the_lattice_serves_every_tile_and_joins_up() {
        let hub = tile(25, 25);
        let inner = |t: Tile| t.range(hub) <= 12;
        for t in tiles().filter(|&t| inner(t) && t.range(hub) <= 11) {
            if !lattice(hub, t) {
                assert!(t.neighbors().any(|(_, n)| lattice(hub, n)), "{t:?}");
            }
        }
        for d in DIAGONALS {
            assert!(lattice(hub, at(hub, d).expect("in room")));
        }
        assert!(!lattice(hub, hub));
        assert!(
            ARMS.iter()
                .all(|&a| !lattice(hub, at(hub, a).expect("in room")))
        );
        let net: Vec<Tile> = tiles().filter(|&t| inner(t) && lattice(hub, t)).collect();
        let mut seen = vec![false; ROOM_AREA];
        let mut stack = vec![net[0]];
        seen[net[0].index()] = true;
        while let Some(u) = stack.pop() {
            for (_, n) in u.neighbors() {
                if inner(n) && lattice(hub, n) && !seen[n.index()] {
                    seen[n.index()] = true;
                    stack.push(n);
                }
            }
        }
        assert!(net.iter().all(|t| seen[t.index()]));
        // Five off the lattice to three on it.
        let window: Vec<Tile> = tiles()
            .filter(|t| (10..18).contains(&t.x()) && (10..18).contains(&t.y()))
            .collect();
        let on = window.iter().filter(|&&t| lattice(hub, t)).count();
        assert_eq!(on * 8, window.len() * 3);
    }

    /// Each assignment puts the storage next to the hub link, all four arms next to the hub, and
    /// all of them on different arms; there are 16.
    #[test]
    fn core_assignments_keep_the_hub_roles() {
        let hub = tile(25, 25);
        let all = assignments();
        assert_eq!(all.len(), 16);
        for a in &all {
            let arms: Vec<Tile> = a
                .iter()
                .map(|&i| at(hub, ARMS[i]).expect("in room"))
                .collect();
            assert_eq!(arms[0].range(arms[1]), 1, "{a:?}");
            assert!(arms.iter().all(|t| t.range(hub) == 1));
            let mut sorted = a.to_vec();
            sorted.sort_unstable();
            assert_eq!(sorted, vec![0, 1, 2, 3]);
        }
        let mut uniq = all.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), all.len());
    }

    fn open_room() -> (TerrainGrid, Vec<Tile>) {
        let t = terrain(|x, y| {
            (x, y) == (10, 10) || (x, y) == (40, 12) || (x, y) == (25, 40) || (x, y) == (12, 38)
        });
        (t, vec![tile(10, 10), tile(40, 12)])
    }

    #[test]
    fn an_open_room_gets_a_valid_plan_fresh_and_on_a_standing_spawn() {
        let (t, sources) = open_room();
        let input = PlanInput {
            terrain: &t,
            controller: tile(25, 40),
            sources: &sources,
            mineral: Some(tile(12, 38)),
            spawn: None,
        };
        let layout = plan(&input, &mut Scratch::default()).expect("plan");
        let report = validate(&input, &layout);
        assert!(report.valid(), "{:?}", report.problems);
        let hub = layout.roles.hub.expect("hub");
        // A standing spawn that fits a spawn arm, and one that cannot (on the ground kept clear
        // for the controller's upgraders), which stays on its own with the core elsewhere.
        let kept_clear = tile(25, 40 - crate::economy::placement::CONTROLLER_CLEAR);
        for (spawn, arm) in [
            (at(hub, (2, 3)).expect("in room"), true),
            (kept_clear, false),
        ] {
            let standing = PlanInput {
                spawn: Some(spawn),
                ..input
            };
            let layout = plan(&standing, &mut Scratch::default()).expect("plan");
            let report = validate(&standing, &layout);
            assert!(report.valid(), "{spawn:?}: {:?}", report.problems);
            let core_hub = layout.roles.hub.expect("hub");
            let on_an_arm = core_hub.range(spawn) == 1
                && (core_hub.x() == spawn.x() || core_hub.y() == spawn.y());
            assert_eq!(on_an_arm, arm, "{spawn:?}");
            let first = layout
                .of(StructureType::Spawn)
                .min_by_key(|i| (i.level, i.tile.index()))
                .expect("spawn");
            assert_eq!((first.tile, first.level), (spawn, 1));
        }
    }

    /// The fill's structures each have a kept road (or the hub) beside them.
    /// A spawn the run broke before the fill (here every free neighbor made a post) ends the run
    /// at the fill's first step with `NoRoom("spawn exit")`, at once rather than after refusing
    /// every tile of the room; a standing spawn given so is exempt and planned around.
    #[test]
    fn a_spawn_broken_before_the_fill_stops_the_run() {
        let (t, sources) = open_room();
        let input = PlanInput {
            terrain: &t,
            controller: tile(25, 40),
            sources: &sources,
            mineral: None,
            spawn: None,
        };
        let mut s = Scratch::default();
        let mut run = Run::new(&input);
        while run.phase() != "fill" {
            assert_eq!(run.step(&input, &mut s), Ok(Step::More));
        }
        let spawn = run
            .board
            .items()
            .iter()
            .find(|i| i.kind == StructureType::Spawn)
            .map(|i| i.tile)
            .expect("the core's spawn");
        let free: Vec<Tile> = spawn
            .neighbors()
            .map(|(_, n)| n)
            .filter(|&n| run.board.walkable(n))
            .collect();
        run.board.roles.mining.extend(free);
        assert_eq!(
            run.step(&input, &mut s),
            Err(PlanError::NoRoom("spawn exit"))
        );
        assert!(run.exempt.is_empty());
    }

    #[test]
    fn every_fill_structure_has_a_road_beside_it() {
        let (t, sources) = open_room();
        let input = PlanInput {
            terrain: &t,
            controller: tile(25, 40),
            sources: &sources,
            mineral: None,
            spawn: None,
        };
        let layout = plan(&input, &mut Scratch::default()).expect("plan");
        let roads: Vec<Tile> = layout.of(StructureType::Road).map(|i| i.tile).collect();
        for kind in FILL_ORDER.iter().copied().chain([StructureType::Lab]) {
            for item in layout.of(kind) {
                assert!(
                    item.tile.neighbors().any(|(_, n)| roads.contains(&n)),
                    "{kind:?} {:?}",
                    item.tile
                );
            }
        }
    }
}
