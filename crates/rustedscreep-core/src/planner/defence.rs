//! Ramparts by minimum cut: the fewest ramparts that keep every protected structure under a rampart
//! or out of ranged reach (V243) of every tile a hostile creep can stand on, coming from an exit.
//!
//! Roles for the cut ([`MinCut`]): walls and the natural objects are not in the graph; edge tiles are
//! sources (hostile creeps arrive there, V244); tiles no rampart may stand on (the border rule,
//! V237) are fixed; each protected structure is guarded at [`RANGED_REACH`]; every other tile can
//! take a rampart at cost 1. So the cut puts ramparts across the passages where that is cheapest and
//! over a structure itself where it is not (a structure beside an exit, for one).

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

/// Places `ramparts` on the board at their [`levels`], the line from `level` (or the first level
/// ramparts are allowed); a rampart already planned on a tile is moved down to its level.
pub fn apply(
    board: &mut Board,
    input: &PlanInput<'_>,
    ramparts: &Ramparts,
    level: u8,
) -> Result<u32, PlanError> {
    let placed_at = levels(input, ramparts, level, |t| board.level_of(t));
    let mut placed = 0;
    for (t, at) in placed_at {
        if board.has_rampart(t) {
            board.lower(StructureType::Rampart, t, at);
            continue;
        }
        if !board.place_at_level(StructureType::Rampart, t, at) {
            return Err(PlanError::NoRoom("rampart tile"));
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
