//! Hand-made rooms for edge cases the P4.2 reviews found (2026-11-18), each planned by both
//! candidates and judged by the validator: a standing spawn that is the one gap between two halves
//! with exits of their own; one with a walled alcove behind it (a pocket as given, which only the
//! validator may judge); and one whose two free neighbors both end up mining tiles unless the
//! earlier source leaves the later source's only tile to it. Last, the live room's standing
//! extensions against each candidate's plan (a measure for ADR-007, `-- --nocapture`).

mod common;

use rustedscreep_consts::{ROOM_AREA, ROOM_SIZE, StructureType, Terrain};
use rustedscreep_core::config::MOVEMENT_MMO;
use rustedscreep_core::economy::placement::next_extension;
use rustedscreep_core::movement::{self, CostGrid, Dial, FlowField, Footprint, TerrainGrid, Tile};
use rustedscreep_core::planner::rules::allowed;
use rustedscreep_core::planner::{
    Layout, PlanError, PlanInput, Problem, Scratch, bunker, flood, validate,
};

const N: u8 = ROOM_SIZE;

type Plan = fn(&PlanInput<'_>, &mut Scratch) -> Result<Layout, PlanError>;

const CANDIDATES: [(&str, Plan); 2] = [("bunker", bunker::plan), ("flood", flood::plan)];

fn tile(x: u8, y: u8) -> Tile {
    Tile::new(x, y).expect("in room")
}

fn edge(x: u8, y: u8) -> bool {
    x == 0 || y == 0 || x == N - 1 || y == N - 1
}

/// Terrain with walls where `wall` says.
fn terrain(wall: impl Fn(u8, u8) -> bool) -> TerrainGrid {
    let mut raw = vec![0u8; ROOM_AREA];
    for y in 0..N {
        for x in 0..N {
            if wall(x, y) {
                raw[usize::from(y) * usize::from(N) + usize::from(x)] = Terrain::Wall as u8;
            }
        }
    }
    TerrainGrid::from_row_major(&raw).expect("length")
}

/// Each candidate's plan for `input` and the validator's problems with it.
fn judged(input: &PlanInput<'_>) -> Vec<(&'static str, Result<Vec<Problem>, PlanError>)> {
    CANDIDATES
        .iter()
        .map(|&(name, plan)| {
            let result = plan(input, &mut Scratch::default()).map(|l| validate(input, &l).problems);
            (name, result)
        })
        .collect()
}

/// The standing spawn is the one gap in a wall line between a left half with exits on the left
/// side and a right half with exits on the right. The hub's side reaches only the left; the
/// validator reads the right half as joined to its own exits, so a valid plan exists. (The flood
/// candidate once kept the right side as one to reach and drained its fill.)
#[test]
fn a_standing_spawn_between_two_exit_halves_is_planned() {
    let naturals = [(10, 40), (15, 10), (12, 44), (8, 20)];
    let t = terrain(|x, y| {
        if (x == 0 || x == N - 1) && (10..=40).contains(&y) {
            return false;
        }
        naturals.contains(&(x, y)) || edge(x, y) || (x == 25 && y != 25)
    });
    let sources = [tile(10, 40), tile(15, 10)];
    for spawn in [tile(25, 25), tile(20, 25)] {
        let input = PlanInput {
            terrain: &t,
            controller: tile(12, 44),
            sources: &sources,
            mineral: Some(tile(8, 20)),
            spawn: Some(spawn),
        };
        for (name, result) in judged(&input) {
            assert_eq!(result, Ok(Vec::new()), "{name} {spawn:?}");
        }
    }
}

/// A standing spawn with a one-tile alcove behind it: a creep born there can never leave (V174),
/// whatever the plan. Both candidates plan the room and leave that one problem to the validator.
#[test]
fn a_standing_spawn_with_a_pocket_as_given_is_left_to_the_validator() {
    let (spawn, alcove) = (tile(20, 20), tile(20, 21));
    let walls: Vec<(u8, u8)> = alcove
        .neighbors()
        .map(|(_, n)| (n.x(), n.y()))
        .filter(|&n| n != (spawn.x(), spawn.y()))
        .collect();
    let naturals = [(10, 40), (40, 10), (30, 44), (44, 30)];
    let t = terrain(|x, y| {
        if x == 0 && (10..=40).contains(&y) {
            return false;
        }
        naturals.contains(&(x, y)) || edge(x, y) || walls.contains(&(x, y))
    });
    let sources = [tile(10, 40), tile(40, 10)];
    for (given, want) in [
        (
            spawn,
            vec![Problem::SpawnPocket {
                tile: (alcove.x(), alcove.y()),
                level: 1,
            }],
        ),
        (tile(20, 18), Vec::new()),
    ] {
        let input = PlanInput {
            terrain: &t,
            controller: tile(30, 44),
            sources: &sources,
            mineral: Some(tile(44, 30)),
            spawn: Some(given),
        };
        for (name, result) in judged(&input) {
            assert_eq!(result, Ok(want.clone()), "{name} {given:?}");
        }
    }
}

/// A standing spawn whose only free neighbors are a1, one of source A's two tiles, and b1, the
/// only tile of source B: if A takes a1 on cost, B must take b1 and the spawn has no free
/// neighbor left that is no post (SpawnBlocked, V174). A leaves b1's spawn its exit by taking
/// its other tile, wherever the controller pulls the hub.
#[test]
fn an_earlier_source_leaves_a_spawn_its_last_exit() {
    let spawn = tile(20, 20);
    let (a, b) = (tile(18, 22), tile(22, 22));
    for controller in [(20u8, 40u8), (6, 22), (20, 6), (40, 22)] {
        let mut walls: Vec<(u8, u8)> =
            vec![(19, 19), (20, 19), (21, 19), (19, 20), (21, 20), (20, 21)];
        for (_, n) in a.neighbors() {
            if ![(19, 21), (17, 23)].contains(&(n.x(), n.y())) {
                walls.push((n.x(), n.y()));
            }
        }
        for (_, n) in b.neighbors() {
            if (n.x(), n.y()) != (21, 21) {
                walls.push((n.x(), n.y()));
            }
        }
        walls.extend([(a.x(), a.y()), (b.x(), b.y()), controller, (30, 30)]);
        let t = terrain(|x, y| {
            if x == 0 && (10..=40).contains(&y) {
                return false;
            }
            edge(x, y) || walls.contains(&(x, y))
        });
        let sources = [a, b];
        let input = PlanInput {
            terrain: &t,
            controller: tile(controller.0, controller.1),
            sources: &sources,
            mineral: Some(tile(30, 30)),
            spawn: Some(spawn),
        };
        for (name, result) in judged(&input) {
            assert_eq!(result, Ok(Vec::new()), "{name} controller {controller:?}");
        }
    }
}

/// E7N57, the live room (STATUS 2026-11-15 to 2026-11-17): its spawn at (43, 21) and the 20
/// extensions ADR-020's interim placement gives it up to RCL4, replayed with
/// `economy::placement::next_extension` on one grid (the first five are checked against the live
/// sites STATUS records; the other fifteen are modeled: the live job rescans the room and blocks
/// what stands, which moved one pick a tile in the review's model and no count). Prints how many
/// each candidate's plan for the room with that spawn keeps as extensions (one may keep none); the
/// rest stand on ground the plan wants for other things or for nothing, and must be destroyed and
/// rebuilt (`CONSTRUCTION_COST` each, V235; the old ones count against `CONTROLLER_STRUCTURES`,
/// V238) as the plan takes over (P4.3).
#[test]
fn the_live_rooms_extensions_against_each_plan() {
    let room = common::corpus::load()
        .into_iter()
        .find(|r| r.name == "E7N57")
        .expect("E7N57 in the corpus");
    let terrain = room.terrain();
    let spawn = tile(43, 21);
    let mineral = room.minerals.first().map(|&(_, t)| t);
    let statics: Vec<Tile> = room.sources.iter().copied().chain(mineral).collect();
    let profile = movement::profile(&MOVEMENT_MMO);
    let mut occupied: Vec<(Tile, bool)> = vec![(spawn, true), (room.controller, true)];
    occupied.extend(statics.iter().map(|&t| (t, true)));
    let mut grid = CostGrid::default();
    grid.build(
        &terrain,
        occupied.iter().map(|&(t, _)| (t, Footprint::Blocked)),
        profile,
    );
    let (mut field, mut dial) = (FlowField::default(), Dial::default());
    let want = allowed(StructureType::Extension, 4) as usize;
    let mut live = Vec::new();
    while live.len() < want {
        let Some(t) = next_extension(
            &grid,
            profile,
            spawn,
            Some(room.controller),
            &statics,
            &occupied,
            &[],
            &mut field,
            &mut dial,
        ) else {
            break;
        };
        occupied.push((t, true));
        live.push(t);
    }
    assert_eq!(live.len(), want);
    let mut first: Vec<Tile> = live.iter().take(5).copied().collect();
    first.sort();
    let mut sites: Vec<Tile> = [(41, 19), (41, 21), (41, 23), (43, 19), (43, 23)]
        .iter()
        .map(|&(x, y)| tile(x, y))
        .collect();
    sites.sort();
    assert_eq!(first, sites, "the replay matches the live sites");
    let input = PlanInput {
        terrain: &terrain,
        controller: room.controller,
        sources: &room.sources,
        mineral,
        spawn: Some(spawn),
    };
    let cost = StructureType::Extension
        .construction_cost()
        .expect("a cost");
    for (name, plan) in CANDIDATES {
        let layout = plan(&input, &mut Scratch::default()).expect("a plan");
        assert!(validate(&input, &layout).valid(), "{name}");
        let at = |t: Tile| {
            layout
                .items
                .iter()
                .find(|i| {
                    i.tile == t && i.kind != StructureType::Road && i.kind != StructureType::Rampart
                })
                .map(|i| i.kind)
        };
        let kept = live
            .iter()
            .filter(|&&t| at(t) == Some(StructureType::Extension))
            .count();
        let under = live
            .iter()
            .filter(|&&t| at(t).is_some_and(|k| k != StructureType::Extension))
            .count();
        let hub = layout.roles.hub.expect("a hub");
        println!(
            "{name}: keeps {kept} of {want} standing extensions, {under} under other planned \
             structures, {} on ground it plans nothing on; {} energy to rebuild the rest; hub {:?} \
             at range {} of the standing spawn",
            want - kept - under,
            (want - kept) as u32 * cost,
            (hub.x(), hub.y()),
            hub.range(spawn)
        );
    }
}
