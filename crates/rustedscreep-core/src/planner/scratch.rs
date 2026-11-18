//! Buffers a planner run reuses: a cost grid, a flow field and its queue, an A* search with its path,
//! the min-cut graph and a distance-transform grid. Built once and kept, so a run (and a time-sliced
//! run across ticks) allocates only to grow them.

use rustedscreep_consts::{MOVE_COST_PLAIN, MOVE_COST_ROAD, MOVE_COST_SWAMP, ROOM_AREA};

use super::mincut::MinCut;
use crate::movement::{CostGrid, CostProfile, Dial, FlowField, PathSearch, Tile};

/// See the module docs.
pub struct Scratch {
    pub grid: CostGrid,
    pub field: FlowField,
    pub dial: Dial,
    pub search: PathSearch,
    pub path: Vec<Tile>,
    pub cut: MinCut,
    pub dt: Box<[u8; ROOM_AREA]>,
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            grid: CostGrid::default(),
            field: FlowField::default(),
            dial: Dial::default(),
            search: PathSearch::default(),
            path: Vec::new(),
            cut: MinCut::new(),
            dt: Box::new([0; ROOM_AREA]),
        }
    }
}

/// The movement fatigue rates as step costs (V54, V154): what a planner measures paths with, and
/// routes roads by (a planned road costs the least, so later roads reuse earlier ones).
///
/// Uses: `MOVE_COST_ROAD`, `MOVE_COST_PLAIN`, `MOVE_COST_SWAMP`.
pub fn fatigue() -> CostProfile {
    let c = |v: u32| u8::try_from(v).unwrap_or(u8::MAX);
    CostProfile {
        road: c(MOVE_COST_ROAD),
        plain: c(MOVE_COST_PLAIN),
        swamp: c(MOVE_COST_SWAMP),
    }
    .sanitized()
}
