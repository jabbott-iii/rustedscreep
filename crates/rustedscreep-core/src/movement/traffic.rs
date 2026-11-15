//! The traffic manager: one room's own creeps, their wanted steps and who stands where, resolved
//! into at most one destination per tile before any move is issued.
//!
//! The engine resolves moves into the same tile by its own rates and drops a move into a tile whose
//! creep stays (V51, V52); a dropped move is still billed (V34). So the manager hands each tile to
//! one creep and moves a creep into an occupied tile only when the occupant moves out in the same
//! tick: on along its own route, back into the mover's tile (a swap, which the engine allows because
//! both move, V52), or aside (a shove of a creep that holds its place, kept within its anchor).
//!
//! Movers go in priority order (highest first, then by id for determinism). Each one claims its best
//! free step; a step held by another creep starts a search for a chain of creeps that can each move
//! on (an augmenting path, at most `depth` creeps deep). A chain commits whole or not at all, so a
//! failed search changes nothing. Creeps that cannot move (fatigued, V50) and foreign creeps are
//! fixed. Shoves are capped by the intents the tick can still afford.

use rustedscreep_consts::ROOM_AREA;

use super::field::{Goal, Steps};
use super::grid::CostGrid;
use super::tile::Tile;

/// What one creep wants this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Step onto one of these tiles, best first.
    Move(Steps),
    /// Stay. A mover may shove it to an adjacent tile within `anchor` (any tile when `None`); an
    /// anchor of range 0 pins it to its goal tile.
    Hold(Option<Goal>),
}

/// One own creep in the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    pub tile: Tile,
    /// Not fatigued, not spawning, and not held as fixed after the game refused its move (no
    /// active MOVE part; V50, V108). A creep that cannot move is never moved and blocks its tile.
    pub can_move: bool,
    pub plan: Plan,
    /// Higher goes first.
    pub priority: u8,
    /// The tie-break among equal priorities (the creep's id key), so the result does not depend on
    /// the engine's object order (V102).
    pub order: u128,
    /// For a mover: the most shoves this tick (all rooms so far included) its chains may bring the
    /// total to, which the CPU left to the tier of the job that asked buys (0.2 CPU each, V34).
    pub shove_cap: u32,
}

/// The bounds of one resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// How many creeps deep a chain may go below the mover.
    pub depth: u8,
    /// Most tiles one mover's search may try to claim, so a crowded room cannot make the
    /// exponential chain search expensive.
    pub claims: u32,
    /// Shoves already made this tick in other rooms (each unit's `shove_cap` counts them).
    pub shoves_before: u32,
}

/// What one resolution did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Movers that got one of their steps.
    pub moved: u32,
    /// Holding creeps moved aside.
    pub shoved: u32,
    /// Movers that stay: every step was taken or blocked, or the creep cannot move.
    pub blocked: u32,
}

const VISITING: u8 = 1;
const LOCKED: u8 = 2;
const SHOVED: u8 = 4;

/// The manager's scratch, kept between ticks so resolving allocates nothing once warm.
pub struct Traffic {
    /// Unit index + 1 of the creep assigned to each tile; 0 for none.
    owner: Box<[u16; ROOM_AREA]>,
    /// Tiles a chain in progress is claiming; no other member may take them.
    reserved: Box<[bool; ROOM_AREA]>,
    /// Tiles with a foreign creep this resolution, marked with `stamp`.
    foreign: Box<[u32; ROOM_AREA]>,
    stamp: u32,
    touched: Vec<Tile>,
    assigned: Vec<Tile>,
    state: Vec<u8>,
    queue: Vec<u16>,
    shoves: u32,
    max_shoves: u32,
    claims_left: u32,
}

impl Default for Traffic {
    fn default() -> Self {
        Self {
            owner: Box::new([0; ROOM_AREA]),
            reserved: Box::new([false; ROOM_AREA]),
            foreign: Box::new([0; ROOM_AREA]),
            stamp: 0,
            touched: Vec::new(),
            assigned: Vec::new(),
            state: Vec::new(),
            queue: Vec::new(),
            shoves: 0,
            max_shoves: 0,
            claims_left: 0,
        }
    }
}

/// Shared inputs of one resolution.
struct Ctx<'a> {
    grid: &'a CostGrid,
    units: &'a [Unit],
}

impl Traffic {
    /// Resolves one room. `units` are the room's own creeps that are not spawning (at most
    /// `u16::MAX - 1`; any beyond are left in place), `foreign` the tiles no own creep may enter
    /// (other players' creeps, and movement's suspect tiles). A chain is
    /// at most `limits.depth` creeps deep below the mover, and one mover tries at most
    /// `limits.claims` tiles; a mover's chains may bring the tick's shoves (from
    /// `limits.shoves_before`) up to its `shove_cap`. Afterwards [`Self::assigned`] gives each unit's
    /// tile for the end of the tick; [`Resolved::shoved`] counts this room's shoves only.
    pub fn resolve(
        &mut self,
        grid: &CostGrid,
        units: &[Unit],
        foreign: &[Tile],
        limits: Limits,
    ) -> Resolved {
        let Limits {
            depth,
            claims,
            shoves_before,
        } = limits;
        for &t in &self.touched {
            if let Some(o) = self.owner.get_mut(t.index()) {
                *o = 0;
            }
        }
        self.touched.clear();
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.foreign.fill(0);
            self.stamp = 1;
        }
        for &t in foreign {
            if let Some(f) = self.foreign.get_mut(t.index()) {
                *f = self.stamp;
            }
        }
        let units = units
            .get(..units.len().min(usize::from(u16::MAX - 1)))
            .unwrap_or(&[]);
        self.assigned.clear();
        self.state.clear();
        self.queue.clear();
        for (i, u) in units.iter().enumerate() {
            self.assigned.push(u.tile);
            self.state.push(0);
            let id = u16::try_from(i + 1).unwrap_or(u16::MAX);
            if let Some(o) = self.owner.get_mut(u.tile.index())
                && *o == 0
            {
                *o = id;
                self.touched.push(u.tile);
            }
            if matches!(u.plan, Plan::Move(_)) {
                self.queue.push(id - 1);
            }
        }
        self.queue.sort_unstable_by_key(|&i| {
            let u = units.get(usize::from(i));
            (
                std::cmp::Reverse(u.map_or(0, |u| u.priority)),
                u.map_or(0, |u| u.order),
                i,
            )
        });
        self.shoves = shoves_before;
        let cx = Ctx { grid, units };
        let mut out = Resolved::default();
        for k in 0..self.queue.len() {
            let Some(&i) = self.queue.get(k) else { break };
            let i = usize::from(i);
            if self.flag(i, LOCKED) {
                // Already moved along its own route as part of an earlier chain.
                out.moved += 1;
                continue;
            }
            let Some(u) = units.get(i) else { continue };
            self.max_shoves = u.shove_cap;
            self.claims_left = claims;
            let moved = u.can_move && self.relocate(&cx, i, depth, 0, None);
            if moved {
                out.moved += 1;
            } else {
                out.blocked += 1;
            }
        }
        out.shoved = self.shoves.saturating_sub(shoves_before);
        out
    }

    /// Each unit's tile at the end of the tick, in `units` order.
    pub fn assigned(&self) -> &[Tile] {
        &self.assigned
    }

    /// Whether unit `i` holds its place but was moved aside.
    pub fn shoved(&self, i: usize) -> bool {
        self.flag(i, SHOVED)
    }

    fn flag(&self, i: usize, f: u8) -> bool {
        self.state.get(i).is_some_and(|s| s & f != 0)
    }

    fn set_flag(&mut self, i: usize, f: u8, on: bool) {
        if let Some(s) = self.state.get_mut(i) {
            if on {
                *s |= f;
            } else {
                *s &= !f;
            }
        }
    }

    fn owner_of(&self, t: Tile) -> Option<usize> {
        match self.owner.get(t.index()).copied().unwrap_or(0) {
            0 => None,
            id => Some(usize::from(id) - 1),
        }
    }

    fn is_reserved(&self, t: Tile) -> bool {
        self.reserved.get(t.index()).copied().unwrap_or(true)
    }

    fn set_reserved(&mut self, t: Tile, on: bool) {
        if let Some(r) = self.reserved.get_mut(t.index()) {
            *r = on;
        }
    }

    fn enterable(&self, cx: &Ctx<'_>, t: Tile) -> bool {
        cx.grid.walkable(t) && self.foreign.get(t.index()) != Some(&self.stamp)
    }

    /// Moves unit `c` (currently somewhere else) onto `t`.
    fn move_into(&mut self, c: usize, t: Tile) {
        let id = u16::try_from(c + 1).unwrap_or(u16::MAX);
        if let Some(&from) = self.assigned.get(c)
            && let Some(o) = self.owner.get_mut(from.index())
            && *o == id
        {
            *o = 0;
        }
        if let Some(o) = self.owner.get_mut(t.index()) {
            *o = id;
        }
        self.touched.push(t);
        if let Some(a) = self.assigned.get_mut(c) {
            *a = t;
        }
    }

    /// Unit `c` takes tile `t`, moving its occupant on if needed. On false nothing changed.
    fn claim(&mut self, cx: &Ctx<'_>, c: usize, t: Tile, depth: u8, chain_shoves: u32) -> bool {
        if self.claims_left == 0 || !self.enterable(cx, t) || self.is_reserved(t) {
            return false;
        }
        self.claims_left -= 1;
        let Some(o) = self.owner_of(t) else {
            self.move_into(c, t);
            return true;
        };
        if o == c {
            return true;
        }
        if self.flag(o, VISITING) {
            // `o` is in this chain and leaving `t`, which no member has claimed: a swap or a cycle.
            self.move_into(c, t);
            return true;
        }
        let can_move = cx.units.get(o).is_some_and(|u| u.can_move);
        if self.flag(o, LOCKED) || !can_move || depth == 0 {
            return false;
        }
        self.set_flag(c, VISITING, true);
        self.set_reserved(t, true);
        let requester = self.assigned.get(c).copied();
        let ok = self.relocate(cx, o, depth - 1, chain_shoves, requester);
        self.set_reserved(t, false);
        self.set_flag(c, VISITING, false);
        if ok {
            self.move_into(c, t);
        }
        ok
    }

    /// Moves unit `o` off its tile: a mover along its own steps, a holding creep aside. `requester`
    /// is the tile of the creep that wants `o`'s tile, if any. On false nothing changed.
    fn relocate(
        &mut self,
        cx: &Ctx<'_>,
        o: usize,
        depth: u8,
        chain_shoves: u32,
        requester: Option<Tile>,
    ) -> bool {
        let Some(unit) = cx.units.get(o).copied() else {
            return false;
        };
        match unit.plan {
            Plan::Move(steps) => {
                for t in steps.tiles() {
                    if self.claim(cx, o, t, depth, chain_shoves) {
                        self.set_flag(o, LOCKED, true);
                        return true;
                    }
                }
                false
            }
            Plan::Hold(anchor) => {
                if self.shoves.saturating_add(chain_shoves) >= self.max_shoves {
                    return false;
                }
                let from = self.assigned.get(o).copied().unwrap_or(unit.tile);
                // Candidates, best first: a free tile beside the requester (a sidestep out of its
                // way), the requester's own tile (a swap: the creep ends behind the mover, which never
                // pushes it again), another free tile, a tile of another creep in this chain, then a
                // tile whose creep must move on in turn. Within a rank the cheapest tile, then
                // direction order. A free tile ahead of the mover would be pushed again next tick.
                let mut cands: [(u8, u8, usize, Option<Tile>); 8] = [(u8::MAX, 0, 0, None); 8];
                for (i, (_, t)) in from.neighbors().enumerate() {
                    if anchor.is_some_and(|g| !g.contains(t))
                        || !self.enterable(cx, t)
                        || self.is_reserved(t)
                    {
                        continue;
                    }
                    let rank = match self.owner_of(t) {
                        None if requester.is_some_and(|r| r.range(t) == 1) => 0,
                        Some(_) if requester == Some(t) => 1,
                        None => 2,
                        Some(w) if self.flag(w, VISITING) => 3,
                        Some(_) => 4,
                    };
                    if let Some(slot) = cands.get_mut(i) {
                        *slot = (rank, cx.grid.cost(t), i, Some(t));
                    }
                }
                cands.sort_unstable();
                let mut done = false;
                for &(_, _, _, t) in &cands {
                    let Some(t) = t else { continue };
                    if self.claim(cx, o, t, depth, chain_shoves + 1) {
                        done = true;
                        break;
                    }
                }
                if done {
                    self.set_flag(o, LOCKED | SHOVED, true);
                    self.shoves += 1;
                }
                done
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::grid::tests::{open_terrain, rates, tile};
    use super::super::grid::{CostGrid, Footprint, TerrainGrid};
    use super::*;

    fn grid(objects: &[(Tile, Footprint)]) -> CostGrid {
        let terrain: TerrainGrid = open_terrain();
        let mut g = CostGrid::default();
        g.build(&terrain, objects.iter().copied(), rates());
        g
    }

    /// A mover whose chains may shove without limit; `lim` caps a whole resolution's shoves.
    fn mover(at: Tile, to: &[Tile], priority: u8, order: u128) -> Unit {
        Unit {
            tile: at,
            can_move: true,
            plan: Plan::Move(Steps::from_tiles(to.iter().copied(), 0)),
            priority,
            order,
            shove_cap: u32::MAX,
        }
    }

    fn holder(at: Tile, anchor: Option<Goal>, order: u128) -> Unit {
        Unit {
            tile: at,
            can_move: true,
            plan: Plan::Hold(anchor),
            priority: 0,
            order,
            shove_cap: 0,
        }
    }

    /// Resolves with `depth`, a claim budget that does not bind, and every unit's shove cap at
    /// `shoves`.
    fn resolve_with(
        t: &mut Traffic,
        g: &CostGrid,
        units: &[Unit],
        foreign: &[Tile],
        depth: u8,
        shoves: u32,
    ) -> Resolved {
        let capped: Vec<Unit> = units
            .iter()
            .map(|u| Unit {
                shove_cap: shoves,
                ..*u
            })
            .collect();
        let limits = Limits {
            depth,
            claims: u32::MAX,
            shoves_before: 0,
        };
        t.resolve(g, &capped, foreign, limits)
    }

    /// Checks the invariants of any resolution: no two units end on one tile, every unit ends on
    /// its own tile or an adjacent enterable one, a unit that cannot move stays, and a unit ends on
    /// an occupied tile only if its occupant left it.
    fn check(units: &[Unit], t: &Traffic, g: &CostGrid, foreign: &[Tile]) {
        let a = t.assigned();
        assert_eq!(a.len(), units.len());
        for (i, u) in units.iter().enumerate() {
            for j in i + 1..units.len() {
                assert_ne!(a[i], a[j], "units {i} and {j} share {:?}", a[i]);
            }
            if a[i] != u.tile {
                assert!(u.can_move, "unit {i} cannot move");
                assert_eq!(u.tile.range(a[i]), 1, "unit {i} jumps");
                assert!(
                    g.walkable(a[i]) && !foreign.contains(&a[i]),
                    "unit {i} enters {:?}",
                    a[i]
                );
                if let Some(j) = units.iter().position(|w| w.tile == a[i]) {
                    assert_ne!(a[j], a[i], "unit {j} stayed on the tile unit {i} entered");
                }
            }
        }
    }

    #[test]
    fn a_free_step_is_taken_and_priority_wins_a_contested_tile() {
        let g = grid(&[]);
        let target = tile(11, 10);
        let units = [
            mover(tile(10, 10), &[target], 1, 2),
            mover(tile(12, 10), &[target], 5, 1),
            mover(tile(20, 20), &[tile(21, 20)], 0, 3),
        ];
        let mut t = Traffic::default();
        let r = resolve_with(&mut t, &g, &units, &[], 2, 10);
        check(&units, &t, &g, &[]);
        assert_eq!(t.assigned()[1], target, "priority 5 wins");
        assert_eq!(t.assigned()[0], tile(10, 10));
        assert_eq!(t.assigned()[2], tile(21, 20));
        assert_eq!(
            r,
            Resolved {
                moved: 2,
                shoved: 0,
                blocked: 1
            }
        );
        // Equal priority: the lower order (id) wins, whatever the slice order.
        let tie = [
            mover(tile(10, 10), &[target], 1, 9),
            mover(tile(12, 10), &[target], 1, 4),
        ];
        resolve_with(&mut t, &g, &tie, &[], 2, 10);
        assert_eq!(t.assigned()[1], target);
    }

    #[test]
    fn a_second_option_is_used_when_the_first_is_taken() {
        let g = grid(&[]);
        let units = [
            mover(tile(10, 10), &[tile(11, 10)], 9, 1),
            mover(tile(10, 11), &[tile(11, 10), tile(11, 11)], 1, 2),
        ];
        let mut t = Traffic::default();
        resolve_with(&mut t, &g, &units, &[], 2, 10);
        check(&units, &t, &g, &[]);
        assert_eq!(t.assigned(), [tile(11, 10), tile(11, 11)]);
    }

    /// Two movers heading into each other's tiles swap (the engine lets both move, V52); a mover
    /// into a holding creep swaps with it when the creep has no better free tile in its anchor.
    #[test]
    fn swaps() {
        let g = grid(&[]);
        let (a, b) = (tile(10, 10), tile(11, 10));
        let units = [mover(a, &[b], 1, 1), mover(b, &[a], 1, 2)];
        let mut t = Traffic::default();
        let r = resolve_with(&mut t, &g, &units, &[], 1, 0);
        check(&units, &t, &g, &[]);
        assert_eq!(t.assigned(), [b, a]);
        assert_eq!((r.moved, r.shoved), (2, 0));
        // A holder pinned to range 1 of a tile left of it: only the mover's tile fits.
        let anchor = Goal {
            tile: tile(9, 10),
            range: 1,
        };
        let units = [
            mover(a, &[b], 1, 1),
            holder(b, Some(Goal { tile: a, range: 0 }), 2),
        ];
        resolve_with(&mut t, &g, &units, &[], 1, 10);
        check(&units, &t, &g, &[]);
        assert_eq!(t.assigned(), [b, a], "range 0 at a: a swap");
        assert!(t.shoved(1));
        // Anchored within 1 of (9, 10), the holder's tiles are a, (10, 9) and (10, 11); the free
        // ones come first, the first in direction order of equal cost: (10, 11).
        let units = [mover(a, &[b], 1, 1), holder(b, Some(anchor), 2)];
        resolve_with(&mut t, &g, &units, &[], 1, 10);
        check(&units, &t, &g, &[]);
        assert_eq!(t.assigned(), [b, tile(10, 11)]);
    }

    /// A holder with free tiles around moves to the cheapest one (a road), not back into the mover.
    #[test]
    fn a_shove_prefers_a_free_cheap_tile() {
        let road = tile(11, 11);
        let g = grid(&[(road, Footprint::Road)]);
        let (a, b) = (tile(10, 10), tile(11, 10));
        let units = [mover(a, &[b], 1, 1), holder(b, None, 2)];
        let mut t = Traffic::default();
        let r = resolve_with(&mut t, &g, &units, &[], 1, 10);
        check(&units, &t, &g, &[]);
        assert_eq!(t.assigned(), [b, road]);
        assert_eq!(
            r,
            Resolved {
                moved: 1,
                shoved: 1,
                blocked: 0
            }
        );
        // With no shove budget the mover waits.
        let r = resolve_with(&mut t, &g, &units, &[], 1, 0);
        assert_eq!(t.assigned(), [a, b]);
        assert_eq!(r.blocked, 1);
    }

    /// A train: each mover steps into the tile of the one ahead. The tail (the lowest id) goes
    /// first and pushes the whole train when the depth allows it.
    #[test]
    fn trains_move_together() {
        let g = grid(&[]);
        let row: Vec<Tile> = (0..6).map(|i| tile(10 + i, 10)).collect();
        // Unit k stands on row[k] and wants row[k + 1]; row[5] is free.
        let units: Vec<Unit> = (0..5)
            .map(|k| mover(row[k], &[row[k + 1]], 1, k as u128))
            .collect();
        let mut t = Traffic::default();
        let r = resolve_with(&mut t, &g, &units, &[], 4, 0);
        check(&units, &t, &g, &[]);
        assert_eq!((r.moved, r.blocked), (5, 0));
        assert_eq!(t.assigned(), &row[1..]);
        // Depth 0: no creep can push the one ahead, and they go tail first, so only the leader moves.
        let r = resolve_with(&mut t, &g, &units, &[], 0, 0);
        check(&units, &t, &g, &[]);
        assert_eq!((r.moved, r.blocked), (1, 4));
        assert_eq!(t.assigned()[4], row[5]);
    }

    /// A rotation of three movers all succeed. In a 1-wide corridor a holder swaps with the mover
    /// rather than pushing on; when its anchor rules out the swap it pushes the next holder on; and
    /// when the far end is fixed, the reserved-tile rule keeps the second holder out of the tile the
    /// mover is claiming, so nobody moves instead of two creeps ending on one tile.
    #[test]
    fn cycles_and_the_reserved_tile_rule() {
        let g = grid(&[]);
        let (a, b, c) = (tile(10, 10), tile(11, 10), tile(11, 11));
        let units = [
            mover(a, &[b], 1, 1),
            mover(b, &[c], 1, 2),
            mover(c, &[a], 1, 3),
        ];
        let mut t = Traffic::default();
        let r = resolve_with(&mut t, &g, &units, &[], 3, 0);
        check(&units, &t, &g, &[]);
        assert_eq!(t.assigned(), [b, c, a]);
        assert_eq!(r.moved, 3);
        // The corridor: y = 10 from x = 10 to 14, walled on every other side.
        let mut walls = Vec::new();
        for x in 9..16 {
            walls.push((tile(x, 9), Footprint::Blocked));
            walls.push((tile(x, 11), Footprint::Blocked));
        }
        walls.push((tile(9, 10), Footprint::Blocked));
        walls.push((tile(15, 10), Footprint::Blocked));
        let g = grid(&walls);
        let m = mover(tile(10, 10), &[tile(11, 10)], 1, 1);
        let units = [
            m,
            holder(tile(11, 10), None, 2),
            holder(tile(12, 10), None, 3),
        ];
        let r = resolve_with(&mut t, &g, &units, &[], 3, 10);
        check(&units, &t, &g, &[]);
        assert_eq!(
            t.assigned(),
            [tile(11, 10), tile(10, 10), tile(12, 10)],
            "a swap"
        );
        assert_eq!((r.moved, r.shoved), (1, 1));
        let ahead = Some(Goal {
            tile: tile(12, 10),
            range: 1,
        });
        let units = [
            m,
            holder(tile(11, 10), ahead, 2),
            holder(tile(12, 10), None, 3),
        ];
        let r = resolve_with(&mut t, &g, &units, &[], 3, 10);
        check(&units, &t, &g, &[]);
        assert_eq!(
            t.assigned(),
            [tile(11, 10), tile(12, 10), tile(13, 10)],
            "pushed on"
        );
        assert_eq!((r.moved, r.shoved), (1, 2));
        let tired = Unit {
            can_move: false,
            ..holder(tile(13, 10), None, 4)
        };
        let units = [
            m,
            holder(tile(11, 10), ahead, 2),
            holder(tile(12, 10), None, 3),
            tired,
        ];
        let r = resolve_with(&mut t, &g, &units, &[], 3, 10);
        check(&units, &t, &g, &[]);
        assert_eq!(
            t.assigned(),
            [tile(10, 10), tile(11, 10), tile(12, 10), tile(13, 10)]
        );
        assert_eq!((r.moved, r.shoved, r.blocked), (0, 0, 1));
    }

    /// Foreign creeps, tired creeps and blocked tiles are never entered; a tired mover stays.
    #[test]
    fn fixed_obstacles() {
        let g = grid(&[(tile(11, 11), Footprint::Blocked)]);
        let a = tile(10, 10);
        let units = [
            mover(a, &[tile(11, 10), tile(11, 11), tile(10, 11)], 1, 1),
            Unit {
                can_move: false,
                ..holder(tile(10, 11), None, 2)
            },
            Unit {
                can_move: false,
                ..mover(tile(20, 20), &[tile(21, 20)], 1, 3)
            },
        ];
        let foreign = [tile(11, 10)];
        let mut t = Traffic::default();
        let r = resolve_with(&mut t, &g, &units, &foreign, 3, 10);
        check(&units, &t, &g, &foreign);
        assert_eq!(t.assigned(), [a, tile(10, 11), tile(20, 20)]);
        assert_eq!(
            r,
            Resolved {
                moved: 0,
                shoved: 0,
                blocked: 2
            }
        );
        // The next resolution forgets the foreign tile.
        let r = resolve_with(&mut t, &g, &units[..1], &[], 3, 10);
        assert_eq!(t.assigned(), [tile(11, 10)]);
        assert_eq!(r.moved, 1);
    }

    /// A crowd: many seeded scenarios on a small open patch, checked against the invariants, with
    /// the same input giving the same output.
    #[test]
    fn seeded_crowds_keep_the_invariants() {
        let g = grid(&[
            (tile(15, 15), Footprint::Blocked),
            (tile(16, 14), Footprint::Blocked),
        ]);
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut t = Traffic::default();
        let mut again = Traffic::default();
        for _ in 0..300 {
            let mut taken = Vec::new();
            let mut units = Vec::new();
            let n = 2 + (next() % 14) as usize;
            while units.len() < n {
                let at = tile(12 + (next() % 7) as u8, 12 + (next() % 7) as u8);
                if taken.contains(&at) || !g.walkable(at) {
                    continue;
                }
                taken.push(at);
                let order = next() as u128;
                let u = match next() % 4 {
                    0 => holder(at, None, order),
                    1 => holder(at, Some(Goal { tile: at, range: 1 }), order),
                    _ => {
                        let opts: Vec<Tile> = at
                            .neighbors()
                            .map(|(_, t)| t)
                            .filter(|_| next() % 3 == 0)
                            .take(3)
                            .collect();
                        mover(at, &opts, (next() % 3) as u8, order)
                    }
                };
                let can_move = next() % 8 != 0;
                units.push(Unit { can_move, ..u });
            }
            let foreign = [tile(12 + (next() % 7) as u8, 19)];
            let depth = (next() % 5) as u8;
            let shoves = (next() % 4) as u32;
            let r = resolve_with(&mut t, &g, &units, &foreign, depth, shoves);
            check(&units, &t, &g, &foreign);
            assert!(r.shoved <= shoves);
            let movers = units
                .iter()
                .filter(|u| matches!(u.plan, Plan::Move(_)))
                .count() as u32;
            assert_eq!(r.moved + r.blocked, movers);
            resolve_with(&mut again, &g, &units, &foreign, depth, shoves);
            assert_eq!(t.assigned(), again.assigned(), "deterministic");
        }
    }

    /// Each mover's chains may bring the tick's shoves only up to its own cap, counting the shoves
    /// of earlier rooms (`shoves_before`); the claim budget stops a mover's search.
    #[test]
    fn shove_caps_and_the_claim_budget() {
        let g = grid(&[]);
        // Two movers in different spots, each needing one shove; the first in order has cap 2, the
        // second cap 1 (a lower tier's budget). With one shove made in an earlier room, only the
        // first gets its shove.
        let units = [
            Unit {
                shove_cap: 2,
                ..mover(tile(10, 10), &[tile(11, 10)], 5, 1)
            },
            holder(tile(11, 10), None, 2),
            Unit {
                shove_cap: 1,
                ..mover(tile(30, 30), &[tile(31, 30)], 1, 3)
            },
            holder(tile(31, 30), None, 4),
        ];
        let mut t = Traffic::default();
        let limits = |shoves_before| Limits {
            depth: 2,
            claims: u32::MAX,
            shoves_before,
        };
        let r = t.resolve(&g, &units, &[], limits(1));
        check(&units, &t, &g, &[]);
        assert_eq!((r.moved, r.shoved, r.blocked), (1, 1, 1));
        assert_eq!(t.assigned()[0], tile(11, 10));
        assert_eq!(
            t.assigned()[2],
            tile(30, 30),
            "its cap of 1 is already used"
        );
        // From 0 both fit: the first brings the total to 1, the second's cap of 1 is then reached.
        let r = t.resolve(&g, &units, &[], limits(0));
        assert_eq!((r.moved, r.shoved), (1, 1));
        // A claim budget of 1 lets a mover take a free tile but not push a creep on (that needs a
        // second claim).
        let units = [
            mover(tile(10, 10), &[tile(11, 10)], 1, 1),
            holder(tile(11, 10), None, 2),
        ];
        let r = t.resolve(
            &g,
            &units,
            &[],
            Limits {
                depth: 2,
                claims: 1,
                shoves_before: 0,
            },
        );
        assert_eq!((r.moved, r.blocked), (0, 1));
        let free = [mover(tile(10, 10), &[tile(11, 10)], 1, 1)];
        let r = t.resolve(
            &g,
            &free,
            &[],
            Limits {
                depth: 2,
                claims: 1,
                shoves_before: 0,
            },
        );
        assert_eq!(r.moved, 1);
    }
}
