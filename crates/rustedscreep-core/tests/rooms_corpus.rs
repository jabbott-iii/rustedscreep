//! The terrain corpus is valid input for the base planner (ROADMAP P4.1). The rooms come from the
//! official MMO, whose map code is closed, so each property the open-source generator gives (V227 to
//! V229) is checked on the data here rather than assumed. The border test also pins the digit order:
//! neighbouring rooms' edges match in row-major order (V224, V226) and not when read transposed.

mod common;

use std::collections::BTreeMap;

use common::corpus::{self, CorpusRoom};
use rustedscreep_consts::{ROOM_AREA, ROOM_SIZE, Terrain};
use rustedscreep_core::movement::{TerrainKind, Tile};

/// ROADMAP P4.1: at least 30 rooms.
const MIN_ROOMS: usize = 30;

fn wall(mask: u8) -> bool {
    mask & Terrain::Wall as u8 != 0
}

/// (x, y), for messages.
fn xy(t: Tile) -> (u8, u8) {
    (t.x(), t.y())
}

fn tiles() -> impl Iterator<Item = Tile> {
    (0..ROOM_AREA).filter_map(Tile::from_index)
}

/// Walkable inside the room: not a wall and not an edge tile (a creep that ends a tick on an edge
/// changes rooms, V151). Natural objects stand on walls (V228), so they are excluded too.
fn open_inner(room: &CorpusRoom, t: Tile) -> bool {
    !t.is_edge() && !wall(room.mask(t))
}

#[test]
fn the_corpus_has_enough_distinct_rooms() {
    let rooms = corpus::load();
    assert!(
        rooms.len() >= MIN_ROOMS,
        "{} rooms, want at least {MIN_ROOMS}",
        rooms.len()
    );
    let mut seen = BTreeMap::new();
    for r in &rooms {
        let key = (r.shard.clone(), r.name.packed_repr());
        assert!(
            seen.insert(key, ()).is_none(),
            "{} {} twice",
            r.shard,
            r.name
        );
    }
}

/// Uses: ROOM_AREA, the `Terrain` discriminants (TERRAIN_MASK_WALL, TERRAIN_MASK_SWAMP). Every byte
/// is a combination of the two masks, and the grid reads it with wall precedence (V86, V156).
#[test]
fn terrain_bytes_are_the_engine_masks() {
    let (w, s) = (Terrain::Wall as u8, Terrain::Swamp as u8);
    for r in corpus::load() {
        assert_eq!(r.raw.len(), ROOM_AREA, "{}", r.name);
        let grid = r.terrain();
        for t in tiles() {
            let m = r.mask(t);
            assert_eq!(m & !(w | s), 0, "{} {:?}: mask {m}", r.name, xy(t));
            let want = if m & w != 0 {
                TerrainKind::Wall
            } else if m & s != 0 {
                TerrainKind::Swamp
            } else {
                TerrainKind::Plain
            };
            assert_eq!(grid.kind(t), want, "{} {:?}", r.name, xy(t));
        }
    }
}

/// V228: the generator puts the controller, each source and the mineral on a wall tile with open
/// ground next to it, away from the edge. V227: one or two sources and at most one mineral per room
/// with a controller (the generator's counts, cited as a pinned example, not constants).
#[test]
fn natural_objects_stand_on_walls_next_to_open_ground() {
    for r in corpus::load() {
        assert!(
            (1..=2).contains(&r.sources.len()),
            "{}: {} sources",
            r.name,
            r.sources.len()
        );
        assert!(
            r.minerals.len() <= 1,
            "{}: {} minerals",
            r.name,
            r.minerals.len()
        );
        let objects: Vec<Tile> = r.objects().collect();
        for (i, &t) in objects.iter().enumerate() {
            assert!(!t.is_edge(), "{} {:?}: on the edge", r.name, xy(t));
            assert!(wall(r.mask(t)), "{} {:?}: not on a wall", r.name, xy(t));
            assert!(
                !objects[..i].contains(&t),
                "{} {:?}: two objects on one tile",
                r.name,
                xy(t)
            );
            assert!(
                t.neighbors().any(|(_, n)| open_inner(&r, n)),
                "{} {:?}: no open neighbour",
                r.name,
                xy(t)
            );
        }
    }
}

/// V229: the generator retries until the open ground is one area. Checked here inside the room (edges
/// excluded): every open inner tile is reached from the controller's side, so every object and at
/// least one exit tile can be reached from any open tile.
#[test]
fn open_ground_is_one_area_that_reaches_every_object_and_an_exit() {
    for r in corpus::load() {
        let start = r
            .controller
            .neighbors()
            .map(|(_, n)| n)
            .find(|&n| open_inner(&r, n))
            .unwrap_or_else(|| panic!("{}: the controller has no open neighbour", r.name));
        let mut reached = vec![false; ROOM_AREA];
        let mut queue = vec![start];
        reached[start.index()] = true;
        while let Some(t) = queue.pop() {
            for (_, n) in t.neighbors() {
                if open_inner(&r, n) && !reached[n.index()] {
                    reached[n.index()] = true;
                    queue.push(n);
                }
            }
        }
        let cut_off: Vec<Tile> = tiles()
            .filter(|&t| open_inner(&r, t) && !reached[t.index()])
            .collect();
        assert!(
            cut_off.is_empty(),
            "{}: {} open tiles cut off, first {:?}",
            r.name,
            cut_off.len(),
            cut_off.first().copied().map(xy)
        );
        for t in r.objects() {
            assert!(
                t.neighbors().any(|(_, n)| reached[n.index()]),
                "{} {:?}: not reached",
                r.name,
                xy(t)
            );
        }
        let exit = tiles().any(|t| {
            t.side().is_some() && !wall(r.mask(t)) && t.neighbors().any(|(_, n)| reached[n.index()])
        });
        assert!(exit, "{}: no exit reached", r.name);
    }
}

/// An edge tile and the tile a creep lands on in the next room (V151, `Tile::across`) are both walls
/// or both open, for every pair of neighbouring rooms in the corpus. Read transposed (`x * 50 + y`)
/// the same edges disagree, so this test fixes the digit order (V224, V226).
///
/// Uses: ROOM_SIZE.
#[test]
fn borders_match_the_neighbouring_rooms() {
    let rooms = corpus::load();
    let index: BTreeMap<(&str, u16), &CorpusRoom> = rooms
        .iter()
        .map(|r| ((r.shard.as_str(), r.name.packed_repr()), r))
        .collect();
    let size = usize::from(ROOM_SIZE);
    let transposed =
        |r: &CorpusRoom, t: Tile| r.raw[usize::from(t.x()) * size + usize::from(t.y())];
    let (mut compared, mut transposed_mismatches) = (0usize, 0usize);
    for r in &rooms {
        for t in tiles() {
            let (Some(side), Some(landing)) = (t.side(), t.across()) else {
                continue;
            };
            let Some(neighbour) = r.name.checked_add(side.room_offset()) else {
                continue;
            };
            let Some(&other) = index.get(&(r.shard.as_str(), neighbour.packed_repr())) else {
                continue;
            };
            assert_eq!(
                wall(r.mask(t)),
                wall(other.mask(landing)),
                "{} {:?} against {} {:?}",
                r.name,
                xy(t),
                other.name,
                xy(landing)
            );
            compared += 1;
            if wall(transposed(r, t)) != wall(transposed(other, landing)) {
                transposed_mismatches += 1;
            }
        }
    }
    // Each shared border is compared from both sides, ROOM_SIZE - 2 tiles each (corners have no side).
    assert!(
        compared > 0 && compared % (2 * (size - 2)) == 0,
        "{compared} edge tiles compared"
    );
    assert!(
        transposed_mismatches > 0,
        "the test cannot tell the digit order apart"
    );
}
