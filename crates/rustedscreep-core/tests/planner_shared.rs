//! The planner's shared steps (P4.2) on every corpus room, around a minimal core: a spawn, the
//! storage, the hub and the hub link's tile in the open 5 x 5 nearest the controller. With the
//! economy's tiles, roads and min-cut ramparts added, the validator must find nothing wrong but
//! the required types a full planner adds (extensions, towers and the rest).

mod common;

use rustedscreep_consts::{ROOM_AREA, StructureType};
use rustedscreep_core::movement::Tile;
use rustedscreep_core::planner::validate::Problem;
use rustedscreep_core::planner::{
    Board, PlanInput, Scratch, defence, distance_transform, roads, sites, validate,
};

fn at(c: Tile, dx: i8, dy: i8) -> Tile {
    Tile::new(c.x().wrapping_add_signed(dx), c.y().wrapping_add_signed(dy)).expect("in room")
}

#[test]
fn shared_steps_leave_only_the_full_planners_work() {
    let rooms = common::corpus::load();
    let mut s = Scratch::default();
    let mut ramparts = Vec::new();
    for room in &rooms {
        let terrain = room.terrain();
        let mineral = room.minerals.first().map(|&(_, t)| t);
        let input = PlanInput {
            terrain: &terrain,
            controller: room.controller,
            sources: &room.sources,
            mineral,
            spawn: None,
        };
        let mut board = Board::new(&input);
        sites::reserve(&mut board, &input);
        distance_transform(|t| board.open(t) && !board.is_clear(t), &mut s.dt);
        let core = (0..ROOM_AREA)
            .filter_map(Tile::from_index)
            .filter(|t| s.dt[t.index()] >= 3)
            .min_by_key(|t| (t.range(room.controller), t.index()))
            .unwrap_or_else(|| panic!("{}: no open 5 x 5", room.name));
        let (hub, hub_link) = (core, at(core, 1, 0));
        assert!(
            board.place(StructureType::Spawn, at(core, 0, -1)).is_some(),
            "{}",
            room.name
        );
        assert!(
            board
                .place(StructureType::Storage, at(core, -1, 0))
                .is_some(),
            "{}",
            room.name
        );
        board.roles.hub = Some(hub);
        board.roles.storage = Some(at(core, -1, 0));
        sites::containers(&mut board, &input, hub, &mut s)
            .unwrap_or_else(|e| panic!("{}: {e:?}", room.name));
        sites::links(&mut board, &input, hub, hub_link, &mut s)
            .unwrap_or_else(|e| panic!("{}: {e:?}", room.name));
        sites::mineral(&mut board, &input, hub, &mut s)
            .unwrap_or_else(|e| panic!("{}: {e:?}", room.name));
        let mining = board.roles.mining.clone();
        for m in mining {
            roads::road(&mut board, hub, m, 0, 2, &mut s)
                .unwrap_or_else(|e| panic!("{}: {e:?}", room.name));
        }
        let cc = board
            .roles
            .controller_container
            .expect("controller container");
        roads::road(&mut board, hub, cc, 1, 2, &mut s)
            .unwrap_or_else(|e| panic!("{}: {e:?}", room.name));
        if let Some(mc) = board.roles.mineral_container {
            roads::road(&mut board, hub, mc, 0, 6, &mut s)
                .unwrap_or_else(|e| panic!("{}: {e:?}", room.name));
        }
        let protect = defence::protected_tiles(&board);
        let plan = defence::plan(&input, &protect, &mut s)
            .unwrap_or_else(|e| panic!("{}: {e:?}", room.name));
        defence::apply(&mut board, &input, &plan, 2)
            .unwrap_or_else(|e| panic!("{}: {e:?}", room.name));
        ramparts.push(plan.tiles().len());
        let layout = board.into_layout();
        let report = validate(&input, &layout);
        let other: Vec<&Problem> = report
            .problems
            .iter()
            .filter(|p| !matches!(p, Problem::Short { .. }))
            .collect();
        assert!(other.is_empty(), "{}: {other:?}", room.name);
        assert_eq!(
            report.metrics.to_sources.len(),
            room.sources.len(),
            "{}",
            room.name
        );
    }
    assert_eq!(ramparts.len(), rooms.len());
}
