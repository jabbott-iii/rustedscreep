//! The tiles the economy works from, chosen the same way for every candidate so they compare on
//! their base alone: the ground kept clear around the sources, the mineral and the controller, the
//! mining tiles and their containers, the controller's container and link, the source links, the
//! hub link (on the tile the candidate's core gives it), the extractor and the mineral container.
//!
//! The rules follow `economy::mining`, which reads these structures by position: a mining tile is a
//! walkable neighbour of its source, off an earlier source's ground where it can be (the sources
//! take their tiles in (x, y) order), then off a spawn's last free neighbour that is no post (a
//! spawn must keep one, V174; counting as posts the tiles of sources with only one) and off a
//! later source's ground (which may be all that source has);
//! the controller container and link stand at `CONTROLLER_LINK_RANGE`, so every tile next to them
//! is in upgrade range (V187); a source link stands next to its mining tile and out of
//! `RECEIVER_RANGE`, or it would read as a controller link, and there is none where the controller
//! link is next to the mining tile (that link takes the miner's energy); no link takes the hub or
//! the hub link's tile; a link never closes a passage (`keeps_neighbours_connected`) or takes a
//! spawn's last free neighbour that is no post (V174).
//!
//! Links are placed in a fixed order, so their levels (V234) give the build order ADR-024 uses:
//! the controller link, the source farthest from the hub, the hub link, then the other sources.

use rustedscreep_consts::{ROOM_AREA, StructureType};

use super::board::Board;
use super::layout::{PlanError, PlanInput};
use super::scratch::{Scratch, fatigue};
use crate::economy::placement::{CONTROLLER_CLEAR, keeps_neighbours_connected};
use crate::economy::{CONTROLLER_LINK_RANGE, RECEIVER_RANGE};
use crate::movement::{Goal, Tile, UNREACHED};

fn tiles() -> impl Iterator<Item = Tile> {
    (0..ROOM_AREA).filter_map(Tile::from_index)
}

/// Keeps the ground the economy works from free of obstacles: next to each source and the mineral
/// (miners and haulers stand there) and within `CONTROLLER_CLEAR` of the controller (upgraders).
/// Candidates call it before they place anything.
pub fn reserve(board: &mut Board<'_>, input: &PlanInput<'_>) {
    for t in tiles() {
        let near = input
            .sources
            .iter()
            .chain(input.mineral.iter())
            .any(|s| s.range(t) <= 1);
        if near || t.range(input.controller) <= CONTROLLER_CLEAR {
            board.keep_clear(t);
        }
    }
}

/// The tiles some source of `sources` can be mined from alone (its only walkable neighbour that
/// takes a container, `except` left out as if an obstacle stood there): its mining tile, whatever
/// the plan, so a post (V174) from the start.
pub fn forced_tiles(board: &Board<'_>, sources: &[Tile], except: Option<Tile>) -> Vec<Tile> {
    sources
        .iter()
        .filter_map(|&x| {
            let mut tiles = x.neighbors().map(|(_, n)| n).filter(|&n| {
                Some(n) != except
                    && board.walkable(n)
                    && board.can_place(StructureType::Container, n)
            });
            match (tiles.next(), tiles.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            }
        })
        .collect()
}

/// Fills `s.field` with path costs from `hub` over the board as planned so far.
fn measure(board: &Board<'_>, hub: Tile, s: &mut Scratch) {
    board.cost_grid(&mut s.grid, fatigue());
    s.field.compute(
        &s.grid,
        Goal {
            tile: hub,
            range: 0,
        },
        &mut s.dial,
    );
}

fn cost(s: &Scratch, t: Tile) -> Option<u16> {
    let d = s.field.distance(t);
    (d != UNREACHED).then_some(d)
}

/// The mining tiles with their containers, and the controller container. Call once the core (and
/// its hub) is placed.
pub fn containers(
    board: &mut Board<'_>,
    input: &PlanInput<'_>,
    hub: Tile,
    s: &mut Scratch,
) -> Result<(), PlanError> {
    measure(board, hub, s);
    // A source with one tile it can mine from will take it: treat those as posts already, so an
    // earlier source does not leave a spawn beside one with no free neighbour (V174).
    let forced = forced_tiles(board, input.sources, None);
    for &source in input.sources {
        // `economy::mining` gives the sources their tiles in (x, y) order, the tile order, each
        // the neighbour with a container: one next to an earlier source could be read as that
        // source's, so such tiles come last.
        let shared = |n: Tile| input.sources.iter().any(|&o| o < source && o.range(n) <= 1);
        let later = |n: Tile| input.sources.iter().any(|&o| o > source && o.range(n) <= 1);
        let last_exit =
            |n: Tile| !board.leaves_spawn_exit(n, |w| !board.walkable(w) || forced.contains(&w));
        let best = source
            .neighbors()
            .map(|(_, n)| n)
            .filter(|&n| {
                board.walkable(n)
                    && board.can_place(StructureType::Container, n)
                    && !board.roles.mining.contains(&n)
            })
            .filter_map(|n| {
                cost(s, n).map(|d| ((shared(n), last_exit(n), later(n)), d, n.index(), n))
            })
            .min()
            .map(|(_, _, _, n)| n)
            .ok_or(PlanError::NoRoom("mining tile"))?;
        board
            .place(StructureType::Container, best)
            .ok_or(PlanError::NoRoom("mining container"))?;
        board.roles.mining.push(best);
        board.roles.source_links.push(None);
    }
    let natural_near = |t: Tile| {
        input
            .sources
            .iter()
            .chain(input.mineral.iter())
            .any(|s| s.range(t) <= 1)
    };
    let pick = |range: u8, board: &Board<'_>| {
        tiles()
            .filter(|&t| {
                t.range(input.controller) == range
                    && board.walkable(t)
                    && board.can_place(StructureType::Container, t)
                    && !natural_near(t)
                    && !board.roles.mining.contains(&t)
            })
            .filter_map(|t| cost(s, t).map(|d| (d, t.index(), t)))
            .min()
            .map(|(_, _, t)| t)
    };
    let chosen = std::iter::once(CONTROLLER_LINK_RANGE)
        .chain((1..=RECEIVER_RANGE).filter(|&r| r != CONTROLLER_LINK_RANGE))
        .find_map(|r| pick(r, board))
        .ok_or(PlanError::NoRoom("controller container"))?;
    board
        .place(StructureType::Container, chosen)
        .ok_or(PlanError::NoRoom("controller container"))?;
    board.roles.controller_container = Some(chosen);
    Ok(())
}

/// Whether a link on `t` keeps the ground around it one passage, leaves every spawn beside it a
/// free neighbour that is no post (V174), and stays out of role tiles.
fn link_fits(board: &Board<'_>, t: Tile) -> bool {
    board.open(t)
        && board.can_place_role(StructureType::Link, t)
        && !board.roles.mining.contains(&t)
        && board.roles.controller_container != Some(t)
        && keeps_neighbours_connected(t, |n| !board.walkable(n) || n == t)
        && board.leaves_spawn_exit(t, |n| !board.walkable(n))
}

/// Upgrade posts with feeds at `feeds`: walkable tiles within `RECEIVER_RANGE` of the controller
/// next to one of them.
fn posts(board: &Board<'_>, controller: Tile, feeds: &[Tile]) -> usize {
    tiles()
        .filter(|&t| {
            t.range(controller) <= RECEIVER_RANGE
                && board.walkable(t)
                && !feeds.contains(&t)
                && feeds.iter().any(|f| f.range(t) <= 1)
        })
        .count()
}

/// Every link, in the build order of the module docs. `hub_link` is the tile the core keeps for
/// it next to the storage and the hub.
pub fn links(
    board: &mut Board<'_>,
    input: &PlanInput<'_>,
    hub: Tile,
    hub_link: Tile,
    s: &mut Scratch,
) -> Result<(), PlanError> {
    measure(board, hub, s);
    // The hub and the hub link's tile stay the core's: the hub link is placed after the first
    // source link.
    let fits = |board: &Board<'_>, t: Tile| t != hub && t != hub_link && link_fits(board, t);
    // The controller link: the most upgrade posts with the container, then the nearest the hub.
    let container = board.roles.controller_container;
    let controller_link = tiles()
        .filter(|&t| t.range(input.controller) == CONTROLLER_LINK_RANGE && fits(board, t))
        .filter_map(|t| {
            let feeds: Vec<Tile> = container.into_iter().chain([t]).collect();
            let d = cost(s, t)?;
            Some((
                usize::MAX - posts(board, input.controller, &feeds),
                d,
                t.index(),
                t,
            ))
        })
        .min()
        .map(|(_, _, _, t)| t)
        .ok_or(PlanError::NoRoom("controller link"))?;
    board
        .place_role(StructureType::Link, controller_link)
        .ok_or(PlanError::NoRoom("controller link"))?;
    board.roles.controller_link = Some(controller_link);

    // Sources farthest from the hub first (by their mining tile's cost).
    let mut order: Vec<(u16, usize)> = board
        .roles
        .mining
        .iter()
        .enumerate()
        .map(|(i, &m)| (cost(s, m).unwrap_or(UNREACHED), i))
        .collect();
    order.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut placed_hub = false;
    for (n, &(_, i)) in order.iter().enumerate() {
        if n == 1 {
            board
                .place_role(StructureType::Link, hub_link)
                .ok_or(PlanError::NoRoom("hub link"))?;
            board.roles.hub_link = Some(hub_link);
            placed_hub = true;
        }
        let (Some(&mining), Some(&source)) = (board.roles.mining.get(i), input.sources.get(i))
        else {
            continue;
        };
        // A controller link next to the mining tile takes the miner's energy itself; a source link
        // there would be a second link next to it, which the economy could read as the source's.
        if controller_link.range(mining) <= 1 {
            continue;
        }
        let others: Vec<Tile> = board
            .roles
            .mining
            .iter()
            .copied()
            .filter(|&m| m != mining)
            .collect();
        let link = mining
            .neighbors()
            .map(|(_, t)| t)
            .filter(|&t| {
                fits(board, t)
                    && t.range(input.controller) > RECEIVER_RANGE
                    && !others.iter().any(|o| o.range(t) <= 1)
            })
            .min_by_key(|&t| (t.range(source) <= 1, t.index()));
        if let Some(t) = link
            && board.place_role(StructureType::Link, t).is_some()
            && let Some(slot) = board.roles.source_links.get_mut(i)
        {
            *slot = Some(t);
        }
    }
    if !placed_hub {
        board
            .place_role(StructureType::Link, hub_link)
            .ok_or(PlanError::NoRoom("hub link"))?;
        board.roles.hub_link = Some(hub_link);
    }
    Ok(())
}

/// The extractor on the mineral and its container next to it, both from the extractor's level.
pub fn mineral(
    board: &mut Board<'_>,
    input: &PlanInput<'_>,
    hub: Tile,
    s: &mut Scratch,
) -> Result<(), PlanError> {
    let Some(m) = input.mineral else {
        return Ok(());
    };
    let level = board
        .place(StructureType::Extractor, m)
        .ok_or(PlanError::NoRoom("extractor"))?;
    measure(board, hub, s);
    let t = m
        .neighbors()
        .map(|(_, n)| n)
        .filter(|&n| board.walkable(n) && board.can_place(StructureType::Container, n))
        .filter_map(|n| cost(s, n).map(|d| (d, n.index(), n)))
        .min()
        .map(|(_, _, n)| n)
        .ok_or(PlanError::NoRoom("mineral container"))?;
    if !board.place_at_level(StructureType::Container, t, level) {
        return Err(PlanError::NoRoom("mineral container"));
    }
    board.roles.mineral_container = Some(t);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::TerrainGrid;
    use crate::planner::validate::{Problem, validate};
    use rustedscreep_consts::{ROOM_SIZE, Terrain};

    fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).expect("in room")
    }

    fn terrain(walls: &[Tile]) -> TerrainGrid {
        let mut raw = vec![0u8; ROOM_AREA];
        for t in walls {
            raw[usize::from(t.y()) * usize::from(ROOM_SIZE) + usize::from(t.x())] =
                Terrain::Wall as u8;
        }
        TerrainGrid::from_row_major(&raw).expect("length")
    }

    /// `economy::mining` gives each source, in (x, y) order, its neighbour with a container: a
    /// later source's mining tile next to an earlier source could be read as the earlier one's.
    #[test]
    fn a_mining_tile_is_not_next_to_an_earlier_source() {
        let sources = [tile(20, 20), tile(22, 20)];
        let controller = tile(40, 40);
        let t = terrain(&[sources[0], sources[1], controller]);
        let input = PlanInput {
            terrain: &t,
            controller,
            sources: &sources,
            mineral: None,
            spawn: None,
        };
        let mut b = Board::new(&input);
        reserve(&mut b, &input);
        let mut s = Scratch::default();
        containers(&mut b, &input, tile(10, 20), &mut s).expect("containers");
        let m = b.roles.mining.clone();
        assert_eq!(m.len(), 2);
        assert!(m[1].range(sources[0]) > 1, "{m:?}");
    }

    /// A source link is picked before the hub link (the module docs' order), so it must not take
    /// the tile the core keeps for the hub link: here the lowest free neighbour of the mining tile.
    #[test]
    fn a_source_link_leaves_the_hub_link_tile() {
        let (controller, sources) = (tile(10, 10), [tile(30, 30)]);
        let t = terrain(&[sources[0], controller]);
        let input = PlanInput {
            terrain: &t,
            controller,
            sources: &sources,
            mineral: None,
            spawn: None,
        };
        let mut b = Board::new(&input);
        reserve(&mut b, &input);
        let (hub, hub_link, storage) = (tile(27, 27), tile(28, 28), tile(26, 27));
        assert!(b.place(StructureType::Spawn, tile(27, 26)).is_some());
        assert!(b.place(StructureType::Storage, storage).is_some());
        b.roles.hub = Some(hub);
        b.roles.storage = Some(storage);
        let mut s = Scratch::default();
        containers(&mut b, &input, hub, &mut s).expect("containers");
        assert_eq!(b.roles.mining, vec![tile(29, 29)]);
        links(&mut b, &input, hub, hub_link, &mut s).expect("links");
        assert_eq!(b.roles.hub_link, Some(hub_link));
        assert_ne!(b.roles.source_links, vec![Some(hub_link)]);
    }

    /// Terrain that leaves the controller two tiles at `CONTROLLER_LINK_RANGE`, one of them next
    /// to the mining tile: the controller link there takes the miner's energy itself, so a source
    /// link would be a second link next to the mining tile, which the economy would misread.
    #[test]
    fn no_source_link_beside_the_controller_link() {
        let (controller, sources) = (tile(20, 20), [tile(24, 20)]);
        let open = [(22, 20), (22, 21), (23, 19), (23, 20), (23, 21), (23, 22)];
        let mut walls: Vec<Tile> = (17..=23u8)
            .flat_map(|x| (17..=23u8).map(move |y| (x, y)))
            .filter(|t| !open.contains(t))
            .map(|(x, y)| tile(x, y))
            .collect();
        walls.push(sources[0]);
        let t = terrain(&walls);
        let input = PlanInput {
            terrain: &t,
            controller,
            sources: &sources,
            mineral: None,
            spawn: None,
        };
        let mut b = Board::new(&input);
        reserve(&mut b, &input);
        let hub = tile(20, 30);
        let mut s = Scratch::default();
        containers(&mut b, &input, hub, &mut s).expect("containers");
        links(&mut b, &input, hub, tile(21, 30), &mut s).expect("links");
        let (mining, link) = (
            b.roles.mining.clone(),
            b.roles.controller_link.expect("controller link"),
        );
        assert_eq!(mining, vec![tile(23, 21)]);
        assert!(link.range(tile(23, 21)) <= 1, "{link:?}");
        assert_eq!(b.roles.source_links, vec![None]);
        let layout = b.into_layout();
        let p = validate(&input, &layout).problems;
        assert!(
            !p.contains(&Problem::Role(
                "a link next to a mining tile is not its source link"
            )),
            "{p:?}"
        );
    }
}
