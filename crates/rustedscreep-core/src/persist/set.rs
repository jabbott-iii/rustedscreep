//! Segment ids and sets of them. A set is a `u128` bitset, so it is `Copy`, compares in one step and
//! iterates in ascending id order, which keeps every plan deterministic.

use rustedscreep_consts::extra::MEMORY_SEGMENT_COUNT;

/// A RawMemory segment id: `0..MEMORY_SEGMENT_COUNT` (0 to 99, V85), checked on construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId(u8);

impl SegmentId {
    /// Uses: `rustedscreep_consts::extra::MEMORY_SEGMENT_COUNT`.
    pub const fn new(id: u8) -> Option<Self> {
        if id < MEMORY_SEGMENT_COUNT {
            Some(Self(id))
        } else {
            None
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    /// The id a key of `RawMemory.segments` names, if the key is exactly the one assigning
    /// `segments[id]` creates ("7", not "07" or "+7"). The driver's save would parse those to id 7
    /// too (V85), but they are other keys, so a write to id 7 would add a key a count did not expect.
    pub fn from_key(key: &str) -> Option<Self> {
        let canonical = !key.is_empty()
            && key.bytes().all(|b| b.is_ascii_digit())
            && !(key.len() > 1 && key.starts_with('0'));
        if !canonical {
            return None;
        }
        key.parse::<u8>().ok().and_then(Self::new)
    }

    /// Index into per-segment tables of `MEMORY_SEGMENT_COUNT` entries.
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The id's bit; ids are below 100 < 128, so the shift never overflows (the fallback is unreachable).
    const fn bit(self) -> u128 {
        match 1u128.checked_shl(self.0 as u32) {
            Some(b) => b,
            None => 0,
        }
    }
}

/// A set of segment ids.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentSet(u128);

impl SegmentSet {
    pub const EMPTY: Self = Self(0);

    pub const fn of(id: SegmentId) -> Self {
        Self(id.bit())
    }

    pub const fn contains(self, id: SegmentId) -> bool {
        self.0 & id.bit() != 0
    }

    pub fn insert(&mut self, id: SegmentId) {
        self.0 |= id.bit();
    }

    pub fn remove(&mut self, id: SegmentId) {
        self.0 &= !id.bit();
    }

    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// The ids in `self` and not in `other`.
    pub const fn minus(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// The lowest `n` ids of the set (all of them if it has `n` or fewer).
    pub fn lowest(self, n: u32) -> Self {
        let mut out = Self::EMPTY;
        for id in self.iter().take(n as usize) {
            out.insert(id);
        }
        out
    }

    /// The ids in ascending order.
    pub fn iter(self) -> Iter {
        Iter(self.0)
    }
}

impl FromIterator<SegmentId> for SegmentSet {
    fn from_iter<I: IntoIterator<Item = SegmentId>>(ids: I) -> Self {
        let mut set = Self::EMPTY;
        for id in ids {
            set.insert(id);
        }
        set
    }
}

/// Ascending iteration over a [`SegmentSet`].
#[derive(Clone, Debug)]
pub struct Iter(u128);

impl Iterator for Iter {
    type Item = SegmentId;

    fn next(&mut self) -> Option<SegmentId> {
        if self.0 == 0 {
            return None;
        }
        let low = self.0.trailing_zeros();
        self.0 &= self.0 - 1;
        // A bit set in a SegmentSet is always a valid id; the conversion cannot fail.
        u8::try_from(low).ok().and_then(SegmentId::new)
    }
}

/// Writes the set as `[1, 2, 99]`.
impl core::fmt::Display for SegmentSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("[")?;
        for (i, id) in self.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}", id.get())?;
        }
        f.write_str("]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> SegmentId {
        SegmentId::new(n).expect("valid id")
    }

    /// Uses: `extra::MEMORY_SEGMENT_COUNT`. Exactly the ids below the count exist.
    #[test]
    fn ids_follow_the_segment_count() {
        for n in 0..=u8::MAX {
            assert_eq!(
                SegmentId::new(n).is_some(),
                n < MEMORY_SEGMENT_COUNT,
                "id {n}"
            );
        }
    }

    /// Only the keys an assignment `segments[id] = ...` creates name an id.
    #[test]
    fn keys_name_ids_only_in_canonical_form() {
        for n in 0..MEMORY_SEGMENT_COUNT {
            assert_eq!(SegmentId::from_key(&n.to_string()), Some(id(n)));
        }
        for bad in [
            "", "07", "00", "+7", "-0", " 7", "7 ", "7.0", "1e1", "100", "256", "99999", "x",
        ] {
            assert_eq!(SegmentId::from_key(bad), None, "{bad:?}");
        }
    }

    /// Every id has its own bit, and iteration returns the members in ascending order.
    #[test]
    fn sets_hold_every_id() {
        let all: SegmentSet = (0..MEMORY_SEGMENT_COUNT).map(id).collect();
        assert_eq!(all.len(), u32::from(MEMORY_SEGMENT_COUNT));
        let back: Vec<u8> = all.iter().map(SegmentId::get).collect();
        assert_eq!(back, (0..MEMORY_SEGMENT_COUNT).collect::<Vec<_>>());
        let mut s = SegmentSet::EMPTY;
        for n in [99, 0, 42, 7] {
            s.insert(id(n));
        }
        assert_eq!(
            s.iter().map(SegmentId::get).collect::<Vec<_>>(),
            [0, 7, 42, 99]
        );
        assert_eq!(s.to_string(), "[0, 7, 42, 99]");
        assert_eq!(SegmentSet::EMPTY.to_string(), "[]");
        s.remove(id(42));
        assert!(!s.contains(id(42)) && s.contains(id(7)));
        assert_eq!(
            s.lowest(2).iter().map(SegmentId::get).collect::<Vec<_>>(),
            [0, 7]
        );
        assert_eq!(s.lowest(10), s);
        let other: SegmentSet = [id(7), id(8)].into_iter().collect();
        assert_eq!(s.minus(other).to_string(), "[0, 99]");
        assert_eq!(s.intersection(other).to_string(), "[7]");
        assert_eq!(s.union(other).to_string(), "[0, 7, 8, 99]");
    }
}
