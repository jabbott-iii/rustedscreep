//! Safe-mode triggers (ROADMAP P7.4, ADR-035): when the bot activates safe mode in an owned room.
//!
//! # What safe mode does, and why it is spent with care
//!
//! While a room's safe mode lasts (`SAFE_MODE_DURATION` ticks), no creep of another user can
//! attack, dismantle, heal, or withdraw from anything in the room. NPC invaders are included, and
//! so are structures (V330, V331). The owner's creeps and towers act as usual (V281). Hostile
//! creeps can still walk in, pick up, and transfer (V333).
//!
//! It is scarce. A room gains one activation per controller level reached (V188, V334). Each
//! activation starts a `SAFE_MODE_COOLDOWN` in that room, and only one of a player's rooms may be in
//! safe mode at a time (V325). So the bot activates it only when ADR-035's triggers say the room
//! would otherwise lose something it cannot replace soon.
//!
//! # When an activation takes effect
//!
//! The engine applies an activation after the tick's intents (V326, V328), so:
//!
//! - hostile damage in the activation's own tick still lands, and
//! - a hostile `attackController` in that tick cancels the activation (no charge is spent). One
//!   `attackController` also blocks safe mode for `CONTROLLER_ATTACK_BLOCKED_UPGRADE` ticks (V329).
//!
//! So both triggers count a hostile `defense.safe_mode_lead` tiles beyond its reach: a creep moves
//! at most one tile a tick, so with a lead of 1 the bot acts on the last tick before the first hit.
//!
//! # The triggers (ADR-035's choices)
//!
//! Both are judged in each owned room with a dangerous hostile this tick ([`super::Power::dangerous`]),
//! whoever owns it: the NPC username is not verified (V335), and the tower test already spares the
//! raids the towers win.
//!
//! - **The core falls first.** The core is the room's spawns, towers, storage, and terminal
//!   ([`is_core`]). Each dangerous hostile's damage a tick on a core structure is its
//!   [`reach_damage`]. The towers' time K is the sum of each such hostile's kill time by P7.2's sim
//!   ([`super::kill_ticks`]: every tower with energy for a shot, every hostile healer within range 3
//!   at its most), as if the towers kill them one after another and each keeps hitting until the
//!   last dies. K is never when one of them is not killed within `defense.max_kill_ticks`, when the
//!   towers distrust it (the shot check), or when no tower can fire. The structure falls first when
//!   K is never, or when the summed damage a tick times K reaches its `hits`, read this tick
//!   ([`falls_first`]).
//! - **A CLAIM creep near the controller.** A hostile with a live CLAIM part at range r of at most
//!   `1 + defense.safe_mode_lead` from the controller triggers when the sim's kill time k is never or
//!   at least `max(r, 2)` ([`claim_wins`]). Its first hit can come r - 1 ticks from now, a hit in the
//!   tick of its death still lands, and an activation protects only from the next tick on. Here k
//!   counts only the towers this tick's plan fires at the creep ([`TowerControl::firing_at`]): the
//!   towers keep one focus across the room, and the first hit is the loss (it blocks safe mode for
//!   `CONTROLLER_ATTACK_BLOCKED_UPGRADE` ticks, V329).
//!
//! # Whether the room can activate
//!
//! The snapshot reads the controller's `safeMode`, `safeModeAvailable`, and `safeModeCooldown`
//! (V327) on each tick a hostile creep stands in the room ([`crate::snapshot::SafeModeState`]).
//! [`blocked`] mirrors the game side's checks in their order (V325). The one-room rule is judged
//! from the rooms the snapshot has read in safe mode in this global, and at most one room is
//! ordered a tick (the first judged, in room order). A room in safe mode the snapshot has not read
//! makes the game refuse the call (ERR_BUSY, free; V325). An accepted call is not taken as safe mode
//! until a read shows it, since a hostile `attackController` in its tick cancels it (V326).
//!
//! # Calls and lines
//!
//! The bot calls `activateSafeMode` at most once in a room for each open threat (P7.1's
//! [`super::Threats::under_threat`]), whatever the call returns: a refusal is free (V325), but the
//! reasons for one last thousands of ticks. A trigger the room cannot meet is reported once per
//! threat ([`SafeModeEvent::Unavailable`]), and so is a threat in a room already in its safe mode
//! ([`SafeModeEvent::Protected`]). An accepted call may still not take effect: on the tick a
//! cooldown ends the game side reads none while the processor still refuses (V338), and a hostile
//! `attackController` in the call's tick cancels it (V326). So when the next read shows no safe
//! mode and the same activations left, the room may call again ([`SafeModeEvent::NotApplied`]), up
//! to `defense.safe_mode_calls` calls in a threat; a call that threw may too. Once the call is made
//! or the line is out for a threat, the room's
//! triggers are not judged again while its state allows nothing new. The state is heap-only: a
//! global reset forgets which threats were tried.

use rustedscreep_consts::{
    CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD, Position, RoomName, StructureType, TOWER_ENERGY_COST,
    TOWER_POWER_ATTACK, controller_downgrade,
};

use super::threat::{HostileThreat, Power, Threats};
use super::towers::{Shot, TowerControl, heal_on, kill_ticks, range, tower_amount};
use crate::config::DefenseConfig;
use crate::scheduler::Fault;
use crate::snapshot::{Controller, ControllerState, Snapshot};

/// Whether `kind` is a core structure (module docs): a spawn, tower, storage, or terminal.
pub const fn is_core(kind: StructureType) -> bool {
    matches!(
        kind,
        StructureType::Spawn
            | StructureType::Tower
            | StructureType::Storage
            | StructureType::Terminal
    )
}

/// A hostile's most damage a tick on a structure at range `r`, counting it `lead` tiles beyond its
/// reach (module docs): the larger of its `attack` and its `dismantle` within `1 + lead`, plus its
/// `rangedAttack` within `3 + lead`. `dismantle` blocks `attack` on the same creep and neither blocks
/// `rangedAttack` (V44). Unit: hit points.
///
/// Uses: rustedscreep_consts::{ATTACK_POWER, DISMANTLE_POWER, RANGED_ATTACK_POWER} (through
/// [`Power`], V288).
pub fn reach_damage(power: Power, r: u32, lead: u32) -> u32 {
    let close = if r <= lead.saturating_add(1) {
        power.melee_damage().max(power.dismantle_damage())
    } else {
        0
    };
    let far = if r <= lead.saturating_add(3) {
        power.ranged_damage()
    } else {
        0
    };
    close.saturating_add(far)
}

/// Whether a structure with `hits` falls before the towers win (module docs): `damage` a tick from
/// the hostiles in reach, and `kill` the towers' time to kill them all, `None` for never. A
/// structure no hostile reaches does not fall.
pub fn falls_first(hits: u32, damage: u32, kill: Option<u32>) -> bool {
    if damage == 0 {
        return false;
    }
    kill.is_none_or(|k| u64::from(damage) * u64::from(k) >= u64::from(hits))
}

/// Whether a CLAIM creep at range `r` of the controller can still hit it on a tick after this one
/// (module docs): the towers kill it in `kill` ticks (`None` for never), and an activation now
/// protects from the next tick on.
pub fn claim_wins(r: u32, kill: Option<u32>) -> bool {
    kill.is_none_or(|k| k >= r.max(2))
}

/// Why a room cannot activate safe mode now (V325's checks, in their order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// The controller's safe-mode state was not read this tick (no hostile stood in the room).
    NotRead,
    /// No activation left (ERR_NOT_ENOUGH_RESOURCES).
    NoCharges,
    /// The cooldown of the last activation (ERR_TIRED). Unit: ticks left.
    Cooldown { ticks: u32 },
    /// A hostile `attackController` blocked it (ERR_TIRED, V329).
    UpgradeBlocked,
    /// The downgrade timer is below `least` (ERR_TIRED). Unit: ticks.
    Downgrade { ticks: u32, least: u32 },
    /// This room is in safe mode already. Unit: ticks left.
    Active { ticks: u32 },
    /// Another owned room is in safe mode (ERR_BUSY).
    OtherRoom { room: RoomName },
}

/// The least downgrade timer at which a controller of `level` may activate safe mode:
/// `CONTROLLER_DOWNGRADE[level] / 2 - CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD`, rounded up, as the
/// game compares in doubles (V325). 0 for a level with no timer. Unit: ticks.
///
/// Uses: rustedscreep_consts::{controller_downgrade, CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD}.
pub fn least_downgrade(level: u8) -> u32 {
    let full = controller_downgrade(level).unwrap_or(0);
    // ticks < full/2 - T  ⟺  2 * ticks < full - 2T, so the least passing whole tick is
    // ceil((full - 2T) / 2).
    full.saturating_sub(CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD.saturating_mul(2))
        .div_ceil(2)
}

/// Why the room of a controller at `level` with `state` cannot activate safe mode, or `None` when
/// it can, as far as this room's own checks go (the one-room rule is the caller's). V325's order.
///
/// Uses: rustedscreep_consts::{controller_downgrade, CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD}
/// (through [`least_downgrade`]).
pub fn blocked(level: u8, state: &ControllerState) -> Option<Blocked> {
    let Some(safe) = state.safe_mode else {
        return Some(Blocked::NotRead);
    };
    if safe.available == 0 {
        return Some(Blocked::NoCharges);
    }
    if safe.cooldown > 0 {
        return Some(Blocked::Cooldown {
            ticks: safe.cooldown,
        });
    }
    if state.upgrade_blocked {
        return Some(Blocked::UpgradeBlocked);
    }
    let least = least_downgrade(level);
    if state.downgrade_ticks < least {
        return Some(Blocked::Downgrade {
            ticks: state.downgrade_ticks,
            least,
        });
    }
    if safe.ticks > 0 {
        return Some(Blocked::Active { ticks: safe.ticks });
    }
    None
}

/// Whether `room` is in the bot's own safe mode this tick, as read (only on a tick a hostile creep
/// stands in it).
pub fn protected(snap: &Snapshot, room: RoomName) -> bool {
    let rooms = &snap.rooms;
    rooms
        .row_of(room)
        .and_then(|r| rooms.controller_states().get(r))
        .and_then(|s| s.safe_mode)
        .is_some_and(|s| s.ticks > 0)
}

/// What fired a trigger, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// A core structure falls first: its type, tile, and hits; the hostiles in reach, their damage a
    /// tick, the towers that can fire, and their time to kill them (`None` for never).
    Core {
        kind: StructureType,
        at: Position,
        hits: u32,
        hostiles: u32,
        damage: u32,
        towers: u32,
        kill: Option<u32>,
    },
    /// A CLAIM creep near the controller: its row in this tick's `Creeps`, its live CLAIM parts, its
    /// range, the towers that can fire, and their time to kill it (`None` for never).
    Claim {
        row: u32,
        claim: u32,
        range: u32,
        towers: u32,
        kill: Option<u32>,
    },
}

/// A room's call this tick: `activateSafeMode` on the controller at `controller`, a row in this
/// tick's `Structures`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafeModeOrder {
    pub room: RoomName,
    pub controller: u32,
    pub reason: Reason,
}

/// What the triggers saw this tick, for the log ([`SafeModes::events`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafeModeEvent {
    /// A trigger fired, but the room cannot activate safe mode (once per threat).
    Unavailable {
        room: RoomName,
        reason: Reason,
        blocked: Blocked,
    },
    /// A threat in a room already in its safe mode (once per threat). Unit: ticks left.
    Protected { room: RoomName, ticks: u32 },
    /// An accepted call did not take effect: the next read shows no safe mode and the same
    /// activations left (the engine refused it, V338, or a hostile `attackController` cancelled it,
    /// V326). `again` when the room may call once more in this threat (`defense.safe_mode_calls`).
    NotApplied { room: RoomName, again: bool },
}

/// One owned room's safe-mode state, kept between ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RoomMode {
    room: RoomName,
    /// The start tick of the threat in which the bot called `activateSafeMode` and may not call
    /// again (cleared when a call did not take effect and the room may retry).
    called: Option<u32>,
    /// The start tick of the threat the room's calls are counted for, and the count.
    counted: Option<u32>,
    calls: u32,
    /// The activations left as read on the tick of a call not known to have failed, until a read
    /// shows whether it took effect.
    pending: Option<u32>,
    /// The start tick of the threat whose line went out (an unmet trigger, or protection).
    logged: Option<u32>,
    /// The tick the room's safe mode is known to end, as last read (V327).
    until: Option<u32>,
}

impl RoomMode {
    const fn new(room: RoomName, until: Option<u32>) -> Self {
        Self {
            room,
            called: None,
            counted: None,
            calls: 0,
            pending: None,
            logged: None,
            until,
        }
    }
}

/// The rooms' safe-mode triggers: this tick's rooms to judge, their calls and events, and each
/// room's state (module docs).
#[derive(Default)]
pub struct SafeModes {
    /// This tick's rooms to judge.
    units: Vec<RoomName>,
    rooms: Vec<RoomMode>,
    orders: Vec<SafeModeOrder>,
    events: Vec<SafeModeEvent>,
    /// Scratch: the room's healers, its towers that can fire, their shots at one hostile, and each
    /// hostile's kill time by index, once a trigger needs it (`None` until then).
    healers: Vec<(Position, Power)>,
    armed: Vec<(Position, u32)>,
    shots: Vec<Shot>,
    kills: Vec<Option<Option<u32>>>,
}

impl SafeModes {
    /// Starts the tick: takes this tick's safe-mode reads of every owned room (for the one-room
    /// rule, whichever room is judged first), then lists the owned rooms with a dangerous hostile
    /// this tick, when `defense.safe_mode` is on.
    pub fn begin(&mut self, snap: &Snapshot, threats: &Threats, cfg: &DefenseConfig) {
        self.units.clear();
        self.orders.clear();
        self.events.clear();
        let now = snap.tick.time;
        let rooms = &snap.rooms;
        for (&room, state) in rooms.names().iter().zip(rooms.controller_states()) {
            let Some(safe) = state.safe_mode else {
                continue;
            };
            let until = (safe.ticks > 0).then(|| now.saturating_add(safe.ticks));
            match self.rooms.iter_mut().find(|m| m.room == room) {
                Some(m) => m.until = until,
                None if until.is_some() => self.rooms.push(RoomMode::new(room, until)),
                None => {}
            }
        }
        if !cfg.safe_mode {
            return;
        }
        self.units.extend(
            threats
                .rooms()
                .iter()
                .filter(|r| r.dangerous > 0)
                .map(|r| r.room),
        );
    }

    /// This tick's rooms to judge.
    pub fn unit_count(&self) -> u32 {
        u32::try_from(self.units.len()).unwrap_or(u32::MAX)
    }

    /// Judges room `unit` (module docs): its state, then the triggers, then the call or the line.
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        threats: &Threats,
        towers: &TowerControl,
        cfg: &DefenseConfig,
    ) -> Result<(), Fault> {
        let room = usize::try_from(unit)
            .ok()
            .and_then(|u| self.units.get(u))
            .copied()
            .ok_or(Fault::new("no safe-mode room for the unit", unit))?;
        let now = snap.tick.time;
        let rooms = &snap.rooms;
        let row = rooms
            .row_of(room)
            .ok_or(Fault::new("safe-mode room not in the snapshot", unit))?;
        let Some(&Controller::Mine { level }) = rooms.controllers().get(row) else {
            return Ok(());
        };
        let state = rooms
            .controller_states()
            .get(row)
            .copied()
            .ok_or(Fault::new("safe-mode room has no controller state", unit))?;
        let since = threats
            .under_threat(room)
            .ok_or(Fault::new("safe-mode room has no open threat", unit))?;
        let at = match self.rooms.iter().position(|r| r.room == room) {
            Some(i) => i,
            None => {
                self.rooms.push(RoomMode::new(room, None));
                self.rooms.len() - 1
            }
        };
        let mut mode = self
            .rooms
            .get(at)
            .copied()
            .ok_or(Fault::new("safe-mode room state missing", unit))?;
        // A call that did not take effect (module docs): the room may call again, up to
        // `defense.safe_mode_calls` in a threat.
        if let (Some(left), Some(safe)) = (mode.pending, state.safe_mode) {
            mode.pending = None;
            if safe.ticks == 0 && safe.available == left && mode.called == Some(since) {
                let again = mode.counted == Some(since) && mode.calls < cfg.safe_mode_calls;
                if again {
                    mode.called = None;
                }
                self.events.push(SafeModeEvent::NotApplied { room, again });
            }
        }
        if let Some(safe) = state.safe_mode
            && safe.ticks > 0
        {
            if mode.logged != Some(since) {
                mode.logged = Some(since);
                self.events.push(SafeModeEvent::Protected {
                    room,
                    ticks: safe.ticks,
                });
            }
            self.store(at, mode);
            return Ok(());
        }
        // The one-room rule (V325): a room read in safe mode this tick or before, or a room
        // ordered this tick (only one may have it, so the first room judged gets the call).
        let other = self
            .rooms
            .iter()
            .find(|m| m.room != room && m.until.is_some_and(|u| u > now))
            .map(|m| m.room)
            .or_else(|| self.orders.first().map(|o| o.room));
        let why = blocked(level, &state).or(other.map(|room| Blocked::OtherRoom { room }));
        // Nothing can come of the triggers when the line is out or the call made for this threat.
        let done = match why {
            Some(_) => mode.logged == Some(since),
            None => mode.called == Some(since),
        };
        let Some(r) = threats.room(room).filter(|_| !done) else {
            self.store(at, mode);
            return Ok(());
        };
        let hostiles = threats.hostiles(r);
        let reason = self.trigger(snap, room, hostiles, towers, cfg);
        if let Some((controller, reason)) = reason {
            match why {
                Some(blocked) => {
                    if mode.logged != Some(since) {
                        mode.logged = Some(since);
                        self.events.push(SafeModeEvent::Unavailable {
                            room,
                            reason,
                            blocked,
                        });
                    }
                }
                None => {
                    if mode.called != Some(since) {
                        mode.called = Some(since);
                        if mode.counted != Some(since) {
                            mode.counted = Some(since);
                            mode.calls = 0;
                        }
                        mode.calls = mode.calls.saturating_add(1);
                        mode.pending = state.safe_mode.map(|s| s.available);
                        self.orders.push(SafeModeOrder {
                            room,
                            controller,
                            reason,
                        });
                    }
                }
            }
        }
        self.store(at, mode);
        Ok(())
    }

    fn store(&mut self, at: usize, mode: RoomMode) {
        if let Some(m) = self.rooms.get_mut(at) {
            *m = mode;
        }
    }

    /// The first trigger that fires in `room`, with its controller's structure row: a core
    /// structure that falls first, else a CLAIM creep near the controller (module docs). `None`
    /// when neither fires, or when the room's controller is not among this tick's structures.
    fn trigger(
        &mut self,
        snap: &Snapshot,
        room: RoomName,
        hostiles: &[HostileThreat],
        towers: &TowerControl,
        cfg: &DefenseConfig,
    ) -> Option<(u32, Reason)> {
        let st = &snap.structures;
        let in_room = |i: usize| st.positions().get(i).is_some_and(|p| p.room_name() == room);
        let controller = st
            .kinds()
            .iter()
            .enumerate()
            .find(|&(i, &k)| k == StructureType::Controller && in_room(i))
            .and_then(|(i, _)| {
                let pos = st.positions().get(i).copied()?;
                Some((u32::try_from(i).ok()?, pos))
            })?;
        let lead = cfg.safe_mode_lead;
        let creeps = &snap.creeps;
        let position = |row: u32| {
            usize::try_from(row)
                .ok()
                .and_then(|i| creeps.positions().get(i))
                .copied()
        };
        // Each hostile's kill time, at most once, for the hostiles a trigger looks at.
        self.armed.clear();
        self.armed.extend(towers.armed_in(room));
        let armed = u32::try_from(self.armed.len()).unwrap_or(u32::MAX);
        self.healers.clear();
        for h in hostiles {
            if h.power.heal > 0
                && let Some(pos) = position(h.row)
            {
                self.healers.push((pos, h.power));
            }
        }
        self.kills.clear();
        self.kills.resize(hostiles.len(), None);
        // The core falls first.
        for (i, &kind) in st.kinds().iter().enumerate() {
            if !is_core(kind) {
                continue;
            }
            let (Some(&at), Some(&hits)) = (st.positions().get(i), st.hits().get(i)) else {
                continue;
            };
            // 0 hits: not read this tick, so it cannot be judged (a standing structure has 1 or
            // more, and the snapshot reads them on every tick a hostile stands in the room).
            if at.room_name() != room || hits == 0 {
                continue;
            }
            let mut damage = 0u32;
            let mut kill = Some(0u32);
            let mut count = 0u32;
            for (index, h) in hostiles.iter().enumerate() {
                if !h.power.dangerous() {
                    continue;
                }
                let Some(pos) = position(h.row) else {
                    continue;
                };
                let d = reach_damage(h.power, range(pos, at), lead);
                if d == 0 {
                    continue;
                }
                let k = self.kill_cached(index, snap, room, h, towers, cfg);
                damage = damage.saturating_add(d);
                kill = kill.zip(k).map(|(a, b)| a.saturating_add(b));
                count = count.saturating_add(1);
            }
            if falls_first(hits, damage, kill) {
                return Some((
                    controller.0,
                    Reason::Core {
                        kind,
                        at,
                        hits,
                        hostiles: count,
                        damage,
                        towers: armed,
                        kill,
                    },
                ));
            }
        }
        // A CLAIM creep near the controller.
        for h in hostiles {
            if h.power.claim == 0 {
                continue;
            }
            let Some(pos) = position(h.row) else {
                continue;
            };
            let r = range(pos, controller.1);
            if r > lead.saturating_add(1) {
                continue;
            }
            // Only the towers this tick's plan fires at it count: they keep one focus across the
            // room (P7.2), so one that could kill it may be shooting another hostile.
            let firing = u32::try_from(towers.firing_at(h.row).count()).unwrap_or(u32::MAX);
            let k = self.kill_planned(snap, h, towers, cfg);
            if claim_wins(r, k) {
                return Some((
                    controller.0,
                    Reason::Claim {
                        row: h.row,
                        claim: h.power.claim,
                        range: r,
                        towers: firing,
                        kill: k,
                    },
                ));
            }
        }
        None
    }

    /// The towers' time to kill hostile `h` with only the towers this tick's plan fires at it, by
    /// P7.2's sim, or `None` when none does (H1 of the P7.4 audit).
    ///
    /// Uses: rustedscreep_consts::{TOWER_POWER_ATTACK, TOWER_ENERGY_COST} (through the sim).
    fn kill_planned(
        &mut self,
        snap: &Snapshot,
        h: &HostileThreat,
        towers: &TowerControl,
        cfg: &DefenseConfig,
    ) -> Option<u32> {
        let creeps = &snap.creeps;
        let index = usize::try_from(h.row).ok()?;
        let pos = creeps.positions().get(index).copied()?;
        let body = creeps.body(index)?;
        self.shots.clear();
        self.shots
            .extend(towers.firing_at(h.row).map(|(t, energy)| Shot {
                damage: tower_amount(TOWER_POWER_ATTACK, range(t, pos)),
                shots: energy / TOWER_ENERGY_COST,
            }));
        if self.shots.is_empty() {
            return None;
        }
        let heal = heal_on(pos, &self.healers);
        kill_ticks(
            body.parts(),
            h.hits,
            h.hits_max,
            &self.shots,
            heal,
            cfg.max_kill_ticks,
        )
    }

    /// [`Self::kill_of`] for the hostile at `index` of the room's, computed once a tick.
    fn kill_cached(
        &mut self,
        index: usize,
        snap: &Snapshot,
        room: RoomName,
        h: &HostileThreat,
        towers: &TowerControl,
        cfg: &DefenseConfig,
    ) -> Option<u32> {
        if let Some(&Some(k)) = self.kills.get(index) {
            return k;
        }
        let k = self.kill_of(snap, room, h, towers, cfg);
        if let Some(slot) = self.kills.get_mut(index) {
            *slot = Some(k);
        }
        k
    }

    /// The towers' time to kill hostile `h` alone by P7.2's sim (module docs), or `None` for never:
    /// no tower can fire, the towers distrust it, its body is unread, or it outlasts
    /// `defense.max_kill_ticks`.
    ///
    /// Uses: rustedscreep_consts::{TOWER_POWER_ATTACK, TOWER_ENERGY_COST} (through the sim).
    fn kill_of(
        &mut self,
        snap: &Snapshot,
        room: RoomName,
        h: &HostileThreat,
        towers: &TowerControl,
        cfg: &DefenseConfig,
    ) -> Option<u32> {
        let creeps = &snap.creeps;
        let index = usize::try_from(h.row).ok()?;
        let pos = creeps.positions().get(index).copied()?;
        let handle = creeps.handles().get(index).copied()?;
        if towers.distrusts(room, handle) {
            return None;
        }
        let body = creeps.body(index)?;
        self.shots.clear();
        self.shots
            .extend(self.armed.iter().map(|&(t, energy)| Shot {
                damage: tower_amount(TOWER_POWER_ATTACK, range(t, pos)),
                shots: energy / TOWER_ENERGY_COST,
            }));
        if self.shots.is_empty() {
            return None;
        }
        let heal = heal_on(pos, &self.healers);
        kill_ticks(
            body.parts(),
            h.hits,
            h.hits_max,
            &self.shots,
            heal,
            cfg.max_kill_ticks,
        )
    }

    /// Records that this tick's call in `room` did not go through: the game refused it (`retry`
    /// false: its reasons last, V325), or it threw or had no JS object (`retry` true: the room may
    /// call again on a later tick of the threat, up to `defense.safe_mode_calls`).
    pub fn not_accepted(&mut self, room: RoomName, retry: bool, cfg: &DefenseConfig) {
        if let Some(m) = self.rooms.iter_mut().find(|m| m.room == room) {
            m.pending = None;
            if retry && m.calls < cfg.safe_mode_calls {
                m.called = None;
            }
        }
    }

    /// Ends the tick: forgets the rooms with no open threat and no safe mode known to last.
    pub fn finish(&mut self, now: u32, threats: &Threats) {
        self.rooms
            .retain(|m| threats.under_threat(m.room).is_some() || m.until.is_some_and(|u| u > now));
    }

    /// This tick's calls.
    pub fn orders(&self) -> &[SafeModeOrder] {
        &self.orders
    }

    /// This tick's events.
    pub fn events(&self) -> &[SafeModeEvent] {
        &self.events
    }
}
