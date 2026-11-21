//! The global spawn queue (ROADMAP P5.1, grown from P2's minimal spawn path): creep bodies
//! ([`Body`]; every job solves its bodies from its targets in `economy::sizing`, P3.6), creep names
//! as identity ([`CreepName`], [`Identity`], [`Role`]), and the queue ([`Spawner`]) that turns this
//! tick's [`SpawnRequest`]s from every room into [`SpawnOrder`]s for the act layer, each served by
//! its home's spawns or, for an owned home without one, by a spawn room within `spawn.help_range`
//! ([`room_distance`]) whose capacity fits the body, on a tick that room starts full while the
//! Standard tier runs. All pure; rustedscreep-wasm reads the spawns, the room energy and the
//! creep names into the snapshot and issues the orders as `spawnCreep` calls.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. After the snapshot, [`Spawner::begin`].
//! 2. Jobs call [`Spawner::request`] (the economy's jobs, P2.4 and P3; the console's request).
//! 3. [`Spawner::close`] ends the requests; the Critical `spawn` job runs [`Spawner::plan_room`]
//!    once per room in [`Spawner::rooms`]: serving rooms with a request and a ready spawn (none
//!    when nothing is asked or every spawn of each serving room is busy or held).
//! 4. The act step issues [`Spawner::orders`] and reports each outcome to [`Spawner::record`].
//!
//! No wasted energy (P5.3): the spawner counts the spawn-ticks a spawn stood idle while its room
//! had a request, by cause, and names the rooms that waited for energy on the last tick
//! ([`Spawner::waits_for_energy`]), whose workers and haulers then bring the spawns the energy in
//! the room's stores.
//!
//! Pre-spawn replacement (P5.2, [`lead`], [`is_due`]): the jobs whose creeps hold a post (miners,
//! haulers, upgraders) count a creep whose `ticksToLive` is within its successor's spawn time,
//! walk and `spawn.replace_margin_ticks` as due, so they ask for the successor while the creep
//! still works and the successor reaches the post by the tick the creep is gone.

mod body;
mod name;
mod replace;
mod spawner;

pub use body::{Body, known_part};
pub use name::{CreepName, Identity, NAME_MAX, ORDERS_PER_TICK, Role, serial};
pub use replace::{is_due, lead, lead_ticks};
pub use spawner::{
    BodySpec, EnergyBudget, SpawnEvent, SpawnOrder, SpawnRequest, SpawnSide, SpawnStats,
    SpawnTotals, Spawner, room_distance,
};

#[cfg(test)]
mod tests;
