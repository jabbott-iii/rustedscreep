//! The base planner (ROADMAP P4, SPEC §3.3): where a room's structures, roads and ramparts go, for
//! every controller level. P4.2 builds two candidates on a shared footing so ADR-007 can pick one
//! by measurement: a stamp/bunker planner and a distance-transform planner with min-cut ramparts.
//!
//! The candidates: [`bunker`] (fixed stamps, ADR-007 option a) and [`flood`] (a distance-transform
//! core and a flood fill, with min-cut ramparts, option b, which ADR-007 chose on 2026-11-18; the
//! bunker stays as the comparison baseline for now). Each is a resumable run: `Run::step`
//! does one phase per call (the scheduler's [`Step`](crate::scheduler::Step)), so P4.3 can slice
//! it across ticks in the Optional tier; `plan` runs every step at once.
//!
//! The shared footing:
//! - [`layout`]: what a planner takes ([`PlanInput`]) and gives ([`Layout`]: every item with the
//!   level it belongs from, and the tiles whose role the economy reads by position);
//! - [`rules`]: the game's placement rules (V234 to V237, V243);
//! - [`board`]: a planner's working state, which enforces the rules on every placement;
//! - [`validate`]: the referee both candidates are judged by, and their measures;
//! - [`sites`], [`roads`], [`defence`] and [`score`]: the steps every candidate shares, so they
//!   compare on their base alone (the economy's tiles, roads by A*, ramparts by minimum cut, the
//!   anchor's score);
//! - [`transform`], [`mincut`] and [`scratch`]: the distance transform, the minimum cut and the
//!   buffers a run reuses.

pub mod board;
pub mod build;
pub mod bunker;
pub mod defence;
pub mod flood;
pub mod job;
pub mod layout;
pub mod mincut;
pub mod roads;
pub mod rules;
pub mod score;
pub mod scratch;
pub mod sites;
pub mod transform;
pub mod validate;

pub use board::Board;
pub use layout::{Item, Layout, MAX_LEVEL, PlanError, PlanInput, Roles};
pub use mincut::{Cut, CutError, CutRole, MinCut};
pub use scratch::Scratch;
pub use transform::distance_transform;
pub use validate::{Metrics, Problem, Report, validate};
