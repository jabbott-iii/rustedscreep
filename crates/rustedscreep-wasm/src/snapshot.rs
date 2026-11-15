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
//!   `fatigue` unless it is spawning; the last since P2.1, V160);
//! - room: 8 with a controller (`get`, `name`, the name copy, `controller`, `my`, `level`, `find`,
//!   `length`), 6 without; hostile creep: 5 (`get`, `id`, the id copy, `pos`, `__packedPos`;
//!   hostile results never include spawning creeps, V101);
//! - structure: 3 (`get`, `id`, the id copy), plus on the tick it is first seen `structureType`,
//!   and `pos` and `__packedPos` when the type is known (again every tick while that read fails).
//!   With `cache.structure_static` off (the naive baseline, P1.10) those three are read every tick.
//!
//! Calls back from JS into wasm: one `__wbindgen_malloc` per id or name copy, and one
//! `__externref_table_alloc` (with a JS `Table.set`) per object returned through an `Option`: every
//! creep's `id` and a room's `controller` when it has one.
//!
//! The id and name copies are separate calls (`__wbindgen_string_get`) because that call is the
//! type check: a getter typed `-> Option<String>` would save one call per object but traps on a
//! non-string (V100). Each copy is one ASCII string in wasm memory, freed in the same tick.
//!
//! Skipped objects are counted every tick and logged through [`log_skips`], at most one line per
//! `log.skip_window_ticks` (`rustedscreep_core::snapshot::SkipLog`).

use std::fmt::Write as _;

use js_sys::{Array, Object};
use rustedscreep_core::config::CacheConfig;
use rustedscreep_core::snapshot::{
    Controller, CreepRow, ObjectKey, SkipCounts, SkipLog, SkipReason, SkipReport, Snapshot,
    StructureStatic, TickContext, known_structure_type, parse_room_name, position_from_packed,
};
use screeps::{
    Creep, FindConstant, HasId, MaybeHasId, OwnedStructureProperties, Position, Room, RoomObject,
    Structure, find, game,
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
}

/// This tick's JS objects, index-aligned with the snapshot's rows. They are stale after this tick
/// (screeps-game-api `game` docs), so the next pass drops them first.
#[derive(Default)]
pub struct JsWorld {
    pub rooms: Vec<Room>,
    pub creeps: Vec<Creep>,
    pub structures: Vec<Structure>,
}

impl JsWorld {
    fn clear(&mut self) {
        self.rooms.clear();
        self.creeps.clear();
        self.structures.clear();
    }

    /// Whether every table has exactly one JS object per row (the act layer relies on it).
    pub fn aligned_with(&self, snapshot: &Snapshot) -> bool {
        self.rooms.len() == snapshot.rooms.len()
            && self.creeps.len() == snapshot.creeps.len()
            && self.structures.len() == snapshot.structures.len()
    }
}

/// Takes this tick's snapshot, with the cache toggles in force.
pub fn take(snapshot: &mut Snapshot, js: &mut JsWorld, cache: &CacheConfig) {
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
    for room in game::rooms().values() {
        push_room(snapshot, js, room);
    }
    let structures = Object::values(&Game::structures_object());
    for index in 0..structures.length() {
        push_structure(snapshot, js, structures.get(index).unchecked_into());
    }
    snapshot.finish();
}

fn push_room(snapshot: &mut Snapshot, js: &mut JsWorld, room: Room) {
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
    let controller = match room.controller() {
        None => Controller::Absent,
        // `my` is undefined or null on an unowned controller; the crate maps that to false (V101).
        Some(c) => Controller::from_parts(c.my(), c.level()),
    };
    // A creep is in one room and each room is listed once, so no hostile is pushed twice.
    let hostiles = room
        .unchecked_ref::<Findable>()
        .find_array(find::HOSTILE_CREEPS.find_code());
    for index in 0..hostiles.length() {
        push_creep(snapshot, js, hostiles.get(index).unchecked_into(), false);
    }
    snapshot.rooms.push(name, controller);
    js.rooms.push(room);
}

fn push_creep(snapshot: &mut Snapshot, js: &mut JsWorld, creep: Creep, my: bool) {
    let pushed = key_of(creep.try_js_raw_id()).and_then(|key| {
        let pos = position_of(&creep)?;
        // FIND_HOSTILE_CREEPS leaves out spawning creeps (V101), so only own creeps need the read.
        let spawning = my && creep.spawning();
        // Movement needs it only for own creeps that can move (V160); others read as 0.
        let fatigue = if my && !spawning { creep.fatigue() } else { 0 };
        snapshot.creeps.push(
            key,
            CreepRow {
                pos,
                my,
                spawning,
                fatigue,
            },
        )
    });
    match pushed {
        Ok(_) => js.creeps.push(creep),
        Err(reason) => snapshot.skipped.record(reason),
    }
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
        Ok(_) => js.structures.push(structure),
        Err(reason) => snapshot.skipped.record(reason),
    }
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
