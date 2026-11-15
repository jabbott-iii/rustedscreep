//! In-room cost grids: terrain plus what stands on it.
//!
//! A [`CostGrid`] holds one byte per tile: the cost of stepping onto it, or [`BLOCKED`]. It is built
//! from the room's [`TerrainGrid`] and its sources and minerals (read once per room), the footprints
//! of its structures, own sites and deposits (rescanned on a cadence), and a [`CostProfile`] from
//! config. Creeps are not in it: they move, so the
//! traffic manager and the detour search handle them per tick.

use rustedscreep_consts::{ROOM_AREA, ROOM_SIZE, StructureType, Terrain};

use super::tile::Tile;

/// The grid value of a tile no creep can stand on or should end a tick on. The same value means
/// "unwalkable" in a PathFinder CostMatrix.
pub const BLOCKED: u8 = u8::MAX;

/// A room's terrain in tile order (column-major), as the raw masks (`Terrain::Wall as u8`,
/// `Terrain::Swamp as u8`; V86, V156).
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

/// The cost of stepping onto each tile of one room, or [`BLOCKED`].
pub struct CostGrid {
    cost: Box<[u8; ROOM_AREA]>,
}

impl Default for CostGrid {
    fn default() -> Self {
        Self {
            cost: Box::new([BLOCKED; ROOM_AREA]),
        }
    }
}

impl CostGrid {
    /// Rebuilds the grid in place: terrain costs from `profile`, then roads, then blocking
    /// footprints, then the room's edge tiles blocked, since a creep that ends a tick on one changes
    /// rooms (V151, V152; inter-room movement is a separate P2 item). A road under a blocking object
    /// stays blocked. `objects` is walked twice (roads, then blockers).
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
        for i in 0..ROOM_AREA {
            if Tile::from_index(i).is_some_and(Tile::is_edge)
                && let Some(cell) = self.cost.get_mut(i)
            {
                *cell = BLOCKED;
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
