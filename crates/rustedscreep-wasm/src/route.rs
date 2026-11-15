//! Inter-room route searches (ROADMAP P2, movement; rustedscreep-core `movement::route`): the core's
//! [`RouteSearch`] over the game's `PathFinder.search`.
//!
//! The search is bound here with `catch`: native PathFinder throws when the search reaches a room
//! with no terrain data (V165), and screeps-game-api's own binding has none, so a throw would unwind
//! through the wasm (V167). The room callback is one `'static` closure made once per global; it
//! answers from [`RouteState`], which holds a copy of each of movement's cost grids as PathFinder
//! bytes, with the edge tiles paths must not step onto and back from set to 255 (refreshed only
//! when the grid's version or those tiles changed), and the JS CostMatrix made from it on first
//! use. A room that is closed (a refused transfer) or whose `Game.map.getRoomStatus` differs
//! from the origin room's (novice and respawn areas are walled off, closed rooms cannot be entered;
//! V168) is blocked (`false`); a room movement has no grid of gets one shared matrix that leaves the
//! terrain to PathFinder but blocks the corners.
//!
//! JS calls per search: 2 room positions (`Object.create` and the `__packedPos` setter each), the
//! goal object and its 2 setters, the options object and its 5 setters (`plainCost`, `swampCost`,
//! `maxOps`, `maxRooms`, `roomCallback`), `PathFinder.search`, `incomplete` and `ops`, and for a
//! found path the `path` getter and 2 per step (the array read and `__packedPos`). Per room callback:
//! the call in and the name's decode (2), the room's status when not read within
//! `route_ttl_ticks` (the name, `getRoomStatus`, the `status` read and its decode: 4), and a
//! CostMatrix the first time a grid version is used (`Object.create`, the bytes' copy and the `_bits`
//! setter: 3).

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{JsString, Object, Reflect};
use rustedscreep_consts::{Position, ROOM_AREA, RoomName};
use rustedscreep_core::movement::route::unknown_room_matrix;
use rustedscreep_core::movement::{BLOCKED, RouteOutcome, RouteQuery, RouteRooms, RouteSearch};
use rustedscreep_core::snapshot::parse_room_name;
use screeps::pathfinder::{JsSearchOptions, SearchResults};
use screeps::{CostMatrix, RoomPosition};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

#[wasm_bindgen]
extern "C" {
    /// `PathFinder.search` with `catch` (V165, V167).
    #[wasm_bindgen(js_namespace = PathFinder, js_name = search, catch)]
    fn pathfinder_search(
        origin: &RoomPosition,
        goal: &JsValue,
        options: &JsValue,
    ) -> Result<SearchResults, JsValue>;

    /// `Game.map.getRoomStatus` with `catch`; the result is read with `Reflect`, so an `undefined`
    /// answer (a name it does not accept, V168) cannot throw.
    #[wasm_bindgen(js_namespace = ["Game", "map"], js_name = getRoomStatus, catch)]
    fn get_room_status(name: &JsString) -> Result<JsValue, JsValue>;

    /// PathFinder's goal object, `{ pos, range }` (V163).
    type Goal;

    #[wasm_bindgen(method, setter = pos)]
    fn set_pos(this: &Goal, pos: &RoomPosition);

    #[wasm_bindgen(method, setter = range)]
    fn set_range(this: &Goal, range: u32);
}

/// A room's status as `Game.map.getRoomStatus` gives it (V168); `None` when it could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Normal,
    Closed,
    Novice,
    Respawn,
    Other,
}

fn read_status(room: RoomName) -> Option<Status> {
    let name = JsString::from(room);
    let result = get_room_status(&name).ok()?;
    let status = Reflect::get(&result, &JsValue::from_str("status")).ok()?;
    Some(match status.as_string()?.as_str() {
        "normal" => Status::Normal,
        "closed" => Status::Closed,
        "novice" => Status::Novice,
        "respawn" => Status::Respawn,
        _ => Status::Other,
    })
}

/// One room's PathFinder bytes (its grid, or the unknown-room matrix, with the edge tiles paths must
/// not step onto and back from set to 255), what they were made from, and the CostMatrix made from
/// them on first use.
struct RoomMatrix {
    name: RoomName,
    /// The grid version (0 without a grid) and a fingerprint of the avoided edge tiles (their
    /// count and an order-free hash of the tiles, so any change in the set shows).
    version: u32,
    dips: (u32, u32),
    bits: Box<[u8; ROOM_AREA]>,
    matrix: Option<CostMatrix>,
}

/// Counters since the global started, for `rustedscreep.movement()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct RouteStats {
    pub searches: u32,
    /// Searches that threw (V165), and that stopped short (V166).
    pub throws: u32,
    pub incomplete: u32,
    /// PathFinder ops the searches used (V166).
    pub ops: u64,
    pub callbacks: u32,
    /// Rooms the callback blocked: closed after a refused transfer, of another status, or a name it
    /// could not read.
    pub blocked_closed: u32,
    pub blocked_status: u32,
    pub bad_names: u32,
}

/// What the room callback answers from, shared between the search and its closure.
#[derive(Default)]
pub struct RouteState {
    /// Sorted by packed name.
    rooms: Vec<RoomMatrix>,
    closed: Vec<RoomName>,
    /// Room statuses and the tick each was read, sorted by packed name.
    statuses: Vec<(RoomName, Option<Status>, u32)>,
    origin: Option<Status>,
    unknown: Option<CostMatrix>,
    now: u32,
    status_ttl: u32,
    status_cap: usize,
    pub stats: RouteStats,
}

impl RouteState {
    /// This tick, and how long a status read stays good and how many are kept (`route_ttl_ticks`;
    /// the movement glue's floor over `route_cache_size`).
    pub fn begin(&mut self, now: u32, status_ttl: u32, status_cap: u32) {
        self.now = now;
        self.status_ttl = status_ttl.max(1);
        self.status_cap = usize::try_from(status_cap).unwrap_or(usize::MAX).max(1);
    }

    /// Rooms with a matrix copied from movement's grids.
    pub fn matrices(&self) -> usize {
        self.rooms.len()
    }

    /// Takes movement's grids and avoided edge tiles (copying a room's bytes only when its grid
    /// version or its avoided tiles changed) and closed rooms.
    fn sync(&mut self, known: &RouteRooms<'_>) {
        self.closed.clear();
        self.closed.extend(known.closed_rooms());
        self.rooms
            .retain(|m| known.grid(m.name).is_some() || known.dips(m.name).next().is_some());
        let grids = known.grids().map(|(n, v, g)| (n, v, Some(g)));
        let dip_only = known
            .dip_rooms()
            .filter(|&n| known.grid(n).is_none())
            .map(|n| (n, 0, None));
        for (name, version, grid) in grids.chain(dip_only) {
            let dips = known.dips(name).fold((0u32, 0u32), |(n, h), t| {
                (n.wrapping_add(1), h.wrapping_add(mix(t.index())))
            });
            let write = |bits: &mut [u8; ROOM_AREA]| match grid {
                Some(g) => g.write_matrix(bits, known.dips(name)),
                None => {
                    unknown_room_matrix(bits);
                    for t in known.dips(name) {
                        if let Some(b) = bits.get_mut(t.index()) {
                            *b = BLOCKED;
                        }
                    }
                }
            };
            match self
                .rooms
                .binary_search_by_key(&name.packed_repr(), |m| m.name.packed_repr())
            {
                Ok(i) => {
                    if let Some(m) = self.rooms.get_mut(i)
                        && (m.version, m.dips) != (version, dips)
                    {
                        write(&mut m.bits);
                        m.version = version;
                        m.dips = dips;
                        m.matrix = None;
                    }
                }
                Err(i) => {
                    let mut bits = Box::new([0u8; ROOM_AREA]);
                    write(&mut bits);
                    self.rooms.insert(
                        i,
                        RoomMatrix {
                            name,
                            version,
                            dips,
                            bits,
                            matrix: None,
                        },
                    );
                }
            }
        }
    }

    /// The room's status, read again once `status_ttl` ticks old.
    fn status(&mut self, room: RoomName) -> Option<Status> {
        let (now, ttl) = (self.now, self.status_ttl);
        match self
            .statuses
            .binary_search_by_key(&room.packed_repr(), |s| s.0.packed_repr())
        {
            Ok(i) => {
                let entry = self.statuses.get_mut(i)?;
                if now.wrapping_sub(entry.2) > ttl {
                    *entry = (room, read_status(room), now);
                }
                entry.1
            }
            Err(i) => {
                let status = read_status(room);
                self.statuses.insert(i, (room, status, now));
                if self.statuses.len() > self.status_cap
                    && let Some(oldest) = self
                        .statuses
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.0 != room)
                        .min_by_key(|(j, s)| (s.2, *j))
                        .map(|(j, _)| j)
                {
                    self.statuses.remove(oldest);
                }
                status
            }
        }
    }

    /// The room callback's answer for `name` (V163, V165): `false` to block the room, else a
    /// CostMatrix.
    fn answer(&mut self, name: &JsString) -> JsValue {
        self.stats.callbacks = self.stats.callbacks.saturating_add(1);
        let text = String::from(name);
        let Some(room) = parse_room_name(text.as_bytes()) else {
            self.stats.bad_names = self.stats.bad_names.saturating_add(1);
            return JsValue::FALSE;
        };
        if self.closed.contains(&room) {
            self.stats.blocked_closed = self.stats.blocked_closed.saturating_add(1);
            return JsValue::FALSE;
        }
        let status = self.status(room);
        if status != self.origin || status == Some(Status::Closed) {
            self.stats.blocked_status = self.stats.blocked_status.saturating_add(1);
            return JsValue::FALSE;
        }
        if let Ok(i) = self
            .rooms
            .binary_search_by_key(&room.packed_repr(), |m| m.name.packed_repr())
            && let Some(m) = self.rooms.get_mut(i)
        {
            let bits = &m.bits;
            let matrix = m
                .matrix
                .get_or_insert_with(|| CostMatrix::new_from_bits(&bits[..]));
            return handle(matrix);
        }
        let unknown = self.unknown.get_or_insert_with(|| {
            let mut bits = [0u8; ROOM_AREA];
            unknown_room_matrix(&mut bits);
            CostMatrix::new_from_bits(&bits)
        });
        handle(unknown)
    }
}

/// Another handle to the same JS CostMatrix. `CostMatrix::clone` is the JS method, which copies the
/// matrix (a call and 2500 bytes); the `JsValue` clone only takes a new reference.
fn handle(matrix: &CostMatrix) -> JsValue {
    let value: &JsValue = matrix.as_ref();
    value.clone()
}

/// A tile index spread over 32 bits (a multiply-xorshift), so that the sum over a set of tiles
/// tells sets apart.
fn mix(index: usize) -> u32 {
    let mut x = u32::try_from(index)
        .unwrap_or(u32::MAX)
        .wrapping_mul(0x9E37_79B9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^ (x >> 13)
}

/// The route search movement calls (installed by the movement glue).
pub struct PathFinderRoutes {
    state: Rc<RefCell<RouteState>>,
    callback: Closure<dyn FnMut(JsString) -> JsValue>,
}

impl PathFinderRoutes {
    /// The search and the state it shares with its callback (the glue keeps a handle for the tick
    /// and the console).
    pub fn new() -> (Self, Rc<RefCell<RouteState>>) {
        let state = Rc::new(RefCell::new(RouteState::default()));
        let shared = Rc::clone(&state);
        let callback = Closure::new(move |name: JsString| -> JsValue {
            match shared.try_borrow_mut() {
                Ok(mut s) => s.answer(&name),
                // Unreachable: the search drops its borrow before PathFinder runs. Block the room.
                Err(_) => JsValue::FALSE,
            }
        });
        let handle = Rc::clone(&state);
        (Self { state, callback }, handle)
    }
}

impl RouteSearch for PathFinderRoutes {
    fn search(
        &mut self,
        query: &RouteQuery,
        rooms: &RouteRooms<'_>,
        path: &mut Vec<Position>,
    ) -> RouteOutcome {
        path.clear();
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return RouteOutcome::Failed;
            };
            st.sync(rooms);
            st.origin = st.status(query.from.room_name());
            st.stats.searches = st.stats.searches.saturating_add(1);
            // A target room the callback would block (closed, or of another status) can never be
            // reached: PathFinder would only spend its ops (V165).
            let goal_room = query.to.room_name();
            let goal_status = st.status(goal_room);
            if st.closed.contains(&goal_room)
                || goal_status != st.origin
                || goal_status == Some(Status::Closed)
            {
                st.stats.blocked_status = st.stats.blocked_status.saturating_add(1);
                return RouteOutcome::Failed;
            }
        }
        let options = JsSearchOptions::new();
        options.plain_cost(query.plain_cost);
        options.swamp_cost(query.swamp_cost);
        options.max_ops(query.max_ops);
        options.max_rooms(query.max_rooms);
        options.room_callback(&self.callback);
        let goal: Goal = Object::new().unchecked_into();
        goal.set_pos(&RoomPosition::from(query.to));
        goal.set_range(u32::from(query.range));
        let origin = RoomPosition::from(query.from);
        let result = pathfinder_search(&origin, &goal, &options);
        let Ok(mut st) = self.state.try_borrow_mut() else {
            return RouteOutcome::Failed;
        };
        let Ok(found) = result else {
            st.stats.throws = st.stats.throws.saturating_add(1);
            return RouteOutcome::Failed;
        };
        st.stats.ops = st.stats.ops.saturating_add(u64::from(found.ops()));
        if found.incomplete() {
            st.stats.incomplete = st.stats.incomplete.saturating_add(1);
            return RouteOutcome::Incomplete;
        }
        for step in found.opaque_path().iter() {
            let step: RoomPosition = step.unchecked_into();
            path.push(Position::from(&step));
        }
        RouteOutcome::Found
    }
}
