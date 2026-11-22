//! Inter-room routes: a PathFinder search (made by the glue through [`RouteSearch`]) for a creep
//! whose target is in another room, or out of reach inside its own, kept whole as a [`Route`]: the
//! path's room visits in order, each with the edge tile it is left by.
//!
//! A creep follows one route leg by leg, so exits found by different searches never mix (kept per
//! room, they could send creeps round between two rooms), and a room a path passes through twice is
//! told apart by the leg. Creeps share routes: one in a room that a kept route to its target leaves
//! joins it at that leg when it can reach the leg's exit in the room's grid. A route expires after
//! `route_ttl_ticks`, and goes sooner when a room on it refuses a creep, when it leads into a room
//! with no way on, or when a creep cannot reach its exit in a room whose grid changed since the
//! search. A search that found nothing is kept by the creep that made it for `route_retry_ticks`.
//! Edge tiles a path steps onto and back into the room from (PathFinder allows it, V166; a creep
//! there would change rooms, V151) are learned when they cost a creep its way, and avoided after.

use rustedscreep_consts::{Position, ROOM_AREA, ROOM_SIZE, RoomName};

use super::RouteRooms;
use super::grid::BLOCKED;
use super::tile::Tile;

/// One search, in PathFinder's terms (V163): from `from` to within `range` of `to`, with the
/// terrain costs PathFinder uses where a room has no cost matrix, and its limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteQuery {
    pub from: Position,
    pub to: Position,
    /// Kept inside the target room ([`pathfinder_range`]).
    pub range: u8,
    pub plain_cost: u8,
    pub swamp_cost: u8,
    pub max_ops: u32,
    pub max_rooms: u8,
}

/// How a search ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteOutcome {
    /// It reached the goal: the path is complete.
    Found,
    /// It stopped short (ops, rooms or no way; V166): the path only leads toward the closest tile.
    Incomplete,
    /// It failed outright (a throw, or the origin's room is closed).
    Failed,
}

/// A route search: PathFinder in the game (rustedscreep-wasm), a model of it in tests.
pub trait RouteSearch {
    /// Searches for `query`. The search's room callback answers from `rooms`: a closed room is
    /// impassable, a room with a grid gets its [`super::CostGrid::write_matrix`] bytes, and any
    /// other room [`unknown_room_matrix`] (its terrain costs, corners blocked). Writes the path into `path` (cleared first): the
    /// tiles after `from`, in order, the last one within range of the target (V166).
    fn search(
        &mut self,
        query: &RouteQuery,
        rooms: &RouteRooms<'_>,
        path: &mut Vec<Position>,
    ) -> RouteOutcome;
}

/// The matrix for a room movement has no grid of: 0 everywhere, so PathFinder takes the terrain
/// costs (V164), except 255 on the four corners (and on any tile the caller then avoids). A corner is on two sides, so which room a creep on
/// one would land in is ambiguous; routes never cross there (grids block corners too, since they
/// are no open exits).
///
/// Uses: `ROOM_SIZE`.
pub fn unknown_room_matrix(out: &mut [u8; ROOM_AREA]) {
    out.fill(0);
    let last = ROOM_SIZE - 1;
    for (x, y) in [(0, 0), (0, last), (last, 0), (last, last)] {
        if let Some(cell) = Tile::new(x, y).and_then(|t| out.get_mut(t.index())) {
            *cell = BLOCKED;
        }
    }
}

/// The goal range to give PathFinder for a target and our range, so that no tile of another room
/// is within it. PathFinder measures range across room borders (world coordinates, V166), and a
/// tile `d` tiles from the target's nearest edge has the first tile of the next room `d + 1` away,
/// so `min(range, d)` keeps the goal in the target room; inside it the in-room field takes the
/// creep to the full range.
///
/// Uses: `ROOM_SIZE`.
pub fn pathfinder_range(target: Position, range: u8) -> u8 {
    let (x, y, last) = (target.x().u8(), target.y().u8(), ROOM_SIZE - 1);
    let to_edge = x.min(y).min(last - x.min(last)).min(last - y.min(last));
    range.min(to_edge)
}

/// One room visit of a route: the room, the exit it is left by (`None` on the last leg, in the
/// target's room), and the version of the room's grid the search used (0 when it had none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leg {
    pub room: RoomName,
    pub exit: Option<Tile>,
    pub version: u32,
}

/// The legs of a route's path, into `out`: one per room visit, in order, each with the edge tile it
/// leaves by (the last leg, in the target's room, has none); versions are left 0 for the caller.
/// `from` is the search's origin, which the path leaves out (V166). A crossing must be an edge tile
/// followed by the tile across it in the room on that side (PathFinder crosses straight over, V166,
/// and the engine lands the creep there, V151). Edge tiles the path steps onto and back into the
/// same room from (PathFinder allows it, V166; a creep there would change rooms, V151) go into
/// `dips`: they do not matter while the room's own flow field reaches the exit another way, and are
/// avoided by the next search when it does not. False, with no legs, when a crossing is not like
/// that or the path does not leave the origin room or end in `target_room`.
pub fn route_legs(
    from: Position,
    path: &[Position],
    target_room: RoomName,
    out: &mut Vec<Leg>,
    dips: &mut Vec<(RoomName, Tile)>,
) -> bool {
    out.clear();
    dips.clear();
    let mut leg = Leg {
        room: from.room_name(),
        exit: None,
        version: 0,
    };
    let mut prev = from;
    // The tile before `p` was reached by a crossing (or is the origin): an edge tile there is where
    // the creep lands or stands, not a dip.
    let mut landed = true;
    for &p in path {
        let at = Tile::of(prev);
        if p.room_name() != prev.room_name() {
            let side = at.side();
            let next_room = side.and_then(|s| prev.room_name().checked_add(s.room_offset()));
            if side.is_none()
                || next_room != Some(p.room_name())
                || at.across() != Some(Tile::of(p))
            {
                out.clear();
                return false;
            }
            leg.exit = Some(at);
            out.push(leg);
            leg = Leg {
                room: p.room_name(),
                exit: None,
                version: 0,
            };
            landed = true;
        } else {
            if !landed && at.is_edge() {
                dips.push((prev.room_name(), at));
            }
            landed = false;
        }
        prev = p;
    }
    if prev.room_name() != target_room || out.is_empty() {
        out.clear();
        return false;
    }
    out.push(leg);
    true
}

/// A route a search found: its origin room, target and range, and its legs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub target: Position,
    pub range: u8,
    pub legs: Vec<Leg>,
    pub made_at: u32,
    last_used: u32,
}

impl Route {
    pub fn origin(&self) -> Option<RoomName> {
        self.legs.first().map(|l| l.room)
    }
}

/// A handle to a route in a [`RouteBook`]; it stops resolving once the route is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteId {
    index: u32,
    generation: u32,
}

#[derive(Default)]
struct Slot {
    generation: u32,
    route: Option<Route>,
}

/// Routes kept whole, at most `capacity` of them (least recently used out first, but never one a
/// creep used this tick or the one before: travelers keep theirs, and the book grows past the
/// capacity until they finish). A creep follows
/// one route leg by leg, so exits found by different searches never mix (which could send creeps
/// round between two rooms), and a room a path passes through twice is told apart by the leg.
/// Routes are shared by their origin room: a creep there takes one whose first exit it can reach.
#[derive(Default)]
pub struct RouteBook {
    slots: Vec<Slot>,
    capacity: usize,
}

impl RouteBook {
    /// Sets the most routes kept (at least 1), dropping the least recently used beyond it that no
    /// creep used at `now` or the tick before.
    pub fn set_capacity(&mut self, capacity: usize, now: u32) {
        self.capacity = capacity.max(1);
        while self.len() > self.capacity && self.evict_lru(now) {}
    }

    /// Adds a route and returns its handle.
    pub fn add(&mut self, target: Position, range: u8, legs: &[Leg], now: u32) -> RouteId {
        while self.len() >= self.capacity.max(1) && self.evict_lru(now) {}
        let route = Route {
            target,
            range,
            legs: legs.to_vec(),
            made_at: now,
            last_used: now,
        };
        let index = match self.slots.iter().position(|s| s.route.is_none()) {
            Some(i) => i,
            None => {
                self.slots.push(Slot::default());
                self.slots.len() - 1
            }
        };
        let slot = self.slots.get_mut(index);
        let generation = match slot {
            Some(s) => {
                s.generation = s.generation.wrapping_add(1);
                s.route = Some(route);
                s.generation
            }
            None => 0,
        };
        RouteId {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            generation,
        }
    }

    /// The route, if it is still kept; marks it used.
    pub fn get(&mut self, id: RouteId, now: u32) -> Option<&Route> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        let route = slot.route.as_mut()?;
        route.last_used = now;
        Some(&*route)
    }

    /// The route, if it is still kept, without marking it used.
    pub fn peek(&self, id: RouteId) -> Option<&Route> {
        let slot = self.slots.get(id.index as usize)?;
        (slot.generation == id.generation)
            .then_some(slot.route.as_ref())
            .flatten()
    }

    /// The legs in `room` of routes to `target` within `range` made within `ttl` ticks that leave
    /// the room (a route's suffix from any of its legs is one path, so a creep may join it there),
    /// as (route, leg index, exit), in slot and leg order.
    pub fn through(
        &self,
        room: RoomName,
        target: Position,
        range: u8,
        now: u32,
        ttl: u32,
    ) -> impl Iterator<Item = (RouteId, u16, Tile)> + '_ {
        self.slots.iter().enumerate().flat_map(move |(i, s)| {
            let id = RouteId {
                index: u32::try_from(i).unwrap_or(u32::MAX),
                generation: s.generation,
            };
            s.route
                .as_ref()
                .filter(|r| {
                    now.wrapping_sub(r.made_at) <= ttl && r.target == target && r.range == range
                })
                .into_iter()
                .flat_map(move |r| {
                    r.legs.iter().enumerate().filter_map(move |(j, l)| {
                        let exit = l.exit.filter(|_| l.room == room)?;
                        Some((id, u16::try_from(j).ok()?, exit))
                    })
                })
        })
    }

    /// The routes from `room` to `target` within `range` made within `ttl` ticks, in slot order.
    pub fn from_room(
        &self,
        room: RoomName,
        target: Position,
        range: u8,
        now: u32,
        ttl: u32,
    ) -> impl Iterator<Item = (RouteId, &Route)> + '_ {
        self.slots.iter().enumerate().filter_map(move |(i, s)| {
            let r = s.route.as_ref()?;
            let fresh = now.wrapping_sub(r.made_at) <= ttl;
            (fresh && r.target == target && r.range == range && r.origin() == Some(room)).then(
                || {
                    (
                        RouteId {
                            index: u32::try_from(i).unwrap_or(u32::MAX),
                            generation: s.generation,
                        },
                        r,
                    )
                },
            )
        })
    }

    pub fn remove(&mut self, id: RouteId) {
        if let Some(s) = self.slots.get_mut(id.index as usize)
            && s.generation == id.generation
        {
            s.route = None;
        }
    }

    /// Drops every route that enters `room` (a room that refused a creep, V152, V168).
    pub fn remove_through(&mut self, room: RoomName) {
        for s in &mut self.slots {
            if s.route
                .as_ref()
                .is_some_and(|r| r.legs.iter().skip(1).any(|l| l.room == room))
            {
                s.route = None;
            }
        }
    }

    /// Drops routes older than `ttl`.
    pub fn evict(&mut self, now: u32, ttl: u32) {
        for s in &mut self.slots {
            if s.route
                .as_ref()
                .is_some_and(|r| now.wrapping_sub(r.made_at) > ttl)
            {
                s.route = None;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.route.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Removes the least recently used route that no creep used at `now` or the tick before (on a
    /// tie the first slot, so it is deterministic). False when every route is in use.
    fn evict_lru(&mut self, now: u32) -> bool {
        let lru = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.route.as_ref().map(|r| (r.last_used, i)))
            .filter(|&(used, _)| now.wrapping_sub(used) > 1)
            .min();
        match lru.and_then(|(_, i)| self.slots.get_mut(i)) {
            Some(s) => {
                s.route = None;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::grid::tests::tile;
    use super::*;
    use rustedscreep_consts::RoomCoordinate;

    fn at(room: &str, x: u8, y: u8) -> Position {
        Position::new(
            RoomCoordinate::new(x).expect("x"),
            RoomCoordinate::new(y).expect("y"),
            room.parse().expect("room"),
        )
    }

    fn room(name: &str) -> RoomName {
        name.parse().expect("room")
    }

    /// The room offsets of the four sides name the neighboring rooms (x grows east, y south; the
    /// crate's `checked_add`, V169), and the world's last rooms have no neighbor beyond.
    #[test]
    fn side_offsets_name_the_neighbouring_rooms() {
        use super::super::tile::Side;
        let r = room("E1N1");
        let next = |s: Side| r.checked_add(s.room_offset());
        assert_eq!(next(Side::Top), Some(room("E1N2")));
        assert_eq!(next(Side::Bottom), Some(room("E1N0")));
        assert_eq!(next(Side::Right), Some(room("E2N1")));
        assert_eq!(next(Side::Left), Some(room("E0N1")));
        assert_eq!(
            room("E0N0").checked_add(Side::Left.room_offset()),
            Some(room("W0N0"))
        );
        assert_eq!(
            room("E0S0").checked_add(Side::Top.room_offset()),
            Some(room("E0N0"))
        );
        assert_eq!(room("E127N1").checked_add(Side::Right.room_offset()), None);
    }

    /// Uses: ROOM_SIZE. A path that crosses W1N1 → W0N1 → W0N2 gives one leg per room visit with the
    /// edge tile it leaves by (V151, V166); a room visited twice gives two legs; a path that steps
    /// onto an edge and back into the room reports that tile as a dip (a landing tile is none); a
    /// jump, a skewed crossing or a path that stays in the room or ends in another room gives none.
    #[test]
    fn legs_follow_the_room_visits() {
        let last = ROOM_SIZE - 1;
        let from = at("W1N1", 47, 10);
        let leg = |r: &str, exit: Option<Tile>| Leg {
            room: room(r),
            exit,
            version: 0,
        };
        let path = [
            at("W1N1", 48, 10),
            at("W1N1", last, 11),
            at("W0N1", 0, 11),
            at("W0N1", 1, 10),
            at("W0N1", 1, 1),
            at("W0N1", 2, 0),
            at("W0N2", 2, last),
            at("W0N2", 3, 48),
        ];
        let mut out = Vec::new();
        let mut dips = Vec::new();
        assert!(route_legs(from, &path, room("W0N2"), &mut out, &mut dips));
        assert!(dips.is_empty());
        assert_eq!(
            out,
            vec![
                leg("W1N1", Some(tile(last, 11))),
                leg("W0N1", Some(tile(2, 0))),
                leg("W0N2", None)
            ]
        );
        assert!(!route_legs(from, &path, room("W0N1"), &mut out, &mut dips));
        assert!(out.is_empty());
        let jump = [at("W1N1", 48, 10), at("W0N1", 0, 12)];
        assert!(!route_legs(from, &jump, room("W0N1"), &mut out, &mut dips));
        let skew = [at("W1N1", last, 10), at("W0N1", 0, 11)];
        assert!(!route_legs(from, &skew, room("W0N1"), &mut out, &mut dips));
        // The origin on the exit, and on an edge it just arrived on: no dip.
        let on_edge = at("W1N1", last, 10);
        assert!(route_legs(
            on_edge,
            &[at("W0N1", 0, 10)],
            room("W0N1"),
            &mut out,
            &mut dips
        ));
        let arrived = at("W1N1", 0, 10);
        let inward = [
            at("W1N1", 1, 10),
            at("W1N1", 2, 10),
            at("W1N1", 3, 0),
            at("W1N2", 3, last),
            at("W1N2", 3, 48),
        ];
        assert!(route_legs(
            arrived,
            &inward,
            room("W1N2"),
            &mut out,
            &mut dips
        ));
        assert!(dips.is_empty(), "the origin and a landing tile are no dips");
        // A room visited twice: two legs for it.
        let twice = [
            at("W1N1", 48, 1),
            at("W1N1", 48, 0),
            at("W1N2", 48, last),
            at("W1N2", 47, 48),
            at("W1N2", 46, last),
            at("W1N1", 46, 0),
            at("W1N1", 47, 1),
            at("W1N1", last, 2),
            at("W0N1", 0, 2),
        ];
        assert!(route_legs(from, &twice, room("W0N1"), &mut out, &mut dips));
        assert_eq!(
            out,
            vec![
                leg("W1N1", Some(tile(48, 0))),
                leg("W1N2", Some(tile(46, last))),
                leg("W1N1", Some(tile(last, 2))),
                leg("W0N1", None)
            ]
        );
        // A dip: onto the bottom edge and back in; the route itself is fine.
        let dip = [
            at("W1N1", 47, 48),
            at("W1N1", 48, last),
            at("W1N1", 47, 48),
            at("W1N1", last, 47),
            at("W0N1", 0, 47),
        ];
        assert!(route_legs(from, &dip, room("W0N1"), &mut out, &mut dips));
        assert_eq!(dips, vec![(room("W1N1"), tile(48, last))]);
        // A path that never leaves the room has no route.
        assert!(!route_legs(
            from,
            &[at("W1N1", 47, 11)],
            room("W1N1"),
            &mut out,
            &mut dips
        ));
    }

    /// Uses: ROOM_SIZE, ROOM_AREA. The matrix for a room without a grid leaves everything to the
    /// terrain but the corners, which no route crosses.
    #[test]
    fn unknown_rooms_block_only_their_corners() {
        let mut bytes = [7u8; ROOM_AREA];
        unknown_room_matrix(&mut bytes);
        for (i, &b) in bytes.iter().enumerate() {
            let t = Tile::from_index(i).expect("tile");
            let corner = t.is_edge() && t.side().is_none();
            assert_eq!(b, if corner { BLOCKED } else { 0 }, "{t:?}");
        }
        assert_eq!(bytes.iter().filter(|&&b| b == BLOCKED).count(), 4);
    }

    /// Uses: ROOM_SIZE. The PathFinder range keeps the goal away from the next room: never more than
    /// the target's distance to its nearest edge.
    #[test]
    fn the_pathfinder_range_stays_in_the_target_room() {
        let last = ROOM_SIZE - 1;
        assert_eq!(pathfinder_range(at("W1N1", 25, 25), 3), 3);
        assert_eq!(pathfinder_range(at("W1N1", 2, 25), 3), 2);
        assert_eq!(pathfinder_range(at("W1N1", 25, last - 1), 3), 1);
        assert_eq!(pathfinder_range(at("W1N1", 0, 25), 3), 0);
        assert_eq!(pathfinder_range(at("W1N1", 25, last), 1), 0);
        for x in 0..ROOM_SIZE {
            let t = at("W1N1", x, 7);
            let r = pathfinder_range(t, u8::MAX);
            // The nearest tile of another room is one past the edge.
            let to_out = x.min(last - x).min(7) + 1;
            assert!(r < to_out, "{x}");
        }
    }

    /// Routes are kept whole and found by their origin room, target and range within the ttl; a
    /// removed route's handle stops resolving (even when its slot is reused), routes through a room
    /// can be dropped, and the least recently used goes first at capacity.
    #[test]
    fn the_route_book_keeps_whole_routes() {
        let (a, b, c) = (room("W1N1"), room("W2N1"), room("W3N1"));
        let target = at("W3N1", 10, 10);
        let ttl = 100;
        let legs = |rooms: &[RoomName]| -> Vec<Leg> {
            rooms
                .iter()
                .enumerate()
                .map(|(i, &r)| Leg {
                    room: r,
                    exit: (i + 1 < rooms.len()).then(|| tile(0, 5)),
                    version: 1,
                })
                .collect()
        };
        let mut book = RouteBook::default();
        book.set_capacity(2, 0);
        let ab = book.add(target, 1, &legs(&[a, b, c]), 0);
        let bc = book.add(target, 1, &legs(&[b, c]), 0);
        assert_eq!(book.get(ab, 1).map(|r| r.legs.len()), Some(3));
        let from_a: Vec<RouteId> = book
            .from_room(a, target, 1, 50, ttl)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(from_a, vec![ab]);
        let through_b: Vec<(RouteId, u16)> = book
            .through(b, target, 1, 50, ttl)
            .map(|(id, leg, _)| (id, leg))
            .collect();
        assert_eq!(
            through_b,
            vec![(ab, 1), (bc, 0)],
            "a middle leg and an origin"
        );
        assert_eq!(
            book.through(c, target, 1, 50, ttl).count(),
            0,
            "the last legs leave nothing"
        );
        assert_eq!(
            book.from_room(a, target, 2, 50, ttl).count(),
            0,
            "another range"
        );
        assert_eq!(
            book.from_room(a, target, 1, 101, ttl).count(),
            0,
            "ttl passed"
        );
        // At capacity the least recently used (bc, last used at 0) goes, but not while in use: at
        // tick 1 both were used within a tick, so the book grows instead.
        let live = book.add(target, 1, &legs(&[c]), 1);
        assert_eq!(book.len(), 3, "nothing evicted while in use");
        book.remove(live);
        let third = book.add(target, 1, &legs(&[c]), 3);
        assert!(book.get(bc, 3).is_none());
        assert!(book.get(ab, 3).is_some());
        book.remove(ab);
        assert!(book.get(ab, 4).is_none());
        let again = book.add(target, 1, &legs(&[a, b, c]), 5);
        assert!(
            book.get(ab, 5).is_none(),
            "an old handle does not see the new route"
        );
        assert!(book.get(again, 5).is_some());
        book.remove_through(b);
        assert!(book.get(again, 6).is_none(), "it entered b");
        assert!(book.get(third, 6).is_some());
        book.evict(1000, ttl);
        assert!(book.is_empty());
    }
}
