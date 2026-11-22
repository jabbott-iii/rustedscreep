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

    /// The side of the room an edge tile is on, or `None` for an inner tile or a corner (a corner
    /// is on two sides, so which room it leads to is ambiguous; inter-room routes never use one).
    ///
    /// Uses: `ROOM_SIZE`.
    pub const fn side(self) -> Option<Side> {
        let (x, y, last) = (self.x(), self.y(), ROOM_SIZE - 1);
        let corner = (x == 0 || x == last) && (y == 0 || y == last);
        if corner {
            None
        } else if y == 0 {
            Some(Side::Top)
        } else if x == last {
            Some(Side::Right)
        } else if y == last {
            Some(Side::Bottom)
        } else if x == 0 {
            Some(Side::Left)
        } else {
            None
        }
    }

    /// The tile a creep that ends a tick on this edge tile lands on in the next room: the mirrored
    /// tile, the other coordinate kept (x 0 to 49, x 49 to 0, y 0 to 49, y 49 to 0; V151). `None`
    /// off the edge and on a corner.
    ///
    /// Uses: `ROOM_SIZE`.
    pub const fn across(self) -> Option<Self> {
        let (x, y, last) = (self.x(), self.y(), ROOM_SIZE - 1);
        match self.side() {
            Some(Side::Top) => Self::new(x, last),
            Some(Side::Bottom) => Self::new(x, 0),
            Some(Side::Left) => Self::new(last, y),
            Some(Side::Right) => Self::new(0, y),
            None => None,
        }
    }

    /// The neighbor in direction `d`, or `None` past the room's edge.
    #[inline]
    pub fn step(self, d: Direction) -> Option<Self> {
        let (dx, dy) = offset(d);
        let x = self.x().checked_add_signed(dx)?;
        let y = self.y().checked_add_signed(dy)?;
        Self::new(x, y)
    }

    /// The neighbors inside the room, in [`DIRECTIONS`] order.
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

/// A side of a room, where its exits are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    pub const ALL: [Self; 4] = [Self::Top, Self::Right, Self::Bottom, Self::Left];

    /// The offset of the room across this side, for `RoomName::checked_add`: x grows to the east and
    /// y to the south, as in tile coordinates.
    pub const fn room_offset(self) -> (i32, i32) {
        match self {
            Self::Top => (0, -1),
            Self::Right => (1, 0),
            Self::Bottom => (0, 1),
            Self::Left => (-1, 0),
        }
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

    /// Uses: ROOM_SIZE. Every edge tile but the 4 corners is on one side, ROOM_SIZE - 2 per side;
    /// `across` is the mirrored tile on the opposite side with the other coordinate kept (V151), and
    /// crossing back returns the same tile.
    #[test]
    fn edge_tiles_have_a_side_and_a_tile_across() {
        let last = ROOM_SIZE - 1;
        for side in Side::ALL {
            let on_side = every_tile().filter(|t| t.side() == Some(side)).count();
            assert_eq!(on_side, usize::from(ROOM_SIZE - 2), "{side:?}");
        }
        for t in every_tile() {
            let corner = (t.x() == 0 || t.x() == last) && (t.y() == 0 || t.y() == last);
            assert_eq!(t.side().is_some(), t.is_edge() && !corner, "{t:?}");
            let Some(side) = t.side() else {
                assert_eq!(t.across(), None, "{t:?}");
                continue;
            };
            let a = t.across().expect("across");
            assert_eq!(a.across(), Some(t), "{t:?}");
            let (dx, dy) = side.room_offset();
            // The landing tile is on the opposite side, so the offset back is the negation.
            assert_eq!(a.side().map(Side::room_offset), Some((-dx, -dy)), "{t:?}");
            match side {
                Side::Top | Side::Bottom => assert_eq!(a.x(), t.x()),
                Side::Left | Side::Right => assert_eq!(a.y(), t.y()),
            }
        }
        assert_eq!(Tile::new(last, 7).and_then(Tile::across), Tile::new(0, 7));
        assert_eq!(Tile::new(7, 0).and_then(Tile::across), Tile::new(7, last));
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
