//! The snapshot's struct-of-arrays tables (ROADMAP P1 module 5). Each table keeps one column per
//! field, all the same length, and row `i` of every column describes the same object. Columns are
//! cleared at the start of each tick and keep their capacity, so a steady tick allocates nothing here.

use rustedscreep_consts::{Position, ROOM_SIZE, RoomName, StructureType};

use super::SkipReason;
use super::intern::{Handle, Interner, ObjectKey};

/// Marker for creep handles.
#[derive(Debug)]
pub enum CreepClass {}
/// Marker for structure handles.
#[derive(Debug)]
pub enum StructureClass {}

/// A creep's handle: stable while the creep is in the snapshot on consecutive ticks.
pub type CreepHandle = Handle<CreepClass>;
/// A structure's handle: stable while the structure is in the snapshot on consecutive ticks.
pub type StructureHandle = Handle<StructureClass>;

/// Decodes a `RoomPosition.__packedPos` value, or `None` when x or y is outside the room.
///
/// Uses: `ROOM_SIZE`. Layout (screeps-game-api 0.23.5 `Position`, the same as the engine's; V103):
/// room x in bits 24-31, room y in bits 16-23, x in bits 8-15 and y in bits 0-7. Every 16-bit room
/// value is a valid `RoomName`, so only x and y need checking, which is also the only check
/// `Position::from_packed` makes (it asserts), so this never panics.
pub fn position_from_packed(packed: u32) -> Option<Position> {
    let size = u32::from(ROOM_SIZE);
    let x = (packed >> 8) & 0xFF;
    let y = packed & 0xFF;
    (x < size && y < size).then(|| Position::from_packed(packed))
}

/// The structure types this build knows, or `None` for the hidden `__Invalid` variant that
/// screeps-game-api uses for a string it does not recognize (V89, V100). The wildcard is required:
/// outside its crate the wasm-bindgen enum cannot be matched exhaustively, even with `__Invalid`
/// named (E0004, V104), so a variant added by a crate bump lands here as unknown and its structures
/// are skipped and counted until this list and its test are updated.
pub fn known_structure_type(kind: StructureType) -> Option<StructureType> {
    use StructureType::*;
    match kind {
        Spawn | Extension | Road | Wall | Rampart | KeeperLair | Portal | Controller | Link
        | Storage | Tower | Observer | PowerBank | PowerSpawn | Extractor | Lab | Terminal
        | Container | Nuker | Factory | InvaderCore => Some(kind),
        _ => None,
    }
}

/// Who holds a room's controller, from `controller.my` and `controller.level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Controller {
    /// The room has no controller (highway, center or source keeper room).
    Absent,
    /// Nobody owns it (level 0). It may still be reserved; reservations are read from P6.
    Neutral,
    /// The bot owns it, at this level.
    Mine { level: u8 },
    /// Another player owns it, at this level.
    Foreign { level: u8 },
}

impl Controller {
    /// Classifies a controller from its `my` and `level` getters.
    pub const fn from_parts(my: bool, level: u8) -> Self {
        match (my, level) {
            (true, level) => Self::Mine { level },
            (false, 0) => Self::Neutral,
            (false, level) => Self::Foreign { level },
        }
    }
}

/// Visible rooms (`Game.rooms`). Rooms are keyed by name, so they need no handles.
#[derive(Default)]
pub struct Rooms {
    name: Vec<RoomName>,
    controller: Vec<Controller>,
}

impl Rooms {
    pub(super) fn begin(&mut self) {
        self.name.clear();
        self.controller.clear();
    }

    /// Adds a room and returns its row.
    pub fn push(&mut self, name: RoomName, controller: Controller) -> usize {
        let row = self.name.len();
        self.name.push(name);
        self.controller.push(controller);
        row
    }

    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }

    pub fn names(&self) -> &[RoomName] {
        &self.name
    }

    pub fn controllers(&self) -> &[Controller] {
        &self.controller
    }

    /// The row of a room, by linear search (a tick sees few rooms).
    pub fn row_of(&self, name: RoomName) -> Option<usize> {
        self.name.iter().position(|&n| n == name)
    }

    /// Rows of the rooms whose controller the bot owns.
    pub fn owned(&self) -> impl Iterator<Item = usize> + '_ {
        self.controller
            .iter()
            .enumerate()
            .filter_map(|(row, c)| matches!(c, Controller::Mine { .. }).then_some(row))
    }
}

/// One creep's columns, as the snapshot reads them each tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreepRow {
    pub pos: Position,
    /// The bot's own creep (from `Game.creeps`) rather than a hostile one (from `FIND_HOSTILE_CREEPS`).
    pub my: bool,
    /// Still being spawned; its position is its spawn's.
    pub spawning: bool,
}

/// Visible creeps: the bot's own (`Game.creeps`) and hostile ones in visible rooms.
#[derive(Default)]
pub struct Creeps {
    ids: Interner<CreepClass, ()>,
    handle: Vec<CreepHandle>,
    pos: Vec<Position>,
    my: Vec<bool>,
    spawning: Vec<bool>,
}

impl Creeps {
    pub(super) fn begin(&mut self) {
        self.ids.begin_pass();
        self.handle.clear();
        self.pos.clear();
        self.my.clear();
        self.spawning.clear();
    }

    pub(super) fn finish(&mut self) -> u32 {
        self.ids.end_pass()
    }

    /// Adds a creep and returns its handle. On an error nothing is added.
    pub fn push(&mut self, key: ObjectKey, row: CreepRow) -> Result<CreepHandle, SkipReason> {
        let index = u32::try_from(self.handle.len()).map_err(|_| SkipReason::SlotsExhausted)?;
        let (handle, ()) = self.ids.mark(key, index, || Ok(()))?;
        self.handle.push(handle);
        self.pos.push(row.pos);
        self.my.push(row.my);
        self.spawning.push(row.spawning);
        Ok(handle)
    }

    pub fn len(&self) -> usize {
        self.handle.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handle.is_empty()
    }

    /// The handle's row in this tick's columns, if the creep is in this tick's snapshot.
    pub fn row_of(&self, handle: CreepHandle) -> Option<usize> {
        usize::try_from(self.ids.row(handle)?).ok()
    }

    /// The live handle of a creep id, if the creep was in the last snapshot.
    pub fn handle_of(&self, key: ObjectKey) -> Option<CreepHandle> {
        self.ids.get(key)
    }

    /// The id of a live creep handle, for an order that survives a global reset.
    pub fn key_of(&self, handle: CreepHandle) -> Option<ObjectKey> {
        self.ids.key(handle)
    }

    pub fn handles(&self) -> &[CreepHandle] {
        &self.handle
    }

    pub fn positions(&self) -> &[Position] {
        &self.pos
    }

    pub fn my(&self) -> &[bool] {
        &self.my
    }

    pub fn spawning(&self) -> &[bool] {
        &self.spawning
    }

    /// Creeps first seen this tick.
    pub fn added(&self) -> u32 {
        self.ids.added()
    }

    /// Handles released at the end of this tick's pass (creeps that were gone).
    pub fn released(&self) -> u32 {
        self.ids.released()
    }

    /// Live handles.
    pub fn live(&self) -> usize {
        self.ids.live()
    }
}

/// What never changes for a structure: read from JS once, when the structure is first seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StructureStatic {
    pub kind: StructureType,
    pub pos: Position,
}

/// The bot's own structures (`Game.structures`). Unowned and hostile structures come with the
/// modules that use them (P2 onward).
#[derive(Default)]
pub struct Structures {
    ids: Interner<StructureClass, StructureStatic>,
    handle: Vec<StructureHandle>,
    kind: Vec<StructureType>,
    pos: Vec<Position>,
    /// The static-data cache is off (`CacheConfig::structure_static` false): read every push.
    reread_static: bool,
}

impl Structures {
    pub(super) fn begin(&mut self) {
        self.ids.begin_pass();
        self.handle.clear();
        self.kind.clear();
        self.pos.clear();
    }

    pub(super) fn finish(&mut self) -> u32 {
        self.ids.end_pass()
    }

    /// Switches the static-data cache (`CacheConfig::structure_static`, on by default): off, every
    /// push reads the type and position again (the naive baseline), and the handles stay as they are.
    pub fn set_static_cache(&mut self, on: bool) {
        self.reread_static = !on;
    }

    /// Adds a structure and returns its handle. With the static-data cache on, `read_static` runs
    /// only when the id is new; on any error nothing is added, so a structure whose static read fails
    /// is read again next tick. With it off, `read_static` runs on every push, and its error skips
    /// the structure for this tick (its handle is released at the end of the pass).
    pub fn push(
        &mut self,
        key: ObjectKey,
        read_static: impl FnOnce() -> Result<StructureStatic, SkipReason>,
    ) -> Result<StructureHandle, SkipReason> {
        let index = u32::try_from(self.handle.len()).map_err(|_| SkipReason::SlotsExhausted)?;
        let (handle, data) = if self.reread_static {
            self.ids.mark_fresh(key, index, read_static)?
        } else {
            self.ids.mark(key, index, read_static)?
        };
        let data = *data;
        self.handle.push(handle);
        self.kind.push(data.kind);
        self.pos.push(data.pos);
        Ok(handle)
    }

    pub fn len(&self) -> usize {
        self.handle.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handle.is_empty()
    }

    /// The handle's row in this tick's columns, if the structure is in this tick's snapshot.
    pub fn row_of(&self, handle: StructureHandle) -> Option<usize> {
        usize::try_from(self.ids.row(handle)?).ok()
    }

    /// The live handle of a structure id, if the structure was in the last snapshot.
    pub fn handle_of(&self, key: ObjectKey) -> Option<StructureHandle> {
        self.ids.get(key)
    }

    /// The id of a live structure handle, for an order that survives a global reset.
    pub fn key_of(&self, handle: StructureHandle) -> Option<ObjectKey> {
        self.ids.key(handle)
    }

    pub fn handles(&self) -> &[StructureHandle] {
        &self.handle
    }

    pub fn kinds(&self) -> &[StructureType] {
        &self.kind
    }

    pub fn positions(&self) -> &[Position] {
        &self.pos
    }

    /// Structures first seen this tick (their static data was read from JS).
    pub fn added(&self) -> u32 {
        self.ids.added()
    }

    /// Handles released at the end of this tick's pass (structures that were gone).
    pub fn released(&self) -> u32 {
        self.ids.released()
    }

    /// Live handles.
    pub fn live(&self) -> usize {
        self.ids.live()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustedscreep_consts::RoomCoordinate;

    fn room(name: &str) -> RoomName {
        name.parse().expect("valid room name")
    }

    fn pos(x: u8, y: u8, name: &str) -> Position {
        Position::new(
            RoomCoordinate::new(x).expect("in room"),
            RoomCoordinate::new(y).expect("in room"),
            room(name),
        )
    }

    /// Uses: ROOM_SIZE. Every in-room x and y decodes; x or y equal to ROOM_SIZE (or any larger
    /// byte) does not, so `Position::from_packed`'s assertion is never reached.
    #[test]
    fn packed_positions_decode_only_inside_the_room() {
        let base = pos(0, 0, "W1N1").packed_repr();
        let last = ROOM_SIZE - 1;
        for (x, y) in [(0, 0), (last, 0), (0, last), (last, last), (25, 10)] {
            let packed = base | (u32::from(x) << 8) | u32::from(y);
            assert_eq!(position_from_packed(packed), Some(pos(x, y, "W1N1")));
        }
        for (x, y) in [(ROOM_SIZE, 0), (0, ROOM_SIZE), (u8::MAX, 0), (0, u8::MAX)] {
            let packed = base | (u32::from(x) << 8) | u32::from(y);
            assert_eq!(position_from_packed(packed), None, "({x}, {y})");
        }
    }

    /// Every 16-bit room value decodes to some room name, so the room part needs no check.
    #[test]
    fn every_room_bits_value_decodes() {
        for room_bits in 0u32..=0xFFFF {
            let packed = (room_bits << 16) | (1 << 8) | 2;
            let p = position_from_packed(packed).expect("in room");
            assert_eq!(p.packed_repr(), packed);
            let name = p.room_name().to_string();
            assert_eq!(
                name.parse::<RoomName>().ok().map(|r| r.packed_repr()),
                u16::try_from(room_bits).ok()
            );
        }
    }

    /// Pinned examples from the engine's `__packedPos` formula (VERIFICATION_LOG V103):
    /// `(roomX + 128) << 24 | (roomY + 128) << 16 | x << 8 | y`, with W1N1 = (-2, -2) and E0S0 = (0, 0).
    /// The engine's value for an E or S room is a negative int32; the wasm boundary passes the same
    /// 32 bits (V100), which is the u32 here.
    #[test]
    fn engine_packed_examples_decode() {
        assert_eq!(position_from_packed(0x7E7E_190A), Some(pos(25, 10, "W1N1")));
        assert_eq!(position_from_packed(0x8080_190A), Some(pos(25, 10, "E0S0")));
        let engine_signed: i32 = -2_139_088_630; // 0x8080190A as the engine's int32
        assert_eq!(engine_signed as u32, 0x8080_190A);
    }

    #[test]
    fn known_structure_types_pass_and_the_hidden_variant_does_not() {
        use StructureType::*;
        let known = [
            Spawn,
            Extension,
            Road,
            Wall,
            Rampart,
            KeeperLair,
            Portal,
            Controller,
            Link,
            Storage,
            Tower,
            Observer,
            PowerBank,
            PowerSpawn,
            Extractor,
            Lab,
            Terminal,
            Container,
            Nuker,
            Factory,
            InvaderCore,
        ];
        for kind in known {
            assert_eq!(known_structure_type(kind), Some(kind));
        }
        assert_eq!(known_structure_type(StructureType::__Invalid), None);
    }

    #[test]
    fn controllers_classify_by_owner_and_level() {
        assert_eq!(
            Controller::from_parts(true, 3),
            Controller::Mine { level: 3 }
        );
        assert_eq!(Controller::from_parts(false, 0), Controller::Neutral);
        assert_eq!(
            Controller::from_parts(false, 8),
            Controller::Foreign { level: 8 }
        );
    }

    #[test]
    fn rooms_find_rows_and_owned_rooms() {
        let mut rooms = Rooms::default();
        rooms.begin();
        assert_eq!(rooms.push(room("W1N1"), Controller::Mine { level: 2 }), 0);
        assert_eq!(rooms.push(room("W2N1"), Controller::Absent), 1);
        assert_eq!(rooms.push(room("W3N1"), Controller::Mine { level: 1 }), 2);
        assert_eq!(rooms.row_of(room("W2N1")), Some(1));
        assert_eq!(rooms.row_of(room("W9N9")), None);
        assert_eq!(rooms.owned().collect::<Vec<_>>(), [0, 2]);
        rooms.begin();
        assert!(rooms.is_empty());
    }

    #[test]
    fn creep_rows_stay_aligned_and_errors_add_nothing() {
        let mut creeps = Creeps::default();
        creeps.begin();
        let row = CreepRow {
            pos: pos(10, 10, "W1N1"),
            my: true,
            spawning: false,
        };
        let a = creeps.push(ObjectKey(1), row).expect("new");
        let hostile = CreepRow {
            pos: pos(20, 20, "W1N1"),
            my: false,
            spawning: false,
        };
        let b = creeps.push(ObjectKey(2), hostile).expect("new");
        assert_eq!(creeps.push(ObjectKey(1), row), Err(SkipReason::Duplicate));
        assert_eq!(creeps.len(), 2);
        assert_eq!(creeps.finish(), 0);
        assert_eq!((creeps.row_of(a), creeps.row_of(b)), (Some(0), Some(1)));
        assert_eq!(creeps.positions(), [row.pos, hostile.pos]);
        assert_eq!(
            (creeps.my(), creeps.spawning()),
            (&[true, false][..], &[false, false][..])
        );
        assert_eq!(creeps.handles(), [a, b]);
        assert_eq!(creeps.handle_of(ObjectKey(2)), Some(b));
        assert_eq!(creeps.key_of(b), Some(ObjectKey(2)));
    }

    #[test]
    fn structure_statics_are_read_on_first_sight_only() {
        let mut structures = Structures::default();
        let spawn = StructureStatic {
            kind: StructureType::Spawn,
            pos: pos(25, 25, "W1N1"),
        };
        let mut reads = 0;
        let mut handles = Vec::new();
        for _ in 0..3 {
            structures.begin();
            let h = structures
                .push(ObjectKey(9), || {
                    reads += 1;
                    Ok(spawn)
                })
                .expect("pushed");
            handles.push(h);
            structures.finish();
            assert_eq!(structures.kinds(), [StructureType::Spawn]);
            assert_eq!(structures.positions(), [spawn.pos]);
        }
        assert_eq!(reads, 1);
        assert!(
            handles.windows(2).all(|w| w[0] == w[1]),
            "one handle for its life"
        );
        assert_eq!(structures.added(), 0);

        // A failed static read adds nothing; the next tick retries it.
        structures.begin();
        let bad = structures.push(ObjectKey(10), || Err(SkipReason::UnknownStructureType));
        assert_eq!(bad, Err(SkipReason::UnknownStructureType));
        assert!(structures.is_empty());
        assert_eq!(
            structures.finish(),
            1,
            "the spawn was not pushed this tick, so it is released"
        );
        assert_eq!(structures.row_of(handles[0]), None);
        assert_eq!(structures.live(), 0);
    }

    /// With the static-data cache off, every push reads the static data again and the new values
    /// win, while the handle stays the same; a failed re-read skips the structure for that tick and
    /// releases its handle; switching the cache back on stops the reads.
    #[test]
    fn structure_statics_are_read_every_push_with_the_cache_off() {
        let mut structures = Structures::default();
        structures.set_static_cache(false);
        let at = |x| StructureStatic {
            kind: StructureType::Spawn,
            pos: pos(x, 25, "W1N1"),
        };
        let mut reads = 0;
        let mut handles = Vec::new();
        for x in 1..=3 {
            structures.begin();
            let h = structures
                .push(ObjectKey(9), || {
                    reads += 1;
                    Ok(at(x))
                })
                .expect("pushed");
            handles.push(h);
            assert_eq!(
                structures.positions(),
                [at(x).pos],
                "the re-read value is used"
            );
            structures.finish();
        }
        assert_eq!(reads, 3);
        assert!(
            handles.windows(2).all(|w| w[0] == w[1]),
            "handles do not churn"
        );
        assert_eq!(structures.added(), 0);

        structures.begin();
        let failed = structures.push(ObjectKey(9), || Err(SkipReason::UnknownStructureType));
        assert_eq!(failed, Err(SkipReason::UnknownStructureType));
        assert!(structures.is_empty());
        assert_eq!(structures.finish(), 1);
        assert_eq!(structures.live(), 0);

        // A duplicate is refused before any read.
        structures.begin();
        structures.push(ObjectKey(5), || Ok(at(5))).expect("pushed");
        let mut dup_reads = 0;
        let dup = structures.push(ObjectKey(5), || {
            dup_reads += 1;
            Ok(at(6))
        });
        assert_eq!(dup, Err(SkipReason::Duplicate));
        assert_eq!(dup_reads, 0);
        assert_eq!(structures.positions(), [at(5).pos]);
        structures.finish();

        structures.set_static_cache(true);
        let mut cached_reads = 0;
        structures.begin();
        structures
            .push(ObjectKey(5), || {
                cached_reads += 1;
                Ok(at(7))
            })
            .expect("pushed");
        structures.finish();
        assert_eq!(cached_reads, 0);
        assert_eq!(structures.positions(), [at(5).pos]);
    }
}
