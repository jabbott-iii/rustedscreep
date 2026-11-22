//! The Chebyshev distance transform: for each tile, how far it is to the nearest closed tile or past
//! the room's border, so a planner finds where a square of open ground fits.
//!
//! `out[t] = 0` on a closed tile; on an open tile it is one more than the smallest value among its 8
//! neighbors, with every tile past the border counted as closed. So `out[t] >= k` exactly when the
//! square of side `2k - 1` centered on `t` is open and inside the room: 1 for the tile alone, 2 for a
//! 3 × 3, 7 for a 13 × 13. Two raster passes (forward over the four neighbors already visited, then
//! backward over the other four) give the exact chessboard distance in `2 * ROOM_AREA` steps with no
//! allocation.

use rustedscreep_consts::{ROOM_AREA, ROOM_SIZE};

use crate::movement::Tile;

/// The value of `t` in `grid`, or 0 (closed) past the border.
#[inline]
fn at(grid: &[u8; ROOM_AREA], x: i16, y: i16) -> u8 {
    let size = i16::from(ROOM_SIZE);
    if x < 0 || y < 0 || x >= size || y >= size {
        return 0;
    }
    // In range: x and y are below ROOM_SIZE, so they fit in u8.
    Tile::new(x as u8, y as u8)
        .and_then(|t| grid.get(t.index()))
        .copied()
        .unwrap_or(0)
}

/// Writes the distance transform of the tiles `open` accepts into `out` (indexed by
/// [`Tile::index`]); see the module docs for the values.
///
/// Uses: `ROOM_SIZE`, `ROOM_AREA`.
pub fn distance_transform(open: impl Fn(Tile) -> bool, out: &mut [u8; ROOM_AREA]) {
    let size = i16::from(ROOM_SIZE);
    // Forward: west, north-west, north, north-east are done.
    for y in 0..size {
        for x in 0..size {
            let Some(t) = Tile::new(x as u8, y as u8) else {
                continue;
            };
            let v = if open(t) {
                let m = at(out, x - 1, y)
                    .min(at(out, x - 1, y - 1))
                    .min(at(out, x, y - 1))
                    .min(at(out, x + 1, y - 1));
                m.saturating_add(1)
            } else {
                0
            };
            if let Some(cell) = out.get_mut(t.index()) {
                *cell = v;
            }
        }
    }
    // Backward: east, south-east, south, south-west.
    for y in (0..size).rev() {
        for x in (0..size).rev() {
            let Some(t) = Tile::new(x as u8, y as u8) else {
                continue;
            };
            let here = at(out, x, y);
            if here == 0 {
                continue;
            }
            let m = at(out, x + 1, y)
                .min(at(out, x + 1, y + 1))
                .min(at(out, x, y + 1))
                .min(at(out, x - 1, y + 1));
            if let Some(cell) = out.get_mut(t.index()) {
                *cell = here.min(m.saturating_add(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator (no ambient randomness in core, CLAUDE.md).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    /// The definition: the largest k such that the square of side 2k - 1 around t is open and
    /// inside the room. Uses: ROOM_SIZE.
    fn brute(open: &[bool; ROOM_AREA], t: Tile) -> u8 {
        let size = i16::from(ROOM_SIZE);
        let mut k = 0u8;
        loop {
            let r = i16::from(k);
            let (x, y) = (i16::from(t.x()), i16::from(t.y()));
            let fits = (x - r..=x + r).all(|xx| {
                (y - r..=y + r).all(|yy| {
                    xx >= 0
                        && yy >= 0
                        && xx < size
                        && yy < size
                        && open[Tile::new(xx as u8, yy as u8).expect("in room").index()]
                })
            });
            if !fits {
                return k;
            }
            k += 1;
        }
    }

    fn check(open: &[bool; ROOM_AREA]) {
        let mut out = [0u8; ROOM_AREA];
        distance_transform(|t| open[t.index()], &mut out);
        for (i, &v) in out.iter().enumerate() {
            let t = Tile::from_index(i).expect("index");
            assert_eq!(v, brute(open, t), "{:?}", (t.x(), t.y()));
        }
    }

    #[test]
    fn an_open_room_peaks_in_the_middle() {
        let open = [true; ROOM_AREA];
        let mut out = [0u8; ROOM_AREA];
        distance_transform(|_| true, &mut out);
        // The border tiles see past the edge: 1. The centre tile fits a square of side ROOM_SIZE - 1.
        let last = ROOM_SIZE - 1;
        assert_eq!(out[Tile::new(0, 0).expect("tile").index()], 1);
        assert_eq!(out[Tile::new(last, 7).expect("tile").index()], 1);
        let mid = ROOM_SIZE / 2;
        assert_eq!(
            out[Tile::new(mid, mid).expect("tile").index()],
            ROOM_SIZE - mid
        );
        check(&open);
    }

    #[test]
    fn closed_tiles_are_zero_and_neighbours_one() {
        let mut open = [true; ROOM_AREA];
        open[Tile::new(20, 20).expect("tile").index()] = false;
        let mut out = [0u8; ROOM_AREA];
        distance_transform(|t| open[t.index()], &mut out);
        assert_eq!(out[Tile::new(20, 20).expect("tile").index()], 0);
        for (_, n) in Tile::new(20, 20).expect("tile").neighbors() {
            assert_eq!(out[n.index()], 1);
        }
        check(&open);
    }

    /// Random rooms of several densities match the definition on every tile.
    #[test]
    fn random_rooms_match_the_definition() {
        let mut rng = Lcg(7);
        for density in [2u32, 10, 30, 60, 90] {
            for _ in 0..4 {
                let mut open = [true; ROOM_AREA];
                for cell in open.iter_mut() {
                    *cell = rng.next() % 100 >= density;
                }
                check(&open);
            }
        }
    }

    #[test]
    fn nothing_open_is_all_zero() {
        let mut out = [9u8; ROOM_AREA];
        distance_transform(|_| false, &mut out);
        assert!(out.iter().all(|&v| v == 0));
    }
}
