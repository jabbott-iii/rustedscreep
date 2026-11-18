//! Where a base's hub should go, scored the same way for every candidate (ADR-007's comparison is
//! of layouts, not of anchor rules): the path cost over the bare room from the hub to standing next
//! to each source and to upgrade range of the controller (V187), at the movement fatigue rates,
//! summed. Those are the walks the room's creeps make most (miners and haulers, upgraders and their
//! haulers). Lower is better.
//!
//! The fields are built from the targets once per run, so scoring a hub reads one distance per
//! target.

use super::layout::PlanInput;
use super::scratch::fatigue;
use crate::economy::RECEIVER_RANGE;
use crate::movement::{CostGrid, Dial, FlowField, Goal, Tile, UNREACHED};

/// Path costs from every tile to the room's sources and controller.
pub struct AnchorCost {
    fields: Vec<FlowField>,
}

impl AnchorCost {
    /// Builds one field per source (range 1) and one for the controller (`RECEIVER_RANGE`) over the
    /// bare room (terrain only).
    pub fn new(input: &PlanInput<'_>) -> Self {
        let mut grid = CostGrid::default();
        grid.build(input.terrain, std::iter::empty(), fatigue());
        let mut dial = Dial::default();
        let goals = input
            .sources
            .iter()
            .map(|&s| Goal { tile: s, range: 1 })
            .chain([Goal {
                tile: input.controller,
                range: RECEIVER_RANGE,
            }]);
        let fields = goals
            .map(|g| {
                let mut f = FlowField::default();
                f.compute(&grid, g, &mut dial);
                f
            })
            .collect();
        Self { fields }
    }

    /// The hub's score, or `None` when a target cannot be reached from it.
    pub fn cost(&self, hub: Tile) -> Option<u32> {
        self.fields.iter().try_fold(0u32, |sum, f| {
            let d = f.distance(hub);
            (d != UNREACHED).then(|| sum + u32::from(d))
        })
    }
}
