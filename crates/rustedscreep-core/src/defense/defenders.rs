//! Scaled defenders (ROADMAP P7.5, ADR-036): ranged creeps that an owned room spawns only when its
//! towers cannot win a fight, as many as P7.2's combat sim says, and that shoot from inside the
//! rampart line.
//!
//! # When a room asks for defenders
//!
//! In each owned room with a dangerous hostile this tick ([`super::Power::dangerous`]), the bot
//! judges every dangerous hostile the defenders can reach (see "Where defenders stand") with the
//! towers' sim ([`super::kill_ticks`]): every tower with energy for a shot fires from where it
//! stands, and every hostile healer within range 3 heals the hostile at its most
//! ([`super::heal_on`]; none in the bot's own safe mode, V300, V330). A hostile the towers kill
//! within `defense.max_kill_ticks` needs no defender: the towers win alone, and no energy goes on
//! it. A hostile the towers do not kill in that time needs defenders, as does one they distrust
//! (the shot check: they no longer fire at it, so it counts with no tower) and any hostile in a room
//! with no tower that holds energy for a shot.
//!
//! # How many, and how large
//!
//! A defender's body is MOVE and RANGED_ATTACK parts in equal numbers, the most whole pairs the
//! serving room's capacity pays for ([`body_for`]). The MOVE parts come first: damage empties the
//! first parts first (V283), so the weapons outlast the legs. Each live RANGED_ATTACK part deals
//! `RANGED_ATTACK_POWER` a tick at range 1 to 3 (V288, V339).
//!
//! For each hostile that needs defenders, [`least_power`] finds the fewest RANGED_ATTACK parts
//! that, firing with the towers each tick, kill it within `defense.max_kill_ticks` by the sim. The
//! room needs the most of these. A hostile that needs more parts than `defense.max_defenders`
//! bodies hold is beyond the defenders: it is logged once a threat
//! ([`DefenderEvent::Outmatched`]), and only the towers and safe mode (P7.4) answer it. The room
//! keeps the most parts it needed since its threat started, so a hostile stepping in and out of
//! reach does not stop and start the requests.
//!
//! While the room's living defenders (wherever they stand; a spawning one counts its full body)
//! hold fewer parts than that, and fewer than `defense.max_defenders` live, the room requests the
//! missing bodies at [`PRIORITY_DEFENDER`]: above every economy role but the zero-creep recovery
//! worker. Each request asks for the exact body at the serving room's capacity, so it waits until
//! that room holds the cost (the haulers fill it meanwhile).
//!
//! # Where defenders stand
//!
//! A defender never leaves the rampart line. The tiles inside it come from a flood from the room's
//! spawns over the movement grid's walkable tiles ([`crate::movement::CostGrid`]): walls and other
//! obstacles are blocked there, and an own rampart is taken in but not passed (it is part of the
//! line, or it covers a structure inside). The flood steps in all 8 directions, as a creep does (a
//! move is checked only against its destination tile, V244), so by construction no inside tile
//! touches a walkable tile outside, whatever the line's thickness: a melee hostile outside cannot
//! reach a defender on an inside tile, while the defender shoots 3 tiles. A rampart taken in does
//! touch the outside; a hit aimed at a creep on it lands on the rampart (V243, V288). The bot never
//! makes a rampart public, which would let hostiles through it (V313). A line with a gap
//! lets the flood out, and the whole room counts as inside; a room with no spawn has no line, and
//! every walkable tile counts. Movement builds a room's grid only while an own creep stands there,
//! so a room left with no creep has none: every hostile then counts as in reach, and the requests
//! still go out. Its first defender brings the grid, and posts, from the next tick.
//!
//! A hostile is in reach when a tile inside lies within range 3 of it. Each defender takes a post
//! within range 3 of the room's target: a tile inside that no hostile stands on and no other
//! defender took this tick. It prefers, in order: a tile out of melee reach (no hostile with an
//! ATTACK part within range 2, one step plus a hit); an own rampart, since a hit aimed at a creep
//! on one lands on the rampart (V243, V288, V340); the tile nearest to it; the tile farthest from
//! the target; the lowest tile index. A tile another defender of the room stands on is not taken,
//! and a defender keeps its last post while it is still free, in range and out of melee reach, so
//! its move's goal (and the flow field movement built for it) stays the same.
//!
//! # What defenders shoot
//!
//! The room's target is the towers' focus this tick when the defenders can reach it, so the two
//! never split their fire (damage spread over two targets is healed back on both). With no focus
//! it is last tick's target while it stays in reach, dangerous, and killable within
//! `defense.max_kill_ticks` by the towers and every defender in the room; else the reachable
//! dangerous hostile the sim kills soonest with them, then the one with the fewest effective hits,
//! then the lowest id. Each defender
//! within range 3 of the target shoots it; one that is not shoots the dangerous hostile within its
//! reach with the fewest hits. The towers' sim counts each defender in its home within range 3 of
//! a candidate as firing at it ([`super::TowerControl::set_allies`]): a defender in its home shoots
//! the towers' focus whenever it stands in range of it, before anything else and even when the
//! focus is harmless (a healer left after its attacker died), so that count holds. A defender never shoots an own creep
//! (the game would let it, V339, V340; the intent buffer refuses one).
//!
//! # Defenders with nothing to do
//!
//! A defender outside its home walks home. One in a home with no reachable target holds where it
//! stands and gives way to any creep that needs its tile, so it never blocks a gate. With no
//! hostile in any owned room and every defender home, they all do so outside the scheduler's jobs
//! ([`Defenders::hold_all`]), with no clock reading. Defenders are not recycled: once spawned they
//! serve their home for their whole life.
//!
//! # State
//!
//! Each room keeps, while its threat is open, the parts it needed, what its last line said, and
//! this tick's target; each room's inside tiles are cached by the movement grid's version and the
//! tiles of its ramparts and spawns, and dropped with its threat. All heap only: a global reset
//! forgets them, and an ongoing threat sizes its team again from the next tick.

use rustedscreep_consts::{
    CREEP_RANGED_ACTION_RANGE, Part, Position, RANGED_ATTACK_POWER, ROOM_AREA, ROOM_SIZE,
    RoomCoordinate, RoomName, StructureType, TOWER_ENERGY_COST, TOWER_POWER_ATTACK,
};

use super::safe_mode::protected;
use super::threat::{HostileThreat, Power, Threats, part_hits};
use super::towers::{Shot, TowerControl, heal_on, kill_ticks, range, tower_amount};
use crate::config::{DefenseConfig, MovementConfig};
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{CostGrid, MoveTo, Movement, Tile};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{BodyPartRead, CreepHandle, Snapshot};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner};

/// A defender's spawn priority (ADR-036): above every economy role but the zero-creep recovery
/// worker (`economy::opening::PRIORITY_RECOVERY`, 200). So above a worker below its room's minimum
/// (150), the miners (125), the haulers, the upgraders and the remote roles.
pub const PRIORITY_DEFENDER: u8 = 175;

/// The traffic priority of a defender's moves: above every economy role's (the miners' 4).
const MOVE_PRIORITY: u8 = 5;

/// A defender's body pattern, each part repeated in place: the MOVE parts, then the RANGED_ATTACK
/// parts (module docs).
pub const PATTERN: [Part; 2] = [Part::Move, Part::RangedAttack];

/// How far a ranged attack reaches (V339). Unit: tiles.
const REACH: u8 = CREEP_RANGED_ACTION_RANGE;

/// A hostile with an ATTACK part this close to a tile can hit a creep there now or after one step.
/// Unit: tiles.
const MELEE_LEAD: u8 = 2;

/// The range a creep walking home must reach from its home's center (25, 25): any tile but the
/// edges. Unit: tiles.
const HOME_RANGE: u8 = 23;

/// Inside tiles (module docs): not inside, inside, an own rampart taken in, and (during the flood
/// only) an own rampart not reached yet.
pub(super) const OUTSIDE: u8 = 0;
pub(super) const INSIDE: u8 = 1;
pub(super) const ON_RAMPART: u8 = 2;
const UNREACHED_RAMPART: u8 = 3;

/// The defender body a room of `capacity` spawns (module docs), or `None` below one pair's cost.
///
/// Uses: rustedscreep_consts::{BODYPART_COST, MAX_CREEP_SIZE} (through [`Body::scaled`]).
pub fn body_for(capacity: u32) -> Option<Body> {
    Body::scaled(&PATTERN, capacity, u8::MAX)
}

/// The fewest unboosted RANGED_ATTACK parts within range that, firing with `towers` each tick, kill
/// a creep of these parts with `hits` of `hits_max`, healed `heal` a tick, within `limit` ticks by
/// [`kill_ticks`]; `None` when even `most` parts do not. More parts never slow a kill (the sim's
/// damage only grows with them), so the search is a binary one. `shots` is scratch.
///
/// Uses: rustedscreep_consts::RANGED_ATTACK_POWER (V288).
#[allow(clippy::too_many_arguments)]
pub fn least_power(
    parts: &[BodyPartRead],
    hits: u32,
    hits_max: u32,
    towers: &[Shot],
    heal: u32,
    limit: u32,
    most: u32,
    shots: &mut Vec<Shot>,
) -> Option<u32> {
    let mut kills = |p: u32| {
        shots.clear();
        shots.extend_from_slice(towers);
        shots.push(Shot {
            damage: p.saturating_mul(RANGED_ATTACK_POWER),
            shots: u32::MAX,
        });
        kill_ticks(parts, hits, hits_max, shots, heal, limit).is_some()
    };
    if most == 0 || !kills(most) {
        return None;
    }
    let (mut low, mut high) = (1u32, most);
    while low < high {
        let mid = low + (high - low) / 2;
        if kills(mid) {
            high = mid;
        } else {
            low = mid.saturating_add(1);
        }
    }
    Some(low)
}

/// The inside tiles of a room (module docs): [`OUTSIDE`], [`INSIDE`] or [`ON_RAMPART`] by tile
/// index. A flood from the walkable neighbors of `starts` (the spawns' tiles) over `grid`'s walkable
/// tiles, taking in but not passing the walkable tiles of `ramparts`; with no start, every walkable
/// tile, the ramparts' marked. `queue` is scratch.
pub fn flood(
    grid: &CostGrid,
    ramparts: &[Tile],
    starts: &[Tile],
    out: &mut [u8; ROOM_AREA],
    queue: &mut Vec<Tile>,
) {
    if starts.is_empty() {
        for (index, cell) in out.iter_mut().enumerate() {
            *cell = match Tile::from_index(index) {
                Some(t) if grid.walkable(t) => INSIDE,
                _ => OUTSIDE,
            };
        }
        for &t in ramparts {
            if let Some(cell) = out.get_mut(t.index())
                && *cell == INSIDE
            {
                *cell = ON_RAMPART;
            }
        }
        return;
    }
    out.fill(OUTSIDE);
    for &t in ramparts {
        if let Some(cell) = out.get_mut(t.index()) {
            *cell = UNREACHED_RAMPART;
        }
    }
    queue.clear();
    let visit = |n: Tile, out: &mut [u8; ROOM_AREA], queue: &mut Vec<Tile>| {
        if !grid.walkable(n) {
            return;
        }
        let Some(cell) = out.get_mut(n.index()) else {
            return;
        };
        match *cell {
            OUTSIDE => {
                *cell = INSIDE;
                queue.push(n);
            }
            UNREACHED_RAMPART => *cell = ON_RAMPART,
            _ => {}
        }
    };
    for &s in starts {
        for (_, n) in s.neighbors() {
            visit(n, out, queue);
        }
    }
    while let Some(t) = queue.pop() {
        for (_, n) in t.neighbors() {
            visit(n, out, queue);
        }
    }
    for cell in out.iter_mut() {
        if *cell == UNREACHED_RAMPART {
            *cell = OUTSIDE;
        }
    }
}

/// Whether a tile of `inside` lies within range 3 of `at` (module docs).
fn in_reach(inside: &[u8; ROOM_AREA], at: Position) -> bool {
    tiles_near(Tile::of(at), REACH).any(|t| inside.get(t.index()).is_some_and(|&c| c != OUTSIDE))
}

/// The tiles within Chebyshev range `r` of `t`, in tile order by column then row.
fn tiles_near(t: Tile, r: u8) -> impl Iterator<Item = Tile> {
    let last = ROOM_SIZE.saturating_sub(1);
    let (x0, x1) = (t.x().saturating_sub(r), t.x().saturating_add(r).min(last));
    let (y0, y1) = (t.y().saturating_sub(r), t.y().saturating_add(r).min(last));
    (x0..=x1).flat_map(move |x| (y0..=y1).filter_map(move |y| Tile::new(x, y)))
}

/// `t` in `room` as a position.
fn at(room: RoomName, t: Tile) -> Option<Position> {
    Some(Position::new(
        RoomCoordinate::new(t.x()).ok()?,
        RoomCoordinate::new(t.y()).ok()?,
        room,
    ))
}

/// The `rangedAttack` power of an own creep's live parts, in boosted parts ([`Power::ranged`]):
/// from its body, read once, and its hits where the snapshot reads them (rooms with an open threat
/// and a tower), else as if every part were whole (in a room with no tower, where no tower's sim
/// counts it). `None` while its body is not known.
///
/// Uses: rustedscreep_consts::extra::BODYPART_HITS (through `part_hits`, V282, V283).
fn live_ranged(snap: &Snapshot, row: usize) -> Option<u32> {
    let creeps = &snap.creeps;
    let body = creeps.body(row)?;
    let parts = body.parts();
    let read = u32::try_from(row)
        .ok()
        .is_some_and(|r| creeps.own_hits_rows().binary_search(&r).is_ok());
    let full = u32::try_from(parts.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(rustedscreep_consts::extra::BODYPART_HITS);
    let hits = if read {
        creeps.hits().get(row).copied().unwrap_or(full)
    } else {
        full
    };
    let mut power = Power::default();
    for (index, &part) in parts.iter().enumerate() {
        if part_hits(parts.len(), hits, index) > 0 {
            power.add_part(part);
        }
    }
    Some(power.ranged)
}

/// How a post ranks (module docs), lower first: exposed to melee, not a rampart, the walk to it,
/// how much nearer than range 3 it stands to the target, and the tile index.
type PostScore = (bool, bool, u8, u8, usize);

/// One of the bot's defenders this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Member {
    home: RoomName,
    row: u32,
    handle: CreepHandle,
    key: u128,
    pos: Position,
    spawning: bool,
    /// Its live `rangedAttack` power in boosted parts; `None` while spawning or its body is unread.
    power: Option<u32>,
}

/// What the room's defenders did this tick, for the log ([`Defenders::events`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefenderEvent {
    /// The room needs more defender parts than its last line said: `parts` RANGED_ATTACK parts in
    /// all, in `defenders` bodies of `per_body` parts costing `cost` energy each, to kill the
    /// hostile at `target` (a row in this tick's `Creeps`) with the towers in `ticks` ticks by the
    /// sim; `have` defenders live. With `defense.defenders` off, nothing is requested.
    Need {
        room: RoomName,
        target: u32,
        parts: u32,
        defenders: u32,
        per_body: u32,
        cost: u32,
        ticks: u32,
        have: u32,
    },
    /// The hostile at `target` survives the towers and `defense.max_defenders` defenders of
    /// `per_body` parts by the sim: no defender is requested for it (once per threat).
    Outmatched {
        room: RoomName,
        target: u32,
        defenders: u32,
        per_body: u32,
    },
}

/// One owned room's defense with defenders, kept while its threat is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RoomState {
    room: RoomName,
    /// The tick its threat started ([`Threats::under_threat`]); a new threat starts afresh.
    since: u32,
    /// The most RANGED_ATTACK parts it needed in this threat.
    need: u32,
    /// The parts its last `Need` line gave.
    logged: u32,
    /// Whether this threat's `Outmatched` line went out.
    outmatched: bool,
    /// This tick's target, and the tick it was planned.
    target: Option<(CreepHandle, Position)>,
    planned: u32,
}

/// A room's inside tiles (module docs), cached by the grid's version and its ramparts and spawns.
struct Inside {
    room: RoomName,
    version: u32,
    key: u64,
    tiles: Box<[u8; ROOM_AREA]>,
}

/// The defenders' plan and state for one global.
#[derive(Default)]
pub struct Defenders {
    /// This tick's defenders, by home then id.
    members: Vec<Member>,
    /// This tick's defenders not spawning, with live RANGED_ATTACK parts: position and damage a tick.
    allies: Vec<(Position, u32)>,
    /// This tick's rooms to plan: owned rooms with a dangerous hostile.
    units: Vec<RoomName>,
    rooms: Vec<RoomState>,
    inside: Vec<Inside>,
    events: Vec<DefenderEvent>,
    /// This tick's spawn requests. Unit: requests.
    requested: u32,
    /// Posts taken this tick.
    taken: Vec<(RoomName, Tile)>,
    /// Each defender's last post, kept while it stays a valid one (so its move's goal, and the
    /// flow field movement built for it, stays the same).
    last: Vec<(CreepHandle, Tile)>,
    /// Scratch.
    armed: Vec<(Position, u32)>,
    healers: Vec<(Position, Power)>,
    shots: Vec<Shot>,
    scratch: Vec<Shot>,
    queue: Vec<Tile>,
    ramparts: Vec<Tile>,
    starts: Vec<Tile>,
}

impl Defenders {
    /// Starts the tick: lists the bot's defenders and the rooms to plan (owned rooms with a
    /// dangerous hostile). Call after the threat assessment, before the towers' plan, which reads
    /// [`Self::allies`].
    pub fn begin(&mut self, snap: &Snapshot, threats: &Threats) {
        self.members.clear();
        self.allies.clear();
        self.units.clear();
        self.events.clear();
        self.taken.clear();
        self.requested = 0;
        let creeps = &snap.creeps;
        // Power matters only with a hostile in an owned room (the towers' sim and the plan).
        let fight = !threats.rooms().is_empty();
        for (row, identity) in creeps.identities().iter().enumerate() {
            let Some(id) = identity.filter(|id| id.role == Role::Defender) else {
                continue;
            };
            if !creeps.my().get(row).copied().unwrap_or(false) {
                continue;
            }
            let (Some(&handle), Some(&pos), Ok(r)) = (
                creeps.handles().get(row),
                creeps.positions().get(row),
                u32::try_from(row),
            ) else {
                continue;
            };
            let spawning = creeps.spawning().get(row).copied().unwrap_or(true);
            let power = if spawning || !fight {
                None
            } else {
                live_ranged(snap, row)
            };
            // Only a defender in its home shoots the towers' focus (`work`), so only it counts in
            // their sim.
            if let Some(p) = power.filter(|&p| p > 0)
                && pos.room_name() == id.home
            {
                self.allies
                    .push((pos, p.saturating_mul(RANGED_ATTACK_POWER)));
            }
            self.members.push(Member {
                home: id.home,
                row: r,
                handle,
                key: creeps.key_of(handle).map_or(u128::MAX, |k| k.0),
                pos,
                spawning,
                power,
            });
        }
        self.members
            .sort_unstable_by_key(|m| (m.home.packed_repr(), m.key));
        let members = &self.members;
        self.last
            .retain(|&(h, _)| members.iter().any(|m| m.handle == h));
        self.units.extend(
            threats
                .rooms()
                .iter()
                .filter(|r| r.dangerous > 0)
                .map(|r| r.room),
        );
    }

    /// This tick's defenders not spawning, as their position and `rangedAttack` damage a tick, for
    /// the towers' sim ([`TowerControl::set_allies`]).
    pub fn allies(&self) -> &[(Position, u32)] {
        &self.allies
    }

    /// This tick's rooms to plan.
    pub fn unit_count(&self) -> u32 {
        u32::try_from(self.units.len()).unwrap_or(u32::MAX)
    }

    /// This tick's defenders, one work unit each.
    pub fn member_count(&self) -> u32 {
        u32::try_from(self.members.len()).unwrap_or(u32::MAX)
    }

    /// Plans room `unit` (module docs): its need, its spawn requests, its events and this tick's
    /// target. Run after the towers' plan (it reads their focus and distrust) and before the spawn
    /// plan. A room with no movement grid (module docs) counts every hostile as in reach.
    ///
    /// Uses: rustedscreep_consts::{TOWER_POWER_ATTACK, TOWER_ENERGY_COST, RANGED_ATTACK_POWER}.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        threats: &Threats,
        towers: &TowerControl,
        movement: &mut Movement,
        spawner: &mut Spawner,
        cfg: &DefenseConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let room = usize::try_from(unit)
            .ok()
            .and_then(|u| self.units.get(u))
            .copied()
            .ok_or(Fault::new("no defender room for the unit", unit))?;
        let Some(r) = threats.room(room) else {
            return Ok(());
        };
        let now = snap.tick.time;
        let since = threats.under_threat(room).unwrap_or(now);
        // `None` while the room has no grid: every hostile then counts as in reach (module docs).
        let inside = self.ensure_inside(snap, movement, room, mcfg, commit);
        let state_at = match self.rooms.iter().position(|s| s.room == room) {
            Some(i) => i,
            None => {
                self.rooms.push(RoomState {
                    room,
                    since,
                    need: 0,
                    logged: 0,
                    outmatched: false,
                    target: None,
                    planned: now,
                });
                self.rooms.len() - 1
            }
        };
        let mut state = self
            .rooms
            .get(state_at)
            .copied()
            .ok_or(Fault::new("defender room state missing", unit))?;
        if state.since != since {
            state = RoomState {
                room,
                since,
                need: 0,
                logged: 0,
                outmatched: false,
                target: None,
                planned: now,
            };
        }
        let body = spawner
            .spawn_side(room)
            .and_then(|side| body_for(side.energy.capacity));
        let per_body = body.map_or(0, |b| {
            u32::try_from(b.count(Part::RangedAttack)).unwrap_or(0)
        });
        let most = per_body.saturating_mul(cfg.max_defenders);
        let creeps = &snap.creeps;
        let position = |row: u32| {
            usize::try_from(row)
                .ok()
                .and_then(|i| creeps.positions().get(i))
                .copied()
        };
        let hostiles = threats.hostiles(r);
        let Self {
            armed,
            healers,
            shots,
            scratch,
            inside: insides,
            ..
        } = self;
        let tiles = inside.and_then(|i| insides.get(i)).map(|i| &*i.tiles);
        let reachable = |pos: Position| tiles.is_none_or(|t| in_reach(t, pos));
        armed.clear();
        armed.extend(towers.armed_in(room));
        healers.clear();
        // In the bot's own safe mode no hostile heals (V300, V330).
        if !protected(snap, room) {
            for h in hostiles {
                if h.power.heal > 0
                    && let Some(pos) = position(h.row)
                {
                    healers.push((pos, h.power));
                }
            }
        }
        // The team: every defender standing in the room, for the target with no tower focus.
        let team = self
            .members
            .iter()
            .filter(|m| m.pos.room_name() == room)
            .filter_map(|m| m.power)
            .fold(0u32, |s, p| {
                s.saturating_add(p.saturating_mul(RANGED_ATTACK_POWER))
            });
        let tower_shots = |shots: &mut Vec<Shot>, pos: Position, handle: CreepHandle| {
            shots.clear();
            if !towers.distrusts(room, handle) {
                shots.extend(armed.iter().map(|&(p, energy)| Shot {
                    damage: tower_amount(TOWER_POWER_ATTACK, range(p, pos)),
                    shots: energy / TOWER_ENERGY_COST,
                }));
            }
        };
        let mut need_now = 0u32;
        let mut worst: Option<(u32, u32)> = None;
        let mut beyond: Option<u32> = None;
        // The room's target with no focus: (ticks, effective hits, key, handle, pos).
        let mut best: Option<(u32, u32, u128, CreepHandle, Position)> = None;
        // Last tick's target, kept while still in reach, dangerous, and killable by the towers and
        // the team (as the towers keep their focus): a switch lets the heal undo the damage done.
        let previous = state.target.map(|t| t.0);
        let mut kept: Option<(CreepHandle, Position)> = None;
        for h in hostiles {
            if !h.power.dangerous() {
                continue;
            }
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
            if !reachable(pos) {
                continue;
            }
            tower_shots(shots, pos, handle);
            let heal = heal_on(pos, healers);
            let alone = kill_ticks(
                body.parts(),
                h.hits,
                h.hits_max,
                shots,
                heal,
                cfg.max_kill_ticks,
            );
            // The target with no focus: the soonest kill with the towers and the whole team.
            if team > 0 {
                shots.push(Shot {
                    damage: team,
                    shots: u32::MAX,
                });
            }
            let with_team = kill_ticks(
                body.parts(),
                h.hits,
                h.hits_max,
                shots,
                heal,
                cfg.max_kill_ticks,
            )
            .unwrap_or(u32::MAX);
            let key = creeps.key_of(handle).map_or(u128::MAX, |k| k.0);
            let score = (with_team, h.effective_hits, key, handle, pos);
            if best.is_none_or(|b| (score.0, score.1, score.2) < (b.0, b.1, b.2)) {
                best = Some(score);
            }
            if previous == Some(handle) && with_team != u32::MAX {
                kept = Some((handle, pos));
            }
            if alone.is_some() {
                continue;
            }
            tower_shots(shots, pos, handle);
            match least_power(
                body.parts(),
                h.hits,
                h.hits_max,
                shots,
                heal,
                cfg.max_kill_ticks,
                most,
                scratch,
            ) {
                Some(p) => {
                    if p > need_now {
                        need_now = p;
                        shots.push(Shot {
                            damage: p.saturating_mul(RANGED_ATTACK_POWER),
                            shots: u32::MAX,
                        });
                        let ticks = kill_ticks(
                            body.parts(),
                            h.hits,
                            h.hits_max,
                            shots,
                            heal,
                            cfg.max_kill_ticks,
                        )
                        .unwrap_or(cfg.max_kill_ticks);
                        worst = Some((h.row, ticks));
                    }
                }
                None => {
                    if beyond.is_none() {
                        beyond = Some(h.row);
                    }
                }
            }
        }
        // The towers' focus when the defenders reach it, else the soonest kill (module docs).
        let focus = towers.focus(room, now).and_then(|h| {
            let pos = creeps.row_of(h).and_then(|i| creeps.positions().get(i))?;
            reachable(*pos).then_some((h, *pos))
        });
        state.target = focus.or(kept).or(best.map(|b| (b.3, b.4)));
        state.planned = now;
        state.need = state.need.max(need_now);
        // The room's living defenders, a spawning one at its full body.
        let (have, have_parts) =
            self.members
                .iter()
                .filter(|m| m.home == room)
                .fold((0u32, 0u32), |(n, p), m| {
                    (
                        n.saturating_add(1),
                        p.saturating_add(m.power.unwrap_or(per_body)),
                    )
                });
        if let Some(body) = body
            && per_body > 0
        {
            if cfg.defenders && state.need > have_parts && have < cfg.max_defenders {
                let missing = state.need - have_parts;
                let bodies = missing
                    .div_ceil(per_body)
                    .min(cfg.max_defenders.saturating_sub(have));
                for _ in 0..bodies {
                    if spawner
                        .request(SpawnRequest {
                            home: room,
                            role: Role::Defender,
                            remote: None,
                            body: BodySpec::Exact(body),
                            budget: EnergyBudget::Capacity,
                            priority: PRIORITY_DEFENDER,
                        })
                        .is_some()
                    {
                        self.requested = self.requested.saturating_add(1);
                    }
                }
            }
            if state.need > state.logged
                && let Some((target, ticks)) = worst
            {
                state.logged = state.need;
                self.events.push(DefenderEvent::Need {
                    room,
                    target,
                    parts: state.need,
                    defenders: state.need.div_ceil(per_body),
                    per_body,
                    cost: body.cost(),
                    ticks,
                    have,
                });
            }
            if let Some(target) = beyond
                && !state.outmatched
            {
                state.outmatched = true;
                self.events.push(DefenderEvent::Outmatched {
                    room,
                    target,
                    defenders: cfg.max_defenders,
                    per_body,
                });
            }
        }
        if let Some(s) = self.rooms.get_mut(state_at) {
            *s = state;
        }
        Ok(())
    }

    /// Runs defender `unit` (module docs): walks home, or shoots and takes its post, or holds and
    /// gives way. Run after [`Self::plan_room`] for every room, in the same tick.
    #[allow(clippy::too_many_arguments)]
    pub fn work(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        threats: &Threats,
        towers: &TowerControl,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let m = usize::try_from(unit)
            .ok()
            .and_then(|u| self.members.get(u))
            .copied()
            .ok_or(Fault::new("no defender for the unit", unit))?;
        if m.spawning {
            return Ok(());
        }
        let here = m.pos.room_name();
        let go = |movement: &mut Movement, target: Position, range: u8, commit: &mut Commit| {
            let to = MoveTo {
                target,
                range,
                priority: MOVE_PRIORITY,
                tier: Tier::Critical,
            };
            // A move that plans nothing (no path yet, the room not read) tries again next tick.
            let _ = movement.request(snap, m.handle, to, mcfg, commit);
        };
        if here != m.home {
            if let Some(center) = Tile::new(25, 25).and_then(|t| at(m.home, t)) {
                go(movement, center, HOME_RANGE, commit);
            }
            return Ok(());
        }
        let now = snap.tick.time;
        let creeps = &snap.creeps;
        let reach = u32::from(REACH);
        let in_range = |h: CreepHandle| {
            creeps
                .row_of(h)
                .and_then(|i| creeps.positions().get(i))
                .is_some_and(|&p| range(m.pos, p) <= reach)
        };
        let target = self
            .rooms
            .iter()
            .find(|s| s.room == here && s.planned == now)
            .and_then(|s| s.target);
        let hostiles = threats.room(here).map_or(&[][..], |r| threats.hostiles(r));
        // The towers' focus first whenever it is in range, harmless or not: their sim counted this
        // defender at it (module docs). Then the room's target, then the weakest dangerous one.
        let shoot = towers
            .focus(here, now)
            .filter(|&h| in_range(h))
            .or_else(|| target.map(|t| t.0).filter(|&h| in_range(h)))
            .or_else(|| nearest_in_reach(snap, hostiles, m.pos));
        // A defender with no live RANGED_ATTACK part would be refused (ERR_NO_BODYPART, V339).
        if let Some(t) = shoot
            && m.power != Some(0)
        {
            // Refused only for a gone or spawning target, or one out of range: nothing to undo.
            let _ = buffer.push(
                snap,
                m.handle,
                CreepIntent::RangedAttack { target: t },
                commit,
            );
        }
        let Some((_, tpos)) = target else {
            movement.give_way(snap, m.handle, None);
            return Ok(());
        };
        match self.post(snap, hostiles, here, m, tpos) {
            Some(tile) => {
                self.taken.push((here, tile));
                match self.last.iter_mut().find(|(h, _)| *h == m.handle) {
                    Some(entry) => entry.1 = tile,
                    None => self.last.push((m.handle, tile)),
                }
                if let Some(p) = at(here, tile) {
                    go(movement, p, 0, commit);
                }
            }
            None => movement.give_way(snap, m.handle, None),
        }
        Ok(())
    }

    /// Whether this tick's defenders need no work unit: no hostile stands in an owned room and each
    /// stands in its home (spawning ones included), so each only holds and gives way
    /// ([`Self::hold_all`]).
    pub fn idle(&self, threats: &Threats) -> bool {
        threats.rooms().is_empty()
            && self
                .members
                .iter()
                .all(|m| m.spawning || m.pos.room_name() == m.home)
    }

    /// Lets every defender not spawning give way, with no work unit (the glue calls it when
    /// [`Self::idle`]).
    pub fn hold_all(&self, snap: &Snapshot, movement: &mut Movement) {
        for m in self.members.iter().filter(|m| !m.spawning) {
            movement.give_way(snap, m.handle, None);
        }
    }

    /// A post for defender `m` against the target at `target` (module docs), or `None` when no
    /// inside tile within range 3 of it is free. Its last post while still valid and out of melee
    /// reach, else the best by [`PostScore`].
    fn post(
        &self,
        snap: &Snapshot,
        hostiles: &[HostileThreat],
        room: RoomName,
        m: Member,
        target: Position,
    ) -> Option<Tile> {
        let me = m.pos;
        let tiles = &self.inside.iter().find(|i| i.room == room)?.tiles;
        let creeps = &snap.creeps;
        let position = |row: u32| {
            usize::try_from(row)
                .ok()
                .and_then(|i| creeps.positions().get(i))
                .copied()
        };
        let t = Tile::of(target);
        let mine = Tile::of(me);
        let last = self
            .last
            .iter()
            .find(|(h, _)| *h == m.handle)
            .map(|&(_, x)| x);
        let mut best: Option<(PostScore, Tile)> = None;
        let mut keep = false;
        for c in tiles_near(t, REACH) {
            let cell = tiles.get(c.index()).copied().unwrap_or(OUTSIDE);
            if cell == OUTSIDE {
                continue;
            }
            let held = hostiles
                .iter()
                .any(|h| position(h.row).is_some_and(|p| Tile::of(p) == c));
            let taken = self.taken.iter().any(|&(r, x)| r == room && x == c);
            // Another defender of the room stands there: it keeps its tile if it may.
            let other = self
                .members
                .iter()
                .any(|o| o.handle != m.handle && o.pos.room_name() == room && Tile::of(o.pos) == c);
            // A hostile's tile is never the creep's own; a tile another defender took or stands on
            // is skipped, so two never share a post.
            if held || taken || other {
                continue;
            }
            let exposed = hostiles.iter().any(|h| {
                h.power.attack > 0
                    && position(h.row).is_some_and(|p| Tile::of(p).range(c) <= MELEE_LEAD)
            });
            if last == Some(c) && !exposed {
                keep = true;
            }
            let score = (
                exposed,
                cell != ON_RAMPART,
                mine.range(c),
                REACH.saturating_sub(c.range(t)),
                c.index(),
            );
            if best.is_none_or(|(b, _)| score < b) {
                best = Some((score, c));
            }
        }
        if keep {
            return last;
        }
        best.map(|(_, c)| c)
    }

    /// The room's inside tiles, built or kept (module docs): their index in `self.inside`, or `None`
    /// while the room's movement grid is not built.
    fn ensure_inside(
        &mut self,
        snap: &Snapshot,
        movement: &mut Movement,
        room: RoomName,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Option<usize> {
        let (grid, version) = movement.room_grid(room, mcfg, commit)?;
        self.ramparts.clear();
        self.starts.clear();
        let st = &snap.structures;
        for (&kind, &pos) in st.kinds().iter().zip(st.positions()) {
            if pos.room_name() != room {
                continue;
            }
            match kind {
                StructureType::Rampart => self.ramparts.push(Tile::of(pos)),
                StructureType::Spawn => self.starts.push(Tile::of(pos)),
                _ => {}
            }
        }
        self.ramparts.sort_unstable();
        self.starts.sort_unstable();
        let key = tiles_key(&self.ramparts, &self.starts);
        let at = self.inside.iter().position(|i| i.room == room);
        if let Some(i) = at
            && let Some(x) = self.inside.get(i)
            && x.version == version
            && x.key == key
        {
            return Some(i);
        }
        let index = match at {
            Some(i) => i,
            None => {
                self.inside.push(Inside {
                    room,
                    version,
                    key,
                    tiles: Box::new([OUTSIDE; ROOM_AREA]),
                });
                self.inside.len() - 1
            }
        };
        let x = self.inside.get_mut(index)?;
        x.version = version;
        x.key = key;
        flood(
            grid,
            &self.ramparts,
            &self.starts,
            &mut x.tiles,
            &mut self.queue,
        );
        commit.heavy();
        Some(index)
    }

    /// Ends the tick: forgets the rooms, and their inside tiles, whose threat is over.
    pub fn finish(&mut self, threats: &Threats) {
        self.rooms
            .retain(|s| threats.under_threat(s.room).is_some());
        self.inside
            .retain(|i| threats.under_threat(i.room).is_some());
    }

    /// This tick's events.
    pub fn events(&self) -> &[DefenderEvent] {
        &self.events
    }

    /// The posts this tick's defenders took, by room and tile, in work order.
    pub fn posts(&self) -> &[(RoomName, Tile)] {
        &self.taken
    }

    /// This tick's spawn requests. Unit: requests.
    pub fn requested(&self) -> u32 {
        self.requested
    }

    /// The bot's defenders this tick. Unit: creeps.
    pub fn count(&self) -> usize {
        self.members.len()
    }
}

/// The dangerous hostile within range 3 of `from` with the fewest hits, then the lowest id.
fn nearest_in_reach(
    snap: &Snapshot,
    hostiles: &[HostileThreat],
    from: Position,
) -> Option<CreepHandle> {
    let creeps = &snap.creeps;
    let reach = u32::from(REACH);
    hostiles
        .iter()
        .filter(|h| h.power.dangerous())
        .filter_map(|h| {
            let i = usize::try_from(h.row).ok()?;
            let pos = *creeps.positions().get(i)?;
            let handle = *creeps.handles().get(i)?;
            (range(from, pos) <= reach).then(|| {
                let key = creeps.key_of(handle).map_or(u128::MAX, |k| k.0);
                (h.hits, key, handle)
            })
        })
        .min_by_key(|&(hits, key, _)| (hits, key))
        .map(|(_, _, h)| h)
}

/// A key for a room's ramparts and spawns (sorted tiles), so the inside tiles are built again when
/// either changes (an FNV-1a style fold; deterministic, not cryptographic).
fn tiles_key(ramparts: &[Tile], starts: &[Tile]) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (tag, tiles) in [(1u64, ramparts), (2u64, starts)] {
        h = (h ^ tag).wrapping_mul(PRIME);
        for t in tiles {
            h = (h ^ (t.index() as u64)).wrapping_mul(PRIME);
        }
    }
    h
}
