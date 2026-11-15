//! Room tiles as one index, and the eight directions.
//!
//! A [`Tile`] is `x * ROOM_SIZE + y`, always below `ROOM_AREA`: column-major, the layout of
//! `LocalCostMatrix` and PathFinder's CostMatrix (VERIFICATION_LOG V86), so a grid can later be
//! handed to PathFinder without reordering. Terrain arrives row-major and is transposed once per
//! room ([`super::grid::TerrainGrid::from_row_major`]).

use rustedscreep_consts::{Direction, Position, ROOM_AREA, ROOM_SIZE};

/// The eight directions in the engine's order, 1 (`Top`) to 8 (`TopLeft`) (V155).
pub const DIRECTIONS: [Direction; 8] = [
    Direction::Top,
    Direction::TopRight,
    Direction::Right,
    Direction::BottomRight,
    Direction::Bottom,
    Direction::BottomLeft,
    Direction::Left,
    Direction::TopLeft,
];

/// `(dx, dy)` of each direction in [`DIRECTIONS`] order: the engine's `offsetsByDirection`, the same
/// as screeps-game-api's `From<Direction> for (i32, i32)` (V155; a test compares the two). y grows
/// downward.
const OFFSETS: [(i8, i8); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// `ROOM_SIZE` as the tile index type.
const SIZE: u16 = ROOM_SIZE as u16;

/// One tile of a room. The constructors keep x and y below `ROOM_SIZE`, so [`Tile::index`] is
/// always below `ROOM_AREA`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tile(u16);

impl Tile {
    /// The tile at `(x, y)`, or `None` outside the room.
    ///
    /// Uses: `ROOM_SIZE`.
    pub const fn new(x: u8, y: u8) -> Option<Self> {
        if x < ROOM_SIZE && y < ROOM_SIZE {
            Some(Self(x as u16 * SIZE + y as u16))
        } else {
            None
        }
    }

    /// A position's tile (its room is dropped). `Position` keeps x and y below `ROOM_SIZE`.
    pub fn of(pos: Position) -> Self {
        Self(u16::from(pos.x().u8()) * SIZE + u16::from(pos.y().u8()))
    }

    /// The tile with column-major index `index`, or `None` from `ROOM_AREA` on.
    pub fn from_index(index: usize) -> Option<Self> {
        if index < ROOM_AREA {
            u16::try_from(index).ok().map(Self)
        } else {
            None
        }
    }

    /// `x * ROOM_SIZE + y`, below `ROOM_AREA`.
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub const fn x(self) -> u8 {
        (self.0 / SIZE) as u8
    }

    pub const fn y(self) -> u8 {
        (self.0 % SIZE) as u8
    }

    /// Whether the tile is on the room's edge (x or y is 0 or `ROOM_SIZE - 1`): a creep that ends a
    /// tick there moves to the next room (V151).
    pub const fn is_edge(self) -> bool {
        let (x, y) = (self.x(), self.y());
        x == 0 || y == 0 || x == ROOM_SIZE - 1 || y == ROOM_SIZE - 1
    }

    /// The neighbour in direction `d`, or `None` past the room's edge.
    #[inline]
    pub fn step(self, d: Direction) -> Option<Self> {
        let (dx, dy) = offset(d);
        let x = self.x().checked_add_signed(dx)?;
        let y = self.y().checked_add_signed(dy)?;
        Self::new(x, y)
    }

    /// The neighbours inside the room, in [`DIRECTIONS`] order.
    #[inline]
    pub fn neighbors(self) -> impl Iterator<Item = (Direction, Self)> {
        DIRECTIONS
            .into_iter()
            .filter_map(move |d| self.step(d).map(|t| (d, t)))
    }

    /// Chebyshev distance, the number of moves between two tiles of one open room.
    pub const fn range(self, other: Self) -> u8 {
        let dx = self.x().abs_diff(other.x());
        let dy = self.y().abs_diff(other.y());
        if dx > dy { dx } else { dy }
    }

    /// The direction of an adjacent tile, or `None` when `other` is not adjacent (or is this tile).
    pub fn direction_to(self, other: Self) -> Option<Direction> {
        let dx = i16::from(other.x()) - i16::from(self.x());
        let dy = i16::from(other.y()) - i16::from(self.y());
        DIRECTIONS
            .into_iter()
            .zip(OFFSETS)
            .find(|&(_, (ox, oy))| i16::from(ox) == dx && i16::from(oy) == dy)
            .map(|(d, _)| d)
    }
}

/// `(dx, dy)` of `d` (V155).
#[inline]
fn offset(d: Direction) -> (i8, i8) {
    // Direction is `#[repr(u8)]` from 1 (Top) to 8 (TopLeft) (V155).
    OFFSETS
        .get(usize::from(d as u8).wrapping_sub(1))
        .copied()
        .unwrap_or((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustedscreep_consts::{RoomCoordinate, RoomName, RoomXY, xy_to_linear_index};

    fn every_tile() -> impl Iterator<Item = Tile> {
        (0..ROOM_AREA).map(|i| Tile::from_index(i).expect("in room"))
    }

    /// Uses: ROOM_SIZE, ROOM_AREA. The index is the crate's column-major `xy_to_linear_index` (the
    /// CostMatrix layout, V86), and every coordinate pair round-trips.
    #[test]
    fn tiles_use_the_cost_matrix_layout() {
        for x in 0..ROOM_SIZE {
            for y in 0..ROOM_SIZE {
                let t = Tile::new(x, y).expect("in room");
                let xy = RoomXY::checked_new(x, y).expect("in room");
                assert_eq!(t.index(), xy_to_linear_index(xy));
                assert_eq!((t.x(), t.y()), (x, y));
                assert_eq!(Tile::from_index(t.index()), Some(t));
            }
        }
        assert_eq!(Tile::new(ROOM_SIZE, 0), None);
        assert_eq!(Tile::new(0, ROOM_SIZE), None);
        assert_eq!(Tile::from_index(ROOM_AREA), None);
        let room: RoomName = "W1N1".parse().expect("room");
        let pos = Position::new(
            RoomCoordinate::new(ROOM_SIZE - 1).expect("x"),
            RoomCoordinate::new(3).expect("y"),
            room,
        );
        assert_eq!(Tile::of(pos), Tile::new(ROOM_SIZE - 1, 3).expect("tile"));
    }

    /// The offsets equal screeps-game-api's `From<Direction> for (i32, i32)`, which matches the
    /// engine's `offsetsByDirection` (V155), and `DIRECTIONS` holds 1 to 8 in order.
    #[test]
    fn offsets_match_the_crate_and_engine_order() {
        for (i, d) in DIRECTIONS.into_iter().enumerate() {
            assert_eq!(usize::from(d as u8), i + 1);
            let (dx, dy): (i32, i32) = d.into();
            assert_eq!(offset(d), (dx as i8, dy as i8), "{d:?}");
        }
    }

    /// `step` agrees with the crate's `RoomXY::checked_add_direction` on every tile and direction,
    /// and `direction_to` inverts it.
    #[test]
    fn step_matches_the_crate_and_direction_to_inverts_it() {
        for t in every_tile() {
            let xy = RoomXY::checked_new(t.x(), t.y()).expect("in room");
            for d in DIRECTIONS {
                let crate_step = xy
                    .checked_add_direction(d)
                    .map(|n| Tile::new(n.x.u8(), n.y.u8()));
                assert_eq!(t.step(d), crate_step.flatten(), "{t:?} {d:?}");
                if let Some(n) = t.step(d) {
                    assert_eq!(t.direction_to(n), Some(d));
                    assert_eq!(t.range(n), 1);
                }
            }
            assert_eq!(t.direction_to(t), None);
        }
    }

    /// Uses: ROOM_SIZE. Edge tiles are those with x or y at 0 or ROOM_SIZE - 1 (V151): 4 × 49.
    #[test]
    fn edges_are_the_border_tiles() {
        let n = usize::from(ROOM_SIZE);
        assert_eq!(every_tile().filter(|t| t.is_edge()).count(), 4 * (n - 1));
        for t in every_tile() {
            // An edge tile has fewer than 8 neighbours inside the room, an inner tile has 8.
            assert_eq!(t.is_edge(), t.neighbors().count() < 8, "{t:?}");
        }
    }

    #[test]
    fn range_is_chebyshev() {
        let a = Tile::new(10, 10).expect("a");
        let b = Tile::new(13, 8).expect("b");
        assert_eq!(a.range(b), 3);
        assert_eq!(b.range(a), 3);
        assert_eq!(a.range(a), 0);
        let far = Tile::new(ROOM_SIZE - 1, 0).expect("far");
        assert_eq!(Tile::new(0, 0).expect("origin").range(far), ROOM_SIZE - 1);
        assert_eq!(a.direction_to(b), None, "not adjacent");
    }
}
