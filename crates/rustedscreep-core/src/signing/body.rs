//! The signer's body (ADR-030): one MOVE part for a trip no seen tower reaches, else the cheapest
//! body of TOUGH parts in front and MOVE parts behind that survives the worst case of the target
//! room's towers.
//!
//! # The walk's model
//!
//! A trip is the route search's path (the tiles after the creep's start, the last next to the
//! controller). The creep stands on each tile for some ticks, and every tick a tower can hit it
//! where it stands. [`Exposure`] sums the tiles by the kind of step onto them ([`Step`]) and the
//! tower damage of a tick on them, so any body's time and damage follow in a few operations:
//!
//! - A tile the creep leaves the room from (an exit tile, followed by the next room's edge tile)
//!   is never stood on at the start of a tick: the engine moves the creep across in the same tick
//!   (V151). It is left out.
//! - A step onto an edge tile from inside the room, or the landing after a crossing, makes no
//!   fatigue (V54): one tick there ([`Step::Free`]).
//! - Any other step makes `MOVE_COST_PLAIN` or `MOVE_COST_SWAMP` fatigue per weighing part, the
//!   rate of the tile stepped onto (V54, V154). Every TOUGH part weighs, dead or not (V284), and
//!   each MOVE part takes 2 off each tick (V55), so a tile holds a body of `t` TOUGH and `m` MOVE
//!   parts for `max(1, ceil(t * rate / (2 * m)))` ticks. A body of only MOVE parts never weighs
//!   and walks a tile a tick anywhere. Roads are not known, so a road counts as its terrain (a
//!   longer stay: more damage and time, never less). A tunnel (a road through a wall) counts as a
//!   plain.
//! - The creep signs on its first tick on the last tile, and a sign lands even on the tick the
//!   creep's hits reach 0 (V278), so that tick's damage is not counted. The walk's time is the
//!   first step off the start, the stays before the last tile, and the signing tick.
//!
//! # Surviving it
//!
//! Damage takes the front parts first (V283), and an unboosted TOUGH part is only hit points
//! (`BODYPART_HITS` each, V279, V282). So the TOUGH parts in front go first, then the MOVE parts
//! one by one. A dead MOVE part stops taking off fatigue (V284; the model counts it dead at once,
//! never a tick late), and a dead TOUGH part still weighs, so a body that has lost MOVE parts walks
//! slower and stays longer under the towers. The walk before the towers is counted by step kind;
//! from the first tile a tower reaches it is walked a tick at a time ([`Exposure::walk`]). The
//! body survives when it reaches the last tile with hits above 0 and a MOVE part for every step
//! before it. A creep in a towered room takes at least `tower_damage(TOWER_FALLOFF_RANGE)` (150)
//! a tick, so a body of `MAX_CREEP_SIZE` parts lives there at most about 34 ticks, and the walk
//! stays short.

use rustedscreep_consts::extra::BODYPART_HITS;
use rustedscreep_consts::{MAX_CREEP_SIZE, MOVE_COST_PLAIN, MOVE_COST_SWAMP, Part, Position};

use crate::movement::{TerrainGrid, TerrainKind, Tile};
use crate::planner::validate::tower_damage;
use crate::spawning::Body;

/// The fatigue each MOVE part takes off a tick (creeps/tick.js, V55).
const MOVE_POWER: u64 = 2;

/// The kind of step onto a tile, for the walk's time (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Onto an edge tile from inside, or the landing after a crossing: no fatigue (V54).
    Free,
    /// Fatigue `MOVE_COST_PLAIN` per weighing part.
    Plain,
    /// Fatigue `MOVE_COST_SWAMP` per weighing part.
    Swamp,
}

impl Step {
    const fn slot(self) -> usize {
        match self {
            Self::Free => 0,
            Self::Plain => 1,
            Self::Swamp => 2,
        }
    }

    /// Fatigue per weighing part.
    ///
    /// Uses: `MOVE_COST_PLAIN`, `MOVE_COST_SWAMP`.
    const fn rate(self) -> u64 {
        match self {
            Self::Free => 0,
            Self::Plain => MOVE_COST_PLAIN as u64,
            Self::Swamp => MOVE_COST_SWAMP as u64,
        }
    }

    /// Ticks a body of `tough` TOUGH and `moves` MOVE parts stays on a tile after this step.
    fn dwell(self, tough: u32, moves: u32) -> u64 {
        let fatigue = u64::from(tough).saturating_mul(self.rate());
        let shed = MOVE_POWER.saturating_mul(u64::from(moves)).max(1);
        fatigue.div_ceil(shed).max(1)
    }
}

/// A trip's tiles before the signing tile: the ones no tower reaches before the first one a
/// tower does, counted by [`Step`], then from that one on each tile in walk order with the tower
/// damage of a tick there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exposure {
    safe: [u32; 3],
    danger: Vec<(Step, u32)>,
}

/// How a body's walk over an [`Exposure`] ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Walked {
    /// The walk's ticks: the first step off the start, the stays before the last tile, and the
    /// signing tick. Unit: ticks.
    pub ticks: u64,
    /// The tower damage before the signing tick, raised by the margin. Unit: hits.
    pub damage: u64,
    /// It reaches the last tile with hits left and signs.
    pub survives: bool,
}

impl Exposure {
    /// Adds a tile stood on before the signing tile, with the damage of one tick there.
    pub fn add(&mut self, step: Step, damage: u32) {
        if damage == 0 && self.danger.is_empty() {
            if let Some(t) = self.safe.get_mut(step.slot()) {
                *t = t.saturating_add(1);
            }
        } else {
            self.danger.push((step, damage));
        }
    }

    /// The tiles added. Unit: tiles.
    pub fn tiles(&self) -> usize {
        self.safe
            .iter()
            .map(|&t| usize::try_from(t).unwrap_or(usize::MAX))
            .fold(self.danger.len(), usize::saturating_add)
    }

    /// Whether any tower reaches a tile added.
    pub fn dangerous(&self) -> bool {
        self.danger.iter().any(|&(_, d)| d > 0)
    }

    /// The walk's ticks for a body of `tough` TOUGH and `moves` MOVE parts with no damage (the
    /// safe tiles' stays, the tiles towers reach at full pace). Unit: ticks.
    pub fn ticks(&self, tough: u32, moves: u32) -> u64 {
        self.walk(tough, moves, 0).ticks
    }

    /// Walks a body of `tough` TOUGH parts in front of `moves` MOVE parts over the tiles, with each
    /// tick's damage raised by `margin_pct` (module docs). From the first tile a tower reaches it
    /// goes a tick at a time, in the engine's order (V278, V55): the move (refused while fatigued,
    /// or with no MOVE part left), then the fatigue the MOVE parts left take off, then the tick's
    /// damage at the tile the creep stood on.
    ///
    /// Uses: `extra::BODYPART_HITS`.
    pub fn walk(&self, tough: u32, moves: u32, margin_pct: u32) -> Walked {
        let hits = u64::from(BODYPART_HITS);
        let total = hits.saturating_mul(u64::from(tough.saturating_add(moves)));
        let front = hits.saturating_mul(u64::from(tough));
        // The first step off the start and the signing tick, then the safe tiles at full pace.
        let mut ticks = [Step::Free, Step::Plain, Step::Swamp]
            .into_iter()
            .fold(2u64, |a, step| {
                let n = u64::from(self.safe.get(step.slot()).copied().unwrap_or(0));
                a.saturating_add(n.saturating_mul(step.dwell(tough, moves)))
            });
        // The MOVE parts left after `d` damage: the TOUGH parts in front go first (V283).
        let left = |d: u64| -> u64 {
            let lost = d.saturating_sub(front) / hits;
            u64::from(moves).saturating_sub(lost)
        };
        let made = |step: Step| u64::from(tough).saturating_mul(step.rate());
        let mut damage = 0u64;
        // The fatigue on the first tile towers reach: its step's, less one tick's shed at full pace.
        let mut fatigue = self.danger.first().map_or(0, |&(step, _)| {
            made(step).saturating_sub(MOVE_POWER.saturating_mul(u64::from(moves)))
        });
        let mut j = 0usize;
        while let Some(&(_, raw)) = self.danger.get(j) {
            ticks = ticks.saturating_add(1);
            let able = left(damage);
            let moved = fatigue == 0 && able > 0;
            if moved {
                // The step onto the next tile (the signing tile's fatigue never matters).
                fatigue = self.danger.get(j + 1).map_or(0, |&(next, _)| made(next));
            }
            fatigue = fatigue.saturating_sub(MOVE_POWER.saturating_mul(able));
            let per_tick = u64::from(raw)
                .saturating_mul(100 + u64::from(margin_pct))
                .div_ceil(100);
            damage = damage.saturating_add(per_tick);
            if damage >= total || (!moved && able == 0) {
                return Walked {
                    ticks,
                    damage,
                    survives: false,
                };
            }
            if moved {
                j += 1;
            }
        }
        Walked {
            ticks,
            damage,
            survives: damage < total,
        }
    }
}

/// The step onto `path[i]`, or `None` for an exit tile (the next tile is in another room) or past
/// the path's end. `from` is where the path starts; `terrain` is the tile's room's.
pub fn step_at(from: Position, path: &[Position], i: usize, terrain: &TerrainGrid) -> Option<Step> {
    let here = *path.get(i)?;
    if path
        .get(i.saturating_add(1))
        .is_some_and(|n| n.room_name() != here.room_name())
    {
        return None;
    }
    let before = if i == 0 {
        Some(from)
    } else {
        path.get(i - 1).copied()
    };
    let tile = Tile::of(here);
    if tile.is_edge() {
        let landed = before.is_none_or(|b| b.room_name() != here.room_name());
        let from_inside = before.is_some_and(|b| !Tile::of(b).is_edge());
        if landed || from_inside {
            return Some(Step::Free);
        }
    }
    Some(match terrain.kind(tile) {
        TerrainKind::Swamp => Step::Swamp,
        TerrainKind::Plain | TerrainKind::Wall => Step::Plain,
    })
}

/// The damage of one tick at `at` from `towers` (every one firing, V239, V281).
///
/// Uses: `TOWER_POWER_ATTACK`, `TOWER_OPTIMAL_RANGE`, `TOWER_FALLOFF_RANGE`, `TOWER_FALLOFF`
/// (through `planner::validate::tower_damage`).
pub fn damage_at(at: Tile, towers: &[Tile]) -> u32 {
    towers
        .iter()
        .fold(0u32, |a, &t| a.saturating_add(tower_damage(at.range(t))))
}

/// The plain signer: one MOVE part.
pub fn plain_signer() -> Option<Body> {
    Body::new(&[Part::Move])
}

/// The cheapest body (then the one with the fewest parts) of TOUGH parts in front and MOVE parts
/// behind, at most `MAX_CREEP_SIZE` parts and `capacity` energy, whose walk over `exposure` takes
/// at most `life` ticks and survives its damage raised by `margin_pct` (module docs). `None` when
/// no such body exists.
///
/// Uses: `MAX_CREEP_SIZE`, `BODYPART_COST` (through `Part::cost`), `extra::BODYPART_HITS`.
pub fn signer_body(exposure: &Exposure, life: u32, capacity: u32, margin_pct: u32) -> Option<Body> {
    let mut best: Option<(u32, u32, u32, u32)> = None;
    for moves in 1..=MAX_CREEP_SIZE {
        for tough in 0..=MAX_CREEP_SIZE - moves {
            let cost = tough
                .saturating_mul(Part::Tough.cost())
                .saturating_add(moves.saturating_mul(Part::Move.cost()));
            // Costs rise with `tough`: past the budget or the best so far, so does every later one.
            if cost > capacity || best.is_some_and(|b| cost > b.0) {
                break;
            }
            let walked = exposure.walk(tough, moves, margin_pct);
            if walked.ticks > u64::from(life) {
                // More TOUGH parts only slow the walk.
                break;
            }
            if !walked.survives {
                continue;
            }
            let key = (cost, tough + moves, tough, moves);
            if best.is_none_or(|b| (key.0, key.1) < (b.0, b.1)) {
                best = Some(key);
            }
        }
    }
    let (_, _, tough, moves) = best?;
    let mut parts = Vec::with_capacity(usize::try_from(tough + moves).unwrap_or(0));
    parts.extend((0..tough).map(|_| Part::Tough));
    parts.extend((0..moves).map(|_| Part::Move));
    Body::new(&parts)
}
