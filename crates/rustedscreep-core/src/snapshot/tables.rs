//! The snapshot's struct-of-arrays tables (ROADMAP P1 module 5). Each table keeps one column per
//! field, all the same length, and row `i` of every column describes the same object. Columns are
//! cleared at the start of each tick and keep their capacity, so a steady tick allocates nothing here.

use rustedscreep_consts::{
    MAX_CREEP_SIZE, Part, Position, ROOM_SIZE, ResourceType, RoomName, StructureType,
};

use super::SkipReason;
use super::intern::{Handle, Interner, ObjectKey};
use crate::spawning::Identity;

/// Marker for creep handles.
#[derive(Debug)]
pub enum CreepClass {}
/// Marker for structure handles.
#[derive(Debug)]
pub enum StructureClass {}
/// Marker for room-object handles (P2.4).
#[derive(Debug)]
pub enum ObjectClass {}

/// A creep's handle: stable while the creep is in the snapshot on consecutive ticks.
pub type CreepHandle = Handle<CreepClass>;
/// A structure's handle: stable while the structure is in the snapshot on consecutive ticks.
pub type StructureHandle = Handle<StructureClass>;
/// A room object's handle (a source, an own site, a pile, a tombstone, a ruin or a container; P2.4):
/// stable while the object is in the snapshot on consecutive ticks.
pub type ObjectHandle = Handle<ObjectClass>;

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

    /// Whether a creep standing in this room may gain or lose a boost from a lab the bot does not
    /// own (P7.1): a boost changes only within range 1 of an active lab (V240, V290), and a lab is
    /// active only in a room whose controller its owner holds at a level with a lab allowance
    /// (V291). So only another player's room at such a level; the bot's own labs never boost a
    /// hostile creep (no code calls `boostCreep` on one).
    ///
    /// Uses: rustedscreep_consts::StructureType::controller_structures (`CONTROLLER_STRUCTURES`,
    /// V234).
    pub fn boosts_possible(self) -> bool {
        match self {
            Self::Foreign { level } => {
                StructureType::Lab.controller_structures(u32::from(level)) > 0
            }
            Self::Absent | Self::Neutral | Self::Mine { .. } => false,
        }
    }
}

/// A room's spawn energy: `Room.energyAvailable` and `energyCapacityAvailable`, the sums over the
/// bot's spawns and extensions that are not off (V175). Read for owned rooms only; 0 elsewhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoomEnergy {
    /// Unit: energy.
    pub available: u32,
    /// Unit: energy.
    pub capacity: u32,
}

/// An owned controller's progress and timers (P2.4), read for owned rooms only; all 0 (and not
/// blocked) elsewhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControllerState {
    /// `progress`. Unit: energy.
    pub progress: u32,
    /// `ticksToDowngrade`. Unit: ticks.
    pub downgrade_ticks: u32,
    /// `upgradeBlocked` is set: `upgradeController` is refused until it ends.
    pub upgrade_blocked: bool,
    /// The controller's safe-mode state (P7.4), read only on a tick a hostile creep stands in the
    /// room; `None` on other ticks.
    pub safe_mode: Option<SafeModeState>,
}

/// An owned controller's safe-mode getters (P7.4, V327): undefined values read as 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SafeModeState {
    /// `safeMode`: the ticks the room's safe mode has left, 0 when none is active. Unit: ticks.
    pub ticks: u32,
    /// `safeModeAvailable`: activations left. Unit: activations.
    pub available: u32,
    /// `safeModeCooldown`: the ticks before the room may activate again, 0 when none. Unit: ticks.
    pub cooldown: u32,
}

/// Who holds a controller's reservation (`controller.reservation`, V263, V264), read in remote
/// rooms (P6.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reservation {
    /// No reservation, or a room not read for one.
    #[default]
    None,
    /// The bot's own, ending in `ticks` (`ticksToEnd`). Unit: ticks.
    Mine { ticks: u32 },
    /// Another user's (another player's, or an NPC invader core's, V101). Unit: ticks.
    Other { ticks: u32 },
}

/// What the snapshot reads in a remote room (P6.2, ADR-029): a visible room the bot does not own
/// that an own creep's name gives as its remote room. Default (not read) for every other room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteRead {
    /// The room was read as a remote room this tick: its room objects, its controller's
    /// reservation and its hostile structures.
    pub read: bool,
    pub reservation: Reservation,
    /// Structures of other users (`FIND_HOSTILE_STRUCTURES`: an invader core or a source keeper's
    /// lair among them). Unit: structures.
    pub hostile_structures: u32,
    /// Its roads were read this tick (they are read every `remote.road_read_ticks`).
    pub roads: bool,
}

/// What a controller's `sign` showed (ADR-030, V275), read in a room an own signer stands in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SignSeen {
    /// No sign, or a room not read for one.
    #[default]
    None,
    /// The signer's text (`signing::SIGN_TEXT`), whoever wrote it.
    Ours,
    /// Another text.
    Other,
    /// The server's sign (username `SYSTEM_USERNAME`): a planned novice or respawn area hides any
    /// player's sign while it lasts (V275).
    System,
}

/// The most towers [`SignRead`] keeps: `CONTROLLER_STRUCTURES` for towers at level 8 (a test
/// checks it).
pub const SIGN_TOWERS: usize = 6;

/// What the snapshot reads in a room an own signer stands in (ADR-030): its controller's sign and
/// the other owner's towers. Default (not read) for every other room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SignRead {
    /// The room was read for the signer this tick.
    pub read: bool,
    pub sign: SignSeen,
    /// The positions of towers of other users (`FIND_HOSTILE_STRUCTURES` of type tower), the first
    /// [`SIGN_TOWERS`]; `towers` counts all of them.
    pub tower_at: [Option<Position>; SIGN_TOWERS],
    pub towers: u32,
}

impl SignRead {
    /// The towers' positions read.
    pub fn tower_positions(&self) -> impl Iterator<Item = Position> + '_ {
        self.tower_at.iter().flatten().copied()
    }

    /// Adds a tower: kept while fewer than [`SIGN_TOWERS`] are, counted always.
    pub fn add_tower(&mut self, at: Position) {
        if let Some(slot) = self.tower_at.iter_mut().find(|s| s.is_none()) {
            *slot = Some(at);
        }
        self.towers = self.towers.saturating_add(1);
    }
}

/// A room's status as `Game.map.getRoomStatus` gives it (V168), read for the atlas (ADR-039).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoomStatus {
    /// Open to everyone (`normal`), or a room not read for it.
    #[default]
    Normal,
    /// Closed, out of the world's borders, or a status this build does not know (the safe reading:
    /// no creep goes there).
    Closed,
    /// A novice area, walled off from the rest of the world until its timer ends.
    Novice,
    /// A respawn area, walled off the same way.
    Respawn,
}

/// The most source tiles [`IntelRead`] and the atlas keep. A room has at most three (a keeper's
/// room or a sector's center room), so one more is spare. Unit: sources.
pub const INTEL_SOURCES: usize = 4;

/// A room's mineral as an intel read gives it (V364).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MineralRead {
    /// Its `mineralType`; `None` for a type this build does not list.
    pub kind: Option<ResourceType>,
    pub at: Position,
    /// `density`: 1 (low) to 4 (ultra). Unit: density level.
    pub density: u8,
}

/// What the snapshot reads in a visible room for the atlas (ADR-039): a room the bot does not own
/// whose record is missing or stale. Default (not read) for every other room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntelRead {
    /// The room was read for the atlas this tick.
    pub read: bool,
    pub status: RoomStatus,
    /// The controller's position, for a room that has one.
    pub controller_at: Option<Position>,
    /// Who reserves the controller, and for how long (V263, V264).
    pub reservation: Reservation,
    /// Another owner's safe mode: the ticks it has left, 0 when none (V337). Unit: ticks.
    pub safe_mode: u32,
    /// The sources' positions, the first [`INTEL_SOURCES`].
    pub source_at: [Option<Position>; INTEL_SOURCES],
    /// The room's mineral (the first one, for a room that shows more).
    pub mineral: Option<MineralRead>,
    /// Structures of other users by type (`FIND_HOSTILE_STRUCTURES`, V267), each capped at 255.
    /// Unit: structures.
    pub towers: u8,
    pub spawns: u8,
    /// Source keepers' lairs.
    pub lairs: u8,
    /// An invader core's `level`, when one stands there (V365).
    pub core: Option<u8>,
    /// Hostile creeps in the room this tick, capped at 255. Unit: creeps.
    pub hostiles: u8,
}

impl IntelRead {
    /// The sources' positions read.
    pub fn source_positions(&self) -> impl Iterator<Item = Position> + '_ {
        self.source_at.iter().flatten().copied()
    }

    /// Adds a source: kept while fewer than [`INTEL_SOURCES`] are.
    pub fn add_source(&mut self, at: Position) {
        if let Some(slot) = self.source_at.iter_mut().find(|s| s.is_none()) {
            *slot = Some(at);
        }
    }
}

/// Visible rooms (`Game.rooms`). Rooms are keyed by name, so they need no handles.
#[derive(Default)]
pub struct Rooms {
    name: Vec<RoomName>,
    controller: Vec<Controller>,
    energy: Vec<RoomEnergy>,
    state: Vec<ControllerState>,
    remote: Vec<RemoteRead>,
    sign: Vec<SignRead>,
    intel: Vec<IntelRead>,
    /// Whether every road's and own rampart's hits were read in the room this tick (P7.3,
    /// `upkeep.read_ticks`).
    upkeep: Vec<bool>,
}

impl Rooms {
    pub(super) fn begin(&mut self) {
        self.name.clear();
        self.controller.clear();
        self.energy.clear();
        self.state.clear();
        self.remote.clear();
        self.sign.clear();
        self.intel.clear();
        self.upkeep.clear();
    }

    /// Adds a room and returns its row. Its energy and controller state are 0 until
    /// [`Self::set_energy`] and [`Self::set_controller_state`], and it is not read as a remote room
    /// until [`Self::set_remote`].
    pub fn push(&mut self, name: RoomName, controller: Controller) -> usize {
        let row = self.name.len();
        self.name.push(name);
        self.controller.push(controller);
        self.energy.push(RoomEnergy::default());
        self.state.push(ControllerState::default());
        self.remote.push(RemoteRead::default());
        self.sign.push(SignRead::default());
        self.intel.push(IntelRead::default());
        self.upkeep.push(false);
        row
    }

    /// Marks the room at `row` as read in full for upkeep this tick (P7.3): every road with its
    /// `hits` and `hitsMax` is in [`RoomObjects`], and every own rampart's hits in
    /// [`Structures::hits`]. Nothing for a row that does not exist.
    pub fn set_upkeep_read(&mut self, row: usize) {
        if let Some(r) = self.upkeep.get_mut(row) {
            *r = true;
        }
    }

    /// Per room, whether it was read in full for upkeep this tick ([`Self::set_upkeep_read`]).
    pub fn upkeep_reads(&self) -> &[bool] {
        &self.upkeep
    }

    /// Sets what was read for the signer in the room at `row` (ADR-030); nothing for a row that
    /// does not exist.
    pub fn set_sign(&mut self, row: usize, read: SignRead) {
        if let Some(r) = self.sign.get_mut(row) {
            *r = read;
        }
    }

    pub fn sign_reads(&self) -> &[SignRead] {
        &self.sign
    }

    /// Sets what was read for the atlas in the room at `row` (ADR-039); nothing for a row that does
    /// not exist.
    pub fn set_intel(&mut self, row: usize, read: IntelRead) {
        if let Some(r) = self.intel.get_mut(row) {
            *r = read;
        }
    }

    pub fn intel_reads(&self) -> &[IntelRead] {
        &self.intel
    }

    /// Sets what was read in the remote room at `row` (P6.2); nothing for a row that does not exist.
    pub fn set_remote(&mut self, row: usize, read: RemoteRead) {
        if let Some(r) = self.remote.get_mut(row) {
            *r = read;
        }
    }

    pub fn remote_reads(&self) -> &[RemoteRead] {
        &self.remote
    }

    /// Sets the controller state of the room at `row` (P2.4); nothing for a row that does not exist.
    pub fn set_controller_state(&mut self, row: usize, state: ControllerState) {
        if let Some(s) = self.state.get_mut(row) {
            *s = state;
        }
    }

    pub fn controller_states(&self) -> &[ControllerState] {
        &self.state
    }

    /// Sets the spawn energy of the room at `row` (P2.3); nothing for a row that does not exist.
    pub fn set_energy(&mut self, row: usize, energy: RoomEnergy) {
        if let Some(e) = self.energy.get_mut(row) {
            *e = energy;
        }
    }

    pub fn energy(&self) -> &[RoomEnergy] {
        &self.energy
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
    /// `creep.fatigue` (V160); read for own creeps only, 0 for hostile ones. A creep with fatigue
    /// cannot move unless pulled (V50).
    pub fatigue: u32,
}

/// What an own creep carries (P2.4): read for own creeps that are not spawning; 0 otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CreepStore {
    /// Energy in the store. Unit: energy.
    pub energy: u32,
    /// The store's capacity for energy, as `store.getCapacity(RESOURCE_ENERGY)` gives it. Unit: energy.
    pub capacity: u32,
}

impl CreepStore {
    /// Room left for energy (0 when the store holds more than its capacity).
    pub const fn free(self) -> u32 {
        self.capacity.saturating_sub(self.energy)
    }
}

/// Visible creeps: the bot's own (`Game.creeps`) and hostile ones in visible rooms.
#[derive(Default)]
pub struct Creeps {
    /// The static data is the identity its name gave an own creep, read once per handle (P2.3).
    ids: Interner<CreepClass, Option<Identity>>,
    handle: Vec<CreepHandle>,
    pos: Vec<Position>,
    my: Vec<bool>,
    spawning: Vec<bool>,
    fatigue: Vec<u32>,
    identity: Vec<Option<Identity>>,
    store: Vec<CreepStore>,
    /// The tick each own creep is gone by (P5.2, [`Self::set_death`]), per row.
    death: Vec<Option<u32>>,
    /// The death ticks known so far, by handle slot with the handle that owns the entry (a slot is
    /// reused by later creeps, which the handle's generation tells apart). Kept between ticks: a
    /// creep's `ageTime` never changes once set (V170) and the bot never renews a creep, so its
    /// death tick is read once in its life and once after a global reset.
    lives: Vec<Option<(CreepHandle, u32)>>,
    /// The `hitsMax` known so far for own creeps, by handle slot with the handle that owns the entry
    /// (P7.2, [`Self::set_own_hits`]): a creep's never changes, so it is read once in its life.
    max_hits: Vec<Option<(CreepHandle, u32)>>,
    /// This tick's rows of own creeps whose hits were read (P7.2), in the order they were set.
    own_hits: Vec<u32>,
    /// A hostile creep's active combat parts (P6.3, [`Self::set_combat`]), per row.
    combat: Vec<Combat>,
    /// Each creep's `hits` (P7.1, [`Self::set_assessed`]), per row: read for hostile creeps in owned
    /// rooms, and for own creeps in rooms with an open threat (P7.2, [`Self::set_own_hits`]); 0 for
    /// every other creep. Unit: hit points.
    hits: Vec<u32>,
    /// Each own creep's `hitsMax` where its `hits` is read (P7.2), per row; 0 otherwise (a hostile's
    /// follows from its body, `BODYPART_HITS` a part). Unit: hit points.
    hits_max: Vec<u32>,
    /// This tick's rows of hostile creeps in owned rooms (P7.1), in the order they were set.
    assessed: Vec<u32>,
    /// The bodies read so far, each with the handle it belongs to (P7.1, [`Self::set_body`]). Kept
    /// between ticks, so a hostile's body is read once while it stays in the snapshot and out of
    /// rooms where it may be boosted ([`Self::forget_body`]); the entry of a released handle is
    /// dropped at the end of the pass. Few hostiles stand in owned rooms at
    /// once, so a lookup is a short linear search.
    bodies: Vec<(CreepHandle, BodyRead)>,
}

/// Parts in the longest body.
const BODY_PARTS: usize = MAX_CREEP_SIZE as usize;

/// One body part as the snapshot reads it (P7.1, V289): its type and its boost compound, `None` for
/// an unboosted part. A string the crate does not know reads as the enum's hidden invalid variant,
/// which matches no part type and no boost, so it adds no power anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyPartRead {
    pub part: Part,
    pub boost: Option<ResourceType>,
}

/// A creep's body as the snapshot reads it once in its life (P7.1): its parts in `Creep.body`'s
/// order, the first part first. Damage takes the first parts first (V283). At most
/// `MAX_CREEP_SIZE` parts, kept inline so a read allocates nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyRead {
    len: u8,
    parts: [BodyPartRead; BODY_PARTS],
}

impl BodyRead {
    /// A body with no parts.
    pub const EMPTY: Self = Self {
        len: 0,
        parts: [BodyPartRead {
            part: Part::Move,
            boost: None,
        }; BODY_PARTS],
    };

    /// The body of these parts, or `None` for more than `MAX_CREEP_SIZE`.
    pub fn of(parts: &[BodyPartRead]) -> Option<Self> {
        let mut body = Self::EMPTY;
        for &part in parts {
            if !body.push(part) {
                return None;
            }
        }
        Some(body)
    }

    /// Adds a part after the others; false, with nothing added, once the body holds
    /// `MAX_CREEP_SIZE` parts.
    pub fn push(&mut self, part: BodyPartRead) -> bool {
        let Some(cell) = self.parts.get_mut(usize::from(self.len)) else {
            return false;
        };
        *cell = part;
        self.len = self.len.saturating_add(1);
        true
    }

    /// The parts, the first part first.
    pub fn parts(&self) -> &[BodyPartRead] {
        self.parts.get(..usize::from(self.len)).unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for BodyRead {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// A hostile creep's active combat parts (`getActiveBodyparts`, V269), read in remote rooms only
/// (P6.3); zero for every other creep. Unit: parts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Combat {
    pub attack: u32,
    pub ranged_attack: u32,
}

impl Combat {
    /// Whether the creep can hurt the bot's creeps.
    pub const fn armed(self) -> bool {
        self.attack > 0 || self.ranged_attack > 0
    }
}

impl Creeps {
    pub(super) fn begin(&mut self) {
        self.ids.begin_pass();
        self.handle.clear();
        self.pos.clear();
        self.my.clear();
        self.spawning.clear();
        self.fatigue.clear();
        self.identity.clear();
        self.store.clear();
        self.death.clear();
        self.combat.clear();
        self.hits.clear();
        self.hits_max.clear();
        self.assessed.clear();
        self.own_hits.clear();
    }

    pub(super) fn finish(&mut self) -> u32 {
        let released = self.ids.end_pass();
        let ids = &self.ids;
        self.bodies.retain(|&(handle, _)| ids.key(handle).is_some());
        released
    }

    /// Adds a creep with no identity and returns its handle. On an error nothing is added.
    pub fn push(&mut self, key: ObjectKey, row: CreepRow) -> Result<CreepHandle, SkipReason> {
        self.push_named(key, row, || None)
    }

    /// Adds a creep and returns its handle; `identity` runs only when the creep has no live handle
    /// yet, so an own creep's name is read and parsed once in its life (and once more after a
    /// global reset). On an error nothing is added.
    pub fn push_named(
        &mut self,
        key: ObjectKey,
        row: CreepRow,
        identity: impl FnOnce() -> Option<Identity>,
    ) -> Result<CreepHandle, SkipReason> {
        let index = u32::try_from(self.handle.len()).map_err(|_| SkipReason::SlotsExhausted)?;
        let (handle, &id) = self.ids.mark(key, index, || Ok(identity()))?;
        self.handle.push(handle);
        self.pos.push(row.pos);
        self.my.push(row.my);
        self.spawning.push(row.spawning);
        self.fatigue.push(row.fatigue);
        self.identity.push(id);
        self.store.push(CreepStore::default());
        self.combat.push(Combat::default());
        self.hits.push(0);
        self.hits_max.push(0);
        let death = usize::try_from(handle.slot())
            .ok()
            .and_then(|slot| self.lives.get(slot).copied().flatten())
            .filter(|&(h, _)| h == handle)
            .map(|(_, d)| d);
        self.death.push(death);
        Ok(handle)
    }

    /// Records that the creep at `row` is gone by tick `death` (P5.2): `Game.time` plus the
    /// `ticksToLive` it shows now (V198). It is kept for the creep's handle, so later ticks know it
    /// without a read; nothing for a row that does not exist.
    pub fn set_death(&mut self, row: usize, death: u32) {
        let (Some(&handle), Some(cell)) = (self.handle.get(row), self.death.get_mut(row)) else {
            return;
        };
        *cell = Some(death);
        let Ok(slot) = usize::try_from(handle.slot()) else {
            return;
        };
        if self.lives.len() <= slot {
            self.lives.resize(slot.saturating_add(1), None);
        }
        if let Some(entry) = self.lives.get_mut(slot) {
            *entry = Some((handle, death));
        }
    }

    /// Sets the active combat parts of the hostile creep at `row` (P6.3); nothing for a row that
    /// does not exist.
    pub fn set_combat(&mut self, row: usize, combat: Combat) {
        if let Some(c) = self.combat.get_mut(row) {
            *c = combat;
        }
    }

    /// Each creep's active combat parts: read for hostiles in remote rooms only (P6.3).
    pub fn combat(&self) -> &[Combat] {
        &self.combat
    }

    /// Records the `hits` of the hostile creep at `row`, which stands in an owned room (P7.1), and
    /// lists the row in [`Self::assessed`]. Rows are set in the order they are pushed, so a row that
    /// does not exist, or is not after the last one set this pass, is left alone.
    pub fn set_assessed(&mut self, row: usize, hits: u32) {
        let (Some(cell), Ok(index)) = (self.hits.get_mut(row), u32::try_from(row)) else {
            return;
        };
        if self.assessed.last().is_some_and(|&last| last >= index) {
            return;
        }
        *cell = hits;
        self.assessed.push(index);
    }

    /// Records the `hits` and `hitsMax` of the own creep at `row` (P7.2: towers heal it), keeps its
    /// `hitsMax` for its handle ([`Self::known_hits_max`]), and lists the row in
    /// [`Self::own_hits_rows`]; nothing for a row that does not exist or was set before.
    pub fn set_own_hits(&mut self, row: usize, hits: u32, hits_max: u32) {
        let (Some(h), Some(m), Ok(index)) = (
            self.hits.get_mut(row),
            self.hits_max.get_mut(row),
            u32::try_from(row),
        ) else {
            return;
        };
        if self.own_hits.last().is_some_and(|&last| last >= index) {
            return;
        }
        *h = hits;
        *m = hits_max;
        self.own_hits.push(index);
        let Some(&handle) = self.handle.get(row) else {
            return;
        };
        let Ok(slot) = usize::try_from(handle.slot()) else {
            return;
        };
        if self.max_hits.len() <= slot {
            self.max_hits.resize(slot.saturating_add(1), None);
        }
        if let Some(entry) = self.max_hits.get_mut(slot) {
            *entry = Some((handle, hits_max));
        }
    }

    /// The `hitsMax` read before for the own creep at `row` (P7.2), if any: the snapshot reads it
    /// once in a creep's life.
    pub fn known_hits_max(&self, row: usize) -> Option<u32> {
        let &handle = self.handle.get(row)?;
        let slot = usize::try_from(handle.slot()).ok()?;
        self.max_hits
            .get(slot)
            .copied()
            .flatten()
            .filter(|&(h, _)| h == handle)
            .map(|(_, m)| m)
    }

    /// This tick's rows of own creeps whose hits were read (P7.2).
    pub fn own_hits_rows(&self) -> &[u32] {
        &self.own_hits
    }

    /// Each creep's `hits`: read for hostile creeps in owned rooms (P7.1) and own creeps in rooms
    /// with an open threat (P7.2), 0 otherwise.
    pub fn hits(&self) -> &[u32] {
        &self.hits
    }

    /// Each own creep's `hitsMax` where its `hits` is read (P7.2), 0 otherwise.
    pub fn hits_max(&self) -> &[u32] {
        &self.hits_max
    }

    /// This tick's rows of hostile creeps in owned rooms, whose hits and body are read (P7.1).
    pub fn assessed(&self) -> &[u32] {
        &self.assessed
    }

    /// The body read for the creep at `row` (P7.1), if one was read for its handle.
    pub fn body(&self, row: usize) -> Option<&BodyRead> {
        let &handle = self.handle.get(row)?;
        self.bodies
            .iter()
            .find(|(h, _)| *h == handle)
            .map(|(_, body)| body)
    }

    /// Whether a body is known for the creep at `row` (P7.1): the snapshot reads one only when not.
    pub fn knows_body(&self, row: usize) -> bool {
        self.body(row).is_some()
    }

    /// Keeps `body` for the creep at `row` while its handle lives (P7.1), in place of any body read
    /// before; nothing for a row that does not exist.
    pub fn set_body(&mut self, row: usize, body: BodyRead) {
        let Some(&handle) = self.handle.get(row) else {
            return;
        };
        match self.bodies.iter_mut().find(|(h, _)| *h == handle) {
            Some(entry) => entry.1 = body,
            None => self.bodies.push((handle, body)),
        }
    }

    /// Drops the body known for the creep at `row` (P7.1): a hostile seen in a room where it may be
    /// boosted ([`Controller::boosts_possible`]) has its body read again when it comes back into an
    /// owned room.
    pub fn forget_body(&mut self, row: usize) {
        let Some(&handle) = self.handle.get(row) else {
            return;
        };
        if let Some(index) = self.bodies.iter().position(|(h, _)| *h == handle) {
            self.bodies.swap_remove(index);
        }
    }

    /// Bodies kept (P7.1).
    pub fn bodies_known(&self) -> usize {
        self.bodies.len()
    }

    /// Sets the store of the creep at `row` (P2.4); nothing for a row that does not exist.
    pub fn set_store(&mut self, row: usize, store: CreepStore) {
        if let Some(s) = self.store.get_mut(row) {
            *s = store;
        }
    }

    /// What each creep carries (P2.4): 0 for hostile and spawning creeps.
    pub fn stores(&self) -> &[CreepStore] {
        &self.store
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

    pub fn fatigue(&self) -> &[u32] {
        &self.fatigue
    }

    /// What each creep's name says (P2.3): `None` for hostile creeps and own creeps whose name does
    /// not parse (`spawning::CreepName`).
    pub fn identities(&self) -> &[Option<Identity>] {
        &self.identity
    }

    /// The tick each creep is gone by (P5.2): `None` for hostile creeps, spawning ones, and own
    /// creeps whose `ticksToLive` has not been read as a number yet.
    pub fn deaths(&self) -> &[Option<u32>] {
        &self.death
    }

    /// The `ticksToLive` of the creep at `row` at tick `now` (its death tick less `now`; 1 on its
    /// last tick, V198), or `None` when its death tick is not known.
    pub fn ticks_to_live(&self, row: usize, now: u32) -> Option<u32> {
        self.death
            .get(row)
            .copied()
            .flatten()
            .map(|d| d.saturating_sub(now))
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

/// The bot's own spawns this tick (P2.3), one row per spawn in `Game.structures`, each with its
/// row in [`Structures`] and whether it is spawning (`StructureSpawn.spawning` not null at the
/// start of the tick, the state `spawnCreep` checks for `ERR_BUSY`, V171, V175).
#[derive(Default)]
pub struct Spawns {
    structure: Vec<StructureHandle>,
    row: Vec<u32>,
    busy: Vec<bool>,
}

impl Spawns {
    pub(super) fn begin(&mut self) {
        self.structure.clear();
        self.row.clear();
        self.busy.clear();
    }

    /// Adds a spawn: its handle, its row in this tick's [`Structures`], and whether it is busy.
    pub fn push(&mut self, structure: StructureHandle, row: u32, busy: bool) {
        self.structure.push(structure);
        self.row.push(row);
        self.busy.push(busy);
    }

    pub fn len(&self) -> usize {
        self.structure.len()
    }

    pub fn is_empty(&self) -> bool {
        self.structure.is_empty()
    }

    pub fn handles(&self) -> &[StructureHandle] {
        &self.structure
    }

    /// Each spawn's row in this tick's [`Structures`] columns.
    pub fn rows(&self) -> &[u32] {
        &self.row
    }

    pub fn busy(&self) -> &[bool] {
        &self.busy
    }
}

/// The bot's own links this tick (P3.2), one row per link in `Game.structures`: its handle, its row
/// in [`Structures`], the energy it holds (`store.getUsedCapacity(RESOURCE_ENERGY)`) and its
/// `cooldown`, both read every tick.
#[derive(Default)]
pub struct Links {
    structure: Vec<StructureHandle>,
    row: Vec<u32>,
    energy: Vec<u32>,
    cooldown: Vec<u32>,
}

impl Links {
    pub(super) fn begin(&mut self) {
        self.structure.clear();
        self.row.clear();
        self.energy.clear();
        self.cooldown.clear();
    }

    /// Adds a link: its handle, its row in this tick's [`Structures`], its energy and its cooldown.
    pub fn push(&mut self, structure: StructureHandle, row: u32, energy: u32, cooldown: u32) {
        self.structure.push(structure);
        self.row.push(row);
        self.energy.push(energy);
        self.cooldown.push(cooldown);
    }

    pub fn len(&self) -> usize {
        self.structure.len()
    }

    pub fn is_empty(&self) -> bool {
        self.structure.is_empty()
    }

    pub fn handles(&self) -> &[StructureHandle] {
        &self.structure
    }

    /// Each link's row in this tick's [`Structures`] columns.
    pub fn rows(&self) -> &[u32] {
        &self.row
    }

    /// Unit: energy.
    pub fn energy(&self) -> &[u32] {
        &self.energy
    }

    /// Unit: ticks.
    pub fn cooldowns(&self) -> &[u32] {
        &self.cooldown
    }
}

/// The bot's own towers this tick (P7.2), one row per tower in `Game.structures`: its handle, its row
/// in [`Structures`], and the energy it holds (`store.getUsedCapacity(RESOURCE_ENERGY)`), read every
/// tick. Its room for more is in [`Structures::free_energy`].
#[derive(Default)]
pub struct Towers {
    structure: Vec<StructureHandle>,
    row: Vec<u32>,
    energy: Vec<u32>,
}

impl Towers {
    pub(super) fn begin(&mut self) {
        self.structure.clear();
        self.row.clear();
        self.energy.clear();
    }

    /// Adds a tower: its handle, its row in this tick's [`Structures`], and its energy.
    pub fn push(&mut self, structure: StructureHandle, row: u32, energy: u32) {
        self.structure.push(structure);
        self.row.push(row);
        self.energy.push(energy);
    }

    pub fn len(&self) -> usize {
        self.structure.len()
    }

    pub fn is_empty(&self) -> bool {
        self.structure.is_empty()
    }

    pub fn handles(&self) -> &[StructureHandle] {
        &self.structure
    }

    /// Each tower's row in this tick's [`Structures`] columns.
    pub fn rows(&self) -> &[u32] {
        &self.row
    }

    /// Unit: energy.
    pub fn energy(&self) -> &[u32] {
        &self.energy
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
    free_energy: Vec<u32>,
    hits: Vec<u32>,
    /// The static-data cache is off (`CacheConfig::structure_static` false): read every push.
    reread_static: bool,
}

impl Structures {
    pub(super) fn begin(&mut self) {
        self.ids.begin_pass();
        self.handle.clear();
        self.kind.clear();
        self.pos.clear();
        self.free_energy.clear();
        self.hits.clear();
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
        self.free_energy.push(0);
        self.hits.push(0);
        Ok(handle)
    }

    /// Sets the hits read for the structure at `row` (P7.3: an own rampart; P7.4: a core
    /// structure); nothing for a row that does not exist.
    pub fn set_hits(&mut self, row: usize, hits: u32) {
        if let Some(h) = self.hits.get_mut(row) {
            *h = hits;
        }
    }

    /// Hits per structure, read only for own ramparts (P7.3: on a room's upkeep read, when first
    /// seen, and for the one a worker is about to repair) and for spawns, towers, storage, and
    /// terminals on a tick a hostile creep stands in their room (P7.4, the safe-mode triggers); 0
    /// when not read this tick (a standing structure always has at least 1).
    pub fn hits(&self) -> &[u32] {
        &self.hits
    }

    /// Sets the room for energy of the structure at `row` (P2.4); nothing for a row that does not exist.
    pub fn set_free_energy(&mut self, row: usize, free: u32) {
        if let Some(f) = self.free_energy.get_mut(row) {
            *f = free;
        }
    }

    /// Room for energy per structure (P2.4), as `store.getFreeCapacity(RESOURCE_ENERGY)` gives it,
    /// read for spawns and extensions in a room that is not full; 0 (full, or not read) otherwise.
    pub fn free_energy(&self) -> &[u32] {
        &self.free_energy
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

/// What a [`RoomObjects`] row is (P2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    /// A source: `amount` is its `energy`, `total` its `energyCapacity`.
    Source,
    /// An own construction site of this structure type: `amount` is its `progress`, `total` its
    /// `progressTotal`.
    Site(StructureType),
    /// A dropped pile of energy: `amount` is its `amount`.
    Energy,
    /// A dropped pile of another resource: `amount` is its `amount` (nothing uses it yet).
    OtherResource,
    /// A tombstone: `amount` is the energy in its store.
    Tombstone,
    /// A ruin: `amount` is the energy in its store.
    Ruin,
    /// A container (it has no owner, so `Game.structures` leaves it out): `amount` is the energy in
    /// its store, `hits` its hits (P3.5; its `hitsMax` is `CONTAINER_HITS`, V223).
    Container,
    /// A remote room's controller (P6.2; an owned room's is in `Structures`): `amount` is its
    /// reservation's `ticksToEnd`, 0 with none.
    Controller,
    /// A road (it has no owner): in a remote room (P6.2) read every `remote.road_read_ticks`, in an
    /// owned room (P7.3) every `upkeep.read_ticks` and when a worker is about to repair it: `hits`
    /// its hits and `total` its `hitsMax` (`ROAD_HITS` times the terrain's ratio, V186).
    Road,
    /// A constructed wall in an owned room (it has no owner, V314, V315; ADR-033): read every
    /// `upkeep.read_ticks` and when a worker is about to repair it: `hits` its hits and `total` its
    /// `hitsMax` (`WALL_HITS_MAX` from RCL2, V310).
    Wall,
}

impl ObjectKind {
    /// Whether the energy in it can be taken: `pickup` for a pile, `withdraw` for the rest.
    pub const fn holds_energy(self) -> bool {
        matches!(
            self,
            Self::Energy | Self::Tombstone | Self::Ruin | Self::Container
        )
    }
}

/// What never changes for a room object: read from JS once, when the object is first seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectStatic {
    pub kind: ObjectKind,
    pub pos: Position,
}

/// What changes for a room object, read every tick (see [`ObjectKind`] for what each holds).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjectRow {
    pub amount: u32,
    pub total: u32,
    /// A container's or a road's hits (P3.5, P6.2); 0 for every other kind.
    pub hits: u32,
}

/// Room objects the opening works with (P2.4), read in the bot's owned rooms: sources, own
/// construction sites, dropped resources, tombstones, ruins and containers; in remote rooms (P6.2)
/// also the controller and, every `remote.road_read_ticks`, the roads.
#[derive(Default)]
pub struct RoomObjects {
    ids: Interner<ObjectClass, ObjectStatic>,
    handle: Vec<ObjectHandle>,
    kind: Vec<ObjectKind>,
    pos: Vec<Position>,
    amount: Vec<u32>,
    total: Vec<u32>,
    hits: Vec<u32>,
}

impl RoomObjects {
    pub(super) fn begin(&mut self) {
        self.ids.begin_pass();
        self.handle.clear();
        self.kind.clear();
        self.pos.clear();
        self.amount.clear();
        self.total.clear();
        self.hits.clear();
    }

    pub(super) fn finish(&mut self) -> u32 {
        self.ids.end_pass()
    }

    /// Adds an object and returns its handle. `read_static` runs only when the id is new; on any
    /// error nothing is added, so an object whose static read fails is read again next tick.
    pub fn push(
        &mut self,
        key: ObjectKey,
        read_static: impl FnOnce() -> Result<ObjectStatic, SkipReason>,
        row: ObjectRow,
    ) -> Result<ObjectHandle, SkipReason> {
        let index = u32::try_from(self.handle.len()).map_err(|_| SkipReason::SlotsExhausted)?;
        let (handle, &data) = self.ids.mark(key, index, read_static)?;
        self.handle.push(handle);
        self.kind.push(data.kind);
        self.pos.push(data.pos);
        self.amount.push(row.amount);
        self.total.push(row.total);
        self.hits.push(row.hits);
        Ok(handle)
    }

    pub fn len(&self) -> usize {
        self.handle.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handle.is_empty()
    }

    /// The handle's row in this tick's columns, if the object is in this tick's snapshot.
    pub fn row_of(&self, handle: ObjectHandle) -> Option<usize> {
        usize::try_from(self.ids.row(handle)?).ok()
    }

    /// The live handle of an object id, if the object was in the last snapshot.
    pub fn handle_of(&self, key: ObjectKey) -> Option<ObjectHandle> {
        self.ids.get(key)
    }

    /// The id of a live object handle, for a stable order (rows follow the engine's order, V102).
    pub fn key_of(&self, handle: ObjectHandle) -> Option<ObjectKey> {
        self.ids.key(handle)
    }

    pub fn handles(&self) -> &[ObjectHandle] {
        &self.handle
    }

    pub fn kinds(&self) -> &[ObjectKind] {
        &self.kind
    }

    pub fn positions(&self) -> &[Position] {
        &self.pos
    }

    pub fn amounts(&self) -> &[u32] {
        &self.amount
    }

    pub fn totals(&self) -> &[u32] {
        &self.total
    }

    /// A container's hits (P3.5), 0 for every other kind ([`ObjectRow::hits`]).
    pub fn hits(&self) -> &[u32] {
        &self.hits
    }

    /// Objects first seen this tick.
    pub fn added(&self) -> u32 {
        self.ids.added()
    }

    /// Handles released at the end of this tick's pass (objects that were gone).
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
    use rustedscreep_consts::{CREEP_LIFE_TIME, RoomCoordinate};

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
            fatigue: 4,
        };
        let a = creeps.push(ObjectKey(1), row).expect("new");
        let hostile = CreepRow {
            pos: pos(20, 20, "W1N1"),
            my: false,
            spawning: false,
            fatigue: 0,
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
        assert_eq!(creeps.fatigue(), [4, 0]);
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

    /// An own creep's name is read once per handle (P2.3): again only after the handle is
    /// released; a creep pushed without a name has no identity, and a failed push reads nothing.
    #[test]
    fn creep_identities_are_read_once_per_handle() {
        use crate::spawning::{Identity, Role};
        let id = Identity {
            role: Role::Worker,
            remote: None,
            home: room("W1N1"),
            serial: 7,
        };
        let row = CreepRow {
            pos: pos(10, 10, "W1N1"),
            my: true,
            spawning: true,
            fatigue: 0,
        };
        let mut creeps = Creeps::default();
        let mut reads = 0;
        for _ in 0..3 {
            creeps.begin();
            creeps
                .push_named(ObjectKey(1), row, || {
                    reads += 1;
                    Some(id)
                })
                .expect("own");
            creeps
                .push(ObjectKey(2), CreepRow { my: false, ..row })
                .expect("hostile");
            let dup = creeps.push_named(ObjectKey(1), row, || {
                reads += 1;
                None
            });
            assert_eq!(dup, Err(SkipReason::Duplicate));
            creeps.finish();
            assert_eq!(creeps.identities(), [Some(id), None]);
        }
        assert_eq!(reads, 1, "read once while the handle lives");
        creeps.begin();
        creeps.finish();
        creeps.begin();
        creeps
            .push_named(ObjectKey(1), row, || {
                reads += 1;
                None
            })
            .expect("again");
        creeps.finish();
        assert_eq!(reads, 2, "read again for a new handle");
        assert_eq!(creeps.identities(), [None]);
    }

    /// Uses: CREEP_LIFE_TIME. A death tick set once (P5.2) stays with its creep's handle on later
    /// ticks, whatever its row, and `ticks_to_live` counts down by one a tick to 1 on its last tick
    /// (V198); a creep that reuses the slot after the handle is released starts unknown, as does a
    /// hostile creep, and a row that does not exist is left alone.
    #[test]
    fn creep_deaths_are_kept_per_handle() {
        let row = CreepRow {
            pos: pos(10, 10, "W1N1"),
            my: true,
            spawning: false,
            fatigue: 0,
        };
        let mut creeps = Creeps::default();
        let start = 1000;
        let death = start + CREEP_LIFE_TIME;
        creeps.begin();
        creeps.push_named(ObjectKey(1), row, || None).expect("own");
        creeps
            .push(ObjectKey(2), CreepRow { my: false, ..row })
            .expect("hostile");
        assert_eq!(creeps.deaths(), [None, None]);
        creeps.set_death(0, death);
        creeps.set_death(5, death);
        creeps.finish();
        assert_eq!(creeps.ticks_to_live(0, start), Some(CREEP_LIFE_TIME));
        let own = creeps.handle_of(ObjectKey(1)).expect("own handle");
        // The rows change order on the next tick (the hostile first): the death tick follows the
        // handle, not the row.
        for (now, hostile_first) in [(start + 1, true), (death - 1, false)] {
            creeps.begin();
            if hostile_first {
                creeps
                    .push(ObjectKey(2), CreepRow { my: false, ..row })
                    .expect("hostile");
            }
            creeps.push_named(ObjectKey(1), row, || None).expect("own");
            if !hostile_first {
                creeps
                    .push(ObjectKey(2), CreepRow { my: false, ..row })
                    .expect("hostile");
            }
            creeps.finish();
            let (mine, theirs) = if hostile_first { (1, 0) } else { (0, 1) };
            assert_eq!(creeps.deaths()[mine], Some(death));
            assert_eq!(creeps.deaths()[theirs], None);
            assert_eq!(creeps.ticks_to_live(mine, now), Some(death - now));
            assert_eq!(creeps.ticks_to_live(theirs, now), None);
        }
        assert_eq!(creeps.ticks_to_live(0, death - 1), Some(1), "its last tick");
        // Gone for a tick: the handle is released, and the next creep in its slot is unknown.
        creeps.begin();
        creeps.finish();
        creeps.begin();
        creeps.push_named(ObjectKey(3), row, || None).expect("new");
        creeps.finish();
        let new = creeps.handle_of(ObjectKey(3)).expect("new handle");
        assert_eq!(new.slot(), own.slot(), "the slot is reused");
        assert_ne!(new, own);
        assert_eq!(creeps.deaths(), [None]);
    }

    #[test]
    fn room_energy_is_zero_until_set_and_resets_each_tick() {
        let mut rooms = Rooms::default();
        rooms.begin();
        let a = rooms.push(room("W1N1"), Controller::Mine { level: 2 });
        let b = rooms.push(room("W2N1"), Controller::Neutral);
        let e = RoomEnergy {
            available: 250,
            capacity: 550,
        };
        rooms.set_energy(a, e);
        rooms.set_energy(9, e);
        assert_eq!(rooms.energy(), [e, RoomEnergy::default()]);
        assert_eq!(rooms.energy().get(b), Some(&RoomEnergy::default()));
        rooms.begin();
        assert!(rooms.energy().is_empty());
    }

    #[test]
    fn spawn_rows_stay_aligned_and_reset_each_tick() {
        let mut structures = Structures::default();
        structures.begin();
        let st = StructureStatic {
            kind: StructureType::Spawn,
            pos: pos(25, 25, "W1N1"),
        };
        let h = structures.push(ObjectKey(1), || Ok(st)).expect("spawn");
        let mut spawns = Spawns::default();
        spawns.begin();
        spawns.push(h, 0, true);
        assert_eq!(
            (spawns.len(), spawns.handles(), spawns.rows(), spawns.busy()),
            (1, &[h][..], &[0][..], &[true][..])
        );
        spawns.begin();
        assert!(spawns.is_empty());
    }
}
