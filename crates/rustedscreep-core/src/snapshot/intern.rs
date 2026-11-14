//! Stable handles for game objects (ROADMAP P1 module 5): an id is interned once, and while its object
//! stays in the snapshot on consecutive ticks it keeps the same small handle.
//!
//! Cache rule: a slot lives from the tick its object is first pushed until the first pass that does
//! not push it. Then the slot is released and its generation advances, so every handle to it stops
//! resolving and a later object that reuses the slot gets a different handle. Free slots are reused
//! oldest first. All of this lives in Rust memory only and rebuilds by itself after a global reset.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use std::collections::VecDeque;

use rustc_hash::FxHashMap;

use super::SkipReason;

/// A game object's id in the packed form of screeps-game-api's `RawObjectId`: the id's hex value
/// shifted left 32 bits, ORed with the id string's length (VERIFICATION_LOG V103), so
/// `RawObjectId::from_packed(key.0)` gives the id back in rustedscreep-wasm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectKey(pub u128);

impl ObjectKey {
    /// The longest id `RawObjectId` packs: 24 hex digits, 96 bits.
    pub const MAX_ID_LEN: usize = 24;

    /// Parses an object id of 1 to [`Self::MAX_ID_LEN`] lowercase hex digits, the form the engine's
    /// ids take (MongoDB ObjectIds on the MMO, shorter random hex on private servers; V103). It packs
    /// like `RawObjectId::from_str`, but rejects the uppercase digits and leading `+` that `from_str`
    /// accepts, because those would alias a real id. So two different accepted ids never give the
    /// same key, and a leading zero counts ("0abc" and "abc" differ).
    pub fn from_hex(id: &str) -> Option<Self> {
        let bytes = id.as_bytes();
        if bytes.is_empty() || bytes.len() > Self::MAX_ID_LEN {
            return None;
        }
        let mut value: u128 = 0;
        for &b in bytes {
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                _ => return None,
            };
            // At most 24 digits, so `value` stays below 2^96 and the shift below cannot overflow.
            value = (value << 4) | u128::from(digit);
        }
        let len = u128::from(u8::try_from(bytes.len()).ok()?);
        Some(Self((value << 32) | len))
    }
}

/// A handle to an object of class `C` (a marker type such as [`super::CreepClass`]). It is two
/// `u32`s: the slot, dense within the class (for side tables), and the slot's generation.
pub struct Handle<C> {
    slot: u32,
    generation: u32,
    _class: PhantomData<fn() -> C>,
}

impl<C> Handle<C> {
    const fn new(slot: u32, generation: u32) -> Self {
        Self {
            slot,
            generation,
            _class: PhantomData,
        }
    }

    /// The slot index, dense within the class. A side table indexed by it must also keep the handle
    /// (or its generation), because a slot is reused by later objects.
    pub const fn slot(self) -> u32 {
        self.slot
    }

    /// The slot's generation when this handle was made.
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

// Implemented by hand so that they do not require `C` to implement them.
impl<C> Clone for Handle<C> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<C> Copy for Handle<C> {}
impl<C> PartialEq for Handle<C> {
    fn eq(&self, other: &Self) -> bool {
        (self.slot, self.generation) == (other.slot, other.generation)
    }
}
impl<C> Eq for Handle<C> {}
impl<C> Hash for Handle<C> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.slot, self.generation).hash(state);
    }
}
impl<C> PartialOrd for Handle<C> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<C> Ord for Handle<C> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        (self.slot, self.generation).cmp(&(other.slot, other.generation))
    }
}
impl<C> fmt::Debug for Handle<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handle({}#{})", self.slot, self.generation)
    }
}

struct Slot<S> {
    key: ObjectKey,
    generation: u32,
    /// The pass that last pushed this slot's object; 0 never matches, because passes start at 1.
    seen: u32,
    /// The object's row in the tables of pass `seen`.
    row: u32,
    live: bool,
    /// Data read once, when the object was first pushed (it never changes for the object's life).
    data: S,
}

/// Interns object ids of class `C` into [`Handle`]s and keeps per-object static data `S`.
pub struct Interner<C, S> {
    by_key: FxHashMap<ObjectKey, u32>,
    slots: Vec<Slot<S>>,
    /// Released slots, oldest first.
    free: VecDeque<u32>,
    pass: u32,
    added: u32,
    released: u32,
    _class: PhantomData<fn() -> C>,
}

impl<C, S> Default for Interner<C, S> {
    fn default() -> Self {
        Self {
            by_key: FxHashMap::default(),
            slots: Vec::new(),
            free: VecDeque::new(),
            pass: 0,
            added: 0,
            released: 0,
            _class: PhantomData,
        }
    }
}

impl<C, S> Interner<C, S> {
    /// Starts a pass. Rows recorded in earlier passes stop resolving.
    pub fn begin_pass(&mut self) {
        self.pass = self.pass.wrapping_add(1);
        if self.pass == 0 {
            // After 2^32 passes: clear the stamps so that no old one matches the new pass.
            for slot in &mut self.slots {
                slot.seen = 0;
            }
            self.pass = 1;
        }
        self.added = 0;
    }

    /// Records that `key`'s object is in this pass at `row`, and returns its handle and static data.
    /// `init` runs only for a key that has no live slot, and its error leaves the interner unchanged.
    ///
    /// Errors: [`SkipReason::Duplicate`] when the key was already marked in this pass,
    /// [`SkipReason::SlotsExhausted`] when no slot can be made, and whatever `init` returns.
    pub fn mark(
        &mut self,
        key: ObjectKey,
        row: u32,
        init: impl FnOnce() -> Result<S, SkipReason>,
    ) -> Result<(Handle<C>, &S), SkipReason> {
        let pass = self.pass;
        let existing = self
            .by_key
            .get(&key)
            .copied()
            .filter(|&slot| self.slot_holds(slot, key));
        let slot_index = match existing {
            Some(slot_index) => slot_index,
            None => {
                // A key whose slot does not hold it would be a bug; dropping it keeps the map in step.
                self.by_key.remove(&key);
                let data = init()?;
                let slot_index = self.allocate(key, data)?;
                self.by_key.insert(key, slot_index);
                self.added = self.added.saturating_add(1);
                slot_index
            }
        };
        let slot = usize::try_from(slot_index)
            .ok()
            .and_then(|i| self.slots.get_mut(i))
            .ok_or(SkipReason::SlotsExhausted)?;
        if slot.seen == pass {
            return Err(SkipReason::Duplicate);
        }
        slot.seen = pass;
        slot.row = row;
        Ok((Handle::new(slot_index, slot.generation), &slot.data))
    }

    /// Ends the pass: every live slot whose object was not marked in it is released. Returns how
    /// many were released.
    pub fn end_pass(&mut self) -> u32 {
        let pass = self.pass;
        let mut released = 0u32;
        for (index, slot) in (0u32..).zip(self.slots.iter_mut()) {
            if !slot.live || slot.seen == pass {
                continue;
            }
            slot.live = false;
            self.by_key.remove(&slot.key);
            // A slot whose generation cannot advance is retired, so no handle can ever alias.
            if let Some(next) = slot.generation.checked_add(1) {
                slot.generation = next;
                self.free.push_back(index);
            }
            released = released.saturating_add(1);
        }
        self.released = released;
        released
    }

    /// The handle's row in the current pass, if its object was marked in it.
    pub fn row(&self, handle: Handle<C>) -> Option<u32> {
        let slot = self.live_slot(handle)?;
        (slot.seen == self.pass).then_some(slot.row)
    }

    /// The static data of a handle whose slot is still live.
    pub fn data(&self, handle: Handle<C>) -> Option<&S> {
        self.live_slot(handle).map(|slot| &slot.data)
    }

    /// The id of a handle whose slot is still live. Sorting by it gives an order that survives a
    /// global reset, unlike handle order, which follows when objects were first seen.
    pub fn key(&self, handle: Handle<C>) -> Option<ObjectKey> {
        self.live_slot(handle).map(|slot| slot.key)
    }

    /// The live handle for `key`, if its object has a slot.
    pub fn get(&self, key: ObjectKey) -> Option<Handle<C>> {
        let index = *self.by_key.get(&key)?;
        let slot = self.slots.get(usize::try_from(index).ok()?)?;
        (slot.live && slot.key == key).then_some(Handle::new(index, slot.generation))
    }

    /// Live slots (objects pushed in the last pass, plus any pushed in the current one).
    pub fn live(&self) -> usize {
        self.by_key.len()
    }

    /// Slots ever made, live or not; side tables indexed by [`Handle::slot`] need this length.
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Keys interned for the first time in the current pass.
    pub fn added(&self) -> u32 {
        self.added
    }

    /// Slots the last [`Self::end_pass`] released.
    pub fn released(&self) -> u32 {
        self.released
    }

    fn slot_holds(&self, index: u32, key: ObjectKey) -> bool {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.slots.get(i))
            .is_some_and(|slot| slot.live && slot.key == key)
    }

    fn live_slot(&self, handle: Handle<C>) -> Option<&Slot<S>> {
        let slot = self.slots.get(usize::try_from(handle.slot).ok()?)?;
        (slot.live && slot.generation == handle.generation).then_some(slot)
    }

    /// Takes the oldest free slot, or makes a new one.
    fn allocate(&mut self, key: ObjectKey, data: S) -> Result<u32, SkipReason> {
        while let Some(index) = self.free.pop_front() {
            let Some(slot) = usize::try_from(index)
                .ok()
                .and_then(|i| self.slots.get_mut(i))
            else {
                continue;
            };
            if slot.live {
                continue;
            }
            slot.key = key;
            slot.seen = 0;
            slot.live = true;
            slot.data = data;
            return Ok(index);
        }
        let index = u32::try_from(self.slots.len()).map_err(|_| SkipReason::SlotsExhausted)?;
        self.slots.push(Slot {
            key,
            generation: 0,
            seen: 0,
            row: 0,
            live: true,
            data,
        });
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Test {}
    type Ids = Interner<Test, u32>;

    fn key(n: u128) -> ObjectKey {
        ObjectKey(n)
    }

    fn mark(ids: &mut Ids, k: u128, row: u32) -> Result<Handle<Test>, SkipReason> {
        ids.mark(key(k), row, || Ok(u32::try_from(k).unwrap_or(u32::MAX)))
            .map(|(h, _)| h)
    }

    /// The packing is RawObjectId's definition (screeps-game-api 0.23.5 raw.rs:110-127, V103):
    /// `(hex value << 32) | string length`. Examples: a 24-digit MongoDB-style id and a 15-digit
    /// private-server-style id.
    #[test]
    fn ids_pack_like_raw_object_id() {
        for id in [
            "5bbcaa9099f9b0a0f7f4b3c6",
            "0000000000000000000000ff",
            "a1b2c3d4e5f6789",
            "0",
        ] {
            let value = u128::from_str_radix(id, 16).expect("hex");
            let len = u128::try_from(id.len()).expect("small");
            assert_eq!(
                ObjectKey::from_hex(id),
                Some(ObjectKey((value << 32) | len)),
                "{id}"
            );
        }
        let max = "f".repeat(ObjectKey::MAX_ID_LEN);
        assert_eq!(
            ObjectKey::from_hex(&max),
            Some(ObjectKey((u128::MAX >> 32 << 32) | 24)),
            "24 digits fill the 96 value bits"
        );
    }

    /// Leading zeros count, and everything RawObjectId accepts but the engine never produces is
    /// rejected, so no accepted id aliases another.
    #[test]
    fn ids_are_injective_and_strict() {
        assert_ne!(ObjectKey::from_hex("0abc"), ObjectKey::from_hex("abc"));
        let too_long = "1".repeat(ObjectKey::MAX_ID_LEN + 1);
        for bad in [
            "",
            "+1",
            "-1",
            "ABC",
            "aBc",
            "0x12",
            " 12",
            "12 ",
            "g",
            "é",
            too_long.as_str(),
        ] {
            assert_eq!(ObjectKey::from_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_handle_is_stable_while_its_object_stays() {
        let mut ids = Ids::default();
        ids.begin_pass();
        let a = mark(&mut ids, 10, 0).expect("new");
        let b = mark(&mut ids, 11, 1).expect("new");
        assert_ne!(a, b);
        assert_eq!(ids.added(), 2);
        assert_eq!(ids.end_pass(), 0);

        ids.begin_pass();
        assert_eq!(ids.row(a), None, "rows of the last pass stop resolving");
        assert_eq!(mark(&mut ids, 11, 0), Ok(b));
        assert_eq!(mark(&mut ids, 10, 1), Ok(a));
        assert_eq!(ids.added(), 0);
        assert_eq!((ids.row(a), ids.row(b)), (Some(1), Some(0)));
        assert_eq!(ids.end_pass(), 0);
        assert_eq!(ids.get(key(10)), Some(a));
        assert_eq!(ids.key(a), Some(key(10)));
        assert_eq!(ids.data(a), Some(&10));

        ids.begin_pass();
        ids.end_pass();
        assert_eq!(ids.key(a), None, "a released handle has no key");
    }

    #[test]
    fn a_missing_object_releases_its_handle_and_the_slot_is_reused_with_a_new_generation() {
        let mut ids = Ids::default();
        ids.begin_pass();
        let a = mark(&mut ids, 10, 0).expect("new");
        ids.end_pass();

        ids.begin_pass();
        assert_eq!(ids.end_pass(), 1);
        assert_eq!(
            (ids.row(a), ids.data(a), ids.get(key(10))),
            (None, None, None)
        );
        assert_eq!(ids.live(), 0);

        ids.begin_pass();
        let c = mark(&mut ids, 12, 0).expect("new");
        assert_eq!(c.slot(), a.slot(), "the free slot is reused");
        assert_ne!(c, a, "with a new generation");
        assert_eq!(
            ids.row(a),
            None,
            "the stale handle never resolves to the new object"
        );
        assert_eq!(ids.data(c), Some(&12));
        assert_eq!(ids.capacity(), 1);

        // The same id coming back later gets a fresh handle, too.
        let back = mark(&mut ids, 10, 1).expect("new");
        assert_ne!(back, a);
    }

    #[test]
    fn free_slots_are_reused_oldest_first() {
        let mut ids = Ids::default();
        ids.begin_pass();
        let handles: Vec<_> = (0..3)
            .map(|k| mark(&mut ids, k, u32::try_from(k).expect("small")).expect("new"))
            .collect();
        ids.end_pass();
        ids.begin_pass();
        mark(&mut ids, 1, 0).expect("seen");
        ids.end_pass(); // releases slots 0 and 2, in that order
        ids.begin_pass();
        let first = mark(&mut ids, 100, 1).expect("new");
        let second = mark(&mut ids, 101, 2).expect("new");
        assert_eq!(first.slot(), handles[0].slot());
        assert_eq!(second.slot(), handles[2].slot());
    }

    #[test]
    fn a_key_marked_twice_in_one_pass_is_a_duplicate() {
        let mut ids = Ids::default();
        ids.begin_pass();
        let a = mark(&mut ids, 10, 0).expect("new");
        assert_eq!(mark(&mut ids, 10, 1), Err(SkipReason::Duplicate));
        assert_eq!(ids.row(a), Some(0), "the first row stays");
        ids.end_pass();
        ids.begin_pass();
        assert_eq!(mark(&mut ids, 10, 0), Ok(a));
        assert_eq!(mark(&mut ids, 10, 1), Err(SkipReason::Duplicate));
    }

    #[test]
    fn init_runs_once_per_object_life_and_its_error_changes_nothing() {
        let mut ids = Ids::default();
        let mut calls = 0;
        for _ in 0..3 {
            ids.begin_pass();
            let r = ids.mark(key(5), 0, || {
                calls += 1;
                Ok(7)
            });
            assert!(r.is_ok());
            ids.end_pass();
        }
        assert_eq!(calls, 1, "static data is read on first sight only");

        ids.begin_pass();
        let r = ids.mark(key(6), 1, || Err(SkipReason::BadPosition));
        assert_eq!(r.map(|(h, _)| h), Err(SkipReason::BadPosition));
        assert_eq!((ids.live(), ids.capacity(), ids.added()), (1, 1, 0));
        assert_eq!(ids.get(key(6)), None);
    }

    #[test]
    fn an_exhausted_generation_retires_its_slot() {
        let mut ids = Ids::default();
        ids.begin_pass();
        let a = mark(&mut ids, 1, 0).expect("new");
        ids.end_pass();
        // Force the slot to the last generation, then release it.
        if let Some(slot) = ids.slots.get_mut(0) {
            slot.generation = u32::MAX;
        }
        ids.begin_pass();
        assert_eq!(ids.end_pass(), 1);
        assert!(ids.free.is_empty(), "the slot is retired, not freed");
        ids.begin_pass();
        let b = mark(&mut ids, 2, 0).expect("new");
        assert_ne!(b.slot(), a.slot());
        assert_eq!(ids.capacity(), 2);
    }

    #[test]
    fn the_pass_counter_wraps_without_matching_old_stamps() {
        let mut ids = Ids::default();
        ids.begin_pass();
        let a = mark(&mut ids, 1, 0).expect("new");
        ids.end_pass();
        ids.pass = u32::MAX;
        ids.begin_pass();
        assert_eq!(ids.pass, 1);
        assert_eq!(
            ids.row(a),
            None,
            "a stamp from before the wrap does not count as seen"
        );
        assert_eq!(mark(&mut ids, 1, 3), Ok(a));
        assert_eq!(ids.row(a), Some(3));
    }

    /// Deterministic churn: each pass keeps a pseudo-random subset of the known ids and adds new
    /// ones. Invariants: a handle stays the same while its id is marked on consecutive passes,
    /// resolves to the row it was marked with, and never resolves once its id was missed.
    #[test]
    fn churn_keeps_the_invariants() {
        let mut ids = Ids::default();
        let mut state: u64 = 0x2545_f491_4f6c_dd1d; // xorshift seed
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut present: Vec<(u128, Handle<Test>)> = Vec::new();
        let mut stale: Vec<Handle<Test>> = Vec::new();
        let mut next_key: u128 = 1;
        for _ in 0..500 {
            ids.begin_pass();
            let mut kept = Vec::new();
            for &(k, h) in &present {
                if next() % 4 == 0 {
                    stale.push(h);
                } else {
                    kept.push((k, h));
                }
            }
            for _ in 0..(next() % 5) {
                kept.push((next_key, Handle::new(u32::MAX, u32::MAX)));
                next_key += 1;
            }
            let mut marked = Vec::new();
            for (row, &(k, old)) in (0u32..).zip(&kept) {
                let h = mark(&mut ids, k, row).expect("mark");
                if old.slot() != u32::MAX {
                    assert_eq!(h, old, "stable while present");
                }
                marked.push((k, h));
            }
            ids.end_pass();
            for (row, &(_, h)) in (0u32..).zip(&marked) {
                assert_eq!(ids.row(h), Some(row));
            }
            for &h in &stale {
                assert_eq!(ids.row(h), None, "stale handles never resolve");
            }
            assert_eq!(ids.live(), marked.len());
            present = marked;
        }
        assert!(
            ids.capacity() < usize::try_from(next_key).expect("small"),
            "slots are reused"
        );
    }
}
