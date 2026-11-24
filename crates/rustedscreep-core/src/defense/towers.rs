//! Tower control (ROADMAP P7.2): focus fire, heal priority, and the combat sim behind them.
//!
//! The engine rules mirrored here are read at engine commit a90d26f: `processor/intents/towers/`
//! (`intents.js`, `attack.js`, `heal.js`), `processor/intents/creeps/tick.js` (`_applyDamage` and
//! the tick's order), `creeps/heal.js` and `rangedHeal.js` (V239, V278, V281, V287, V288, V295,
//! V296).
//!
//! # A tower's action
//!
//! A tower runs one action a tick: heal, else repair, else attack (`towers/intents.js`, V295), each
//! for `TOWER_ENERGY_COST` energy, and none with less (V281, V296). It reaches the whole room, and its
//! power falls off with range: full up to `TOWER_OPTIMAL_RANGE`, then linearly down to
//! `1 - TOWER_FALLOFF` of it at `TOWER_FALLOFF_RANGE` and beyond, floored once ([`tower_amount`];
//! V239 for attack, V296 for heal). Its damage on a creep lands in the creep's own tick with the
//! tick's other damage, through the creep's boosted TOUGH parts, and the tick's heal after it (V278,
//! V287). Power effects (PWR_OPERATE_TOWER, PWR_DISRUPT_TOWER) scale the amount (V299); the bot reads
//! no effects, so it models every tower at its plain power.
//!
//! # The combat sim
//!
//! [`kill_ticks`] plays a hostile's fight forward a tick at a time. Every tower with energy for the
//! shot fires at it from where it stands, and every hostile healer within range 3 of it heals it: at
//! range 1 or on itself with `heal`, at 2 or 3 with `rangedHeal`, the most each can give
//! ([`heal_on`]). A healer heals one creep a tick, so counting every one on the target is the
//! worst case for the bot. Each tick the damage goes through the target's boosted TOUGH parts as
//! its hits then stand ([`super::damage_taken`]), the heal lands, capped at its `hitsMax`, and the
//! creep dies once its hits reach 0. The sim takes the positions as they are this tick; a creep that
//! moves changes the ranges, and the sim runs again on the next tick. The heal is the healers' power
//! as their parts stand this tick: a healer that loses HEAL parts heals less later (the bot's
//! error is on the safe side), and one healed back regains them (it is not). In a room in the bot's
//! own safe mode no hostile can heal (V300, V330), so the sim counts no heal there
//! ([`super::safe_mode::protected`]).
//!
//! # Focus fire
//!
//! In a room with a hostile, the towers shoot one target: damage spread over two targets is healed
//! back on both. The target is the one the sim kills within `defense.max_kill_ticks`:
//! dangerous hostiles ([`super::Power::dangerous`]) before harmless ones, then the fewest ticks, then
//! the most heal, then the lowest id. A harmless healer within range 3 of that target goes first
//! when killing it and then the target, without its heal, takes fewer ticks than the target alone
//! through it (the classic attacker and healer pair). Last
//! tick's target is kept while the sim still kills it within the limit, unless it is harmless and a
//! dangerous one can be killed: switching would let the heal undo the damage done. With no target
//! the sim kills within the limit, the towers hold fire: shooting into heal that matches them only
//! drains them (the usual way an attacker empties towers before the real attack).
//!
//! Only the fewest towers that kill the target in the ticks all of them would take fire at it,
//! strongest at it first (`fewest`): one tower kills a scout as soon as six would, and each more
//! shot is a billed intent and `TOWER_ENERGY_COST` for nothing. When the target dies in one tick,
//! the spare towers take the other hostiles they kill in one tick, in the same order, the fewest
//! for each (`split`); the rest heal, as with no target.
//!
//! # The shot check
//!
//! After each shot the bot keeps the hits the sim expects its target to have on the next tick,
//! with the worst-case heal on it; a target the towers hit as modeled can only come out at or
//! below that. One that comes out above it on `defense.miss_ticks` ticks in a row is not hit as
//! modeled: a rampart on its tile takes the shots (V239, V281), a power effect weakens the towers
//! (V299), or a tower is inactive (V291, V297). The towers stop firing at it while the room's state
//! lives, with one `Distrust` event, so no such target drains them.
//!
//! # Heal priority
//!
//! Each tower not shooting that holds at least `defense.heal_reserve` heals a damaged own creep: in id order, each takes the creep with the most hits still missing after the heals of the
//! towers before it, at its range's power. Own creeps' hits are read only in rooms with an open
//! threat (the snapshot's choice), the only time hostiles damage them in an owned room.
//!
//! # Rooms and state
//!
//! [`TowerControl::begin`] lists this tick's rooms to plan: an owned room with a tower holding
//! energy for a shot, and a hostile creep in it, or a damaged own creep and a tower holding
//! `defense.heal_reserve`. [`TowerControl::plan_room`] fills the orders for one room. Each room keeps
//! its target's handle, whether it holds fire, and its last line, while it plans or its threat is
//! open, in the heap only: a global reset starts them again. A new target and a hold of fire are
//! events ([`TowerEvent`]), at most one per `defense.threat_log_ticks` per room, except that the
//! first one about danger (a dangerous target, or a hold) after one that was not always goes out,
//! so a hostile stepping in and out, or a kill swinging around the limit, cannot fill the log.

use rustedscreep_consts::{
    Position, RoomName, TOWER_ENERGY_COST, TOWER_FALLOFF, TOWER_FALLOFF_RANGE, TOWER_OPTIMAL_RANGE,
    TOWER_POWER_ATTACK, TOWER_POWER_HEAL,
};

use super::threat::{Power, Threats, damage_taken};
use crate::config::DefenseConfig;
use crate::scheduler::Fault;
use crate::snapshot::{BodyPartRead, Controller, CreepHandle, Snapshot};

/// A tower's power `power` (`TOWER_POWER_ATTACK`, `TOWER_POWER_HEAL`, or `TOWER_POWER_REPAIR`) at
/// Chebyshev range `range`: full up to `TOWER_OPTIMAL_RANGE`, then
/// `power - power * TOWER_FALLOFF * (min(range, TOWER_FALLOFF_RANGE) - TOWER_OPTIMAL_RANGE) /
/// (TOWER_FALLOFF_RANGE - TOWER_OPTIMAL_RANGE)`, floored once, in the engine's order of operations
/// (V239, V296). Unit: hit points.
///
/// Uses: rustedscreep_consts::{TOWER_OPTIMAL_RANGE, TOWER_FALLOFF_RANGE, TOWER_FALLOFF}.
pub fn tower_amount(power: u32, range: u32) -> u32 {
    let optimal = u32::from(TOWER_OPTIMAL_RANGE);
    if range <= optimal {
        return power;
    }
    let far = u32::from(TOWER_FALLOFF_RANGE);
    let r = range.min(far);
    let p = f64::from(power);
    let drop = p * TOWER_FALLOFF * f64::from(r - optimal) / f64::from(far - optimal);
    let amount = (p - drop).floor();
    if amount <= 0.0 {
        0
    } else {
        // At most `power`, and whole after the floor.
        amount as u32
    }
}

/// Chebyshev range between two tiles of one room (every caller passes a tower and a creep of the
/// room it plans); `u32::MAX` for tiles of two rooms, which [`heal_on`] reads as out of reach.
pub(super) fn range(a: Position, b: Position) -> u32 {
    if a.room_name() != b.room_name() {
        return u32::MAX;
    }
    let dx = a.x().u8().abs_diff(b.x().u8());
    let dy = a.y().u8().abs_diff(b.y().u8());
    u32::from(dx.max(dy))
}

/// One tower's fire at a target: its damage there, and the shots its energy pays for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shot {
    /// Unit: hit points a tick.
    pub damage: u32,
    /// Unit: ticks.
    pub shots: u32,
}

/// The heal hostile healers can put on a creep at `at` in one tick (module docs): each within range
/// 1 (the creep itself at range 0) at its `heal`, each at range 2 or 3 at its `rangedHeal`. Unit: hit
/// points.
///
/// Uses: rustedscreep_consts::{HEAL_POWER, RANGED_HEAL_POWER} (through [`Power`]).
pub fn heal_on(at: Position, healers: &[(Position, Power)]) -> u32 {
    healers.iter().fold(0u32, |sum, &(pos, power)| {
        let heal = match range(pos, at) {
            0 | 1 => power.heal_adjacent(),
            2 | 3 => power.heal_at_range(),
            _ => 0,
        };
        sum.saturating_add(heal)
    })
}

/// The ticks the towers' fire takes to kill a creep of these parts with `hits` of `hits_max`, healed
/// `heal` a tick (module docs), or `None` when it does not die within `limit` ticks. Each tick the
/// towers with a shot left fire; their damage goes through the creep's boosted TOUGH parts at its
/// hits then (`damage_taken`), the heal is added and capped at `hits_max`, and the creep dies once
/// its hits reach 0 (V287). A tick that leaves it no lower means it never dies: the next tick's hits,
/// `min(hits_max, hits - taken(hits) + heal)`, never fall as the hits before rise (one more hit
/// changes the taken damage by less than one, up to the saving's rounding), and the towers' damage
/// only falls as their shots run out, so no later tick does better.
///
/// Uses: rustedscreep_consts::{extra::BODYPART_HITS, ResourceType::boost} (`BOOSTS`, through
/// `damage_taken`).
pub fn kill_ticks(
    parts: &[BodyPartRead],
    hits: u32,
    hits_max: u32,
    towers: &[Shot],
    heal: u32,
    limit: u32,
) -> Option<u32> {
    let mut hits = hits;
    for tick in 1..=limit {
        let damage = towers
            .iter()
            .filter(|s| s.shots >= tick)
            .fold(0u32, |sum, s| sum.saturating_add(s.damage));
        if damage == 0 {
            return None;
        }
        let taken = damage_taken(parts, hits, damage);
        let after = i64::from(hits) - i64::from(taken) + i64::from(heal);
        if after <= 0 {
            return Some(tick);
        }
        let after = u32::try_from(after).unwrap_or(u32::MAX).min(hits_max);
        if after >= hits {
            return None;
        }
        hits = after;
    }
    None
}

/// What one tower does this tick: its row in this tick's `Structures`, and a creep row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TowerOrder {
    Attack { tower: u32, target: u32 },
    Heal { tower: u32, target: u32 },
}

/// What a room's towers started doing this tick, for the log ([`TowerControl::events`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TowerEvent {
    /// The towers took a new target: the creep at `target` (a row in this tick's `Creeps`), with
    /// `towers` towers for `damage` a tick against `heal`, dead in `ticks` by the sim.
    Fire {
        room: RoomName,
        target: u32,
        towers: u32,
        damage: u32,
        heal: u32,
        ticks: u32,
    },
    /// The towers hold fire: `dangerous` dangerous hostiles are in the room, and the sim kills none
    /// of them within `defense.max_kill_ticks`.
    Hold { room: RoomName, dangerous: u32 },
    /// The towers stop firing at the creep at `target`: its hits came out above the sim's for
    /// `misses` ticks in a row (a rampart on its tile, a power effect, an inactive tower).
    Distrust {
        room: RoomName,
        target: u32,
        misses: u32,
    },
}

/// One owned room's towers, kept between ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RoomState {
    room: RoomName,
    /// Last tick's target.
    focus: Option<CreepHandle>,
    /// Whether last tick held fire with a dangerous hostile in the room.
    holding: bool,
    /// The tick of the last plan; a room is dropped once it neither plans nor has an open threat.
    seen: u32,
    /// The tick of the room's last `Fire` or `Hold` event, and whether it was about danger (a
    /// dangerous target, or a hold), for the rate limit (module docs).
    line_at: Option<u32>,
    line_danger: bool,
    /// The focus target and the hits the sim expects it to have on the next tick, and the ticks in
    /// a row it has had more (the shot check, module docs).
    expect: Option<(CreepHandle, u32)>,
    misses: u32,
}

/// One tower this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TowerRef {
    row: u32,
    pos: Position,
    energy: u32,
    key: u128,
}

/// A target the sim kills within the limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Candidate {
    row: u32,
    handle: CreepHandle,
    pos: Position,
    hits: u32,
    hits_max: u32,
    dangerous: bool,
    ticks: u32,
    heal_power: u32,
    key: u128,
    damage: u32,
    heal: u32,
}

/// A damaged own creep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Damaged {
    row: u32,
    pos: Position,
    missing: u32,
    key: u128,
}

/// The rooms' towers: this tick's rooms to plan, their orders and events, and each room's target.
#[derive(Default)]
pub struct TowerControl {
    /// This tick's towers by room, then id.
    towers: Vec<(RoomName, TowerRef)>,
    /// This tick's rooms to plan, each with its range in `towers`.
    units: Vec<(RoomName, usize, usize)>,
    rooms: Vec<RoomState>,
    /// Creeps the towers no longer fire at, by room (the shot check), while the room's state lives.
    distrust: Vec<(RoomName, CreepHandle)>,
    orders: Vec<TowerOrder>,
    events: Vec<TowerEvent>,
    /// Scratch.
    armed: Vec<TowerRef>,
    healers: Vec<(Position, Power)>,
    others: Vec<(Position, Power)>,
    picks: Vec<Candidate>,
    shots: Vec<Shot>,
    candidates: Vec<Candidate>,
    damaged: Vec<Damaged>,
}

impl TowerControl {
    /// Starts the tick: lists the towers by room and the rooms to plan (module docs). With no
    /// hostile in an owned room and no own creep's hits read, there is nothing to plan.
    pub fn begin(&mut self, snap: &Snapshot, threats: &Threats, cfg: &DefenseConfig) {
        self.orders.clear();
        self.events.clear();
        self.towers.clear();
        self.units.clear();
        if threats.rooms().is_empty() && snap.creeps.own_hits_rows().is_empty() {
            return;
        }
        let st = &snap.structures;
        let tw = &snap.towers;
        for ((&row, &energy), &handle) in tw.rows().iter().zip(tw.energy()).zip(tw.handles()) {
            let Some(&pos) = usize::try_from(row)
                .ok()
                .and_then(|r| st.positions().get(r))
            else {
                continue;
            };
            let key = st.key_of(handle).map_or(u128::MAX, |k| k.0);
            self.towers.push((
                pos.room_name(),
                TowerRef {
                    row,
                    pos,
                    energy,
                    key,
                },
            ));
        }
        self.towers.sort_unstable_by_key(|&(room, t)| (room, t.key));
        let mut start = 0;
        while start < self.towers.len() {
            let Some(&(room, _)) = self.towers.get(start) else {
                break;
            };
            let end = start
                + self
                    .towers
                    .get(start..)
                    .unwrap_or(&[])
                    .iter()
                    .take_while(|(r, _)| *r == room)
                    .count();
            let room_towers = self.towers.get(start..end).unwrap_or(&[]);
            let armed = room_towers
                .iter()
                .any(|(_, t)| t.energy >= TOWER_ENERGY_COST);
            // A room with only damaged creeps needs a tower that may heal.
            let healer = room_towers
                .iter()
                .any(|(_, t)| t.energy >= cfg.heal_reserve.max(TOWER_ENERGY_COST));
            let owned = snap
                .rooms
                .row_of(room)
                .and_then(|r| snap.rooms.controllers().get(r))
                .is_some_and(|c| matches!(c, Controller::Mine { .. }));
            let wanted = threats.room(room).is_some() || (healer && damaged_in(snap, room));
            if armed && owned && wanted {
                self.units.push((room, start, end));
            }
            start = end.max(start + 1);
        }
    }

    /// This tick's rooms to plan.
    pub fn unit_count(&self) -> u32 {
        u32::try_from(self.units.len()).unwrap_or(u32::MAX)
    }

    /// The owned room of plan unit `unit`.
    pub fn unit_room(&self, unit: u32) -> Option<RoomName> {
        usize::try_from(unit)
            .ok()
            .and_then(|u| self.units.get(u))
            .map(|u| u.0)
    }

    /// Plans room `unit`'s towers (module docs): focus fire, else heals, else nothing.
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        threats: &Threats,
        cfg: &DefenseConfig,
    ) -> Result<(), Fault> {
        let &(room, start, end) = usize::try_from(unit)
            .ok()
            .and_then(|u| self.units.get(u))
            .ok_or(Fault::new("no tower room for the unit", unit))?;
        let now = snap.tick.time;
        let state_at = match self.rooms.iter().position(|r| r.room == room) {
            Some(i) => i,
            None => {
                self.rooms.push(RoomState {
                    room,
                    focus: None,
                    holding: false,
                    seen: now,
                    line_at: None,
                    line_danger: false,
                    expect: None,
                    misses: 0,
                });
                self.rooms.len() - 1
            }
        };
        let prev = self
            .rooms
            .get(state_at)
            .copied()
            .ok_or(Fault::new("tower room state missing", unit))?;
        // A scratch list taken out of `self` for the plan, and put back after it.
        let mut armed = std::mem::take(&mut self.armed);
        armed.clear();
        armed.extend(
            self.towers
                .get(start..end)
                .unwrap_or(&[])
                .iter()
                .map(|&(_, t)| t)
                .filter(|t| t.energy >= TOWER_ENERGY_COST),
        );
        // The shot check (module docs): last tick's target with more hits than the sim expected.
        let mut misses = 0;
        if let Some((h, expected)) = prev.expect
            && let Some(row) = snap.creeps.row_of(h)
            && snap
                .creeps
                .assessed()
                .contains(&u32::try_from(row).unwrap_or(u32::MAX))
        {
            let seen = snap.creeps.hits().get(row).copied().unwrap_or(0);
            if seen > expected {
                misses = prev.misses.saturating_add(1);
            }
            if misses >= cfg.miss_ticks {
                misses = 0;
                self.distrust.push((room, h));
                self.events.push(TowerEvent::Distrust {
                    room,
                    target: u32::try_from(row).unwrap_or(u32::MAX),
                    misses: cfg.miss_ticks,
                });
            }
        }
        let pick = self.choose_target(snap, threats, room, &armed, prev.focus, cfg);
        let dangerous = threats.room(room).map_or(0, |r| r.dangerous);
        let mut next = RoomState {
            focus: None,
            holding: false,
            seen: now,
            expect: None,
            misses,
            ..prev
        };
        // The rate limit on the room's lines (module docs): a window since the last, except that
        // the first line about danger after one that was not always goes out.
        let window = prev
            .line_at
            .is_none_or(|t| now.saturating_sub(t) >= cfg.threat_log_ticks);
        let allow = |danger: bool| window || (danger && !prev.line_danger);
        match pick {
            Some(c) => {
                let all = armed.len();
                let (n, damage) = self
                    .fewest(snap, &mut armed, &c, c.ticks, cfg)
                    .unwrap_or((all, 0));
                for t in armed.iter().take(n) {
                    self.orders.push(TowerOrder::Attack {
                        tower: t.row,
                        target: c.row,
                    });
                }
                next.focus = Some(c.handle);
                next.expect = self.expected(snap, &c, damage).map(|e| (c.handle, e));
                // A one-tick kill leaves the spare towers for other one-tick kills (module docs).
                let used = if c.ticks == 1 {
                    self.split(snap, &mut armed, n, c.handle, cfg)
                } else {
                    n
                };
                if prev.focus != Some(c.handle) && allow(c.dangerous) {
                    next.line_at = Some(now);
                    next.line_danger = c.dangerous;
                    self.events.push(TowerEvent::Fire {
                        room,
                        target: c.row,
                        towers: u32::try_from(n).unwrap_or(u32::MAX),
                        damage,
                        heal: c.heal,
                        ticks: c.ticks,
                    });
                }
                // The towers the kills do not need heal, as with no target.
                self.plan_heals(
                    snap,
                    room,
                    armed.get(used..).unwrap_or(&[]),
                    cfg.heal_reserve,
                );
            }
            None => {
                if dangerous > 0 {
                    next.holding = true;
                    if !prev.holding && allow(true) {
                        next.line_at = Some(now);
                        next.line_danger = true;
                        self.events.push(TowerEvent::Hold { room, dangerous });
                    }
                }
                self.plan_heals(snap, room, &armed, cfg.heal_reserve);
            }
        }
        self.armed = armed;
        if let Some(r) = self.rooms.get_mut(state_at) {
            *r = next;
        }
        Ok(())
    }

    /// The target the towers fire at, if the sim kills one within the limit (module docs).
    ///
    /// Uses: rustedscreep_consts::{TOWER_POWER_ATTACK, TOWER_ENERGY_COST}.
    fn choose_target(
        &mut self,
        snap: &Snapshot,
        threats: &Threats,
        room: RoomName,
        armed: &[TowerRef],
        focus: Option<CreepHandle>,
        cfg: &DefenseConfig,
    ) -> Option<Candidate> {
        let r = threats.room(room)?;
        let hostiles = threats.hostiles(r);
        let creeps = &snap.creeps;
        let position = |row: u32| {
            usize::try_from(row)
                .ok()
                .and_then(|i| creeps.positions().get(i))
                .copied()
        };
        self.healers.clear();
        // In the bot's own safe mode no hostile heals (module docs).
        let heals = !super::safe_mode::protected(snap, room);
        for h in hostiles {
            if heals
                && h.power.heal > 0
                && let Some(pos) = position(h.row)
            {
                self.healers.push((pos, h.power));
            }
        }
        self.candidates.clear();
        for h in hostiles {
            let Ok(index) = usize::try_from(h.row) else {
                continue;
            };
            let (Some(pos), Some(body), Some(&handle)) = (
                position(h.row),
                creeps.body(index),
                creeps.handles().get(index),
            ) else {
                continue;
            };
            if self.distrust.contains(&(room, handle)) {
                continue;
            }
            self.shots.clear();
            self.shots.extend(armed.iter().map(|t| Shot {
                damage: tower_amount(TOWER_POWER_ATTACK, range(t.pos, pos)),
                shots: t.energy / TOWER_ENERGY_COST,
            }));
            let heal = heal_on(pos, &self.healers);
            let Some(ticks) = kill_ticks(
                body.parts(),
                h.hits,
                h.hits_max,
                &self.shots,
                heal,
                cfg.max_kill_ticks,
            ) else {
                continue;
            };
            self.candidates.push(Candidate {
                row: h.row,
                handle,
                pos,
                hits: h.hits,
                hits_max: h.hits_max,
                dangerous: h.power.dangerous(),
                ticks,
                heal_power: h.power.heal,
                key: creeps.key_of(handle).map_or(u128::MAX, |k| k.0),
                damage: self.shots.iter().fold(0, |s, t| s.saturating_add(t.damage)),
                heal,
            });
        }
        let best = self
            .candidates
            .iter()
            .min_by_key(|c| {
                (
                    !c.dangerous,
                    c.ticks,
                    std::cmp::Reverse(c.heal_power),
                    c.key,
                )
            })
            .copied()?;
        let kept = focus
            .and_then(|h| self.candidates.iter().find(|c| c.handle == h))
            .copied()
            .filter(|c| c.dangerous || !best.dangerous);
        let pick = kept.unwrap_or(best);
        if !pick.dangerous {
            return Some(pick);
        }
        // A harmless healer within range 3 of a dangerous target goes first when its kill and then
        // the target's, without its heal, take fewer ticks than the target's through it.
        let Some(body) = usize::try_from(pick.row).ok().and_then(|r| creeps.body(r)) else {
            return Some(pick);
        };
        self.shots.clear();
        self.shots.extend(armed.iter().map(|t| Shot {
            damage: tower_amount(TOWER_POWER_ATTACK, range(t.pos, pick.pos)),
            shots: t.energy / TOWER_ENERGY_COST,
        }));
        let mut first: Option<(u32, Candidate)> = None;
        for h in &self.candidates {
            if h.dangerous || h.heal_power == 0 || range(h.pos, pick.pos) > 3 {
                continue;
            }
            self.others.clear();
            self.others
                .extend(self.healers.iter().copied().filter(|&(p, _)| p != h.pos));
            let without = heal_on(pick.pos, &self.others);
            let Some(after) = kill_ticks(
                body.parts(),
                pick.hits,
                pick.hits_max,
                &self.shots,
                without,
                cfg.max_kill_ticks,
            ) else {
                continue;
            };
            let total = h.ticks.saturating_add(after);
            if total < pick.ticks && first.is_none_or(|(t, f)| (total, h.key) < (t, f.key)) {
                first = Some((total, *h));
            }
        }
        Some(first.map_or(pick, |(_, h)| h))
    }

    /// The hits the sim expects `c` to have on the next tick after `damage` from the towers firing
    /// at it, or `None` when it dies (V287's order).
    fn expected(&self, snap: &Snapshot, c: &Candidate, damage: u32) -> Option<u32> {
        let body = usize::try_from(c.row)
            .ok()
            .and_then(|r| snap.creeps.body(r))?;
        let taken = damage_taken(body.parts(), c.hits, damage);
        let after = i64::from(c.hits) - i64::from(taken) + i64::from(c.heal);
        (after > 0).then(|| u32::try_from(after).unwrap_or(u32::MAX).min(c.hits_max))
    }

    /// Orders the spare towers (`armed` from `used` on) at the other candidates each kills in one
    /// tick, in the targets' order, the fewest towers for each; returns how many towers are used.
    fn split(
        &mut self,
        snap: &Snapshot,
        armed: &mut [TowerRef],
        mut used: usize,
        first: CreepHandle,
        cfg: &DefenseConfig,
    ) -> usize {
        let mut picks = std::mem::take(&mut self.picks);
        picks.clear();
        picks.extend(
            self.candidates
                .iter()
                .copied()
                .filter(|c| c.handle != first),
        );
        picks.sort_unstable_by_key(|c| {
            (
                !c.dangerous,
                c.ticks,
                std::cmp::Reverse(c.heal_power),
                c.key,
            )
        });
        for d in &picks {
            let Some(rest) = armed.get_mut(used..) else {
                break;
            };
            if rest.is_empty() {
                break;
            }
            let Some((k, _)) = self.fewest(snap, rest, d, 1, cfg) else {
                continue;
            };
            for t in rest.iter().take(k) {
                self.orders.push(TowerOrder::Attack {
                    tower: t.row,
                    target: d.row,
                });
            }
            used = used.saturating_add(k);
        }
        self.picks = picks;
        used
    }

    /// The fewest of `armed` that kill `c` in the sim's ticks (module docs), and their damage at it:
    /// sorts `armed` by damage at the target, strongest first, then id, and searches the shortest
    /// such prefix (one more tower never slows a kill, so the search is a binary one).
    fn fewest(
        &mut self,
        snap: &Snapshot,
        armed: &mut [TowerRef],
        c: &Candidate,
        ticks: u32,
        cfg: &DefenseConfig,
    ) -> Option<(usize, u32)> {
        let at = |t: &TowerRef| tower_amount(TOWER_POWER_ATTACK, range(t.pos, c.pos));
        armed.sort_unstable_by_key(|t| (std::cmp::Reverse(at(t)), t.key));
        let total = |k: usize| {
            armed
                .get(..k)
                .unwrap_or(&[])
                .iter()
                .fold(0u32, |sum, t| sum.saturating_add(at(t)))
        };
        let body = usize::try_from(c.row)
            .ok()
            .and_then(|r| snap.creeps.body(r))?;
        let kills = |k: usize, shots: &mut Vec<Shot>| {
            shots.clear();
            shots.extend(armed.get(..k).unwrap_or(&[]).iter().map(|t| Shot {
                damage: at(t),
                shots: t.energy / TOWER_ENERGY_COST,
            }));
            kill_ticks(
                body.parts(),
                c.hits,
                c.hits_max,
                shots,
                c.heal,
                cfg.max_kill_ticks,
            )
            .is_some_and(|t| t <= ticks)
        };
        let mut shots = std::mem::take(&mut self.shots);
        if !kills(armed.len(), &mut shots) {
            self.shots = shots;
            return None;
        }
        let (mut low, mut high) = (1usize, armed.len());
        while low < high {
            let mid = low + (high - low) / 2;
            if kills(mid, &mut shots) {
                high = mid;
            } else {
                low = mid.saturating_add(1);
            }
        }
        self.shots = shots;
        Some((low, total(low)))
    }

    /// Heals for the room's damaged own creeps by those of `towers` holding at least `reserve`
    /// (module docs).
    ///
    /// Uses: rustedscreep_consts::TOWER_POWER_HEAL.
    fn plan_heals(&mut self, snap: &Snapshot, room: RoomName, towers: &[TowerRef], reserve: u32) {
        if !towers.iter().any(|t| t.energy >= reserve) {
            return;
        }
        self.damaged.clear();
        let creeps = &snap.creeps;
        for &r in creeps.own_hits_rows() {
            let Ok(row) = usize::try_from(r) else {
                continue;
            };
            let (Some(&pos), Some(&hits), Some(&max), Some(&handle)) = (
                creeps.positions().get(row),
                creeps.hits().get(row),
                creeps.hits_max().get(row),
                creeps.handles().get(row),
            ) else {
                continue;
            };
            if pos.room_name() != room || hits >= max {
                continue;
            }
            self.damaged.push(Damaged {
                row: r,
                pos,
                missing: max - hits,
                key: creeps.key_of(handle).map_or(u128::MAX, |k| k.0),
            });
        }
        for t in towers.iter().filter(|t| t.energy >= reserve) {
            let Some(d) = self
                .damaged
                .iter_mut()
                .filter(|d| d.missing > 0)
                .max_by_key(|d| (d.missing, std::cmp::Reverse(d.key)))
            else {
                break;
            };
            d.missing = d
                .missing
                .saturating_sub(tower_amount(TOWER_POWER_HEAL, range(t.pos, d.pos)));
            self.orders.push(TowerOrder::Heal {
                tower: t.row,
                target: d.row,
            });
        }
    }

    /// Ends the tick: forgets the rooms that neither planned this tick nor have an open threat, so a
    /// later threat there starts with no target and no rate limit; a hostile stepping out and back
    /// keeps its room's target and limit.
    pub fn finish(&mut self, now: u32, threats: &Threats) {
        self.rooms
            .retain(|r| r.seen == now || threats.under_threat(r.room).is_some());
        let rooms = &self.rooms;
        self.distrust
            .retain(|(room, _)| rooms.iter().any(|r| r.room == *room));
    }

    /// This tick's orders, room by room.
    pub fn orders(&self) -> &[TowerOrder] {
        &self.orders
    }

    /// This tick's events.
    pub fn events(&self) -> &[TowerEvent] {
        &self.events
    }

    /// The towers in owned rooms this tick. Unit: towers.
    pub fn tower_count(&self) -> usize {
        self.towers.len()
    }

    /// The towers of `room` that hold energy for a shot this tick, as their position and energy
    /// (P7.4's kill times). Empty before [`Self::begin`] lists them, and on a tick it lists none.
    pub fn armed_in(&self, room: RoomName) -> impl Iterator<Item = (Position, u32)> + '_ {
        self.towers
            .iter()
            .filter(move |&&(r, t)| r == room && t.energy >= TOWER_ENERGY_COST)
            .map(|&(_, t)| (t.pos, t.energy))
    }

    /// The towers this tick's plan orders to attack the creep at row `target` of this tick's
    /// `Creeps`, as their position and energy (P7.4's claim trigger). Empty before the plan.
    pub fn firing_at(&self, target: u32) -> impl Iterator<Item = (Position, u32)> + '_ {
        self.orders
            .iter()
            .filter_map(move |o| match *o {
                TowerOrder::Attack { tower, target: t } if t == target => Some(tower),
                _ => None,
            })
            .filter_map(|row| self.towers.iter().find(|(_, t)| t.row == row))
            .map(|&(_, t)| (t.pos, t.energy))
    }

    /// Whether the towers of `room` no longer fire at `creep` (the shot check, module docs).
    pub fn distrusts(&self, room: RoomName, creep: CreepHandle) -> bool {
        self.distrust.contains(&(room, creep))
    }
}

/// Whether an own creep in `room` has lost hits (read only in rooms with an open threat).
fn damaged_in(snap: &Snapshot, room: RoomName) -> bool {
    let c = &snap.creeps;
    c.own_hits_rows().iter().any(|&r| {
        let row = usize::try_from(r).unwrap_or(usize::MAX);
        let (Some(p), Some(&h), Some(&m)) = (
            c.positions().get(row),
            c.hits().get(row),
            c.hits_max().get(row),
        ) else {
            return false;
        };
        p.room_name() == room && h < m
    })
}
