//! Flow fields: the cost from every tile of a room to a goal, so every creep heading for the same
//! target reads its next step from one shared table.
//!
//! A field is a Dijkstra search from the goal outward over a [`CostGrid`], with Dial's bucket queue
//! (step costs are small integers, so a ring of 256 buckets replaces the heap). Building one visits
//! each walkable tile once and relaxes its 8 neighbours: about 20 000 relaxations for an open room,
//! whatever the number of creeps that use it. The [`FieldCache`] keeps fields by room, goal and the
//! grid's version, so a field is rebuilt only when the grid changed or it was evicted.

use rustedscreep_consts::{ROOM_AREA, ROOM_SIZE, RoomName};

use super::grid::CostGrid;
use super::tile::Tile;

/// The distance of a tile no goal can be reached from.
pub const UNREACHED: u16 = u16::MAX;

/// How many equally good next steps [`FlowField::steps`] offers, so the traffic manager can pick a
/// free one when the first is taken.
pub const STEP_OPTIONS: usize = 3;

/// Buckets in the ring. Step costs are 1 to 254 ([`super::grid::CostProfile::sanitized`]), so a
/// distance never lands in the bucket being drained.
const BUCKETS: usize = 256;

/// Where a creep wants to be: any walkable tile within `range` (Chebyshev) of `tile`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Goal {
    pub tile: Tile,
    pub range: u8,
}

impl Goal {
    /// Whether `t` is within range of the goal (walkability is the grid's business).
    #[inline]
    pub fn contains(self, t: Tile) -> bool {
        t.range(self.tile) <= self.range
    }
}

/// What a creep heads for. `Near`: any walkable tile within range of the goal, where it stays.
/// `Exit`: a way out of the room toward the next one on an inter-room route, any open exit
/// ([`CostGrid::open_exit`]) on the side of `goal.tile` within `goal.range` of it. A creep steps onto
/// one and the engine moves it to the next room at the end of the tick (V151).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Aim {
    Near(Goal),
    Exit(Goal),
}

impl From<Goal> for Aim {
    fn from(goal: Goal) -> Self {
        Self::Near(goal)
    }
}

impl Aim {
    pub fn goal(self) -> Goal {
        match self {
            Self::Near(g) | Self::Exit(g) => g,
        }
    }

    /// Whether `t` is one of the aim's goal tiles on `grid`.
    #[inline]
    pub fn reaches(self, grid: &CostGrid, t: Tile) -> bool {
        match self {
            Self::Near(g) => g.contains(t) && grid.walkable(t),
            Self::Exit(g) => {
                g.contains(t)
                    && grid.open_exit(t)
                    && t.side().is_some_and(|s| g.tile.side() == Some(s))
            }
        }
    }

    /// The cost of stepping onto `t` for a creep with this aim, or `None` if it may not: walkable
    /// tiles, and for `Exit` its goal tiles too, at the exit cost.
    #[inline]
    pub fn enter_cost(self, grid: &CostGrid, t: Tile) -> Option<u8> {
        if grid.walkable(t) {
            Some(grid.cost(t))
        } else if matches!(self, Self::Exit(_)) && self.reaches(grid, t) {
            Some(grid.exit_cost())
        } else {
            None
        }
    }

    /// The aim's goal tiles on `grid`.
    pub fn tiles(self, grid: &CostGrid) -> impl Iterator<Item = Tile> + '_ {
        let g = self.goal();
        let (gx, gy, r) = (g.tile.x(), g.tile.y(), g.range);
        let last = ROOM_SIZE - 1;
        (gx.saturating_sub(r)..=gx.saturating_add(r).min(last))
            .flat_map(move |x| {
                (gy.saturating_sub(r)..=gy.saturating_add(r).min(last))
                    .filter_map(move |y| Tile::new(x, y))
            })
            .filter(move |&t| self.reaches(grid, t))
    }
}

/// The best next steps from one tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Steps {
    tiles: [Option<Tile>; STEP_OPTIONS],
    /// The cost to the goal through any of them (the step's cost plus the tile's distance).
    pub total: u32,
}

impl Steps {
    /// The steps, best first (ties in direction order).
    pub fn tiles(&self) -> impl Iterator<Item = Tile> + '_ {
        self.tiles.iter().flatten().copied()
    }

    pub fn first(&self) -> Option<Tile> {
        self.tiles.first().copied().flatten()
    }

    pub fn is_empty(&self) -> bool {
        self.first().is_none()
    }

    /// Up to [`STEP_OPTIONS`] tiles, best first.
    pub fn from_tiles(tiles: impl IntoIterator<Item = Tile>, total: u32) -> Self {
        let mut out = Self {
            tiles: [None; STEP_OPTIONS],
            total,
        };
        for (slot, t) in out.tiles.iter_mut().zip(tiles) {
            *slot = Some(t);
        }
        out
    }
}

/// The ring of buckets, kept between builds so a build allocates nothing once warm.
pub struct Dial {
    buckets: Vec<Vec<Tile>>,
}

impl Default for Dial {
    fn default() -> Self {
        Self {
            buckets: (0..BUCKETS).map(|_| Vec::new()).collect(),
        }
    }
}

/// The cost of reaching the goal from each tile: the sum of the step costs of the tiles entered on
/// the way (the start tile's own cost is not counted), [`UNREACHED`] where no goal can be reached.
pub struct FlowField {
    dist: Box<[u16; ROOM_AREA]>,
}

impl Default for FlowField {
    fn default() -> Self {
        Self {
            dist: Box::new([UNREACHED; ROOM_AREA]),
        }
    }
}

impl FlowField {
    #[inline]
    pub fn distance(&self, t: Tile) -> u16 {
        self.dist.get(t.index()).copied().unwrap_or(UNREACHED)
    }

    /// Builds the field for `aim` on `grid` (a [`Goal`] is a `Near` aim). The goal tiles are
    /// [`Aim::tiles`]: for `Near` the walkable tiles within range, for `Exit` the open exits on its
    /// side within range (unwalkable in the grid, so only their neighbours inside the room are
    /// relaxed). Returns how many there are (0 means nothing can reach the aim).
    pub fn compute(&mut self, grid: &CostGrid, aim: impl Into<Aim>, queue: &mut Dial) -> u32 {
        let aim = aim.into();
        self.dist.fill(UNREACHED);
        for b in &mut queue.buckets {
            b.clear();
        }
        let mut pending: usize = 0;
        let mut seeds: u32 = 0;
        for t in aim.tiles(grid) {
            self.set(t, 0);
            if let Some(b) = queue.buckets.first_mut() {
                b.push(t);
            }
            pending += 1;
            seeds += 1;
        }
        let mut d: u32 = 0;
        while pending > 0 {
            let slot = (d as usize) % BUCKETS;
            let Some(bucket) = queue.buckets.get_mut(slot) else {
                break;
            };
            let mut drained = std::mem::take(bucket);
            for &u in &drained {
                pending = pending.saturating_sub(1);
                if u32::from(self.distance(u)) != d {
                    continue; // a stale entry: u was reached more cheaply later
                }
                // The cost of stepping onto u: a goal exit costs the exit step, which is not its
                // (blocked) grid cost.
                let Some(enter) = aim.enter_cost(grid, u) else {
                    continue;
                };
                let next = d + u32::from(enter);
                if next >= u32::from(UNREACHED) {
                    continue;
                }
                for (_, v) in u.neighbors() {
                    if grid.walkable(v) && next < u32::from(self.distance(v)) {
                        self.set(v, next as u16);
                        if let Some(b) = queue.buckets.get_mut((next as usize) % BUCKETS) {
                            b.push(v);
                            pending += 1;
                        }
                    }
                }
            }
            drained.clear();
            if let Some(bucket) = queue.buckets.get_mut(slot) {
                *bucket = drained;
            }
            d += 1;
        }
        seeds
    }

    /// The best next steps from `from` (which need not be walkable itself, so a creep on an edge or
    /// a blocked tile still finds its way): the neighbours with a distance that minimize the step's
    /// cost plus that distance, up to [`STEP_OPTIONS`] ties in direction order. Only walkable tiles
    /// and the goal exits of an `Exit` field have a distance; an exit's step costs
    /// [`CostGrid::exit_cost`]. Empty when no neighbour reaches the goal.
    pub fn steps(&self, grid: &CostGrid, from: Tile) -> Steps {
        let mut best = u32::MAX;
        let mut out = Steps::default();
        let mut n = 0usize;
        for (_, t) in from.neighbors() {
            let dist = self.distance(t);
            if dist == UNREACHED {
                continue;
            }
            // A tile with a distance that is not walkable is a goal exit (only those are seeded
            // off the walkable tiles).
            let enter = if grid.walkable(t) {
                grid.cost(t)
            } else if grid.open_exit(t) {
                grid.exit_cost()
            } else {
                continue;
            };
            let total = u32::from(enter) + u32::from(dist);
            if total < best {
                best = total;
                out = Steps::default();
                n = 0;
            }
            if total == best {
                if let Some(slot) = out.tiles.get_mut(n) {
                    *slot = Some(t);
                    n += 1;
                }
                out.total = total;
            }
        }
        out
    }

    /// Like [`Self::steps`], but the best [`STEP_OPTIONS`] neighbours with a distance whatever their
    /// cost, best first (direction order on ties): for a creep on an edge tile, which leaves the room
    /// if it stays (V151), a costlier way in beats waiting on one taken tile.
    pub fn ranked_steps(&self, grid: &CostGrid, from: Tile) -> Steps {
        let mut ranked: [(u32, usize, Option<Tile>); 8] = [(u32::MAX, 0, None); 8];
        for (i, (_, t)) in from.neighbors().enumerate() {
            let dist = self.distance(t);
            if dist == UNREACHED {
                continue;
            }
            let enter = if grid.walkable(t) {
                grid.cost(t)
            } else if grid.open_exit(t) {
                grid.exit_cost()
            } else {
                continue;
            };
            if let Some(slot) = ranked.get_mut(i) {
                *slot = (u32::from(enter) + u32::from(dist), i, Some(t));
            }
        }
        ranked.sort_unstable();
        let total = ranked.first().map_or(0, |&(total, _, _)| total);
        Steps::from_tiles(ranked.iter().filter_map(|&(_, _, t)| t), total)
    }

    fn set(&mut self, t: Tile, d: u16) {
        if let Some(cell) = self.dist.get_mut(t.index()) {
            *cell = d;
        }
    }
}

struct Entry {
    room: RoomName,
    goal: Aim,
    /// The grid version the field was built on.
    version: u32,
    last_used: u32,
    field: FlowField,
}

/// Cached fields, at most `capacity` of them, least recently used out first. Fields are keyed by room
/// and aim and carry the grid version they were built on: a field of an older grid is rebuilt in
/// place on its next use, and one unused for `idle_ticks` is dropped ([`FieldCache::evict_idle`]).
#[derive(Default)]
pub struct FieldCache {
    entries: Vec<Entry>,
    /// Most fields kept, and whether to keep any (`CacheConfig::flow_fields`), set by
    /// [`FieldCache::set_policy`].
    capacity: usize,
    keep: bool,
    /// The field used when the cache is off (`CacheConfig::flow_fields`, the naive baseline).
    scratch: FlowField,
    queue: Dial,
    built: u32,
    hits: u32,
}

impl FieldCache {
    /// Sets how many fields to keep (at least 1) and whether to keep any; with `keep` false (the
    /// naive baseline) every [`Self::get`] builds into a scratch field and the cache is emptied.
    /// A new cache keeps nothing until this is called.
    pub fn set_policy(&mut self, capacity: usize, keep: bool) {
        self.capacity = capacity.max(1);
        self.keep = keep;
        if !keep {
            self.entries.clear();
        }
        while self.entries.len() > self.capacity {
            self.evict_lru();
        }
    }

    /// The field for `goal` (a [`Goal`] is a `Near` aim) in `room` on a grid at `version`, built if
    /// it is missing or older. Returns the field and whether it was built now.
    pub fn get(
        &mut self,
        room: RoomName,
        goal: impl Into<Aim>,
        grid: &CostGrid,
        version: u32,
        now: u32,
    ) -> (&FlowField, bool) {
        let goal = goal.into();
        if !self.keep {
            self.scratch.compute(grid, goal, &mut self.queue);
            self.built = self.built.saturating_add(1);
            return (&self.scratch, true);
        }
        let found = self
            .entries
            .iter()
            .position(|e| e.room == room && e.goal == goal);
        let index = match found {
            Some(i) => i,
            None => {
                let field = if self.entries.len() < self.capacity.max(1) {
                    FlowField::default()
                } else {
                    self.evict_lru().unwrap_or_default()
                };
                self.entries.push(Entry {
                    room,
                    goal,
                    // Never a grid's version, so the field is built below.
                    version: version.wrapping_add(1),
                    last_used: now,
                    field,
                });
                self.entries.len() - 1
            }
        };
        let (queue, entries) = (&mut self.queue, &mut self.entries);
        let Some(e) = entries.get_mut(index) else {
            // Unreachable: `index` is a position in `entries`. Fall back to the scratch field.
            self.scratch.compute(grid, goal, &mut self.queue);
            return (&self.scratch, true);
        };
        e.last_used = now;
        let built = e.version != version;
        if built {
            e.field.compute(grid, goal, queue);
            e.version = version;
            self.built = self.built.saturating_add(1);
        } else {
            self.hits = self.hits.saturating_add(1);
        }
        (&e.field, built)
    }

    /// Drops fields not used for more than `idle_ticks` ticks (a field used at tick `t` stays through
    /// `t + idle_ticks`). Their memory is freed.
    pub fn evict_idle(&mut self, now: u32, idle_ticks: u32) {
        self.entries
            .retain(|e| now.wrapping_sub(e.last_used) <= idle_ticks);
    }

    /// Removes the least recently used field (on a tie the earliest entry, so it is deterministic)
    /// and returns it for reuse.
    fn evict_lru(&mut self) -> Option<FlowField> {
        let lru = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(i, e)| (e.last_used, *i))
            .map(|(i, _)| i)?;
        Some(self.entries.swap_remove(lru).field)
    }

    /// Drops every field (a global's state is lost the same way on a reset).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Fields built since the global started (cached or scratch).
    pub fn built(&self) -> u32 {
        self.built
    }

    /// Requests answered by a cached field that was still current.
    pub fn hits(&self) -> u32 {
        self.hits
    }
}

#[cfg(test)]
mod tests {
    use super::super::grid::tests::{open_terrain, rates, raw, tile};
    use super::super::grid::{BLOCKED, CostGrid, Footprint, TerrainGrid};
    use super::*;
    use rustedscreep_consts::Terrain;

    fn grid_of(terrain: &TerrainGrid, objects: &[(Tile, Footprint)]) -> CostGrid {
        let mut g = CostGrid::default();
        g.build(terrain, objects.iter().copied(), rates());
        g
    }

    /// A reference Dijkstra over the same definition (O(n^2) scan, no queue), for comparison.
    fn reference(grid: &CostGrid, goal: Goal) -> Vec<u32> {
        let n = ROOM_AREA;
        let mut dist = vec![u32::MAX; n];
        let mut done = vec![false; n];
        for t in (0..n).filter_map(Tile::from_index) {
            if goal.contains(t) && grid.walkable(t) {
                dist[t.index()] = 0;
            }
        }
        loop {
            let next = (0..n)
                .filter(|&i| !done[i] && dist[i] != u32::MAX)
                .min_by_key(|&i| dist[i]);
            let Some(i) = next else { break };
            done[i] = true;
            let u = Tile::from_index(i).expect("tile");
            let step = dist[i] + u32::from(grid.cost(u));
            for (_, v) in u.neighbors() {
                if grid.walkable(v) && step < dist[v.index()] {
                    dist[v.index()] = step;
                }
            }
        }
        dist
    }

    /// Uses: MOVE_COST_PLAIN (through `rates`). On open plain every tile's distance is the
    /// plain cost times its Chebyshev distance to the nearest goal tile.
    #[test]
    fn open_plain_distances_are_chebyshev_times_the_plain_cost() {
        let grid = grid_of(&open_terrain(), &[]);
        let goal = Goal {
            tile: tile(25, 25),
            range: 1,
        };
        let mut f = FlowField::default();
        let seeds = f.compute(&grid, goal, &mut Dial::default());
        assert_eq!(seeds, 9);
        let plain = u32::from(rates().plain);
        for t in (0..ROOM_AREA).filter_map(Tile::from_index) {
            if !grid.walkable(t) {
                assert_eq!(f.distance(t), UNREACHED, "{t:?}");
                continue;
            }
            let steps = u32::from(t.range(goal.tile).saturating_sub(goal.range));
            assert_eq!(u32::from(f.distance(t)), steps * plain, "{t:?}");
        }
    }

    /// The bucket queue gives exactly a plain Dijkstra's distances on a maze of walls, swamps and
    /// roads, for goals of several ranges, including one whose goal tiles are all blocked.
    #[test]
    fn distances_match_a_reference_dijkstra() {
        let wall = Terrain::Wall as u8;
        let swamp = Terrain::Swamp as u8;
        let terrain = TerrainGrid::from_row_major(&raw(|x, y| {
            let h = (u32::from(x) * 7 + u32::from(y) * 13 + u32::from(x) * u32::from(y)) % 11;
            match h {
                0 | 1 => wall,
                2 | 3 => swamp,
                _ => 0,
            }
        }))
        .expect("length");
        let roads: Vec<(Tile, Footprint)> = (5..45)
            .map(|x| (tile(x, 20), Footprint::Road))
            .chain([(tile(30, 30), Footprint::Blocked)])
            .collect();
        let grid = grid_of(&terrain, &roads);
        let mut f = FlowField::default();
        let mut q = Dial::default();
        for (gx, gy, range) in [(25, 25, 1), (3, 40, 0), (40, 8, 3), (30, 30, 0)] {
            let goal = Goal {
                tile: tile(gx, gy),
                range,
            };
            f.compute(&grid, goal, &mut q);
            let want = reference(&grid, goal);
            for t in (0..ROOM_AREA).filter_map(Tile::from_index) {
                let got = f.distance(t);
                let w = want[t.index()];
                let w = if w == u32::MAX {
                    UNREACHED
                } else {
                    u16::try_from(w).expect("fits")
                };
                assert_eq!(got, w, "{t:?} goal {goal:?}");
            }
        }
        // (30, 30) is blocked and has range 0: no goal tile, nothing reachable.
        let blocked = Goal {
            tile: tile(30, 30),
            range: 0,
        };
        assert_eq!(f.compute(&grid, blocked, &mut q), 0);
        assert_eq!(f.distance(tile(31, 31)), UNREACHED);
    }

    /// Uses: MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP. Every route from (10, 25) to (30, 25)
    /// crosses a swamp band at x = 20 once; a road at (20, 26) is within the 20 moves of the shortest
    /// route, so the cheapest route costs 19 plain steps and the road, and following the steps pays
    /// exactly the field's distance, each step lowering it by its own cost.
    #[test]
    fn steps_follow_the_cheapest_route() {
        let swamp = Terrain::Swamp as u8;
        let terrain = TerrainGrid::from_row_major(&raw(|x, _| if x == 20 { swamp } else { 0 }))
            .expect("length");
        let grid = grid_of(&terrain, &[(tile(20, 26), Footprint::Road)]);
        let goal = Goal {
            tile: tile(30, 25),
            range: 0,
        };
        let mut f = FlowField::default();
        f.compute(&grid, goal, &mut Dial::default());
        let p = rates();
        let mut at = tile(10, 25);
        let start = u32::from(f.distance(at));
        assert_eq!(start, 19 * u32::from(p.plain) + u32::from(p.road));
        assert!(
            start < 19 * u32::from(p.plain) + u32::from(p.swamp),
            "the road beats the swamp"
        );
        let mut paid = 0u32;
        let mut crossed = None;
        while at != goal.tile {
            let s = f.steps(&grid, at);
            let next = s.first().expect("a step");
            assert_eq!(
                s.total,
                u32::from(grid.cost(next)) + u32::from(f.distance(next))
            );
            assert!(u32::from(f.distance(next)) < u32::from(f.distance(at)));
            paid += u32::from(grid.cost(next));
            if next.x() == 20 {
                crossed = Some(next);
            }
            at = next;
        }
        assert_eq!(paid, start);
        assert_eq!(crossed, Some(tile(20, 26)));
        assert_eq!(f.distance(goal.tile), 0);
    }

    /// Uses: ROOM_SIZE, MOVE_COST_* (through `rates`). An `Exit` field is seeded on the open exits of
    /// its side within range (never a wall exit, a corner, or another side's tiles near a corner),
    /// a tile beside one is one exit step away (V54), and following the steps from inside the room
    /// ends on one of them. A `Near` goal on the same tile seeds only walkable tiles, so its field
    /// never leads onto an edge.
    #[test]
    fn exit_fields_lead_onto_the_open_exits_of_their_side() {
        let wall = Terrain::Wall as u8;
        let last = ROOM_SIZE - 1;
        let terrain = TerrainGrid::from_row_major(&raw(|x, y| match (x, y) {
            (49, 24) => wall,
            _ => 0,
        }))
        .expect("length");
        let grid = grid_of(&terrain, &[]);
        let p = rates();
        let goal = Goal {
            tile: tile(last, 25),
            range: 1,
        };
        let mut f = FlowField::default();
        let seeds = f.compute(&grid, Aim::Exit(goal), &mut Dial::default());
        assert_eq!(seeds, 2, "(49,25) and (49,26); (49,24) is a wall");
        assert_eq!(f.distance(tile(last, 25)), 0);
        assert_eq!(f.distance(tile(last, 24)), UNREACHED);
        assert_eq!(f.distance(tile(last - 1, 25)), u16::from(grid.exit_cost()));
        assert_eq!(grid.exit_cost(), p.cheapest());
        let beside = f.steps(&grid, tile(last - 1, 24));
        assert_eq!(beside.first(), Some(tile(last, 25)), "the open exit");
        assert_eq!(beside.total, u32::from(grid.exit_cost()));
        let mut at = tile(30, 10);
        for _ in 0..usize::from(ROOM_SIZE) {
            if grid.open_exit(at) {
                break;
            }
            at = f.steps(&grid, at).first().expect("a step");
        }
        assert!(Aim::Exit(goal).reaches(&grid, at), "{at:?}");
        // Near a corner the range box covers the top edge too, but only the right side seeds.
        let corner = Goal {
            tile: tile(last, 1),
            range: 2,
        };
        let seeded: Vec<Tile> = Aim::Exit(corner).tiles(&grid).collect();
        assert_eq!(seeded, vec![tile(last, 1), tile(last, 2), tile(last, 3)]);
        // The same goal as a Near goal: walkable tiles only.
        f.compute(&grid, goal, &mut Dial::default());
        assert_eq!(f.distance(tile(last, 25)), UNREACHED);
        assert_eq!(f.distance(tile(last - 1, 25)), 0);
        assert!(
            f.steps(&grid, tile(last - 2, 25))
                .tiles()
                .all(|t| !t.is_edge())
        );
    }

    /// A creep on an edge tile (blocked in the grid, V151) still gets steps back into the room;
    /// ties come in direction order, at most STEP_OPTIONS of them.
    #[test]
    fn steps_from_a_blocked_tile_and_ties() {
        let grid = grid_of(&open_terrain(), &[]);
        assert_eq!(grid.cost(tile(0, 25)), BLOCKED);
        let goal = Goal {
            tile: tile(10, 25),
            range: 0,
        };
        let mut f = FlowField::default();
        f.compute(&grid, goal, &mut Dial::default());
        let s = f.steps(&grid, tile(0, 25));
        // (1, 24), (1, 25) and (1, 26) are all 9 moves from (10, 25) on open plain.
        let got: Vec<Tile> = s.tiles().collect();
        assert_eq!(got, vec![tile(1, 24), tile(1, 25), tile(1, 26)]);
        assert_eq!(STEP_OPTIONS, 3);
        // Nothing reachable: no steps.
        let walled = grid_of(
            &open_terrain(),
            &tile(10, 25)
                .neighbors()
                .map(|(_, t)| (t, Footprint::Blocked))
                .collect::<Vec<_>>(),
        );
        f.compute(&walled, goal, &mut Dial::default());
        assert!(f.steps(&walled, tile(5, 5)).is_empty());
    }

    /// The cache returns a hit for the same room, goal and version, rebuilds for a new version,
    /// evicts the least recently used at capacity, drops idle fields, and keeps nothing when off.
    #[test]
    fn the_cache_hits_rebuilds_and_evicts() {
        let grid = grid_of(&open_terrain(), &[]);
        let room: RoomName = "W1N1".parse().expect("room");
        let other: RoomName = "W2N1".parse().expect("room");
        let g = |x| Goal {
            tile: tile(x, 10),
            range: 1,
        };
        let mut c = FieldCache::default();
        c.set_policy(2, true);
        assert!(c.get(room, g(10), &grid, 1, 100).1, "built");
        assert!(!c.get(room, g(10), &grid, 1, 101).1, "hit");
        assert!(c.get(room, g(10), &grid, 2, 102).1, "new version");
        assert!(c.get(other, g(10), &grid, 1, 103).1, "other room");
        assert_eq!((c.len(), c.built(), c.hits()), (2, 3, 1));
        // At capacity: g(20) evicts the least recently used, (room, g(10)) at 102.
        assert!(c.get(room, g(20), &grid, 2, 104).1);
        assert_eq!(c.len(), 2);
        assert!(c.get(room, g(10), &grid, 2, 105).1, "it was evicted");
        // Idle: at 105 + 10, fields last used at 104 and 105 stay through 114 and 115.
        c.evict_idle(115, 10);
        assert_eq!(c.len(), 1);
        c.evict_idle(116, 10);
        assert!(c.is_empty());
        // Off: every call builds, nothing is kept.
        c.set_policy(2, false);
        let before = c.built();
        assert!(c.get(room, g(10), &grid, 2, 117).1);
        assert!(c.get(room, g(10), &grid, 2, 117).1);
        assert_eq!((c.len(), c.built()), (0, before + 2));
        // The scratch field is a real field.
        let (f, _) = c.get(room, g(10), &grid, 2, 118);
        assert_eq!(f.distance(tile(10, 10)), 0);
        // Shrinking the capacity evicts the least recently used.
        c.set_policy(3, true);
        for (x, t) in [(10, 120), (20, 121), (30, 122)] {
            c.get(room, g(x), &grid, 2, t);
        }
        c.set_policy(1, true);
        assert_eq!(c.len(), 1);
        assert!(
            !c.get(room, g(30), &grid, 2, 123).1,
            "the most recent one is kept"
        );
    }
}
