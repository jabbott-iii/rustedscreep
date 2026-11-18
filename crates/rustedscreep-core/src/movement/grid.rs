//! In-room cost grids: terrain plus what stands on it.
//!
//! A [`CostGrid`] holds one byte per tile: the cost of stepping onto it, or [`BLOCKED`]. It is built
//! from the room's [`TerrainGrid`] and its sources and minerals (read once per room), the footprints
//! of its structures, own sites and deposits (rescanned on a cadence), and a [`CostProfile`] from
//! config. Creeps are not in it: they move, so the
//! traffic manager and the detour search handle them per tick.
//!
//! Edge tiles are [`BLOCKED`] for in-room movement (a creep that ends a tick on one changes rooms,
//! V151), but the grid remembers which of them are open exits ([`CostGrid::open_exit`]): an
//! inter-room route steps onto one on purpose, and PathFinder gets them back as terrain tiles
//! ([`CostGrid::write_matrix`]).

use rustedscreep_consts::{ROOM_AREA, ROOM_SIZE, StructureType, Terrain};

use super::tile::Tile;

/// The grid value of a tile no creep can stand on or should end a tick on. The same value means
/// "unwalkable" in a PathFinder CostMatrix.
pub const BLOCKED: u8 = u8::MAX;

/// A room's terrain in tile order (column-major), as the raw masks (`Terrain::Wall as u8`,
/// `Terrain::Swamp as u8`; V86, V156).
#[derive(Clone)]
pub struct TerrainGrid {
    mask: Box<[u8; ROOM_AREA]>,
}

/// One tile's terrain, with wall precedence over swamp (a raw 3 reads as wall, V86, V156).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerrainKind {
    Plain,
    Swamp,
    Wall,
}

impl TerrainGrid {
    /// From `Room.Terrain.getRawBuffer()`'s bytes: `ROOM_AREA` of them, row-major (`y * 50 + x`,
    /// V156). `None` when the length is wrong.
    ///
    /// Uses: `ROOM_AREA`, `ROOM_SIZE`.
    pub fn from_row_major(raw: &[u8]) -> Option<Self> {
        if raw.len() != ROOM_AREA {
            return None;
        }
        let mut mask = Box::new([0u8; ROOM_AREA]);
        let size = usize::from(ROOM_SIZE);
        for (row_major, &byte) in raw.iter().enumerate() {
            let (y, x) = (row_major / size, row_major % size);
            if let Some(cell) = mask.get_mut(x * size + y) {
                *cell = byte;
            }
        }
        Some(Self { mask })
    }

    /// The terrain at `t`. Uses: the `Terrain` discriminants (TERRAIN_MASK_WALL, TERRAIN_MASK_SWAMP).
    #[inline]
    pub fn kind(&self, t: Tile) -> TerrainKind {
        let m = self
            .mask
            .get(t.index())
            .copied()
            .unwrap_or(Terrain::Wall as u8);
        if m & (Terrain::Wall as u8) != 0 {
            TerrainKind::Wall
        } else if m & (Terrain::Swamp as u8) != 0 {
            TerrainKind::Swamp
        } else {
            TerrainKind::Plain
        }
    }
}

/// What an object does to the tile it stands on, for movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Footprint {
    /// A road: the road cost, even on a wall tile (a tunnel; V154).
    Road,
    /// No creep of ours can stand there: an obstacle, a rampart that is not ours and not public, an
    /// own construction site of an obstacle type, or a tile that moves the creep away (a portal).
    Blocked,
}

/// A structure's footprint, from its type and, for a rampart, whether it is ours or public.
///
/// Uses: `StructureType::is_obstacle` (OBSTACLE_OBJECT_TYPES; checked against the engine's list by
/// the constants self-check, V131, V143). Rules: a road is walkable at the road cost (V154); a
/// rampart blocks unless it is ours or public (V49, V52); a portal teleports a creep that ends a
/// tick on it (V153), so in-room paths avoid it. Containers, extractors and keeper lairs are not
/// obstacles (an extractor's mineral is one, read with the room's terrain as a static blocker).
pub fn structure_footprint(kind: StructureType, mine: bool, public: bool) -> Option<Footprint> {
    match kind {
        StructureType::Road => Some(Footprint::Road),
        StructureType::Rampart => (!mine && !public).then_some(Footprint::Blocked),
        StructureType::Portal => Some(Footprint::Blocked),
        k if k.is_obstacle() => Some(Footprint::Blocked),
        _ => None,
    }
}

/// An own construction site's footprint: a site of an obstacle type blocks the owner's own creeps
/// (V52); others' sites are trampled (V54), so the scan reads only own ones.
///
/// Uses: `StructureType::is_obstacle`.
pub fn site_footprint(kind: StructureType) -> Option<Footprint> {
    kind.is_obstacle().then_some(Footprint::Blocked)
}

/// What each terrain costs to step onto. Config gives the values (`MovementConfig`); the defaults
/// are the engine's fatigue rates per tile (`MOVE_COST_*`, V54, V154), so a field minimizes the
/// fatigue a loaded or MOVE-light creep gathers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CostProfile {
    pub road: u8,
    pub plain: u8,
    pub swamp: u8,
}

impl CostProfile {
    /// The profile with every cost raised to at least 1 and lowered below [`BLOCKED`], so a step
    /// always costs something and a cost never reads as blocked.
    pub fn sanitized(self) -> Self {
        let clamp = |c: u8| c.clamp(1, BLOCKED - 1);
        Self {
            road: clamp(self.road),
            plain: clamp(self.plain),
            swamp: clamp(self.swamp),
        }
    }

    /// The cheapest step, which the detour search's heuristic multiplies by the remaining range.
    pub fn cheapest(self) -> u8 {
        self.road.min(self.plain).min(self.swamp)
    }
}

/// Words in the open-exit bitset, one bit per tile.
const EXIT_WORDS: usize = ROOM_AREA.div_ceil(64);

/// The cost of stepping onto each tile of one room, or [`BLOCKED`], and which edge tiles are open
/// exits.
pub struct CostGrid {
    cost: Box<[u8; ROOM_AREA]>,
    /// One bit per tile: an edge tile off the corners whose terrain and objects let a creep stand
    /// on it.
    exits: [u64; EXIT_WORDS],
    /// What stepping onto an open exit costs: the profile's cheapest step, since a move onto an edge
    /// tile from inside the room adds no fatigue (V54).
    exit_cost: u8,
}

impl Default for CostGrid {
    fn default() -> Self {
        Self {
            cost: Box::new([BLOCKED; ROOM_AREA]),
            exits: [0; EXIT_WORDS],
            exit_cost: 1,
        }
    }
}

impl CostGrid {
    /// Rebuilds the grid in place: terrain costs from `profile`, then roads, then blocking
    /// footprints, then the room's edge tiles blocked, since a creep that ends a tick on one changes
    /// rooms (V151, V152). An edge tile that was walkable before that, off the corners, is kept as an
    /// open exit. A road under a blocking object stays blocked. `objects` is walked twice (roads,
    /// then blockers).
    pub fn build<I>(&mut self, terrain: &TerrainGrid, objects: I, profile: CostProfile)
    where
        I: IntoIterator<Item = (Tile, Footprint)>,
        I::IntoIter: Clone,
    {
        let objects = objects.into_iter();
        let p = profile.sanitized();
        for (i, cell) in self.cost.iter_mut().enumerate() {
            let kind = Tile::from_index(i).map_or(TerrainKind::Wall, |t| terrain.kind(t));
            *cell = match kind {
                TerrainKind::Wall => BLOCKED,
                TerrainKind::Swamp => p.swamp,
                TerrainKind::Plain => p.plain,
            };
        }
        for (t, f) in objects.clone() {
            if f == Footprint::Road {
                self.set(t, p.road);
            }
        }
        for (t, f) in objects {
            if f == Footprint::Blocked {
                self.set(t, BLOCKED);
            }
        }
        self.exits = [0; EXIT_WORDS];
        self.exit_cost = p.cheapest();
        for i in 0..ROOM_AREA {
            let Some(t) = Tile::from_index(i).filter(|t| t.is_edge()) else {
                continue;
            };
            let Some(cell) = self.cost.get_mut(i) else {
                continue;
            };
            if *cell != BLOCKED
                && t.side().is_some()
                && let Some(word) = self.exits.get_mut(i / 64)
            {
                *word |= 1 << (i % 64);
            }
            *cell = BLOCKED;
        }
    }

    /// Whether `t` is an open exit: an edge tile off the corners that a creep can step onto, which
    /// takes it to the next room at the end of the tick (V151). Never walkable in the grid itself.
    #[inline]
    pub fn open_exit(&self, t: Tile) -> bool {
        let i = t.index();
        self.exits
            .get(i / 64)
            .is_some_and(|word| word & (1 << (i % 64)) != 0)
    }

    /// The cost of stepping onto an open exit (the profile's cheapest step; V54).
    #[inline]
    pub fn exit_cost(&self) -> u8 {
        self.exit_cost
    }

    /// The cost of stepping onto `t` when open exits may be entered: the grid's cost for a walkable
    /// tile, [`Self::exit_cost`] for an open exit, `None` otherwise.
    #[inline]
    pub fn enter_cost(&self, t: Tile) -> Option<u8> {
        if self.walkable(t) {
            Some(self.cost(t))
        } else if self.open_exit(t) {
            Some(self.exit_cost)
        } else {
            None
        }
    }

    /// The grid as a PathFinder CostMatrix's bytes (`_bits`, index `x * 50 + y`, the tile order;
    /// V164): each walkable tile's cost (so a road, or a tunnel through a wall, is walkable at the
    /// road cost and an obstacle is 255), 0 on open exits (PathFinder charges their own terrain,
    /// plain or swamp, V164: a cheaper byte would draw paths onto the edge and back, where no creep
    /// can walk, V151), and 255 on every other tile. `avoid` tiles are 255 as well (edge tiles a
    /// path stepped onto and back from).
    pub fn write_matrix(&self, out: &mut [u8; ROOM_AREA], avoid: impl IntoIterator<Item = Tile>) {
        for (i, (byte, &cost)) in out.iter_mut().zip(self.cost.iter()).enumerate() {
            let exit = Tile::from_index(i).is_some_and(|t| self.open_exit(t));
            *byte = if exit { 0 } else { cost };
        }
        for t in avoid {
            if let Some(byte) = out.get_mut(t.index()) {
                *byte = BLOCKED;
            }
        }
    }

    /// The cost of stepping onto `t`, or [`BLOCKED`].
    #[inline]
    pub fn cost(&self, t: Tile) -> u8 {
        self.cost.get(t.index()).copied().unwrap_or(BLOCKED)
    }

    #[inline]
    pub fn walkable(&self, t: Tile) -> bool {
        self.cost(t) != BLOCKED
    }

    fn set(&mut self, t: Tile, cost: u8) {
        if let Some(cell) = self.cost.get_mut(t.index()) {
            *cell = cost;
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use rustedscreep_consts::{MOVE_COST_PLAIN, MOVE_COST_ROAD, MOVE_COST_SWAMP};

    /// The default profile: the engine's fatigue rates (V54, V154).
    pub fn rates() -> CostProfile {
        let c = |v: u32| u8::try_from(v).expect("small");
        CostProfile {
            road: c(MOVE_COST_ROAD),
            plain: c(MOVE_COST_PLAIN),
            swamp: c(MOVE_COST_SWAMP),
        }
    }

    pub fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).expect("in room")
    }

    /// Row-major raw terrain from a closure over (x, y), as `getRawBuffer` lays it out (V156).
    pub fn raw(f: impl Fn(u8, u8) -> u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(ROOM_AREA);
        for y in 0..ROOM_SIZE {
            for x in 0..ROOM_SIZE {
                out.push(f(x, y));
            }
        }
        out
    }

    pub fn open_terrain() -> TerrainGrid {
        TerrainGrid::from_row_major(&raw(|_, _| 0)).expect("length")
    }

    /// Uses: ROOM_AREA, the `Terrain` discriminants. The raw buffer is row-major and the grid
    /// column-major: (x, y) reads raw[y * 50 + x]; 3 reads as wall (V86, V156).
    #[test]
    fn terrain_is_transposed_with_wall_precedence() {
        let wall = Terrain::Wall as u8;
        let swamp = Terrain::Swamp as u8;
        let t = TerrainGrid::from_row_major(&raw(|x, y| match (x, y) {
            (3, 1) => wall,
            (1, 3) => swamp,
            (4, 4) => wall | swamp,
            _ => 0,
        }))
        .expect("length");
        assert_eq!(t.kind(tile(3, 1)), TerrainKind::Wall);
        assert_eq!(t.kind(tile(1, 3)), TerrainKind::Swamp);
        assert_eq!(t.kind(tile(4, 4)), TerrainKind::Wall);
        assert_eq!(t.kind(tile(1, 1)), TerrainKind::Plain);
        assert!(TerrainGrid::from_row_major(&[0; ROOM_AREA - 1]).is_none());
        assert!(TerrainGrid::from_row_major(&[0; ROOM_AREA + 1]).is_none());
    }

    /// Uses: MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP, `StructureType::is_obstacle`.
    #[test]
    fn grid_costs_follow_terrain_roads_obstacles_and_edges() {
        let wall = Terrain::Wall as u8;
        let swamp = Terrain::Swamp as u8;
        let terrain = TerrainGrid::from_row_major(&raw(|x, y| match (x, y) {
            (10, 10) => wall,
            (11, 10) => swamp,
            (12, 10) => swamp,
            _ => 0,
        }))
        .expect("length");
        let objects = [
            (tile(10, 10), Footprint::Road), // a tunnel: walkable at the road cost
            (tile(12, 10), Footprint::Road), // a road over swamp: the road cost
            (tile(13, 10), Footprint::Road), // a road under a spawn: blocked
            (tile(13, 10), Footprint::Blocked),
            (tile(14, 10), Footprint::Blocked),
            (tile(0, 20), Footprint::Road), // a road on an edge tile: still blocked
        ];
        let mut grid = CostGrid::default();
        grid.build(&terrain, objects.iter().copied(), rates());
        let p = rates();
        assert_eq!(grid.cost(tile(10, 10)), p.road);
        assert_eq!(grid.cost(tile(11, 10)), p.swamp);
        assert_eq!(grid.cost(tile(12, 10)), p.road);
        assert_eq!(grid.cost(tile(13, 10)), BLOCKED);
        assert_eq!(grid.cost(tile(14, 10)), BLOCKED);
        assert_eq!(grid.cost(tile(15, 10)), p.plain);
        for t in (0..ROOM_AREA).filter_map(Tile::from_index) {
            if t.is_edge() {
                assert!(!grid.walkable(t), "edge {t:?} (V151)");
            }
        }
        // A rebuild replaces everything: without objects the tunnel is a wall again.
        grid.build(&terrain, [], p);
        assert_eq!(grid.cost(tile(10, 10)), BLOCKED);
        assert_eq!(grid.cost(tile(13, 10)), p.plain);
    }

    /// Uses: ROOM_SIZE, the `Terrain` discriminants, MOVE_COST_* (through `rates`). Open exits are
    /// the edge tiles off the corners that are walkable before the edge is blocked (V151, V152): not
    /// a wall, no blocking object. They cost the cheapest step in the grid (V54) and 0 (their
    /// terrain) in the PathFinder bytes, where every other tile keeps its grid cost (V164) and
    /// avoided tiles are 255.
    #[test]
    fn open_exits_and_the_pathfinder_bytes() {
        let wall = Terrain::Wall as u8;
        let last = ROOM_SIZE - 1;
        let terrain = TerrainGrid::from_row_major(&raw(|x, y| match (x, y) {
            (0, 5..=9) => wall, // a closed stretch of the west edge
            (20, 20) => wall,
            _ => 0,
        }))
        .expect("length");
        let objects = [
            (tile(last, 30), Footprint::Blocked),
            (tile(20, 20), Footprint::Road), // a tunnel
            (tile(21, 20), Footprint::Blocked),
        ];
        let mut grid = CostGrid::default();
        let p = rates();
        grid.build(&terrain, objects.iter().copied(), p);
        let mut open = 0usize;
        for t in (0..ROOM_AREA).filter_map(Tile::from_index) {
            let expect = t.side().is_some()
                && !(t.x() == 0 && (5..=9).contains(&t.y()))
                && t != tile(last, 30);
            assert_eq!(grid.open_exit(t), expect, "{t:?}");
            assert!(!grid.walkable(t) || !grid.open_exit(t), "{t:?}");
            open += usize::from(expect);
        }
        // 4 sides of ROOM_SIZE - 2 tiles, less 5 walls and one blocked tile.
        assert_eq!(open, 4 * usize::from(ROOM_SIZE - 2) - 6);
        assert_eq!(grid.exit_cost(), p.cheapest());
        assert_eq!(grid.enter_cost(tile(0, 20)), Some(p.cheapest()));
        assert_eq!(grid.enter_cost(tile(0, 7)), None, "a wall exit");
        assert_eq!(grid.enter_cost(tile(0, 0)), None, "a corner");
        assert_eq!(grid.enter_cost(tile(10, 10)), Some(p.plain));
        assert_eq!(grid.enter_cost(tile(21, 20)), None);
        let mut bytes = [7u8; ROOM_AREA];
        grid.write_matrix(&mut bytes, [tile(0, 30)]);
        for t in (0..ROOM_AREA).filter_map(Tile::from_index) {
            let want = if t == tile(0, 30) {
                BLOCKED
            } else if grid.open_exit(t) {
                0
            } else {
                grid.cost(t)
            };
            assert_eq!(bytes.get(t.index()).copied(), Some(want), "{t:?}");
        }
        assert_eq!(
            bytes.get(tile(20, 20).index()).copied(),
            Some(p.road),
            "tunnel"
        );
        assert_eq!(
            bytes.get(tile(0, 7).index()).copied(),
            Some(BLOCKED),
            "wall exit"
        );
        assert_eq!(bytes.get(tile(last, 30).index()).copied(), Some(BLOCKED));
        // A rebuild without the blocker reopens its exit.
        grid.build(&terrain, [], p);
        assert!(grid.open_exit(tile(last, 30)));
    }

    /// Uses: `StructureType::is_obstacle`. Every known type's footprint follows the rules: roads are
    /// roads; ramparts block unless ours or public; portals block; obstacle types block; the rest
    /// (container, extractor, keeper lair) leave the tile as it is.
    #[test]
    fn footprints_follow_the_obstacle_list() {
        use StructureType::*;
        let all = [
            Spawn,
            Extension,
            Road,
            Wall,
            Rampart,
            KeeperLair,
            Portal,
            Controller,
            Link,
            Storage,
            Tower,
            Observer,
            PowerBank,
            PowerSpawn,
            Extractor,
            Lab,
            Terminal,
            Container,
            Nuker,
            Factory,
            InvaderCore,
        ];
        for kind in all {
            let f = structure_footprint(kind, false, false);
            let want = match kind {
                Road => Some(Footprint::Road),
                Rampart | Portal => Some(Footprint::Blocked),
                k if k.is_obstacle() => Some(Footprint::Blocked),
                _ => None,
            };
            assert_eq!(f, want, "{kind:?}");
            assert_eq!(
                site_footprint(kind),
                kind.is_obstacle().then_some(Footprint::Blocked)
            );
        }
        assert_eq!(structure_footprint(Container, false, false), None);
        assert_eq!(structure_footprint(Extractor, true, false), None);
        assert_eq!(structure_footprint(Rampart, true, false), None);
        assert_eq!(structure_footprint(Rampart, false, true), None);
        assert_eq!(
            structure_footprint(Rampart, false, false),
            Some(Footprint::Blocked)
        );
        assert_eq!(site_footprint(Road), None);
        assert_eq!(site_footprint(Extension), Some(Footprint::Blocked));
    }

    #[test]
    fn profiles_are_sanitized() {
        let p = CostProfile {
            road: 0,
            plain: BLOCKED,
            swamp: 5,
        }
        .sanitized();
        assert_eq!((p.road, p.plain, p.swamp), (1, BLOCKED - 1, 5));
        assert_eq!(p.cheapest(), 1);
        assert_eq!(rates().cheapest(), rates().road);
    }
}
