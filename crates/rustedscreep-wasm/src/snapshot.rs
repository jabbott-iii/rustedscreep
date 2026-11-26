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
//!   once after a global reset (P5.2, V198; its death tick is kept with its handle), and in a room
//!   with a tower and an open threat last tick `hits`, with `hitsMax` once in its life (P7.2, for
//!   the towers' heals); a defender (P7.5) or a remote defender (P7.6) skips the store and has its
//!   `body` read once in its life (3N + 2), and a remote defender its `hits` every tick (`hitsMax`
//!   once) for its own heals; the claimer (P8.3, no CARRY part) skips the store too;
//! - room: 8 with a controller (`get`, `name`, the name copy, `controller`, `my`, `level`, `find`,
//!   `length`), 6 without, plus `energyAvailable` and `energyCapacityAvailable` when the bot owns
//!   it (P2.3, V175), and for an owned room its controller's 3 progress and timer getters (3 more
//!   for its safe-mode state on a tick a hostile creep stands there, P7.4, V327) and its
//!   room objects (P2.4, `push_objects`: 6 `find` calls and their `length`s, then per object `get`,
//!   `id`, the id copy, its amount, a container's `hits` (P3.5), and on the tick it is first seen
//!   its position and type);
//!   hostile creep: 5 (`get`, `id`, the id copy, `pos`, `__packedPos`; hostile results never
//!   include spawning creeps, V101), and in a remote room named by an own creep other than a scout
//!   2 more, `getActiveBodyparts` for ATTACK and for RANGED_ATTACK (P6.3, V269); in an owned room,
//!   and in such a remote room on a tick one of its hostiles is armed (P7.6, for the remote
//!   defenders), 1 more, `hits` (P7.1), and while no body is known for its handle `body`, the
//!   array's `length`, and per part the element, `type` and `boost` (3N + 2 for N parts, V289):
//!   once while the creep stays in the snapshot, since a body is forgotten only when the creep is
//!   seen in another player's room at a level with labs, where it may be boosted (`read_body`,
//!   V290, V291);
//! - remote room (P6.2, ADR-029: a visible room the bot does not own that an own creep's name gives
//!   as its remote room; `push_remote`): an owned room's 6 `find` calls and room objects, then the
//!   controller as an object (`id`, the copy, its position and type when first seen, `reservation`,
//!   and for a reservation its `username` and `ticksToEnd`, V268), `FIND_HOSTILE_STRUCTURES` and
//!   its `length` (V267), and on ticks divisible by `remote.road_read_ticks`, in a room an own remote
//!   hauler stands in, the roads through the engine's filter, each with `get`, `id`, the copy,
//!   `hits` and `hitsMax`, and its position and type (the handle is released between reads). The
//!   rooms an own scout stands in are read the same way (its tour, ADR-029). The bot's username:
//!   an own creep's `owner` and `username`, once a global;
//! - claimer room (P8.3, ADR-042: a visible room the bot does not own that an own claimer stands
//!   in and claims, its name's room): its controller as an object (`id`, the copy, its position and type when first seen),
//!   unless the remote read pushed it. The claimer's own room (its name's) gets no remote read, so
//!   remote mining learns nothing of the room being claimed;
//! - signer room (ADR-030, `push_sign`: a visible room an own signer stands in, owned or not): its
//!   controller as an object (unless the remote read pushed it), the controller's `sign` and, for
//!   one, its `username` and `text` (V275), and `FIND_HOSTILE_STRUCTURES` through the tower filter,
//!   each tower's `pos` and `__packedPos`;
//! - atlas room (ADR-039, `push_intel`: a visible room the bot does not own whose record is
//!   missing or stale, at most `scout.max_reads` a tick, once the atlas has loaded): its status,
//!   its controller's position, reservation and (another owner's) safe mode, its sources'
//!   positions, its mineral's position, type and density, and its hostile towers, spawns, lairs
//!   and invader core through four filtered `find` calls, about 25 calls;
//! - structure: 3 (`get`, `id`, the id copy), plus on the tick it is first seen `structureType`,
//!   and `pos` and `__packedPos` when the type is known (again every tick while that read fails).
//!   With `cache.structure_static` off (the naive baseline, P1.10) those three are read every tick.
//!   A spawn adds `spawning` (P2.3, V175); while it is spawning the `Spawning` object comes back
//!   through an `Option` (below), and dropping it runs in wasm (V100). A spawn or an extension in a
//!   room that is not full adds `store` and `getFreeCapacity` for energy (P2.4, V196). A link adds
//!   `store` and `getUsedCapacity` for energy every tick, and `cooldown` while it holds energy (an
//!   empty link cannot send, so its cooldown is never read) (P3.2, V216). A tower adds `store`,
//!   `getUsedCapacity` and `getFreeCapacity` for energy on a tick its room is full, has an
//!   assessed hostile, or had an open threat (P7.2, V196; `tower_wanted`). An own rampart adds
//!   `hits` on an upkeep read tick, on the tick it is first seen, and while a worker repairing it
//!   stands within one step of reach (P7.3, V307). A spawn, tower, storage, or terminal adds `hits`
//!   on a tick a hostile creep stands in its room, while `defense.safe_mode` is on and the room is
//!   not in its safe mode (P7.4, the safe-mode triggers);
//! - upkeep (P7.3, ADR-032): in each owned room, on ticks divisible by `upkeep.read_ticks`, the
//!   roads as a remote room's (`push_roads`); between reads each road a worker repairing it stands
//!   within one step of reach of, by id (`Game.getObjectById`, V308), then `id`, the copy, `hits`,
//!   `hitsMax`, and its position and type.
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
use rustedscreep_core::config::{
    CacheConfig, DefenseConfig, RemoteConfig, ScoutConfig, UpkeepConfig,
};
use rustedscreep_core::defense::safe_mode::is_core;
use rustedscreep_core::expansion::Scouting;
use rustedscreep_core::signing::SIGN_TEXT;
use rustedscreep_core::snapshot::{
    BodyPartRead, BodyRead, Combat, Controller, ControllerState, CreepRow, CreepStore, IntelRead,
    LabState, LabTerminal, MineralContainer, MineralRead, MineralSite, MineralTerminal, ObjectKey,
    ObjectKind, ObjectRow, ObjectStatic, RemoteRead, Reservation, RoomEnergy, RoomStatus,
    SafeModeState, SignRead, SignSeen, SkipCounts, SkipLog, SkipReason, SkipReport, Snapshot,
    StructureHandle, StructureStatic, TickContext, known_structure_type, lab_resource,
    parse_room_name, position_from_packed,
};
use rustedscreep_core::spawning::{CreepName, Identity, Role};
use screeps::{
    ConstructionSite, Creep, FindConstant, HasId, MaybeHasId, Mineral, OwnedStructureProperties,
    Part, Position, RawObjectId, Resource, ResourceType, Room, RoomName, RoomObject,
    SYSTEM_USERNAME, Source, Store, Structure, StructureController, StructureExtractor,
    StructureLab, StructureLink, StructureSpawn, StructureType, find, game,
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

    /// A mineral, for `mineralType` and `density` as plain values (ADR-039, V364): the crate's
    /// getters convert to wasm-bindgen enums, which throw on a value they do not know.
    type MineralInfo;

    #[wasm_bindgen(method, getter = mineralType)]
    fn mineral_type_value(this: &MineralInfo) -> JsValue;

    #[wasm_bindgen(method, getter = density)]
    fn density_value(this: &MineralInfo) -> JsValue;

    /// An invader core, for `level` as a plain value (ADR-039, V365).
    type CoreInfo;

    #[wasm_bindgen(method, getter = level)]
    fn level_value(this: &CoreInfo) -> JsValue;
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
    /// `{filter: {structureType: "constructedWall"}}`, likewise (ADR-033).
    wall_filter: Option<JsValue>,
    /// `{filter: {structureType: ...}}` for spawns, keepers' lairs and invader cores, likewise
    /// (ADR-039, the atlas's reads).
    spawn_filter: Option<JsValue>,
    lair_filter: Option<JsValue>,
    core_filter: Option<JsValue>,
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
    /// Scratch: the rooms an own claimer stands in this tick, sorted: their controller is pushed as
    /// an object for its `claimController` (P8.3).
    claim_rooms: Vec<RoomName>,
    /// Scratch: the rooms an own signer stands in this tick, sorted: their controller's sign and
    /// the towers of other users are read (ADR-030).
    sign_rooms: Vec<RoomName>,
    /// The owned rooms with an open threat at the end of last tick, sorted (P7.2): own creeps'
    /// `hits` and `hitsMax` are read there, for the towers' heals.
    threat_rooms: Vec<RoomName>,
    /// What the opening asked last tick for its repairers (P7.3): the roads and constructed walls
    /// (ADR-033) to read by id, and the own ramparts whose `hits` to read, by handle slot.
    upkeep_roads: Vec<ObjectKey>,
    upkeep_ramparts: Vec<StructureHandle>,
    /// Scratch: the owned rooms with a hostile creep this tick, sorted (P7.1, P7.4).
    hostile_rooms: Vec<RoomName>,
    /// Scratch: the owned rooms whose core structures' `hits` this tick reads, sorted (P7.4): a
    /// hostile creep there, `defense.safe_mode` on, and the room not in its safe mode.
    core_rooms: Vec<RoomName>,
    /// The owned rooms whose mineral the next snapshot reads, sorted (P9.1, ADR-044,
    /// `Minerals::reads`).
    mineral_rooms: Vec<RoomName>,
    /// The owned rooms whose terminal and labs the next snapshot reads, sorted (P9.2, ADR-045,
    /// `Labs::reads`).
    lab_rooms: Vec<RoomName>,
}

impl JsWorld {
    /// Sets the rooms whose own creeps' hits the next snapshot reads (P7.2): the rooms with an open
    /// threat.
    pub fn set_threat_rooms(&mut self, rooms: impl Iterator<Item = RoomName>) {
        self.threat_rooms.clear();
        self.threat_rooms.extend(rooms);
        self.threat_rooms.sort_unstable_by_key(|n| n.packed_repr());
        self.threat_rooms.dedup();
    }

    /// Sets what the next snapshot reads for the opening's repairers (P7.3,
    /// `Opening::upkeep_reads`): the roads by id, and the own ramparts' `hits`.
    pub fn set_upkeep_reads(&mut self, roads: &[ObjectKey], ramparts: &[StructureHandle]) {
        self.upkeep_roads.clear();
        self.upkeep_roads.extend_from_slice(roads);
        self.upkeep_ramparts.clear();
        self.upkeep_ramparts.extend_from_slice(ramparts);
        self.upkeep_ramparts.sort_unstable_by_key(|h| h.slot());
    }

    /// Sets the owned rooms whose mineral, container, extractor and terminal the next snapshot
    /// reads (P9.1, ADR-044, `Minerals::reads`).
    pub fn set_mineral_reads(&mut self, rooms: &[RoomName]) {
        self.mineral_rooms.clear();
        self.mineral_rooms.extend_from_slice(rooms);
        self.mineral_rooms.sort_unstable_by_key(|n| n.packed_repr());
        self.mineral_rooms.dedup();
    }

    /// Sets the owned rooms whose terminal stock and labs the next snapshot reads (P9.2,
    /// ADR-045, `Labs::reads`).
    pub fn set_lab_reads(&mut self, rooms: &[RoomName]) {
        self.lab_rooms.clear();
        self.lab_rooms.extend_from_slice(rooms);
        self.lab_rooms.sort_unstable_by_key(|n| n.packed_repr());
        self.lab_rooms.dedup();
    }

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

    /// The constructed wall filter, built on first use like the container filter (ADR-033).
    fn wall_filter(&mut self) -> &JsValue {
        self.wall_filter
            .get_or_insert_with(|| type_filter("constructedWall"))
    }

    /// The spawn filter, built on first use like the container filter (ADR-039).
    fn spawn_filter(&mut self) -> &JsValue {
        self.spawn_filter
            .get_or_insert_with(|| type_filter("spawn"))
    }

    /// The keeper's lair filter, likewise (ADR-039).
    fn lair_filter(&mut self) -> &JsValue {
        self.lair_filter
            .get_or_insert_with(|| type_filter("keeperLair"))
    }

    /// The invader core filter, likewise (ADR-039).
    fn core_filter(&mut self) -> &JsValue {
        self.core_filter
            .get_or_insert_with(|| type_filter("invaderCore"))
    }
}

/// Which visible rooms this tick's snapshot reads for the atlas (ADR-039): a room the bot does not
/// own that [`Scouting::wants_read`] says is due, at most `scout.max_reads` a tick, and none unless
/// the last tick ran the Optional tier (scouting is Optional work; the snapshot runs before this
/// tick's budget, so last tick's stands in, and a global's first tick reads none).
pub struct IntelPass<'a> {
    pub scouting: &'a Scouting,
    pub cfg: &'a ScoutConfig,
    left: u32,
}

impl<'a> IntelPass<'a> {
    pub fn new(scouting: &'a Scouting, cfg: &'a ScoutConfig, optional: bool) -> Self {
        Self {
            scouting,
            cfg,
            left: if optional { cfg.max_reads } else { 0 },
        }
    }

    /// Whether `room` is read now; a read counts against this tick's reads.
    fn take(&mut self, room: RoomName, now: u32) -> bool {
        let wanted = self.left > 0 && self.scouting.wants_read(room, now, self.cfg);
        if wanted {
            self.left -= 1;
        }
        wanted
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
pub fn take(
    snapshot: &mut Snapshot,
    js: &mut JsWorld,
    cache: &CacheConfig,
    remote: &RemoteConfig,
    upkeep: &UpkeepConfig,
    defense: &DefenseConfig,
    mut intel: IntelPass<'_>,
) {
    js.clear();
    js.hostile_rooms.clear();
    js.core_rooms.clear();
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
    js.claim_rooms.clear();
    js.sign_rooms.clear();
    let creeps = &snapshot.creeps;
    for (id, pos) in creeps.identities().iter().zip(creeps.positions()) {
        let Some(id) = id else {
            continue;
        };
        let here = pos.room_name();
        // The claimer's name carries its room too (P8.3), but that room gets no remote read: remote
        // mining would take it as intel and mine the room being claimed. Only its controller is
        // pushed, while the claimer stands there.
        if let Some(r) = id.remote
            && id.role != Role::Claimer
        {
            insert_sorted(&mut js.remote_names, r);
            if id.role != Role::Scout {
                insert_sorted(&mut js.crew_rooms, r);
            }
        }
        match id.role {
            Role::Scout => insert_sorted(&mut js.remote_names, here),
            // Only in the room it claims: its controller is of no use elsewhere.
            Role::Claimer if id.remote == Some(here) => insert_sorted(&mut js.claim_rooms, here),
            Role::RemoteHauler => insert_sorted(&mut js.hauler_rooms, here),
            Role::Signer => insert_sorted(&mut js.sign_rooms, here),
            _ => {}
        }
    }
    let road_tick = snapshot
        .tick
        .time
        .is_multiple_of(remote.road_read_ticks.max(1));
    // P7.3: every road and own rampart of the owned rooms on an upkeep read tick.
    let upkeep_tick = upkeep.enabled && snapshot.tick.time.is_multiple_of(upkeep.read_ticks.max(1));
    for room in game::rooms().values() {
        push_room(
            snapshot,
            js,
            room,
            road_tick,
            upkeep_tick,
            defense.safe_mode,
            &mut intel,
        );
    }
    if upkeep.enabled && !upkeep_tick {
        push_asked_roads(snapshot, js);
    }
    let structures = Object::values(&Game::structures_object());
    for index in 0..structures.length() {
        push_structure(
            snapshot,
            js,
            structures.get(index).unchecked_into(),
            upkeep.enabled,
            upkeep_tick,
        );
    }
    snapshot.finish();
}

/// The roads the opening asked for (P7.3, [`JsWorld::set_upkeep_reads`]), each by id
/// (`Game.getObjectById`, V308) unless already read this tick, as `push_roads` reads them. A road
/// that is gone is not pushed, and the opening drops it.
fn push_asked_roads(snapshot: &mut Snapshot, js: &mut JsWorld) {
    let keys = std::mem::take(&mut js.upkeep_roads);
    for &key in &keys {
        let read = snapshot
            .objects
            .handle_of(key)
            .and_then(|h| snapshot.objects.row_of(h))
            .is_some();
        if read {
            continue;
        }
        if let Some(object) = game::get_object_by_id_erased(&RawObjectId::from_packed(key.0)) {
            push_road(snapshot, js, object);
        }
    }
    js.upkeep_roads = keys;
}

fn push_room(
    snapshot: &mut Snapshot,
    js: &mut JsWorld,
    room: Room,
    road_tick: bool,
    upkeep_tick: bool,
    safe_mode: bool,
    intel: &mut IntelPass<'_>,
) {
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
    let owned = matches!(controller, Controller::Mine { .. });
    let boosts = controller.boosts_possible();
    let mined = !owned
        && js
            .crew_rooms
            .binary_search_by_key(&name.packed_repr(), |n| n.packed_repr())
            .is_ok();
    let hostile_count = hostiles.length();
    // P7.6: this room's first hostile row, and whether one is armed (a mined room only).
    let first_row = snapshot.creeps.len();
    let mut any_armed = false;
    for index in 0..hostile_count {
        let before = snapshot.creeps.len();
        push_creep(snapshot, js, hostiles.get(index).unchecked_into(), false);
        if snapshot.creeps.len() <= before {
            continue;
        }
        let Some(creep) = js.creeps.last() else {
            continue;
        };
        if owned {
            // P7.1: its hits every tick, and its body while none is known for its handle. An
            // empty read, which the engine never gives, is not kept, so the next tick reads again
            // and the creep counts as unread meanwhile.
            snapshot.creeps.set_assessed(before, creep.hits());
            if !snapshot.creeps.knows_body(before) {
                let body = read_body(creep);
                if !body.is_empty() {
                    snapshot.creeps.set_body(before, body);
                }
            }
        } else if boosts {
            // A hostile in another player's room with labs may be boosted there (P7.1).
            snapshot.creeps.forget_body(before);
        }
        if mined {
            let combat = Combat {
                attack: u32::from(creep.get_active_bodyparts(Part::Attack)),
                ranged_attack: u32::from(creep.get_active_bodyparts(Part::RangedAttack)),
            };
            snapshot.creeps.set_combat(before, combat);
            any_armed |= combat.armed();
        }
    }
    // P7.6: in a mined room that holds an armed hostile, every hostile's hits, and its body while
    // none is known (a healer's heal counts against the remote defenders), as in an owned room.
    if mined && any_armed {
        for row in first_row..snapshot.creeps.len() {
            let Some(creep) = js.creeps.get(row) else {
                continue;
            };
            snapshot.creeps.set_assessed(row, creep.hits());
            if !snapshot.creeps.knows_body(row) {
                let body = read_body(creep);
                if !body.is_empty() {
                    snapshot.creeps.set_body(row, body);
                }
            }
        }
    }
    let row = snapshot.rooms.push(name, controller);
    snapshot.rooms.set_energy(row, energy);
    // Whether this room's controller went into the objects table already (a remote read).
    let mut controller_pushed = false;
    if matches!(controller, Controller::Mine { .. }) {
        if let (Some(c), Controller::Mine { level }) = (controller_object.as_ref(), controller) {
            // P7.4: the safe-mode getters only on a tick a hostile creep stands in the room, and
            // the core structures' hits then too while safe mode may be activated there.
            let hostile_here = hostile_count > 0;
            let state = controller_state(c, level, hostile_here);
            if hostile_here {
                insert_sorted(&mut js.hostile_rooms, name);
                if safe_mode && state.safe_mode.is_some_and(|s| s.ticks == 0) {
                    insert_sorted(&mut js.core_rooms, name);
                }
            }
            snapshot.rooms.set_controller_state(row, state);
        }
        let objects_from = snapshot.objects.len();
        push_objects(snapshot, js, room.unchecked_ref::<Findable>());
        // P9.1: the mineral of a room the mineral plan asked for, after its containers.
        if listed(&js.mineral_rooms, name) {
            push_mineral(
                snapshot,
                js,
                room.unchecked_ref::<Findable>(),
                name,
                objects_from,
            );
        }
        // P9.2: a row for a room the lab plan asked for; its terminal and labs come with the
        // room's structures.
        if listed(&js.lab_rooms, name) {
            snapshot.labs.push(name);
        }
        if upkeep_tick {
            push_roads(snapshot, js, room.unchecked_ref::<Findable>());
            push_walls(snapshot, js, room.unchecked_ref::<Findable>());
            snapshot.rooms.set_upkeep_read(row);
        }
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
    // P8.3: a room an own claimer stands in, not owned: its controller as an object.
    if !controller_pushed
        && !matches!(controller, Controller::Mine { .. })
        && js
            .claim_rooms
            .binary_search_by_key(&name.packed_repr(), |n| n.packed_repr())
            .is_ok()
        && let Some(c) = controller_object.as_ref()
    {
        push_object(
            snapshot,
            js,
            c.clone().unchecked_into(),
            |_| Ok(ObjectKind::Controller),
            ObjectRow::default(),
        );
        controller_pushed = true;
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
    // ADR-039: a due room the bot does not own, for the atlas.
    if !owned && intel.take(name, snapshot.tick.time) {
        let read = push_intel(
            snapshot,
            js,
            &room,
            name,
            controller_object.as_ref(),
            controller,
            hostile_count,
        );
        snapshot.rooms.set_intel(row, read);
    }
    js.rooms.push(room);
}

/// A due room for the atlas (ADR-039): its status (`Game.map.getRoomStatus`, V168, and the
/// `status` read: 4), the controller's `pos` and `__packedPos` and its `reservation` (and for one
/// its `username` and `ticksToEnd`), another owner's `safeMode` (V337), `FIND_SOURCES` and its
/// `length` and per source `get`, `pos` and `__packedPos`, `FIND_MINERALS` and its `length` and
/// for the first `get`, `pos`, `__packedPos`, `mineralType` and `density` (V364),
/// `FIND_HOSTILE_STRUCTURES` through the tower, spawn, lair and core filters with each `length`
/// (V267), and an invader core's `get` and `level` (V365): about 25 calls. The hostile creeps are
/// the room's count, already read.
fn push_intel(
    snapshot: &Snapshot,
    js: &mut JsWorld,
    room: &Room,
    name: RoomName,
    controller: Option<&StructureController>,
    kind: Controller,
    hostiles: u32,
) -> IntelRead {
    let mut read = IntelRead {
        read: true,
        hostiles: u8::try_from(hostiles).unwrap_or(u8::MAX),
        ..IntelRead::default()
    };
    // A status this build does not know, or one it could not read, is the safe reading: closed.
    read.status = match crate::route::read_status(name) {
        Some(crate::route::Status::Normal) => RoomStatus::Normal,
        Some(crate::route::Status::Novice) => RoomStatus::Novice,
        Some(crate::route::Status::Respawn) => RoomStatus::Respawn,
        Some(crate::route::Status::Closed | crate::route::Status::Other) | None => {
            RoomStatus::Closed
        }
    };
    if let Some(c) = controller {
        read.controller_at = position_of(c.unchecked_ref::<RoomObject>()).ok();
        read.reservation = reservation_of(snapshot, js, c).1;
        if matches!(kind, Controller::Foreign { .. }) {
            read.safe_mode = c.safe_mode().unwrap_or(0);
        }
    }
    let finder = room.unchecked_ref::<Findable>();
    let sources = finder.find_array(find::SOURCES.find_code());
    for i in 0..sources.length() {
        let object: RoomObject = sources.get(i).unchecked_into();
        if let Ok(at) = position_of(&object) {
            read.add_source(at);
        }
    }
    let minerals = finder.find_array(find::MINERALS.find_code());
    if minerals.length() > 0 {
        let object: RoomObject = minerals.get(0).unchecked_into();
        if let Ok(at) = position_of(&object) {
            let info = object.unchecked_ref::<MineralInfo>();
            let density = info
                .density_value()
                .as_f64()
                .filter(|d| d.is_finite() && *d >= 0.0 && *d <= f64::from(u8::MAX))
                .map_or(0, |d| d as u8);
            read.mineral = Some(MineralRead {
                kind: info
                    .mineral_type_value()
                    .as_string()
                    .and_then(|t| mineral_kind(&t)),
                at,
                density,
            });
        }
    }
    let hostile = find::HOSTILE_STRUCTURES.find_code();
    let count = |list: Array| u8::try_from(list.length()).unwrap_or(u8::MAX);
    read.towers = count(finder.find_filtered(hostile, js.tower_filter()));
    read.spawns = count(finder.find_filtered(hostile, js.spawn_filter()));
    read.lairs = count(finder.find_filtered(hostile, js.lair_filter()));
    let cores = finder.find_filtered(hostile, js.core_filter());
    if cores.length() > 0 {
        let core: JsValue = cores.get(0);
        read.core = Some(
            core.unchecked_ref::<CoreInfo>()
                .level_value()
                .as_f64()
                .filter(|l| l.is_finite() && *l >= 0.0 && *l < f64::from(u8::MAX))
                .map_or(0, |l| l as u8),
        );
    }
    read
}

/// An owned room's mineral read (P9.1, ADR-044): `FIND_MINERALS` with its `length`, and for the
/// first its `get`, `id`, `pos` and `__packedPos` (on a new handle), `mineralAmount`,
/// `ticksToRegeneration` and `mineralType` (V394, V391); then for the first of the room's
/// containers (pushed just before, from row `objects_from`) next to it, its `store` and
/// `getUsedCapacity` for the mineral and for everything (V397): about 12 calls. The extractor and
/// the terminal come with the room's structures ([`push_structure`]).
fn push_mineral(
    snapshot: &mut Snapshot,
    js: &mut JsWorld,
    finder: &Findable,
    room: RoomName,
    objects_from: usize,
) {
    let list = finder.find_array(find::MINERALS.find_code());
    if list.length() == 0 {
        return;
    }
    let object: RoomObject = list.get(0).unchecked_into();
    let mineral = object.unchecked_ref::<Mineral>();
    let amount = mineral.mineral_amount();
    let regen = mineral.ticks_to_regeneration();
    let kind = object
        .unchecked_ref::<MineralInfo>()
        .mineral_type_value()
        .as_string()
        .and_then(|t| mineral_kind(&t));
    let row = ObjectRow {
        amount,
        total: regen.unwrap_or(0),
        hits: 0,
    };
    let before = snapshot.objects.len();
    push_object(snapshot, js, object, |_| Ok(ObjectKind::Mineral), row);
    let objects = &snapshot.objects;
    if objects.len() <= before {
        return;
    }
    let (Some(&handle), Some(&at)) = (objects.handles().last(), objects.positions().last()) else {
        return;
    };
    let near = |p: &Position| {
        p.room_name() == at.room_name()
            && p.x().u8().abs_diff(at.x().u8()) <= 1
            && p.y().u8().abs_diff(at.y().u8()) <= 1
    };
    let container = (objects_from..before)
        .find(|&r| {
            objects.kinds().get(r) == Some(&ObjectKind::Container)
                && objects.positions().get(r).is_some_and(near)
        })
        .and_then(|r| {
            let store = js.objects.get(r)?.unchecked_ref::<Stored>().store_of();
            Some(MineralContainer {
                handle: *objects.handles().get(r)?,
                at: *objects.positions().get(r)?,
                stock: kind.map_or(0, |k| store.get_used_capacity(Some(k))),
                used: store.get_used_capacity(None),
            })
        });
    snapshot.minerals.push(MineralSite {
        room,
        handle,
        at,
        kind,
        amount,
        regen,
        extractor: None,
        container,
        terminal: None,
    });
}

/// A terminal's stock of every lab resource (P9.2, ADR-045): `Object.keys(store)`, then for each
/// key a lab handles (`LAB_RESOURCES`, read as a plain value, so a key this build does not know
/// is skipped) its amount (`store[key]`): 1 call, and 2 a key past energy. Energy is not read.
fn read_terminal_stock(snapshot: &mut Snapshot, store: &Store, room: RoomName) {
    for key in Object::keys(store.unchecked_ref::<Object>()).iter() {
        let Some(kind) = ResourceType::from_js_value(&key) else {
            continue;
        };
        if kind == ResourceType::Energy || lab_resource(kind).is_none() {
            continue;
        }
        let amount = store.get(kind).unwrap_or(0);
        snapshot.labs.set_stock(room, kind, amount);
    }
}

/// An own lab in a room read for its labs (P9.2, ADR-045): `mineralType` as a plain value (V413;
/// undefined while it holds no mineral), and with one its `store` and the amount (`store[type]`,
/// V412), and `cooldown` (V406, V408): 2 calls for an empty lab, 4 for one holding a mineral.
fn read_lab(
    snapshot: &mut Snapshot,
    structure: &Structure,
    handle: StructureHandle,
    row: usize,
    at: Position,
) {
    let kind = ResourceType::from_js_value(
        &structure
            .unchecked_ref::<MineralInfo>()
            .mineral_type_value(),
    )
    .filter(|&k| lab_resource(k).is_some());
    let amount = kind.map_or(0, |k| {
        structure
            .unchecked_ref::<Stored>()
            .store_of()
            .get(k)
            .unwrap_or(0)
    });
    let cooldown = structure.unchecked_ref::<StructureLab>().cooldown();
    let Ok(row) = u32::try_from(row) else {
        return;
    };
    snapshot.labs.push_lab(LabState {
        handle,
        row,
        at,
        kind,
        amount,
        cooldown,
    });
}

/// The first mineral in a lab hauler's store (P9.2): `Object.keys(store)` and the first key past
/// energy that a lab handles and that holds an amount (`store[key]`; the processors write a
/// resource spent to 0 rather than delete it, V419), read as a plain value; `None` when there is
/// none.
fn first_mineral(store: &Store) -> Option<ResourceType> {
    Object::keys(store.unchecked_ref::<Object>())
        .iter()
        .filter_map(|k| ResourceType::from_js_value(&k))
        .filter(|&k| k != ResourceType::Energy && lab_resource(k).is_some())
        .find(|&k| store.get(k).unwrap_or(0) > 0)
}

/// A mineral's type from its `mineralType` text (V364): the seven a room can hold, `None` for any
/// other.
fn mineral_kind(text: &str) -> Option<ResourceType> {
    Some(match text {
        "H" => ResourceType::Hydrogen,
        "O" => ResourceType::Oxygen,
        "U" => ResourceType::Utrium,
        "L" => ResourceType::Lemergium,
        "K" => ResourceType::Keanium,
        "Z" => ResourceType::Zynthium,
        "X" => ResourceType::Catalyst,
        _ => return None,
    })
}

/// A controller's reservation (P6.2, V263): `reservation`, and for one its `username` and
/// `ticksToEnd` (the bot's username read once a global). Returns the ticks to its end (0 for none)
/// and who holds it.
fn reservation_of(
    snapshot: &Snapshot,
    js: &mut JsWorld,
    c: &StructureController,
) -> (u32, Reservation) {
    let value = c.unchecked_ref::<Reserving>().reservation_value();
    if !value.is_object() {
        return (0, Reservation::None);
    }
    let r = value.unchecked_ref::<UserRecord>();
    let ticks = r
        .ticks_to_end_value()
        .as_f64()
        .filter(|t| t.is_finite() && *t >= 0.0)
        .map_or(0, |t| t as u32);
    let user = r.username_value().as_string();
    ensure_username(snapshot, js);
    let holder = match (user, js.username.as_deref()) {
        (Some(u), Some(me)) if u == me => Reservation::Mine { ticks },
        _ => Reservation::Other { ticks },
    };
    (ticks, holder)
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
        let (ticks, holder) = reservation_of(snapshot, js, c);
        read.reservation = holder;
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
        push_roads(snapshot, js, finder);
    }
    read
}

/// A room's roads through the engine's filter (P6.2, P7.3): per road `get`, then `push_road`.
fn push_roads(snapshot: &mut Snapshot, js: &mut JsWorld, finder: &Findable) {
    let list = finder.find_filtered(find::STRUCTURES.find_code(), js.road_filter());
    for i in 0..list.length() {
        push_road(snapshot, js, list.get(i).unchecked_into());
    }
}

/// An owned room's constructed walls through the engine's filter (ADR-033), as `push_roads` reads
/// roads: per wall `get`, then `push_road`.
fn push_walls(snapshot: &mut Snapshot, js: &mut JsWorld, finder: &Findable) {
    let list = finder.find_filtered(find::STRUCTURES.find_code(), js.wall_filter());
    for i in 0..list.length() {
        push_road(snapshot, js, list.get(i).unchecked_into());
    }
}

/// One road (P6.2, P7.3) or constructed wall (ADR-033): `hits`, `hitsMax`, `id` and the copy, and
/// its type and position when first seen this pass (the handle is released between reads); an
/// object of another type is skipped.
fn push_road(snapshot: &mut Snapshot, js: &mut JsWorld, object: RoomObject) {
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
    let check = |o: &RoomObject| match o.unchecked_ref::<Structure>().structure_type() {
        StructureType::Road => Ok(ObjectKind::Road),
        StructureType::Wall => Ok(ObjectKind::Wall),
        _ => Err(SkipReason::UnknownStructureType),
    };
    push_object(snapshot, js, object, check, row);
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
/// fire on it. With `safe_mode` (a hostile creep in the room, P7.4), 3 more: `safeMode`,
/// `safeModeAvailable`, and `safeModeCooldown` (V327, V336), an undefined one read as 0.
fn controller_state(c: &StructureController, level: u8, safe_mode: bool) -> ControllerState {
    ControllerState {
        safe_mode: safe_mode.then(|| SafeModeState {
            ticks: c.safe_mode().unwrap_or(0),
            available: c.safe_mode_available(),
            cooldown: c.safe_mode_cooldown().unwrap_or(0),
        }),
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

/// A hostile creep's body (P7.1, V289): each part's `type` and `boost`, the first part first. A
/// string the crate does not know reads as the enum's hidden invalid variant, which core counts as
/// no part type and no boost; it never goes back to JS. Parts past `MAX_CREEP_SIZE`, which the
/// engine never makes, are left out.
fn read_body(creep: &Creep) -> BodyRead {
    let mut body = BodyRead::EMPTY;
    for part in creep.body() {
        let read = BodyPartRead {
            part: part.part(),
            boost: part.boost(),
        };
        if !body.push(read) {
            break;
        }
    }
    body
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
                let row = snapshot.creeps.len().saturating_sub(1);
                // P7.5 and P7.6: a defender or a remote defender, and P8.3's claimer (no CARRY part,
                // nothing reads its store).
                let role = snapshot
                    .creeps
                    .identities()
                    .get(row)
                    .copied()
                    .flatten()
                    .map(|id| id.role);
                let remote_defender = role == Some(Role::RemoteDefender);
                let defender = role == Some(Role::Defender) || remote_defender;
                // P9.1: nor the mineral miner's (no CARRY part either).
                let no_store =
                    defender || role == Some(Role::Claimer) || role == Some(Role::MineralMiner);
                // P2.4: `store`, `getUsedCapacity` and `getCapacity` for energy; a creep with no
                // active CARRY part has a capacity of null, read as 0 (V196, V197).
                if !no_store {
                    let store = creep.store();
                    let energy = ResourceType::Energy;
                    let held = store.get_used_capacity(Some(energy));
                    // P9.1: a mineral hauler's whole load too (`getUsedCapacity()`, V397); it
                    // carries nothing but its room's mineral. P9.2: a lab hauler's too, and the
                    // type when it holds some (the first key of its store other than energy; it
                    // takes one type at a time).
                    let hauls = matches!(role, Some(Role::MineralHauler | Role::LabHauler));
                    let other = if hauls {
                        store.get_used_capacity(None).saturating_sub(held)
                    } else {
                        0
                    };
                    // Only in a room read for its labs this tick: the type is used only there.
                    let lab_home = snapshot
                        .creeps
                        .identities()
                        .get(row)
                        .copied()
                        .flatten()
                        .is_some_and(|id| listed(&js.lab_rooms, id.home));
                    let other_kind = if role == Some(Role::LabHauler) && other > 0 && lab_home {
                        first_mineral(&store)
                    } else {
                        None
                    };
                    snapshot.creeps.set_store(
                        row,
                        CreepStore {
                            energy: held,
                            capacity: store.get_capacity(Some(energy)),
                            other,
                            other_kind,
                        },
                    );
                }
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
                // P7.2: `hits` and `hitsMax` in a room with an open threat, for the towers' heals;
                // P7.6: a remote defender's every tick, for its own heals.
                if remote_defender
                    || js
                        .threat_rooms
                        .binary_search_by_key(&pos.room_name().packed_repr(), |n| n.packed_repr())
                        .is_ok()
                {
                    // `hitsMax` once in the creep's life: it never changes.
                    let max = snapshot
                        .creeps
                        .known_hits_max(row)
                        .unwrap_or_else(|| creep.hits_max());
                    snapshot.creeps.set_own_hits(row, creep.hits(), max);
                }
                // P7.5: a defender's body once in its life (its RANGED_ATTACK parts are its
                // power; the engine never changes a body but by boosts, which the bot gives none),
                // and a remote defender's (its RANGED_ATTACK and HEAL parts, P7.6).
                if defender && !snapshot.creeps.knows_body(row) {
                    let body = read_body(&creep);
                    if !body.is_empty() {
                        snapshot.creeps.set_body(row, body);
                    }
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

fn push_structure(
    snapshot: &mut Snapshot,
    js: &mut JsWorld,
    structure: Structure,
    upkeep: bool,
    upkeep_tick: bool,
) {
    let key = key_of(Some(structure.js_raw_id()));
    // P7.3: new this pass when the last pass had no live handle for its id (with the static-data
    // cache off, the static read below runs on every push, so it cannot tell).
    let first_seen = key
        .as_ref()
        .is_ok_and(|&k| snapshot.structures.handle_of(k).is_none());
    let pushed = key.and_then(|key| {
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
            // P7.3: an own rampart's `hits` (V307) on an upkeep read, when first seen, and while
            // its repairer stands within one step of reach.
            if upkeep
                && kind == Some(StructureType::Rampart)
                && (upkeep_tick
                    || first_seen
                    || js
                        .upkeep_ramparts
                        .binary_search_by_key(&handle.slot(), |h| h.slot())
                        .is_ok_and(|i| js.upkeep_ramparts.get(i) == Some(&handle)))
            {
                snapshot.structures.set_hits(row, structure.hits());
            }
            // P7.4: a core structure's `hits` on a tick a hostile creep stands in its room (the
            // safe-mode triggers; the rooms are read before the structures).
            if kind.is_some_and(is_core)
                && snapshot
                    .structures
                    .positions()
                    .get(row)
                    .is_some_and(|p| listed(&js.core_rooms, p.room_name()))
            {
                snapshot.structures.set_hits(row, structure.hits());
            }
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
            if kind == Some(StructureType::Tower) {
                // P7.2: `store`, then its energy and its room for more (V196), only where a fill or
                // a fight can use them; elsewhere both stay 0.
                let row = structures.len().saturating_sub(1);
                let wanted = snapshot
                    .structures
                    .positions()
                    .get(row)
                    .is_some_and(|p| tower_wanted(snapshot, js, p.room_name()));
                let mut energy = 0;
                if wanted {
                    let store = structure.unchecked_ref::<Stored>().store_of();
                    energy = store.get_used_capacity(Some(ResourceType::Energy));
                    let free = store.get_free_capacity(Some(ResourceType::Energy));
                    snapshot
                        .structures
                        .set_free_energy(row, u32::try_from(free).unwrap_or(0));
                }
                let row = u32::try_from(row).unwrap_or(u32::MAX);
                snapshot.towers.push(handle, row, energy);
            }
            // P9.1: in a room read for its mineral, the extractor on it (its `cooldown`, V396)
            // and the terminal (its `store`, the mineral's `getUsedCapacity` and the free room).
            // P9.2: in a room read for its labs, the terminal's stock of every lab resource (the
            // store read once for both), and each lab ([`read_lab`]).
            // Only these three kinds look up their room's mineral and lab reads.
            let lab_kinds = matches!(
                kind,
                Some(StructureType::Extractor | StructureType::Terminal | StructureType::Lab)
            );
            let at = lab_kinds
                .then(|| snapshot.structures.positions().get(row).copied())
                .flatten();
            let site = at.and_then(|at| snapshot.minerals.get(at.room_name()).copied());
            let labs = at.is_some_and(|at| snapshot.labs.wants(at.room_name()));
            if kind == Some(StructureType::Extractor)
                && let (Some(at), Some(site)) = (at, site)
                && site.at == at
            {
                let cooldown = structure.unchecked_ref::<StructureExtractor>().cooldown();
                snapshot.minerals.set_extractor(at, handle, cooldown);
            }
            if kind == Some(StructureType::Terminal)
                && (site.is_some() || labs)
                && let Some(at) = at
            {
                let store = structure.unchecked_ref::<Stored>().store_of();
                let free = u32::try_from(store.get_free_capacity(None)).unwrap_or(0);
                if let Some(site) = site {
                    let stock = site.kind.map_or(0, |k| store.get_used_capacity(Some(k)));
                    snapshot.minerals.set_terminal(MineralTerminal {
                        handle,
                        at,
                        stock,
                        free,
                    });
                }
                if labs {
                    read_terminal_stock(snapshot, &store, at.room_name());
                    snapshot.labs.set_terminal(LabTerminal { handle, at, free });
                }
            }
            if kind == Some(StructureType::Lab)
                && labs
                && let Some(at) = at
            {
                read_lab(snapshot, &structure, handle, row, at);
            }
            let structures = &snapshot.structures;
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

/// Whether a tower in `room` is read this tick (P7.2): its room is full (the fills may take it), a
/// hostile creep stands there this tick (the rooms are read before the structures), or it had an
/// open threat last tick (its towers may heal).
fn tower_wanted(snapshot: &Snapshot, js: &JsWorld, room: RoomName) -> bool {
    let rooms = &snapshot.rooms;
    let full = rooms
        .row_of(room)
        .and_then(|r| rooms.energy().get(r))
        .is_some_and(|e| e.available >= e.capacity);
    let hostile = listed(&js.hostile_rooms, room);
    let threat = listed(&js.threat_rooms, room);
    full || hostile || threat
}

/// Whether `room` is in the sorted `list` (the rooms are read before the structures, so this
/// tick's room lists are complete there).
fn listed(list: &[RoomName], room: RoomName) -> bool {
    list.binary_search_by_key(&room.packed_repr(), |n| n.packed_repr())
        .is_ok()
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
