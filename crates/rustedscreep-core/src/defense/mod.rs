//! Defense (ROADMAP P7, SPEC §3.6): what hostile creeps in the bot's rooms can do, and the bot's
//! answers. So far this is threat assessment (P7.1, [`threat`]): each hostile creep's power and
//! effective hit points from its body and boosts, the sums per owned room, and the events that say
//! when a room's threat starts, changes, and ends; and tower control (P7.2, [`towers`]): focus fire
//! driven by a combat sim, heal priority, and holding fire against heal the towers cannot beat.
//! Rampart upkeep, safe-mode triggers, scaled defenders, and remote evacuation build on them.

pub mod threat;
pub mod towers;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod towers_tests;

pub use threat::{
    HostileThreat, Power, RoomThreat, ThreatEvent, Threats, damage_taken, effective_hits, part_hits,
};
pub use towers::{Shot, TowerControl, TowerEvent, TowerOrder, heal_on, kill_ticks, tower_amount};
