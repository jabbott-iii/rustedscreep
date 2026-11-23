//! The signer's record (ADR-030): what the bot knows of each room's controller sign, kept across
//! global resets so a reset does not send a signer back to rooms it already signed.
//!
//! One frame (`persist::frame`, owner [`crate::persist::kind::SIGN`], schema [`SCHEMA`]) in the
//! segment `PersistConfig::sign_segment`, written when an entry changes, at most once per
//! `sign.save_ticks`. The payload, little-endian:
//!
//! ```text
//! payload    = count:u16 entry*count        (entries in room order, no room twice)
//! entry      = room:u16 state:u8 seen:u32 controller:u16 deaths:u8 overwrites:u8 n:u8 tower:u16*n
//! room       = RoomName::packed_repr (V255) controller = Tile::index, or NO_TILE for none
//! state      = SignState's code             tower = Tile::index, n <= MAX_TOWERS
//! ```
//!
//! Anything else (a short or long payload, an unknown state, a tile off the room, too many towers,
//! rooms out of order) refuses the whole record: the signer then starts as with none.

use rustedscreep_consts::RoomName;

use crate::config::SignConfig;
use crate::movement::Tile;
use crate::snapshot::SIGN_TOWERS;

/// The payload layout's version (the frame's `schema`).
pub const SCHEMA: u16 = 1;

/// The most towers an entry keeps: [`SIGN_TOWERS`], the most a room can have at level 8.
pub const MAX_TOWERS: usize = SIGN_TOWERS;

/// The controller tile's code when the tile is not known.
const NO_TILE: u16 = u16::MAX;

/// What the bot last knew of a room's controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignState {
    /// Seen with the text ([`super::SIGN_TEXT`]).
    Signed,
    /// Seen without it: no sign, or another text.
    Unsigned,
    /// Seen with the server's sign (V275): nothing a player writes shows while it lasts.
    System,
    /// No route there within one creep life, or its controller walled off from every entrance.
    Unreachable,
    /// No body within `MAX_CREEP_SIZE` and the home's energy capacity survives its towers.
    Deadly,
    /// Seen with no controller.
    NoController,
}

impl SignState {
    /// Every state, in code order.
    pub const ALL: [Self; 6] = [
        Self::Signed,
        Self::Unsigned,
        Self::System,
        Self::Unreachable,
        Self::Deadly,
        Self::NoController,
    ];

    /// The state's byte in the record. Never change or reuse one.
    pub const fn code(self) -> u8 {
        match self {
            Self::Signed => 0,
            Self::Unsigned => 1,
            Self::System => 2,
            Self::Unreachable => 3,
            Self::Deadly => 4,
            Self::NoController => 5,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.code() == code)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Signed => "signed",
            Self::Unsigned => "unsigned",
            Self::System => "server sign",
            Self::Unreachable => "unreachable",
            Self::Deadly => "deadly",
            Self::NoController => "no controller",
        }
    }
}

/// One room's entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub room: RoomName,
    pub state: SignState,
    /// The tick of the last look, or of the decision that set the state. Unit: game tick.
    pub seen: u32,
    /// The controller's tile, once a creep has seen the room.
    pub controller: Option<Tile>,
    /// Towers of another owner seen there (at most [`MAX_TOWERS`]: the record keeps no more), each
    /// counted as firing.
    pub towers: Vec<Tile>,
    /// Signers lost there since the last sign. Unit: creeps.
    pub deaths: u8,
    /// Times the text was found replaced there. Unit: signs.
    pub overwrites: u8,
}

impl Entry {
    /// A room the bot learns of now, with nothing known of it yet.
    pub fn new(room: RoomName, now: u32) -> Self {
        Self {
            room,
            state: SignState::Unsigned,
            seen: now,
            controller: None,
            towers: Vec::new(),
            deaths: 0,
            overwrites: 0,
        }
    }

    /// Whether a signer gave up on it for `retry_ticks`: too many signers lost or signs replaced.
    pub fn given_up(&self, cfg: &SignConfig) -> bool {
        u32::from(self.deaths) >= cfg.max_deaths || u32::from(self.overwrites) >= cfg.max_overwrites
    }

    /// The tick from which a signer should go there.
    pub fn due_at(&self, cfg: &SignConfig) -> u32 {
        match self.state {
            SignState::Signed => self.seen.saturating_add(cfg.recheck_ticks),
            SignState::Unsigned if !self.given_up(cfg) => 0,
            SignState::Unsigned
            | SignState::System
            | SignState::Unreachable
            | SignState::Deadly => self.seen.saturating_add(cfg.retry_ticks),
            SignState::NoController => u32::MAX,
        }
    }
}

/// Why a payload was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    /// The payload ends inside an entry, or before its count.
    Short,
    /// Bytes are left after the last entry.
    Long,
    /// A state code [`SignState::from_code`] does not know.
    State(u8),
    /// A tile index at or past `ROOM_AREA`.
    Tile(u16),
    /// More towers than [`MAX_TOWERS`].
    Towers(u8),
    /// A room not after the one before it.
    Order,
}

/// Every room's entry, in room order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    entries: Vec<Entry>,
}

impl Book {
    pub fn entries(&self) -> &[Entry] {
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

    pub fn get(&self, room: RoomName) -> Option<&Entry> {
        self.find(room).ok().and_then(|i| self.entries.get(i))
    }

    /// The room's entry, a new one ([`Entry::new`]) if there is none.
    pub fn entry(&mut self, room: RoomName, now: u32) -> Option<&mut Entry> {
        let i = match self.find(room) {
            Ok(i) => i,
            Err(i) => {
                self.entries.insert(i, Entry::new(room, now));
                i
            }
        };
        self.entries.get_mut(i)
    }

    /// The tick from which a signer should go to `room` (0, at once, for a room with no entry).
    pub fn due_at(&self, room: RoomName, cfg: &SignConfig) -> u32 {
        self.get(room).map_or(0, |e| e.due_at(cfg))
    }

    /// Appends the payload to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let count = u16::try_from(self.entries.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&count.to_le_bytes());
        for e in self.entries.iter().take(usize::from(count)) {
            out.extend_from_slice(&e.room.packed_repr().to_le_bytes());
            out.push(e.state.code());
            out.extend_from_slice(&e.seen.to_le_bytes());
            let tile = e.controller.map_or(NO_TILE, tile_code);
            out.extend_from_slice(&tile.to_le_bytes());
            out.push(e.deaths);
            out.push(e.overwrites);
            out.push(u8::try_from(e.towers.len().min(MAX_TOWERS)).unwrap_or(0));
            for &t in e.towers.iter().take(MAX_TOWERS) {
                out.extend_from_slice(&tile_code(t).to_le_bytes());
            }
        }
    }

    /// Parses a payload [`Self::encode`] wrote.
    pub fn decode(payload: &[u8]) -> Result<Self, RecordError> {
        let mut r = Reader(payload);
        let count = r.u16()?;
        let mut entries: Vec<Entry> = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let room = RoomName::from_packed(r.u16()?);
            if entries
                .last()
                .is_some_and(|p| p.room.packed_repr() >= room.packed_repr())
            {
                return Err(RecordError::Order);
            }
            let code = r.u8()?;
            let state = SignState::from_code(code).ok_or(RecordError::State(code))?;
            let seen = r.u32()?;
            let controller = match r.u16()? {
                NO_TILE => None,
                code => Some(tile_from(code)?),
            };
            let deaths = r.u8()?;
            let overwrites = r.u8()?;
            let n = r.u8()?;
            if usize::from(n) > MAX_TOWERS {
                return Err(RecordError::Towers(n));
            }
            let mut towers = Vec::with_capacity(usize::from(n));
            for _ in 0..n {
                towers.push(tile_from(r.u16()?)?);
            }
            entries.push(Entry {
                room,
                state,
                seen,
                controller,
                towers,
                deaths,
                overwrites,
            });
        }
        if !r.0.is_empty() {
            return Err(RecordError::Long);
        }
        Ok(Self { entries })
    }
}

/// The payload's length for `rooms` entries holding `towers` towers in all. Unit: bytes.
pub const fn payload_len(rooms: usize, towers: usize) -> usize {
    2 + rooms * 12 + towers * 2
}

fn tile_code(t: Tile) -> u16 {
    u16::try_from(t.index()).unwrap_or(NO_TILE)
}

fn tile_from(code: u16) -> Result<Tile, RecordError> {
    Tile::from_index(usize::from(code)).ok_or(RecordError::Tile(code))
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
