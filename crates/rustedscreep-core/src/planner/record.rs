//! The planner's persisted record (ADR-027): per owned room, the tile its plan is anchored on and
//! the containers and links the plan adopted (ADR-026), so a global reset rebuilds the plan the
//! room was built from even while its anchor spawn is lost and not yet sited, or an adopted
//! container is lost and not yet re-sited, and the anchor search takes one run.
//!
//! One frame (`persist::frame`, owner [`crate::persist::kind::PLANNER`], schema [`SCHEMA`]) in the
//! segment `PersistConfig::planner_segment`, written when a room's entry changes (a plan made, a
//! room no longer owned). The payload, little-endian:
//!
//! ```text
//! payload = count:u16 entry*count          (entries in room order, no room twice)
//! entry   = room:u16 anchor:u16 n:u8 (kind:u8 tile:u16)*n
//! room    = RoomName::packed_repr (V255)   anchor, tile = Tile::index (x * 50 + y, < ROOM_AREA)
//! kind    = 0 container, 1 link            n <= MAX_ADOPTED
//! ```
//!
//! Anything else (a short or long payload, a tile off the room, an unknown kind, rooms out of
//! order) refuses the whole record: the planner then plans from what stands, as without one.

use rustedscreep_consts::{RoomName, StructureType};

use super::layout::MAX_LEVEL;
use super::rules::allowed;
use crate::movement::Tile;

/// The payload layout's version (the frame's `schema`).
pub const SCHEMA: u16 = 1;

/// The most adopted structures one entry holds: every container and link the top level allows.
///
/// Uses: `CONTROLLER_STRUCTURES` (through `rules::allowed` at `MAX_LEVEL`).
pub fn max_adopted() -> usize {
    let n = allowed(StructureType::Container, MAX_LEVEL) + allowed(StructureType::Link, MAX_LEVEL);
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// One room's entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub room: RoomName,
    /// The tile the room's plan was anchored on.
    pub anchor: Tile,
    /// The containers and links the plan adopted, in kind then tile order.
    pub adopted: Vec<(StructureType, Tile)>,
}

/// Why a payload was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    /// The payload ends inside an entry, or before its count.
    Short,
    /// Bytes are left after the last entry.
    Long,
    /// A tile index at or past `ROOM_AREA`.
    Tile(u16),
    /// A kind code other than a container's or a link's.
    Kind(u8),
    /// More adopted structures than [`max_adopted`].
    Adopted(u8),
    /// A room not after the one before it.
    Order,
}

impl core::fmt::Display for RecordError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Short => f.write_str("the payload ends early"),
            Self::Long => f.write_str("bytes after the last entry"),
            Self::Tile(t) => write!(f, "tile index {t} off the room"),
            Self::Kind(k) => write!(f, "unknown structure code {k}"),
            Self::Adopted(n) => write!(f, "{n} adopted structures, more than a room holds"),
            Self::Order => f.write_str("rooms out of order"),
        }
    }
}

/// The record: entries by room.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    entries: Vec<Entry>,
}

fn kind_code(kind: StructureType) -> Option<u8> {
    match kind {
        StructureType::Container => Some(0),
        StructureType::Link => Some(1),
        _ => None,
    }
}

fn code_kind(code: u8) -> Option<StructureType> {
    match code {
        0 => Some(StructureType::Container),
        1 => Some(StructureType::Link),
        _ => None,
    }
}

/// Reads the payload's fields in order.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, RecordError> {
        let (&b, rest) = self.bytes.split_first().ok_or(RecordError::Short)?;
        self.bytes = rest;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16, RecordError> {
        Ok(u16::from_le_bytes([self.u8()?, self.u8()?]))
    }

    fn tile(&mut self) -> Result<Tile, RecordError> {
        let i = self.u16()?;
        Tile::from_index(usize::from(i)).ok_or(RecordError::Tile(i))
    }
}

fn tile_code(t: Tile) -> u16 {
    u16::try_from(t.index()).unwrap_or(u16::MAX)
}

impl Record {
    /// The entries, in room order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn position(&self, room: RoomName) -> Result<usize, usize> {
        self.entries
            .binary_search_by_key(&room.packed_repr(), |e| e.room.packed_repr())
    }

    /// Room `room`'s entry.
    pub fn get(&self, room: RoomName) -> Option<&Entry> {
        self.position(room).ok().and_then(|i| self.entries.get(i))
    }

    /// Sets the entry of `entry.room` (its adopted structures sorted, only containers and links,
    /// at most [`max_adopted`]). Whether the record changed.
    pub fn set(&mut self, mut entry: Entry) -> bool {
        entry.adopted.retain(|&(k, _)| kind_code(k).is_some());
        entry
            .adopted
            .sort_unstable_by_key(|&(k, t)| (kind_code(k), t.index()));
        entry.adopted.dedup();
        entry.adopted.truncate(max_adopted());
        match self.position(entry.room) {
            Ok(i) => match self.entries.get_mut(i) {
                Some(e) if *e == entry => false,
                Some(e) => {
                    *e = entry;
                    true
                }
                None => false,
            },
            Err(i) => {
                self.entries.insert(i, entry);
                true
            }
        }
    }

    /// Keeps the entries of the rooms `keep` says. Whether the record changed.
    pub fn retain(&mut self, mut keep: impl FnMut(RoomName) -> bool) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| keep(e.room));
        self.entries.len() != before
    }

    /// The payload (module docs) into `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let count = u16::try_from(self.entries.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&count.to_le_bytes());
        for e in self.entries.iter().take(usize::from(count)) {
            out.extend_from_slice(&e.room.packed_repr().to_le_bytes());
            out.extend_from_slice(&tile_code(e.anchor).to_le_bytes());
            let n = u8::try_from(e.adopted.len()).unwrap_or(u8::MAX);
            out.push(n);
            for &(k, t) in e.adopted.iter().take(usize::from(n)) {
                out.push(kind_code(k).unwrap_or(u8::MAX));
                out.extend_from_slice(&tile_code(t).to_le_bytes());
            }
        }
    }

    /// The record a payload holds, or why it is refused (module docs).
    pub fn decode(payload: &[u8]) -> Result<Self, RecordError> {
        let mut r = Reader { bytes: payload };
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
            let anchor = r.tile()?;
            let n = r.u8()?;
            if usize::from(n) > max_adopted() {
                return Err(RecordError::Adopted(n));
            }
            let mut adopted = Vec::with_capacity(usize::from(n));
            for _ in 0..n {
                let code = r.u8()?;
                let kind = code_kind(code).ok_or(RecordError::Kind(code))?;
                adopted.push((kind, r.tile()?));
            }
            entries.push(Entry {
                room,
                anchor,
                adopted,
            });
        }
        if r.bytes.is_empty() {
            Ok(Self { entries })
        } else {
            Err(RecordError::Long)
        }
    }
}

/// Bytes the payload of `rooms` entries with `adopted` structures each takes: a room's entry is at
/// most `payload_len(1, max_adopted())` bytes, so a realistic empire fits one segment.
pub const fn payload_len(rooms: usize, adopted: usize) -> usize {
    2 + rooms * (5 + 3 * adopted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::MAX_PAYLOAD;
    use rustedscreep_consts::ROOM_AREA;

    fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).expect("in room")
    }

    fn room(name: &str) -> RoomName {
        name.parse().expect("room name")
    }

    fn sample() -> Record {
        let mut r = Record::default();
        r.set(Entry {
            room: room("W1N1"),
            anchor: tile(25, 25),
            adopted: vec![
                (StructureType::Link, tile(30, 30)),
                (StructureType::Container, tile(11, 10)),
            ],
        });
        r.set(Entry {
            room: room("E7N57"),
            anchor: tile(43, 21),
            adopted: Vec::new(),
        });
        r
    }

    #[test]
    fn a_record_round_trips() {
        let r = sample();
        let mut out = Vec::new();
        r.encode(&mut out);
        assert_eq!(out.len(), payload_len(1, 2) + payload_len(1, 0) - 2);
        assert_eq!(Record::decode(&out), Ok(r.clone()));
        // Entries come out in room order, adopted in kind then tile order.
        let e = r.get(room("W1N1")).expect("entry");
        assert_eq!(
            e.adopted,
            [
                (StructureType::Container, tile(11, 10)),
                (StructureType::Link, tile(30, 30)),
            ]
        );
        let empty = Record::default();
        let mut out = Vec::new();
        empty.encode(&mut out);
        assert_eq!(Record::decode(&out), Ok(empty));
    }

    #[test]
    fn set_and_retain_say_whether_the_record_changed() {
        let mut r = sample();
        let same = r.get(room("E7N57")).cloned().expect("entry");
        assert!(!r.set(same.clone()));
        let moved = Entry {
            anchor: tile(40, 20),
            ..same
        };
        assert!(r.set(moved.clone()));
        assert_eq!(r.get(room("E7N57")), Some(&moved));
        assert!(!r.retain(|_| true));
        assert!(r.retain(|n| n == room("W1N1")));
        assert_eq!(r.entries().len(), 1);
        // Other types are not kept, and a room never holds more than its level allows.
        let mut r = Record::default();
        let many: Vec<_> = (0..=max_adopted())
            .map(|i| {
                (
                    StructureType::Link,
                    tile(10 + u8::try_from(i).expect("small"), 10),
                )
            })
            .chain([(StructureType::Extension, tile(1, 1))])
            .collect();
        r.set(Entry {
            room: room("W1N1"),
            anchor: tile(25, 25),
            adopted: many,
        });
        let e = r.get(room("W1N1")).expect("entry");
        assert_eq!(e.adopted.len(), max_adopted());
        assert!(e.adopted.iter().all(|&(k, _)| k == StructureType::Link));
    }

    /// Every malformed payload is refused whole. Uses: `ROOM_AREA` (a tile off the room),
    /// `CONTROLLER_STRUCTURES` (`max_adopted`).
    #[test]
    fn a_malformed_payload_is_refused() {
        let mut good = Vec::new();
        sample().encode(&mut good);
        for cut in 0..good.len() {
            assert_eq!(
                Record::decode(&good[..cut]),
                Err(RecordError::Short),
                "cut at {cut}"
            );
        }
        let mut long = good.clone();
        long.push(0);
        assert_eq!(Record::decode(&long), Err(RecordError::Long));
        let off = u16::try_from(ROOM_AREA).expect("fits");
        let mut bad = Vec::new();
        bad.extend_from_slice(&1u16.to_le_bytes());
        bad.extend_from_slice(&room("W1N1").packed_repr().to_le_bytes());
        bad.extend_from_slice(&off.to_le_bytes());
        bad.push(0);
        assert_eq!(Record::decode(&bad), Err(RecordError::Tile(off)));
        let entry = |code: u8, n: u8| {
            let mut b = Vec::new();
            b.extend_from_slice(&1u16.to_le_bytes());
            b.extend_from_slice(&room("W1N1").packed_repr().to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.push(n);
            for _ in 0..n {
                b.push(code);
                b.extend_from_slice(&0u16.to_le_bytes());
            }
            b
        };
        assert_eq!(Record::decode(&entry(2, 1)), Err(RecordError::Kind(2)));
        let over = u8::try_from(max_adopted() + 1).expect("small");
        assert_eq!(
            Record::decode(&entry(0, over)),
            Err(RecordError::Adopted(over))
        );
        // Two entries for one room, or out of order.
        let mut twice = Vec::new();
        twice.extend_from_slice(&2u16.to_le_bytes());
        for _ in 0..2 {
            twice.extend_from_slice(&room("W1N1").packed_repr().to_le_bytes());
            twice.extend_from_slice(&0u16.to_le_bytes());
            twice.push(0);
        }
        assert_eq!(Record::decode(&twice), Err(RecordError::Order));
    }

    /// The most rooms the count field allows, each with every container and link the top level
    /// allows, would not fit a segment, but any realistic empire does: a hundred rooms take a
    /// small part of one. Uses: `MAX_PAYLOAD` (the segment's payload limit), `max_adopted`.
    #[test]
    fn a_large_empire_fits_one_segment() {
        assert!(payload_len(100, max_adopted()) < MAX_PAYLOAD / 10);
    }
}
