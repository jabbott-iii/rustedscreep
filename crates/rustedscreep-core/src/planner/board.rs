//! A planner's working state: what is planned on each tile so far, with the game's placement rules
//! checked on every placement, and the items staged by level as they are placed.
//!
//! Each tile holds at most one costed structure, plus a road and a rampart over it (V237). A tile
//! can be kept clear: no obstacle goes there (the ground creeps stand on to mine, upgrade and
//! fill), though roles may still take it on purpose ([`Board::place_role`]).

use rustedscreep_consts::{ROOM_AREA, StructureType};

use super::layout::{Item, KINDS, Layout, MAX_LEVEL, PlanInput, Roles, kind_rank};
use super::rules::{blocks, earliest_level, tile_allows};
use crate::movement::{CostGrid, CostProfile, Footprint, TerrainGrid, TerrainKind, Tile};

/// No costed structure on the tile.
const FREE: u8 = 0;
/// The controller, a source or the mineral stands here (on a wall, V228).
const NATURAL: u8 = u8::MAX;

/// See the module docs.
pub struct Board<'a> {
    terrain: &'a TerrainGrid,
    /// 0, [`NATURAL`], or `kind_rank + 1` of the costed structure on the tile (not a road or rampart).
    what: Box<[u8; ROOM_AREA]>,
    road: Box<[bool; ROOM_AREA]>,
    rampart: Box<[bool; ROOM_AREA]>,
    clear: Box<[bool; ROOM_AREA]>,
    mineral: Option<Tile>,
    counts: [u32; KINDS.len()],
    items: Vec<Item>,
    pub roles: Roles,
}

impl<'a> Board<'a> {
    pub fn new(input: &PlanInput<'a>) -> Self {
        let mut b = Self {
            terrain: input.terrain,
            what: Box::new([FREE; ROOM_AREA]),
            road: Box::new([false; ROOM_AREA]),
            rampart: Box::new([false; ROOM_AREA]),
            clear: Box::new([false; ROOM_AREA]),
            mineral: input.mineral,
            counts: [0; KINDS.len()],
            items: Vec::new(),
            roles: Roles::default(),
        };
        for t in input
            .sources
            .iter()
            .copied()
            .chain(input.mineral)
            .chain([input.controller])
        {
            if let Some(cell) = b.what.get_mut(t.index()) {
                *cell = NATURAL;
            }
        }
        b
    }

    pub fn terrain(&self) -> &TerrainGrid {
        self.terrain
    }

    /// The terrain of `t`.
    pub fn kind(&self, t: Tile) -> TerrainKind {
        self.terrain.kind(t)
    }

    /// The costed structure planned on `t`, if any.
    pub fn structure(&self, t: Tile) -> Option<StructureType> {
        match self.what.get(t.index()).copied().unwrap_or(NATURAL) {
            FREE | NATURAL => None,
            k => KINDS.get(usize::from(k - 1)).copied(),
        }
    }

    /// Whether the controller, a source or the mineral stands on `t`.
    pub fn natural(&self, t: Tile) -> bool {
        self.what.get(t.index()) == Some(&NATURAL)
    }

    pub fn has_road(&self, t: Tile) -> bool {
        self.road.get(t.index()).copied().unwrap_or(false)
    }

    pub fn has_rampart(&self, t: Tile) -> bool {
        self.rampart.get(t.index()).copied().unwrap_or(false)
    }

    pub fn is_clear(&self, t: Tile) -> bool {
        self.clear.get(t.index()).copied().unwrap_or(false)
    }

    /// Keeps `t` free of obstacles from now on (it may already hold a road or a container).
    pub fn keep_clear(&mut self, t: Tile) {
        if let Some(c) = self.clear.get_mut(t.index()) {
            *c = true;
        }
    }

    /// Whether a creep could stand on `t` once the plan is built: inside the room, not a wall, and
    /// no obstacle planned there.
    pub fn walkable(&self, t: Tile) -> bool {
        !t.is_edge()
            && self.kind(t) != TerrainKind::Wall
            && !self.natural(t)
            && !self.structure(t).is_some_and(blocks)
    }

    /// Whether nothing is planned on `t` and it is open ground inside the room: an obstacle may
    /// go here unless it is kept clear.
    pub fn open(&self, t: Tile) -> bool {
        self.walkable(t) && self.structure(t).is_none() && !self.has_road(t)
    }

    /// Whether `kind` may be placed on `t` now: the game's rules (V237) and the plan's own (no
    /// road on a wall, under an obstacle, or twice; no obstacle on a road or kept-clear tile).
    pub fn can_place(&self, kind: StructureType, t: Tile) -> bool {
        self.can_place_inner(kind, t, false)
    }

    /// As [`Board::can_place`], ignoring the kept-clear mark (see [`Board::place_role`]).
    pub fn can_place_role(&self, kind: StructureType, t: Tile) -> bool {
        self.can_place_inner(kind, t, true)
    }

    fn can_place_inner(&self, kind: StructureType, t: Tile, role: bool) -> bool {
        if !tile_allows(self.terrain, kind, t) {
            return false;
        }
        let here = self.structure(t);
        match kind {
            StructureType::Road => {
                !self.has_road(t)
                    && self.kind(t) != TerrainKind::Wall
                    && !self.natural(t)
                    && !here.is_some_and(blocks)
            }
            StructureType::Rampart => !self.has_rampart(t) && !self.natural(t),
            StructureType::Extractor => self.mineral == Some(t) && self.count(kind) == 0,
            StructureType::Container => here.is_none() && !self.natural(t),
            _ => {
                here.is_none()
                    && !self.natural(t)
                    && !self.has_road(t)
                    && (role || !self.is_clear(t) || !blocks(kind))
            }
        }
    }

    /// How many of `kind` are planned.
    pub fn count(&self, kind: StructureType) -> u32 {
        self.counts.get(kind_rank(kind)).copied().unwrap_or(0)
    }

    /// The level the next `kind` would belong from (its count's earliest level, V234), or `None`
    /// when the top level allows no more.
    pub fn next_level(&self, kind: StructureType) -> Option<u8> {
        earliest_level(kind, self.count(kind))
    }

    /// Places `kind` on `t` at the next level its count allows. `None` (and nothing placed) when
    /// the tile does not allow it or the top level allows no more.
    pub fn place(&mut self, kind: StructureType, t: Tile) -> Option<u8> {
        let level = self.next_level(kind)?;
        self.place_at(kind, t, level, false).then_some(level)
    }

    /// As [`Board::place`], but at a given level (at least the next one the count allows), for
    /// items a planner builds later than they are allowed (roads with what they serve, ramparts).
    /// Nothing is placed for a level above [`MAX_LEVEL`].
    pub fn place_at_level(&mut self, kind: StructureType, t: Tile, level: u8) -> bool {
        if level > MAX_LEVEL {
            return false;
        }
        let Some(first) = self.next_level(kind) else {
            return false;
        };
        self.place_at(kind, t, level.max(first), false)
    }

    /// As [`Board::place`], ignoring the kept-clear mark: for the role tiles the economy needs
    /// there (the controller link next to its upgraders, a source link).
    pub fn place_role(&mut self, kind: StructureType, t: Tile) -> Option<u8> {
        let level = self.next_level(kind)?;
        self.place_at(kind, t, level, true).then_some(level)
    }

    fn place_at(&mut self, kind: StructureType, t: Tile, level: u8, role: bool) -> bool {
        if !self.can_place_inner(kind, t, role) {
            return false;
        }
        let i = t.index();
        match kind {
            StructureType::Road => {
                if let Some(c) = self.road.get_mut(i) {
                    *c = true;
                }
            }
            StructureType::Rampart => {
                if let Some(c) = self.rampart.get_mut(i) {
                    *c = true;
                }
            }
            _ => {
                if let Some(c) = self.what.get_mut(i) {
                    *c = (kind_rank(kind) as u8).saturating_add(1);
                }
            }
        }
        if let Some(c) = self.counts.get_mut(kind_rank(kind)) {
            *c += 1;
        }
        self.items.push(Item {
            kind,
            tile: t,
            level,
        });
        true
    }

    /// The items placed so far, in placement order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The level the costed structure planned on `t` belongs from (not a road or a rampart).
    pub fn level_of(&self, t: Tile) -> Option<u8> {
        self.items
            .iter()
            .find(|i| {
                i.tile == t && !matches!(i.kind, StructureType::Road | StructureType::Rampart)
            })
            .map(|i| i.level)
    }

    /// Moves the road planned on `t` to `level` when it belongs from a later one, so a road
    /// routed over it at `level` is whole from that level. Whether it moved.
    pub fn lower_road(&mut self, t: Tile, level: u8) -> bool {
        self.lower(StructureType::Road, t, level)
    }

    /// Moves the `kind` (a road or a rampart) planned on `t` to `level`, or to the first level
    /// that allows one if that is later, when it belongs from a later level. Whether it moved.
    pub fn lower(&mut self, kind: StructureType, t: Tile, level: u8) -> bool {
        let first = earliest_level(kind, 0).unwrap_or(level);
        match self
            .items
            .iter_mut()
            .find(|i| i.tile == t && i.kind == kind)
        {
            Some(i) if i.level > level.max(first) => {
                i.level = level.max(first);
                true
            }
            _ => false,
        }
    }

    /// Whether a spawn on `sp` keeps the validator's spawn rule (V174), on ground where `free`
    /// says which tiles stay walkable and `reached` which of them the hub's ground reaches: every
    /// free neighbour but the hub joined to the room's exits (reached, since the hub's ground
    /// reaches them, or else joined over `free` ground of its own, [`Board::joins_exit`]), so a new
    /// creep is never placed where it cannot leave, and one of them no creep's post (the hub, a
    /// mining tile), so a creep can be placed at all. `reached` stands for joined only while the
    /// hub's ground keeps an exit (every candidate keeps the exit sides the bare room reaches with
    /// the standing spawn in place). Out of scope: a room with no exit at all, which the generator
    /// never makes (V229), and one whose standing spawn cuts the hub's ground off from every exit,
    /// which no plan can serve (the validator reports its spawns).
    pub fn spawn_free(
        &self,
        sp: Tile,
        hub: Option<Tile>,
        free: impl Fn(Tile) -> bool,
        reached: impl Fn(Tile) -> bool,
    ) -> bool {
        let mut exit = false;
        for (_, n) in sp.neighbors() {
            if Some(n) == hub || !free(n) {
                continue;
            }
            if !reached(n) && !self.joins_exit(n, &free) {
                return false;
            }
            exit |= !self.roles.mining.contains(&n);
        }
        exit
    }

    /// Whether `t` is an exit as the validator reads one: an edge tile on a side (corners have
    /// none) that is not a wall.
    pub fn is_exit(&self, t: Tile) -> bool {
        t.side().is_some() && self.kind(t) != TerrainKind::Wall
    }

    /// Whether `from` is joined to an exit ([`Board::is_exit`]) over the inner tiles `free`
    /// accepts, 8-connected: the validator's reading of ground a creep can leave the room from. A
    /// flood of its own, for the rare ground the hub's does not reach.
    pub fn joins_exit(&self, from: Tile, free: impl Fn(Tile) -> bool) -> bool {
        let mut seen = vec![false; ROOM_AREA];
        let mut stack = vec![from];
        if let Some(c) = seen.get_mut(from.index()) {
            *c = true;
        }
        while let Some(t) = stack.pop() {
            for (_, n) in t.neighbors() {
                if n.is_edge() {
                    if self.is_exit(n) {
                        return true;
                    }
                    continue;
                }
                if free(n)
                    && let Some(c) = seen.get_mut(n.index())
                    && !*c
                {
                    *c = true;
                    stack.push(n);
                }
            }
        }
        false
    }

    /// Whether the standing spawn on `t` keeps the spawn rule (V174) on the board as it is, with
    /// no hub yet: every free neighbour joined to an exit and one of them no post (`posts`: the
    /// mining tiles the input forces, `sites::forced_tiles`). One flood from the exits. A spawn
    /// that does not was given so: the planners leave it to the validator.
    pub fn standing_spawn_free(&self, t: Tile, posts: &[Tile]) -> bool {
        let free = |n: Tile| self.walkable(n) && n != t;
        let mut joined = vec![false; ROOM_AREA];
        let mut stack: Vec<Tile> = Vec::new();
        for n in (0..ROOM_AREA).filter_map(Tile::from_index) {
            if free(n)
                && n.neighbors().any(|(_, e)| self.is_exit(e))
                && let Some(c) = joined.get_mut(n.index())
            {
                *c = true;
                stack.push(n);
            }
        }
        while let Some(u) = stack.pop() {
            for (_, n) in u.neighbors() {
                if free(n)
                    && let Some(c) = joined.get_mut(n.index())
                    && !*c
                {
                    *c = true;
                    stack.push(n);
                }
            }
        }
        let reached = |n: Tile| joined.get(n.index()).copied().unwrap_or(false);
        let mut exit = false;
        for (_, n) in t.neighbors() {
            if !free(n) {
                continue;
            }
            if !reached(n) {
                return false;
            }
            exit |= !posts.contains(&n);
        }
        exit
    }

    /// Whether every spawn beside `t` keeps a free neighbour that is no creep's post once `t`
    /// takes an obstacle or a post (V174): with `blocked` the tiles that are not free.
    pub fn leaves_spawn_exit(&self, t: Tile, blocked: impl Fn(Tile) -> bool) -> bool {
        t.neighbors().all(|(_, sp)| {
            self.structure(sp) != Some(StructureType::Spawn)
                || sp.neighbors().any(|(_, w)| {
                    w != t
                        && !blocked(w)
                        && Some(w) != self.roles.hub
                        && !self.roles.mining.contains(&w)
                })
        })
    }

    /// Fills `grid` with the room as planned so far: terrain at `profile`'s costs, planned roads
    /// at road cost, planned obstacles blocked (movement's footprints, V235).
    pub fn cost_grid(&self, grid: &mut CostGrid, profile: CostProfile) {
        let objects = (0..ROOM_AREA).filter_map(move |i| {
            let t = Tile::from_index(i)?;
            if self.structure(t).is_some_and(blocks) {
                Some((t, Footprint::Blocked))
            } else if self.has_road(t) {
                Some((t, Footprint::Road))
            } else {
                None
            }
        });
        grid.build(self.terrain, objects, profile);
    }

    /// The finished plan, in its canonical order.
    pub fn into_layout(self) -> Layout {
        let mut layout = Layout {
            items: self.items,
            roles: self.roles,
        };
        layout.sort();
        layout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::layout::Layout;
    use crate::planner::rules::allowed;
    use crate::planner::validate::{Problem, validate};
    use rustedscreep_consts::{ROOM_SIZE, Terrain};

    /// The validator reads an exit as an edge tile on a side, and a corner has none: ground that
    /// touches only an open corner is no way out (no corpus room has one, V229), for the board's
    /// reading as for the validator's.
    #[test]
    fn an_open_corner_is_no_exit() {
        let last = ROOM_SIZE - 1;
        let mut raw = vec![0u8; ROOM_AREA];
        for y in 0..ROOM_SIZE {
            for x in 0..ROOM_SIZE {
                let edge = x == 0 || y == 0 || x == last || y == last;
                let open = (x, y) == (0, 0) || (x == 0 && (10..=40).contains(&y));
                let inner = [(2, 1), (1, 2), (25, 8), (40, 40)].contains(&(x, y));
                if (edge && !open) || inner {
                    raw[usize::from(y) * usize::from(ROOM_SIZE) + usize::from(x)] =
                        Terrain::Wall as u8;
                }
            }
        }
        let t = TerrainGrid::from_row_major(&raw).expect("length");
        let (spawn, pocket) = (tile(2, 2), tile(1, 1));
        let sources = [tile(40, 40)];
        let input = PlanInput {
            terrain: &t,
            controller: tile(25, 8),
            sources: &sources,
            mineral: None,
            spawn: Some(spawn),
        };
        let b = Board::new(&input);
        assert!(!b.is_exit(tile(0, 0)));
        assert!(b.is_exit(tile(0, 20)));
        let free = |n: Tile| b.walkable(n) && n != spawn;
        assert!(!b.joins_exit(pocket, free));
        assert!(b.joins_exit(tile(3, 3), free));
        assert!(!b.standing_spawn_free(spawn, &[]));
        let layout = Layout {
            items: vec![Item {
                kind: StructureType::Spawn,
                tile: spawn,
                level: 1,
            }],
            roles: Roles::default(),
        };
        assert!(
            validate(&input, &layout)
                .problems
                .contains(&Problem::SpawnPocket {
                    tile: (pocket.x(), pocket.y()),
                    level: 1
                })
        );
    }

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

    #[test]
    fn placements_follow_the_object_rules_and_stage_by_count() {
        let t = terrain(&[(10, 10), (30, 30)]);
        let sources = [tile(10, 10)];
        let input = PlanInput {
            terrain: &t,
            controller: tile(30, 30),
            sources: &sources,
            mineral: None,
            spawn: None,
        };
        let mut b = Board::new(&input);
        // The natural objects' tiles take nothing.
        assert!(!b.can_place(StructureType::Road, tile(10, 10)));
        assert!(!b.can_place(StructureType::Extension, tile(30, 30)));
        // Extensions stage by the level counts (V234).
        let mut levels = Vec::new();
        for x in 0..u8::try_from(allowed(StructureType::Extension, 8)).expect("small") {
            let at = tile(5 + x % 30, 15 + x / 30 * 2);
            levels.push(b.place(StructureType::Extension, at).expect("placed"));
        }
        for (n, &l) in levels.iter().enumerate() {
            assert_eq!(Some(l), earliest_level(StructureType::Extension, n as u32));
        }
        assert_eq!(
            b.place(StructureType::Extension, tile(40, 40)),
            None,
            "the top level is full"
        );
        // One costed structure per tile; a road and a rampart go over; no road under an obstacle.
        let at = tile(20, 40);
        assert!(b.place(StructureType::Tower, at).is_some());
        assert!(!b.can_place(StructureType::Container, at));
        assert!(!b.can_place(StructureType::Road, at));
        assert!(b.place_at_level(StructureType::Rampart, at, 4));
        assert!(!b.can_place(StructureType::Rampart, at));
        // A container takes a road over it, not the other way round.
        let c = tile(22, 40);
        assert!(b.place(StructureType::Container, c).is_some());
        assert!(b.place(StructureType::Road, c).is_some());
        // Kept-clear tiles take no obstacle but a role, a road or a container.
        let k = tile(24, 40);
        b.keep_clear(k);
        assert!(!b.can_place(StructureType::Link, k));
        assert!(b.can_place(StructureType::Container, k));
        assert!(b.place_role(StructureType::Link, k).is_some());
        assert!(!b.walkable(k));
        // No item belongs past the top level (the validator would call it a bad level).
        assert!(!b.place_at_level(StructureType::Rampart, tile(30, 40), MAX_LEVEL + 1));
        assert!(!b.has_rampart(tile(30, 40)));
        let layout = b.into_layout();
        assert_eq!(layout.count(StructureType::Link, 8), 1);
        assert!(layout.items.windows(2).all(|w| (
            w[0].level,
            kind_rank(w[0].kind),
            w[0].tile.index()
        ) <= (
            w[1].level,
            kind_rank(w[1].kind),
            w[1].tile.index()
        )));
    }
}
