//! Checks a plan against the game's rules and the economy's needs, and measures it, so both
//! candidate planners (ADR-007) are judged by one referee. It reads only the [`Layout`] and the
//! room ([`PlanInput`]), never a planner's working state.
//!
//! Validity, level by level (the items with `level <= L` stand):
//! - counts: no type over `CONTROLLER_STRUCTURES` at any level, and every [`REQUIRED`] type exactly
//!   at it (V234), the extractor only with a mineral;
//! - tiles: the site rules for terrain and border (V237), nothing on the controller or a source,
//!   only the extractor on the mineral, no road on a wall (a tunnel's cost, V242), at most one
//!   costed structure and one road and one rampart per tile, no road under an obstacle;
//! - reach: from the first spawn, every obstacle has a reached neighbour (filled or used from it),
//!   every rampart and constructed wall has reached ground within `REPAIR_REACH` (built and
//!   repaired from there, V185, V222), every road and container stands on reached ground, the
//!   controller has reached ground in upgrade range, the mineral a reached neighbour once the extractor stands, and every side of
//!   the room whose exits the bare room reaches still has one;
//! - spawns (V174): every spawn has a free neighbour joined to the room's exits that is no creep's
//!   post (the hub, a mining tile), and no free neighbour but the hub is a dead end a new creep
//!   could be placed in and never leave;
//! - labs (V240): two of the labs that stand are within `LAB_REACTION_RANGE` of every other one;
//! - the hub, from the storage's level: reached, or next to a spawn that stands;
//! - roles (at the top level): the mining tiles, links and containers stand where the economy
//!   reads them as meant (`economy::mining` classifies them by position: a container next to a
//!   source is its mining tile, sources taken in (x, y) order; a link next to a mining tile its
//!   source link; a container or link within `RECEIVER_RANGE` of the controller the controller's),
//!   and no other link or container stands where it would be misread; the hub is next to a spawn;
//!   the controller link stands from the first link level and a source link or the hub link from
//!   the second, so the links that stand send to it (with no more links than allowed, all are
//!   active, V218);
//! - defence (at every level from the first that allows ramparts): every protected structure that
//!   stands is under a rampart that stands or out of ranged reach (V243) of every tile a hostile
//!   creep can reach from an exit past the ramparts and constructed walls that stand.

use rustedscreep_consts::{
    CONSTRUCTION_COST_ROAD_SWAMP_RATIO, LAB_REACTION_RANGE, MOVE_COST_PLAIN, MOVE_COST_ROAD,
    MOVE_COST_SWAMP, ROOM_AREA, StructureType, TOWER_FALLOFF, TOWER_FALLOFF_RANGE,
    TOWER_OPTIMAL_RANGE, TOWER_POWER_ATTACK,
};

use super::layout::{KINDS, Layout, MAX_LEVEL, PlanInput, is_level, kind_rank};
use super::rules::{
    RANGED_REACH, REPAIR_REACH, REQUIRED, allowed, blocks, earliest_level, protected, tile_allows,
};
use crate::economy::RECEIVER_RANGE;
use crate::movement::{
    CostGrid, CostProfile, Dial, FlowField, Footprint, Goal, Side, TerrainKind, Tile, UNREACHED,
};

/// One way a plan breaks a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A level outside 1 to [`MAX_LEVEL`], or below the first level that allows the type.
    Level {
        kind: StructureType,
        tile: (u8, u8),
        level: u8,
    },
    OverLimit {
        kind: StructureType,
        level: u8,
        have: u32,
        allowed: u32,
    },
    Short {
        kind: StructureType,
        level: u8,
        have: u32,
        want: u32,
    },
    BadTile {
        kind: StructureType,
        tile: (u8, u8),
    },
    Overlap {
        tile: (u8, u8),
    },
    RoadUnderObstacle {
        tile: (u8, u8),
    },
    /// An item the first spawn's ground does not reach at `level`: an obstacle with no reached
    /// neighbour, a road or container off reached ground, a rampart or wall with no reached ground
    /// within `REPAIR_REACH`.
    Unreachable {
        kind: StructureType,
        tile: (u8, u8),
        level: u8,
    },
    ControllerUnreachable {
        level: u8,
    },
    ExitLost {
        level: u8,
    },
    /// A protected structure standing at `level` with no rampart standing over it, in ranged
    /// reach (V243) of ground a hostile creep reaches from an exit at that level.
    Exposed {
        kind: StructureType,
        tile: (u8, u8),
        level: u8,
    },
    Role(&'static str),
    /// A spawn with no free neighbour for a new creep (V174): none of its walkable neighbours
    /// leads to the room's exits, or every one that does is a creep's post (the hub, a mining
    /// tile), where a creep stands for good.
    SpawnBlocked {
        tile: (u8, u8),
        level: u8,
    },
    /// A walkable neighbour of a spawn (not the hub) with no way to the room's exits: a creep born
    /// there (the first free direction, V174) can never leave.
    SpawnPocket {
        tile: (u8, u8),
        level: u8,
    },
    /// The labs standing at `level` have no two source labs within `LAB_REACTION_RANGE` of every
    /// other lab (V240), so some of them can never run a reaction.
    Labs {
        level: u8,
    },
}

/// What a plan costs and gives, at the top level. Path costs are flow-field costs from the hub at
/// the movement fatigue rates with the plan's roads (V54, V154).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics {
    pub ramparts: u32,
    pub walls: u32,
    pub roads: u32,
    /// The roads' build cost: `CONSTRUCTION_COST[road]` scaled on swamp (V235).
    pub road_energy: u32,
    pub to_sources: Vec<u32>,
    pub to_controller: Option<u32>,
    pub to_mineral: Option<u32>,
    /// The mean (in hundredths) and the most of each extension's cost from the hub to stand next
    /// to it.
    pub extension_mean_x100: u32,
    pub extension_max: u32,
    /// Walkable tiles in upgrade range of the controller next to its container or link.
    pub posts: u32,
    /// The least total tower damage on a rampart tile (V239), `None` without ramparts or towers.
    pub weakest_rampart_damage: Option<u32>,
    /// Open tiles inside the rampart line (no hostile creep reaches them).
    pub inside: u32,
    pub source_links: u32,
}

/// The problems found and the measures.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub problems: Vec<Problem>,
    pub metrics: Metrics,
}

impl Report {
    pub fn valid(&self) -> bool {
        self.problems.is_empty()
    }
}

fn xy(t: Tile) -> (u8, u8) {
    (t.x(), t.y())
}

fn tiles() -> impl Iterator<Item = Tile> + Clone {
    (0..ROOM_AREA).filter_map(Tile::from_index)
}

/// What stands on each tile, by level.
struct Index {
    /// The costed structure on the tile (not a road or rampart) and its level.
    costed: Vec<Option<(StructureType, u8)>>,
    road: Vec<Option<u8>>,
    rampart: Vec<Option<u8>>,
}

fn get<T: Copy>(v: &[Option<T>], t: Tile) -> Option<T> {
    v.get(t.index()).copied().flatten()
}

/// The tiles within Chebyshev `r` of `t` (itself included), inside the room.
fn within(t: Tile, r: u8) -> impl Iterator<Item = Tile> {
    let r = i16::from(r);
    let (x, y) = (i16::from(t.x()), i16::from(t.y()));
    (-r..=r).flat_map(move |dx| {
        (-r..=r).filter_map(move |dy| {
            let (a, b) = (u8::try_from(x + dx).ok()?, u8::try_from(y + dy).ok()?);
            Tile::new(a, b)
        })
    })
}

/// One flag per tile.
struct Mask(Vec<bool>);

impl Mask {
    fn new() -> Self {
        Self(vec![false; ROOM_AREA])
    }

    fn get(&self, t: Tile) -> bool {
        self.0.get(t.index()).copied().unwrap_or(false)
    }

    /// Sets the flag; whether it was clear.
    fn set(&mut self, t: Tile) -> bool {
        match self.0.get_mut(t.index()) {
            Some(c) if !*c => {
                *c = true;
                true
            }
            _ => false,
        }
    }

    fn clear(&mut self) {
        self.0.iter_mut().for_each(|c| *c = false);
    }
}

/// Floods `reached` (8-connected) over the tiles `walk` accepts, from `seeds`.
fn flood(
    seeds: impl IntoIterator<Item = Tile>,
    walk: impl Fn(Tile) -> bool,
    reached: &mut Mask,
    stack: &mut Vec<Tile>,
) {
    reached.clear();
    stack.clear();
    for t in seeds {
        if walk(t) && reached.set(t) {
            stack.push(t);
        }
    }
    while let Some(t) = stack.pop() {
        for (_, n) in t.neighbors() {
            if walk(n) && reached.set(n) {
                stack.push(n);
            }
        }
    }
}

/// Which sides of the room have an open exit next to a reached tile.
fn exit_sides(reached: &Mask, wall: impl Fn(Tile) -> bool) -> [bool; 4] {
    let mut sides = [false; 4];
    for t in tiles() {
        let Some(side) = t.side() else { continue };
        if wall(t) || !t.neighbors().any(|(_, n)| reached.get(n)) {
            continue;
        }
        if let Some(cell) = Side::ALL
            .iter()
            .position(|&s| s == side)
            .and_then(|i| sides.get_mut(i))
        {
            *cell = true;
        }
    }
    sides
}

/// The damage of one tower at Chebyshev `range` (towers/attack.js, V239).
///
/// Uses: `TOWER_POWER_ATTACK`, `TOWER_OPTIMAL_RANGE`, `TOWER_FALLOFF_RANGE`, `TOWER_FALLOFF`.
pub fn tower_damage(range: u8) -> u32 {
    let power = f64::from(TOWER_POWER_ATTACK);
    if range <= TOWER_OPTIMAL_RANGE {
        return TOWER_POWER_ATTACK;
    }
    let r = range.min(TOWER_FALLOFF_RANGE);
    let drop = power * TOWER_FALLOFF * f64::from(r - TOWER_OPTIMAL_RANGE)
        / f64::from(TOWER_FALLOFF_RANGE - TOWER_OPTIMAL_RANGE);
    (power - drop).floor() as u32
}

/// Whether two of `labs` can be the source labs of every other one (labs/run-reaction.js L26 and
/// L38, V240): each other lab within `LAB_REACTION_RANGE` of both.
///
/// Uses: `LAB_REACTION_RANGE`.
fn labs_react(labs: &[Tile]) -> bool {
    let near = |a: Tile, b: Tile| u32::from(a.range(b)) <= LAB_REACTION_RANGE;
    labs.iter().enumerate().any(|(i, &a)| {
        labs.iter().skip(i + 1).any(|&b| {
            labs.iter()
                .all(|&o| o == a || o == b || (near(o, a) && near(o, b)))
        })
    })
}

/// Checks and measures `layout` for the room `input`.
pub fn validate(input: &PlanInput<'_>, layout: &Layout) -> Report {
    let mut report = Report::default();
    let p = &mut report.problems;
    let terrain = input.terrain;
    let wall = |t: Tile| terrain.kind(t) == TerrainKind::Wall;
    let mut ix = Index {
        costed: vec![None; ROOM_AREA],
        road: vec![None; ROOM_AREA],
        rampart: vec![None; ROOM_AREA],
    };

    // Levels, tiles and overlaps.
    for item in &layout.items {
        let (k, t, l) = (item.kind, item.tile, item.level);
        if !is_level(l) || allowed(k, l) == 0 || kind_rank(k) == KINDS.len() {
            p.push(Problem::Level {
                kind: k,
                tile: xy(t),
                level: l,
            });
        }
        let on_mineral = input.mineral == Some(t);
        let natural = t == input.controller || input.sources.contains(&t);
        let bad = !tile_allows(terrain, k, t)
            || natural
            || (on_mineral != (k == StructureType::Extractor))
            || (k == StructureType::Road && wall(t));
        if bad {
            p.push(Problem::BadTile {
                kind: k,
                tile: xy(t),
            });
        }
        let slot = match k {
            StructureType::Road => ix.road.get_mut(t.index()),
            StructureType::Rampart => ix.rampart.get_mut(t.index()),
            _ => None,
        };
        match slot {
            Some(cell) => {
                if cell.is_some() {
                    p.push(Problem::Overlap { tile: xy(t) });
                }
                *cell = Some(l);
            }
            None => {
                if let Some(cell) = ix.costed.get_mut(t.index()) {
                    if cell.is_some() {
                        p.push(Problem::Overlap { tile: xy(t) });
                    }
                    *cell = Some((k, l));
                }
            }
        }
    }
    for t in tiles() {
        if get(&ix.road, t).is_some() && get(&ix.costed, t).is_some_and(|(k, _)| blocks(k)) {
            p.push(Problem::RoadUnderObstacle { tile: xy(t) });
        }
    }

    // Counts.
    for kind in KINDS {
        for level in 1..=MAX_LEVEL {
            let have = layout.count(kind, level);
            let cap = allowed(kind, level);
            if have > cap {
                p.push(Problem::OverLimit {
                    kind,
                    level,
                    have,
                    allowed: cap,
                });
            }
            if REQUIRED.contains(&kind) {
                let want = if kind == StructureType::Extractor && input.mineral.is_none() {
                    0
                } else {
                    cap
                };
                if have < want {
                    p.push(Problem::Short {
                        kind,
                        level,
                        have,
                        want,
                    });
                }
            }
        }
    }

    // Reach, level by level.
    let first_spawn = layout
        .items
        .iter()
        .filter(|i| i.kind == StructureType::Spawn)
        .min_by_key(|i| (i.level, i.tile.index()))
        .map(|i| i.tile);
    let natural =
        |t: Tile| t == input.controller || input.sources.contains(&t) || input.mineral == Some(t);
    let Some(spawn) = first_spawn else {
        p.push(Problem::Role("no spawn"));
        return report;
    };
    if let Some(fixed) = input.spawn
        && !layout
            .items
            .iter()
            .any(|i| i.kind == StructureType::Spawn && i.tile == fixed && i.level == 1)
    {
        p.push(Problem::Role("the standing spawn is not kept"));
    }
    let mut reached = Mask::new();
    let mut stack = Vec::new();
    let walk_at = |level: u8| {
        let ix = &ix;
        move |t: Tile| {
            !t.is_edge()
                && !wall(t)
                && !natural(t)
                && !get(&ix.costed, t).is_some_and(|(k, l)| l <= level && blocks(k))
        }
    };
    // The bare room (no level stands yet) gives the exits a plan must keep.
    flood(
        spawn.neighbors().map(|(_, n)| n),
        walk_at(0),
        &mut reached,
        &mut stack,
    );
    let bare_exits = exit_sides(&reached, wall);
    let mut failed = Mask::new();
    // The ground joined to the room's exits, where a creep can go anywhere the room lets it (a
    // room with no exit, which the generator never makes, V229, falls back to the spawn's ground).
    let has_exits = bare_exits.iter().any(|&b| b);
    let next_to_exit = |t: &Tile| t.neighbors().any(|(_, n)| n.side().is_some() && !wall(n));
    let roles = &layout.roles;
    let spawns: Vec<(Tile, u8)> = layout
        .of(StructureType::Spawn)
        .map(|i| (i.tile, i.level))
        .collect();
    let storage_from = roles
        .storage
        .and_then(|t| get(&ix.costed, t))
        .filter(|&(k, _)| k == StructureType::Storage)
        .map(|(_, l)| l);
    let (mut joined, mut pockets, mut shut) = (Mask::new(), Mask::new(), Mask::new());
    let mut hub_shut = false;
    for level in 1..=MAX_LEVEL {
        flood(
            spawn.neighbors().map(|(_, n)| n),
            walk_at(level),
            &mut reached,
            &mut stack,
        );
        let near = |t: Tile| t.neighbors().any(|(_, n)| reached.get(n));
        let in_repair_reach = |t: Tile| within(t, REPAIR_REACH).any(|n| reached.get(n));
        for item in layout.items.iter().filter(|i| i.level <= level) {
            let ok = match item.kind {
                StructureType::Rampart | StructureType::Wall => in_repair_reach(item.tile),
                StructureType::Extractor => near(item.tile),
                k if blocks(k) => near(item.tile),
                _ => reached.get(item.tile),
            };
            // Each tile is reported once, at the first level it fails.
            if !ok && failed.set(item.tile) {
                p.push(Problem::Unreachable {
                    kind: item.kind,
                    tile: xy(item.tile),
                    level,
                });
            }
        }
        if !tiles().any(|t| t.range(input.controller) <= RECEIVER_RANGE && reached.get(t)) {
            p.push(Problem::ControllerUnreachable { level });
        }
        let now = exit_sides(&reached, wall);
        if bare_exits.iter().zip(now.iter()).any(|(&b, &n)| b && !n) {
            p.push(Problem::ExitLost { level });
        }

        // Spawns (V174): a new creep goes onto the first free neighbour, and stays put on none
        // that is free. A creep's post (the hub, a mining tile) is never free for long.
        if has_exits {
            flood(
                tiles().filter(next_to_exit),
                walk_at(level),
                &mut joined,
                &mut stack,
            );
        } else {
            joined.0.clone_from(&reached.0);
        }
        let walk = walk_at(level);
        for &(sp, _) in spawns.iter().filter(|&&(_, l)| l <= level) {
            let mut exit = false;
            for (_, n) in sp.neighbors().filter(|&(_, n)| walk(n)) {
                let hub = Some(n) == roles.hub;
                if !joined.get(n) {
                    if !hub && pockets.set(n) {
                        p.push(Problem::SpawnPocket { tile: xy(n), level });
                    }
                } else if !hub && !roles.mining.contains(&n) {
                    exit = true;
                }
            }
            if !exit && shut.set(sp) {
                p.push(Problem::SpawnBlocked {
                    tile: xy(sp),
                    level,
                });
            }
        }

        // Labs (V240).
        let labs: Vec<Tile> = layout
            .items
            .iter()
            .filter(|i| i.kind == StructureType::Lab && i.level <= level)
            .map(|i| i.tile)
            .collect();
        if labs.len() >= 3 && !labs_react(&labs) {
            p.push(Problem::Labs { level });
        }

        // The hub, once the storage stands: the filler walks there, or is spawned onto it.
        if let Some(h) = roles.hub
            && storage_from.is_some_and(|l| l <= level)
            && !reached.get(h)
            && !spawns.iter().any(|&(sp, l)| l <= level && sp.range(h) == 1)
        {
            hub_shut = true;
        }
    }

    // Roles.
    let r = roles;
    let kind_at = |t: Tile| get(&ix.costed, t).map(|(k, _)| k);
    let mut role = |ok: bool, what: &'static str| {
        if !ok {
            p.push(Problem::Role(what));
        }
    };
    let ctrl_range = |t: Tile| t.range(input.controller);
    match (r.storage, r.hub, r.hub_link) {
        (Some(s), Some(h), Some(l)) => {
            role(
                kind_at(s) == Some(StructureType::Storage),
                "the storage role holds no storage",
            );
            role(
                kind_at(l) == Some(StructureType::Link),
                "the hub link role holds no link",
            );
            role(
                h.range(s) == 1 && h.range(l) == 1,
                "the hub is not next to the storage and the hub link",
            );
            role(
                !h.is_edge() && !wall(h) && !kind_at(h).is_some_and(blocks),
                "the hub tile is blocked",
            );
            role(
                ctrl_range(l) > RECEIVER_RANGE,
                "the hub link would read as a controller link",
            );
        }
        _ => role(false, "no storage, hub or hub link"),
    }
    if let Some(h) = r.hub {
        role(
            spawns.iter().any(|&(sp, _)| sp.range(h) == 1),
            "the hub is not next to a spawn",
        );
        role(!r.mining.contains(&h), "the hub is a mining tile");
        role(!hub_shut, "the hub cannot be reached");
    }
    role(
        r.mining.len() == input.sources.len(),
        "not one mining tile per source",
    );
    role(
        r.source_links.len() == input.sources.len(),
        "not one source link entry per source",
    );
    for (i, (&m, &s)) in r.mining.iter().zip(input.sources).enumerate() {
        role(
            m.range(s) == 1 && !m.is_edge(),
            "a mining tile is not next to its source",
        );
        role(
            kind_at(m) == Some(StructureType::Container),
            "a mining tile holds no container",
        );
        role(
            !r.mining.iter().take(i).any(|&o| o == m),
            "two sources share a mining tile",
        );
        if let Some(Some(l)) = r.source_links.get(i) {
            role(
                kind_at(*l) == Some(StructureType::Link),
                "a source link role holds no link",
            );
            role(
                l.range(m) == 1,
                "a source link is not next to its mining tile",
            );
            role(
                ctrl_range(*l) > RECEIVER_RANGE,
                "a source link would read as a controller link",
            );
            report.metrics.source_links += 1;
        }
    }
    match r.controller_container {
        Some(c) => {
            role(
                kind_at(c) == Some(StructureType::Container),
                "the controller container role holds no container",
            );
            role(
                ctrl_range(c) <= RECEIVER_RANGE,
                "the controller container is out of range",
            );
            role(
                !input.sources.iter().any(|s| s.range(c) <= 1),
                "the controller container is next to a source",
            );
        }
        None => role(false, "no controller container"),
    }
    match r.controller_link {
        Some(c) => {
            role(
                kind_at(c) == Some(StructureType::Link),
                "the controller link role holds no link",
            );
            role(
                ctrl_range(c) <= RECEIVER_RANGE,
                "the controller link is out of range",
            );
        }
        None => role(false, "no controller link"),
    }
    match (input.mineral, r.mineral_container) {
        (Some(m), Some(c)) => {
            role(
                kind_at(c) == Some(StructureType::Container),
                "the mineral container role holds no container",
            );
            role(
                c.range(m) == 1,
                "the mineral container is not next to the mineral",
            );
        }
        (None, None) => {}
        _ => role(false, "the mineral container does not match the mineral"),
    }
    // What `economy::mining` reads by position, for role and plain structures alike: a source's
    // mining tile is its neighbour with a container, the sources taken in (x, y) order (the tile
    // order) and each one's tile off the later ones' ground; a mining tile's source link is the link
    // next to it (the controller link when there is no source link); the controller's container
    // and link are those within `RECEIVER_RANGE` (a container next to a source excepted).
    let link_roles: Vec<Tile> = r
        .hub_link
        .into_iter()
        .chain(r.controller_link)
        .chain(r.source_links.iter().flatten().copied())
        .collect();
    let mut order: Vec<(Tile, usize)> = input.sources.iter().copied().zip(0..).collect();
    order.sort_unstable();
    for (rank, &(source, i)) in order.iter().enumerate() {
        let mine = r.mining.get(i).copied();
        let earlier: Vec<Tile> = order
            .iter()
            .take(rank)
            .filter_map(|&(_, j)| r.mining.get(j).copied())
            .collect();
        for c in layout.of(StructureType::Container).map(|c| c.tile) {
            if c.range(source) <= 1 && Some(c) != mine && !earlier.contains(&c) {
                role(false, "a container next to a source is not its mining tile");
            }
        }
    }
    for (i, &m) in r.mining.iter().enumerate() {
        let own = r
            .source_links
            .get(i)
            .copied()
            .flatten()
            .or(r.controller_link);
        for l in layout.of(StructureType::Link).map(|l| l.tile) {
            if l.range(m) <= 1 && Some(l) != own {
                role(false, "a link next to a mining tile is not its source link");
            }
        }
    }
    for item in &layout.items {
        let t = item.tile;
        if item.kind == StructureType::Link && !link_roles.contains(&t) {
            role(
                ctrl_range(t) > RECEIVER_RANGE,
                "a link would read as a controller link",
            );
        }
        let near_source = input.sources.iter().any(|s| s.range(t) <= 1);
        if item.kind == StructureType::Container
            && r.controller_container != Some(t)
            && !near_source
        {
            role(
                ctrl_range(t) > RECEIVER_RANGE,
                "a container would read as the controller container",
            );
        }
    }
    // The links' levels (V218): the `allowed` links nearest the controller are active, so with no
    // more links than allowed every link that stands is; then the controller link must stand from
    // the first link level, and from the second a link that sends to it (a source link, or the
    // hub link with the storage's energy).
    let link_level = |t: Tile| {
        get(&ix.costed, t)
            .filter(|&(k, _)| k == StructureType::Link)
            .map(|(_, l)| l)
    };
    let first = layout.of(StructureType::Link).map(|i| i.level).min();
    if let (Some(first), Some(c)) = (first, r.controller_link) {
        role(
            link_level(c) == Some(first),
            "the controller link does not stand from the first link level",
        );
    }
    let sender = r
        .hub_link
        .into_iter()
        .chain(r.source_links.iter().flatten().copied())
        .filter_map(link_level)
        .min();
    if let Some(second) = (1..=MAX_LEVEL).find(|&l| layout.count(StructureType::Link, l) >= 2) {
        role(
            sender.is_some_and(|l| l <= second),
            "no source link or hub link stands with the second link",
        );
    }

    // Defence, at every level from the first that allows ramparts: hostile reach from the exits
    // past the ramparts and walls that stand. Each tile is reported once, at the first level.
    let remote: Vec<Tile> = r
        .controller_link
        .into_iter()
        .chain(r.source_links.iter().flatten().copied())
        .collect();
    let mut hostile = Mask::new();
    let mut exposed = Mask::new();
    let first_rampart = earliest_level(StructureType::Rampart, 0).unwrap_or(MAX_LEVEL);
    for level in first_rampart..=MAX_LEVEL {
        let covered = |t: Tile| get(&ix.rampart, t).is_some_and(|l| l <= level);
        let open = |t: Tile| {
            !(wall(t)
                || natural(t)
                || covered(t)
                || get(&ix.costed, t).is_some_and(|(k, l)| k == StructureType::Wall && l <= level))
        };
        flood(
            tiles().filter(|t| t.is_edge()),
            open,
            &mut hostile,
            &mut stack,
        );
        for item in layout.items.iter().filter(|i| i.level <= level) {
            let t = item.tile;
            if !protected(item.kind, remote.contains(&t)) || covered(t) {
                continue;
            }
            if within(t, RANGED_REACH).any(|h| hostile.get(h)) && exposed.set(t) {
                p.push(Problem::Exposed {
                    kind: item.kind,
                    tile: xy(t),
                    level,
                });
            }
        }
    }

    // Measures.
    let m = &mut report.metrics;
    m.ramparts = layout.count(StructureType::Rampart, MAX_LEVEL);
    m.walls = layout.count(StructureType::Wall, MAX_LEVEL);
    m.roads = layout.count(StructureType::Road, MAX_LEVEL);
    let road_cost = StructureType::Road.construction_cost().unwrap_or(0);
    m.road_energy = layout
        .of(StructureType::Road)
        .map(|i| {
            if terrain.kind(i.tile) == TerrainKind::Swamp {
                road_cost * CONSTRUCTION_COST_ROAD_SWAMP_RATIO
            } else {
                road_cost
            }
        })
        .sum();
    m.inside = tiles()
        .filter(|&t| {
            !t.is_edge()
                && !wall(t)
                && !natural(t)
                && !hostile.get(t)
                && !kind_at(t).is_some_and(blocks)
        })
        .count() as u32;
    let towers: Vec<Tile> = layout.of(StructureType::Tower).map(|i| i.tile).collect();
    if !towers.is_empty() {
        m.weakest_rampart_damage = layout
            .of(StructureType::Rampart)
            .map(|i| {
                towers
                    .iter()
                    .map(|&w| tower_damage(w.range(i.tile)))
                    .sum::<u32>()
            })
            .min();
    }
    if let Some(hub) = r.hub {
        let c = |v: u32| u8::try_from(v).unwrap_or(u8::MAX);
        let profile = CostProfile {
            road: c(MOVE_COST_ROAD),
            plain: c(MOVE_COST_PLAIN),
            swamp: c(MOVE_COST_SWAMP),
        };
        let mut grid = CostGrid::default();
        let objects = tiles().filter_map(|t| {
            if kind_at(t).is_some_and(blocks) {
                Some((t, Footprint::Blocked))
            } else if get(&ix.road, t).is_some() {
                Some((t, Footprint::Road))
            } else {
                None
            }
        });
        grid.build(terrain, objects, profile);
        let mut field = FlowField::default();
        let mut dial = Dial::default();
        field.compute(
            &grid,
            Goal {
                tile: hub,
                range: 0,
            },
            &mut dial,
        );
        let cost = |t: Tile| {
            let d = field.distance(t);
            (d != UNREACHED).then_some(u32::from(d))
        };
        let beside = |t: Tile| t.neighbors().filter_map(|(_, n)| cost(n)).min();
        m.to_sources = r.mining.iter().filter_map(|&t| cost(t)).collect();
        m.to_controller = r.controller_container.and_then(cost);
        m.to_mineral = r.mineral_container.and_then(cost);
        let ext: Vec<u32> = layout
            .of(StructureType::Extension)
            .filter_map(|i| beside(i.tile))
            .collect();
        if !ext.is_empty() {
            m.extension_mean_x100 = ext.iter().sum::<u32>() * 100 / ext.len() as u32;
            m.extension_max = ext.iter().copied().max().unwrap_or(0);
        }
    }
    let feeds: Vec<Tile> = r
        .controller_container
        .into_iter()
        .chain(r.controller_link)
        .collect();
    m.posts = tiles()
        .filter(|&t| {
            t.range(input.controller) <= RECEIVER_RANGE
                && !t.is_edge()
                && !wall(t)
                && !natural(t)
                && !kind_at(t).is_some_and(blocks)
                && feeds.iter().any(|f| f.range(t) <= 1)
        })
        .count() as u32;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::TerrainGrid;
    use crate::planner::layout::{Item, Roles};
    use rustedscreep_consts::{ROOM_SIZE, Terrain};

    fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).expect("in room")
    }

    fn terrain(walls: &[(u8, u8)]) -> TerrainGrid {
        let mut raw = vec![0u8; ROOM_AREA];
        for &(x, y) in walls {
            raw[usize::from(y) * usize::from(ROOM_SIZE) + usize::from(x)] = Terrain::Wall as u8;
        }
        TerrainGrid::from_row_major(&raw).expect("length")
    }

    fn it(kind: StructureType, x: u8, y: u8, level: u8) -> Item {
        Item {
            kind,
            tile: tile(x, y),
            level,
        }
    }

    /// A room with the controller at (10, 10), `sources` and `mineral` (each on a wall, V228) and
    /// any extra `walls`, judged with `items` and `roles`.
    fn judge(
        walls: &[(u8, u8)],
        sources: &[Tile],
        mineral: Option<Tile>,
        items: Vec<Item>,
        roles: Roles,
    ) -> Vec<Problem> {
        let controller = tile(10, 10);
        let mut all: Vec<(u8, u8)> = walls.to_vec();
        all.extend(
            sources
                .iter()
                .chain(mineral.iter())
                .chain([&controller])
                .map(|t| xy(*t)),
        );
        let t = terrain(&all);
        let input = PlanInput {
            terrain: &t,
            controller,
            sources,
            mineral,
            spawn: None,
        };
        let mut layout = Layout {
            items,
            roles,
            needs: Vec::new(),
        };
        layout.sort();
        validate(&input, &layout).problems
    }

    /// `n` extensions on `at`, staged by the level counts (V234).
    fn extensions(at: &[(u8, u8)]) -> Vec<Item> {
        at.iter()
            .enumerate()
            .map(|(n, &(x, y))| {
                let level =
                    crate::planner::rules::earliest_level(StructureType::Extension, n as u32)
                        .expect("allowed");
                it(StructureType::Extension, x, y, level)
            })
            .collect()
    }

    /// V240: an output lab must be within `LAB_REACTION_RANGE` of both source labs, so labs in a
    /// row 3 apart cannot react; a triangle and the usual 2 + 8 cluster can, at every level.
    #[test]
    fn labs_that_cannot_all_react_are_rejected() {
        let spawn = it(StructureType::Spawn, 25, 25, 1);
        let gap = u8::try_from(LAB_REACTION_RANGE).expect("small") + 1;
        let row = vec![
            spawn,
            it(StructureType::Lab, 20, 30, 6),
            it(StructureType::Lab, 20 + gap, 30, 6),
            it(StructureType::Lab, 20 + 2 * gap, 30, 6),
        ];
        let p = judge(&[], &[tile(40, 10)], None, row, Roles::default());
        assert!(p.contains(&Problem::Labs { level: 6 }), "{p:?}");
        let triangle = vec![
            spawn,
            it(StructureType::Lab, 20, 30, 6),
            it(StructureType::Lab, 22, 30, 6),
            it(StructureType::Lab, 21, 32, 6),
        ];
        let p = judge(&[], &[tile(40, 10)], None, triangle, Roles::default());
        assert!(
            !p.iter().any(|x| matches!(x, Problem::Labs { .. })),
            "{p:?}"
        );
        // Source labs (21, 31) and (23, 31); every other lab within 2 of both, 3, 6 and 10 labs.
        let cluster = [
            (21, 31, 6),
            (23, 31, 6),
            (22, 29, 6),
            (21, 29, 7),
            (23, 29, 7),
            (22, 30, 7),
            (21, 33, 8),
            (22, 33, 8),
            (23, 33, 8),
            (22, 32, 8),
        ];
        let mut items = vec![spawn];
        items.extend(
            cluster
                .iter()
                .map(|&(x, y, l)| it(StructureType::Lab, x, y, l)),
        );
        let p = judge(&[], &[tile(40, 10)], None, items, Roles::default());
        assert!(
            !p.iter().any(|x| matches!(x, Problem::Labs { .. })),
            "{p:?}"
        );
    }

    /// V174: a new creep needs a free neighbour of its spawn; the hub's filler stands on the hub for
    /// good, so a spawn whose only free neighbour is the hub cannot spawn.
    #[test]
    fn a_spawn_whose_only_exit_is_the_hub_cannot_spawn() {
        let ring = [
            (24, 24),
            (25, 24),
            (26, 24),
            (24, 25),
            (24, 26),
            (25, 26),
            (26, 26),
        ];
        let mut items = vec![it(StructureType::Spawn, 25, 25, 1)];
        items.extend(extensions(&ring));
        let level = items.iter().map(|i| i.level).max().expect("items");
        let hub = Roles {
            hub: Some(tile(26, 25)),
            ..Roles::default()
        };
        let p = judge(&[], &[tile(40, 10)], None, items.clone(), hub);
        assert!(
            p.contains(&Problem::SpawnBlocked {
                tile: (25, 25),
                level
            }),
            "{p:?}"
        );
        let p = judge(&[], &[tile(40, 10)], None, items, Roles::default());
        assert!(
            !p.iter().any(|x| matches!(x, Problem::SpawnBlocked { .. })),
            "{p:?}"
        );
    }

    /// V174: the spawn places a new creep on its first free neighbour; one walled in by
    /// extensions is a dead end the creep never leaves.
    #[test]
    fn a_spawn_beside_a_dead_end_traps_its_creeps() {
        let walls_in = [
            (23, 24),
            (23, 25),
            (23, 26),
            (24, 24),
            (24, 26),
            (25, 24),
            (25, 26),
        ];
        let mut items = vec![it(StructureType::Spawn, 25, 25, 1)];
        items.extend(extensions(&walls_in));
        let level = items.iter().map(|i| i.level).max().expect("items");
        let p = judge(&[], &[tile(40, 10)], None, items, Roles::default());
        assert!(
            p.contains(&Problem::SpawnPocket {
                tile: (24, 25),
                level
            }),
            "{p:?}"
        );
        // With (23, 25) open the tile leads out.
        let open = [(23, 24), (23, 26), (24, 24), (24, 26), (25, 24), (25, 26)];
        let mut items = vec![it(StructureType::Spawn, 25, 25, 1)];
        items.extend(extensions(&open));
        let p = judge(&[], &[tile(40, 10)], None, items, Roles::default());
        assert!(
            !p.iter().any(|x| matches!(x, Problem::SpawnPocket { .. })),
            "{p:?}"
        );
    }

    /// V222: a rampart is built and repaired from within `REPAIR_REACH`; one in a pocket the
    /// spawn's ground never comes that close to is never built or kept.
    #[test]
    fn a_rampart_out_of_repair_reach_is_unreachable() {
        let c = 33u8;
        let (lo, hi) = (c - REPAIR_REACH, c + REPAIR_REACH);
        let mut ring = Vec::new();
        for i in lo..=hi {
            ring.extend([(i, lo), (i, hi), (lo, i), (hi, i)]);
        }
        let level =
            crate::planner::rules::earliest_level(StructureType::Rampart, 0).expect("ramparts");
        let items = vec![
            it(StructureType::Spawn, 20, 20, 1),
            it(StructureType::Rampart, c, c, level),
            it(StructureType::Rampart, c - 1, c, level),
        ];
        let p = judge(&ring, &[tile(40, 10)], None, items, Roles::default());
        let away = |x: u8| Problem::Unreachable {
            kind: StructureType::Rampart,
            tile: (x, c),
            level,
        };
        assert!(p.contains(&away(c)), "{p:?}");
        assert!(!p.contains(&away(c - 1)), "{p:?}");
    }

    /// A constructed wall is part of the barrier (hostile creeps must break it, V52, V244), not a
    /// structure the ramparts guard: one in a gap of the terrain is not exposed.
    #[test]
    fn a_constructed_wall_in_the_line_is_not_exposed() {
        let mut ring = Vec::new();
        for i in 10..=30u8 {
            ring.extend([(i, 10), (i, 30), (10, i), (30, i)]);
        }
        ring.retain(|&t| t != (20, 10));
        let items = vec![
            it(StructureType::Spawn, 20, 22, 1),
            it(StructureType::Wall, 20, 10, 2),
        ];
        let p = judge(&ring, &[tile(25, 25)], None, items, Roles::default());
        assert!(
            !p.iter().any(|x| matches!(x, Problem::Exposed { .. })),
            "{p:?}"
        );
    }

    /// Defence holds level by level: a rampart that closes the only way in from a later level
    /// than the structure behind it leaves that structure exposed until then.
    #[test]
    fn a_line_that_closes_late_exposes_what_stands_behind_it() {
        let mut ring = Vec::new();
        for i in 10..=30u8 {
            ring.extend([(i, 10), (i, 30), (10, i), (30, i)]);
        }
        ring.retain(|&t| t != (20, 10));
        let first = earliest_level(StructureType::Rampart, 0).expect("ramparts");
        let late = first + 2;
        let items = |door: u8| {
            vec![
                it(StructureType::Spawn, 20, 22, 1),
                it(StructureType::Rampart, 20, 10, door),
            ]
        };
        let p = judge(&ring, &[tile(25, 25)], None, items(late), Roles::default());
        assert!(
            p.contains(&Problem::Exposed {
                kind: StructureType::Spawn,
                tile: (20, 22),
                level: first
            }),
            "{p:?}"
        );
        let p = judge(&ring, &[tile(25, 25)], None, items(first), Roles::default());
        assert!(
            !p.iter().any(|x| matches!(x, Problem::Exposed { .. })),
            "{p:?}"
        );
    }

    /// V218: the active links are the allowed ones nearest the controller. The controller link
    /// must stand from the first link level, and a link that sends to it from the second link.
    #[test]
    fn links_follow_the_level_order() {
        let late = "the controller link does not stand from the first link level";
        let idle = "no source link or hub link stands with the second link";
        let roles = Roles {
            controller_link: Some(tile(12, 12)),
            hub_link: Some(tile(30, 30)),
            mining: vec![tile(39, 11)],
            source_links: vec![Some(tile(38, 12))],
            ..Roles::default()
        };
        let source = [tile(40, 10)];
        let links = |c: u8, s: u8, h: u8| {
            vec![
                it(StructureType::Spawn, 25, 25, 1),
                it(StructureType::Container, 39, 11, 1),
                it(StructureType::Link, 12, 12, c),
                it(StructureType::Link, 38, 12, s),
                it(StructureType::Link, 30, 30, h),
            ]
        };
        let p = judge(&[], &source, None, links(6, 5, 5), roles.clone());
        assert!(p.contains(&Problem::Role(late)), "{p:?}");
        let p = judge(&[], &source, None, links(5, 5, 6), roles.clone());
        assert!(
            !p.contains(&Problem::Role(late)) && !p.contains(&Problem::Role(idle)),
            "{p:?}"
        );
        let p = judge(&[], &source, None, links(5, 6, 5), roles.clone());
        assert!(
            !p.contains(&Problem::Role(idle)),
            "the hub link sends: {p:?}"
        );
        // A second link at level 5 that is neither a source link nor the hub link.
        let mut items = links(5, 6, 7);
        items.push(it(StructureType::Link, 30, 40, 5));
        let p = judge(&[], &source, None, items, roles);
        assert!(p.contains(&Problem::Role(idle)), "{p:?}");
    }

    /// `economy::mining` reads a source's mining tile as its neighbour with a container, sources in
    /// (x, y) order, nearest the spawn among several: a container next to a source must be its
    /// mining tile or an earlier source's.
    #[test]
    fn containers_next_to_a_source_are_read_as_its_mining_tile() {
        let misread = "a container next to a source is not its mining tile";
        let sources = [tile(20, 20), tile(22, 20)];
        let with = |a: (u8, u8), b: (u8, u8)| {
            let items = vec![
                it(StructureType::Spawn, 30, 30, 1),
                it(StructureType::Container, a.0, a.1, 1),
                it(StructureType::Container, b.0, b.1, 1),
            ];
            let roles = Roles {
                mining: vec![tile(a.0, a.1), tile(b.0, b.1)],
                ..Roles::default()
            };
            judge(&[], &sources, None, items, roles)
        };
        // The later source's tile is next to the earlier source: the earlier one may take it.
        let p = with((19, 20), (21, 20));
        assert!(p.contains(&Problem::Role(misread)), "{p:?}");
        // The earlier source's tile next to the later one is taken by then.
        let p = with((21, 20), (23, 20));
        assert!(!p.contains(&Problem::Role(misread)), "{p:?}");
    }

    /// The mineral container is a container like any other to the economy: next to a source it
    /// reads as a mining tile, within `RECEIVER_RANGE` of the controller as its container.
    #[test]
    fn the_mineral_container_is_not_misread() {
        let near_source = "a container next to a source is not its mining tile";
        let near_controller = "a container would read as the controller container";
        let source = [tile(40, 20)];
        let case = |mineral: Tile, container: (u8, u8)| {
            let items = vec![
                it(StructureType::Spawn, 30, 30, 1),
                it(StructureType::Container, 39, 21, 1),
                it(StructureType::Extractor, mineral.x(), mineral.y(), 6),
                it(StructureType::Container, container.0, container.1, 6),
            ];
            let roles = Roles {
                mining: vec![tile(39, 21)],
                mineral_container: Some(tile(container.0, container.1)),
                ..Roles::default()
            };
            judge(&[], &source, Some(mineral), items, roles)
        };
        let p = case(tile(42, 18), (41, 19));
        assert!(p.contains(&Problem::Role(near_source)), "{p:?}");
        let p = case(tile(13, 13), (12, 12));
        assert!(p.contains(&Problem::Role(near_controller)), "{p:?}");
        let p = case(tile(30, 40), (30, 39));
        assert!(
            !p.contains(&Problem::Role(near_source))
                && !p.contains(&Problem::Role(near_controller)),
            "{p:?}"
        );
    }

    /// `economy::mining` takes the link next to a mining tile as the source's link (the lowest
    /// (x, y) among several): any other link there, the hub link included, would be misread.
    #[test]
    fn links_next_to_a_mining_tile_are_read_as_its_source_link() {
        let misread = "a link next to a mining tile is not its source link";
        let source = [tile(40, 10)];
        let case = |hub_link: (u8, u8)| {
            let items = vec![
                it(StructureType::Spawn, 25, 25, 1),
                it(StructureType::Container, 39, 11, 1),
                it(StructureType::Link, 12, 12, 5),
                it(StructureType::Link, 40, 12, 5),
                it(StructureType::Link, hub_link.0, hub_link.1, 6),
            ];
            let roles = Roles {
                controller_link: Some(tile(12, 12)),
                hub_link: Some(tile(hub_link.0, hub_link.1)),
                mining: vec![tile(39, 11)],
                source_links: vec![Some(tile(40, 12))],
                ..Roles::default()
            };
            judge(&[], &source, None, items, roles)
        };
        let p = case((38, 12));
        assert!(p.contains(&Problem::Role(misread)), "{p:?}");
        let p = case((30, 30));
        assert!(!p.contains(&Problem::Role(misread)), "{p:?}");
    }

    /// The hub is the filler's post next to the storage, the hub link and a spawn (`Roles`), and
    /// the filler must be able to get there.
    #[test]
    fn the_hub_is_next_to_a_spawn_and_can_be_reached() {
        let lonely = "the hub is not next to a spawn";
        let shut = "the hub cannot be reached";
        let roles = Roles {
            hub: Some(tile(30, 30)),
            storage: Some(tile(29, 30)),
            hub_link: Some(tile(31, 30)),
            ..Roles::default()
        };
        let core = |ring: &[(u8, u8)], spawn: (u8, u8)| {
            let mut items = vec![
                it(StructureType::Spawn, spawn.0, spawn.1, 1),
                it(StructureType::Storage, 29, 30, 4),
                it(StructureType::Link, 31, 30, 5),
            ];
            items.extend(extensions(ring));
            items
        };
        let p = judge(
            &[],
            &[tile(40, 10)],
            None,
            core(&[], (20, 20)),
            roles.clone(),
        );
        assert!(p.contains(&Problem::Role(lonely)), "{p:?}");
        let p = judge(
            &[],
            &[tile(40, 10)],
            None,
            core(&[], (30, 29)),
            roles.clone(),
        );
        assert!(
            !p.contains(&Problem::Role(lonely)) && !p.contains(&Problem::Role(shut)),
            "{p:?}"
        );
        // Shut in by extensions and away from every spawn.
        let ring = [(29, 29), (30, 29), (31, 29), (29, 31), (30, 31), (31, 31)];
        let p = judge(
            &[],
            &[tile(40, 10)],
            None,
            core(&ring, (20, 20)),
            roles.clone(),
        );
        assert!(p.contains(&Problem::Role(shut)), "{p:?}");
        let p = judge(
            &[],
            &[tile(40, 10)],
            None,
            core(&ring[..5], (20, 20)),
            roles,
        );
        assert!(!p.contains(&Problem::Role(shut)), "{p:?}");
    }

    /// V239: full power within the optimal range, the floor of the falloff past it, capped at the
    /// falloff range.
    #[test]
    fn tower_damage_follows_the_falloff() {
        assert_eq!(tower_damage(0), TOWER_POWER_ATTACK);
        assert_eq!(tower_damage(TOWER_OPTIMAL_RANGE), TOWER_POWER_ATTACK);
        let floor = (f64::from(TOWER_POWER_ATTACK) * (1.0 - TOWER_FALLOFF)).floor() as u32;
        assert_eq!(tower_damage(TOWER_FALLOFF_RANGE), floor);
        assert_eq!(tower_damage(TOWER_FALLOFF_RANGE + 10), floor);
        for r in TOWER_OPTIMAL_RANGE..TOWER_FALLOFF_RANGE {
            assert!(tower_damage(r) > tower_damage(r + 1), "{r}");
        }
    }
}
