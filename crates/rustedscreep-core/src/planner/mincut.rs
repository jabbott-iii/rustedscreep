//! The minimum set of tiles that separates a room's exits from the ground a base protects: where its
//! ramparts go.
//!
//! Each tile the caller marks is a [`CutRole`]: a wall (no node), a source (where attackers enter,
//! such as an exit tile), a fixed tile (walkable, but no rampart can stand there), a cuttable tile
//! with a cost, a sink (ground to keep inside) or a guarded tile (a structure, safe under a rampart
//! of its own or when no attacker can stand within the solve's `reach` of it). Creeps move to any of
//! the 8 neighbors, with no rule against passing between two diagonal walls (V244), so the graph
//! is 8-connected. A vertex cut becomes an edge cut by splitting each tile into an entry node and an
//! exit node joined by an edge of the tile's cost (unbounded for sources, fixed tiles and sinks);
//! every move between neighbors, and the edges from the super source and to the super sink, are
//! unbounded. A guarded tile's exit node leads to the sink, and the exit node of every tile within
//! `reach` of it leads into its entry node: an attacker that stands within reach gets to the sink
//! unless the guarded tile itself is cut (a rampart over the structure), so the cut weighs
//! ramparting the structure against keeping attackers out of reach (a ranged attack needs no line of
//! sight, V243). Dinic's max-flow finds the cut's
//! cost; the cut is read from the residual graph: the cuttable tiles whose entry the source still
//! reaches and whose exit it does not. That is the minimum cut nearest the sources, so the ground
//! left inside is as large as any minimum cut allows.
//!
//! The buffers are kept between solves (about 50 000 edges for an open room); a solve allocates
//! only to grow them and for the returned tiles.

use rustedscreep_consts::ROOM_AREA;

use crate::movement::Tile;

/// What a tile is to the cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutRole {
    /// Not in the graph: nothing walks here (a natural wall, or a wall the plan keeps).
    Wall,
    /// Attackers start here (an exit tile). It cannot hold a rampart.
    Source,
    /// Walkable, but it cannot hold a rampart (for example next to an exit).
    Fixed,
    /// It can hold a rampart at this cost (a cost of 0 counts as 1).
    Cuttable(u8),
    /// Ground to protect. It is never cut.
    Sink,
    /// A structure: safe when cut itself (a rampart over it, at this cost) or when no attacker
    /// stands within the solve's `reach` of it.
    Guarded(u8),
}

/// The tiles of a minimum cut, in [`Tile::index`] order, and their total cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    pub tiles: Vec<Tile>,
    pub cost: u32,
}

/// Why no cut exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutError {
    /// A source reaches a sink through tiles none of which can be cut.
    Unbounded,
}

const NONE: u32 = u32::MAX;
/// Far above the sum of every finite capacity (at most `ROOM_AREA * u8::MAX`). Leftover capacity
/// on such an edge can fall below it once finite flow has crossed it, so unboundedness is judged by
/// the total flow, never by one path's bottleneck.
const INF: u32 = u32::MAX / 4;
const SOURCE: usize = 2 * ROOM_AREA;
const SINK: usize = 2 * ROOM_AREA + 1;
const NODES: usize = 2 * ROOM_AREA + 2;

#[inline]
fn entry(t: Tile) -> usize {
    2 * t.index()
}

#[inline]
fn exit(t: Tile) -> usize {
    2 * t.index() + 1
}

/// Reusable max-flow buffers; see the module docs.
pub struct MinCut {
    roles: Vec<CutRole>,
    head: Vec<u32>,
    next: Vec<u32>,
    to: Vec<u32>,
    cap: Vec<u32>,
    level: Vec<u32>,
    arc: Vec<u32>,
    queue: Vec<u32>,
    path: Vec<u32>,
    /// The sum of the cuttable tiles' costs: the most a bounded cut can cost.
    finite: u64,
}

impl Default for MinCut {
    fn default() -> Self {
        Self::new()
    }
}

impl MinCut {
    pub fn new() -> Self {
        Self {
            roles: Vec::new(),
            head: Vec::new(),
            next: Vec::new(),
            to: Vec::new(),
            cap: Vec::new(),
            level: Vec::new(),
            arc: Vec::new(),
            queue: Vec::new(),
            path: Vec::new(),
            finite: 0,
        }
    }

    /// The minimum cut for the roles `role` gives each tile; a guarded tile is in reach of every
    /// tile within Chebyshev `reach` of it.
    ///
    /// Uses: `ROOM_AREA`.
    pub fn solve(&mut self, role: impl Fn(Tile) -> CutRole, reach: u8) -> Result<Cut, CutError> {
        self.build(role, reach);
        let mut flow: u64 = 0;
        while self.levels() {
            self.arc.clear();
            self.arc.extend_from_slice(&self.head);
            loop {
                let pushed = self.augment();
                if pushed == 0 {
                    break;
                }
                // Any cut with a cuttable tile on every path costs at most `finite`; more flow than
                // that crossed a path of unbounded edges only.
                flow = flow.saturating_add(u64::from(pushed));
                if flow > self.finite {
                    return Err(CutError::Unbounded);
                }
            }
        }
        // The source side of the residual graph.
        self.levels();
        let mut tiles = Vec::new();
        let mut cost = 0u32;
        for i in 0..ROOM_AREA {
            let Some(t) = Tile::from_index(i) else {
                continue;
            };
            let c = match self.roles.get(i).copied() {
                Some(CutRole::Cuttable(c) | CutRole::Guarded(c)) => c,
                _ => continue,
            };
            let reached = |n: usize| self.level.get(n).is_some_and(|&l| l != NONE);
            if reached(entry(t)) && !reached(exit(t)) {
                tiles.push(t);
                cost = cost.saturating_add(u32::from(c.max(1)));
            }
        }
        Ok(Cut { tiles, cost })
    }

    fn build(&mut self, role: impl Fn(Tile) -> CutRole, reach: u8) {
        self.roles.clear();
        self.roles
            .extend((0..ROOM_AREA).map(|i| Tile::from_index(i).map_or(CutRole::Wall, &role)));
        self.head.clear();
        self.head.resize(NODES, NONE);
        self.next.clear();
        self.to.clear();
        self.cap.clear();
        self.finite = 0;
        for i in 0..ROOM_AREA {
            let Some(t) = Tile::from_index(i) else {
                continue;
            };
            let r = self.role(i);
            let inner = match r {
                CutRole::Wall => continue,
                CutRole::Cuttable(c) | CutRole::Guarded(c) => u32::from(c.max(1)),
                CutRole::Source | CutRole::Fixed | CutRole::Sink => INF,
            };
            if inner < INF {
                self.finite = self.finite.saturating_add(u64::from(inner));
            }
            self.edge(entry(t), exit(t), inner);
            match r {
                CutRole::Source => self.edge(SOURCE, entry(t), INF),
                CutRole::Sink | CutRole::Guarded(_) => self.edge(exit(t), SINK, INF),
                _ => {}
            }
            if matches!(r, CutRole::Guarded(_)) {
                let span = i16::from(reach);
                for dx in -span..=span {
                    for dy in -span..=span {
                        let (x, y) = (i16::from(t.x()) + dx, i16::from(t.y()) + dy);
                        let Some(h) = u8::try_from(x)
                            .ok()
                            .zip(u8::try_from(y).ok())
                            .and_then(|(x, y)| Tile::new(x, y))
                        else {
                            continue;
                        };
                        if h != t && self.role(h.index()) != CutRole::Wall {
                            self.edge(exit(h), entry(t), INF);
                        }
                    }
                }
            }
            for (_, n) in t.neighbors() {
                if self.role(n.index()) != CutRole::Wall {
                    self.edge(exit(t), entry(n), INF);
                }
            }
        }
    }

    #[inline]
    fn role(&self, i: usize) -> CutRole {
        self.roles.get(i).copied().unwrap_or(CutRole::Wall)
    }

    /// A forward edge and its reverse, at indices `e` and `e ^ 1`.
    fn edge(&mut self, from: usize, to: usize, cap: u32) {
        for (a, b, c) in [(from, to, cap), (to, from, 0)] {
            let id = self.to.len() as u32;
            self.to.push(b as u32);
            self.cap.push(c);
            self.next.push(self.head.get(a).copied().unwrap_or(NONE));
            if let Some(h) = self.head.get_mut(a) {
                *h = id;
            }
        }
    }

    /// Breadth-first levels from the source over edges with capacity left; whether the sink is
    /// reached.
    fn levels(&mut self) -> bool {
        self.level.clear();
        self.level.resize(NODES, NONE);
        self.queue.clear();
        if let Some(l) = self.level.get_mut(SOURCE) {
            *l = 0;
        }
        self.queue.push(SOURCE as u32);
        let mut read = 0;
        while let Some(&u) = self.queue.get(read) {
            read += 1;
            let lu = self.level.get(u as usize).copied().unwrap_or(NONE);
            let mut e = self.head.get(u as usize).copied().unwrap_or(NONE);
            while e != NONE {
                let (v, c) = (self.to_of(e), self.cap_of(e));
                if c > 0 && self.level.get(v as usize).is_some_and(|&l| l == NONE) {
                    if let Some(l) = self.level.get_mut(v as usize) {
                        *l = lu.saturating_add(1);
                    }
                    self.queue.push(v);
                }
                e = self.next_of(e);
            }
        }
        self.level.get(SINK).is_some_and(|&l| l != NONE)
    }

    /// One augmenting path in the level graph (iterative, with current arcs); the amount pushed, 0
    /// when the level graph is exhausted.
    fn augment(&mut self) -> u32 {
        self.path.clear();
        let mut u = SOURCE as u32;
        loop {
            if u as usize == SINK {
                let f = self.path.iter().map(|&e| self.cap_of(e)).min().unwrap_or(0);
                for i in 0..self.path.len() {
                    let Some(&e) = self.path.get(i) else { continue };
                    if let Some(c) = self.cap.get_mut(e as usize) {
                        *c = c.saturating_sub(f);
                    }
                    if let Some(c) = self.cap.get_mut((e ^ 1) as usize) {
                        *c = c.saturating_add(f);
                    }
                }
                return f;
            }
            let lu = self.level.get(u as usize).copied().unwrap_or(NONE);
            let mut advanced = false;
            loop {
                let e = self.arc.get(u as usize).copied().unwrap_or(NONE);
                if e == NONE {
                    break;
                }
                let v = self.to_of(e);
                if self.cap_of(e) > 0
                    && self
                        .level
                        .get(v as usize)
                        .is_some_and(|&l| l == lu.saturating_add(1))
                {
                    self.path.push(e);
                    u = v;
                    advanced = true;
                    break;
                }
                let n = self.next_of(e);
                if let Some(a) = self.arc.get_mut(u as usize) {
                    *a = n;
                }
            }
            if !advanced {
                // A dead end: drop it from the level graph and step back past the edge that led here.
                if u as usize == SOURCE {
                    return 0;
                }
                if let Some(l) = self.level.get_mut(u as usize) {
                    *l = NONE;
                }
                let Some(e) = self.path.pop() else { return 0 };
                u = self.to_of(e ^ 1);
                let n = self.next_of(e);
                if let Some(a) = self.arc.get_mut(u as usize) {
                    *a = n;
                }
            }
        }
    }

    #[inline]
    fn to_of(&self, e: u32) -> u32 {
        self.to.get(e as usize).copied().unwrap_or(NONE)
    }

    #[inline]
    fn cap_of(&self, e: u32) -> u32 {
        self.cap.get(e as usize).copied().unwrap_or(0)
    }

    #[inline]
    fn next_of(&self, e: u32) -> u32 {
        self.next.get(e as usize).copied().unwrap_or(NONE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustedscreep_consts::ROOM_SIZE;

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

    fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).expect("in room")
    }

    /// Whether a source still reaches a sink with the cut tiles closed (8-connected).
    fn separated(roles: &[CutRole; ROOM_AREA], cut: &[Tile]) -> bool {
        let mut closed = [false; ROOM_AREA];
        for t in cut {
            closed[t.index()] = true;
        }
        let mut seen = [false; ROOM_AREA];
        let mut stack: Vec<Tile> = (0..ROOM_AREA)
            .filter(|&i| roles[i] == CutRole::Source)
            .map(|i| Tile::from_index(i).expect("index"))
            .collect();
        for t in &stack {
            seen[t.index()] = true;
        }
        while let Some(t) = stack.pop() {
            if roles[t.index()] == CutRole::Sink {
                return false;
            }
            for (_, n) in t.neighbors() {
                let i = n.index();
                if !seen[i] && !closed[i] && roles[i] != CutRole::Wall {
                    seen[i] = true;
                    stack.push(n);
                }
            }
        }
        true
    }

    fn solve(roles: &[CutRole; ROOM_AREA]) -> Result<Cut, CutError> {
        MinCut::new().solve(|t| roles[t.index()], 0)
    }

    /// Walls everywhere, then an open band from the west edge (the sources) to a protected block in
    /// the east, narrowed to `width` tiles at x = 20.
    fn corridor(width: u8) -> [CutRole; ROOM_AREA] {
        let mut roles = [CutRole::Wall; ROOM_AREA];
        for x in 0..40u8 {
            for y in 10..30u8 {
                let narrow = x == 20 && !(10..10 + width).contains(&y);
                if narrow {
                    continue;
                }
                roles[tile(x, y).index()] = match x {
                    0 => CutRole::Source,
                    1 => CutRole::Fixed,
                    35.. => CutRole::Sink,
                    _ => CutRole::Cuttable(1),
                };
            }
        }
        roles
    }

    #[test]
    fn a_corridor_is_cut_at_its_narrowest() {
        for width in [1u8, 3, 7] {
            let roles = corridor(width);
            let cut = solve(&roles).expect("bounded");
            assert_eq!(cut.cost, u32::from(width));
            assert_eq!(cut.tiles.len(), usize::from(width));
            assert!(cut.tiles.iter().all(|t| t.x() == 20), "{:?}", cut.tiles);
            assert!(separated(&roles, &cut.tiles));
        }
    }

    /// Two equally narrow places: the cut nearest the sources is taken (more ground inside).
    #[test]
    fn the_cut_nearest_the_sources_is_taken() {
        let mut roles = corridor(2);
        for y in 12..30u8 {
            roles[tile(8, y).index()] = CutRole::Wall;
        }
        let cut = solve(&roles).expect("bounded");
        assert_eq!(cut.cost, 2);
        assert!(cut.tiles.iter().all(|t| t.x() == 8), "{:?}", cut.tiles);
    }

    /// A dear narrow place moves the cut beside it: the 3 narrow tiles at cost 5 (15) lose to the
    /// 4 tiles each side of them that every path through the narrow crosses (y 10 to 13, cost 4),
    /// and of the two sides the one nearest the sources (x = 19) is taken.
    #[test]
    fn costs_steer_the_cut() {
        let mut roles = corridor(3);
        for y in 10..13u8 {
            roles[tile(20, y).index()] = CutRole::Cuttable(5);
        }
        let cut = solve(&roles).expect("bounded");
        assert_eq!(cut.cost, 4);
        assert!(
            cut.tiles
                .iter()
                .all(|t| t.x() == 19 && (10..14).contains(&t.y())),
            "{:?}",
            cut.tiles
        );
        assert!(separated(&roles, &cut.tiles));
    }

    /// An unbounded path found after finite flow has crossed its edges is still unbounded (the
    /// leftover capacity of those edges is below `INF` by then).
    #[test]
    fn an_unbounded_path_after_finite_flow_is_unbounded() {
        let rows = [
            "#....##T", "#...##.T", "S......T", "S#..c#.T", "S..#...T", "#...##.T", "##c.#.#T",
            "Sc...#.T",
        ];
        let mut roles = [CutRole::Wall; ROOM_AREA];
        for (dy, row) in rows.iter().enumerate() {
            for (x, ch) in row.chars().enumerate() {
                roles[tile(x as u8, 20 + dy as u8).index()] = match ch {
                    '#' => CutRole::Wall,
                    'S' => CutRole::Source,
                    '.' => CutRole::Fixed,
                    'c' => CutRole::Cuttable(1),
                    _ => CutRole::Sink,
                };
            }
        }
        assert_eq!(solve(&roles), Err(CutError::Unbounded));
    }

    #[test]
    fn a_source_beside_a_sink_cannot_be_cut() {
        let mut roles = [CutRole::Wall; ROOM_AREA];
        roles[tile(0, 5).index()] = CutRole::Source;
        roles[tile(1, 5).index()] = CutRole::Fixed;
        roles[tile(2, 5).index()] = CutRole::Sink;
        assert_eq!(solve(&roles), Err(CutError::Unbounded));
    }

    #[test]
    fn no_path_needs_no_cut() {
        let mut roles = corridor(3);
        for y in 10..30u8 {
            roles[tile(20, y).index()] = CutRole::Wall;
        }
        let cut = solve(&roles).expect("bounded");
        assert_eq!(
            cut,
            Cut {
                tiles: Vec::new(),
                cost: 0
            }
        );
    }

    /// The smallest separating subset of a few cuttable tiles, by trying every subset.
    fn brute(roles: &[CutRole; ROOM_AREA]) -> Option<u32> {
        let cuttable: Vec<Tile> = (0..ROOM_AREA)
            .filter(|&i| matches!(roles[i], CutRole::Cuttable(_)))
            .map(|i| Tile::from_index(i).expect("index"))
            .collect();
        let mut best = None;
        for mask in 0u32..(1 << cuttable.len()) {
            let chosen: Vec<Tile> = cuttable
                .iter()
                .enumerate()
                .filter(|(b, _)| mask & (1 << b) != 0)
                .map(|(_, &t)| t)
                .collect();
            let cost: u32 = chosen
                .iter()
                .map(|t| match roles[t.index()] {
                    CutRole::Cuttable(c) => u32::from(c.max(1)),
                    _ => 0,
                })
                .sum();
            if best.is_some_and(|b| cost >= b) {
                continue;
            }
            if separated(roles, &chosen) {
                best = Some(cost);
            }
        }
        best
    }

    /// Random small rooms with at most 10 cuttable tiles: the solver's cost is the brute-force
    /// minimum, its cut separates, and an unbounded answer means no subset separates. The cuttable
    /// tiles sit in two gate columns (x = 3 and 4) so most rooms have a bounded cut, and paths step
    /// diagonally through the gates' gaps.
    #[test]
    fn random_small_rooms_match_brute_force() {
        let mut rng = Lcg(11);
        let (mut bounded, mut unbounded) = (0, 0);
        for _ in 0..150 {
            let mut roles = [CutRole::Wall; ROOM_AREA];
            // An 8 x 8 patch at the west edge: x = 0 is the source column, x = 7 the sink column.
            for x in 0..8u8 {
                for y in 20..28u8 {
                    let r = rng.next() % 100;
                    roles[tile(x, y).index()] = match x {
                        0 if r < 50 => CutRole::Source,
                        0 => CutRole::Wall,
                        7 => CutRole::Sink,
                        3 | 4 if r < 25 => CutRole::Wall,
                        3 | 4 if r < 85 => CutRole::Cuttable(1 + (rng.next() % 3) as u8),
                        3 | 4 => CutRole::Fixed,
                        _ if r < 20 => CutRole::Wall,
                        _ => CutRole::Fixed,
                    };
                }
            }
            let mut cuttable = 0;
            for cell in roles.iter_mut() {
                if matches!(cell, CutRole::Cuttable(_)) {
                    cuttable += 1;
                    if cuttable > 10 {
                        *cell = CutRole::Fixed;
                    }
                }
            }
            let want = brute(&roles);
            match solve(&roles) {
                Ok(cut) => {
                    bounded += 1;
                    assert_eq!(Some(cut.cost), want);
                    assert!(separated(&roles, &cut.tiles));
                }
                Err(CutError::Unbounded) => {
                    unbounded += 1;
                    assert_eq!(want, None);
                }
            }
        }
        assert!(
            bounded > 50 && unbounded > 5,
            "{bounded} bounded, {unbounded} unbounded"
        );
    }

    /// A whole open room: exits on every edge, fixed tiles beside them, a protected 5 x 5 in the
    /// middle. The cheapest cut is the ring right around the block (far cheaper than a ring near
    /// the exits), and a second solve on the same buffers gives the same cut.
    #[test]
    fn an_open_room_rings_the_protected_block() {
        let last = ROOM_SIZE - 1;
        let role = |t: Tile| {
            let (x, y) = (t.x(), t.y());
            if t.is_edge() {
                CutRole::Source
            } else if x == 1 || y == 1 || x == last - 1 || y == last - 1 {
                CutRole::Fixed
            } else if (23..28).contains(&x) && (23..28).contains(&y) {
                CutRole::Sink
            } else {
                CutRole::Cuttable(1)
            }
        };
        let mut roles = [CutRole::Wall; ROOM_AREA];
        for (i, cell) in roles.iter_mut().enumerate() {
            *cell = role(Tile::from_index(i).expect("index"));
        }
        let mut solver = MinCut::new();
        let first = solver.solve(|t| roles[t.index()], 0).expect("bounded");
        assert!(separated(&roles, &first.tiles));
        let block = 5u32;
        assert_eq!(first.cost, 4 * (block + 2) - 4);
        assert!(
            first
                .tiles
                .iter()
                .all(|t| (22..29).contains(&t.x()) && (22..29).contains(&t.y()))
        );
        let second = solver.solve(|t| roles[t.index()], 0).expect("bounded");
        assert_eq!(first, second);
    }

    /// Whether `chosen` keeps every guarded tile safe at `reach`: under a chosen tile, or with no
    /// attacker (flooding from the sources past the chosen tiles) within `reach`; and no sink reached.
    fn guarded_safe(roles: &[CutRole; ROOM_AREA], chosen: &[Tile], reach: u8) -> bool {
        let mut closed = [false; ROOM_AREA];
        for t in chosen {
            closed[t.index()] = true;
        }
        let mut seen = [false; ROOM_AREA];
        let mut stack: Vec<Tile> = (0..ROOM_AREA)
            .filter(|&i| roles[i] == CutRole::Source)
            .map(|i| Tile::from_index(i).expect("index"))
            .collect();
        for t in &stack {
            seen[t.index()] = true;
        }
        while let Some(t) = stack.pop() {
            for (_, n) in t.neighbors() {
                let i = n.index();
                if !seen[i] && !closed[i] && roles[i] != CutRole::Wall {
                    seen[i] = true;
                    stack.push(n);
                }
            }
        }
        (0..ROOM_AREA).all(|i| {
            let t = Tile::from_index(i).expect("index");
            match roles[i] {
                CutRole::Sink => !seen[i],
                CutRole::Guarded(_) => {
                    closed[i]
                        || !(0..ROOM_AREA).any(|j| {
                            let h = Tile::from_index(j).expect("index");
                            seen[j] && h != t && h.range(t) <= reach
                        })
                }
                _ => true,
            }
        })
    }

    /// Random small rooms with guarded tiles: the solver's cost is the cheapest choice of cut and
    /// guarded tiles that keeps every guarded tile safe, by trying every subset.
    #[test]
    fn guarded_tiles_match_brute_force() {
        let mut rng = Lcg(23);
        let mut cases = 0;
        for round in 0..120u32 {
            let reach = 1 + (round % 3) as u8;
            let mut roles = [CutRole::Wall; ROOM_AREA];
            for x in 0..8u8 {
                for y in 20..28u8 {
                    let r = rng.next() % 100;
                    roles[tile(x, y).index()] = match x {
                        0 if r < 50 => CutRole::Source,
                        0 => CutRole::Wall,
                        _ if r < 25 => CutRole::Wall,
                        3 | 4 if r < 70 => CutRole::Cuttable(1 + (rng.next() % 2) as u8),
                        6 | 7 if r < 40 => CutRole::Guarded(1 + (rng.next() % 3) as u8),
                        _ => CutRole::Fixed,
                    };
                }
            }
            let mut choosable = 0;
            for cell in roles.iter_mut() {
                if matches!(cell, CutRole::Cuttable(_) | CutRole::Guarded(_)) {
                    choosable += 1;
                    if choosable > 11 && matches!(cell, CutRole::Cuttable(_)) {
                        *cell = CutRole::Fixed;
                    }
                }
            }
            let options: Vec<Tile> = (0..ROOM_AREA)
                .filter(|&i| matches!(roles[i], CutRole::Cuttable(_) | CutRole::Guarded(_)))
                .map(|i| Tile::from_index(i).expect("index"))
                .collect();
            if options.len() > 14 {
                continue;
            }
            let price = |t: &Tile| match roles[t.index()] {
                CutRole::Cuttable(c) | CutRole::Guarded(c) => u32::from(c.max(1)),
                _ => 0,
            };
            let mut best: Option<u32> = None;
            for mask in 0u32..(1 << options.len()) {
                let chosen: Vec<Tile> = options
                    .iter()
                    .enumerate()
                    .filter(|(b, _)| mask & (1 << b) != 0)
                    .map(|(_, &t)| t)
                    .collect();
                let cost: u32 = chosen.iter().map(price).sum();
                if best.is_some_and(|b| cost >= b) {
                    continue;
                }
                if guarded_safe(&roles, &chosen, reach) {
                    best = Some(cost);
                }
            }
            let cut = MinCut::new()
                .solve(|t| roles[t.index()], reach)
                .expect("a guarded tile can always be cut");
            assert_eq!(Some(cut.cost), best, "round {round}");
            assert!(guarded_safe(&roles, &cut.tiles, reach), "round {round}");
            cases += 1;
        }
        assert!(cases > 60, "{cases} cases");
    }
}
