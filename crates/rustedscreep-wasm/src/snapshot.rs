//! Snapshot reader (JS → Rust; ROADMAP P1 module 5). One pass at the start of the tick fills
//! `rustedscreep_core::snapshot::Snapshot` and keeps this tick's JS objects in [`JsWorld`], row for
//! row, so the act layer can call methods on them without `Game.getObjectById`.
//!
//! Every value the snapshot keeps is checked: an id, name or packed position that is missing or
//! malformed skips that one object, counts it in `Snapshot::skipped`, and the pass goes on.
//! screeps-game-api's panicking conveniences are not used (VERIFICATION_LOG V100): `HasId::raw_id`
//! and `try_raw_id` (expect), `Room::name` (expect), `HasPosition::pos` (`Position::from_packed`
//! asserts) and `String::from(JsString)` (throws on a non-string). A JS exception from a getter (for
//! example an object without `pos`, which the engine never makes) still ends the tick, and the
//! loader recovers (ADR-003).
//!
//! JS calls per tick, counted in the generated glue (V100, V101):
//! - tick context: 4 (`time`, `limit`, `tickLimit`, `bucket`);
//! - each of `Game.creeps`, `Game.rooms`, `Game.structures`: 3 (the getter, `Object.values`,
//!   `length`), plus one `Array.get` per element, counted below;
//! - own creep: 7 (`get`, `id`, the id string copy, `pos`, `__packedPos`, `spawning`, and
//!   `fatigue` unless it is spawning; the last since P2.1, V160), plus on the tick its handle is
//!   new `name` and the name copy (P2.3; its identity, `spawning::CreepName`); unless it is
//!   spawning, `store`, `getUsedCapacity` and `getCapacity` for energy (P2.4, V196, V197), and
//!   `ticksToLive` on each tick until it reads a number, normally once or twice in its life and
//!   once after a global reset (P5.2, V198; its death tick is kept with its handle);
//! - room: 8 with a controller (`get`, `name`, the name copy, `controller`, `my`, `level`, `find`,
//!   `length`), 6 without, plus `energyAvailable` and `energyCapacityAvailable` when the bot owns
//!   it (P2.3, V175), and for an owned room its controller's 3 progress and timer getters and its
//!   room objects (P2.4, `push_objects`: 6 `find` calls and their `length`s, then per object `get`,
//!   `id`, the id copy, its amount, a container's `hits` (P3.5), and on the tick it is first seen
//!   its position and type);
//!   hostile creep: 5 (`get`, `id`, the id copy, `pos`, `__packedPos`; hostile results never
//!   include spawning creeps, V101), and in a remote room named by an own creep other than a scout
//!   2 more, `getActiveBodyparts` for ATTACK and for RANGED_ATTACK (P6.3, V269);
//! - remote room (P6.2, ADR-029: a visible room the bot does not own that an own creep's name gives
//!   as its remote room; `push_remote`): an owned room's 6 `find` calls and room objects, then the
//!   controller as an object (`id`, the copy, its position and type when first seen, `reservation`,
//!   and for a reservation its `username` and `ticksToEnd`, V268), `FIND_HOSTILE_STRUCTURES` and
//!   its `length` (V267), and on ticks divisible by `remote.road_read_ticks`, in a room an own remote
//!   hauler stands in, the roads through the engine's filter, each with `get`, `id`, the copy,
//!   `hits` and `hitsMax`, and its position and type (the handle is released between reads). The
//!   rooms an own scout stands in are read the same way (its tour, ADR-029). The bot's username:
//!   an own creep's `owner` and `username`, once a global;
//! - signer room (ADR-030, `push_sign`: a visible room an own signer stands in, owned or not): its
//!   controller as an object (unless the remote read pushed it), the controller's `sign` and, for
//!   one, its `username` and `text` (V275), and `FIND_HOSTILE_STRUCTURES` through the tower filter,
//!   each tower's `pos` and `__packedPos`;
//! - structure: 3 (`get`, `id`, the id copy), plus on the tick it is first seen `structureType`,
//!   and `pos` and `__packedPos` when the type is known (again every tick while that read fails).
//!   With `cache.structure_static` off (the naive baseline, P1.10) those three are read every tick.
//!   A spawn adds `spawning` (P2.3, V175); while it is spawning the `Spawning` object comes back
//!   through an `Option` (below), and dropping it runs in wasm (V100). A spawn or an extension in a
//!   room that is not full adds `store` and `getFreeCapacity` for energy (P2.4, V196). A link adds
//!   `store` and `getUsedCapacity` for energy every tick, and `cooldown` while it holds energy (an
//!   empty link cannot send, so its cooldown is never read) (P3.2, V216).
//!
//! Calls back from JS into wasm: one `__wbindgen_malloc` per id or name copy, and one
//! `__externref_table_alloc` (with a JS `Table.set`) per object returned through an `Option`: every
//! creep's `id`, a room's `controller` when it has one, and a busy spawn's `Spawning` (P2.3).
//!
//! The id and name copies are separate calls (`__wbindgen_string_get`) because that call is the
//! type check: a getter typed `-> Option<String>` would save one call per object but traps on a
//! non-string (V100). Each copy is one ASCII string in wasm memory, freed in the same tick.
//!
//! Skipped objects are counted every tick and logged through [`log_skips`], at most one line per
//! `log.skip_window_ticks` (`rustedscreep_core::snapshot::SkipLog`).

use std::fmt::Write as _;

use js_sys::{Array, Object, Reflect};
use rustedscreep_core::config::{CacheConfig, RemoteConfig};
use rustedscreep_core::signing::SIGN_TEXT;
use rustedscreep_core::snapshot::{
    Combat, Controller, ControllerState, CreepRow, CreepStore, ObjectKey, ObjectKind, ObjectRow,
    ObjectStatic, RemoteRead, Reservation, RoomEnergy, SignRead, SignSeen, SkipCounts, SkipLog,
    SkipReason, SkipReport, Snapshot, StructureStatic, TickContext, known_structure_type,
    parse_room_name, position_from_packed,
};
use rustedscreep_core::spawning::{CreepName, Identity, Role};
use screeps::{
    ConstructionSite, Creep, FindConstant, HasId, MaybeHasId, OwnedStructureProperties, Part,
    Position, Resource, ResourceType, Room, RoomName, RoomObject, SYSTEM_USERNAME, Source, Store,
    Structure, StructureController, StructureLink, StructureSpawn, StructureType, find, game,
};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

#[wasm_bindgen]
extern "C" {
    /// The `Game` global. Screeps builds a new one every tick; a structural getter reads the global
    /// on every call (V100), as screeps-game-api's own bindings do.
    type Game;

    /// `Game.structures` as a plain object, so its values can be read as base `Structure`s without
    /// the `structureType` read that screeps-game-api's `StructureObject` conversion makes for each
    /// one every tick (the snapshot reads the type once per structure).
    #[wasm_bindgen(static_method_of = Game, getter = structures)]
    fn structures_object() -> Object;

    /// Any JS object with a `name` property. A method binding on screeps-game-api's own `Room` type
    /// would be an inherent impl on a foreign type (E0116, V104), so rooms are cast to this one.
    type Named;

    /// `name` as a plain value; screeps-game-api's `Room::name` panics when it does not parse.
    #[wasm_bindgen(method, getter = name)]
    fn name_value(this: &Named) -> JsValue;

    /// A room, for `find` without screeps-game-api's `Room::find`, which collects into a new `Vec`.
    pub(crate) type Findable;

    /// `room.find(type)` as the engine's array (a clone of its per-tick cache, V101).
    #[wasm_bindgen(method, js_name = find)]
    pub(crate) fn find_array(this: &Findable, ty: find::Find) -> Array;

    /// A `RoomPosition`, for reading `__packedPos` as an option.
    type Packed;

    /// `__packedPos`, or `None` when it is undefined or null (the crate's `packed()` reads that as
    /// 0, which is the valid position (0, 0) in W127N127).
    #[wasm_bindgen(method, getter = __packedPos)]
    fn packed_pos(this: &Packed) -> Option<u32>;

    /// `room.find(type, opts)`: the engine applies `opts.filter` to its cached list (V199).
    #[wasm_bindgen(method, js_name = find)]
    fn find_filtered(this: &Findable, ty: find::Find, opts: &JsValue) -> Array;

    /// Any object with a `store` (creeps, spawns, extensions, containers, tombstones, ruins), for
    /// its `Store` without a typed cast (P2.4, V196).
    type Stored;

    #[wasm_bindgen(method, getter = store)]
    fn store_of(this: &Stored) -> Store;

    /// Any object with an `id`, read as a plain value (sources, sites, piles, tombstones, ruins).
    type WithId;

    #[wasm_bindgen(method, getter = id)]
    fn id_value(this: &WithId) -> JsValue;

    /// A controller, for `reservation` as a plain value (undefined without one; P6.2, V263).
    type Reserving;

    #[wasm_bindgen(method, getter = reservation)]
    fn reservation_value(this: &Reserving) -> JsValue;

    /// A reservation (`{username, ticksToEnd}`), or an owner (`{username}`), read as plain values.
    type UserRecord;

    #[wasm_bindgen(method, getter = username)]
    fn username_value(this: &UserRecord) -> JsValue;

    #[wasm_bindgen(method, getter = ticksToEnd)]
    fn ticks_to_end_value(this: &UserRecord) -> JsValue;

    /// A creep, for its `owner` as a plain value (the bot's username, P6.2).
    type HasOwner;

    #[wasm_bindgen(method, getter = owner)]
    fn owner_value(this: &HasOwner) -> JsValue;

    /// A controller, for `sign` as a plain value (undefined without one; ADR-030, V275).
    type Signed;

    #[wasm_bindgen(method, getter = sign)]
    fn sign_value(this: &Signed) -> JsValue;

    /// A sign (`{username, text, time, datetime}`), read as plain values (V275).
    type SignRecord;

    #[wasm_bindgen(method, getter = username)]
    fn sign_username(this: &SignRecord) -> JsValue;

    #[wasm_bindgen(method, getter = text)]
    fn sign_text(this: &SignRecord) -> JsValue;

    /// A structure, for `hitsMax` (a road's, P6.2).
    type WithHitsMax;

    #[wasm_bindgen(method, getter = hitsMax)]
    fn hits_max_value(this: &WithHitsMax) -> JsValue;
}

/// This tick's JS objects, index-aligned with the snapshot's rows. They are stale after this tick
/// (screeps-game-api `game` docs), so the next pass drops them first.
#[derive(Default)]
pub struct JsWorld {
    pub rooms: Vec<Room>,
    pub creeps: Vec<Creep>,
    pub structures: Vec<Structure>,
    /// Sources, own sites, piles, tombstones, ruins and containers of owned rooms (P2.4), and of
    /// remote rooms with their controllers and roads (P6.2).
    pub objects: Vec<RoomObject>,
    /// `{filter: {structureType: "container"}}`, built once per global: a plain object, not a
    /// game object, so it stays valid across ticks.
    container_filter: Option<JsValue>,
    /// `{filter: {structureType: "road"}}`, likewise (P6.2).
    road_filter: Option<JsValue>,
    /// `{filter: {structureType: "tower"}}`, likewise (ADR-030).
    tower_filter: Option<JsValue>,
    /// The bot's username, read once per global from an own creep's `owner` (P6.2): a remote
    /// room's reservation is the bot's when it names it.
    username: Option<String>,
    /// Scratch: the remote rooms own creeps' names give this tick, and the rooms own scouts stand
    /// in (their tour's rooms), sorted (P6.2).
    remote_names: Vec<RoomName>,
    /// Scratch: the rooms an own remote hauler stands in this tick, sorted: only their roads are
    /// read (P6.2).
    hauler_rooms: Vec<RoomName>,
    /// Scratch: the remote rooms named by own creeps other than scouts (the rooms the bot mines),
    /// sorted: only their hostiles' combat parts are read (P6.3).
    crew_rooms: Vec<RoomName>,
    /// Scratch: the rooms an own signer stands in this tick, sorted: their controller's sign and
    /// the towers of other users are read (ADR-030).
    sign_rooms: Vec<RoomName>,
}

impl JsWorld {
    fn clear(&mut self) {
        self.rooms.clear();
        self.creeps.clear();
        self.structures.clear();
        self.objects.clear();
    }

    /// Whether every table has exactly one JS object per row (the act layer relies on it).
    pub fn aligned_with(&self, snapshot: &Snapshot) -> bool {
        self.rooms.len() == snapshot.rooms.len()
            && self.creeps.len() == snapshot.creeps.len()
            && self.structures.len() == snapshot.structures.len()
            && self.objects.len() == snapshot.objects.len()
    }

    /// The container filter, built on first use (2 objects and 2 `Reflect.set` calls).
    fn container_filter(&mut self) -> &JsValue {
        self.container_filter
            .get_or_insert_with(|| type_filter("container"))
    }

    /// The road filter, built on first use like the container filter (P6.2).
    fn road_filter(&mut self) -> &JsValue {
        self.road_filter.get_or_insert_with(|| type_filter("road"))
    }

    /// The tower filter, built on first use like the container filter (ADR-030).
    fn tower_filter(&mut self) -> &JsValue {
        self.tower_filter
            .get_or_insert_with(|| type_filter("tower"))
    }
}

/// `{filter: {structureType: kind}}`. A fresh plain object never refuses a property; a failure
/// leaves the filter empty, which `find` reads as no filter, so only the readers' type checks drop
/// other structures.
fn type_filter(kind: &str) -> JsValue {
    let inner = Object::new();
    let opts = Object::new();
    let _ = Reflect::set(&inner, &"structureType".into(), &kind.into());
    let _ = Reflect::set(&opts, &"filter".into(), &inner);
    opts.into()
}

/// Takes this tick's snapshot, with the cache toggles in force.
pub fn take(snapshot: &mut Snapshot, js: &mut JsWorld, cache: &CacheConfig, remote: &RemoteConfig) {
    js.clear();
    snapshot.structures.set_static_cache(cache.structure_static);
    snapshot.begin(TickContext::from_raw(
        game::time(),
        game::cpu::limit(),
        game::cpu::tick_limit(),
        game::cpu::bucket(),
    ));
    for creep in game::creeps().values() {
        push_creep(snapshot, js, creep, true);
    }
    // P6.2: the remote rooms own creeps' names give, the rooms own scouts stand in, and the rooms
    // own remote haulers stand in (identities and positions are read before the rooms).
    js.remote_names.clear();
    js.hauler_rooms.clear();
    js.crew_rooms.clear();
    js.sign_rooms.clear();
    let creeps = &snapshot.creeps;
    for (id, pos) in creeps.identities().iter().zip(creeps.positions()) {
        let Some(id) = id else {
            continue;
        };
        let here = pos.room_name();
        if let Some(r) = id.remote {
            insert_sorted(&mut js.remote_names, r);
            if id.role != Role::Scout {
                insert_sorted(&mut js.crew_rooms, r);
            }
        }
        match id.role {
            Role::Scout => insert_sorted(&mut js.remote_names, here),
            Role::RemoteHauler => insert_sorted(&mut js.hauler_rooms, here),
            Role::Signer => insert_sorted(&mut js.sign_rooms, here),
            _ => {}
        }
    }
    let road_tick = snapshot
        .tick
        .time
        .is_multiple_of(remote.road_read_ticks.max(1));
    for room in game::rooms().values() {
        push_room(snapshot, js, room, road_tick);
    }
    let structures = Object::values(&Game::structures_object());
    for index in 0..structures.length() {
        push_structure(snapshot, js, structures.get(index).unchecked_into());
    }
    snapshot.finish();
}

fn push_room(snapshot: &mut Snapshot, js: &mut JsWorld, room: Room, road_tick: bool) {
    // `parse_room_name` has `RoomName::from_str`'s semantics without its str slicing, which links
    // about 8 KB of Unicode and Debug tables into the release wasm (V114). Anything that parses a
    // room name through the crate brings them back: `Room::name`, `RoomName::new`, `str::parse`,
    // and the `JsCollectionFromValue` or `TryFrom<JsValue>` conversions to `RoomName` (for example
    // `game::rooms().keys()`). Read names with `Named::name_value` and parse them here.
    let Some(name) = room
        .unchecked_ref::<Named>()
        .name_value()
        .as_string()
        .and_then(|s| parse_room_name(s.as_bytes()))
    else {
        snapshot.skipped.record(SkipReason::BadRoomName);
        return;
    };
    let controller_object = room.controller();
    let controller = match controller_object.as_ref() {
        None => Controller::Absent,
        // `my` is undefined or null on an unowned controller; the crate maps that to false (V101).
        Some(c) => Controller::from_parts(c.my(), c.level()),
    };
    // Plain u32 getters (V177); only the bot's rooms have spawn energy to plan with.
    let energy = if matches!(controller, Controller::Mine { .. }) {
        RoomEnergy {
            available: room.energy_available(),
            capacity: room.energy_capacity_available(),
        }
    } else {
        RoomEnergy::default()
    };
    // A creep is in one room and each room is listed once, so no hostile is pushed twice.
    let hostiles = room
        .unchecked_ref::<Findable>()
        .find_array(find::HOSTILE_CREEPS.find_code());
    // P6.3: in a remote room the bot mines, each hostile's active ATTACK and RANGED_ATTACK parts
    // (2 calls, after the push succeeded), so a harmless creep (a scout, a hauler) is no threat.
    let mined = !matches!(controller, Controller::Mine { .. })
        && js
            .crew_rooms
            .binary_search_by_key(&name.packed_repr(), |n| n.packed_repr())
            .is_ok();
    for index in 0..hostiles.length() {
        let before = snapshot.creeps.len();
        push_creep(snapshot, js, hostiles.get(index).unchecked_into(), false);
        if mined
            && snapshot.creeps.len() > before
            && let Some(creep) = js.creeps.last()
        {
            let combat = Combat {
                attack: u32::from(creep.get_active_bodyparts(Part::Attack)),
                ranged_attack: u32::from(creep.get_active_bodyparts(Part::RangedAttack)),
            };
            snapshot.creeps.set_combat(before, combat);
        }
    }
    let row = snapshot.rooms.push(name, controller);
    snapshot.rooms.set_energy(row, energy);
    // Whether this room's controller went into the objects table already (a remote read).
    let mut controller_pushed = false;
    if matches!(controller, Controller::Mine { .. }) {
        if let (Some(c), Controller::Mine { level }) = (controller_object.as_ref(), controller) {
            snapshot
                .rooms
                .set_controller_state(row, controller_state(c, level));
        }
        push_objects(snapshot, js, room.unchecked_ref::<Findable>());
    } else if js
        .remote_names
        .binary_search_by_key(&name.packed_repr(), |n| n.packed_repr())
        .is_ok()
    {
        let roads = road_tick
            && js
                .hauler_rooms
                .binary_search_by_key(&name.packed_repr(), |n| n.packed_repr())
                .is_ok();
        let read = push_remote(snapshot, js, &room, controller_object.as_ref(), roads);
        snapshot.rooms.set_remote(row, read);
        controller_pushed = controller_object.is_some();
    }
    if js
        .sign_rooms
        .binary_search_by_key(&name.packed_repr(), |n| n.packed_repr())
        .is_ok()
    {
        let read = push_sign(
            snapshot,
            js,
            &room,
            controller_object.as_ref(),
            controller_pushed,
        );
        snapshot.rooms.set_sign(row, read);
    }
    js.rooms.push(room);
}

/// A remote room (P6.2, ADR-029): a visible room the bot does not own that an own creep's name
/// gives, or that an own scout stands in. Its room objects as an owned room's (`push_objects`), then its controller as an object
/// (`get`, `id`, the id copy, its position and type when first seen, and `reservation`: undefined,
/// or its `username` and `ticksToEnd`), `FIND_HOSTILE_STRUCTURES` and its `length`, and on a road
/// tick (every `remote.road_read_ticks`) its roads through the engine's filter, each with `get`,
/// `id`, the id copy, `hits` and `hitsMax` (its position and type when first seen).
fn push_remote(
    snapshot: &mut Snapshot,
    js: &mut JsWorld,
    room: &Room,
    controller: Option<&StructureController>,
    roads: bool,
) -> RemoteRead {
    let finder = room.unchecked_ref::<Findable>();
    push_objects(snapshot, js, finder);
    let mut read = RemoteRead {
        read: true,
        ..RemoteRead::default()
    };
    if let Some(c) = controller {
        let value = c.unchecked_ref::<Reserving>().reservation_value();
        let ticks = if value.is_object() {
            let r = value.unchecked_ref::<UserRecord>();
            let ticks = r
                .ticks_to_end_value()
                .as_f64()
                .filter(|t| t.is_finite() && *t >= 0.0)
                .map_or(0, |t| t as u32);
            let user = r.username_value().as_string();
            ensure_username(snapshot, js);
            read.reservation = match (user, js.username.as_deref()) {
                (Some(u), Some(me)) if u == me => Reservation::Mine { ticks },
                _ => Reservation::Other { ticks },
            };
            ticks
        } else {
            0
        };
        let row = ObjectRow {
            amount: ticks,
            total: 0,
            hits: 0,
        };
        push_object(
            snapshot,
            js,
            c.clone().unchecked_into(),
            |_| Ok(ObjectKind::Controller),
            row,
        );
    }
    read.hostile_structures = finder
        .find_array(find::HOSTILE_STRUCTURES.find_code())
        .length();
    if roads {
        read.roads = true;
        let list = finder.find_filtered(find::STRUCTURES.find_code(), js.road_filter());
        for i in 0..list.length() {
            let object: RoomObject = list.get(i).unchecked_into();
            let hits = object.unchecked_ref::<Structure>().hits();
            let total = object
                .unchecked_ref::<WithHitsMax>()
                .hits_max_value()
                .as_f64()
                .filter(|t| t.is_finite() && *t >= 0.0)
                .map_or(0, |t| t as u32);
            let row = ObjectRow {
                amount: 0,
                total,
                hits,
            };
            let check = |o: &RoomObject| {
                if o.unchecked_ref::<Structure>().structure_type() != StructureType::Road {
                    return Err(SkipReason::UnknownStructureType);
                }
                Ok(ObjectKind::Road)
            };
            push_object(snapshot, js, object, check, row);
        }
    }
    read
}

/// A room an own signer stands in (ADR-030), owned or not: its controller as an object (unless the
/// remote read pushed it: `get`, `id`, the id copy, and its position and type when first seen), the
/// controller's `sign` and, for one, its `username` and `text` (V275), then
/// `FIND_HOSTILE_STRUCTURES` through the tower filter and its `length`, and per tower `get`, `pos`
/// and `__packedPos`.
fn push_sign(
    snapshot: &mut Snapshot,
    js: &mut JsWorld,
    room: &Room,
    controller: Option<&StructureController>,
    controller_pushed: bool,
) -> SignRead {
    let mut read = SignRead {
        read: true,
        ..SignRead::default()
    };
    if let Some(c) = controller {
        if !controller_pushed {
            push_object(
                snapshot,
                js,
                c.clone().unchecked_into(),
                |_| Ok(ObjectKind::Controller),
                ObjectRow::default(),
            );
        }
        let value = c.unchecked_ref::<Signed>().sign_value();
        read.sign = if value.is_object() {
            let sign = value.unchecked_ref::<SignRecord>();
            if sign.sign_username().as_string().as_deref() == Some(SYSTEM_USERNAME) {
                SignSeen::System
            } else if sign.sign_text().as_string().as_deref() == Some(SIGN_TEXT) {
                SignSeen::Ours
            } else {
                SignSeen::Other
            }
        } else {
            SignSeen::None
        };
    }
    let finder = room.unchecked_ref::<Findable>();
    let list = finder.find_filtered(find::HOSTILE_STRUCTURES.find_code(), js.tower_filter());
    for i in 0..list.length() {
        // The filter leaves only towers (a filter that failed to build would leave every hostile
        // structure, which only counts more towers than stand).
        let object: RoomObject = list.get(i).unchecked_into();
        if let Ok(at) = position_of(&object) {
            read.add_tower(at);
        }
    }
    read
}

/// Reads the bot's username once per global from the first own creep's `owner.username` (3 calls)
/// into `js.username` (P6.2).
fn ensure_username(snapshot: &Snapshot, js: &mut JsWorld) {
    if js.username.is_none() {
        let first = snapshot
            .creeps
            .my()
            .iter()
            .position(|&m| m)
            .and_then(|row| js.creeps.get(row));
        if let Some(creep) = first {
            let owner = creep.unchecked_ref::<HasOwner>().owner_value();
            if owner.is_object() {
                js.username = owner
                    .unchecked_ref::<UserRecord>()
                    .username_value()
                    .as_string();
            }
        }
    }
}

/// Inserts `room` into the sorted list once.
fn insert_sorted(list: &mut Vec<RoomName>, room: RoomName) {
    if let Err(at) = list.binary_search_by_key(&room.packed_repr(), |n| n.packed_repr()) {
        list.insert(at, room);
    }
}

/// An owned controller's progress and timers (P2.4): 3 calls, each `Option` from the crate's
/// getters (an undefined `upgradeBlocked` is `None`, V189). `ticksToDowngrade` is undefined until
/// the processor first sets the timer (a controller just claimed, V189): that reads as the full
/// timer the processor sets, `CONTROLLER_DOWNGRADE[level] + 1`, so the downgrade guard does not
/// fire on it.
fn controller_state(c: &StructureController, level: u8) -> ControllerState {
    ControllerState {
        progress: c.progress().unwrap_or(0),
        downgrade_ticks: c.ticks_to_downgrade().unwrap_or_else(|| {
            screeps::constants::controller_downgrade(level)
                .map_or(u32::MAX, |t| t.saturating_add(1))
        }),
        upgrade_blocked: c.upgrade_blocked().is_some_and(|t| t > 0),
    }
}

/// The room objects of an owned room (P2.4): 6 `find` calls with their `length`s (sources, own
/// sites, dropped resources, tombstones, ruins, and containers through the engine's filter, V199),
/// then per object `get`, `id` and the id copy, its position and type on the tick it is first seen
/// (the static data), and its amount: a source's `energy` and `energyCapacity`, a site's
/// `progress` and `progressTotal`, a pile's `amount`, a tombstone's, ruin's or container's `store`
/// and `getUsedCapacity("energy")` (V184, V190, V196, V199), and a container's `hits` (P3.5, V223).
fn push_objects(snapshot: &mut Snapshot, js: &mut JsWorld, finder: &Findable) {
    let list = finder.find_array(find::SOURCES.find_code());
    for i in 0..list.length() {
        let source: Source = list.get(i).unchecked_into();
        let row = ObjectRow {
            amount: source.energy(),
            total: source.energy_capacity(),
            hits: 0,
        };
        push_object(snapshot, js, source.into(), |_| Ok(ObjectKind::Source), row);
    }
    let list = finder.find_array(find::MY_CONSTRUCTION_SITES.find_code());
    for i in 0..list.length() {
        let site: ConstructionSite = list.get(i).unchecked_into();
        let row = ObjectRow {
            amount: site.progress(),
            total: site.progress_total(),
            hits: 0,
        };
        let kind = |o: &RoomObject| {
            known_structure_type(o.unchecked_ref::<ConstructionSite>().structure_type())
                .map(ObjectKind::Site)
                .ok_or(SkipReason::UnknownStructureType)
        };
        push_object(snapshot, js, site.into(), kind, row);
    }
    let list = finder.find_array(find::DROPPED_RESOURCES.find_code());
    for i in 0..list.length() {
        let pile: Resource = list.get(i).unchecked_into();
        let row = ObjectRow {
            amount: pile.amount(),
            total: 0,
            hits: 0,
        };
        let kind = |o: &RoomObject| {
            Ok(
                if o.unchecked_ref::<Resource>().resource_type() == ResourceType::Energy {
                    ObjectKind::Energy
                } else {
                    ObjectKind::OtherResource
                },
            )
        };
        push_object(snapshot, js, pile.into(), kind, row);
    }
    let stored = [
        (
            finder.find_array(find::TOMBSTONES.find_code()),
            ObjectKind::Tombstone,
        ),
        (finder.find_array(find::RUINS.find_code()), ObjectKind::Ruin),
        (
            finder.find_filtered(find::STRUCTURES.find_code(), js.container_filter()),
            ObjectKind::Container,
        ),
    ];
    for (list, kind) in stored {
        for i in 0..list.length() {
            let object: RoomObject = list.get(i).unchecked_into();
            // A container's `hits` (P3.5): the crate's getter reads an undefined value as 0 (V223).
            let hits = if kind == ObjectKind::Container {
                object.unchecked_ref::<Structure>().hits()
            } else {
                0
            };
            let row = ObjectRow {
                amount: energy_in(object.unchecked_ref()),
                total: 0,
                hits,
            };
            let check = |o: &RoomObject| {
                // The filter came back empty-handed if building it failed: check the type.
                if kind == ObjectKind::Container
                    && o.unchecked_ref::<Structure>().structure_type() != StructureType::Container
                {
                    return Err(SkipReason::UnknownStructureType);
                }
                Ok(kind)
            };
            push_object(snapshot, js, object, check, row);
        }
    }
}

/// Energy in a store (`getUsedCapacity("energy")`: a number, 0 when none; V196).
fn energy_in(object: &Stored) -> u32 {
    object
        .store_of()
        .get_used_capacity(Some(ResourceType::Energy))
}

fn push_object(
    snapshot: &mut Snapshot,
    js: &mut JsWorld,
    object: RoomObject,
    kind: impl FnOnce(&RoomObject) -> Result<ObjectKind, SkipReason>,
    row: ObjectRow,
) {
    let pushed = key_of(Some(object.unchecked_ref::<WithId>().id_value())).and_then(|key| {
        snapshot.objects.push(
            key,
            || {
                Ok(ObjectStatic {
                    kind: kind(&object)?,
                    pos: position_of(&object)?,
                })
            },
            row,
        )
    });
    match pushed {
        Ok(_) => js.objects.push(object),
        Err(reason) => snapshot.skipped.record(reason),
    }
}

fn push_creep(snapshot: &mut Snapshot, js: &mut JsWorld, creep: Creep, my: bool) {
    let pushed = key_of(creep.try_js_raw_id()).and_then(|key| {
        let pos = position_of(&creep)?;
        // FIND_HOSTILE_CREEPS leaves out spawning creeps (V101), so only own creeps need the read.
        let spawning = my && creep.spawning();
        // Movement needs it only for own creeps that can move (V160); others read as 0.
        let fatigue = if my && !spawning { creep.fatigue() } else { 0 };
        let row = CreepRow {
            pos,
            my,
            spawning,
            fatigue,
        };
        if my {
            // The name is read only when the creep's handle is new (P2.3).
            let handle = snapshot
                .creeps
                .push_named(key, row, || identity_of(&creep))?;
            if !spawning {
                // P2.4: `store`, `getUsedCapacity` and `getCapacity` for energy; a creep with no
                // active CARRY part has a capacity of null, read as 0 (V196, V197).
                let store = creep.store();
                let energy = ResourceType::Energy;
                let row = snapshot.creeps.len().saturating_sub(1);
                snapshot.creeps.set_store(
                    row,
                    CreepStore {
                        energy: store.get_used_capacity(Some(energy)),
                        capacity: store.get_capacity(Some(energy)),
                    },
                );
                // P5.2: `ticksToLive` until it reads a number (undefined before `ageTime` is set,
                // V198); its death tick is then kept with the handle, since `ageTime` never
                // changes and the bot renews no creep (V170).
                if snapshot
                    .creeps
                    .deaths()
                    .get(row)
                    .copied()
                    .flatten()
                    .is_none()
                    && let Some(ttl) = creep.ticks_to_live()
                {
                    let death = snapshot.tick.time.saturating_add(ttl);
                    snapshot.creeps.set_death(row, death);
                }
            }
            Ok(handle)
        } else {
            snapshot.creeps.push(key, row)
        }
    });
    match pushed {
        Ok(_) => js.creeps.push(creep),
        Err(reason) => snapshot.skipped.record(reason),
    }
}

/// What an own creep's name says, read as a plain value (`Creep::name` panics on a non-string,
/// V177) and parsed in core; `None` for a name the bot did not give.
fn identity_of(creep: &Creep) -> Option<Identity> {
    let name = creep.unchecked_ref::<Named>().name_value().as_string()?;
    CreepName::parse(name.as_bytes())
}

fn push_structure(snapshot: &mut Snapshot, js: &mut JsWorld, structure: Structure) {
    let pushed = key_of(Some(structure.js_raw_id())).and_then(|key| {
        snapshot.structures.push(key, || {
            let kind = known_structure_type(structure.structure_type())
                .ok_or(SkipReason::UnknownStructureType)?;
            Ok(StructureStatic {
                kind,
                pos: position_of(&structure)?,
            })
        })
    });
    match pushed {
        Ok(handle) => {
            let row = snapshot.structures.len().saturating_sub(1);
            let kind = snapshot.structures.kinds().last().copied();
            if matches!(kind, Some(StructureType::Spawn | StructureType::Extension)) {
                read_free_energy(snapshot, &structure, row);
            }
            let structures = &snapshot.structures;
            if kind == Some(StructureType::Spawn) {
                // Null when idle (V175); the crate's getter maps null and undefined to None (V177).
                let busy = structure
                    .unchecked_ref::<StructureSpawn>()
                    .spawning()
                    .is_some();
                let row = u32::try_from(structures.len().saturating_sub(1)).unwrap_or(u32::MAX);
                snapshot.spawns.push(handle, row, busy);
            }
            if kind == Some(StructureType::Link) {
                // `store.energy` behind the getter, and the raw `cooldown || 0` (V216): the crate's
                // getters read them as u32 (a missing value is 0), with no type check to trap on.
                // An empty link cannot send (V216), so its cooldown stays 0, unread.
                let energy = structure
                    .unchecked_ref::<Stored>()
                    .store_of()
                    .get_used_capacity(Some(ResourceType::Energy));
                let cooldown = if energy > 0 {
                    structure.unchecked_ref::<StructureLink>().cooldown()
                } else {
                    0
                };
                let row = u32::try_from(structures.len().saturating_sub(1)).unwrap_or(u32::MAX);
                snapshot.links.push(handle, row, energy, cooldown);
            }
            js.structures.push(structure);
        }
        Err(reason) => snapshot.skipped.record(reason),
    }
}

/// A spawn's or extension's room for energy (P2.4), only while its room is not full: `store` and
/// `getFreeCapacity("energy")` (V196). A full room's are all 0, which the snapshot leaves them at.
fn read_free_energy(snapshot: &mut Snapshot, structure: &Structure, row: usize) {
    let Some(&pos) = snapshot.structures.positions().get(row) else {
        return;
    };
    let rooms = &snapshot.rooms;
    let full = rooms
        .row_of(pos.room_name())
        .and_then(|r| rooms.energy().get(r))
        .is_none_or(|e| e.available >= e.capacity);
    if full {
        return;
    }
    let free = structure
        .unchecked_ref::<Stored>()
        .store_of()
        .get_free_capacity(Some(ResourceType::Energy));
    snapshot
        .structures
        .set_free_energy(row, u32::try_from(free).unwrap_or(0));
}

/// The object's id as an [`ObjectKey`]. `JsValue::as_string` returns `None` for a non-string
/// instead of throwing (V100).
fn key_of(id: Option<impl AsRef<JsValue>>) -> Result<ObjectKey, SkipReason> {
    let id = id.ok_or(SkipReason::NoId)?;
    let text = id.as_ref().as_string().ok_or(SkipReason::BadId)?;
    ObjectKey::from_hex(&text).ok_or(SkipReason::BadId)
}

/// The object's position from `pos.__packedPos`, checked before `Position::from_packed` (V100).
pub(crate) fn position_of(object: &RoomObject) -> Result<Position, SkipReason> {
    object
        .js_pos()
        .unchecked_ref::<Packed>()
        .packed_pos()
        .and_then(position_from_packed)
        .ok_or(SkipReason::BadPosition)
}

/// Logs the skipped objects when the rate limit lets a line through (one per `window_ticks` at
/// most; the first skip at once).
#[inline]
pub fn log_skips(log: &mut SkipLog, snapshot: &Snapshot, window_ticks: u32) {
    if let Some(report) = log.note(snapshot.tick.time, &snapshot.skipped, window_ticks) {
        log_skip_report(&report);
    }
}

#[cold]
#[inline(never)]
fn log_skip_report(r: &SkipReport) {
    let mut text = format!(
        "rustedscreep snapshot: skipped {} object(s) on {} tick(s) from tick {} to {}:",
        r.counts.total(),
        r.ticks,
        r.first,
        r.last
    );
    reasons(&mut text, &r.counts);
    log::warn!("{text}");
}

/// Appends `, reason n` for every reason with skips.
fn reasons(text: &mut String, counts: &SkipCounts) {
    let mut first = true;
    for reason in SkipReason::ALL {
        let n = counts.get(reason);
        if n > 0 {
            let _ = write!(
                text,
                "{} {} {n}",
                if first { "" } else { "," },
                reason.as_str()
            );
            first = false;
        }
    }
}

/// Rooms the summary lists by name; the rest are counted.
const SUMMARY_ROOMS: usize = 10;

/// A one-line summary for the `rustedscreep.snapshot()` console command. The per-room counts come
/// from the decoded positions, so they also show that the room bits crossed the boundary intact.
pub fn summary(snapshot: &Snapshot, js: &JsWorld) -> String {
    let t = snapshot.tick;
    let rooms = &snapshot.rooms;
    let creeps = &snapshot.creeps;
    let structures = &snapshot.structures;
    let mine = creeps.my().iter().filter(|&&m| m).count();
    let spawning = creeps.spawning().iter().filter(|&&s| s).count();
    let mut text = format!(
        "rustedscreep snapshot at tick {}: cpu limit {}, tickLimit {}, bucket {} | rooms {} (owned {})",
        t.time,
        t.cpu_limit,
        t.tick_limit,
        t.bucket,
        rooms.len(),
        rooms.owned().count(),
    );
    for (row, (&name, controller)) in rooms.names().iter().zip(rooms.controllers()).enumerate() {
        if row == SUMMARY_ROOMS {
            let _ = write!(
                text,
                ", +{} more",
                rooms.len().saturating_sub(SUMMARY_ROOMS)
            );
            break;
        }
        let in_room = |p: &&Position| p.room_name() == name;
        let _ = write!(text, "{} {name} ", if row == 0 { ":" } else { "," });
        let _ = match controller {
            Controller::Absent => write!(text, "no controller"),
            Controller::Neutral => write!(text, "neutral"),
            Controller::Mine { level } => write!(text, "mine RCL {level}"),
            Controller::Foreign { level } => write!(text, "foreign RCL {level}"),
        };
        let _ = write!(
            text,
            " (creeps {}, structures {})",
            creeps.positions().iter().filter(in_room).count(),
            structures.positions().iter().filter(in_room).count(),
        );
    }
    let _ = write!(
        text,
        " | creeps {} (mine {mine}, hostile {}, spawning {spawning}; new {}, gone {}, live handles {}) | \
         structures {} (new {}, gone {}, live handles {}) | skipped {}",
        creeps.len(),
        creeps.len().saturating_sub(mine),
        creeps.added(),
        creeps.released(),
        creeps.live(),
        structures.len(),
        structures.added(),
        structures.released(),
        structures.live(),
        snapshot.skipped.total(),
    );
    for reason in SkipReason::ALL {
        let n = snapshot.skipped.get(reason);
        if n > 0 {
            let _ = write!(text, ", {} {n}", reason.as_str());
        }
    }
    if !js.aligned_with(snapshot) {
        text.push_str(" | JS objects NOT aligned with rows");
    }
    text
}
