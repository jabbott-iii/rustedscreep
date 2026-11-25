//! The atlas (ADR-039): what the bot last saw of each room near its homes, kept across global
//! resets so claim scoring (P8.2) and the explorer know the rooms without seeing them again.
//!
//! One frame (`persist::frame`, owner [`crate::persist::kind::ATLAS`], schema [`SCHEMA`]) in the
//! segment `PersistConfig::atlas_segment`, written when an entry changes, at most once per
//! `scout.save_ticks`. The payload, little-endian:
//!
//! ```text
//! payload    = count:u16 entry*count            (entries in room order, no room twice)
//! entry      = room:u16 seen:u32 flags:u8 status:u8 owner:u8 level:u8 controller:u16
//!              reserve:u8 reserve_end:u32 safe_end:u32 n:u8 source:u16*n
//!              mineral:u8 mineral_at:u16 density:u8 towers:u8 spawns:u8 lairs:u8 core:u8
//!              hostiles:u8
//! room       = RoomName::packed_repr (V255)     controller, source, mineral_at = Tile::index,
//! flags      = bit 0 known, bit 1 unreachable                or NO_TILE for none
//! status     = RoomStatus's code                owner = Controller's code, level its level
//! reserve    = Reserved's code                  reserve_end, safe_end = a game tick, 0 for none
//! mineral    = Mineral's code, 0 for none       n <= MAX_SOURCES; core = a level, NO_CORE for none
//! ```
//!
//! An entry with two sources takes 35 bytes ([`entry_len`]). Of the 441 rooms within 10 of one
//! home, about 318 can have a controller, about 11 KB; all 441 with the most sources an entry
//! keeps would still take under a quarter of one segment's payload ([`crate::persist::MAX_PAYLOAD`]). Anything
//! else (a short or long payload, an unknown code, a tile off the room, too many sources, rooms
//! out of order) refuses the whole record: the atlas then starts as with none.

use core::cmp::Reverse;

use rustedscreep_consts::{ResourceType, RoomName};

use crate::movement::Tile;
use crate::snapshot::{Controller, INTEL_SOURCES, IntelRead, Reservation, RoomStatus};

/// The payload layout's version (the frame's `schema`).
pub const SCHEMA: u16 = 1;

/// The most source tiles an entry keeps: [`INTEL_SOURCES`].
pub const MAX_SOURCES: usize = INTEL_SOURCES;

/// A tile's code when the tile is not known.
const NO_TILE: u16 = u16::MAX;

/// The core byte when no invader core stands in the room.
const NO_CORE: u8 = u8::MAX;

/// The flags: the entry holds a read.
const KNOWN: u8 = 1;
/// The flags: the explorer found no route there at `seen`.
const UNREACHABLE: u8 = 2;

/// Who reserves a room's controller, and until which tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reserved {
    #[default]
    None,
    /// The bot's own reservation, ending at this tick. Unit: game tick.
    Mine { until: u32 },
    /// Another user's (a player's or an invader core's), ending at this tick. Unit: game tick.
    Other { until: u32 },
}

impl Reserved {
    const fn code(self) -> (u8, u32) {
        match self {
            Self::None => (0, 0),
            Self::Mine { until } => (1, until),
            Self::Other { until } => (2, until),
        }
    }

    const fn from_code(code: u8, until: u32) -> Option<Self> {
        match code {
            0 => Some(Self::None),
            1 => Some(Self::Mine { until }),
            2 => Some(Self::Other { until }),
            _ => None,
        }
    }
}

/// A room's mineral type: the seven a room can hold (V364), and any other the server may add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mineral {
    Hydrogen,
    Oxygen,
    Utrium,
    Lemergium,
    Keanium,
    Zynthium,
    Catalyst,
    /// A type this build does not list (a Seasonal server's, for example).
    Other,
}

impl Mineral {
    /// Every type, in code order.
    pub const ALL: [Self; 8] = [
        Self::Hydrogen,
        Self::Oxygen,
        Self::Utrium,
        Self::Lemergium,
        Self::Keanium,
        Self::Zynthium,
        Self::Catalyst,
        Self::Other,
    ];

    /// The type's byte in the record (0 is no mineral). Never change or reuse one.
    pub const fn code(self) -> u8 {
        match self {
            Self::Hydrogen => 1,
            Self::Oxygen => 2,
            Self::Utrium => 3,
            Self::Lemergium => 4,
            Self::Keanium => 5,
            Self::Zynthium => 6,
            Self::Catalyst => 7,
            Self::Other => 8,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.code() == code)
    }

    /// The type of a mineral the game gives as `resource`.
    pub fn from_resource(resource: ResourceType) -> Self {
        match resource {
            ResourceType::Hydrogen => Self::Hydrogen,
            ResourceType::Oxygen => Self::Oxygen,
            ResourceType::Utrium => Self::Utrium,
            ResourceType::Lemergium => Self::Lemergium,
            ResourceType::Keanium => Self::Keanium,
            ResourceType::Zynthium => Self::Zynthium,
            ResourceType::Catalyst => Self::Catalyst,
            _ => Self::Other,
        }
    }

    /// The type as a resource, `None` for [`Mineral::Other`].
    pub const fn resource(self) -> Option<ResourceType> {
        match self {
            Self::Hydrogen => Some(ResourceType::Hydrogen),
            Self::Oxygen => Some(ResourceType::Oxygen),
            Self::Utrium => Some(ResourceType::Utrium),
            Self::Lemergium => Some(ResourceType::Lemergium),
            Self::Keanium => Some(ResourceType::Keanium),
            Self::Zynthium => Some(ResourceType::Zynthium),
            Self::Catalyst => Some(ResourceType::Catalyst),
            Self::Other => None,
        }
    }

    /// The type's symbol, as the game names it (`other` for [`Mineral::Other`]).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hydrogen => "H",
            Self::Oxygen => "O",
            Self::Utrium => "U",
            Self::Lemergium => "L",
            Self::Keanium => "K",
            Self::Zynthium => "Z",
            Self::Catalyst => "X",
            Self::Other => "other",
        }
    }
}

/// A room's mineral: its type, tile, and `density` (1 to 4, V364).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deposit {
    pub mineral: Mineral,
    pub tile: Tile,
    pub density: u8,
}

/// What the bot last knew of one room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomIntel {
    pub room: RoomName,
    /// The tick of the last read, or of the explorer's last failed route after it. Unit: game tick.
    pub seen: u32,
    /// The fields below hold a read (from `seen`, or from before the failed route).
    pub known: bool,
    /// The explorer found no route there at `seen`.
    pub unreachable: bool,
    pub status: RoomStatus,
    pub controller: Controller,
    pub controller_tile: Option<Tile>,
    pub reserved: Reserved,
    /// The tick another owner's safe mode ends; 0 for none. Unit: game tick.
    pub safe_until: u32,
    /// The sources' tiles, the first [`MAX_SOURCES`].
    pub sources: [Option<Tile>; MAX_SOURCES],
    pub mineral: Option<Deposit>,
    /// Structures of other users seen there, by type. Unit: structures.
    pub towers: u8,
    pub spawns: u8,
    /// Source keepers' lairs.
    pub lairs: u8,
    /// An invader core's level, when one stood there.
    pub core: Option<u8>,
    /// Hostile creeps seen there. Unit: creeps.
    pub hostiles: u8,
}

impl RoomIntel {
    /// A room the bot knows nothing of yet, as of `now`.
    pub const fn new(room: RoomName, now: u32) -> Self {
        Self {
            room,
            seen: now,
            known: false,
            unreachable: false,
            status: RoomStatus::Normal,
            controller: Controller::Absent,
            controller_tile: None,
            reserved: Reserved::None,
            safe_until: 0,
            sources: [None; MAX_SOURCES],
            mineral: None,
            towers: 0,
            spawns: 0,
            lairs: 0,
            core: None,
            hostiles: 0,
        }
    }

    /// The record of `read`, a read of `room` (whose controller is `controller`) at `now`.
    pub fn from_read(room: RoomName, controller: Controller, read: &IntelRead, now: u32) -> Self {
        let reserved = match read.reservation {
            Reservation::None => Reserved::None,
            Reservation::Mine { ticks } => Reserved::Mine {
                until: now.saturating_add(ticks),
            },
            Reservation::Other { ticks } => Reserved::Other {
                until: now.saturating_add(ticks),
            },
        };
        let mut sources = [None; MAX_SOURCES];
        for (slot, p) in sources.iter_mut().zip(read.source_positions()) {
            *slot = Some(Tile::of(p));
        }
        let safe_until = if read.safe_mode > 0 {
            now.saturating_add(read.safe_mode)
        } else {
            0
        };
        Self {
            room,
            seen: now,
            known: true,
            unreachable: false,
            status: read.status,
            controller,
            controller_tile: read.controller_at.map(Tile::of),
            reserved,
            safe_until,
            sources,
            mineral: read.mineral.map(|m| Deposit {
                mineral: m.kind.map_or(Mineral::Other, Mineral::from_resource),
                tile: Tile::of(m.at),
                density: m.density,
            }),
            towers: read.towers,
            spawns: read.spawns,
            lairs: read.lairs,
            core: read.core,
            hostiles: read.hostiles,
        }
    }

    /// The sources' tiles.
    pub fn source_tiles(&self) -> impl Iterator<Item = Tile> + '_ {
        self.sources.iter().flatten().copied()
    }

    /// Unit: sources.
    pub fn source_count(&self) -> usize {
        self.sources.iter().flatten().count()
    }

    /// Whether its record is due again at `now`: read (or marked) `refresh` ticks ago or more.
    pub const fn due(&self, now: u32, refresh: u32) -> bool {
        now >= self.seen.saturating_add(refresh)
    }

    /// Whether another owner's towers were seen there: the explorer stays out (one shot kills a
    /// one-MOVE creep, V285).
    pub const fn towered(&self) -> bool {
        self.known && self.towers > 0 && matches!(self.controller, Controller::Foreign { .. })
    }

    /// The same facts as `other`, the read's tick aside: whether a read changed anything worth a
    /// log line (the owner, the reservation's holder, the towers, the room's status).
    pub fn same_holder(&self, other: &Self) -> bool {
        let holder = |r: Reserved| match r {
            Reserved::None => 0,
            Reserved::Mine { .. } => 1,
            Reserved::Other { .. } => 2,
        };
        self.known == other.known
            && self.status == other.status
            && self.controller == other.controller
            && holder(self.reserved) == holder(other.reserved)
            && self.towers == other.towers
            && self.core == other.core
    }
}

/// Why a payload was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    /// The payload ends inside an entry, or before its count.
    Short,
    /// Bytes are left after the last entry.
    Long,
    /// A code this build does not know: the field's name and the code.
    Code(&'static str, u8),
    /// A tile index at or past `ROOM_AREA`.
    Tile(u16),
    /// More sources than [`MAX_SOURCES`].
    Sources(u8),
    /// A room not after the one before it.
    Order,
}

/// Every room's entry, in room order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Atlas {
    entries: Vec<RoomIntel>,
}

impl Atlas {
    pub fn entries(&self) -> &[RoomIntel] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn find(&self, room: RoomName) -> Result<usize, usize> {
        self.entries
            .binary_search_by_key(&room.packed_repr(), |e| e.room.packed_repr())
    }

    pub fn get(&self, room: RoomName) -> Option<&RoomIntel> {
        self.find(room).ok().and_then(|i| self.entries.get(i))
    }

    /// Whether `room` is due at `now`: no entry, or one [`RoomIntel::due`] says is.
    pub fn due(&self, room: RoomName, now: u32, refresh: u32) -> bool {
        self.get(room).is_none_or(|e| e.due(now, refresh))
    }

    /// Puts `intel` in place of the room's entry. Returns the entry it replaced.
    pub fn put(&mut self, intel: RoomIntel) -> Option<RoomIntel> {
        match self.find(intel.room) {
            Ok(i) => self
                .entries
                .get_mut(i)
                .map(|e| core::mem::replace(e, intel)),
            Err(i) => {
                self.entries.insert(i, intel);
                None
            }
        }
    }

    /// Marks `room` unreachable at `now`: its read, if any, is kept, and it is due again
    /// `scout.refresh_ticks` later.
    pub fn mark_unreachable(&mut self, room: RoomName, now: u32) {
        let i = match self.find(room) {
            Ok(i) => i,
            Err(i) => {
                self.entries.insert(i, RoomIntel::new(room, now));
                i
            }
        };
        if let Some(e) = self.entries.get_mut(i) {
            e.seen = now;
            e.unreachable = true;
        }
    }

    /// Keeps only the entries `keep` says to. Returns how many it dropped.
    pub fn retain(&mut self, keep: impl Fn(&RoomIntel) -> bool) -> usize {
        let before = self.entries.len();
        self.entries.retain(keep);
        before.saturating_sub(self.entries.len())
    }

    /// Appends the payload to `out`, at most `max_len` bytes of it: when the whole atlas does not
    /// fit, the entries seen longest ago are left out (the rest stay in room order). Returns how
    /// many it left out.
    pub fn encode(&self, out: &mut Vec<u8>, max_len: usize) -> usize {
        let total: usize = 2 + self
            .entries
            .iter()
            .map(|e| entry_len(e.source_count()))
            .sum::<usize>();
        let cap = usize::from(u16::MAX);
        let mut keep = vec![true; self.entries.len()];
        let mut left_out = 0;
        if total > max_len || self.entries.len() > cap {
            // The newest first, each kept while it still fits.
            let mut order: Vec<usize> = (0..self.entries.len()).collect();
            order.sort_by_key(|&i| {
                self.entries
                    .get(i)
                    .map_or((Reverse(0), 0), |e| (Reverse(e.seen), e.room.packed_repr()))
            });
            let mut used = 2usize;
            let mut kept = 0usize;
            for i in order {
                let len = self
                    .entries
                    .get(i)
                    .map_or(usize::MAX, |e| entry_len(e.source_count()));
                let fits = used.saturating_add(len) <= max_len && kept < cap;
                if fits {
                    used = used.saturating_add(len);
                    kept += 1;
                } else if let Some(k) = keep.get_mut(i) {
                    *k = false;
                    left_out += 1;
                }
            }
        }
        let count = keep.iter().filter(|&&k| k).count();
        let count = u16::try_from(count).unwrap_or(u16::MAX);
        out.extend_from_slice(&count.to_le_bytes());
        for (e, _) in self.entries.iter().zip(&keep).filter(|&(_, &k)| k) {
            write_entry(out, e);
        }
        left_out
    }

    /// Parses a payload [`Self::encode`] wrote.
    pub fn decode(payload: &[u8]) -> Result<Self, RecordError> {
        let mut r = Reader(payload);
        let count = r.u16()?;
        // A count the payload cannot hold reserves no more than it could.
        let fits = payload.len() / entry_len(0);
        let mut entries: Vec<RoomIntel> = Vec::with_capacity(usize::from(count).min(fits));
        for _ in 0..count {
            let e = read_entry(&mut r)?;
            if entries
                .last()
                .is_some_and(|p| p.room.packed_repr() >= e.room.packed_repr())
            {
                return Err(RecordError::Order);
            }
            entries.push(e);
        }
        if !r.0.is_empty() {
            return Err(RecordError::Long);
        }
        Ok(Self { entries })
    }
}

/// An entry's length with `sources` source tiles. Unit: bytes.
pub const fn entry_len(sources: usize) -> usize {
    31 + 2 * sources
}

const fn status_code(s: RoomStatus) -> u8 {
    match s {
        RoomStatus::Normal => 0,
        RoomStatus::Closed => 1,
        RoomStatus::Novice => 2,
        RoomStatus::Respawn => 3,
    }
}

const fn status_from(code: u8) -> Option<RoomStatus> {
    match code {
        0 => Some(RoomStatus::Normal),
        1 => Some(RoomStatus::Closed),
        2 => Some(RoomStatus::Novice),
        3 => Some(RoomStatus::Respawn),
        _ => None,
    }
}

const fn owner_code(c: Controller) -> (u8, u8) {
    match c {
        Controller::Absent => (0, 0),
        Controller::Neutral => (1, 0),
        Controller::Mine { level } => (2, level),
        Controller::Foreign { level } => (3, level),
    }
}

const fn owner_from(code: u8, level: u8) -> Option<Controller> {
    match code {
        0 => Some(Controller::Absent),
        1 => Some(Controller::Neutral),
        2 => Some(Controller::Mine { level }),
        3 => Some(Controller::Foreign { level }),
        _ => None,
    }
}

fn write_entry(out: &mut Vec<u8>, e: &RoomIntel) {
    out.extend_from_slice(&e.room.packed_repr().to_le_bytes());
    out.extend_from_slice(&e.seen.to_le_bytes());
    let flags = if e.known { KNOWN } else { 0 } | if e.unreachable { UNREACHABLE } else { 0 };
    out.push(flags);
    out.push(status_code(e.status));
    let (owner, level) = owner_code(e.controller);
    out.push(owner);
    out.push(level);
    out.extend_from_slice(&e.controller_tile.map_or(NO_TILE, tile_code).to_le_bytes());
    let (reserve, until) = e.reserved.code();
    out.push(reserve);
    out.extend_from_slice(&until.to_le_bytes());
    out.extend_from_slice(&e.safe_until.to_le_bytes());
    let n = e.source_count();
    out.push(u8::try_from(n).unwrap_or(0));
    for t in e.source_tiles() {
        out.extend_from_slice(&tile_code(t).to_le_bytes());
    }
    match e.mineral {
        Some(d) => {
            out.push(d.mineral.code());
            out.extend_from_slice(&tile_code(d.tile).to_le_bytes());
            out.push(d.density);
        }
        None => {
            out.push(0);
            out.extend_from_slice(&NO_TILE.to_le_bytes());
            out.push(0);
        }
    }
    out.push(e.towers);
    out.push(e.spawns);
    out.push(e.lairs);
    out.push(e.core.unwrap_or(NO_CORE));
    out.push(e.hostiles);
}

fn read_entry(r: &mut Reader<'_>) -> Result<RoomIntel, RecordError> {
    let room = RoomName::from_packed(r.u16()?);
    let seen = r.u32()?;
    let flags = r.u8()?;
    if flags & !(KNOWN | UNREACHABLE) != 0 {
        return Err(RecordError::Code("flags", flags));
    }
    let code = r.u8()?;
    let status = status_from(code).ok_or(RecordError::Code("status", code))?;
    let (owner, level) = (r.u8()?, r.u8()?);
    let controller = owner_from(owner, level).ok_or(RecordError::Code("owner", owner))?;
    let controller_tile = maybe_tile(r.u16()?)?;
    let (reserve, until) = (r.u8()?, r.u32()?);
    let reserved =
        Reserved::from_code(reserve, until).ok_or(RecordError::Code("reserve", reserve))?;
    let safe_until = r.u32()?;
    let n = r.u8()?;
    if usize::from(n) > MAX_SOURCES {
        return Err(RecordError::Sources(n));
    }
    let mut sources = [None; MAX_SOURCES];
    for slot in sources.iter_mut().take(usize::from(n)) {
        *slot = Some(tile_from(r.u16()?)?);
    }
    let (kind, at, density) = (r.u8()?, r.u16()?, r.u8()?);
    let mineral = match kind {
        0 => None,
        code => Some(Deposit {
            mineral: Mineral::from_code(code).ok_or(RecordError::Code("mineral", code))?,
            tile: tile_from(at)?,
            density,
        }),
    };
    let towers = r.u8()?;
    let spawns = r.u8()?;
    let lairs = r.u8()?;
    let core = match r.u8()? {
        NO_CORE => None,
        level => Some(level),
    };
    let hostiles = r.u8()?;
    Ok(RoomIntel {
        room,
        seen,
        known: flags & KNOWN != 0,
        unreachable: flags & UNREACHABLE != 0,
        status,
        controller,
        controller_tile,
        reserved,
        safe_until,
        sources,
        mineral,
        towers,
        spawns,
        lairs,
        core,
        hostiles,
    })
}

fn tile_code(t: Tile) -> u16 {
    u16::try_from(t.index()).unwrap_or(NO_TILE)
}

fn tile_from(code: u16) -> Result<Tile, RecordError> {
    Tile::from_index(usize::from(code)).ok_or(RecordError::Tile(code))
}

fn maybe_tile(code: u16) -> Result<Option<Tile>, RecordError> {
    match code {
        NO_TILE => Ok(None),
        code => tile_from(code).map(Some),
    }
}

/// Reads the payload's little-endian fields front to back.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], RecordError> {
        let (head, rest) = self.0.split_at_checked(N).ok_or(RecordError::Short)?;
        self.0 = rest;
        head.try_into().map_err(|_| RecordError::Short)
    }

    fn u8(&mut self) -> Result<u8, RecordError> {
        Ok(u8::from_le_bytes(self.take::<1>()?))
    }

    fn u16(&mut self) -> Result<u16, RecordError> {
        Ok(u16::from_le_bytes(self.take::<2>()?))
    }

    fn u32(&mut self) -> Result<u32, RecordError> {
        Ok(u32::from_le_bytes(self.take::<4>()?))
    }
}
