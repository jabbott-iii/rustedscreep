//! The game's rules a plan must keep, in one place: where a site of each type may stand (V237), what
//! creeps can walk through (V235), what the ramparts must cover (V243) and how many of each type a
//! level allows (V234).

use rustedscreep_consts::{CREEP_RANGED_ACTION_RANGE, ROOM_SIZE, StructureType};

use super::layout::MAX_LEVEL;
use crate::movement::{TerrainGrid, TerrainKind, Tile};

/// The range within which a hostile creep outside the ramparts hits a structure with a ranged
/// attack (V243): a structure not under a rampart must be farther than this from every tile such a
/// creep can stand on.
pub const RANGED_REACH: u8 = CREEP_RANGED_ACTION_RANGE;

/// How far a creep repairs and builds from: within 3 on each axis, the range of every ranged
/// creep action (`repair.js` L19, V222; `Creep.build`'s `inRangeTo(target, 3)`, V185). A rampart
/// or wall no own creep can stand this close to is never built or kept, and a new rampart is gone
/// at its first decay (V241).
pub const REPAIR_REACH: u8 = CREEP_RANGED_ACTION_RANGE;

/// The room's edge tiles, each once: the exits where they are not walls.
pub fn edge_tiles() -> impl Iterator<Item = Tile> + Clone {
    let last = ROOM_SIZE - 1;
    let rows = (0..ROOM_SIZE).flat_map(move |x| [(x, 0), (x, last)]);
    let columns = (1..last).flat_map(move |y| [(0, y), (last, y)]);
    rows.chain(columns).filter_map(|(x, y)| Tile::new(x, y))
}

/// Whether `level` allows `kind` at all.
pub fn allowed(kind: StructureType, level: u8) -> u32 {
    kind.controller_structures(u32::from(level))
}

/// The lowest level at which an `n`th (0-based) structure of `kind` is allowed, or `None` when
/// even the top level allows no more than `n`. Uses: `CONTROLLER_STRUCTURES` (V234).
pub fn earliest_level(kind: StructureType, n: u32) -> Option<u8> {
    (1..=MAX_LEVEL).find(|&l| allowed(kind, l) > n)
}

/// The types a full plan holds exactly as many of as each level allows: placing fewer leaves
/// capacity, defence or production unused. Links, containers, roads, ramparts and walls are
/// placed as the layout needs, up to the limit. The extractor counts only in a room with a
/// mineral.
pub const REQUIRED: [StructureType; 11] = [
    StructureType::Spawn,
    StructureType::Extension,
    StructureType::Storage,
    StructureType::Tower,
    StructureType::Terminal,
    StructureType::Extractor,
    StructureType::Lab,
    StructureType::Factory,
    StructureType::PowerSpawn,
    StructureType::Nuker,
    StructureType::Observer,
];

/// Whether creeps can stand on a built `kind` (V235): roads, containers, own ramparts and the
/// extractor (on its mineral, which blocks anyway) do not block.
pub fn blocks(kind: StructureType) -> bool {
    kind.is_obstacle()
}

/// Whether a built `kind` on `t` must be out of ranged reach of hostile creeps (V243): every
/// obstacle the base keeps, except constructed walls (a wall is part of the barrier hostile creeps
/// must break, V52, V244, not something behind it) and the links that work at the sources and the
/// controller (the plan names those, and the caller passes `remote`).
pub fn protected(kind: StructureType, remote: bool) -> bool {
    blocks(kind) && kind != StructureType::Wall && !remote
}

/// The border tiles a site off road and container at x or y of 1 or 48 needs to be natural
/// walls (V237): the checks run in the order x 1, x 48, y 1, y 48, and a later one replaces an
/// earlier one, so at a corner the y rule holds. `None` when no rule applies.
fn border(t: Tile) -> Option<[(u8, u8); 3]> {
    let (x, y, last) = (t.x(), t.y(), ROOM_SIZE - 1);
    let near = last - 1;
    let mut tiles = None;
    if x == 1 {
        tiles = Some([(0, y.wrapping_sub(1)), (0, y), (0, y + 1)]);
    }
    if x == near {
        tiles = Some([(last, y.wrapping_sub(1)), (last, y), (last, y + 1)]);
    }
    if y == 1 {
        tiles = Some([(x.wrapping_sub(1), 0), (x, 0), (x + 1, 0)]);
    }
    if y == near {
        tiles = Some([(x.wrapping_sub(1), last), (x, last), (x + 1, last)]);
    }
    tiles
}

/// Whether the terrain and the room's border let a site of `kind` stand on `t` (V237). The object
/// rules (one costed structure per tile, roads and ramparts over anything, the extractor on its
/// mineral) are the caller's: a plan checks them against its own items.
///
/// Uses: `ROOM_SIZE`, the `Terrain` wall mask through [`TerrainGrid::kind`].
pub fn tile_allows(terrain: &TerrainGrid, kind: StructureType, t: Tile) -> bool {
    if t.is_edge() {
        return false;
    }
    if kind != StructureType::Road
        && kind != StructureType::Container
        && let Some(tiles) = border(t)
    {
        let all_walls = tiles
            .iter()
            .all(|&(x, y)| Tile::new(x, y).is_some_and(|b| terrain.kind(b) == TerrainKind::Wall));
        if !all_walls {
            return false;
        }
    }
    match kind {
        StructureType::Extractor => true,
        StructureType::Road => true,
        _ => terrain.kind(t) != TerrainKind::Wall,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::TerrainGrid;
    use rustedscreep_consts::{ROOM_AREA, Terrain};

    #[test]
    fn edge_tiles_are_each_edge_tile_once() {
        let mut seen: Vec<Tile> = edge_tiles().collect();
        let n = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), n);
        let all: Vec<Tile> = (0..ROOM_AREA)
            .filter_map(Tile::from_index)
            .filter(|t| t.is_edge())
            .collect();
        assert_eq!(seen.len(), all.len());
        assert!(all.iter().all(|t| seen.binary_search(t).is_ok()));
    }

    fn grid(walls: &[(u8, u8)]) -> TerrainGrid {
        let mut raw = vec![0u8; ROOM_AREA];
        for &(x, y) in walls {
            raw[usize::from(y) * usize::from(ROOM_SIZE) + usize::from(x)] = Terrain::Wall as u8;
        }
        TerrainGrid::from_row_major(&raw).expect("length")
    }

    fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).expect("in room")
    }

    /// V237: the edge refuses everything; 1 and 48 refuse all but road and container unless the
    /// three border tiles are walls; only a road (and the extractor) may stand on a wall.
    #[test]
    fn sites_follow_the_border_and_terrain_rules() {
        let last = ROOM_SIZE - 1;
        let open = grid(&[]);
        for kind in [
            StructureType::Road,
            StructureType::Rampart,
            StructureType::Extension,
        ] {
            assert!(!tile_allows(&open, kind, tile(0, 10)));
            assert!(!tile_allows(&open, kind, tile(10, last)));
        }
        assert!(tile_allows(&open, StructureType::Road, tile(1, 10)));
        assert!(tile_allows(&open, StructureType::Container, tile(48, 10)));
        assert!(!tile_allows(&open, StructureType::Rampart, tile(1, 10)));
        assert!(!tile_allows(&open, StructureType::Extension, tile(10, 48)));
        assert!(tile_allows(&open, StructureType::Rampart, tile(2, 10)));
        // Walls on the three border tiles beside (1, 10) lift the rule there.
        let walled = grid(&[(0, 9), (0, 10), (0, 11)]);
        assert!(tile_allows(&walled, StructureType::Rampart, tile(1, 10)));
        assert!(!tile_allows(&walled, StructureType::Rampart, tile(1, 12)));
        // At the corner (1, 1) the y rule replaces the x rule: (0, 0), (1, 0), (2, 0) count.
        let top = grid(&[(0, 0), (1, 0), (2, 0)]);
        assert!(tile_allows(&top, StructureType::Tower, tile(1, 1)));
        let side = grid(&[(0, 0), (0, 1), (0, 2)]);
        assert!(!tile_allows(&side, StructureType::Tower, tile(1, 1)));
        // Terrain walls.
        let wall = grid(&[(20, 20)]);
        assert!(!tile_allows(&wall, StructureType::Extension, tile(20, 20)));
        assert!(!tile_allows(&wall, StructureType::Rampart, tile(20, 20)));
        assert!(tile_allows(&wall, StructureType::Road, tile(20, 20)));
        assert!(tile_allows(&wall, StructureType::Extractor, tile(20, 20)));
    }

    /// V234: each level's count of a type is the first level it reaches that count at.
    #[test]
    fn earliest_levels_follow_the_counts() {
        for kind in super::super::layout::KINDS {
            let mut n = 0;
            for level in 1..=MAX_LEVEL {
                while n < allowed(kind, level) {
                    assert_eq!(earliest_level(kind, n), Some(level), "{kind:?} {n}");
                    n += 1;
                }
            }
            assert_eq!(
                earliest_level(kind, allowed(kind, MAX_LEVEL)),
                None,
                "{kind:?}"
            );
        }
    }
}
