//! The minimal spawn path (ROADMAP P2; the P5 global spawn queue replaces the planner): creep bodies
//! ([`Body`]; every job solves its bodies from its targets in `economy::sizing`, P3.6), creep names
//! as identity ([`CreepName`], [`Identity`], [`Role`]), and the per-room planner ([`Spawner`]) that
//! turns this tick's [`SpawnRequest`]s into [`SpawnOrder`]s for the act layer. All pure; rustedscreep-wasm reads the spawns, the room energy and the creep names into
//! the snapshot and issues the orders as `spawnCreep` calls.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. After the snapshot, [`Spawner::begin`].
//! 2. Jobs call [`Spawner::request`] (the RCL1–3 opening from the next P2 item; the console's
//!    request until then).
//! 3. [`Spawner::close`] ends the requests; the Critical `spawn` job runs [`Spawner::plan_room`]
//!    once per room in [`Spawner::rooms`]: rooms with a request and a ready spawn (none when
//!    nothing is asked or every spawn of each asking room is busy or held).
//! 4. The act step issues [`Spawner::orders`] and reports each outcome to [`Spawner::record`].

mod body;
mod name;
mod spawner;

pub use body::{Body, known_part};
pub use name::{CreepName, Identity, NAME_MAX, ORDERS_PER_TICK, Role, serial};
pub use spawner::{
    BodySpec, EnergyBudget, SpawnEvent, SpawnOrder, SpawnRequest, SpawnStats, SpawnTotals, Spawner,
};

#[cfg(test)]
mod tests;
