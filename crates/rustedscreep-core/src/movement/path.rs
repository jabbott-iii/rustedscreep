//! A* detours: one creep's path around what the flow field does not know about (creeps standing in
//! the way), searched only after its moves failed `stuck_ticks` times, then followed tile by tile.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rustedscreep_consts::ROOM_AREA;

use super::field::{Aim, FlowField, Goal, UNREACHED};
use super::grid::CostGrid;
use super::tile::Tile;

/// One search: from `start` to one of the aim's goal tiles ([`Aim::reaches`]: a walkable tile within
/// range, or for an `Exit` aim one of its open exits) that `blocked` does not reject, on `grid`,
/// popping at most `max_pops` entries. `estimate` gives a lower bound on the cost
/// from a tile to the goal, or `None` when the goal cannot be reached from it at all (such tiles are
/// never entered). Pass the goal's flow-field distance ([`field_estimate`]): blocked tiles only
/// raise costs, so the field never overestimates, and a detour around a few creeps expands little
/// more than its own path. [`range_estimate`] needs no field.
pub struct Query<'a, B, E> {
    pub grid: &'a CostGrid,
    pub blocked: B,
    pub estimate: E,
    pub start: Tile,
    pub goal: Aim,
    pub max_pops: u32,
}

/// The remaining range times `cheapest`, the lowest step cost: each move costs at least that and
/// shortens the range by at most 1, so it never overestimates.
pub fn range_estimate(goal: Goal, cheapest: u8) -> impl Fn(Tile) -> Option<u32> {
    move |t: Tile| {
        Some(u32::from(t.range(goal.tile).saturating_sub(goal.range)) * u32::from(cheapest.max(1)))
    }
}

/// The flow field's distance as the estimate: exact without the blocked tiles, a lower bound with
/// them, and `None` where the field cannot reach the goal.
pub fn field_estimate(field: &FlowField) -> impl Fn(Tile) -> Option<u32> + '_ {
    move |t: Tile| {
        let d = field.distance(t);
        (d != UNREACHED).then_some(u32::from(d))
    }
}

/// Scratch for [`PathSearch::search`], kept between searches so a search allocates nothing once
/// warm. A stamp per tile marks which costs belong to the current search, so nothing is cleared.
pub struct PathSearch {
    cost: Box<[u32; ROOM_AREA]>,
    parent: Box<[u16; ROOM_AREA]>,
    seen: Box<[u32; ROOM_AREA]>,
    stamp: u32,
    /// (estimate, heuristic, tile): the lowest estimate first, then the one nearer the goal.
    open: BinaryHeap<Reverse<(u32, u32, u16)>>,
}

impl Default for PathSearch {
    fn default() -> Self {
        Self {
            cost: Box::new([0; ROOM_AREA]),
            parent: Box::new([0; ROOM_AREA]),
            seen: Box::new([0; ROOM_AREA]),
            stamp: 0,
            open: BinaryHeap::new(),
        }
    }
}

impl PathSearch {
    /// The cheapest path for `q` into `out` as the tiles to enter, the LAST entry first to enter
    /// (so a follower pops from the end). Returns false, with `out` empty, when no such path exists
    /// or the search popped more than `q.max_pops` entries. A start already in the goal gives an
    /// empty path. With an estimate that never overestimates, the path is the cheapest one.
    pub fn search<B, E>(&mut self, q: Query<'_, B, E>, out: &mut Vec<Tile>) -> bool
    where
        B: Fn(Tile) -> bool,
        E: Fn(Tile) -> Option<u32>,
    {
        let Query {
            grid,
            blocked,
            estimate,
            start,
            goal,
            max_pops,
        } = q;
        out.clear();
        let is_goal = |t: Tile| goal.reaches(grid, t) && !blocked(t);
        if is_goal(start) {
            return true;
        }
        let Some(h0) = estimate(start) else {
            return false;
        };
        self.next_stamp();
        self.open.clear();
        self.visit(start, 0, start);
        self.open.push(Reverse((h0, h0, start.index() as u16)));
        let mut pops = 0u32;
        while let Some(Reverse((f, h, raw))) = self.open.pop() {
            pops += 1;
            if pops > max_pops {
                break;
            }
            let Some(u) = Tile::from_index(usize::from(raw)) else {
                continue;
            };
            let g = self.cost_of(u);
            if f.saturating_sub(h) > g {
                continue; // a stale entry: u was reached more cheaply since it was pushed
            }
            if is_goal(u) {
                self.trace(start, u, out);
                return true;
            }
            for (_, v) in u.neighbors() {
                let Some(enter) = goal.enter_cost(grid, v) else {
                    continue;
                };
                if blocked(v) {
                    continue;
                }
                let Some(hv) = estimate(v) else { continue };
                let next = g.saturating_add(u32::from(enter));
                if !self.was_seen(v) || next < self.cost_of(v) {
                    self.visit(v, next, u);
                    self.open
                        .push(Reverse((next.saturating_add(hv), hv, v.index() as u16)));
                }
            }
        }
        out.clear();
        false
    }

    fn next_stamp(&mut self) {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.seen.fill(0);
            self.stamp = 1;
        }
    }

    fn was_seen(&self, t: Tile) -> bool {
        self.seen.get(t.index()) == Some(&self.stamp)
    }

    fn cost_of(&self, t: Tile) -> u32 {
        if self.was_seen(t) {
            self.cost.get(t.index()).copied().unwrap_or(u32::MAX)
        } else {
            u32::MAX
        }
    }

    fn visit(&mut self, t: Tile, cost: u32, parent: Tile) {
        let i = t.index();
        if let (Some(c), Some(p), Some(s)) = (
            self.cost.get_mut(i),
            self.parent.get_mut(i),
            self.seen.get_mut(i),
        ) {
            *c = cost;
            *p = parent.index() as u16;
            *s = self.stamp;
        }
    }

    /// Writes the path from `start` (excluded) to `end` into `out`, end first.
    fn trace(&self, start: Tile, end: Tile, out: &mut Vec<Tile>) {
        let mut at = end;
        // A path in one room has fewer than ROOM_AREA tiles; the bound guards a corrupt chain.
        for _ in 0..ROOM_AREA {
            if at == start {
                return;
            }
            out.push(at);
            let p = self.parent.get(at.index()).copied().unwrap_or(0);
            match Tile::from_index(usize::from(p)) {
                Some(next) if next != at => at = next,
                _ => break,
            }
        }
        out.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::super::field::{Dial, FlowField};
    use super::super::grid::tests::{open_terrain, rates, raw, tile};
    use super::super::grid::{CostGrid, Footprint, TerrainGrid};
    use super::*;
    use rustedscreep_consts::Terrain;

    /// A query with the range estimate.
    fn q<'a, B: Fn(Tile) -> bool>(
        grid: &'a CostGrid,
        blocked: B,
        start: Tile,
        goal: Goal,
        cheapest: u8,
        max_pops: u32,
    ) -> Query<'a, B, impl Fn(Tile) -> Option<u32>> {
        Query {
            grid,
            blocked,
            estimate: range_estimate(goal, cheapest),
            start,
            goal: goal.into(),
            max_pops,
        }
    }

    fn grid_of(terrain: &TerrainGrid, objects: &[(Tile, Footprint)]) -> CostGrid {
        let mut g = CostGrid::default();
        g.build(terrain, objects.iter().copied(), rates());
        g
    }

    /// Follows `path` from `start`: each step is adjacent, walkable and not blocked; returns the
    /// sum of the step costs and the last tile.
    fn walk(
        grid: &CostGrid,
        start: Tile,
        path: &[Tile],
        blocked: impl Fn(Tile) -> bool,
    ) -> (u32, Tile) {
        let mut at = start;
        let mut paid = 0u32;
        for &t in path.iter().rev() {
            assert_eq!(at.range(t), 1, "{at:?} -> {t:?}");
            assert!(grid.walkable(t) && !blocked(t), "{t:?}");
            paid += u32::from(grid.cost(t));
            at = t;
        }
        (paid, at)
    }

    /// With nothing extra blocked, A* finds a path as cheap as the flow field's distance, on a maze
    /// of walls, swamps and roads, for several starts and goals.
    #[test]
    fn a_star_matches_the_field_cost() {
        let wall = Terrain::Wall as u8;
        let swamp = Terrain::Swamp as u8;
        let terrain = TerrainGrid::from_row_major(&raw(|x, y| {
            match (u32::from(x) * 5 + u32::from(y) * 3 + u32::from(x) * u32::from(y)) % 9 {
                0 => wall,
                1 | 2 => swamp,
                _ => 0,
            }
        }))
        .expect("length");
        let roads: Vec<(Tile, Footprint)> =
            (3..47).map(|y| (tile(24, y), Footprint::Road)).collect();
        let grid = grid_of(&terrain, &roads);
        let cheapest = rates().cheapest();
        let mut search = PathSearch::default();
        let mut field = FlowField::default();
        let mut dial = Dial::default();
        let mut path = Vec::new();
        let mut field_path = Vec::new();
        for (sx, sy, gx, gy, range) in [(3, 3, 45, 45, 1), (40, 5, 6, 44, 3), (24, 3, 24, 46, 0)] {
            let start = tile(sx, sy);
            let goal = Goal {
                tile: tile(gx, gy),
                range,
            };
            field.compute(&grid, goal, &mut dial);
            let expect = field.steps(&grid, start).total;
            let found = search.search(
                q(&grid, |_| false, start, goal, cheapest, u32::MAX),
                &mut path,
            );
            // The field's own distance as the estimate: the same cost, and no tile it cannot reach.
            let by_field = Query {
                grid: &grid,
                blocked: |_| false,
                estimate: field_estimate(&field),
                start,
                goal: goal.into(),
                max_pops: u32::MAX,
            };
            let found_by_field = search.search(by_field, &mut field_path);
            if field.steps(&grid, start).is_empty() {
                assert!(!found && path.is_empty());
                assert!(!found_by_field && field_path.is_empty());
                continue;
            }
            assert!(found && found_by_field, "{start:?} -> {goal:?}");
            let (paid, end) = walk(&grid, start, &path, |_| false);
            assert!(goal.contains(end));
            assert_eq!(paid, expect, "{start:?} -> {goal:?}");
            let (paid, end) = walk(&grid, start, &field_path, |_| false);
            assert!(goal.contains(end));
            assert_eq!(paid, expect, "by field: {start:?} -> {goal:?}");
        }
    }

    /// A wall of creeps across a corridor sends the detour through the one gap; with the gap closed
    /// there is no path; a pop budget that is too small fails without a partial path.
    #[test]
    fn detours_avoid_blocked_tiles() {
        let grid = grid_of(&open_terrain(), &[]);
        let start = tile(10, 25);
        let goal = Goal {
            tile: tile(30, 25),
            range: 0,
        };
        let gap = tile(20, 40);
        let creeps = |t: Tile| t.x() == 20 && t != gap;
        let mut search = PathSearch::default();
        let mut path = Vec::new();
        assert!(search.search(
            q(&grid, creeps, start, goal, rates().cheapest(), u32::MAX),
            &mut path
        ));
        let (_, end) = walk(&grid, start, &path, creeps);
        assert_eq!(end, goal.tile);
        assert!(path.contains(&gap));
        let closed = |t: Tile| t.x() == 20;
        assert!(!search.search(
            q(&grid, closed, start, goal, rates().cheapest(), u32::MAX),
            &mut path
        ));
        assert!(path.is_empty());
        assert!(!search.search(
            q(&grid, creeps, start, goal, rates().cheapest(), 5),
            &mut path
        ));
        assert!(path.is_empty());
        // Already there: an empty path that counts as found.
        assert!(search.search(q(&grid, |_| false, goal.tile, goal, 1, u32::MAX), &mut path));
        assert!(path.is_empty());
        // A goal tile that is blocked does not count: range 1 around (30, 25) with (30, 25) blocked
        // ends next to it.
        let near = Goal {
            tile: tile(30, 25),
            range: 1,
        };
        let center = |t: Tile| t == tile(30, 25);
        assert!(search.search(q(&grid, center, start, near, 1, u32::MAX), &mut path));
        let (_, end) = walk(&grid, start, &path, center);
        assert_eq!(end.range(near.tile), 1);
    }

    /// The stamp wraps without reading stale costs.
    #[test]
    fn the_stamp_wraps() {
        let grid = grid_of(&open_terrain(), &[]);
        let mut search = PathSearch {
            stamp: u32::MAX - 1,
            ..PathSearch::default()
        };
        let mut path = Vec::new();
        let goal = Goal {
            tile: tile(12, 10),
            range: 0,
        };
        for _ in 0..3 {
            assert!(search.search(
                q(&grid, |_| false, tile(10, 10), goal, 1, u32::MAX),
                &mut path
            ));
            assert_eq!(path.len(), 2);
        }
        assert_eq!(search.stamp, 2);
    }

    /// Around a wall of creeps, the field's estimate finds the same cheapest detour as the range
    /// estimate with fewer pops: a budget the field-guided search fits in is too small for the
    /// other.
    #[test]
    fn the_field_estimate_expands_less() {
        let grid = grid_of(&open_terrain(), &[]);
        let start = tile(10, 25);
        let goal = Goal {
            tile: tile(40, 25),
            range: 0,
        };
        let creeps = |t: Tile| t.x() == 20 && (15..=35).contains(&t.y());
        let mut field = FlowField::default();
        field.compute(&grid, goal, &mut Dial::default());
        let mut search = PathSearch::default();
        let mut path = Vec::new();
        let mut pops = 1u32;
        // The smallest budget the field-guided search succeeds in.
        loop {
            let by_field = Query {
                grid: &grid,
                blocked: creeps,
                estimate: field_estimate(&field),
                start,
                goal: goal.into(),
                max_pops: pops,
            };
            if search.search(by_field, &mut path) {
                break;
            }
            pops += 1;
            assert!(pops < 5000, "no detour found");
        }
        let (field_cost, _) = walk(&grid, start, &path, creeps);
        assert!(!search.search(
            q(&grid, creeps, start, goal, rates().cheapest(), pops),
            &mut path
        ));
        assert!(search.search(
            q(&grid, creeps, start, goal, rates().cheapest(), u32::MAX),
            &mut path
        ));
        let (range_cost, _) = walk(&grid, start, &path, creeps);
        assert_eq!(field_cost, range_cost, "both are the cheapest detour");
    }

    /// Uses: MOVE_COST_* (through `rates`). An `Exit` detour ends on an open exit of its side, around
    /// a creep on the field's own route, and enters the exit at the exit cost; a `Near` search for
    /// the same tile never steps onto the edge.
    #[test]
    fn an_exit_detour_ends_on_an_open_exit() {
        let last = rustedscreep_consts::ROOM_SIZE - 1;
        let grid = {
            let mut g = CostGrid::default();
            g.build(&open_terrain(), [], rates());
            g
        };
        let goal = Goal {
            tile: tile(last, 25),
            range: 0,
        };
        let aim = Aim::Exit(goal);
        let mut field = FlowField::default();
        field.compute(&grid, aim, &mut Dial::default());
        let start = tile(last - 3, 25);
        let creep = tile(last - 2, 25);
        let mut search = PathSearch::default();
        let mut path = Vec::new();
        let query = Query {
            grid: &grid,
            blocked: |t: Tile| t == creep,
            estimate: field_estimate(&field),
            start,
            goal: aim,
            max_pops: u32::MAX,
        };
        assert!(search.search(query, &mut path));
        assert_eq!(path.first(), Some(&goal.tile), "ends on the exit");
        assert!(!path.contains(&creep));
        assert_eq!(path.len(), 3, "around the creep in three steps");
        assert!(path.iter().skip(1).all(|&t| grid.walkable(t)));
        let near = Query {
            grid: &grid,
            blocked: |_: Tile| false,
            estimate: range_estimate(goal, rates().cheapest()),
            start,
            goal: goal.into(),
            max_pops: u32::MAX,
        };
        assert!(
            !search.search(near, &mut path),
            "an edge tile is no Near goal"
        );
    }
}
