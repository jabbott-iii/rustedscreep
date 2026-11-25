//! Threat assessment (ROADMAP P7.1): what each hostile creep in an owned room can do this tick, from
//! its body and boosts, and the sums per room.
//!
//! # A hostile creep's power
//!
//! The snapshot reads a hostile creep's body once (each part's type and boost compound, V289) and its
//! `hits` every tick ([`crate::snapshot::Creeps::set_assessed`]). The engine derives each part's hit
//! points from the creep's total: from the last part to the first, each part holds up to
//! `BODYPART_HITS` of what is left (V282, V283). So damage empties the first parts first, and the live
//! parts (those with hits above 0) are the last `ceil(hits / BODYPART_HITS)` ([`part_hits`]).
//!
//! An action's power is the sum, over the live parts of the action's type, of the action's base power
//! times the part's boost multiplier for that action, or 1 for an unboosted part
//! (`calcBodyEffectiveness`, V286; the multipliers are the `BOOSTS` table, which the constants
//! self-check compares with the server's, V131). [`Power`] keeps these sums in boosted parts: an
//! unboosted part counts 1, and a part boosted with a tier-3 compound counts 4. Its methods turn them
//! into damage and heal per tick with each action's base power (V288):
//!
//! - melee `attack` (ATTACK parts, range 1) at `ATTACK_POWER`. A creep hit in melee hits back with
//!   its own ATTACK parts, unless the attacker stands on a rampart or (as summarized, `_damage.js`
//!   about L39-40) the attacker's own safe mode is active in the room;
//! - `rangedAttack` (RANGED_ATTACK parts, range 3) at `RANGED_ATTACK_POWER`, and `rangedMassAttack`
//!   at `RANGED_MASS_ATTACK_POWER_RANGE_1` to `_3` per boosted part by range;
//! - `heal` (HEAL parts, range 1) at `HEAL_POWER`, and `rangedHeal` (range 3) at `RANGED_HEAL_POWER`;
//! - `dismantle` (WORK parts, range 1, structures only) at `DISMANTLE_POWER`;
//! - `attackController` (CLAIM parts, range 1). Boosts play no part in it.
//!
//! The sums say what each action can do. Which of them a creep can use in the same tick is the intent
//! table's job ([`crate::intents`], V44): `attack` and `rangedAttack` can both run, but `heal` blocks
//! `attack`, `dismantle`, and `rangedHeal`.
//!
//! A part at 0 hits adds nothing. The engine's `_oldHits` (a part's hits before the tick's last
//! recalculation, which `calcBodyEffectiveness` would still count) is set only after the tick's
//! intents have run and is stripped when the tick's objects are saved (V293), so these sums are the
//! engine's for the coming tick. That is read in the open-source driver; the MMO's is not public. If
//! it ever lasted a tick, the power here would be short by the parts just lost, which is the unsafe
//! direction for defense.
//!
//! # Effective hit points
//!
//! A TOUGH part boosted with a compound that has a `damage` ratio (0.7, 0.5, or 0.3) takes only that
//! share of the damage that reaches it. Damage walks the body from the first part, and each part
//! absorbs up to its hits divided by its ratio. The engine sums a tick's damage, subtracts the
//! rounded saving (`_applyDamage`, V287, [`damage_taken`]), then adds the tick's heal, and the creep
//! dies when its hits reach 0. [`effective_hits`] is the least damage in one tick, with no heal, that
//! kills the creep as its parts stand now. With no boosted TOUGH part it is the creep's hits.
//!
//! # Rooms and events
//!
//! [`Threats::assess`] groups the tick's assessed hostiles by room, owned rooms only, and keeps one
//! [`RoomThreat`] per room with a hostile in it (the snapshot also assesses the hostiles of a mined
//! remote room that holds an armed one, for the remote defenders of P7.6, [`super::remote`]). A hostile is dangerous when it can attack, ranged
//! attack, dismantle, or attack the controller ([`Power::dangerous`]). A healer, a scout, or a hauler
//! alone is not. A room with a dangerous hostile is under threat from that tick until none has been
//! seen there for `defense.threat_end_ticks`. The events ([`ThreatEvent`]) report a threat's start,
//! a change in its count of dangerous or boosted hostiles (at most once per `defense.threat_log_ticks`),
//! and its end. The state is heap-only: a global reset forgets every threat, so an ongoing one starts
//! again.

use rustedscreep_consts::extra::BODYPART_HITS;
use rustedscreep_consts::{
    ATTACK_POWER, Boost, DISMANTLE_POWER, HEAL_POWER, Part, RANGED_ATTACK_POWER, RANGED_HEAL_POWER,
    RANGED_MASS_ATTACK_POWER_RANGE_1, RANGED_MASS_ATTACK_POWER_RANGE_2,
    RANGED_MASS_ATTACK_POWER_RANGE_3, ResourceType, RoomName,
};

use crate::config::DefenseConfig;
use crate::snapshot::{BodyPartRead, BodyRead, Controller, Snapshot};

/// A creep's combat power in boosted parts (module docs): each live part of an action's type counts
/// its boost's multiplier for that action, or 1 unboosted. Unit: boosted parts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Power {
    /// ATTACK parts, for `attack` (`BOOSTS` key `attack`).
    pub attack: u32,
    /// RANGED_ATTACK parts, for `rangedAttack` and `rangedMassAttack` (every compound gives both the
    /// same multiplier, V131, V288).
    pub ranged: u32,
    /// HEAL parts, for `heal` and `rangedHeal` (the same multiplier for both, V131).
    pub heal: u32,
    /// WORK parts, for `dismantle` (a WORK part boosted for another action counts 1).
    pub dismantle: u32,
    /// CLAIM parts, for `attackController` (no boost changes it, V288).
    pub claim: u32,
}

impl Power {
    /// Adds one live part's power.
    ///
    /// Uses: rustedscreep_consts::ResourceType::boost (the `BOOSTS` table, V131). A boost counts only
    /// for the part type and action the table lists it under, as the engine's
    /// `BOOSTS[type][boost][method]` lookup does (V286).
    pub fn add_part(&mut self, part: BodyPartRead) {
        let boost = part.boost.and_then(ResourceType::boost);
        match part.part {
            Part::Attack => {
                let m = match boost {
                    Some(Boost::Attack(m)) => m,
                    _ => 1,
                };
                self.attack = self.attack.saturating_add(m);
            }
            Part::RangedAttack => {
                let m = match boost {
                    Some(Boost::RangedAttack(m)) => m,
                    _ => 1,
                };
                self.ranged = self.ranged.saturating_add(m);
            }
            Part::Heal => {
                let m = match boost {
                    Some(Boost::Heal(m)) => m,
                    _ => 1,
                };
                self.heal = self.heal.saturating_add(m);
            }
            Part::Work => {
                let m = match boost {
                    Some(Boost::Dismantle(m)) => m,
                    _ => 1,
                };
                self.dismantle = self.dismantle.saturating_add(m);
            }
            Part::Claim => self.claim = self.claim.saturating_add(1),
            _ => {}
        }
    }

    /// Both creeps' power together (saturating).
    pub const fn plus(self, other: Self) -> Self {
        Self {
            attack: self.attack.saturating_add(other.attack),
            ranged: self.ranged.saturating_add(other.ranged),
            heal: self.heal.saturating_add(other.heal),
            dismantle: self.dismantle.saturating_add(other.dismantle),
            claim: self.claim.saturating_add(other.claim),
        }
    }

    /// Melee damage a tick to one target next to the creep. Unit: hit points.
    ///
    /// Uses: rustedscreep_consts::ATTACK_POWER (V288).
    pub const fn melee_damage(self) -> u32 {
        self.attack.saturating_mul(ATTACK_POWER)
    }

    /// `rangedAttack` damage a tick to one target within range 3. Unit: hit points.
    ///
    /// Uses: rustedscreep_consts::RANGED_ATTACK_POWER (V288).
    pub const fn ranged_damage(self) -> u32 {
        self.ranged.saturating_mul(RANGED_ATTACK_POWER)
    }

    /// `rangedMassAttack` damage a tick to each target at Chebyshev range `range` (0 for the creep's
    /// own tile counts as 1; nothing beyond 3). The engine rounds the total once, and every total is a
    /// multiple of 10, so this per-part product equals it (V288). Unit: hit points.
    ///
    /// Uses: rustedscreep_consts::{RANGED_MASS_ATTACK_POWER_RANGE_1, RANGED_MASS_ATTACK_POWER_RANGE_2,
    /// RANGED_MASS_ATTACK_POWER_RANGE_3}.
    pub const fn mass_damage(self, range: u32) -> u32 {
        let per_part = match range {
            0 | 1 => RANGED_MASS_ATTACK_POWER_RANGE_1,
            2 => RANGED_MASS_ATTACK_POWER_RANGE_2,
            3 => RANGED_MASS_ATTACK_POWER_RANGE_3,
            _ => 0,
        };
        self.ranged.saturating_mul(per_part)
    }

    /// `heal` a tick to one creep next to the healer. Unit: hit points.
    ///
    /// Uses: rustedscreep_consts::HEAL_POWER (V288).
    pub const fn heal_adjacent(self) -> u32 {
        self.heal.saturating_mul(HEAL_POWER)
    }

    /// `rangedHeal` a tick to one creep within range 3. Unit: hit points.
    ///
    /// Uses: rustedscreep_consts::RANGED_HEAL_POWER (V288).
    pub const fn heal_at_range(self) -> u32 {
        self.heal.saturating_mul(RANGED_HEAL_POWER)
    }

    /// `dismantle` damage a tick to one structure next to the creep. Unit: hit points.
    ///
    /// Uses: rustedscreep_consts::DISMANTLE_POWER (V288).
    pub const fn dismantle_damage(self) -> u32 {
        self.dismantle.saturating_mul(DISMANTLE_POWER)
    }

    /// Whether the creep can damage creeps (melee or ranged).
    pub const fn armed(self) -> bool {
        self.attack > 0 || self.ranged > 0
    }

    /// Whether the creep can harm the room: attack, ranged attack, dismantle, or attack the
    /// controller. Healing alone is not.
    pub const fn dangerous(self) -> bool {
        self.armed() || self.dismantle > 0 || self.claim > 0
    }
}

/// The hit points of part `index` (0 the first) of a body of `len` parts that has `hits` in all: the
/// engine fills the parts from the last to the first, up to `BODYPART_HITS` each (V283). Unit: hit
/// points.
///
/// Uses: rustedscreep_consts::extra::BODYPART_HITS (V282).
pub fn part_hits(len: usize, hits: u32, index: usize) -> u32 {
    let behind = len.saturating_sub(index.saturating_add(1));
    let filled_behind = u32::try_from(behind)
        .unwrap_or(u32::MAX)
        .saturating_mul(BODYPART_HITS);
    hits.saturating_sub(filled_behind).min(BODYPART_HITS)
}

/// The share of the damage reaching `part` that it takes: the `damage` ratio of a boosted TOUGH part
/// (V287), or `None` for a part that takes it all. The ratio comes from the table's `f32` as
/// hundredths, so it is the same double as the engine's literal (0.7, 0.5, or 0.3): widened from the
/// `f32` instead, it rounds a tie the other way (the pinned examples in the tests). A ratio farther
/// than the `f32`'s precision from a whole number of hundredths, which no compound has, is widened
/// as it is.
///
/// Uses: rustedscreep_consts::ResourceType::boost (`BOOSTS[tough][compound].damage`, V131).
fn damage_ratio(part: BodyPartRead) -> Option<f64> {
    if part.part != Part::Tough {
        return None;
    }
    match part.boost.and_then(ResourceType::boost) {
        Some(Boost::Tough(ratio)) => {
            // Within the f32's precision of a whole number of hundredths: the engine's decimal.
            let scaled = f64::from(ratio) * 100.0;
            let hundredths = scaled.round();
            let exact = if (scaled - hundredths).abs() < 1e-3 {
                hundredths / 100.0
            } else {
                f64::from(ratio)
            };
            (exact > 0.0 && exact < 1.0).then_some(exact)
        }
        _ => None,
    }
}

/// The hits a creep loses to `damage` taken in one tick, before its heal: the engine's `_applyDamage`
/// (V287). Damage walks the parts from the first; each part absorbs up to its hits over its ratio,
/// and a boosted TOUGH part saves `1 - ratio` of what it absorbs. The saving is summed and rounded
/// once. The arithmetic is the engine's, in the same order and in doubles, so the rounding agrees.
///
/// Uses: rustedscreep_consts::extra::BODYPART_HITS (through [`part_hits`]).
pub fn damage_taken(parts: &[BodyPartRead], hits: u32, damage: u32) -> u32 {
    let len = parts.len();
    if !parts.iter().any(|p| damage_ratio(*p).is_some()) {
        // With no part that has a ratio every saving is 0, as when the engine skips the loop.
        return damage;
    }
    let mut saved = 0.0f64;
    let mut left = f64::from(damage);
    for (index, &part) in parts.iter().enumerate() {
        if left <= 0.0 {
            break;
        }
        let ratio = damage_ratio(part).unwrap_or(1.0);
        let absorbs = f64::from(part_hits(len, hits, index)) / ratio;
        let taken = absorbs.min(left);
        saved += taken * (1.0 - ratio);
        left -= taken;
    }
    // `Math.round` of a sum that is never negative, so rounding half away from zero agrees.
    let lost = f64::from(damage) - saved.round();
    if lost <= 0.0 {
        0
    } else if lost >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        // In range and whole (an integer less a rounded value), so the cast is exact.
        lost as u32
    }
}

/// The least damage in one tick, with no heal, that kills a creep of these parts with `hits` in all
/// (module docs): the least `D` with [`damage_taken`]`(D) >= hits`. The taken damage never falls as
/// `D` rises (each unit adds at most `1 - 0.3` to the saving, so the rounded saving rises by at most
/// 1), so a binary search finds it. Once `D` covers every part's hits over its ratio (`absorb`), the
/// saving is `absorb - hits`, so `D = ceil(absorb) + 1` always kills. Unit: hit points.
///
/// Uses: rustedscreep_consts::extra::BODYPART_HITS (through [`part_hits`]).
pub fn effective_hits(parts: &[BodyPartRead], hits: u32) -> u32 {
    let len = parts.len();
    // Hits above the body's capacity cannot happen in the engine; cap them so the bound holds.
    let cap = u32::try_from(len)
        .unwrap_or(u32::MAX)
        .saturating_mul(BODYPART_HITS);
    let hits = hits.min(cap);
    let mut absorb = 0.0f64;
    let mut reduced = false;
    for (index, &part) in parts.iter().enumerate() {
        let part_hit = f64::from(part_hits(len, hits, index));
        match damage_ratio(part) {
            Some(ratio) => {
                reduced |= part_hit > 0.0;
                absorb += part_hit / ratio;
            }
            None => absorb += part_hit,
        }
    }
    if !reduced {
        return hits;
    }
    // `absorb` is at most `MAX_CREEP_SIZE * BODYPART_HITS / 0.3`, far inside u32.
    let mut high = (absorb.ceil() as u32).saturating_add(1);
    let mut low = hits;
    // The bound above always kills; should it not, widen it rather than answer low (an effective hits
    // figure too low is the unsafe direction for defense).
    let mut widenings = 0;
    while damage_taken(parts, hits, high) < hits && widenings < 8 {
        high = high.saturating_mul(2);
        widenings += 1;
    }
    while low < high {
        let mid = low + (high - low) / 2;
        if damage_taken(parts, hits, mid) >= hits {
            high = mid;
        } else {
            low = mid.saturating_add(1);
        }
    }
    low
}

/// One hostile creep in an owned room this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostileThreat {
    /// Its row in this tick's [`crate::snapshot::Creeps`].
    pub row: u32,
    /// What its live parts can do.
    pub power: Power,
    /// Unit: hit points.
    pub hits: u32,
    /// `body.length * BODYPART_HITS` (V282). Unit: hit points.
    pub hits_max: u32,
    /// [`effective_hits`]. Unit: hit points.
    pub effective_hits: u32,
    /// Whether any part, live or not, carries a boost compound the `BOOSTS` table lists.
    pub boosted: bool,
}

impl HostileThreat {
    /// Assesses a hostile creep from its body and hits (module docs).
    ///
    /// Uses: rustedscreep_consts::extra::BODYPART_HITS (V282, V283), ResourceType::boost.
    pub fn of(row: u32, body: &BodyRead, hits: u32) -> Self {
        let parts = body.parts();
        let len = parts.len();
        let mut power = Power::default();
        let mut boosted = false;
        for (index, &part) in parts.iter().enumerate() {
            boosted |= part.boost.and_then(ResourceType::boost).is_some();
            if part_hits(len, hits, index) > 0 {
                power.add_part(part);
            }
        }
        Self {
            row,
            power,
            hits,
            hits_max: u32::try_from(len)
                .unwrap_or(u32::MAX)
                .saturating_mul(BODYPART_HITS),
            effective_hits: effective_hits(parts, hits),
            boosted,
        }
    }
}

/// The hostile creeps in one owned room this tick, and their sums.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomThreat {
    pub room: RoomName,
    /// Where its hostiles start in [`Threats::hostiles`], and how many there are.
    start: u32,
    len: u32,
    /// Hostile creeps in the room, its unread ones included. Unit: creeps.
    pub hostiles: u32,
    /// Hostiles that can harm the room ([`Power::dangerous`]). Unit: creeps.
    pub dangerous: u32,
    /// Hostiles with a boosted part. Unit: creeps.
    pub boosted: u32,
    /// Hostiles whose body was not read (none while the snapshot reads every one). Unit: creeps.
    pub unread: u32,
    /// The hostiles' power together.
    pub power: Power,
    /// The hostiles' hits together. Unit: hit points.
    pub hits: u32,
    /// The hostiles' [`effective_hits`] together. Unit: hit points.
    pub effective_hits: u32,
}

/// What a room's threat did this tick ([`Threats::events`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreatEvent {
    /// A dangerous hostile is in the room, and no threat was open there: its [`RoomThreat`] is in
    /// [`Threats::rooms`].
    Started { room: RoomName },
    /// The room's counts of dangerous or boosted hostiles differ from the last line's, and that line
    /// is at least `defense.threat_log_ticks` old: its [`RoomThreat`] is in [`Threats::rooms`].
    Changed { room: RoomName, since: u32 },
    /// No dangerous hostile has been seen in the room for `defense.threat_end_ticks`: the threat that
    /// started at tick `since` and was last seen at tick `last` is over.
    Ended {
        room: RoomName,
        since: u32,
        last: u32,
    },
}

/// An open threat in one room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Watch {
    room: RoomName,
    /// The tick it started.
    since: u32,
    /// The last tick a dangerous hostile was seen.
    seen: u32,
    /// The tick of its last start or change line, and the counts that line gave.
    logged_at: u32,
    dangerous: u32,
    boosted: u32,
}

/// The tick's threats in owned rooms, and the open threats across ticks (module docs).
#[derive(Default)]
pub struct Threats {
    rooms: Vec<RoomThreat>,
    hostiles: Vec<HostileThreat>,
    watches: Vec<Watch>,
    events: Vec<ThreatEvent>,
    /// Scratch: this tick's assessed rows with their room, sorted by room.
    found: Vec<(RoomName, u32)>,
}

impl Threats {
    /// Starts a tick: clears last tick's rooms, hostiles, and events. [`Self::assess`] does it too.
    pub fn clear(&mut self) {
        self.rooms.clear();
        self.hostiles.clear();
        self.events.clear();
    }

    /// Whether anything is to do this tick: an assessed hostile, or an open threat to time out.
    pub fn needed(&self, snap: &Snapshot) -> bool {
        !snap.creeps.assessed().is_empty() || !self.watches.is_empty()
    }

    /// Assesses this tick's hostile creeps in owned rooms (the snapshot's assessed rows), then opens,
    /// updates, and ends the rooms' threats.
    pub fn assess(&mut self, snap: &Snapshot, cfg: &DefenseConfig) {
        self.clear();
        let now = snap.tick.time;
        let creeps = &snap.creeps;
        self.found.clear();
        for &row in creeps.assessed() {
            let Ok(index) = usize::try_from(row) else {
                continue;
            };
            if let Some(pos) = creeps.positions().get(index) {
                self.found.push((pos.room_name(), row));
            }
        }
        self.found.sort_unstable();
        // Owned rooms only: a mined remote room's hostiles are assessed too (P7.6), for the remote
        // defenders, which read them on their own.
        let mut mine: Option<(RoomName, bool)> = None;
        for &(room, row) in &self.found {
            let owned = match mine {
                Some((r, o)) if r == room => o,
                _ => {
                    let o = owned(snap, room);
                    mine = Some((room, o));
                    o
                }
            };
            if !owned {
                continue;
            }
            if self.rooms.last().is_none_or(|r| r.room != room) {
                self.rooms.push(RoomThreat {
                    room,
                    start: u32::try_from(self.hostiles.len()).unwrap_or(u32::MAX),
                    len: 0,
                    hostiles: 0,
                    dangerous: 0,
                    boosted: 0,
                    unread: 0,
                    power: Power::default(),
                    hits: 0,
                    effective_hits: 0,
                });
            }
            let Some(r) = self.rooms.last_mut() else {
                continue;
            };
            r.hostiles = r.hostiles.saturating_add(1);
            let Ok(index) = usize::try_from(row) else {
                continue;
            };
            let Some(body) = creeps.body(index) else {
                r.unread = r.unread.saturating_add(1);
                continue;
            };
            let hits = creeps.hits().get(index).copied().unwrap_or(0);
            let threat = HostileThreat::of(row, body, hits);
            r.len = r.len.saturating_add(1);
            r.dangerous = r
                .dangerous
                .saturating_add(u32::from(threat.power.dangerous()));
            r.boosted = r.boosted.saturating_add(u32::from(threat.boosted));
            r.power = r.power.plus(threat.power);
            r.hits = r.hits.saturating_add(threat.hits);
            r.effective_hits = r.effective_hits.saturating_add(threat.effective_hits);
            self.hostiles.push(threat);
        }
        self.track(now, cfg);
    }

    /// Opens, updates, and ends the rooms' threats from this tick's rooms (module docs).
    fn track(&mut self, now: u32, cfg: &DefenseConfig) {
        for r in &self.rooms {
            if r.dangerous == 0 {
                continue;
            }
            match self.watches.iter_mut().find(|w| w.room == r.room) {
                None => {
                    self.watches.push(Watch {
                        room: r.room,
                        since: now,
                        seen: now,
                        logged_at: now,
                        dangerous: r.dangerous,
                        boosted: r.boosted,
                    });
                    self.events.push(ThreatEvent::Started { room: r.room });
                }
                Some(w) => {
                    w.seen = now;
                    let changed = (w.dangerous, w.boosted) != (r.dangerous, r.boosted);
                    if changed && now.saturating_sub(w.logged_at) >= cfg.threat_log_ticks {
                        w.logged_at = now;
                        w.dangerous = r.dangerous;
                        w.boosted = r.boosted;
                        self.events.push(ThreatEvent::Changed {
                            room: r.room,
                            since: w.since,
                        });
                    }
                }
            }
        }
        let events = &mut self.events;
        self.watches.retain(|w| {
            let over = now.saturating_sub(w.seen) >= cfg.threat_end_ticks;
            if over {
                events.push(ThreatEvent::Ended {
                    room: w.room,
                    since: w.since,
                    last: w.seen,
                });
            }
            !over
        });
    }

    /// This tick's owned rooms with a hostile creep in them, in room order.
    pub fn rooms(&self) -> &[RoomThreat] {
        &self.rooms
    }

    /// This tick's threat in `room`, if a hostile creep is in it.
    pub fn room(&self, room: RoomName) -> Option<&RoomThreat> {
        self.rooms.iter().find(|r| r.room == room)
    }

    /// The assessed hostiles of `room` (those with a body read), in row order.
    pub fn hostiles(&self, room: &RoomThreat) -> &[HostileThreat] {
        let start = usize::try_from(room.start).unwrap_or(usize::MAX);
        let end = start.saturating_add(usize::try_from(room.len).unwrap_or(0));
        self.hostiles.get(start..end).unwrap_or(&[])
    }

    /// The tick the open threat in `room` started, if one is open.
    pub fn under_threat(&self, room: RoomName) -> Option<u32> {
        self.watches
            .iter()
            .find(|w| w.room == room)
            .map(|w| w.since)
    }

    /// Open threats. Unit: rooms.
    pub fn open(&self) -> usize {
        self.watches.len()
    }

    /// The rooms with an open threat, in the order the threats started.
    pub fn open_rooms(&self) -> impl Iterator<Item = RoomName> + '_ {
        self.watches.iter().map(|w| w.room)
    }

    /// This tick's events: starts and changes in room order, then ends in the order the threats
    /// started.
    pub fn events(&self) -> &[ThreatEvent] {
        &self.events
    }
}

/// Whether the bot owns `room` by this tick's snapshot.
fn owned(snap: &Snapshot, room: RoomName) -> bool {
    snap.rooms
        .row_of(room)
        .and_then(|r| snap.rooms.controllers().get(r))
        .is_some_and(|c| matches!(c, Controller::Mine { .. }))
}
