//! Defense (ROADMAP P7, SPEC §3.6): what hostile creeps in the bot's rooms can do, and the bot's
//! answers. So far this is threat assessment (P7.1, [`threat`]): each hostile creep's power and
//! effective hit points from its body and boosts, the sums per owned room, and the events that say
//! when a room's threat starts, changes, and ends; and tower control (P7.2, [`towers`]): focus fire
//! driven by a combat sim, heal priority, and holding fire against heal the towers cannot beat;
//! and the safe-mode triggers (P7.4, ADR-035, [`safe_mode`]): when a core structure would fall
//! before the towers win, or a CLAIM creep would reach the controller; and the scaled defenders
//! (P7.5, ADR-036, [`defenders`]): ranged creeps spawned only when the towers cannot win, sized by
//! the combat sim; and the remote defenders (P7.6, ADR-037, [`remote`]): ranged creeps with heal
//! parts that a home sends into a remote room it mines when armed hostiles stand there, if a
//! two-sided sim says they win and they cost less than the room's wait (P6.3's evacuation) would
//! lose. Rampart upkeep lives in the economy's opening (P7.3).

pub mod defenders;
pub mod remote;
pub mod safe_mode;
pub mod threat;
pub mod towers;

#[cfg(test)]
mod defenders_tests;
#[cfg(test)]
mod remote_tests;
#[cfg(test)]
mod safe_mode_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod towers_tests;

pub use defenders::{DefenderEvent, Defenders, PRIORITY_DEFENDER, body_for, least_power};
pub use remote::{
    DuelScratch, Fighter, Foe, Loss, PRIORITY_REMOTE_DEFENDER, RemoteDefense, RemoteDefenseEvent,
    Shape, SizeScratch, Sizing, duel, fight, fit, pays, size,
};
pub use safe_mode::{Blocked, Reason, SafeModeEvent, SafeModeOrder, SafeModes};
pub use threat::{
    HostileThreat, Power, RoomThreat, ThreatEvent, Threats, damage_taken, effective_hits, part_hits,
};
pub use towers::{Shot, TowerControl, TowerEvent, TowerOrder, heal_on, kill_ticks, tower_amount};
