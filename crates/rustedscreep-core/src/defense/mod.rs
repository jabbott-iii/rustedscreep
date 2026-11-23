//! Defense (ROADMAP P7, SPEC §3.6): what hostile creeps in the bot's rooms can do, and the bot's
//! answers. So far this is threat assessment (P7.1, [`threat`]): each hostile creep's power and
//! effective hit points from its body and boosts, the sums per owned room, and the events that say
//! when a room's threat starts, changes, and ends. Tower focus fire, rampart upkeep, safe-mode
//! triggers, scaled defenders, and remote evacuation build on it.

pub mod threat;

#[cfg(test)]
mod tests;

pub use threat::{
    HostileThreat, Power, RoomThreat, ThreatEvent, Threats, damage_taken, effective_hits, part_hits,
};
