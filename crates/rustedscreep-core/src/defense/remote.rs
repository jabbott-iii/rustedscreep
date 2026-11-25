//! Remote defenders (ROADMAP P7.6, ADR-037): ranged creeps with heal parts that a home sends into a
//! remote room it mines (a room the bot mines without owning it, `economy::Remotes`) when armed
//! hostiles stand there, if a two-sided combat sim says they win and they cost less than the wait
//! would lose. Otherwise P6.3's evacuation stands alone.
//!
//! # What happens without them
//!
//! P6.3 already answers armed hostiles (an active ATTACK or RANGED_ATTACK part) in a mined room: its
//! creeps walk home and wait `remote.evacuate_ticks` (1500) after the last sight of them
//! (`economy::Threat`). An evacuated room is unseen, so the wait never ends early, and a raid costs
//! the room's income for about 1500 ticks. A raid comes after about `INVADERS_ENERGY_GOAL` (100000)
//! energy is mined in the room (V271, V342), so a two-source reserved room (10 energy a tick a
//! source) loses about a third of its income to the waits. A raid on a reserved room is usually one
//! small invader of 10 parts (V344, V345), which a body of a few hundred to a thousand energy kills.
//!
//! # When a home sends them
//!
//! For each threatened room (`Remotes::threats`), the bot keeps the bodies and hits of its armed
//! hostiles and healers as last seen ([`Foe`]): the snapshot reads every hostile's body once and its
//! `hits` each tick in a mined room that holds an armed hostile. The room is unseen while its
//! creeps wait, so the sizing works from that sight, with every foe whole: its healers refill it
//! while the team spawns and walks (V352), and a refilled part fights again (V283). [`size`] finds
//! the cheapest team that wins:
//! extra bodies of one [`Shape`], at most `defense.max_remote_defenders` bodies with the living
//! defenders of the room, each within the home's energy capacity. When the living team wins already,
//! nothing is requested. When the cheapest team costs more than the room's net income over the
//! wait that is left ([`pays`]), nothing is requested either, and one line says so. A threat beyond
//! the cap gets one line too. The requests go out at [`PRIORITY_REMOTE_DEFENDER`] (95): above every
//! remote role, below the home's own workers (100), so a home's economy never waits for its remote
//! rooms (P6.2's rule).
//!
//! # The sim ([`duel`])
//!
//! A stand-and-trade fight, chosen to err on the bot's side:
//!
//! - Every defender's live RANGED_ATTACK parts fire at one foe each tick (`RANGED_ATTACK_POWER` a
//!   part, range 3, V288), the armed foes in order of their effective hits (the fewest first,
//!   [`super::effective_hits`]), through the foe's boosted TOUGH parts ([`super::damage_taken`]).
//! - Every live foe with HEAL parts heals the target as if adjacent (`HEAL_POWER` a part, V288,
//!   V352), after the tick's damage (V287).
//! - Every armed foe alive at the tick's start hits with its melee damage and its `rangedAttack`
//!   both (V44, V348, V351: an invader uses both), as if always in reach, and each defender must
//!   survive all of it alone: whichever one the foes pick (`shootAtWill`'s pick among several is not
//!   verified, V351), it heals itself (`heal`, V354 to V356) after the damage and stays above its
//!   floor. So the sim holds for any targeting, and a defender's fate depends only on the fight's
//!   length and its own body.
//! - A defender is lost when its hits fall to its floor: it may lose `defense.remote_loss_pct` of
//!   its hits, at most its MOVE parts' hits. Its MOVE parts come first in the body, and damage takes
//!   the first parts first (V283), so above the floor every RANGED_ATTACK and HEAL part is live and
//!   the sim's damage and heal hold.
//! - The fight is won when every armed foe is dead within `defense.max_kill_ticks` (50) ticks; a
//!   target the team cannot bring lower (its heal matches the damage) is a loss.
//!
//! # When they go in
//!
//! Bodies spawn one after another, but the sim has the whole team fight from the first tick. So a
//! room's defenders go in only together: each tick the room's ready defenders (out of their spawn,
//! above their floor) are judged by the sim at their hits against the foes as last seen ([`fit`]):
//! the first to fall stays out and the rest are judged again, until they win or none is left. Those
//! left out, or all of them while none win, wait in the home (one already in the room walks out).
//! Invaders cannot leave their room (V353), so waiting outside is safe. The sizing judges new bodies
//! the same way, and counts a team only if every new body goes in. A defender at or below its floor
//! leaves too, heals itself, and does not count in the team until it is above it again.
//!
//! # Where they stand and what they do
//!
//! Once in, the room has one target, so the team never splits its fire (damage spread over two
//! targets is healed back on both): the armed foe with the fewest effective hits, kept while it
//! lives. Each defender shoots it when within range 3 (else the armed foe within range 3 with the
//! fewest hits), heals itself when damaged or when a foe can hit it this tick (a ranged one within
//! 3, a melee one next to it: an attack is checked against the tick's starting positions; a heal on
//! a full creep still lands after the tick's damage, V355), and takes a post within range 3 of the
//! target that no foe with an ATTACK part is within 2 tiles of, so a melee invader, which chases
//! (V348), does not reach it while the defender's MOVE parts are whole, and a ranged one, which
//! holds at range 3 (V350), stays in reach. When it sees the room with no armed hostile, the
//! room's threat ends (`Remotes::clear_threat`): the miners and haulers go back, and the threat does
//! not count toward `remote.contest_threats`. Only a remote defender standing in the room ends it:
//! evacuating creeps that still see the room when a hostile steps out over its edge do not, so a
//! player who steps in and out does not pull the crew back into reach, and with no defender P6.3's
//! wait stands. Then the defender stays in the room for the rest of its life, off the edge tiles, as
//! a guard: the next raid is sized with it first. A guard whose room is no longer mined (abandoned,
//! P6.3) walks home and waits there, so the room is no longer read.
//!
//! # State
//!
//! Each threatened room keeps its foes as last seen and whole, the sizing and the key it was
//! computed for (the foes' and the team's bodies, not their hits, so a fight does not size again
//! each tick), whether its team goes in this tick, its target, and what its lines said; each
//! defender its post. All of it is heap only: a global
//! reset forgets it, the defenders keep their remote room in their names, and a raid in a room they
//! guard is seen again at once. Nothing is read from or written to `Memory`.

use rustedscreep_consts::extra::BODYPART_HITS;
use rustedscreep_consts::{
    CREEP_LIFE_TIME, CREEP_RANGED_ACTION_RANGE, MAX_CREEP_SIZE, Part, Position, ROOM_SIZE,
    RoomCoordinate, RoomName,
};

use super::threat::{HostileThreat, Power, damage_taken, part_hits};
use crate::config::{DefenseConfig, MovementConfig};
use crate::economy::Remotes;
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{MoveTo, Movement, Tile};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{BodyPartRead, BodyRead, CreepHandle, Snapshot};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner};

/// A remote defender's spawn priority (ADR-037): above every remote role (the remote miner's 90),
/// below the home's own workers (`economy::opening`'s normal priority, 100).
pub const PRIORITY_REMOTE_DEFENDER: u8 = 95;

/// The traffic priority of a remote defender's moves: the home defenders' (above every economy
/// role's).
const MOVE_PRIORITY: u8 = 5;

/// How far a ranged attack reaches (V339). Unit: tiles.
const REACH: u8 = CREEP_RANGED_ACTION_RANGE;

/// How far a melee `attack` reaches (V288: the target next to the attacker). Unit: tiles.
const MELEE_REACH: u8 = 1;

/// A foe with an ATTACK part this close to a tile can hit a creep there now or after one step.
/// Unit: tiles.
const MELEE_LEAD: u8 = MELEE_REACH + 1;

/// The room's center, and the range from it that holds every tile off the two outer rings: a move
/// "into the room" arrives one step past the edge (as the remote creeps', `economy::remotes`).
const CENTER: u8 = ROOM_SIZE / 2;
const INSIDE: u8 = ROOM_SIZE / 2 - 2;

/// The most parts of one type pair a body holds: MOVE parts match the others one for one.
const MAX_PAIRS: u32 = MAX_CREEP_SIZE / 2;

/// A remote defender's body (module docs): `ranged` RANGED_ATTACK and `heal` HEAL parts behind as
/// many MOVE parts as both together, so whole it moves one tile a tick on plains (each part but
/// MOVE adds 2 fatigue a step there, and each live MOVE part removes 2, V55); its first MOVE parts
/// are the first to go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Shape {
    pub ranged: u32,
    pub heal: u32,
}

impl Shape {
    /// Its MOVE parts. Unit: parts.
    pub const fn moves(self) -> u32 {
        self.ranged.saturating_add(self.heal)
    }

    /// Its parts. Unit: parts.
    pub const fn parts(self) -> u32 {
        self.moves().saturating_mul(2)
    }

    /// Its spawn cost. Unit: energy.
    ///
    /// Uses: rustedscreep_consts::BODYPART_COST (through `Part::cost`).
    pub const fn cost(self) -> u32 {
        self.ranged
            .saturating_mul(Part::RangedAttack.cost())
            .saturating_add(self.heal.saturating_mul(Part::Heal.cost()))
            .saturating_add(self.moves().saturating_mul(Part::Move.cost()))
    }

    /// Its hits when whole (V282). Unit: hit points.
    ///
    /// Uses: rustedscreep_consts::extra::BODYPART_HITS.
    pub const fn hits(self) -> u32 {
        self.parts().saturating_mul(BODYPART_HITS)
    }

    /// The most hits it may lose in a fight by the sim (module docs): `loss_pct` of its hits, at
    /// most its MOVE parts' hits. Unit: hit points.
    ///
    /// Uses: rustedscreep_consts::extra::BODYPART_HITS.
    pub const fn allowance(self, loss_pct: u32) -> u32 {
        let share = self.hits().saturating_mul(loss_pct) / 100;
        let moves = self.moves().saturating_mul(BODYPART_HITS);
        if share < moves { share } else { moves }
    }

    /// Its body, the MOVE parts first, then the RANGED_ATTACK parts, the HEAL parts last (module
    /// docs); `None` with no RANGED_ATTACK part or more than `MAX_CREEP_SIZE` parts.
    pub fn body(self) -> Option<Body> {
        if self.ranged == 0 || self.parts() > MAX_CREEP_SIZE {
            return None;
        }
        let mut parts = [Part::Move; MAX_CREEP_SIZE as usize];
        let (moves, ranged) = (self.moves() as usize, self.ranged as usize);
        let len = self.parts() as usize;
        for (i, cell) in parts.iter_mut().enumerate().take(len) {
            *cell = if i < moves {
                Part::Move
            } else if i < moves + ranged {
                Part::RangedAttack
            } else {
                Part::Heal
            };
        }
        Body::new(parts.get(..len)?)
    }

    /// The shape of a body's RANGED_ATTACK and HEAL parts, live or not (an own remote defender's).
    pub fn of_parts(parts: &[BodyPartRead]) -> Self {
        let n = |p: Part| u32::try_from(parts.iter().filter(|x| x.part == p).count()).unwrap_or(0);
        Self {
            ranged: n(Part::RangedAttack),
            heal: n(Part::Heal),
        }
    }

    /// A fresh defender of this shape in the sim.
    ///
    /// Uses: rustedscreep_consts::{RANGED_ATTACK_POWER, HEAL_POWER} (through [`Power`]).
    pub const fn fighter(self, loss_pct: u32) -> Fighter {
        let power = Power {
            attack: 0,
            ranged: self.ranged,
            heal: self.heal,
            dismantle: 0,
            claim: 0,
        };
        let hits = self.hits();
        Fighter {
            damage: power.ranged_damage(),
            heal: power.heal_adjacent(),
            hits,
            hits_max: hits,
            floor: hits.saturating_sub(self.allowance(loss_pct)),
        }
    }
}

/// One defender in the sim: its `rangedAttack` damage and self heal a tick, its hits, and the hits
/// at or below which it counts as lost (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fighter {
    pub damage: u32,
    pub heal: u32,
    pub hits: u32,
    pub hits_max: u32,
    pub floor: u32,
}

/// A hostile as seen (module docs): its body, hits and what its live parts can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Foe {
    pub body: BodyRead,
    pub threat: HostileThreat,
}

impl Foe {
    /// The foe of a body read with `hits`.
    pub fn of(body: &BodyRead, hits: u32) -> Self {
        Self {
            body: *body,
            threat: HostileThreat::of(0, body, hits),
        }
    }

    /// The same foe whole (its healers refill it, module docs).
    pub fn whole(&self) -> Self {
        Self::of(&self.body, self.threat.hits_max)
    }

    /// Its damage a tick to one creep in reach: melee and `rangedAttack` together (module docs).
    ///
    /// Uses: rustedscreep_consts::{ATTACK_POWER, RANGED_ATTACK_POWER} (through [`Power`]).
    pub const fn damage(&self) -> u32 {
        self.threat
            .power
            .melee_damage()
            .saturating_add(self.threat.power.ranged_damage())
    }
}

/// Scratch for [`duel`] and [`fit`].
#[derive(Default)]
pub struct DuelScratch {
    hits: Vec<u32>,
    order: Vec<usize>,
    own: Vec<u32>,
    sub: Vec<Fighter>,
    map: Vec<usize>,
}

/// Why a fight is lost by the sim ([`fight`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loss {
    /// The defender at this index of the team starts at or falls to its floor first.
    Falls(usize),
    /// The team deals no damage, cannot bring a target lower, or outlasts the limit.
    Stalls,
}

/// The ticks `team` takes to kill every armed foe by the sim (module docs), or `None` when it loses
/// ([`fight`]).
pub fn duel(foes: &[Foe], team: &[Fighter], limit: u32, s: &mut DuelScratch) -> Option<u32> {
    fight(foes, team, limit, s).ok()
}

/// The fight by the sim (module docs): the ticks `team` takes to kill every armed foe, `0` with no
/// armed foe, or why it loses within `limit` ticks. Each defender takes every armed foe's damage
/// each tick, so the answer holds whichever one the foes pick.
///
/// Uses: rustedscreep_consts::{extra::BODYPART_HITS, ResourceType::boost} (through
/// `damage_taken`), HEAL_POWER, ATTACK_POWER, RANGED_ATTACK_POWER (through [`Power`]).
pub fn fight(foes: &[Foe], team: &[Fighter], limit: u32, s: &mut DuelScratch) -> Result<u32, Loss> {
    s.hits.clear();
    s.hits.extend(foes.iter().map(|f| f.threat.hits));
    s.order.clear();
    s.order.extend(
        foes.iter()
            .enumerate()
            .filter(|(_, f)| f.threat.power.armed() && f.threat.hits > 0)
            .map(|(i, _)| i),
    );
    s.order
        .sort_by_key(|&i| (foes.get(i).map_or(u32::MAX, |f| f.threat.effective_hits), i));
    if s.order.is_empty() {
        return Ok(0);
    }
    if let Some(i) = team.iter().position(|f| f.hits <= f.floor) {
        return Err(Loss::Falls(i));
    }
    let damage = team
        .iter()
        .fold(0u32, |sum, f| sum.saturating_add(f.damage));
    if damage == 0 {
        return Err(Loss::Stalls);
    }
    s.own.clear();
    s.own.extend(team.iter().map(|f| f.hits));
    let mut next = 0usize;
    for tick in 1..=limit {
        // This tick's blows and heals come from the foes alive at its start.
        let (mut incoming, mut heal) = (0u32, 0u32);
        for (f, &h) in foes.iter().zip(s.hits.iter()) {
            if h == 0 {
                continue;
            }
            if f.threat.power.armed() {
                incoming = incoming.saturating_add(f.damage());
            }
            heal = heal.saturating_add(f.threat.power.heal_adjacent());
        }
        let (Some(&target), stalls) = (s.order.get(next), Err(Loss::Stalls)) else {
            return Err(Loss::Stalls);
        };
        let (Some(foe), Some(&before)) = (foes.get(target), s.hits.get(target)) else {
            return stalls;
        };
        // No boosted part, no TOUGH saving: the damage lands whole (`damage_taken` would agree).
        let taken = if foe.threat.boosted {
            damage_taken(foe.body.parts(), before, damage)
        } else {
            damage
        };
        let after = i64::from(before) - i64::from(taken) + i64::from(heal);
        let Some(slot) = s.hits.get_mut(target) else {
            return stalls;
        };
        if after <= 0 {
            *slot = 0;
            next = next.saturating_add(1);
        } else {
            let after = u32::try_from(after)
                .unwrap_or(u32::MAX)
                .min(foe.threat.hits_max);
            if after >= before {
                return stalls;
            }
            *slot = after;
        }
        for (i, (f, h)) in team.iter().zip(s.own.iter_mut()).enumerate() {
            let own = i64::from(*h) - i64::from(incoming) + i64::from(f.heal);
            let own = u32::try_from(own.max(0)).unwrap_or(0).min(f.hits_max);
            if own <= f.floor {
                return Err(Loss::Falls(i));
            }
            *h = own;
        }
        if next >= s.order.len() {
            return Ok(tick);
        }
    }
    Err(Loss::Stalls)
}

/// Which of `team` go into the fight (module docs): the whole team while the sim says it wins,
/// else the team without the first defender to fall, again, until the rest win. Returns the ticks
/// the members marked in `keep` take, or `None` when none is left or the fight stalls (taking a
/// defender out only lowers the team's damage, so no smaller team does better).
pub fn fit(
    foes: &[Foe],
    team: &[Fighter],
    keep: &mut Vec<bool>,
    limit: u32,
    s: &mut DuelScratch,
) -> Option<u32> {
    keep.clear();
    keep.resize(team.len(), true);
    let mut sub = core::mem::take(&mut s.sub);
    let mut map = core::mem::take(&mut s.map);
    let result = loop {
        sub.clear();
        map.clear();
        for (i, (f, &k)) in team.iter().zip(keep.iter()).enumerate() {
            if k {
                sub.push(*f);
                map.push(i);
            }
        }
        if sub.is_empty() {
            break None;
        }
        match fight(foes, &sub, limit, s) {
            Ok(ticks) => break Some(ticks),
            Err(Loss::Falls(j)) => match map.get(j).and_then(|&i| keep.get_mut(i)) {
                Some(k) => *k = false,
                None => break None,
            },
            Err(Loss::Stalls) => break None,
        }
    };
    s.sub = sub;
    s.map = map;
    result
}

/// What a threatened room needs ([`size`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sizing {
    /// The room's living defenders win alone, in `ticks` by the sim.
    Held { ticks: u32 },
    /// `count` more bodies of `shape`, with the living defenders, win in `ticks` by the sim.
    Need {
        shape: Shape,
        count: u32,
        ticks: u32,
    },
    /// No team within the cap and the home's capacity wins.
    Beyond,
}

/// A winning team in [`size`]'s search: its rank (energy in all, bodies, parts), its shape, its
/// bodies, and its ticks by the sim.
type Candidate = ((u64, u32, u32), Shape, u32, u32);

/// Scratch for [`size`].
#[derive(Default)]
pub struct SizeScratch {
    team: Vec<Fighter>,
    keep: Vec<bool>,
    duel: DuelScratch,
}

/// The cheapest team that wins against `foes` (module docs): `living` first (their bodies count in
/// `have`, with the ones the sim has no fighter for), then up to `cap - have` bodies of one
/// [`Shape`] that costs at most `capacity`, every one of which goes in ([`fit`]: a living defender
/// that would fall stays out). Among the teams that win, the least energy in all, then the fewest
/// bodies, then the fewest parts. With the same HEAL parts, more RANGED_ATTACK parts never lose a
/// fight fewer won (the fight only shortens, and each defender's fate depends on nothing but the
/// fight's length and its own body), so the least for each HEAL count is a binary search.
///
/// Uses: rustedscreep_consts::{BODYPART_COST, MAX_CREEP_SIZE} (through [`Shape`]).
#[allow(clippy::too_many_arguments)]
pub fn size(
    foes: &[Foe],
    living: &[Fighter],
    have: u32,
    capacity: u32,
    cap: u32,
    limit: u32,
    loss_pct: u32,
    s: &mut SizeScratch,
) -> Sizing {
    if !living.is_empty()
        && let Some(ticks) = fit(foes, living, &mut s.keep, limit, &mut s.duel)
    {
        return Sizing::Held { ticks };
    }
    let room = cap.saturating_sub(have);
    let mut best: Option<Candidate> = None;
    for count in 1..=room {
        for heal in 0..MAX_PAIRS {
            // The most RANGED_ATTACK parts that fit the body and the capacity.
            let most = (1..=MAX_PAIRS.saturating_sub(heal))
                .rev()
                .find(|&ranged| Shape { ranged, heal }.cost() <= capacity);
            let Some(most) = most else {
                break;
            };
            // Even one RANGED_ATTACK part with these HEAL parts costs more than the best team found:
            // so does every shape after it (the cost only grows with HEAL parts).
            let least =
                u64::from(Shape { ranged: 1, heal }.cost()).saturating_mul(u64::from(count));
            if best.is_some_and(|((k, ..), ..)| least > k) {
                break;
            }
            let mut wins = |ranged: u32| {
                let shape = Shape { ranged, heal };
                s.team.clear();
                s.team.extend_from_slice(living);
                for _ in 0..count {
                    s.team.push(shape.fighter(loss_pct));
                }
                let ticks = fit(foes, &s.team, &mut s.keep, limit, &mut s.duel)?;
                // Every new body goes in: one left out would be spent for nothing.
                s.keep
                    .get(living.len()..)
                    .is_some_and(|new| new.iter().all(|&k| k))
                    .then_some(ticks)
            };
            let Some(mut ticks) = wins(most) else {
                continue;
            };
            let (mut low, mut high) = (1u32, most);
            while low < high {
                let mid = low + (high - low) / 2;
                match wins(mid) {
                    Some(t) => {
                        high = mid;
                        ticks = t;
                    }
                    None => low = mid.saturating_add(1),
                }
            }
            // `ticks` is the last winning search's, at `high`, which is `low` now.
            let shape = Shape { ranged: low, heal };
            let key = (
                u64::from(shape.cost()).saturating_mul(u64::from(count)),
                count,
                shape.parts(),
            );
            if best.is_none_or(|(k, ..)| key < k) {
                best = Some((key, shape, count, ticks));
            }
        }
    }
    match best {
        Some((_, shape, count, ticks)) => Sizing::Need {
            shape,
            count,
            ticks,
        },
        None => Sizing::Beyond,
    }
}

/// Whether `cost` energy of defenders pays for itself (module docs): less than the room's `net`
/// energy a creep life (`economy::RemoteProfit::net`) over the `wait_left` ticks the wait still
/// has, less the `busy` ticks before the team ends the fight (its spawn time and the sim's kill
/// time; the walk is not counted).
///
/// Uses: rustedscreep_consts::CREEP_LIFE_TIME.
pub fn pays(cost: u32, net: i64, wait_left: u32, busy: u32) -> bool {
    let Ok(net) = u64::try_from(net) else {
        return false;
    };
    let saved = u64::from(wait_left.saturating_sub(busy));
    u64::from(cost).saturating_mul(u64::from(CREEP_LIFE_TIME)) < net.saturating_mul(saved)
}

/// What the remote defense did this tick, for the log ([`RemoteDefense::events`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteDefenseEvent {
    /// `room`'s threat is answered: `count` more bodies of `shape` (each `cost` energy), with the
    /// `have` living defenders, kill its `foes` armed hostiles in `ticks` by the sim. With
    /// `defense.remote_defenders` off, nothing is requested.
    Need {
        home: RoomName,
        room: RoomName,
        shape: Shape,
        count: u32,
        cost: u32,
        ticks: u32,
        have: u32,
        foes: u32,
    },
    /// The `have` living defenders win alone, in `ticks` by the sim (no request).
    Held {
        home: RoomName,
        room: RoomName,
        ticks: u32,
        have: u32,
    },
    /// The cheapest team costs `cost` energy, not less than the `lost` energy the wait would lose:
    /// the room waits (P6.3).
    NotWorth {
        home: RoomName,
        room: RoomName,
        cost: u32,
        lost: u64,
    },
    /// No team of up to `defense.max_remote_defenders` bodies within the home's capacity wins: the
    /// room waits (P6.3).
    Outmatched {
        home: RoomName,
        room: RoomName,
        foes: u32,
        cap: u32,
    },
    /// A remote defender in `room` saw it with no armed hostile: its threat, open since `since`,
    /// ends now, and its creeps go back.
    Cleared {
        home: RoomName,
        room: RoomName,
        since: u32,
    },
}

/// What a room's last line said this threat, so each kind goes out once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Logged {
    need: Option<(Shape, u32)>,
    held: bool,
    not_worth: bool,
    outmatched: bool,
}

/// One threatened room's defense, kept while its threat is open.
#[derive(Clone, Debug)]
struct RoomState {
    home: RoomName,
    room: RoomName,
    /// The tick its threat started (`economy::Threat::since`); a new threat starts afresh.
    since: u32,
    /// Its armed hostiles and healers as last seen, and the same whole (for the sizing).
    current: Vec<Foe>,
    foes: Vec<Foe>,
    /// The sizing, and the key of what it was computed from.
    sized: Option<(u64, Sizing)>,
    logged: Logged,
    /// The shape last requested for the room, for its spawning bodies (whose body the snapshot
    /// does not read).
    shape: Option<Shape>,
    /// The tick of the last plan, and the ready defenders that go in then (module docs).
    planned: u32,
    going: Vec<CreepHandle>,
}

/// One of the bot's remote defenders this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Member {
    home: RoomName,
    remote: RoomName,
    row: u32,
    handle: CreepHandle,
    key: u128,
    pos: Position,
    spawning: bool,
    /// Its live power, from its body read once and its hits read each tick; `None` while spawning
    /// or its body is unread.
    power: Option<Power>,
    /// Its hits and `hitsMax` as read this tick; `None` while spawning.
    hits: Option<(u32, u32)>,
    /// The hits at or below which it leaves the fight (module docs), from its whole body.
    floor: Option<u32>,
    /// Its home still mines its remote room (or it is threatened).
    mined: bool,
}

impl Member {
    fn damaged(&self) -> bool {
        self.hits.is_some_and(|(h, m)| h < m)
    }

    /// At or below its floor: it leaves the fight and does not count in the team.
    fn spent(&self) -> bool {
        matches!((self.hits, self.floor), (Some((h, _)), Some(f)) if h <= f)
    }

    /// The sim's fighter for it: its live power at its hits, `None` while spawning or unread.
    fn fighter(&self) -> Option<Fighter> {
        let (p, (hits, hits_max), floor) = (self.power?, self.hits?, self.floor?);
        Some(Fighter {
            damage: p.ranged_damage(),
            heal: p.heal_adjacent(),
            hits,
            hits_max,
            floor,
        })
    }
}

/// One threatened room to plan this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unit {
    home: RoomName,
    room: RoomName,
    since: u32,
    until: u32,
}

/// The remote defense's plan and state for one global.
#[derive(Default)]
pub struct RemoteDefense {
    /// This tick's remote defenders, by home, then remote room, then id.
    members: Vec<Member>,
    units: Vec<Unit>,
    rooms: Vec<RoomState>,
    events: Vec<RemoteDefenseEvent>,
    /// This tick's spawn requests. Unit: requests.
    requested: u32,
    /// Each defender's last post, kept while it stays valid.
    last: Vec<(CreepHandle, Tile)>,
    /// Each remote room's target (module docs), kept while it is an armed foe there.
    targets: Vec<(RoomName, CreepHandle)>,
    /// Posts taken this tick.
    taken: Vec<(RoomName, Tile)>,
    /// The hostiles of one room this tick, and which room and tick they are of.
    here: Vec<Present>,
    here_of: Option<(RoomName, u32)>,
    /// Scratch.
    living: Vec<Fighter>,
    ready: Vec<Fighter>,
    ready_of: Vec<CreepHandle>,
    keep: Vec<bool>,
    seen: Vec<Foe>,
    scratch: SizeScratch,
}

/// A hostile in a defender's room this tick ([`RemoteDefense::work`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Present {
    handle: CreepHandle,
    key: u128,
    pos: Position,
    threat: HostileThreat,
}

impl RemoteDefense {
    /// Starts the tick: lists the bot's remote defenders and the threatened rooms to plan. Call after
    /// the remote manager's plan (its threats are this tick's) and before the spawn plan.
    pub fn begin(&mut self, snap: &Snapshot, remotes: &Remotes, cfg: &DefenseConfig) {
        self.members.clear();
        self.units.clear();
        self.events.clear();
        self.taken.clear();
        self.here_of = None;
        self.requested = 0;
        self.units
            .extend(remotes.threats().map(|(home, room, t)| Unit {
                home,
                room,
                since: t.since,
                until: t.until,
            }));
        self.units
            .sort_unstable_by_key(|u| (u.home.packed_repr(), u.room.packed_repr()));
        let creeps = &snap.creeps;
        for (row, identity) in creeps.identities().iter().enumerate() {
            let Some(id) = identity.filter(|id| id.role == Role::RemoteDefender) else {
                continue;
            };
            let Some(remote) = id.remote else {
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
            let read = creeps.own_hits_rows().binary_search(&r).is_ok();
            let hits = (!spawning && read).then(|| {
                (
                    creeps.hits().get(row).copied().unwrap_or(0),
                    creeps.hits_max().get(row).copied().unwrap_or(0),
                )
            });
            let (power, floor) = if spawning {
                (None, None)
            } else {
                let floor = creeps.body(row).map(|b| {
                    let full = u32::try_from(b.len())
                        .unwrap_or(u32::MAX)
                        .saturating_mul(BODYPART_HITS);
                    full.saturating_sub(Shape::of_parts(b.parts()).allowance(cfg.remote_loss_pct))
                });
                (live_power(snap, row, hits.map(|h| h.0)), floor)
            };
            let mined = self
                .units
                .iter()
                .any(|u| u.home == id.home && u.room == remote)
                || mines(remotes, id.home, remote);
            self.members.push(Member {
                home: id.home,
                remote,
                row: r,
                handle,
                key: creeps.key_of(handle).map_or(u128::MAX, |k| k.0),
                pos,
                spawning,
                power,
                hits,
                floor,
                mined,
            });
        }
        self.members
            .sort_unstable_by_key(|m| (m.home.packed_repr(), m.remote.packed_repr(), m.key));
        let members = &self.members;
        self.last
            .retain(|&(h, _)| members.iter().any(|m| m.handle == h));
    }

    /// This tick's threatened rooms to plan.
    pub fn unit_count(&self) -> u32 {
        u32::try_from(self.units.len()).unwrap_or(u32::MAX)
    }

    /// This tick's remote defenders, one work unit each.
    pub fn member_count(&self) -> u32 {
        u32::try_from(self.members.len()).unwrap_or(u32::MAX)
    }

    /// Plans threatened room `unit` (module docs): ends its threat when one of its remote defenders
    /// stands in it and sees no armed hostile; else keeps its foes as seen, decides whether its ready
    /// defenders go in, and, while `spare` (the Standard tier runs this tick, as for remote mining's
    /// requests), sizes its team and requests the bodies it lacks. A sizing that runs (its key
    /// changed) is a heavy unit. Run before the spawn plan.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        remotes: &mut Remotes,
        spawner: &mut Spawner,
        cfg: &DefenseConfig,
        spare: bool,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let u = usize::try_from(unit)
            .ok()
            .and_then(|i| self.units.get(i))
            .copied()
            .ok_or(Fault::new("no remote room for the unit", unit))?;
        let now = snap.tick.time;
        let at = match self
            .rooms
            .iter()
            .position(|s| s.home == u.home && s.room == u.room)
        {
            Some(i) => i,
            None => {
                self.rooms.push(RoomState {
                    home: u.home,
                    room: u.room,
                    since: u.since,
                    current: Vec::new(),
                    foes: Vec::new(),
                    sized: None,
                    logged: Logged::default(),
                    shape: None,
                    planned: now,
                    going: Vec::new(),
                });
                self.rooms.len() - 1
            }
        };
        let state = self
            .rooms
            .get_mut(at)
            .ok_or(Fault::new("no state for the remote room", unit))?;
        if state.since != u.since {
            // A new threat: its own foes and lines (the shape last requested stays, for a body of
            // the last threat still spawning).
            state.since = u.since;
            state.current.clear();
            state.foes.clear();
            state.sized = None;
            state.logged = Logged::default();
        }
        state.planned = now;
        state.going.clear();
        // Seen this tick: the armed hostiles are gone (the wait ends only once a remote defender of
        // the room stands in it, so evacuating creeps that saw a hostile step out do not send the
        // crew back), or their bodies are read now.
        let guarded = self.members.iter().any(|m| {
            m.home == u.home && m.remote == u.room && !m.spawning && m.pos.room_name() == u.room
        });
        if let Some(intel) = remotes.intel(u.room).filter(|i| i.seen == now) {
            if intel.armed == 0 {
                if guarded && remotes.clear_threat(u.room) {
                    self.events.push(RemoteDefenseEvent::Cleared {
                        home: u.home,
                        room: u.room,
                        since: u.since,
                    });
                }
                return Ok(());
            }
            let creeps = &snap.creeps;
            self.seen.clear();
            for &row in creeps.assessed() {
                let Ok(index) = usize::try_from(row) else {
                    continue;
                };
                if creeps
                    .positions()
                    .get(index)
                    .is_none_or(|p| p.room_name() != u.room)
                {
                    continue;
                }
                if let Some(body) = creeps.body(index) {
                    let foe = Foe::of(body, creeps.hits().get(index).copied().unwrap_or(0));
                    if foe.threat.power.armed() || foe.threat.power.heal > 0 {
                        self.seen.push(foe);
                    }
                }
            }
            let armed_read = self.seen.iter().filter(|f| f.threat.power.armed()).count();
            // A body not read this tick keeps the last sight's foes (none occur while the snapshot
            // reads every one).
            if u32::try_from(armed_read).unwrap_or(u32::MAX) >= intel.armed {
                state.current.clear();
                state.current.extend_from_slice(&self.seen);
                state.foes.clear();
                state.foes.extend(self.seen.iter().map(Foe::whole));
            }
        }
        if state.foes.is_empty() {
            // Nothing seen to size from (after a reset, with the room unseen): the room waits.
            return Ok(());
        }
        // The room's defenders: every one counts toward the cap; the sim's team leaves out those
        // spent (at or below their floor), and counts a spawning one at the shape last requested;
        // the ones ready to go in are those out of their spawn.
        self.living.clear();
        self.ready.clear();
        self.ready_of.clear();
        let mut have = 0u32;
        for m in self
            .members
            .iter()
            .filter(|m| m.home == u.home && m.remote == u.room)
        {
            have = have.saturating_add(1);
            if m.spent() {
                continue;
            }
            match m.fighter() {
                Some(f) => {
                    self.living.push(f);
                    self.ready.push(f);
                    self.ready_of.push(m.handle);
                }
                None => {
                    if let Some(s) = state.shape.filter(|_| m.spawning) {
                        self.living.push(s.fighter(cfg.remote_loss_pct));
                    }
                }
            }
        }
        // The ready defenders go in only together, while they win against the foes as last seen;
        // one that would fall stays out (`fit`).
        if fit(
            &state.current,
            &self.ready,
            &mut self.keep,
            cfg.max_kill_ticks,
            &mut self.scratch.duel,
        )
        .is_some()
        {
            state.going.extend(
                self.ready_of
                    .iter()
                    .zip(self.keep.iter())
                    .filter(|&(_, &k)| k)
                    .map(|(&h, _)| h),
            );
        }
        if !spare {
            return Ok(());
        }
        let capacity = snap
            .rooms
            .row_of(u.home)
            .and_then(|r| snap.rooms.energy().get(r))
            .map_or(0, |e| e.capacity);
        let key = sizing_key(&state.foes, &self.living, have, capacity, cfg);
        let sizing = match state.sized {
            Some((k, s)) if k == key => s,
            _ => {
                let s = size(
                    &state.foes,
                    &self.living,
                    have,
                    capacity,
                    cfg.max_remote_defenders,
                    cfg.max_kill_ticks,
                    cfg.remote_loss_pct,
                    &mut self.scratch,
                );
                commit.heavy();
                state.sized = Some((key, s));
                s
            }
        };
        let foes = u32::try_from(state.foes.iter().filter(|f| f.threat.power.armed()).count())
            .unwrap_or(u32::MAX);
        match sizing {
            Sizing::Held { ticks } => {
                if !state.logged.held && state.logged.need.is_none() {
                    state.logged.held = true;
                    self.events.push(RemoteDefenseEvent::Held {
                        home: u.home,
                        room: u.room,
                        ticks,
                        have,
                    });
                }
            }
            Sizing::Need {
                shape,
                count,
                ticks,
            } => {
                let Some(body) = shape.body() else {
                    return Ok(());
                };
                let cost = shape.cost();
                let all = cost.saturating_mul(count);
                let wait = u.until.saturating_sub(now);
                // The bodies spawn one after another (at worst on one spawn), then fight.
                let busy = body
                    .spawn_time()
                    .saturating_mul(count)
                    .saturating_add(ticks);
                let net = remote_net(remotes, u.home, u.room);
                if !pays(all, net, wait, busy) {
                    if !state.logged.not_worth {
                        state.logged.not_worth = true;
                        let lost = u64::try_from(net)
                            .unwrap_or(0)
                            .saturating_mul(u64::from(wait.saturating_sub(busy)))
                            / u64::from(CREEP_LIFE_TIME);
                        self.events.push(RemoteDefenseEvent::NotWorth {
                            home: u.home,
                            room: u.room,
                            cost: all,
                            lost,
                        });
                    }
                    return Ok(());
                }
                if state.logged.need != Some((shape, count)) {
                    state.logged.need = Some((shape, count));
                    self.events.push(RemoteDefenseEvent::Need {
                        home: u.home,
                        room: u.room,
                        shape,
                        count,
                        cost,
                        ticks,
                        have,
                        foes,
                    });
                }
                if cfg.remote_defenders {
                    state.shape = Some(shape);
                    for _ in 0..count {
                        if spawner
                            .request(SpawnRequest {
                                home: u.home,
                                role: Role::RemoteDefender,
                                remote: Some(u.room),
                                body: BodySpec::Exact(body),
                                budget: EnergyBudget::Capacity,
                                priority: PRIORITY_REMOTE_DEFENDER,
                            })
                            .is_some()
                        {
                            self.requested = self.requested.saturating_add(1);
                        }
                    }
                }
            }
            Sizing::Beyond => {
                if !state.logged.outmatched {
                    state.logged.outmatched = true;
                    self.events.push(RemoteDefenseEvent::Outmatched {
                        home: u.home,
                        room: u.room,
                        foes,
                        cap: cfg.max_remote_defenders,
                    });
                }
            }
        }
        Ok(())
    }

    /// Runs remote defender `unit` (module docs): waits in the home (spent, its room no longer mined,
    /// or its team not going in), walks to its remote room, or shoots, heals and takes its post, or
    /// guards off the edge. Run after [`Self::plan_room`] for every room.
    pub fn work(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let m = usize::try_from(unit)
            .ok()
            .and_then(|u| self.members.get(u))
            .copied()
            .ok_or(Fault::new("no remote defender for the unit", unit))?;
        if m.spawning {
            return Ok(());
        }
        let now = snap.tick.time;
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
        let heal_self = |buffer: &mut IntentBuffer, commit: &mut Commit| {
            if m.power.is_some_and(|p| p.heal > 0) {
                let _ = buffer.push(
                    snap,
                    m.handle,
                    CreepIntent::Heal { target: m.handle },
                    commit,
                );
            }
        };
        let here = m.pos.room_name();
        let center_of = |room: RoomName| Tile::new(CENTER, CENTER).and_then(|t| at(room, t));
        // Out of the fight: spent, its room no longer mined, or its team not going in this tick.
        let held_back = self
            .rooms
            .iter()
            .find(|s| s.home == m.home && s.room == m.remote && s.planned == now)
            .is_some_and(|s| !s.going.contains(&m.handle));
        if m.spent() || !m.mined || held_back {
            if m.damaged() {
                heal_self(buffer, commit);
            }
            self.last.retain(|(h, _)| *h != m.handle);
            if let Some(c) = center_of(m.home) {
                if here == m.home && !Tile::of(m.pos).is_edge() {
                    movement.give_way(snap, m.handle, Some((c, INSIDE)));
                } else {
                    go(movement, c, INSIDE, commit);
                }
            }
            return Ok(());
        }
        let center = center_of(m.remote);
        if here != m.remote {
            if let Some(c) = center {
                go(movement, c, INSIDE, commit);
            }
            if m.damaged() {
                heal_self(buffer, commit);
            }
            self.last.retain(|(h, _)| *h != m.handle);
            return Ok(());
        }
        self.read_here(snap, m.remote);
        let reach = u32::from(REACH);
        let target = self
            .targets
            .iter()
            .find(|(r, _)| *r == m.remote)
            .and_then(|&(_, h)| self.here.iter().find(|p| p.handle == h))
            .map(|p| (p.handle, p.pos));
        // Heal itself when hurt, or when a foe can hit it this tick (module docs).
        let threatened = self.here.iter().any(|p| {
            let r = chebyshev(m.pos, p.pos);
            (p.threat.power.ranged > 0 && r <= reach)
                || (p.threat.power.attack > 0 && r <= u32::from(MELEE_REACH))
        });
        if m.damaged() || threatened {
            heal_self(buffer, commit);
        }
        // A defender with no live RANGED_ATTACK part would be refused (ERR_NO_BODYPART, V339).
        if m.power.is_some_and(|p| p.ranged > 0) {
            let shoot = target
                .filter(|&(_, p)| chebyshev(m.pos, p) <= reach)
                .map(|(h, _)| h)
                .or_else(|| {
                    self.here
                        .iter()
                        .filter(|p| p.threat.power.armed())
                        .filter(|p| chebyshev(m.pos, p.pos) <= reach)
                        .min_by_key(|p| (p.threat.hits, p.key))
                        .map(|p| p.handle)
                });
            if let Some(t) = shoot {
                // Refused only for a gone or spawning target, or one out of range: nothing to undo.
                let _ = buffer.push(
                    snap,
                    m.handle,
                    CreepIntent::RangedAttack { target: t },
                    commit,
                );
            }
        }
        let Some((_, tpos)) = target else {
            // No armed foe: guard off the edge tiles (one there is carried out at the tick's end,
            // V151), giving way to any creep that needs its tile.
            self.last.retain(|(h, _)| *h != m.handle);
            if Tile::of(m.pos).is_edge() {
                if let Some(c) = center {
                    go(movement, c, INSIDE, commit);
                }
            } else {
                movement.give_way(snap, m.handle, center.map(|c| (c, INSIDE)));
            }
            return Ok(());
        };
        let last_post = self
            .last
            .iter()
            .find(|(h, _)| *h == m.handle)
            .map(|&(_, t)| t);
        let post = match movement.room_grid(m.remote, mcfg, commit) {
            Some((grid, _)) => self.post(&m, tpos, last_post, |t| grid.walkable(t)),
            None => None,
        };
        self.last.retain(|(h, _)| *h != m.handle);
        match post.and_then(|t| at(m.remote, t).map(|p| (t, p))) {
            Some((t, p)) => {
                self.last.push((m.handle, t));
                self.taken.push((m.remote, t));
                go(movement, p, 0, commit);
            }
            // No grid yet, or no free tile: close to range 3.
            None => go(movement, tpos, REACH, commit),
        }
        Ok(())
    }

    /// Reads `room`'s hostiles this tick into `self.here` (once a room and tick), and picks the
    /// room's target: last tick's while it is an armed foe here, else the armed foe with the fewest
    /// effective hits, then the lowest id.
    fn read_here(&mut self, snap: &Snapshot, room: RoomName) {
        let now = snap.tick.time;
        if self.here_of == Some((room, now)) {
            return;
        }
        self.here_of = Some((room, now));
        self.here.clear();
        let creeps = &snap.creeps;
        for &row in creeps.assessed() {
            let Ok(i) = usize::try_from(row) else {
                continue;
            };
            let Some(&pos) = creeps.positions().get(i) else {
                continue;
            };
            if pos.room_name() != room {
                continue;
            }
            let (Some(&handle), Some(body)) = (creeps.handles().get(i), creeps.body(i)) else {
                continue;
            };
            let threat = HostileThreat::of(row, body, creeps.hits().get(i).copied().unwrap_or(0));
            let key = creeps.key_of(handle).map_or(u128::MAX, |k| k.0);
            self.here.push(Present {
                handle,
                key,
                pos,
                threat,
            });
        }
        let kept = self
            .targets
            .iter()
            .find(|(r, _)| *r == room)
            .map(|&(_, h)| h)
            .filter(|&h| {
                self.here
                    .iter()
                    .any(|p| p.handle == h && p.threat.power.armed())
            });
        let target = kept.or_else(|| {
            self.here
                .iter()
                .filter(|p| p.threat.power.armed())
                .min_by_key(|p| (p.threat.effective_hits, p.key))
                .map(|p| p.handle)
        });
        self.targets.retain(|(r, _)| *r != room);
        if let Some(t) = target {
            self.targets.push((room, t));
        }
    }

    /// A post for `m` against the target at `target` (module docs), from this tick's hostiles in its
    /// room (`self.here`): a walkable tile within range 3 of it, off the edge, that no hostile
    /// stands on, that no hostile with a live ATTACK part is within 2 tiles of, and that no other
    /// defender took this tick or stands on; its last post while still one, else the nearest, then
    /// the farthest from the target, then the lowest index. `None` when no tile qualifies.
    fn post(
        &self,
        m: &Member,
        target: Position,
        last: Option<Tile>,
        walkable: impl Fn(Tile) -> bool,
    ) -> Option<Tile> {
        let t = Tile::of(target);
        let mine = Tile::of(m.pos);
        let ok = |c: Tile| {
            !c.is_edge()
                && walkable(c)
                && !self.here.iter().any(|p| {
                    let f = Tile::of(p.pos);
                    f == c || (p.threat.power.attack > 0 && f.range(c) <= MELEE_LEAD)
                })
                && !self.taken.iter().any(|&(r, x)| r == m.remote && x == c)
                && !self.members.iter().any(|o| {
                    o.handle != m.handle && o.pos.room_name() == m.remote && Tile::of(o.pos) == c
                })
        };
        if let Some(l) = last
            && l.range(t) <= REACH
            && ok(l)
        {
            return Some(l);
        }
        tiles_near(t, REACH)
            .filter(|&c| ok(c))
            .min_by_key(|&c| (mine.range(c), REACH.saturating_sub(c.range(t)), c.index()))
    }

    /// Whether this tick's remote defenders need no work unit: no room is threatened, and each
    /// stands where it waits, off the edge and whole, with no armed hostile there: in its remote
    /// room, or in its home when that room is no longer mined (spawning ones included). Each then
    /// only gives way ([`Self::hold_all`]).
    pub fn idle(&self, snap: &Snapshot) -> bool {
        let creeps = &snap.creeps;
        let armed_in = |room: RoomName| {
            creeps
                .positions()
                .iter()
                .zip(creeps.combat())
                .any(|(p, c)| p.room_name() == room && c.armed())
        };
        self.units.is_empty()
            && self.members.iter().all(|m| {
                let post = if m.mined { m.remote } else { m.home };
                m.spawning
                    || (m.pos.room_name() == post
                        && !Tile::of(m.pos).is_edge()
                        && !m.damaged()
                        && !armed_in(post))
            })
    }

    /// Lets every remote defender not spawning give way near the center of the room it waits in,
    /// with no work unit (the glue calls it when [`Self::idle`]).
    pub fn hold_all(&self, snap: &Snapshot, movement: &mut Movement) {
        for m in self.members.iter().filter(|m| !m.spawning) {
            let post = if m.mined { m.remote } else { m.home };
            let center = Tile::new(CENTER, CENTER).and_then(|t| at(post, t));
            movement.give_way(snap, m.handle, center.map(|c| (c, INSIDE)));
        }
    }

    /// Ends the tick: forgets the rooms whose threat is over, and the targets of rooms no defender
    /// stands in.
    pub fn finish(&mut self) {
        let units = &self.units;
        self.rooms
            .retain(|s| units.iter().any(|u| u.home == s.home && u.room == s.room));
        let members = &self.members;
        self.targets
            .retain(|(r, _)| members.iter().any(|m| m.pos.room_name() == *r));
    }

    /// This tick's events.
    pub fn events(&self) -> &[RemoteDefenseEvent] {
        &self.events
    }

    /// This tick's spawn requests. Unit: requests.
    pub fn requested(&self) -> u32 {
        self.requested
    }

    /// The posts this tick's remote defenders took, by room and tile, in work order.
    pub fn posts(&self) -> &[(RoomName, Tile)] {
        &self.taken
    }

    /// Whether `home`'s threatened remote room `room` sends any ready defender in this tick (module
    /// docs); `None` when it was not planned this tick.
    pub fn engaged(&self, home: RoomName, room: RoomName, now: u32) -> Option<bool> {
        self.rooms
            .iter()
            .find(|s| s.home == home && s.room == room && s.planned == now)
            .map(|s| !s.going.is_empty())
    }

    /// The bot's remote defenders this tick. Unit: creeps.
    pub fn count(&self) -> usize {
        self.members.len()
    }
}

/// The live power of the own creep at `row` (module docs), from its body read once and `hits` (as
/// if whole while not read); `None` while its body is not known.
///
/// Uses: rustedscreep_consts::extra::BODYPART_HITS (through `part_hits`, V282, V283).
fn live_power(snap: &Snapshot, row: usize, hits: Option<u32>) -> Option<Power> {
    let body = snap.creeps.body(row)?;
    let parts = body.parts();
    let full = u32::try_from(parts.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(BODYPART_HITS);
    let hits = hits.unwrap_or(full);
    let mut power = Power::default();
    for (index, &part) in parts.iter().enumerate() {
        if part_hits(parts.len(), hits, index) > 0 {
            power.add_part(part);
        }
    }
    Some(power)
}

/// The net energy a creep life of `home`'s remote room `room` under its plan
/// (`economy::RemoteProfit::net`); 0 while it is not priced.
fn remote_net(remotes: &Remotes, home: RoomName, room: RoomName) -> i64 {
    remotes
        .home_states()
        .filter(|(h, _)| *h == home)
        .flat_map(|(_, rooms)| rooms.iter())
        .find(|r| r.room == room)
        .and_then(|r| r.profit)
        .map_or(0, |p| p.net)
}

/// A key for what a sizing was computed from: the whole foes' bodies (their power, hits and
/// effective hits whole), the living team's bodies (not their hits, so a fight does not size again
/// each tick), the defenders counted, the capacity and the config. The search runs again only when
/// one changes (an FNV-1a style fold; deterministic, not cryptographic).
fn sizing_key(
    foes: &[Foe],
    living: &[Fighter],
    have: u32,
    capacity: u32,
    cfg: &DefenseConfig,
) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut fold = |v: u32| h = (h ^ u64::from(v)).wrapping_mul(PRIME);
    for f in foes {
        let p = f.threat.power;
        for v in [
            f.threat.hits,
            p.attack,
            p.ranged,
            p.heal,
            f.threat.effective_hits,
        ] {
            fold(v);
        }
    }
    fold(u32::MAX);
    for f in living {
        for v in [f.damage, f.heal, f.hits_max, f.floor] {
            fold(v);
        }
    }
    for v in [
        have,
        capacity,
        cfg.max_remote_defenders,
        cfg.max_kill_ticks,
        cfg.remote_loss_pct,
    ] {
        fold(v);
    }
    h
}

/// Whether `home` mines `room` now (`economy::Remote::active`).
fn mines(remotes: &Remotes, home: RoomName, room: RoomName) -> bool {
    remotes
        .home_states()
        .filter(|(h, _)| *h == home)
        .flat_map(|(_, rooms)| rooms.iter())
        .any(|r| r.room == room && r.active)
}

/// The Chebyshev range between two positions in one room; `u32::MAX` across rooms.
fn chebyshev(a: Position, b: Position) -> u32 {
    if a.room_name() != b.room_name() {
        return u32::MAX;
    }
    u32::from(Tile::of(a).range(Tile::of(b)))
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
