//! Roads between the hub and the places creeps walk to, routed by A* over the room as planned: a
//! planned road costs the least step, so a later road follows an earlier one where it can, and
//! swamp costs what a creep's fatigue pays there (V54), which also keeps roads off swamp, where
//! they cost 5 times as much to build (V235). A route never steps on a wall (no tunnels, V242).

use rustedscreep_consts::{ROOM_AREA, StructureType};

use super::board::Board;
use super::layout::PlanError;
use super::scratch::{Scratch, fatigue};
use crate::movement::{Aim, Goal, Query, Tile, range_estimate};

/// The most A* pops a road search may take: every tile several times over.
const MAX_POPS: u32 = 8 * ROOM_AREA as u32;

/// Plans a road from `from` to within `range` of `to`, every new tile at `level`, and a road it
/// reuses that belongs from a later level moved to `level`, so the route is whole from `level`.
/// Returns how many tiles were added.
pub fn road(
    board: &mut Board<'_>,
    from: Tile,
    to: Tile,
    range: u8,
    level: u8,
    s: &mut Scratch,
) -> Result<u32, PlanError> {
    board.cost_grid(&mut s.grid, fatigue());
    let goal = Goal { tile: to, range };
    let q = Query {
        grid: &s.grid,
        blocked: |_: Tile| false,
        estimate: range_estimate(goal, fatigue().cheapest()),
        start: from,
        goal: Aim::Near(goal),
        max_pops: MAX_POPS,
    };
    if !s.search.search(q, &mut s.path) {
        return Err(PlanError::NoRoom("road"));
    }
    let mut added = 0;
    for &t in s.path.iter().chain([from].iter()) {
        if board.has_road(t) {
            board.lower_road(t, level);
        } else if board.can_place(StructureType::Road, t)
            && board.place_at_level(StructureType::Road, t, level)
        {
            added += 1;
        }
    }
    Ok(added)
}
