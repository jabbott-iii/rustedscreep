//! The stamp/bunker candidate (ADR-007 option a): fixed stamps written as data, tried in their 8
//! orientations (rotations and mirrors) wherever every cell fits, with a rampart over every
//! protected structure (the bunker convention).
//!
//! The stamps:
//! - the core ([`CORE`]): the hub `H` where a filler stands between the storage, the hub-link tile
//!   `L` (kept free for [`sites::links`]), the terminal, the factory, the power spawn, the nuker and
//!   the first spawn; a road ring around that block, 2 more spawns and 6 towers on the next ring,
//!   and 60 extensions out to radius 5, served by diagonal and axis spokes that reach the stamp's
//!   corners. It is tried cut to radius 5, 4, 3 and 1 ([`CORE_RADII`]) and then as the minimal
//!   [`MINIMAL`] (the hub, the storage, the link tile and one spawn), so a cramped room still gets a
//!   hub; what a smaller core lacks comes from pods and single placements;
//! - the labs ([`LABS`]): 10 labs on a 4 x 4 with a diagonal road, the 2 source labs within
//!   `LAB_REACTION_RANGE` of the other 8 (V240);
//! - extension pods ([`PODS`]): a road row between two rows of extensions, 10 and 6 long.
//!
//! What no stamp holds (a small core's spawns, towers and hub structures, the observer, the
//! extensions no pod holds) goes on single tiles nearest the hub by path, each one keeping the
//! walkable ground around it one piece. Singles also compete with pods: a step takes the single
//! tiles that cost no more to stand beside than the best pod's extensions on average, less
//! [`POD_BIAS`], before it places the pod, so corridors beside a cramped hub fill before pods go
//! far away.
//!
//! The core's hub is chosen by [`AnchorCost`] (the shared score): the cheapest hub the minimal stamp
//! fits at, then the largest core cut whose hub costs at most [`CORE_COST_SLACK`] more. With a
//! standing spawn, the core is fitted so a spawn slot stands on it when a large enough core fits
//! there ([`SPAWN_CORE_RADIUS`]); else the spawn stays alone and the core goes where it fits.
//!
//! A run, one phase per [`Run::step`]: reserve the economy's ground; choose and place the core;
//! the containers, links and mineral sites; the economy's roads; the structures a small core
//! lacks; one extension step (a pod or singles) per call while extensions are missing; the labs;
//! the remaining singles; the ramparts. When the pods leave no room for the lab stamp, the run
//! starts over once with the labs before the pods.
//!
//! Every stamp placement is checked before it is made by a flood from the hub over the plan as it
//! would stand (the validator's reach rules at the top level): every obstacle keeps a reached
//! neighbour, every road and container stands on reached ground, the controller keeps reached
//! ground in upgrade range, every side of the room with an exit and every exit tile the bare room
//! reaches stay reached, and every spawn keeps the spawn rule (V174, [`Board::spawn_free`]; a
//! standing spawn that broke it before the plan began is left to the validator). Removing items
//! only adds ground, so a plan that passes at the top level passes at every lower level. A single
//! tile is checked locally (`single_ok`). Each rampart over a structure belongs from that
//! structure's level.

use rustedscreep_consts::{MOVE_COST_PLAIN, ROOM_AREA, StructureType};

use super::board::Board;
use super::defence;
use super::layout::{Layout, MAX_LEVEL, PlanError, PlanInput};
use super::roads;
use super::rules::{allowed, blocks, earliest_level, edge_tiles};
use super::score::AnchorCost;
use super::scratch::{Scratch, fatigue};
use super::sites;
use super::transform::distance_transform;
use crate::economy::placement::keeps_neighbours_connected;
use crate::economy::{CONTROLLER_LINK_RANGE, RECEIVER_RANGE};
use crate::movement::{Goal, Side, TerrainKind, Tile, UNREACHED};
use crate::scheduler::Step;

/// The radii the core template is cut to, largest first; the minimal stamp is tried after them.
///
/// Default `[5, 4, 3, 1]` (Chebyshev tiles from the hub). Radius 5 holds every spawn, tower and
/// extension (11 x 11), 4 holds 32 extensions (9 x 9), 3 holds the spawns and towers with 8
/// extensions (7 x 7), 1 the hub block alone (3 x 3). Safe range: any descending subset of 1 to 5;
/// dropping the small radii makes cramped rooms rely on the minimal stamp and single placements.
pub const CORE_RADII: [u8; 4] = [5, 4, 3, 1];

/// How much more a larger core's hub may cost than the cheapest hub the minimal stamp finds and
/// still be chosen (a larger core puts the extensions nearer the hub and on planned roads).
///
/// Default `12 * MOVE_COST_PLAIN`: the summed path cost to the sources and the controller at the
/// fatigue rates ([`AnchorCost`]), 12 plain steps over all the targets. Safe range 0 (always the
/// cheapest hub) to about `40 * MOVE_COST_PLAIN`; on the corpus, raising it moves the hub a few
/// steps from the sources and brings the extensions nearer (P4.2 sweep), and much larger lets a
/// big core pull the hub far from the sources.
///
/// Uses: `MOVE_COST_PLAIN` (V54).
pub const CORE_COST_SLACK: u32 = 12 * MOVE_COST_PLAIN;

/// The least radius a core built around a standing spawn may have when a larger core fits
/// elsewhere: below it the standing spawn is kept alone and the larger core is placed where it
/// fits (a core of radius 3 already holds every spawn and tower, so the extensions are what
/// moves out to pods).
///
/// Default 3 (Chebyshev tiles, one of [`CORE_RADII`]). Safe range 0 (always build around the
/// standing spawn when any core fits there) to 5 (only the full core).
pub const SPAWN_CORE_RADIUS: u8 = 3;

/// How much cheaper single tiles must be to stand beside than a pod's extensions on average
/// before they are taken instead of the pod: a pod brings its own road and keeps the ground
/// around it in rows, so it wins ties.
///
/// Default `2 * MOVE_COST_PLAIN` (path cost at the fatigue rates, 2 plain steps). Safe range 0
/// to about `10 * MOVE_COST_PLAIN`; large values place pods even where corridors beside the hub
/// would hold the extensions nearer.
///
/// Uses: `MOVE_COST_PLAIN` (V54).
pub const POD_BIAS: u32 = 2 * MOVE_COST_PLAIN;

/// How many of the best-scored placements of one stamp are flood-checked before the step gives
/// up on that stamp (and falls back to a smaller one).
///
/// Default 48 placements. Each check is one flood of the room (about `8 * ROOM_AREA` steps). Safe
/// range 1 to a few hundred; fewer makes a crowded room fall back sooner.
pub const MAX_REACH_CHECKS: usize = 48;

/// The core template around the hub `H` (rows from the top, y growing down). The letters: `H` the
/// hub, `L` the hub link's tile, `S` storage, `T` terminal, `F` factory, `P` power spawn, `N`
/// nuker, `1` to `3` spawn slots and `4` to `9` towers (each in build order), `e` extension, `r`
/// road, `.` nothing planned. Radius 1 is the hub block; radius 2 a road ring; radius 3 holds the
/// spawns `2` and `3` and the towers with 8 extensions; extensions fill radius 4 and 5, served by
/// the diagonal spokes (which reach the four corners) and the axis spokes. The `.` cells at
/// radius 5 have no road beside them.
pub const CORE: [&str; 11] = [
    "ree.eee.eer",
    "ereeereeere",
    "eer9er4eree",
    ".eerrrrr6e.",
    "ee3rPSTreee",
    "errrLHFrrre",
    "eeer1rNr2ee",
    ".e7rrrrree.",
    "eere5re8ree",
    "ereeereeere",
    "ree.eee.eer",
];

/// The smallest core: the hub, the storage and the hub-link tile next to both, the first spawn
/// next to the hub, and the road the hub is reached by.
pub const MINIMAL: [&str; 3] = [".S", "LH", "1r"];

/// The lab stamp: a diagonal road, the 2 source labs `s` beside it in the middle and the 8 other
/// labs `l` around them, each within `LAB_REACTION_RANGE` of both sources (V240) and next to the
/// road.
pub const LABS: [&str; 4] = ["rll.", "lrsl", "lsrl", ".llr"];

/// Extension pods, largest first: a road row between two rows of extensions, open at both ends.
pub const PODS: [&[&str]; 2] = [&["eeeee", "rrrrr", "eeeee"], &["eee", "rrr", "eee"]];

/// The structures a core holds one of, in the order they are placed (the build order: the
/// storage from RCL4, the terminal from 6, then the late game).
const UNIQUE: [StructureType; 5] = [
    StructureType::Storage,
    StructureType::Terminal,
    StructureType::Factory,
    StructureType::PowerSpawn,
    StructureType::Nuker,
];

/// The structures a small core lacks, placed singly near the hub before the extensions and labs.
const ESSENTIAL: [StructureType; 6] = [
    StructureType::Spawn,
    StructureType::Tower,
    StructureType::Terminal,
    StructureType::Factory,
    StructureType::PowerSpawn,
    StructureType::Nuker,
];

/// The structures single placements complete at the end (every required type a stamp or a shared
/// step may have left short, but the labs, which must keep their reaction range).
const LAST: [StructureType; 8] = [
    StructureType::Spawn,
    StructureType::Extension,
    StructureType::Tower,
    StructureType::Terminal,
    StructureType::Factory,
    StructureType::PowerSpawn,
    StructureType::Nuker,
    StructureType::Observer,
];

/// The core keeps this far from every source (Chebyshev, more than): a mining tile is next to its
/// source and a source link next to the mining tile, so within 2, and the core leaves that ground
/// to [`sites`]. The hub and its link tile are then never a mining tile or next to one.
const SOURCE_GAP: u8 = 2;

/// The 8 orientations: 4 rotations, then the same of the mirror image.
const ORIENTATIONS: u8 = 8;

/// What a stamp cell holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum What {
    /// `H`: the hub, walkable and free (the hub road goes here).
    Hub,
    /// `L`: the hub link's tile, kept free for [`sites::links`].
    Link,
    /// `r`: a road (any walkable ground, an existing road kept).
    Road,
    /// `1` to `3`: a spawn slot, in build order.
    Spawn(u8),
    /// `4` to `9`: a tower, in build order.
    Tower(u8),
    /// `e`.
    Extension,
    /// `s`: a source lab (placed before the others, so the first reaction has its inputs).
    SourceLab,
    /// `l`.
    Lab,
    /// `S` storage, `T` terminal, `F` factory, `P` power spawn, `N` nuker.
    One(StructureType),
}

/// One cell of a stamp, as an offset from its origin (the hub for a core, the top-left corner
/// otherwise).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    dx: i8,
    dy: i8,
    what: What,
}

impl Cell {
    /// The Chebyshev distance from the origin.
    fn ring(self) -> u8 {
        self.dx.unsigned_abs().max(self.dy.unsigned_abs())
    }

    /// The structure the cell holds, if any (all of them obstacles, V235).
    fn kind(self) -> Option<StructureType> {
        match self.what {
            What::Spawn(_) => Some(StructureType::Spawn),
            What::Tower(_) => Some(StructureType::Tower),
            What::Extension => Some(StructureType::Extension),
            What::SourceLab | What::Lab => Some(StructureType::Lab),
            What::One(k) => Some(k),
            What::Hub | What::Link | What::Road => None,
        }
    }
}

fn what(c: u8) -> Option<What> {
    Some(match c {
        b'H' => What::Hub,
        b'L' => What::Link,
        b'r' => What::Road,
        b'1'..=b'3' => What::Spawn(c - b'1'),
        b'4'..=b'9' => What::Tower(c - b'4'),
        b'e' => What::Extension,
        b's' => What::SourceLab,
        b'l' => What::Lab,
        b'S' => What::One(StructureType::Storage),
        b'T' => What::One(StructureType::Terminal),
        b'F' => What::One(StructureType::Factory),
        b'P' => What::One(StructureType::PowerSpawn),
        b'N' => What::One(StructureType::Nuker),
        _ => return None,
    })
}

/// The cells of a stamp in row order, as offsets from the `H` cell (or the top-left corner when
/// there is none).
fn parse(rows: &[&str]) -> Vec<Cell> {
    let origin = rows
        .iter()
        .enumerate()
        .find_map(|(y, r)| r.bytes().position(|c| c == b'H').map(|x| (x, y)))
        .unwrap_or((0, 0));
    let mut out = Vec::new();
    for (y, row) in rows.iter().enumerate() {
        for (x, c) in row.bytes().enumerate() {
            if let Some(what) = what(c) {
                let dx = (x as i16 - origin.0 as i16) as i8;
                let dy = (y as i16 - origin.1 as i16) as i8;
                out.push(Cell { dx, dy, what });
            }
        }
    }
    out
}

/// The core variants: the template cut to each of [`CORE_RADII`], then [`MINIMAL`].
fn core_variants() -> Vec<Vec<Cell>> {
    let core = parse(&CORE);
    CORE_RADII
        .iter()
        .map(|&r| core.iter().copied().filter(|c| c.ring() <= r).collect())
        .chain(std::iter::once(parse(&MINIMAL)))
        .collect()
}

/// `(dx, dy)` in orientation `o` (0 to 3 rotate a quarter turn each, 4 to 7 the mirror image).
fn orient(o: u8, dx: i8, dy: i8) -> (i8, i8) {
    let x = if o >= 4 { -dx } else { dx };
    match o % 4 {
        0 => (x, dy),
        1 => (-dy, x),
        2 => (-x, -dy),
        _ => (dy, -x),
    }
}

fn offset(t: Tile, dx: i8, dy: i8) -> Option<Tile> {
    Tile::new(t.x().checked_add_signed(dx)?, t.y().checked_add_signed(dy)?)
}

/// The tile of `c` for a stamp at `anchor` in orientation `o`, or `None` outside the room.
fn at(anchor: Tile, o: u8, c: &Cell) -> Option<Tile> {
    let (dx, dy) = orient(o, c.dx, c.dy);
    offset(anchor, dx, dy)
}

/// The distinct orientations of a stamp, each shifted so its cells start at (0, 0): a stamp is
/// then tried at every anchor in each shape once.
fn shapes(cells: &[Cell]) -> Vec<Vec<Cell>> {
    let mut out: Vec<Vec<Cell>> = Vec::new();
    let mut keys: Vec<Vec<Cell>> = Vec::new();
    for o in 0..ORIENTATIONS {
        let turned: Vec<Cell> = cells
            .iter()
            .map(|c| {
                let (dx, dy) = orient(o, c.dx, c.dy);
                Cell {
                    dx,
                    dy,
                    what: c.what,
                }
            })
            .collect();
        let mx = turned.iter().map(|c| c.dx).min().unwrap_or(0);
        let my = turned.iter().map(|c| c.dy).min().unwrap_or(0);
        let shape: Vec<Cell> = turned
            .iter()
            .map(|c| Cell {
                dx: c.dx - mx,
                dy: c.dy - my,
                what: c.what,
            })
            .collect();
        let mut key = shape.clone();
        key.sort_by_key(|c| (c.dx, c.dy));
        if !keys.contains(&key) {
            keys.push(key);
            out.push(shape);
        }
    }
    out
}

/// The largest square of cells around the origin: a stamp fits only where the distance transform
/// of usable ground is at least this (see [`distance_transform`]).
fn need_dt(cells: &[Cell]) -> u8 {
    let has = |dx: i8, dy: i8| cells.iter().any(|c| c.dx == dx && c.dy == dy);
    let mut k: i8 = 0;
    while k < 8 && (-k..=k).all(|dx| (-k..=k).all(|dy| has(dx, dy))) {
        k += 1;
    }
    k.unsigned_abs()
}

fn tiles() -> impl Iterator<Item = Tile> {
    (0..ROOM_AREA).filter_map(Tile::from_index)
}

/// The tiles within Chebyshev `r` of `c`.
fn square(c: Tile, r: u8) -> impl Iterator<Item = Tile> {
    let (x0, x1, y0, y1) = (
        c.x().saturating_sub(r),
        c.x().saturating_add(r),
        c.y().saturating_sub(r),
        c.y().saturating_add(r),
    );
    (x0..=x1).flat_map(move |x| (y0..=y1).filter_map(move |y| Tile::new(x, y)))
}

fn natural(input: &PlanInput<'_>, t: Tile) -> bool {
    t == input.controller || input.sources.contains(&t) || input.mineral == Some(t)
}

/// The first level that builds anything but a spawn: the economy's roads, and the least level of a
/// stamp road.
fn first_build() -> u8 {
    earliest_level(StructureType::Extension, 0).unwrap_or(1)
}

/// A stamp road's level: that of the earliest structure beside it that the same stamp placed, and
/// never before [`first_build`].
fn road_level(t: Tile, placed: &[(Tile, u8)], first: u8) -> u8 {
    placed
        .iter()
        .filter(|(p, _)| p.range(t) <= 1)
        .map(|&(_, l)| l)
        .min()
        .map_or(first, |l| l.max(first))
}

fn label(kind: StructureType) -> &'static str {
    match kind {
        StructureType::Spawn => "spawn",
        StructureType::Extension => "extension",
        StructureType::Tower => "tower",
        StructureType::Terminal => "terminal",
        StructureType::Factory => "factory",
        StructureType::PowerSpawn => "power spawn",
        StructureType::Nuker => "nuker",
        StructureType::Observer => "observer",
        StructureType::Lab => "lab",
        StructureType::Storage => "storage",
        _ => "structure",
    }
}

/// A tile may take an obstacle now.
const M_OBST: u8 = 1;
/// A tile is walkable: it may take a stamp road.
const M_ROAD: u8 = 2;
/// Walkable and without a costed structure: it may be the hub.
const M_FREE: u8 = 4;
/// It may take the hub link: open, not kept clear, and the link's site rules.
const M_LINK: u8 = 8;
/// Away from the ground [`sites`] picks from (see [`SOURCE_GAP`], and the controller's container
/// and link at `CONTROLLER_LINK_RANGE`): the core's cells must be.
const M_AWAY: u8 = 16;

/// What each tile may take now, as `M_*` bits.
fn masks(board: &Board<'_>, input: &PlanInput<'_>) -> Vec<u8> {
    let of = |t: Tile| {
        let mut m = 0;
        if board.can_place(StructureType::Extension, t) {
            m |= M_OBST;
        }
        if board.walkable(t) {
            m |= M_ROAD;
            if board.structure(t).is_none() {
                m |= M_FREE;
            }
        }
        if board.open(t) && !board.is_clear(t) && board.can_place_role(StructureType::Link, t) {
            m |= M_LINK;
        }
        if input.sources.iter().all(|&s| s.range(t) > SOURCE_GAP)
            && input.controller.range(t) > CONTROLLER_LINK_RANGE
        {
            m |= M_AWAY;
        }
        m
    };
    (0..ROOM_AREA)
        .map(|i| Tile::from_index(i).map_or(0, of))
        .collect()
}

fn mask(m: &[u8], t: Tile) -> u8 {
    m.get(t.index()).copied().unwrap_or(0)
}

/// Whether every cell of `cells` at `anchor` in orientation `o` may take what it holds (`extra`
/// bits required of every cell besides its own).
fn fits(cells: &[Cell], anchor: Tile, o: u8, m: &[u8], extra: u8) -> bool {
    cells.iter().all(|c| {
        let own = match c.what {
            What::Hub => M_FREE,
            What::Link => M_LINK,
            What::Road => M_ROAD,
            _ => M_OBST,
        };
        let need = own | extra;
        at(anchor, o, c).is_some_and(|t| mask(m, t) & need == need)
    })
}

/// Whether a hub and its link tile keep clear of the economy's ground: both out of reach of the
/// mining tiles and source links ([`SOURCE_GAP`]) and farther than `RECEIVER_RANGE` from the
/// controller (so the link never reads as the controller link and the controller container never
/// lands on the hub), and the hub not next to the mineral (where its container goes).
fn hub_ok(input: &PlanInput<'_>, hub: Tile, link: Tile) -> bool {
    let far = |t: Tile| {
        input.sources.iter().all(|&s| s.range(t) > SOURCE_GAP)
            && t.range(input.controller) > RECEIVER_RANGE
    };
    far(hub) && far(link) && input.mineral.is_none_or(|m| m.range(hub) > 1)
}

/// The exits a flood reached, as the validator reads them: the sides with an open exit tile next
/// to reached ground, in [`Side::ALL`] order, and how many such exit tiles there are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Exits {
    sides: [bool; 4],
    tiles: u16,
}

impl Exits {
    /// Whether `now` keeps every side and every exit tile `self` has (a plan only adds
    /// obstacles, so the reached exit tiles only shrink: an equal count is the same tiles).
    fn kept_by(self, now: Exits) -> bool {
        now.tiles >= self.tiles
            && self
                .sides
                .iter()
                .zip(now.sides.iter())
                .all(|(&before, &after)| !before || after)
    }
}

/// Flood buffers a run keeps (every use resets them first, so nothing carries between steps).
#[derive(Default)]
struct Flood {
    mark: Vec<u8>,
    stack: Vec<Tile>,
}

const SEEN: u8 = 1;
const SHUT: u8 = 2;

impl Flood {
    fn reset(&mut self) {
        self.mark.clear();
        self.mark.resize(ROOM_AREA, 0);
        self.stack.clear();
    }

    fn state(&self, t: Tile) -> u8 {
        self.mark.get(t.index()).copied().unwrap_or(SHUT)
    }

    fn seen(&self, t: Tile) -> bool {
        self.state(t) == SEEN
    }

    fn near(&self, t: Tile) -> bool {
        t.neighbors().any(|(_, n)| self.seen(n))
    }

    fn set(&mut self, t: Tile, v: u8) {
        if let Some(c) = self.mark.get_mut(t.index()) {
            *c = v;
        }
    }

    /// Floods (8-connected) from `seed` over the tiles `walk` accepts and not shut.
    fn fill(&mut self, seed: Tile, walk: impl Fn(Tile) -> bool) {
        if self.state(seed) != 0 || !walk(seed) {
            return;
        }
        self.set(seed, SEEN);
        self.stack.push(seed);
        while let Some(t) = self.stack.pop() {
            for (_, n) in t.neighbors() {
                if self.state(n) == 0 && walk(n) {
                    self.set(n, SEEN);
                    self.stack.push(n);
                }
            }
        }
    }

    /// The exits next to reached ground (the validator's rule): only the edge tiles count.
    fn exits(&self, input: &PlanInput<'_>) -> Exits {
        let mut out = Exits::default();
        for t in edge_tiles() {
            let Some(side) = t.side() else { continue };
            if input.terrain.kind(t) == TerrainKind::Wall || !self.near(t) {
                continue;
            }
            out.tiles = out.tiles.saturating_add(1);
            if let Some(c) = Side::ALL
                .iter()
                .position(|&s| s == side)
                .and_then(|i| out.sides.get_mut(i))
            {
                *c = true;
            }
        }
        out
    }
}

/// The exits of the bare room the ground around `hub` reaches, with the standing spawn (an
/// obstacle whatever the plan) in place.
fn bare_exits(input: &PlanInput<'_>, hub: Tile, f: &mut Flood) -> Exits {
    f.reset();
    if let Some(t) = input.spawn {
        f.set(t, SHUT);
    }
    f.fill(hub, |t| {
        !t.is_edge() && input.terrain.kind(t) != TerrainKind::Wall && !natural(input, t)
    });
    f.exits(input)
}

/// What a placement adds: obstacles on `shut` (the new spawns among them on `spawns`) and roads
/// on `walk`.
struct Change<'c> {
    shut: &'c [Tile],
    spawns: &'c [Tile],
    walk: &'c [Tile],
}

/// Whether the plan with `change` keeps the validator's reach rules from the hub: every item
/// reached (an obstacle or the extractor by a neighbour), each source and the mineral by a
/// neighbour (their mining tiles and container come later), every new obstacle by a neighbour and
/// every new road itself, the controller's upgrade range, every side and exit tile of `exits`,
/// and the spawn rule ([`Board::spawn_free`]) for every spawn planned and new but the `exempt` (a
/// standing spawn that broke it before the plan began).
fn reach_ok(
    board: &Board<'_>,
    input: &PlanInput<'_>,
    hub: Tile,
    change: &Change<'_>,
    exits: Exits,
    exempt: &[Tile],
    f: &mut Flood,
) -> bool {
    let Change { shut, spawns, walk } = *change;
    f.reset();
    for &t in shut {
        f.set(t, SHUT);
    }
    f.fill(hub, |t| board.walkable(t));
    let f = &*f;
    let items = board.items().iter().all(|i| match i.kind {
        StructureType::Rampart | StructureType::Wall => true,
        StructureType::Extractor => f.near(i.tile),
        k if blocks(k) => f.near(i.tile),
        _ => f.seen(i.tile),
    });
    // The spawn rule last: an unreached free neighbour costs a flood of its own.
    let free = |n: Tile| board.walkable(n) && !shut.contains(&n);
    let spawns_ok = || {
        board
            .items()
            .iter()
            .filter(|i| i.kind == StructureType::Spawn)
            .map(|i| i.tile)
            .chain(spawns.iter().copied())
            .filter(|sp| !exempt.contains(sp))
            .all(|sp| board.spawn_free(sp, Some(hub), free, |n| f.seen(n)))
    };
    items
        && input
            .sources
            .iter()
            .chain(input.mineral.iter())
            .all(|&t| f.near(t))
        && shut.iter().all(|&t| f.near(t))
        && walk.iter().all(|&t| f.seen(t))
        && square(input.controller, RECEIVER_RANGE).any(|t| f.seen(t))
        && exits.kept_by(f.exits(input))
        && spawns_ok()
}

/// Fills `s.field` with path costs to `hub` over the plan so far.
fn measure(board: &Board<'_>, hub: Tile, s: &mut Scratch) {
    board.cost_grid(&mut s.grid, fatigue());
    s.field.compute(
        &s.grid,
        Goal {
            tile: hub,
            range: 0,
        },
        &mut s.dial,
    );
}

fn dist(s: &Scratch, t: Tile) -> Option<u32> {
    let d = s.field.distance(t);
    (d != UNREACHED).then_some(u32::from(d))
}

/// The cost of standing beside a stamp structure on `t` from the hub: the least path cost of the
/// stamp's road tiles next to it (`s.field` measured from the hub).
fn reach_of<'r>(roads: &'r [Tile], s: &'r Scratch) -> impl Fn(Tile) -> u32 + 'r {
    move |t: Tile| {
        roads
            .iter()
            .filter(|r| r.range(t) <= 1)
            .filter_map(|&r| dist(s, r))
            .min()
            .unwrap_or(u32::MAX)
    }
}

/// Whether an obstacle of `kind` may go on `t` alone and keep the plan's reach: the site rules,
/// no open exit beside it, a reached walkable neighbour, its free neighbours one group (so the
/// walkable ground stays one piece, `keeps_neighbours_connected`), and every obstacle beside it
/// left with another reached walkable neighbour (for a spawn, one that is no creep's post); a new
/// spawn keeps the spawn rule ([`Board::spawn_free`]).
fn single_ok(board: &Board<'_>, kind: StructureType, t: Tile, reached: &[bool]) -> bool {
    let r = |n: Tile| reached.get(n.index()).copied().unwrap_or(false) && board.walkable(n);
    board.can_place(kind, t)
        && !t
            .neighbors()
            .any(|(_, n)| n.is_edge() && board.kind(n) != TerrainKind::Wall)
        && t.neighbors().any(|(_, n)| r(n))
        && keeps_neighbours_connected(t, |n| !board.walkable(n))
        // A new spawn keeps the spawn rule itself (V174).
        && (kind != StructureType::Spawn
            || board.spawn_free(t, board.roles.hub, |n| board.walkable(n) && n != t, r))
        && t.neighbors().all(|(_, m)| match board.structure(m) {
            // A spawn keeps a free neighbour that is no creep's post (V174).
            Some(StructureType::Spawn) => m.neighbors().any(|(_, w)| {
                w != t && r(w) && Some(w) != board.roles.hub && !board.roles.mining.contains(&w)
            }),
            Some(k) if blocks(k) => m.neighbors().any(|(_, w)| w != t && r(w)),
            _ => true,
        })
}

/// A core placement: its hub's cost, orientation, the spawn slot standing on the standing spawn
/// (`k + 1`, or 0), and the hub's index. Ordered so the best comes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Fit {
    cost: u32,
    o: u8,
    slot: u8,
    hub: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Reserve,
    Core,
    Containers,
    Links,
    Mineral,
    Roads,
    Essentials,
    Labs,
    Pods,
    Singles,
    Ramparts,
    Done,
}

/// One planning run for a room, a phase per [`Run::step`].
pub struct Run<'a> {
    input: &'a PlanInput<'a>,
    board: Board<'a>,
    phase: Phase,
    hub: Option<Tile>,
    hub_link: Option<Tile>,
    /// The exits of the bare room reached from the hub.
    exits: Exits,
    /// Spawns that broke the spawn rule before the plan began (a standing spawn on such ground):
    /// the validator judges them, the reach checks leave them out.
    exempt: Vec<Tile>,
    /// The standing spawn stands alone (no core spawn slot fits on it).
    lone: bool,
    /// The first pod in [`PODS`] still worth trying.
    pod: usize,
    /// The labs go before the pods: set when a run with the pods first left no room for them,
    /// and the run starts over.
    labs_first: bool,
    flood: Flood,
}

impl<'a> Run<'a> {
    pub fn new(input: &'a PlanInput<'a>) -> Self {
        Self {
            input,
            board: Board::new(input),
            phase: Phase::Reserve,
            hub: None,
            hub_link: None,
            exits: Exits::default(),
            exempt: Vec::new(),
            lone: false,
            pod: 0,
            labs_first: false,
            flood: Flood::default(),
        }
    }

    /// Does the next phase. `Step::Done` once the plan is complete; then [`Run::finish`].
    pub fn step(&mut self, s: &mut Scratch) -> Result<Step, PlanError> {
        let input = self.input;
        match self.phase {
            Phase::Reserve => {
                sites::reserve(&mut self.board, input);
                self.phase = Phase::Core;
            }
            Phase::Core => {
                self.core(s)?;
                self.phase = Phase::Containers;
            }
            Phase::Containers => {
                let hub = self.hub()?;
                sites::containers(&mut self.board, input, hub, s)?;
                self.phase = Phase::Links;
            }
            Phase::Links => {
                let hub = self.hub()?;
                let link = self.hub_link.ok_or(PlanError::NoRoom("hub link"))?;
                sites::links(&mut self.board, input, hub, link, s)?;
                self.phase = Phase::Mineral;
            }
            Phase::Mineral => {
                let hub = self.hub()?;
                sites::mineral(&mut self.board, input, hub, s)?;
                self.phase = Phase::Roads;
            }
            Phase::Roads => {
                self.roads(s)?;
                self.phase = Phase::Essentials;
            }
            Phase::Essentials => {
                let kinds = self.missing(&ESSENTIAL);
                if !kinds.is_empty() {
                    self.singles(s, &kinds)?;
                }
                self.phase = if self.labs_first {
                    Phase::Labs
                } else {
                    Phase::Pods
                };
            }
            Phase::Labs => {
                if self.board.count(StructureType::Lab) == 0 && !self.stamp(s, &LABS)? {
                    if self.labs_first {
                        return Err(PlanError::NoRoom("labs"));
                    }
                    self.restart();
                    return Ok(Step::More);
                }
                self.phase = if self.labs_first {
                    Phase::Pods
                } else {
                    Phase::Singles
                };
            }
            Phase::Pods => {
                if !self.pods(s)? {
                    self.phase = if self.labs_first {
                        Phase::Singles
                    } else {
                        Phase::Labs
                    };
                }
            }
            Phase::Singles => {
                let kinds = self.missing(&LAST);
                if !kinds.is_empty() {
                    self.singles(s, &kinds)?;
                }
                self.phase = Phase::Ramparts;
            }
            Phase::Ramparts => {
                self.ramparts()?;
                self.phase = Phase::Done;
                return Ok(Step::Done);
            }
            Phase::Done => return Ok(Step::Done),
        }
        Ok(Step::More)
    }

    /// The plan, once a step returned `Step::Done`.
    pub fn finish(self) -> Layout {
        self.board.into_layout()
    }

    /// The phase the next [`Run::step`] does (for timings and logs).
    pub fn phase(&self) -> &'static str {
        match self.phase {
            Phase::Reserve => "reserve",
            Phase::Core => "core",
            Phase::Containers => "containers",
            Phase::Links => "links",
            Phase::Mineral => "mineral",
            Phase::Roads => "roads",
            Phase::Essentials => "essentials",
            Phase::Labs => "labs",
            Phase::Pods => "pods",
            Phase::Singles => "singles",
            Phase::Ramparts => "ramparts",
            Phase::Done => "done",
        }
    }

    fn hub(&self) -> Result<Tile, PlanError> {
        self.hub.ok_or(PlanError::NoRoom("hub"))
    }

    /// One of each structure of `kinds` the top level allows and the plan lacks, in order.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` at [`MAX_LEVEL`] (V234).
    fn missing(&self, kinds: &[StructureType]) -> Vec<StructureType> {
        let mut out = Vec::new();
        for &kind in kinds {
            let n = allowed(kind, MAX_LEVEL).saturating_sub(self.board.count(kind));
            out.extend(std::iter::repeat_n(kind, n as usize));
        }
        out
    }

    /// Chooses and places the core (see the module docs), and with it the standing spawn: on one
    /// of the core's spawn slots when a core fits there that is at least [`SPAWN_CORE_RADIUS`] or
    /// no smaller than the best core with the spawn left alone, else alone (placed first, so at
    /// level 1) with the best core elsewhere.
    fn core(&mut self, s: &mut Scratch) -> Result<(), PlanError> {
        let input = self.input;
        let cost = AnchorCost::new(input);
        let variants = core_variants();
        let radius = |v: usize| CORE_RADII.get(v).copied().unwrap_or(0);
        if let Some(t) = input.spawn {
            self.check_standing(t)?;
        }
        let chosen = match input.spawn {
            None => self.core_free(&variants, &cost, s, None),
            Some(t) => {
                let on = self.core_on(t, &variants, &cost);
                let alone = self.core_free(&variants, &cost, s, Some(t));
                match (on, alone) {
                    (Some((v, fit)), Some((w, _))) if v <= w || radius(v) >= SPAWN_CORE_RADIUS => {
                        Some((v, fit))
                    }
                    (Some(on), None) => Some(on),
                    (_, alone) => {
                        // As a role: it may stand on ground the economy keeps clear (by the
                        // controller or a source), which the game allows (V237).
                        self.board
                            .place_role(StructureType::Spawn, t)
                            .ok_or(PlanError::BadInput("standing spawn"))?;
                        self.lone = true;
                        alone
                    }
                }
            }
        };
        let (v, fit) = chosen.ok_or(PlanError::NoRoom("core"))?;
        let cells = variants.get(v).ok_or(PlanError::NoRoom("core"))?;
        self.place_core(cells, fit)
    }

    /// What the standing spawn `t` does to the room before anything is planned: a source or the
    /// mineral it leaves no walkable neighbour can never be mined (`NoRoom("mining tile")` or
    /// `NoRoom("mineral container")`, as the flood candidate's shared steps report it; one whose
    /// neighbours it seals in a pocket fails later, at the core), and one whose free neighbours
    /// already break the spawn rule (V174, [`Board::standing_spawn_free`]) is exempt from the reach
    /// checks, so the validator judges it rather than every placement failing on it.
    fn check_standing(&mut self, t: Tile) -> Result<(), PlanError> {
        let board = &self.board;
        let starved = |x: &Tile| !x.neighbors().any(|(_, n)| board.walkable(n) && n != t);
        if self.input.sources.iter().any(starved) {
            return Err(PlanError::NoRoom("mining tile"));
        }
        if self.input.mineral.iter().any(starved) {
            return Err(PlanError::NoRoom("mineral container"));
        }
        if !board.standing_spawn_free(t, &sites::forced_tiles(board, self.input.sources, Some(t))) {
            self.exempt.push(t);
        }
        Ok(())
    }

    /// The best core with a spawn slot on the standing spawn `t`: per variant the cheapest hub
    /// that passes the reach check, then the largest variant within [`CORE_COST_SLACK`] of the
    /// cheapest of them.
    fn core_on(
        &mut self,
        t: Tile,
        variants: &[Vec<Cell>],
        cost: &AnchorCost,
    ) -> Option<(usize, Fit)> {
        let m = masks(&self.board, self.input);
        let mut best = Vec::new();
        for cells in variants {
            let Some(link) = cells.iter().find(|c| c.what == What::Link) else {
                best.push(None);
                continue;
            };
            let mut found = Vec::new();
            for o in 0..ORIENTATIONS {
                for c in cells {
                    let What::Spawn(k) = c.what else { continue };
                    let (dx, dy) = orient(o, c.dx, c.dy);
                    let Some(hub) = offset(t, dx.wrapping_neg(), dy.wrapping_neg()) else {
                        continue;
                    };
                    let Some(score) = cost.cost(hub) else {
                        continue;
                    };
                    if at(hub, o, link).is_some_and(|l| hub_ok(self.input, hub, l))
                        && fits(cells, hub, o, &m, M_AWAY)
                    {
                        found.push(Fit {
                            cost: score,
                            o,
                            slot: k + 1,
                            hub: hub.index(),
                        });
                    }
                }
            }
            best.push(self.first_reaching(cells, found, None));
        }
        let least = best.iter().flatten().map(|f| f.cost).min()?;
        best.iter().enumerate().find_map(|(v, f)| {
            f.filter(|f| f.cost <= least.saturating_add(CORE_COST_SLACK))
                .map(|f| (v, f))
        })
    }

    /// The best core anywhere: the cheapest hub of the minimal stamp that passes the reach check,
    /// then the largest variant with a hub within [`CORE_COST_SLACK`] of it. With `standing`, as
    /// if a spawn stood there alone (not yet on the board).
    fn core_free(
        &mut self,
        variants: &[Vec<Cell>],
        cost: &AnchorCost,
        s: &mut Scratch,
        standing: Option<Tile>,
    ) -> Option<(usize, Fit)> {
        let mut m = masks(&self.board, self.input);
        if let Some(cell) = standing.and_then(|t| m.get_mut(t.index())) {
            *cell = 0;
        }
        distance_transform(
            |t| mask(&m, t) & (M_ROAD | M_AWAY) == (M_ROAD | M_AWAY),
            &mut s.dt,
        );
        let last = variants.len().checked_sub(1)?;
        let minimal = variants.get(last)?;
        let found = self.free_fits(minimal, cost, &m, &s.dt, None);
        let least = self.first_reaching(minimal, found, standing)?;
        let bound = least.cost.saturating_add(CORE_COST_SLACK);
        for (v, cells) in variants.iter().enumerate().take(last) {
            let found = self.free_fits(cells, cost, &m, &s.dt, Some(bound));
            if let Some(fit) = self.first_reaching(cells, found, standing) {
                return Some((v, fit));
            }
        }
        Some((last, least))
    }

    /// Every placement of `cells` whose hub costs at most `bound`.
    fn free_fits(
        &self,
        cells: &[Cell],
        cost: &AnchorCost,
        m: &[u8],
        dt: &[u8; ROOM_AREA],
        bound: Option<u32>,
    ) -> Vec<Fit> {
        let need = need_dt(cells);
        let mut out = Vec::new();
        let Some(link) = cells.iter().find(|c| c.what == What::Link) else {
            return out;
        };
        for hub in tiles() {
            if dt.get(hub.index()).copied().unwrap_or(0) < need {
                continue;
            }
            let Some(score) = cost.cost(hub) else {
                continue;
            };
            if bound.is_some_and(|b| score > b) {
                continue;
            }
            for o in 0..ORIENTATIONS {
                if at(hub, o, link).is_some_and(|l| hub_ok(self.input, hub, l))
                    && fits(cells, hub, o, m, M_AWAY)
                {
                    out.push(Fit {
                        cost: score,
                        o,
                        slot: 0,
                        hub: hub.index(),
                    });
                }
            }
        }
        out
    }

    /// The best of `found` that passes the reach check, trying at most [`MAX_REACH_CHECKS`].
    fn first_reaching(
        &mut self,
        cells: &[Cell],
        mut found: Vec<Fit>,
        standing: Option<Tile>,
    ) -> Option<Fit> {
        found.sort_unstable();
        found.into_iter().take(MAX_REACH_CHECKS).find(|fit| {
            Tile::from_index(fit.hub)
                .is_some_and(|hub| self.core_reaches(cells, hub, *fit, standing))
        })
    }

    /// The spawn slots of `cells` that a core places, in build order: the slot on the standing
    /// spawn first, then by number, as many as the top level still allows (a spawn at `standing`
    /// counted as placed).
    fn spawn_slots(
        &self,
        cells: &[Cell],
        hub: Tile,
        fit: Fit,
        standing: Option<Tile>,
    ) -> Vec<Tile> {
        let mut spawns: Vec<(bool, u8, Tile)> = cells
            .iter()
            .filter_map(|c| match c.what {
                What::Spawn(k) => at(hub, fit.o, c).map(|t| (fit.slot != k + 1, k, t)),
                _ => None,
            })
            .collect();
        spawns.sort_unstable_by_key(|&(later, k, _)| (later, k));
        let standing = u32::from(standing.is_some());
        let room = allowed(StructureType::Spawn, MAX_LEVEL)
            .saturating_sub(self.board.count(StructureType::Spawn) + standing);
        spawns
            .into_iter()
            .take(room as usize)
            .map(|(_, _, t)| t)
            .collect()
    }

    /// Whether the core at `hub` passes [`reach_ok`] (the link tile counted as the obstacle it
    /// becomes, and a spawn at `standing`).
    fn core_reaches(
        &mut self,
        cells: &[Cell],
        hub: Tile,
        fit: Fit,
        standing: Option<Tile>,
    ) -> bool {
        let spawns = self.spawn_slots(cells, hub, fit, standing);
        let mut shut: Vec<Tile> = standing.into_iter().collect();
        let mut walk = vec![hub];
        for c in cells {
            let Some(t) = at(hub, fit.o, c) else {
                return false;
            };
            match c.what {
                What::Road => walk.push(t),
                What::Hub => {}
                What::Spawn(_) if !spawns.contains(&t) => {}
                _ => shut.push(t),
            }
        }
        let exits = bare_exits(self.input, hub, &mut self.flood);
        let new_spawns: Vec<Tile> = standing.into_iter().chain(spawns.iter().copied()).collect();
        let change = Change {
            shut: &shut,
            spawns: &new_spawns,
            walk: &walk,
        };
        reach_ok(
            &self.board,
            self.input,
            hub,
            &change,
            exits,
            &self.exempt,
            &mut self.flood,
        )
    }

    /// Places the core: spawns, the unique structures, towers, extensions (inner rings first),
    /// then its roads; and sets the hub and storage roles.
    fn place_core(&mut self, cells: &[Cell], fit: Fit) -> Result<(), PlanError> {
        let hub = Tile::from_index(fit.hub).ok_or(PlanError::NoRoom("core"))?;
        let o = fit.o;
        let mut placed: Vec<(Tile, u8)> = Vec::new();
        for t in self.spawn_slots(cells, hub, fit, None) {
            let level = self
                .board
                .place(StructureType::Spawn, t)
                .ok_or(PlanError::NoRoom("spawn"))?;
            placed.push((t, level));
        }
        for kind in UNIQUE {
            for c in cells.iter().filter(|c| c.what == What::One(kind)) {
                let t = at(hub, o, c).ok_or(PlanError::NoRoom("core"))?;
                let level = self
                    .board
                    .place(kind, t)
                    .ok_or(PlanError::NoRoom(label(kind)))?;
                placed.push((t, level));
                if kind == StructureType::Storage {
                    self.board.roles.storage = Some(t);
                }
            }
        }
        let mut towers: Vec<(u8, Tile)> = cells
            .iter()
            .filter_map(|c| match c.what {
                What::Tower(k) => at(hub, o, c).map(|t| (k, t)),
                _ => None,
            })
            .collect();
        towers.sort_unstable_by_key(|&(k, _)| k);
        for (_, t) in towers {
            let level = self
                .board
                .place(StructureType::Tower, t)
                .ok_or(PlanError::NoRoom("tower"))?;
            placed.push((t, level));
        }
        let mut extensions: Vec<(u8, usize, Tile)> = cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.what == What::Extension)
            .filter_map(|(i, c)| at(hub, o, c).map(|t| (c.ring(), i, t)))
            .collect();
        extensions.sort_unstable_by_key(|&(ring, i, _)| (ring, i));
        for (_, _, t) in extensions {
            let level = self
                .board
                .place(StructureType::Extension, t)
                .ok_or(PlanError::NoRoom("extension"))?;
            placed.push((t, level));
        }
        let first = first_build();
        for c in cells.iter().filter(|c| c.what == What::Road) {
            let t = at(hub, o, c).ok_or(PlanError::NoRoom("core"))?;
            let level = road_level(t, &placed, first);
            if self.board.has_road(t) {
                self.board.lower_road(t, level);
            } else if !self.board.place_at_level(StructureType::Road, t, level) {
                return Err(PlanError::NoRoom("core road"));
            }
        }
        self.board.roles.hub = Some(hub);
        self.hub = Some(hub);
        self.hub_link = cells
            .iter()
            .find(|c| c.what == What::Link)
            .and_then(|c| at(hub, o, c));
        self.exits = bare_exits(self.input, hub, &mut self.flood);
        Ok(())
    }

    /// The economy's roads (the shared rule), and one to a lone standing spawn.
    fn roads(&mut self, s: &mut Scratch) -> Result<(), PlanError> {
        let hub = self.hub()?;
        let first = first_build();
        let mining = self.board.roles.mining.clone();
        for m in mining {
            roads::road(&mut self.board, hub, m, 0, first, s)?;
        }
        if let Some(c) = self.board.roles.controller_container {
            roads::road(&mut self.board, hub, c, 1, first, s)?;
        }
        if let Some(c) = self.board.roles.mineral_container {
            let level = self
                .board
                .items()
                .iter()
                .find(|i| i.kind == StructureType::Extractor)
                .map_or(first, |i| i.level);
            roads::road(&mut self.board, hub, c, 0, level, s)?;
        }
        if self.lone
            && let Some(t) = self.input.spawn
        {
            roads::road(&mut self.board, hub, t, 1, first, s)?;
        }
        Ok(())
    }

    /// Starts the run over with the labs before the pods.
    fn restart(&mut self) {
        let input = self.input;
        *self = Self {
            labs_first: true,
            ..Self::new(input)
        };
    }

    /// One step of the extensions: the best pod that fits (the largest the plan still has
    /// extensions for), unless single tiles are as cheap to fill as the pod's extensions on
    /// average ([`POD_BIAS`] in the pod's favour); then those singles instead (all of them, or
    /// with no pod left every single the plan still lacks). False when neither fits.
    fn pods(&mut self, s: &mut Scratch) -> Result<bool, PlanError> {
        let missing = allowed(StructureType::Extension, MAX_LEVEL)
            .saturating_sub(self.board.count(StructureType::Extension));
        if missing == 0 {
            return Ok(false);
        }
        let hub = self.hub()?;
        measure(&self.board, hub, s);
        let mut pod = None;
        while let Some(rows) = PODS.get(self.pod) {
            let size: usize = rows
                .iter()
                .map(|r| r.bytes().filter(|&c| c == b'e').count())
                .sum();
            if size > 0 && size as u32 <= missing {
                pod = self.best_stamp(s, rows, hub).map(|p| (p, size));
                if pod.is_some() {
                    break;
                }
            }
            self.pod += 1;
        }
        let limit = pod.as_ref().map(|((shape, anchor, walk), _)| {
            let reach = reach_of(walk, s);
            let costs: Vec<u32> = shape
                .iter()
                .filter(|c| c.kind().is_some())
                .filter_map(|c| at(*anchor, 0, c))
                .map(reach)
                .collect();
            let n = u32::try_from(costs.len()).unwrap_or(u32::MAX).max(1);
            let sum = costs.iter().fold(0u32, |a, &c| a.saturating_add(c));
            (sum / n).saturating_sub(POD_BIAS)
        });
        let kinds = vec![StructureType::Extension; missing as usize];
        if self.place_singles(s, &kinds, limit) > 0 {
            return Ok(true);
        }
        match pod {
            Some(((shape, anchor, walk), _)) => {
                self.commit(&shape, anchor, &walk, s)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Places `rows` at its best placement ([`Run::best_stamp`]); false when none.
    fn stamp(&mut self, s: &mut Scratch, rows: &[&str]) -> Result<bool, PlanError> {
        let hub = self.hub()?;
        measure(&self.board, hub, s);
        match self.best_stamp(s, rows, hub) {
            Some((shape, anchor, walk)) => {
                self.commit(&shape, anchor, &walk, s)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The best placement of `rows` that fits and passes the reach check (`s.field` measured from
    /// the hub): the least summed path cost from the hub to its road cells, then shape and anchor
    /// index. The shape, its anchor and its road tiles.
    fn best_stamp(
        &mut self,
        s: &Scratch,
        rows: &[&str],
        hub: Tile,
    ) -> Option<(Vec<Cell>, Tile, Vec<Tile>)> {
        let m = masks(&self.board, self.input);
        let shapes = shapes(&parse(rows));
        let mut found: Vec<(u32, usize, usize)> = Vec::new();
        for (k, shape) in shapes.iter().enumerate() {
            for anchor in tiles() {
                if !fits(shape, anchor, 0, &m, 0) {
                    continue;
                }
                let score = shape
                    .iter()
                    .filter(|c| c.what == What::Road)
                    .try_fold(0u32, |sum, c| {
                        at(anchor, 0, c).and_then(|t| dist(s, t)).map(|d| sum + d)
                    });
                if let Some(score) = score {
                    found.push((score, k, anchor.index()));
                }
            }
        }
        found.sort_unstable();
        for &(_, k, a) in found.iter().take(MAX_REACH_CHECKS) {
            let (Some(shape), Some(anchor)) = (shapes.get(k), Tile::from_index(a)) else {
                continue;
            };
            let shut: Vec<Tile> = shape
                .iter()
                .filter(|c| c.kind().is_some())
                .filter_map(|c| at(anchor, 0, c))
                .collect();
            let walk: Vec<Tile> = shape
                .iter()
                .filter(|c| c.what == What::Road)
                .filter_map(|c| at(anchor, 0, c))
                .collect();
            let change = Change {
                shut: &shut,
                spawns: &[],
                walk: &walk,
            };
            if reach_ok(
                &self.board,
                self.input,
                hub,
                &change,
                self.exits,
                &self.exempt,
                &mut self.flood,
            ) {
                return Some((shape.clone(), anchor, walk));
            }
        }
        None
    }

    /// Places a stamp's structures (source labs first, then nearest the hub through the stamp's
    /// roads) and its roads.
    fn commit(
        &mut self,
        shape: &[Cell],
        anchor: Tile,
        roads: &[Tile],
        s: &Scratch,
    ) -> Result<(), PlanError> {
        let reach = reach_of(roads, s);
        let mut builds: Vec<(bool, u32, usize, StructureType, Tile)> = shape
            .iter()
            .filter_map(|c| {
                let kind = c.kind()?;
                let t = at(anchor, 0, c)?;
                Some((c.what != What::SourceLab, reach(t), t.index(), kind, t))
            })
            .collect();
        builds.sort_unstable_by_key(|b| (b.0, b.1, b.2));
        let mut placed = Vec::new();
        for (_, _, _, kind, t) in builds {
            let level = self
                .board
                .place(kind, t)
                .ok_or(PlanError::NoRoom(label(kind)))?;
            placed.push((t, level));
        }
        let first = first_build();
        for &t in roads {
            let level = road_level(t, &placed, first);
            if self.board.has_road(t) {
                self.board.lower_road(t, level);
            } else if !self.board.place_at_level(StructureType::Road, t, level) {
                return Err(PlanError::NoRoom("stamp road"));
            }
        }
        Ok(())
    }

    /// Places `kinds` in order, each on the single tile nearest the hub by path (the least cost
    /// to stand beside it) that [`single_ok`] accepts; all of them, or an error.
    fn singles(&mut self, s: &mut Scratch, kinds: &[StructureType]) -> Result<(), PlanError> {
        let hub = self.hub()?;
        measure(&self.board, hub, s);
        let placed = self.place_singles(s, kinds, None);
        match kinds.get(placed) {
            Some(&kind) => Err(PlanError::NoRoom(label(kind))),
            None => Ok(()),
        }
    }

    /// Places `kinds` in order as [`Run::singles`] does (`s.field` measured from the hub), on
    /// tiles that cost at most `limit` to stand beside; stops at the first it cannot place, and
    /// returns how many it placed.
    fn place_singles(&mut self, s: &Scratch, kinds: &[StructureType], limit: Option<u32>) -> usize {
        let mut reached: Vec<bool> = (0..ROOM_AREA)
            .map(|i| Tile::from_index(i).is_some_and(|t| dist(s, t).is_some()))
            .collect();
        let mut order: Vec<(u32, usize)> = tiles()
            .filter(|&t| self.board.can_place(StructureType::Extension, t))
            .filter_map(|t| {
                t.neighbors()
                    .filter_map(|(_, n)| dist(s, n))
                    .min()
                    .map(|d| (d, t.index()))
            })
            .filter(|&(d, _)| limit.is_none_or(|l| d <= l))
            .collect();
        order.sort_unstable();
        for (n, &kind) in kinds.iter().enumerate() {
            let pick = order
                .iter()
                .filter_map(|&(_, i)| Tile::from_index(i))
                .find(|&t| single_ok(&self.board, kind, t, &reached));
            let Some(t) = pick else { return n };
            if self.board.place(kind, t).is_none() {
                return n;
            }
            if let Some(c) = reached.get_mut(t.index()) {
                *c = false;
            }
        }
        kinds.len()
    }

    /// A rampart over every protected structure, from the level that structure stands (at least
    /// the first level ramparts are allowed): one over nothing yet only decays (V241).
    fn ramparts(&mut self) -> Result<(), PlanError> {
        for t in defence::protected_tiles(&self.board) {
            if self.board.has_rampart(t) {
                continue;
            }
            let level = self.board.level_of(t).unwrap_or(0);
            if !self.board.place_at_level(StructureType::Rampart, t, level) {
                return Err(PlanError::NoRoom("rampart"));
            }
        }
        Ok(())
    }
}

/// Every step at once.
pub fn plan(input: &PlanInput<'_>, s: &mut Scratch) -> Result<Layout, PlanError> {
    let mut run = Run::new(input);
    while run.step(s)? == Step::More {}
    Ok(run.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::TerrainGrid;
    use crate::planner::validate::validate;
    use rustedscreep_consts::{LAB_REACTION_RANGE, ROOM_SIZE, Terrain};

    fn count(cells: &[Cell], f: impl Fn(What) -> bool) -> u32 {
        cells.iter().filter(|c| f(c.what)).count() as u32
    }

    fn range(a: &Cell, b: &Cell) -> u8 {
        a.dx.abs_diff(b.dx).max(a.dy.abs_diff(b.dy))
    }

    fn walkable(w: What) -> bool {
        matches!(w, What::Road | What::Hub)
    }

    /// Every structure cell and the link tile have a road or the hub beside them, inside the stamp.
    fn served(cells: &[Cell]) -> bool {
        cells
            .iter()
            .filter(|c| !walkable(c.what))
            .all(|c| cells.iter().any(|w| walkable(w.what) && range(c, w) == 1))
    }

    /// The road and hub cells are one 8-connected group.
    fn connected(cells: &[Cell]) -> bool {
        let walk: Vec<&Cell> = cells.iter().filter(|c| walkable(c.what)).collect();
        let Some(first) = walk.first() else {
            return false;
        };
        let mut seen = vec![*first];
        let mut i = 0;
        while let Some(c) = seen.get(i).copied() {
            for w in &walk {
                if range(c, w) == 1 && !seen.contains(w) {
                    seen.push(w);
                }
            }
            i += 1;
        }
        seen.len() == walk.len()
    }

    /// How many sides of the stamp's bounding box have a road cell on them.
    fn road_sides(cells: &[Cell]) -> usize {
        let (x0, x1) = (
            cells.iter().map(|c| c.dx).min().unwrap(),
            cells.iter().map(|c| c.dx).max().unwrap(),
        );
        let (y0, y1) = (
            cells.iter().map(|c| c.dy).min().unwrap(),
            cells.iter().map(|c| c.dy).max().unwrap(),
        );
        let roads: Vec<&Cell> = cells.iter().filter(|c| c.what == What::Road).collect();
        [
            roads.iter().any(|c| c.dx == x0),
            roads.iter().any(|c| c.dx == x1),
            roads.iter().any(|c| c.dy == y0),
            roads.iter().any(|c| c.dy == y1),
        ]
        .iter()
        .filter(|&&b| b)
        .count()
    }

    fn one(cells: &[Cell], what: What) -> Cell {
        let all: Vec<&Cell> = cells.iter().filter(|c| c.what == what).collect();
        assert_eq!(all.len(), 1, "{what:?}");
        *all[0]
    }

    /// Uses: `CONTROLLER_STRUCTURES` at the top level (V234).
    #[test]
    fn the_full_core_holds_every_spawn_tower_and_extension() {
        let core = parse(&CORE);
        let top = |k: StructureType| allowed(k, MAX_LEVEL);
        assert_eq!(
            count(&core, |w| matches!(w, What::Spawn(_))),
            top(StructureType::Spawn)
        );
        assert_eq!(
            count(&core, |w| matches!(w, What::Tower(_))),
            top(StructureType::Tower)
        );
        assert_eq!(
            count(&core, |w| w == What::Extension),
            top(StructureType::Extension)
        );
        for kind in UNIQUE {
            assert_eq!(
                count(&core, |w| w == What::One(kind)),
                top(kind),
                "{kind:?}"
            );
        }
        // Each slot number once, so the build order is the numbering.
        for k in 0..u8::try_from(top(StructureType::Spawn)).unwrap() {
            one(&core, What::Spawn(k));
        }
        for k in 0..u8::try_from(top(StructureType::Tower)).unwrap() {
            let t = one(&core, What::Tower(k));
            assert!(t.ring() <= 3, "towers near the centre");
        }
        assert!(core.iter().all(|c| c.ring() <= 5));
    }

    #[test]
    fn every_core_cut_serves_its_structures_from_one_road_net() {
        for (v, cells) in core_variants().iter().enumerate() {
            assert!(served(cells), "variant {v}");
            assert!(connected(cells), "variant {v}");
            let sides = road_sides(cells);
            let radius = CORE_RADII.get(v).copied().unwrap_or(0);
            assert!(
                sides >= if radius >= 3 { 4 } else { 1 },
                "variant {v}: {sides} sides"
            );
            // The hub block: storage and link tile next to the hub and to each other, the first
            // spawn next to the hub, and a road out of it.
            let (h, s, l, p) = (
                one(cells, What::Hub),
                one(cells, What::One(StructureType::Storage)),
                one(cells, What::Link),
                one(cells, What::Spawn(0)),
            );
            assert_eq!(
                (range(&h, &s), range(&h, &l), range(&s, &l), range(&h, &p)),
                (1, 1, 1, 1),
                "variant {v}"
            );
            assert!(
                cells
                    .iter()
                    .any(|c| c.what == What::Road && range(c, &h) == 1),
                "variant {v}"
            );
            // Every spawn has walkable ground beside it to place its creeps on (V174).
            for sp in cells.iter().filter(|c| matches!(c.what, What::Spawn(_))) {
                assert!(
                    cells.iter().any(|w| walkable(w.what) && range(sp, w) == 1),
                    "variant {v}"
                );
            }
            // The fit's pruning: every cell within need_dt - 1 of the hub is part of the stamp.
            let need = i8::try_from(need_dt(cells)).unwrap();
            assert!(need >= 1);
            for dx in 1 - need..need {
                for dy in 1 - need..need {
                    assert!(
                        cells.iter().any(|c| c.dx == dx && c.dy == dy),
                        "variant {v}"
                    );
                }
            }
        }
    }

    /// V240: both source labs within `LAB_REACTION_RANGE` of every other lab.
    #[test]
    fn the_labs_react_within_range() {
        let labs = parse(&LABS);
        let sources: Vec<&Cell> = labs.iter().filter(|c| c.what == What::SourceLab).collect();
        assert_eq!(sources.len(), 2);
        assert_eq!(
            count(&labs, |w| matches!(w, What::SourceLab | What::Lab)),
            allowed(StructureType::Lab, MAX_LEVEL)
        );
        let reach = u8::try_from(LAB_REACTION_RANGE).unwrap();
        for lab in labs.iter().filter(|c| c.what == What::Lab) {
            assert!(sources.iter().all(|s| range(s, lab) <= reach), "{lab:?}");
        }
        assert!(served(&labs) && connected(&labs));
        assert!(road_sides(&labs) >= 2);
    }

    #[test]
    fn pods_serve_every_extension() {
        for rows in PODS {
            let pod = parse(rows);
            assert!(served(&pod) && connected(&pod) && road_sides(&pod) >= 2);
            assert!(
                pod.iter()
                    .all(|c| matches!(c.what, What::Extension | What::Road))
            );
        }
        let sizes: Vec<u32> = PODS
            .iter()
            .map(|rows| count(&parse(rows), |w| w == What::Extension))
            .collect();
        assert!(sizes.windows(2).all(|w| w[0] > w[1]), "largest first");
    }

    /// Rotations and mirrors keep every range between cells, so the invariants above hold in all
    /// 8 orientations; the core's are all distinct, and the symmetric stamps dedupe.
    #[test]
    fn orientations_keep_ranges() {
        let anchor = Tile::new(25, 25).unwrap();
        let mut stamps: Vec<Vec<Cell>> = core_variants();
        stamps.push(parse(&LABS));
        stamps.extend(PODS.iter().map(|rows| parse(rows)));
        for cells in &stamps {
            for o in 0..ORIENTATIONS {
                let placed: Vec<Tile> = cells.iter().map(|c| at(anchor, o, c).unwrap()).collect();
                for (i, a) in cells.iter().enumerate() {
                    for (j, b) in cells.iter().enumerate() {
                        assert_eq!(placed[i].range(placed[j]), range(a, b));
                    }
                }
            }
        }
        let core = parse(&CORE);
        let mut seen: Vec<Vec<Tile>> = Vec::new();
        for o in 0..ORIENTATIONS {
            let link = at(anchor, o, &one(&core, What::Link)).unwrap();
            let storage = at(anchor, o, &one(&core, What::One(StructureType::Storage))).unwrap();
            let key = vec![link, storage];
            assert!(!seen.contains(&key), "orientation {o} repeats");
            seen.push(key);
        }
        assert_eq!(shapes(&parse(&LABS)).len(), 2);
        assert_eq!(shapes(&parse(PODS[0])).len(), 2);
        for shape in shapes(&parse(&LABS)) {
            assert_eq!(shape.iter().map(|c| c.dx).min(), Some(0));
            assert_eq!(shape.iter().map(|c| c.dy).min(), Some(0));
        }
    }

    fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).unwrap()
    }

    /// An open room with walls only under the natural objects (V228) and a wall strip.
    fn terrain(walls: &[(u8, u8)]) -> TerrainGrid {
        let mut raw = vec![0u8; ROOM_AREA];
        for &(x, y) in walls {
            raw[usize::from(y) * usize::from(ROOM_SIZE) + usize::from(x)] = Terrain::Wall as u8;
        }
        TerrainGrid::from_row_major(&raw).unwrap()
    }

    fn stepped(input: &PlanInput<'_>) -> Layout {
        let mut run = Run::new(input);
        while run.step(&mut Scratch::default()).unwrap() == Step::More {}
        run.finish()
    }

    #[test]
    fn an_open_room_gets_the_full_core_and_a_valid_plan() {
        let (c, s1, s2, m) = ((25, 8), (8, 40), (42, 40), (40, 10));
        let t = terrain(&[c, s1, s2, m]);
        let sources = [tile(s1.0, s1.1), tile(s2.0, s2.1)];
        let input = PlanInput {
            terrain: &t,
            controller: tile(c.0, c.1),
            sources: &sources,
            mineral: Some(tile(m.0, m.1)),
            spawn: None,
        };
        let layout = plan(&input, &mut Scratch::default()).unwrap();
        let report = validate(&input, &layout);
        assert!(report.valid(), "{:?}", report.problems);
        let hub = layout.roles.hub.unwrap();
        let top = allowed(StructureType::Extension, MAX_LEVEL) as usize;
        assert_eq!(
            layout
                .of(StructureType::Extension)
                .filter(|i| i.tile.range(hub) <= 5)
                .count(),
            top
        );
        // A rampart over every protected structure, from the level it stands (at least the first
        // level ramparts are allowed).
        let first = earliest_level(StructureType::Rampart, 0).unwrap();
        let ramparts: Vec<_> = layout.of(StructureType::Rampart).collect();
        for r in &ramparts {
            let under = layout
                .items
                .iter()
                .find(|i| i.tile == r.tile && i.kind != StructureType::Rampart)
                .unwrap();
            assert_eq!(r.level, under.level.max(first), "{r:?} over {under:?}");
        }
        assert!(ramparts.iter().any(|r| r.level > first));
        assert_eq!(stepped(&input), layout);
    }

    #[test]
    fn a_standing_spawn_stays_at_level_one() {
        let (c, s1, s2) = ((25, 8), (8, 40), (42, 40));
        let t = terrain(&[c, s1, s2]);
        let sources = [tile(s1.0, s1.1), tile(s2.0, s2.1)];
        let spawn = tile(25, 30);
        let input = PlanInput {
            terrain: &t,
            controller: tile(c.0, c.1),
            sources: &sources,
            mineral: None,
            spawn: Some(spawn),
        };
        let layout = plan(&input, &mut Scratch::default()).unwrap();
        let report = validate(&input, &layout);
        assert!(report.valid(), "{:?}", report.problems);
        assert!(
            layout
                .of(StructureType::Spawn)
                .any(|i| i.tile == spawn && i.level == 1)
        );
        // The open ground fits the full core around it.
        assert!(layout.roles.hub.unwrap().range(spawn) <= 3);
        assert_eq!(
            layout.count(StructureType::Spawn, MAX_LEVEL),
            allowed(StructureType::Spawn, MAX_LEVEL)
        );
        assert_eq!(stepped(&input), layout);
    }

    /// A player may put the first spawn on ground the economy keeps clear (by the controller or a
    /// source; the game allows it, V237): it stays alone at level 1 and the plan is still valid.
    #[test]
    fn a_standing_spawn_on_kept_clear_ground_stays_alone() {
        let (c, s1, s2) = ((25, 8), (8, 40), (42, 40));
        let t = terrain(&[c, s1, s2]);
        let sources = [tile(s1.0, s1.1), tile(s2.0, s2.1)];
        let clear = crate::economy::placement::CONTROLLER_CLEAR;
        for spawn in [tile(25, 8 + clear), tile(s1.0 + 1, s1.1 - 1)] {
            let input = PlanInput {
                terrain: &t,
                controller: tile(c.0, c.1),
                sources: &sources,
                mineral: None,
                spawn: Some(spawn),
            };
            let layout = plan(&input, &mut Scratch::default()).unwrap();
            let report = validate(&input, &layout);
            assert!(report.valid(), "{spawn:?}: {:?}", report.problems);
            assert!(
                layout
                    .of(StructureType::Spawn)
                    .any(|i| i.tile == spawn && i.level == 1),
                "{spawn:?}"
            );
            assert_eq!(stepped(&input), layout);
        }
    }
}
