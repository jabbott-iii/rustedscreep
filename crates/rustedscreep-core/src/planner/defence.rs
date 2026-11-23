//! Ramparts by minimum cut: the fewest ramparts that keep every protected structure under a rampart
//! or out of ranged reach (V243) of every tile a hostile creep can stand on, coming from an exit.
//!
//! Roles for the cut ([`MinCut`]): walls and the natural objects are not in the graph; edge tiles are
//! sources (hostile creeps arrive there, V244); tiles no rampart may stand on (the border rule,
//! V237) are fixed; each protected structure is guarded at [`RANGED_REACH`]; every other tile can
//! take a rampart at cost 1. So the cut puts ramparts across the passages where that is cheapest and
//! over a structure itself where it is not (a structure beside an exit, for one).
//!
//! Walls in the line (ADR-033): a constructed wall never decays (V309) and closes a tile to every
//! creep, the owner's too (V313), while a rampart loses `RAMPART_DECAY_AMOUNT` every
//! `RAMPART_DECAY_TIME` (V304) and lets the owner's creeps through. So [`apply`] keeps ramparts only
//! where own creeps must pass (the gates, [`gates`]) and over protected structures, and closes the
//! rest of the cut with walls: the gates become the line's choke points, and only they need upkeep
//! against decay.

use rustedscreep_consts::{ROOM_AREA, StructureType};

use super::board::Board;
use super::layout::{PlanError, PlanInput};
use super::mincut::{CutError, CutRole};
use super::rules::{RANGED_REACH, blocks, edge_tiles, protected, tile_allows};
use super::scratch::Scratch;
use crate::movement::{TerrainKind, Tile};

/// The rampart plan for `protect`: the ramparts across passages, and those over a protected
/// structure itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ramparts {
    pub cut: Vec<Tile>,
    pub own: Vec<Tile>,
}

impl Ramparts {
    /// Every tile that takes a rampart, in index order, without repeats.
    pub fn tiles(&self) -> Vec<Tile> {
        let mut all: Vec<Tile> = self.cut.iter().chain(self.own.iter()).copied().collect();
        all.sort();
        all.dedup();
        all
    }
}

/// The protected structures of the board, sorted: every obstacle except the links the roles keep at
/// the sources and the controller.
pub fn protected_tiles(board: &Board) -> Vec<Tile> {
    let r = &board.roles;
    let remote: Vec<Tile> = r
        .controller_link
        .into_iter()
        .chain(r.source_links.iter().flatten().copied())
        .collect();
    let mut out: Vec<Tile> = board
        .items()
        .iter()
        .filter(|i| {
            i.kind != StructureType::Rampart
                && blocks(i.kind)
                && protected(i.kind, remote.contains(&i.tile))
        })
        .map(|i| i.tile)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Plans the ramparts for `protect` (in any order, repeats allowed) in the room `input`.
pub fn plan(
    input: &PlanInput<'_>,
    protect: &[Tile],
    s: &mut Scratch,
) -> Result<Ramparts, PlanError> {
    // Looked up by binary search below, so sorted here whatever order the caller keeps.
    let mut protect = protect.to_vec();
    protect.sort_unstable();
    protect.dedup();
    let protect = protect.as_slice();
    let terrain = input.terrain;
    let natural =
        |t: Tile| t == input.controller || input.sources.contains(&t) || input.mineral == Some(t);
    let role = |t: Tile| {
        if terrain.kind(t) == TerrainKind::Wall || natural(t) {
            CutRole::Wall
        } else if t.is_edge() {
            CutRole::Source
        } else if !tile_allows(terrain, StructureType::Rampart, t) {
            CutRole::Fixed
        } else if protect.binary_search(&t).is_ok() {
            CutRole::Guarded(1)
        } else {
            CutRole::Cuttable(1)
        }
    };
    match s.cut.solve(role, RANGED_REACH) {
        Ok(cut) => {
            let (own, line) = cut
                .tiles
                .into_iter()
                .partition(|t| protect.binary_search(t).is_ok());
            Ok(Ramparts { cut: line, own })
        }
        // A protected structure stands on ground no rampart may take (it cannot: the border rule
        // is the same for both, V237), so this is a planner bug, not a room the plan can fix.
        Err(CutError::Unbounded) => Err(PlanError::NoRoom("ramparts")),
    }
}

/// The level each rampart of `ramparts` belongs from: `line` (the first level ramparts are
/// allowed) for the cut's, and for each over a protected structure that borders the ground hostile
/// creeps reach past all of them (a guarded tile the cut takes also closes the way through it, so
/// its rampart is part of the line); each other over a structure from that structure's level
/// (`level_of`), since a rampart over nothing yet only decays (V241). A rampart that borders no
/// hostile ground opens no way in when it is left out, so the line is whole at every level from
/// `line`. That holds when `ramparts` is the cut [`plan`] solved for this room and every rampart
/// and wall the plan will have, `line` is the first level ramparts are allowed (the validator
/// checks from there), and the structures' levels do not change afterwards. In tile order.
pub fn levels(
    input: &PlanInput<'_>,
    ramparts: &Ramparts,
    line: u8,
    level_of: impl Fn(Tile) -> Option<u8>,
) -> Vec<(Tile, u8)> {
    let all = ramparts.tiles();
    let terrain = input.terrain;
    let natural =
        |t: Tile| t == input.controller || input.sources.contains(&t) || input.mineral == Some(t);
    let open = |t: Tile| {
        terrain.kind(t) != TerrainKind::Wall && !natural(t) && all.binary_search(&t).is_err()
    };
    let mut hostile = vec![false; ROOM_AREA];
    let mut stack: Vec<Tile> = Vec::new();
    for t in edge_tiles().filter(|&t| open(t)) {
        if let Some(c) = hostile.get_mut(t.index()) {
            *c = true;
            stack.push(t);
        }
    }
    while let Some(t) = stack.pop() {
        for (_, n) in t.neighbors() {
            if open(n)
                && let Some(c) = hostile.get_mut(n.index())
                && !*c
            {
                *c = true;
                stack.push(n);
            }
        }
    }
    let borders = |t: Tile| {
        t.neighbors()
            .any(|(_, n)| hostile.get(n.index()).copied().unwrap_or(false))
    };
    all.iter()
        .map(|&t| {
            let level = if !ramparts.own.contains(&t) || borders(t) {
                line
            } else {
                level_of(t).map_or(line, |l| l.max(line))
            };
            (t, level)
        })
        .collect()
}

/// The cut's tiles that stay ramparts so own creeps pass the line (ADR-033, the user's choice,
/// 2026-11-23: "roads, and one per exit region"), in index order. They are:
/// - each cut tile under a planned road (the way to a mining tile or the controller outside the
///   line, which own creeps walk), and each that may not take a wall (a kept-clear tile, or one
///   with another planned structure: [`Board::can_place`]);
/// - for each region of the ground hostile creeps reach from the exits past the line (8-connected,
///   as creeps move) that own creeps cannot reach yet from the hub, the cut tile between it and
///   ground they reach that is nearest a planned tower (Chebyshev range, then nearest the hub, then
///   index), until no more regions can be joined. So every exit region the bare room reached is
///   reached again from the base (the validator's exits rule), and the way through is under the
///   towers' fire, their damage falling off with range (V239).
///
/// Reach is judged on the ground as the plan will stand: planned obstacles and the cut's walls
/// close a tile, the gates do not. A gate counts for a region only once it joins the region to the
/// base: one that opens on an inside pocket the line cuts off from the base (ground kept from
/// hostile creeps only to keep them out of ranged reach of a structure, V243) joins nothing.
///
/// Every other cut tile takes a wall. The ramparts over protected structures are not gates (the
/// structure under one is an obstacle) and stay ramparts. Without a hub or a standing spawn to
/// reach from, each region gets the cut tile bordering it nearest a tower.
pub fn gates(board: &Board, input: &PlanInput<'_>, ramparts: &Ramparts) -> Vec<Tile> {
    let all = ramparts.tiles();
    let terrain = input.terrain;
    let natural =
        |t: Tile| t == input.controller || input.sources.contains(&t) || input.mineral == Some(t);
    let open = |t: Tile| {
        terrain.kind(t) != TerrainKind::Wall && !natural(t) && all.binary_search(&t).is_err()
    };
    // The hostile ground by region: 0 for none, else the region's number from 1.
    let mut region = vec![0u16; ROOM_AREA];
    let mut stack: Vec<Tile> = Vec::new();
    let mut regions = 0u16;
    for start in edge_tiles().filter(|&t| open(t)) {
        if region.get(start.index()).copied().unwrap_or(1) != 0 {
            continue;
        }
        regions = regions.saturating_add(1);
        if let Some(c) = region.get_mut(start.index()) {
            *c = regions;
        }
        stack.push(start);
        while let Some(t) = stack.pop() {
            for (_, n) in t.neighbors() {
                if open(n)
                    && let Some(c) = region.get_mut(n.index())
                    && *c == 0
                {
                    *c = regions;
                    stack.push(n);
                }
            }
        }
    }
    let region_of = |t: Tile| region.get(t.index()).copied().unwrap_or(0);
    let borders = |t: Tile, k: u16| t.neighbors().any(|(_, n)| region_of(n) == k);
    let mut cut = ramparts.cut.clone();
    cut.sort_unstable();
    cut.dedup();
    let mut out: Vec<Tile> = cut
        .iter()
        .copied()
        .filter(|&t| board.has_road(t) || !board.can_place(StructureType::Wall, t))
        .collect();
    let towers: Vec<Tile> = board
        .items()
        .iter()
        .filter(|i| i.kind == StructureType::Tower)
        .map(|i| i.tile)
        .collect();
    let hub = board.roles.hub;
    let rank = |t: Tile| {
        (
            towers.iter().map(|&w| t.range(w)).min().unwrap_or(u8::MAX),
            hub.map_or(u8::MAX, |h| t.range(h)),
            t.index(),
        )
    };
    let Some(start) = hub.or(input.spawn) else {
        for k in 1..=regions {
            if out.iter().any(|&g| borders(g, k)) {
                continue;
            }
            if let Some(t) = cut
                .iter()
                .copied()
                .filter(|&t| borders(t, k))
                .min_by_key(|&t| rank(t))
            {
                out.push(t);
            }
        }
        out.sort_unstable();
        out.dedup();
        return out;
    };
    // Reach from the hub over the ground as the plan will stand.
    let mut reached = vec![false; ROOM_AREA];
    let mut served = vec![false; usize::from(regions) + 1];
    let flood = |from: Tile,
                 out: &[Tile],
                 reached: &mut Vec<bool>,
                 served: &mut Vec<bool>,
                 stack: &mut Vec<Tile>| {
        let pass = |t: Tile| {
            board.walkable(t) && (cut.binary_search(&t).is_err() || out.binary_search(&t).is_ok())
        };
        if !pass(from) || reached.get(from.index()).copied().unwrap_or(true) {
            return;
        }
        if let Some(r) = reached.get_mut(from.index()) {
            *r = true;
        }
        stack.push(from);
        while let Some(t) = stack.pop() {
            if let Some(s) = served.get_mut(usize::from(region_of(t))) {
                *s = true;
            }
            for (_, n) in t.neighbors() {
                if pass(n)
                    && let Some(r) = reached.get_mut(n.index())
                    && !*r
                {
                    *r = true;
                    stack.push(n);
                }
            }
        }
    };
    out.sort_unstable();
    out.dedup();
    // A standing spawn is an obstacle: its neighbors start the reach.
    for from in std::iter::once(start).chain(start.neighbors().map(|(_, n)| n)) {
        flood(from, &out, &mut reached, &mut served, &mut stack);
    }
    loop {
        let mut joined = false;
        for k in 1..=regions {
            if served.get(usize::from(k)).copied().unwrap_or(true) {
                continue;
            }
            let reach = |t: Tile| {
                t.neighbors()
                    .any(|(_, n)| reached.get(n.index()).copied().unwrap_or(false))
            };
            let pick = cut
                .iter()
                .copied()
                .filter(|&t| out.binary_search(&t).is_err() && borders(t, k) && reach(t))
                .min_by_key(|&t| rank(t));
            if let Some(t) = pick {
                if let Err(at) = out.binary_search(&t) {
                    out.insert(at, t);
                }
                flood(t, &out, &mut reached, &mut served, &mut stack);
                joined = true;
            }
        }
        if !joined {
            break;
        }
    }
    out
}

/// Places `ramparts` on the board at their [`levels`], the line from `level` (or the first level
/// ramparts are allowed): ramparts on the [`gates`] and over the protected structures, walls on the
/// rest of the cut (ADR-033). A rampart already planned on a tile is moved down to its level and
/// stays a rampart. Returns how many ramparts and walls it placed.
pub fn apply(
    board: &mut Board,
    input: &PlanInput<'_>,
    ramparts: &Ramparts,
    level: u8,
) -> Result<u32, PlanError> {
    let placed_at = levels(input, ramparts, level, |t| board.level_of(t));
    let gates = gates(board, input, ramparts);
    let mut placed = 0;
    for (t, at) in placed_at {
        if board.has_rampart(t) {
            board.lower(StructureType::Rampart, t, at);
            continue;
        }
        let wall = ramparts.cut.contains(&t) && gates.binary_search(&t).is_err();
        let kind = if wall {
            StructureType::Wall
        } else {
            StructureType::Rampart
        };
        if !board.place_at_level(kind, t, at) {
            return Err(PlanError::NoRoom(if wall {
                "wall tile"
            } else {
                "rampart tile"
            }));
        }
        placed += 1;
    }
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::TerrainGrid;
    use rustedscreep_consts::{ROOM_AREA, ROOM_SIZE, Terrain};

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

    /// A constructed wall is the barrier itself (V52, V244), not a structure behind it: guarding
    /// it would put a rampart over every wall of a line.
    #[test]
    fn walls_are_not_protected() {
        let t = terrain(&[(10, 10), (40, 40)]);
        let sources = [tile(40, 40)];
        let input = PlanInput {
            terrain: &t,
            controller: tile(10, 10),
            sources: &sources,
            mineral: None,
            spawn: None,
        };
        let mut b = Board::new(&input);
        assert!(b.place(StructureType::Spawn, tile(25, 25)).is_some());
        assert!(b.place(StructureType::Wall, tile(20, 20)).is_some());
        assert_eq!(protected_tiles(&b), vec![tile(25, 25)]);
    }

    /// A rampart the board already holds on a tile of the plan moves down to the level `levels`
    /// gives it (never below the first level ramparts are allowed), and one already lower stays.
    #[test]
    fn apply_moves_a_standing_rampart_down_to_its_level() {
        let t = terrain(&[(10, 10), (40, 40)]);
        let sources = [tile(40, 40)];
        let input = PlanInput {
            terrain: &t,
            controller: tile(10, 10),
            sources: &sources,
            mineral: None,
            spawn: None,
        };
        let first =
            crate::planner::rules::earliest_level(StructureType::Rampart, 0).expect("ramparts");
        let level_at = |b: &Board, at: Tile| {
            b.items()
                .iter()
                .find(|i| i.kind == StructureType::Rampart && i.tile == at)
                .map(|i| i.level)
        };
        let (late, early) = (tile(30, 30), tile(32, 30));
        let mut b = Board::new(&input);
        assert!(b.place_at_level(StructureType::Rampart, late, first + 4));
        assert!(b.place_at_level(StructureType::Rampart, early, first + 1));
        let line = Ramparts {
            cut: vec![late, early],
            own: Vec::new(),
        };
        apply(&mut b, &input, &line, first + 2).expect("placed");
        assert_eq!(level_at(&b, late), Some(first + 2));
        assert_eq!(level_at(&b, early), Some(first + 1));
        apply(&mut b, &input, &line, 0).expect("placed");
        assert_eq!(level_at(&b, late), Some(first));
        assert_eq!(b.count(StructureType::Rampart), 2);
    }

    /// A room split by a terrain wall at x = 15, open only at y 20 to 24, its east part closed to the
    /// exits, with a spawn, a tower at (20, 30) and six extensions in it: more protected structures
    /// than the gap is wide, so the cut closes the gap. The terrain, the board and the tower.
    fn ringed() -> (TerrainGrid, Board, Tile) {
        let mut walls: Vec<(u8, u8)> = (0..ROOM_SIZE)
            .filter(|y| !(20..=24).contains(y))
            .map(|y| (15, y))
            .collect();
        for i in 15..ROOM_SIZE {
            walls.extend([(i, 0), (i, ROOM_SIZE - 1)]);
        }
        walls.extend((0..ROOM_SIZE).map(|y| (ROOM_SIZE - 1, y)));
        let t = terrain(&walls);
        let tower = tile(20, 30);
        let input = input_for(&t);
        let mut b = Board::new(&input);
        assert!(b.place(StructureType::Spawn, tile(30, 25)).is_some());
        assert!(b.place(StructureType::Tower, tower).is_some());
        for y in [10, 12, 14, 16, 18, 20] {
            assert!(b.place(StructureType::Extension, tile(35, y)).is_some());
        }
        (t, b, tower)
    }

    fn input_for(t: &TerrainGrid) -> PlanInput<'_> {
        const SOURCES: [Tile; 1] = [match Tile::new(5, 5) {
            Some(t) => t,
            None => panic!("in room"),
        }];
        PlanInput {
            terrain: t,
            controller: tile(45, 45),
            sources: &SOURCES,
            mineral: None,
            spawn: Some(tile(30, 25)),
        }
    }

    /// ADR-033: with no road across the line, the cut around a spawn and a tower in an open room
    /// keeps one rampart, the cut tile nearest the tower (the one region outside is joined to the
    /// base through it), and every other cut tile takes a wall at the line's level; the walls and
    /// the gate together still close the line (no tile outside reaches a protected one).
    #[test]
    fn the_line_is_walls_but_one_gate_nearest_the_tower() {
        let (t, mut b, tower) = ringed();
        let input = input_for(&t);
        let mut s = Scratch::default();
        let line = plan(&input, &protected_tiles(&b), &mut s).expect("bounded");
        assert!(line.cut.len() > 4, "{line:?}");
        let gates = gates(&b, &input, &line);
        let nearest = line
            .cut
            .iter()
            .map(|&c| c.range(tower))
            .min()
            .expect("a cut");
        assert_eq!(gates.len(), 1, "{gates:?}");
        assert_eq!(gates.iter().map(|&g| g.range(tower)).min(), Some(nearest));
        let first =
            crate::planner::rules::earliest_level(StructureType::Rampart, 0).expect("ramparts");
        apply(&mut b, &input, &line, first).expect("placed");
        let of = |k: StructureType| -> Vec<Tile> {
            let mut v: Vec<Tile> = b
                .items()
                .iter()
                .filter(|i| i.kind == k)
                .map(|i| i.tile)
                .collect();
            v.sort();
            v
        };
        assert_eq!(of(StructureType::Rampart), gates);
        assert_eq!(of(StructureType::Wall).len(), line.cut.len() - 1);
        assert!(
            b.items()
                .iter()
                .filter(|i| i.kind == StructureType::Wall)
                .all(|i| i.level == first)
        );
    }

    /// ADR-033: a road across the line is its gate (own creeps walk it), and the region it opens
    /// needs no other, so every other cut tile takes a wall.
    #[test]
    fn a_road_across_the_line_is_its_gate() {
        let (t, mut b, tower) = ringed();
        let input = input_for(&t);
        let mut s = Scratch::default();
        let line = plan(&input, &protected_tiles(&b), &mut s).expect("bounded");
        // The cut tile farthest from the tower, so the tower rule alone would not pick it.
        let far = line
            .cut
            .iter()
            .copied()
            .max_by_key(|&c| (c.range(tower), c.index()))
            .expect("a cut");
        assert!(b.place_road(far, 1, crate::planner::layout::Gate::Base));
        assert_eq!(gates(&b, &input, &line), vec![far]);
        let first =
            crate::planner::rules::earliest_level(StructureType::Rampart, 0).expect("ramparts");
        apply(&mut b, &input, &line, first).expect("placed");
        assert_eq!(b.count(StructureType::Rampart), 1);
        assert_eq!(b.count(StructureType::Wall) as usize, line.cut.len() - 1);
        assert!(b.has_rampart(far) && b.has_road(far));
    }

    /// `plan` looks the protected tiles up by binary search: the caller's order must not matter.
    #[test]
    fn the_cut_does_not_depend_on_the_order_of_protect() {
        let t = terrain(&[(10, 10), (40, 40)]);
        let sources = [tile(40, 40)];
        let input = PlanInput {
            terrain: &t,
            controller: tile(10, 10),
            sources: &sources,
            mineral: None,
            spawn: None,
        };
        let mut s = Scratch::default();
        let up = vec![tile(20, 20), tile(25, 25), tile(30, 30)];
        let down: Vec<Tile> = up.iter().rev().copied().collect();
        let a = plan(&input, &up, &mut s).expect("bounded");
        let b = plan(&input, &down, &mut s).expect("bounded");
        assert_eq!(a, b);
        assert_eq!(a.own.len() + a.cut.len(), a.tiles().len());
    }
}
