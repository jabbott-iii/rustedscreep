//! Runtime overrides (ROADMAP P1 module 10): changes to the active profile's values that the console
//! makes (`rustedscreep.configSet`, `configUnset`, `configReset`) and a RawMemory segment keeps
//! across global resets.
//!
//! - Only fields with [`Field::runtime`] can be overridden.
//! - A number outside its field's safe range is clamped to it. Any other bad value, an unknown key or
//!   a field that cannot be overridden is refused with a [`Note`], and the value is ignored, never
//!   trusted.
//! - An override that would break one of [`RULES`] is refused: a console change leaves the overrides
//!   as they were, and a loaded record loses every override in a broken rule. Every profile satisfies
//!   every rule, so this always ends with a coherent [`Config`].
//! - The record is an ADR-004 frame of kind [`kind::CONFIG`] and schema [`SCHEMA`]. Its payload is
//!   ASCII lines `key=value\n` in field order, so a removed or renamed field is refused by key on the
//!   next load rather than misread.
//!
//! [`Store`] holds the profile's values, the overrides and the result, which the tick reads as a
//! plain struct. The effective config changes only when the console changes an override, the record
//! loads, or the naive guard ends a naive span ([`Store::naive_tick`], the one call per tick: a few
//! comparisons, and work only on the tick it ends a span; ADR-022).

use core::fmt::{self, Write as _};

use super::{Config, FIELD_COUNT, FIELDS, Field, Kind, LogLevel, RULES, Rule, Value, field};
use crate::persist::{Frame, LoadError, kind};

/// The payload layout version written into the frame header.
pub const SCHEMA: u16 = 1;

/// Why an override was refused, clamped or dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// The record's payload is not UTF-8.
    NotText,
    /// A record line without `=`.
    BadLine { line: u32 },
    /// No field has this key.
    UnknownKey { key: String },
    /// The field exists but only a profile can change it.
    NotRuntime { key: &'static str },
    /// The text is not a value of the field's kind.
    BadValue {
        key: &'static str,
        value: String,
        expected: Kind,
    },
    /// A number outside the safe range, replaced by the nearest bound.
    Clamped {
        key: &'static str,
        requested: i64,
        used: u32,
    },
    /// A record line repeats a key; the first one counts.
    Duplicate { key: &'static str },
    /// The override would break a rule.
    Conflict {
        key: &'static str,
        rule: &'static str,
    },
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotText => f.write_str("the record is not text"),
            Self::BadLine { line } => write!(f, "line {line} has no '='"),
            Self::UnknownKey { key } => write!(f, "no config field is named '{key}'"),
            Self::NotRuntime { key } => {
                write!(
                    f,
                    "{key} cannot be overridden at runtime (only a profile sets it)"
                )
            }
            Self::BadValue {
                key,
                value,
                expected,
            } => write!(f, "{key}: '{value}' is not one of: {expected}"),
            Self::Clamped {
                key,
                requested,
                used,
            } => write!(
                f,
                "{key}: {requested} is outside the safe range, {used} used"
            ),
            Self::Duplicate { key } => write!(f, "{key} appears twice; the first one counts"),
            Self::Conflict { key, rule } => write!(f, "{key} refused: {rule}"),
        }
    }
}

/// One accepted change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub field: &'static Field,
    /// The value now in force (clamped if `clamped` is set).
    pub value: Value,
    /// The number asked for, when it was clamped to `value`.
    pub clamped: Option<i64>,
    /// The override this one replaced.
    pub previous: Option<Value>,
}

impl PartialEq for Field {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for Field {}

impl fmt::Debug for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key)
    }
}

/// The overrides in force: at most one value per field, each of the field's kind and inside its safe
/// range, together satisfying every rule with the profile's values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overrides {
    values: [Option<Value>; FIELD_COUNT],
}

impl Default for Overrides {
    fn default() -> Self {
        Self {
            values: [None; FIELD_COUNT],
        }
    }
}

/// A trimmed whole number with an optional `-`, saturated to the `i64` range.
fn parse_number(text: &str) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = digits.bytes().fold(0i64, |n, b| {
        n.saturating_mul(10).saturating_add(i64::from(b - b'0'))
    });
    Some(if negative { n.saturating_neg() } else { n })
}

/// `text` as a value of `field`'s kind, a number clamped to the safe range (with the number asked
/// for when it was clamped).
fn parse(field: &'static Field, text: &str) -> Result<(Value, Option<i64>), Note> {
    let text = text.trim();
    let bad = || Note::BadValue {
        key: field.key,
        value: text.chars().take(40).collect(),
        expected: field.kind,
    };
    match field.kind {
        Kind::Number { min, max } => {
            let n = parse_number(text).ok_or_else(bad)?;
            let used = n.clamp(i64::from(min), i64::from(max));
            // `used` is inside `min..=max`, which are u32.
            let used = u32::try_from(used).map_err(|_| bad())?;
            let clamped = (i64::from(used) != n).then_some(n);
            Ok((Value::Number(used), clamped))
        }
        Kind::Flag => match text {
            "true" => Ok((Value::Flag(true), None)),
            "false" => Ok((Value::Flag(false), None)),
            _ => Err(bad()),
        },
        Kind::Level => LogLevel::parse(text)
            .map(|l| (Value::Level(l), None))
            .ok_or_else(bad),
        // No recovery-mode field can be overridden at runtime.
        Kind::Recovery => Err(bad()),
    }
}

/// The first rule that `config` breaks among those that involve `key` (every rule when `key` is
/// `None`).
fn broken(config: &Config, key: Option<&str>) -> Option<&'static Rule> {
    RULES
        .iter()
        .filter(|r| key.is_none_or(|k| r.keys.contains(&k)))
        .find(|r| !(r.holds)(config))
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        self.values.iter().all(Option::is_none)
    }

    pub fn len(&self) -> usize {
        self.values.iter().filter(|v| v.is_some()).count()
    }

    /// The overrides in field order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static Field, Value)> + '_ {
        FIELDS
            .iter()
            .zip(&self.values)
            .filter_map(|(f, v)| v.map(|v| (f, v)))
    }

    /// The override of the field with key `key`, if any.
    pub fn get(&self, key: &str) -> Option<Value> {
        let (i, _) = field(key)?;
        self.values.get(i).copied().flatten()
    }

    /// `base` with the overrides applied.
    pub fn apply(&self, base: &Config) -> Config {
        let mut config = *base;
        for (f, v) in self.iter() {
            // Every stored value has its field's kind and range, so this always writes.
            let _ = (f.set)(&mut config, v);
        }
        config
    }

    /// Overrides the field with key `key` with `text`, clamping a number to the safe range. Refused,
    /// with nothing changed, for an unknown key, a field that cannot be overridden, a bad value or a
    /// change that would break a rule with `base` and the other overrides.
    pub fn set(&mut self, base: &Config, key: &str, text: &str) -> Result<Change, Note> {
        let (index, field) = field(key).ok_or_else(|| Note::UnknownKey {
            key: key.chars().take(60).collect(),
        })?;
        if !field.runtime {
            return Err(Note::NotRuntime { key: field.key });
        }
        let (value, clamped) = parse(field, text)?;
        let mut next = *self;
        let slot = next
            .values
            .get_mut(index)
            .ok_or(Note::NotRuntime { key: field.key })?;
        let previous = slot.replace(value);
        if let Some(rule) = broken(&next.apply(base), Some(field.key)) {
            return Err(Note::Conflict {
                key: field.key,
                rule: rule.text,
            });
        }
        *self = next;
        Ok(Change {
            field,
            value,
            clamped,
            previous,
        })
    }

    /// Removes the override of the field with key `key`; returns the value it had. Refused for an
    /// unknown key, and for a removal that would break a rule with `base` and the other overrides
    /// (for example lowering the Optional gate below an overridden Standard gate).
    pub fn unset(&mut self, base: &Config, key: &str) -> Result<Option<Value>, Note> {
        let (index, field) = field(key).ok_or_else(|| Note::UnknownKey {
            key: key.chars().take(60).collect(),
        })?;
        let mut next = *self;
        let previous = next.values.get_mut(index).and_then(Option::take);
        if let Some(rule) = broken(&next.apply(base), Some(field.key)) {
            return Err(Note::Conflict {
                key: field.key,
                rule: rule.text,
            });
        }
        *self = next;
        Ok(previous)
    }

    /// Removes every override (the profile's values always satisfy every rule).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Appends the record's payload: one `key=value\n` line per override, in field order.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut text = String::new();
        for (f, v) in self.iter() {
            let _ = writeln!(text, "{}={v}", f.key);
        }
        out.extend_from_slice(text.as_bytes());
    }

    /// Reads a record's payload against `base`. Every line that is refused, clamped or dropped adds a
    /// note; the rest are kept, and the result satisfies every rule.
    pub fn decode(payload: &[u8], base: &Config, notes: &mut Vec<Note>) -> Self {
        let mut out = Self::default();
        let Ok(text) = core::str::from_utf8(payload) else {
            notes.push(Note::NotText);
            return out;
        };
        for (line, raw) in (1u32..).zip(text.split('\n')) {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let Some((key, value)) = raw.split_once('=') else {
                notes.push(Note::BadLine { line });
                continue;
            };
            let key = key.trim();
            let Some((index, field)) = field(key) else {
                notes.push(Note::UnknownKey {
                    key: key.chars().take(60).collect(),
                });
                continue;
            };
            if !field.runtime {
                notes.push(Note::NotRuntime { key: field.key });
                continue;
            }
            let Some(slot) = out.values.get_mut(index) else {
                continue;
            };
            if slot.is_some() {
                notes.push(Note::Duplicate { key: field.key });
                continue;
            }
            match parse(field, value) {
                Ok((v, clamped)) => {
                    if let (Some(requested), Value::Number(used)) = (clamped, v) {
                        notes.push(Note::Clamped {
                            key: field.key,
                            requested,
                            used,
                        });
                    }
                    *slot = Some(v);
                }
                Err(note) => notes.push(note),
            }
        }
        // Drop the overrides of every broken rule until none is broken. Each round drops at least
        // one override, and with none left the profile's values hold every rule.
        while let Some(rule) = broken(&out.apply(base), None) {
            let mut dropped = false;
            for key in rule.keys {
                if let Some((i, f)) = field(key)
                    && out.values.get_mut(i).and_then(Option::take).is_some()
                {
                    dropped = true;
                    notes.push(Note::Conflict {
                        key: f.key,
                        rule: rule.text,
                    });
                }
            }
            if !dropped {
                // A profile that breaks a rule (the config tests rule it out): keep what is left.
                break;
            }
        }
        out
    }
}

/// The cache toggles whose `false` override is the naive baseline (ROADMAP evidence conventions);
/// the naive guard removes these overrides (ADR-022).
pub const NAIVE_KEYS: [&str; 3] = [
    "cache.structure_static",
    "cache.movement_grid",
    "cache.flow_fields",
];

/// Where the naive baseline stands (ADR-022).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Naive {
    /// No override turns a cache off.
    Off,
    /// A console change turned a cache off; the span starts with the next loop that runs with it.
    Pending,
    /// The span's first loop was on tick `since`. For overrides a record brought into this global,
    /// the tick that record was last written: normally the previous global's first naive loop (a
    /// tick or so later if the save cap or a failed write held it back), but a later change written
    /// during the span moves it (then the span ends later than `CacheConfig::naive_max_ticks` after
    /// its true start). The loops between a global's start and the record's load run cached.
    Since(u32),
}

/// Why the guard ended a naive span (ADR-022).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NaiveEnd {
    /// It lasted `CacheConfig::naive_max_ticks`.
    Length,
    /// The bucket at the start of the tick (carried here) was below `CacheConfig::naive_stop_bucket`.
    Bucket(u32),
}

/// A naive span the guard ended: every cache is on from the next loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NaiveStop {
    /// The span's first tick ([`Naive::Since`]).
    pub since: u32,
    /// Its last tick: the loop that ended it.
    pub last: u32,
    /// Its length, `last - since + 1`. Unit: ticks.
    pub ticks: u32,
    pub why: NaiveEnd,
    /// The cache overrides removed (those that turned a cache off).
    pub removed: usize,
}

/// Where the stored record stands in this global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    /// Not loaded yet. Console changes wait, so that they cannot overwrite a record not yet read.
    Loading,
    /// The segment holds no record (never written, or cleared): no overrides.
    Empty,
    /// The record written on tick `written` was read.
    Loaded { written: u32 },
    /// This global wrote the record on tick `written` (after a console change).
    Written { written: u32 },
    /// The load failed or the record was refused: the profile's values apply, and the next console
    /// change writes a new record over it.
    Failed(LoadError),
}

/// Why a console change was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The stored record has not loaded yet (it loads with the Standard tier, a tick or two after a
    /// global starts).
    Loading,
    /// The change itself is invalid.
    Invalid(Note),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loading => f.write_str(
                "the stored overrides have not loaded yet (they load with the Standard tier); try again in a tick or two",
            ),
            Self::Invalid(note) => write!(f, "{note}"),
        }
    }
}

/// The active profile's values, the overrides and the config in force.
#[derive(Clone, Debug)]
pub struct Store {
    base: Config,
    overrides: Overrides,
    config: Config,
    record: Record,
    naive: Naive,
}

impl Store {
    /// A new global: the profile's values, until the record loads.
    pub fn new(base: Config) -> Self {
        Self {
            base,
            overrides: Overrides::default(),
            config: base,
            record: Record::Loading,
            naive: Naive::Off,
        }
    }

    /// The config in force.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The profile's values.
    pub fn base(&self) -> &Config {
        &self.base
    }

    pub fn overrides(&self) -> &Overrides {
        &self.overrides
    }

    pub fn record(&self) -> Record {
        self.record
    }

    /// Where the naive baseline stands.
    pub fn naive(&self) -> Naive {
        self.naive
    }

    /// Whether an override turns a cache off.
    fn naive_overridden(&self) -> bool {
        NAIVE_KEYS
            .iter()
            .any(|k| self.overrides.get(k) == Some(Value::Flag(false)))
    }

    /// After a change to the overrides: off if no override turns a cache off, else `start` for a
    /// span that was off, and an existing span goes on (turning a second cache off keeps its start).
    fn naive_changed(&mut self, start: Naive) {
        self.naive = if !self.naive_overridden() {
            Naive::Off
        } else if self.naive == Naive::Off {
            start
        } else {
            self.naive
        };
    }

    /// The naive guard, once per loop after the act step (ADR-022). `ran` says the loop ran with a
    /// cache off (`CacheConfig::naive` of the config it started with), so a loop on the profile's
    /// values (the one that loaded a record, say) neither starts nor ends a span (its tick still
    /// counts toward a running span's length, which is in ticks). A pending span starts on this
    /// tick; a start later than `now` (a record from another clock) becomes `now`. When the span
    /// has lasted `CacheConfig::naive_max_ticks`, or the bucket is below
    /// `CacheConfig::naive_stop_bucket`, the overrides that turn a cache off are removed (every
    /// cache is on from the next loop) and the stop is returned: the caller logs it and writes the
    /// record. Only the length is reported when both hold. Other overrides stay.
    #[inline]
    pub fn naive_tick(&mut self, now: u32, bucket: u32, ran: bool) -> Option<NaiveStop> {
        if !ran {
            return None;
        }
        let since = match self.naive {
            Naive::Off => return None,
            Naive::Pending => now,
            Naive::Since(s) => s.min(now),
        };
        self.naive = Naive::Since(since);
        let ticks = now.saturating_sub(since).saturating_add(1);
        let cache = &self.config.cache;
        let why = if ticks >= cache.naive_max_ticks {
            NaiveEnd::Length
        } else if bucket < cache.naive_stop_bucket {
            NaiveEnd::Bucket(bucket)
        } else {
            return None;
        };
        Some(self.naive_end(since, now, ticks, why))
    }

    /// The guard's rare part, kept out of the per-tick `loop` function: removes the overrides that
    /// turn a cache off (no rule names a cache key, so a removal is never refused; a test checks
    /// it) and reapplies the rest.
    #[cold]
    #[inline(never)]
    fn naive_end(&mut self, since: u32, now: u32, ticks: u32, why: NaiveEnd) -> NaiveStop {
        let mut removed = 0;
        for key in NAIVE_KEYS {
            if self.overrides.get(key) == Some(Value::Flag(false))
                && self.overrides.unset(&self.base, key).is_ok()
            {
                removed += 1;
            }
        }
        self.config = self.overrides.apply(&self.base);
        self.naive_changed(Naive::Off);
        NaiveStop {
            since,
            last: now,
            ticks,
            why,
            removed,
        }
    }

    /// The record's load finished: its frame, or why there is none. Notes about refused or clamped
    /// lines go to `notes`. Returns whether the segment held a usable record or none at all (an
    /// empty segment), for the segment manager's counters. Ignored unless the store is loading.
    pub fn loaded(&mut self, frame: Result<Frame<'_>, LoadError>, notes: &mut Vec<Note>) -> bool {
        if self.record != Record::Loading {
            return true;
        }
        let frame = frame.and_then(|f| {
            f.header
                .expect(kind::CONFIG, SCHEMA)
                .map_err(LoadError::Decode)
                .map(|()| f)
        });
        match frame {
            Ok(f) => {
                self.overrides = Overrides::decode(f.payload, &self.base, notes);
                self.config = self.overrides.apply(&self.base);
                self.record = Record::Loaded {
                    written: f.header.written,
                };
                self.naive_changed(Naive::Since(f.header.written));
                true
            }
            Err(LoadError::Decode(crate::persist::DecodeError::Empty)) => {
                self.record = Record::Empty;
                true
            }
            Err(e) => {
                self.record = Record::Failed(e);
                false
            }
        }
    }

    /// The record was written to the segment on tick `now`. Ignored while loading (nothing writes
    /// then: changes wait for the load).
    pub fn written(&mut self, now: u32) {
        if self.record != Record::Loading {
            self.record = Record::Written { written: now };
        }
    }

    fn editable(&self) -> Result<(), Refusal> {
        if self.record == Record::Loading {
            Err(Refusal::Loading)
        } else {
            Ok(())
        }
    }

    /// Overrides `key` with `text` (see [`Overrides::set`]). The caller writes the record after an
    /// accepted change.
    pub fn set(&mut self, key: &str, text: &str) -> Result<Change, Refusal> {
        self.editable()?;
        let change = self
            .overrides
            .set(&self.base, key, text)
            .map_err(Refusal::Invalid)?;
        self.config = self.overrides.apply(&self.base);
        self.naive_changed(Naive::Pending);
        Ok(change)
    }

    /// Removes the override of `key` (see [`Overrides::unset`]); returns the value it had.
    pub fn unset(&mut self, key: &str) -> Result<Option<Value>, Refusal> {
        self.editable()?;
        let previous = self
            .overrides
            .unset(&self.base, key)
            .map_err(Refusal::Invalid)?;
        self.config = self.overrides.apply(&self.base);
        self.naive_changed(Naive::Pending);
        Ok(previous)
    }

    /// Removes every override; returns how many there were.
    pub fn reset(&mut self) -> Result<usize, Refusal> {
        self.editable()?;
        let n = self.overrides.len();
        self.overrides.clear();
        self.config = self.base;
        self.naive = Naive::Off;
        Ok(n)
    }

    /// Appends the record's payload ([`Overrides::encode`]).
    pub fn payload(&self, out: &mut Vec<u8>) {
        self.overrides.encode(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MMO, RULES};
    use crate::persist::{DecodeError, frame};

    const BASE: Config = MMO.config;

    fn by_key(key: &str) -> &'static Field {
        field(key).map(|(_, f)| f).expect("known key")
    }

    fn range(key: &str) -> (u32, u32) {
        match by_key(key).kind {
            Kind::Number { min, max } => (min, max),
            _ => panic!("{key} is not a number"),
        }
    }

    fn base_number(key: &str) -> u32 {
        match (by_key(key).get)(&BASE) {
            Value::Number(n) => n,
            v => panic!("{key} is {v:?}"),
        }
    }

    /// A value of `f`'s kind inside its range that differs from the profile's, or None.
    fn other_value(f: &Field) -> Option<Value> {
        match ((f.get)(&BASE), f.kind) {
            (Value::Number(n), Kind::Number { min, max }) => {
                if n < max {
                    Some(Value::Number(n + 1))
                } else if n > min {
                    Some(Value::Number(n - 1))
                } else {
                    None
                }
            }
            (Value::Flag(b), _) => Some(Value::Flag(!b)),
            (Value::Level(l), _) => Some(Value::Level(if l == LogLevel::Trace {
                LogLevel::Warn
            } else {
                LogLevel::Trace
            })),
            _ => None,
        }
    }

    #[test]
    fn numbers_parse_strictly_and_saturate() {
        assert_eq!(parse_number("12"), Some(12));
        assert_eq!(parse_number("007"), Some(7));
        assert_eq!(parse_number("-5"), Some(-5));
        assert_eq!(parse_number("99999999999999999999999"), Some(i64::MAX));
        assert_eq!(parse_number("-99999999999999999999999"), Some(-i64::MAX));
        for bad in ["", "-", "+5", "1.5", "1e3", "0x10", "12a", "١٢"] {
            assert_eq!(parse_number(bad), None, "{bad}");
        }
    }

    /// Whether `o` with `key` set to `value` breaks a rule that involves `key`: the only reason a
    /// valid value may be refused.
    fn breaks_rule(o: &Overrides, key: &'static str, value: Value) -> bool {
        let mut c = o.apply(&BASE);
        assert!((by_key(key).set)(&mut c, value), "{key}");
        RULES
            .iter()
            .any(|r| r.keys.contains(&key) && !(r.holds)(&c))
    }

    /// Every runtime field accepts a value of its kind, clamps numbers outside its range to the
    /// nearest bound with a note of the number asked for (refused only when that bound breaks a
    /// rule), and refuses text of another kind.
    #[test]
    fn set_clamps_numbers_and_refuses_bad_values() {
        for f in FIELDS.iter().filter(|f| f.runtime) {
            let mut o = Overrides::default();
            match f.kind {
                Kind::Number { min, max } => {
                    let c = o
                        .set(&BASE, f.key, &(f.get)(&BASE).to_string())
                        .expect("profile value");
                    assert_eq!(
                        (c.value, c.clamped, c.previous),
                        ((f.get)(&BASE), None, None)
                    );
                    // Outside the range: clamped to the nearest bound, refused only when that bound
                    // breaks a rule with the profile's values.
                    for (asked, bound) in [(i64::from(max) + 1, max), (i64::from(min) - 1, min)] {
                        let conflict = breaks_rule(&o, f.key, Value::Number(bound));
                        match o.set(&BASE, f.key, &asked.to_string()) {
                            Ok(c) if !conflict => {
                                assert_eq!(
                                    (c.value, c.clamped),
                                    (Value::Number(bound), Some(asked))
                                );
                            }
                            Err(Note::Conflict { key, .. }) if conflict => assert_eq!(key, f.key),
                            r => panic!("{} {asked}: {r:?} (a rule breaks: {conflict})", f.key),
                        }
                    }
                    for bad in ["", "abc", "1.5", "true"] {
                        let before = o;
                        assert!(
                            matches!(o.set(&BASE, f.key, bad), Err(Note::BadValue { key, .. }) if key == f.key),
                            "{} {bad}",
                            f.key
                        );
                        assert_eq!(o, before);
                    }
                }
                Kind::Flag => {
                    assert_eq!(
                        o.set(&BASE, f.key, "false").map(|c| c.value),
                        Ok(Value::Flag(false))
                    );
                    assert_eq!(
                        o.set(&BASE, f.key, " true ").map(|c| c.value),
                        Ok(Value::Flag(true))
                    );
                    for bad in ["1", "yes", "True", ""] {
                        assert!(
                            matches!(o.set(&BASE, f.key, bad), Err(Note::BadValue { .. })),
                            "{bad}"
                        );
                    }
                }
                Kind::Level => {
                    for l in LogLevel::ALL {
                        assert_eq!(
                            o.set(&BASE, f.key, l.as_str()).map(|c| c.value),
                            Ok(Value::Level(l))
                        );
                    }
                    for bad in ["off", "error", "INFO", "warning", ""] {
                        assert!(
                            matches!(o.set(&BASE, f.key, bad), Err(Note::BadValue { .. })),
                            "{bad}"
                        );
                    }
                }
                Kind::Recovery => panic!("{} is a runtime recovery field", f.key),
            }
        }
    }

    #[test]
    fn unknown_and_profile_only_keys_are_refused() {
        let mut o = Overrides::default();
        assert!(matches!(
            o.set(&BASE, "scheduler.nope", "1"),
            Err(Note::UnknownKey { .. })
        ));
        for f in FIELDS.iter().filter(|f| !f.runtime) {
            let text = (f.get)(&BASE).to_string();
            assert_eq!(
                o.set(&BASE, f.key, &text),
                Err(Note::NotRuntime { key: f.key })
            );
        }
        assert!(o.is_empty());
    }

    /// Uses: rustedscreep_consts::CPU_TICK_LIMIT_MAX, through the rule on the Standard gate. A
    /// change that breaks a rule with the profile's values or the other overrides is refused and
    /// changes nothing; the same values in an order that keeps every rule are accepted.
    #[test]
    fn set_refuses_changes_that_break_a_rule() {
        let standard = base_number("scheduler.standard_bucket_min");
        let optional = base_number("scheduler.optional_bucket_min");
        let target = base_number("scheduler.bucket_target");
        let boot = base_number("loader.boot_bucket_min");
        let mut o = Overrides::default();
        // Above the Optional gate, below the boot gate's floor: both refused.
        for bad in [
            optional + 1,
            boot + rustedscreep_consts::CPU_TICK_LIMIT_MAX - 1,
        ] {
            assert!(
                matches!(
                    o.set(&BASE, "scheduler.standard_bucket_min", &bad.to_string()),
                    Err(Note::Conflict { .. })
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            o.set(
                &BASE,
                "scheduler.bucket_target",
                &(optional - 1).to_string()
            ),
            Err(Note::Conflict { .. })
        ));
        assert!(o.is_empty());
        // Raising all three in the order target, optional, standard keeps every rule. The Standard
        // gate ends above the profile's Optional gate.
        let new_standard = optional + 500;
        let new_optional = new_standard + 500;
        let new_target = target.max(new_optional);
        for (key, v) in [
            ("scheduler.bucket_target", new_target),
            ("scheduler.optional_bucket_min", new_optional),
            ("scheduler.standard_bucket_min", new_standard),
        ] {
            o.set(&BASE, key, &v.to_string()).expect(key);
        }
        let c = o.apply(&BASE);
        assert_eq!(
            (
                c.scheduler.standard_bucket_min,
                c.scheduler.optional_bucket_min,
                c.scheduler.bucket_target
            ),
            (new_standard, new_optional, new_target)
        );
        assert!(standard < new_standard);
        // Removing the raised Optional gate would put it below the raised Standard gate.
        let before = o;
        assert!(matches!(
            o.unset(&BASE, "scheduler.optional_bucket_min"),
            Err(Note::Conflict { .. })
        ));
        assert_eq!(o, before);
        assert_eq!(
            o.unset(&BASE, "scheduler.standard_bucket_min"),
            Ok(Some(Value::Number(new_standard)))
        );
        assert_eq!(o.unset(&BASE, "scheduler.standard_bucket_min"), Ok(None));
        assert!(matches!(
            o.unset(&BASE, "nope"),
            Err(Note::UnknownKey { .. })
        ));
    }

    /// Every accepted set of overrides round-trips through the payload, and the decoded overrides
    /// apply to the same config.
    #[test]
    fn payload_round_trips() {
        let mut o = Overrides::default();
        let mut empty = Vec::new();
        o.encode(&mut empty);
        assert!(empty.is_empty());
        let mut notes = Vec::new();
        assert_eq!(Overrides::decode(&empty, &BASE, &mut notes), o);
        // Each change is refused exactly when it breaks a rule with the overrides so far.
        let mut accepted = 0;
        for f in FIELDS.iter().filter(|f| f.runtime) {
            if let Some(v) = other_value(f) {
                let conflict = breaks_rule(&o, f.key, v);
                match o.set(&BASE, f.key, &v.to_string()) {
                    Ok(c) if !conflict => {
                        assert_eq!(c.value, v, "{}", f.key);
                        accepted += 1;
                    }
                    Err(Note::Conflict { .. }) if conflict => {}
                    r => panic!("{}: {r:?} (a rule breaks: {conflict})", f.key),
                }
            }
        }
        assert_eq!(o.len(), accepted);
        assert!(accepted > 0);
        let mut payload = Vec::new();
        o.encode(&mut payload);
        let text = core::str::from_utf8(&payload).expect("ascii");
        assert!(text.is_ascii() && text.ends_with('\n'));
        assert_eq!(text.lines().count(), o.len());
        let decoded = Overrides::decode(&payload, &BASE, &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(decoded, o);
        assert_eq!(decoded.apply(&BASE), o.apply(&BASE));
    }

    /// A record read on the next global is checked like a console change: unknown keys, profile-only
    /// fields, bad values, duplicates and bad lines are dropped with a note; numbers are clamped.
    #[test]
    fn decode_drops_what_it_cannot_trust() {
        let (_, reserve_max) = range("scheduler.tick_reserve_pct");
        let text = "scheduler.tick_reserve_pct=999\n\
             loader.boot_bucket_min=10\n\
             scheduler.removed_field=3\n\
             log.level=loud\n\
             no equals sign\n\
             \n\
             cache.structure_static=false\n\
             cache.structure_static=true\n\
             log.level = debug \r\n";
        let mut notes = Vec::new();
        let o = Overrides::decode(text.as_bytes(), &BASE, &mut notes);
        assert_eq!(
            o.get("scheduler.tick_reserve_pct"),
            Some(Value::Number(reserve_max))
        );
        assert_eq!(o.get("cache.structure_static"), Some(Value::Flag(false)));
        assert_eq!(o.get("loader.boot_bucket_min"), None);
        // `log.level=loud` was refused, so the later valid line counts.
        assert_eq!(o.get("log.level"), Some(Value::Level(LogLevel::Debug)));
        assert_eq!(o.len(), 3);
        assert_eq!(
            notes,
            vec![
                Note::Clamped {
                    key: "scheduler.tick_reserve_pct",
                    requested: 999,
                    used: reserve_max
                },
                Note::NotRuntime {
                    key: "loader.boot_bucket_min"
                },
                Note::UnknownKey {
                    key: String::from("scheduler.removed_field")
                },
                Note::BadValue {
                    key: "log.level",
                    value: String::from("loud"),
                    expected: Kind::Level
                },
                Note::BadLine { line: 5 },
                Note::Duplicate {
                    key: "cache.structure_static"
                },
            ]
        );
        let mut notes = Vec::new();
        assert!(Overrides::decode(&[0xff, b'\n'], &BASE, &mut notes).is_empty());
        assert_eq!(notes, vec![Note::NotText]);
    }

    /// A record whose overrides break a rule together (from an older build, or ranges that changed)
    /// loses every override in the broken rule; the rest stay, and the result satisfies every rule.
    #[test]
    fn decode_drops_the_overrides_of_a_broken_rule() {
        let optional = base_number("scheduler.optional_bucket_min");
        let text = format!(
            "scheduler.standard_bucket_min={}\nscheduler.optional_bucket_min={}\nscheduler.tick_reserve_pct=12\n",
            optional + 200,
            optional + 100
        );
        let mut notes = Vec::new();
        let o = Overrides::decode(text.as_bytes(), &BASE, &mut notes);
        assert_eq!(o.len(), 1);
        assert_eq!(o.get("scheduler.tick_reserve_pct"), Some(Value::Number(12)));
        assert_eq!(notes.len(), 2);
        assert!(
            notes.iter().all(|n| matches!(n, Note::Conflict { .. })),
            "{notes:?}"
        );
        let c = o.apply(&BASE);
        assert!(RULES.iter().all(|r| (r.holds)(&c)));
    }

    /// A seeded walk of random console changes: the overrides always satisfy every rule, keep every
    /// value inside its range, and survive the payload unchanged.
    #[test]
    fn random_changes_stay_coherent() {
        let runtime: Vec<&Field> = FIELDS.iter().filter(|f| f.runtime).collect();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut o = Overrides::default();
        for _ in 0..20_000 {
            let f = runtime[(next() % runtime.len() as u64) as usize];
            let r = next();
            if r % 5 == 0 {
                let _ = o.unset(&BASE, f.key);
                continue;
            }
            let text = match f.kind {
                Kind::Number { min, max } => {
                    let span = i64::from(max) - i64::from(min) + 3;
                    (i64::from(min) - 1 + (r % span as u64) as i64).to_string()
                }
                Kind::Flag => (r % 2 == 0).to_string(),
                Kind::Level => LogLevel::ALL[(r % LogLevel::ALL.len() as u64) as usize]
                    .as_str()
                    .to_string(),
                Kind::Recovery => continue,
            };
            let _ = o.set(&BASE, f.key, &text);
            let c = o.apply(&BASE);
            assert!(RULES.iter().all(|rule| (rule.holds)(&c)), "{o:?}");
            assert!(FIELDS.iter().all(|f| f.accepts((f.get)(&c))));
        }
        let mut payload = Vec::new();
        o.encode(&mut payload);
        let mut notes = Vec::new();
        assert_eq!(Overrides::decode(&payload, &BASE, &mut notes), o);
        assert!(notes.is_empty());
    }

    fn frame_text(kind: u16, schema: u16, written: u32, payload: &[u8]) -> String {
        let mut text = String::new();
        frame::encode(kind, schema, written, payload, &mut text).expect("fits");
        text
    }

    fn load(store: &mut Store, text: &str, notes: &mut Vec<Note>) -> bool {
        let mut buf = Vec::new();
        let f = frame::decode(text, &mut buf).map_err(LoadError::Decode);
        store.loaded(f, notes)
    }

    /// The store refuses changes until the record has loaded; a loaded record sets the config; an
    /// empty segment means no overrides; a refused record leaves the profile's values and lets the
    /// console write a new one.
    #[test]
    fn store_follows_the_record() {
        let mut s = Store::new(BASE);
        assert_eq!(s.record(), Record::Loading);
        assert_eq!(
            s.set("scheduler.tick_reserve_pct", "12"),
            Err(Refusal::Loading)
        );
        assert_eq!(s.unset("scheduler.tick_reserve_pct"), Err(Refusal::Loading));
        assert_eq!(s.reset(), Err(Refusal::Loading));
        let mut notes = Vec::new();
        let text = frame_text(kind::CONFIG, SCHEMA, 77, b"scheduler.tick_reserve_pct=12\n");
        assert!(load(&mut s, &text, &mut notes));
        assert_eq!(s.record(), Record::Loaded { written: 77 });
        assert_eq!(s.config().scheduler.tick_reserve_pct, 12);
        assert_eq!(*s.base(), BASE);
        // A second delivery cannot replace what the console may have changed since.
        assert!(load(
            &mut s,
            &frame_text(kind::CONFIG, SCHEMA, 78, b""),
            &mut notes
        ));
        assert_eq!(s.config().scheduler.tick_reserve_pct, 12);
        let c = s.set("cache.structure_static", "false").expect("set");
        assert_eq!(c.previous, None);
        assert!(!s.config().cache.structure_static);
        let mut payload = Vec::new();
        s.payload(&mut payload);
        assert_eq!(
            payload,
            b"scheduler.tick_reserve_pct=12\ncache.structure_static=false\n"
        );
        s.written(90);
        assert_eq!(s.record(), Record::Written { written: 90 });
        assert_eq!(
            s.unset("cache.structure_static"),
            Ok(Some(Value::Flag(false)))
        );
        assert_eq!(s.reset(), Ok(1));
        assert_eq!(*s.config(), BASE);
        assert!(notes.is_empty());

        let mut empty = Store::new(BASE);
        let mut buf = Vec::new();
        assert!(empty.loaded(
            frame::decode("", &mut buf).map_err(LoadError::Decode),
            &mut notes
        ));
        assert_eq!(empty.record(), Record::Empty);
        assert_eq!(*empty.config(), BASE);

        for (text, why) in [
            (
                frame_text(kind::SELF_TEST, SCHEMA, 5, b"x"),
                "another owner's record",
            ),
            (
                frame_text(kind::CONFIG, SCHEMA + 1, 5, b""),
                "another schema",
            ),
            (String::from("{\"v\":1}"), "foreign text"),
        ] {
            let mut s = Store::new(BASE);
            assert!(!load(&mut s, &text, &mut notes), "{why}");
            assert!(
                matches!(s.record(), Record::Failed(LoadError::Decode(_))),
                "{why}"
            );
            assert_eq!(*s.config(), BASE, "{why}");
            assert!(s.set("scheduler.tick_reserve_pct", "11").is_ok(), "{why}");
        }
        let mut gone = Store::new(BASE);
        gone.written(5);
        assert_eq!(
            gone.record(),
            Record::Loading,
            "nothing writes before the load"
        );
        assert!(!gone.loaded(Err(LoadError::Unavailable { tries: 3 }), &mut notes));
        assert_eq!(
            gone.record(),
            Record::Failed(LoadError::Unavailable { tries: 3 })
        );
        assert!(Store::new(BASE).loaded(Err(LoadError::Decode(DecodeError::Empty)), &mut notes));
    }

    /// A store whose record loaded empty (no overrides), so console changes are accepted.
    fn ready_store() -> Store {
        let mut s = Store::new(BASE);
        let mut notes = Vec::new();
        assert!(s.loaded(Err(LoadError::Decode(DecodeError::Empty)), &mut notes));
        s
    }

    fn naive_off(s: &mut Store, keys: &[&str]) {
        for key in keys {
            assert!(s.set(key, "false").is_ok(), "{key}");
        }
    }

    /// Uses: CACHE_MMO.naive_max_ticks, CACHE_MMO.naive_stop_bucket (ADR-022). A span turned on by
    /// the console starts on the first loop that runs it and ends on its `naive_max_ticks`-th tick
    /// (a bucket at the stop level does not stop it); the three cache overrides go, every other one
    /// stays, and the next loop starts no new span.
    #[test]
    fn naive_span_ends_after_its_length() {
        let max = BASE.cache.naive_max_ticks;
        let stop = BASE.cache.naive_stop_bucket;
        let mut s = ready_store();
        assert!(s.set("scheduler.tick_reserve_pct", "12").is_ok());
        assert_eq!(s.naive(), Naive::Off);
        naive_off(&mut s, &NAIVE_KEYS);
        assert_eq!(s.naive(), Naive::Pending);
        assert!(s.config().cache.naive());
        let t0 = 84_302_207;
        for t in t0..t0 + max - 1 {
            assert_eq!(s.naive_tick(t, stop, true), None, "tick {t}");
            assert_eq!(s.naive(), Naive::Since(t0));
        }
        let last = t0 + max - 1;
        assert_eq!(
            s.naive_tick(last, stop, true),
            Some(NaiveStop {
                since: t0,
                last,
                ticks: max,
                why: NaiveEnd::Length,
                removed: NAIVE_KEYS.len(),
            })
        );
        assert_eq!(s.naive(), Naive::Off);
        assert!(!s.config().cache.naive());
        assert_eq!(s.overrides().len(), 1);
        assert_eq!(s.config().scheduler.tick_reserve_pct, 12);
        assert_eq!(s.naive_tick(last + 1, stop, false), None);
        assert_eq!(s.naive_tick(last + 1, stop, true), None);
    }

    /// Uses: CACHE_MMO.naive_max_ticks, CACHE_MMO.naive_stop_bucket. A bucket below the stop
    /// bucket ends the span at once with the bucket in the stop; a span at its length with a low
    /// bucket reports its length.
    #[test]
    fn naive_span_ends_below_the_stop_bucket() {
        let max = BASE.cache.naive_max_ticks;
        let stop = BASE.cache.naive_stop_bucket;
        assert!(stop > 0 && max > 2);
        let mut s = ready_store();
        naive_off(&mut s, &["cache.movement_grid"]);
        assert_eq!(s.naive_tick(500, stop, true), None);
        assert_eq!(
            s.naive_tick(501, stop - 1, true),
            Some(NaiveStop {
                since: 500,
                last: 501,
                ticks: 2,
                why: NaiveEnd::Bucket(stop - 1),
                removed: 1,
            })
        );
        assert!(!s.config().cache.naive());
        assert!(s.overrides().is_empty());

        naive_off(&mut s, &["cache.flow_fields"]);
        assert_eq!(s.naive_tick(1000, stop, true), None);
        let last = 1000 + max - 1;
        assert_eq!(
            s.naive_tick(last, 0, true).map(|x| (x.why, x.ticks)),
            Some((NaiveEnd::Length, max))
        );
    }

    /// A pending span waits for a loop that ran with a cache off; turning a second cache off keeps
    /// the span's start, removing one of two keeps it, removing the last (or a reset) ends it; a
    /// `true` override is not naive.
    #[test]
    fn naive_span_follows_the_overrides() {
        let stop = BASE.cache.naive_stop_bucket;
        let mut s = ready_store();
        naive_off(&mut s, &["cache.structure_static"]);
        assert_eq!(s.naive_tick(10, stop, false), None);
        assert_eq!(s.naive(), Naive::Pending);
        assert_eq!(s.naive_tick(11, stop, true), None);
        assert_eq!(s.naive(), Naive::Since(11));
        naive_off(&mut s, &["cache.flow_fields", "cache.structure_static"]);
        assert_eq!(s.naive(), Naive::Since(11));
        assert!(s.unset("cache.structure_static").is_ok());
        assert_eq!(s.naive(), Naive::Since(11));
        assert!(s.unset("cache.flow_fields").is_ok());
        assert_eq!(s.naive(), Naive::Off);
        assert_eq!(s.naive_tick(12, stop, true), None);

        assert!(s.set("cache.movement_grid", "true").is_ok());
        assert_eq!(s.naive(), Naive::Off);
        naive_off(&mut s, &["cache.movement_grid"]);
        assert_eq!(s.naive(), Naive::Pending);
        assert_eq!(s.reset(), Ok(1));
        assert_eq!(s.naive(), Naive::Off);
        assert_eq!(s.naive_tick(13, 0, true), None);
    }

    /// Uses: CACHE_MMO.naive_max_ticks, CACHE_MMO.naive_stop_bucket. Overrides that a record brings
    /// count from the record's write tick, so a span across a global reset still ends
    /// `naive_max_ticks` after it began; the loop that loaded the record (on the profile's values)
    /// counts nothing; a write tick later than now counts from now; a record that turns no cache off
    /// starts nothing.
    #[test]
    fn naive_span_from_a_record_counts_from_its_write() {
        let max = BASE.cache.naive_max_ticks;
        let stop = BASE.cache.naive_stop_bucket;
        let written = 70_000;
        let mut s = Store::new(BASE);
        let mut notes = Vec::new();
        let text = frame_text(kind::CONFIG, SCHEMA, written, b"cache.flow_fields=false\n");
        assert!(load(&mut s, &text, &mut notes));
        assert_eq!(s.naive(), Naive::Since(written));
        let resumed = written + max / 2;
        assert_eq!(s.naive_tick(resumed, stop, false), None);
        assert_eq!(s.naive_tick(resumed + 1, stop, true), None);
        let last = written + max - 1;
        assert_eq!(
            s.naive_tick(last, stop, true)
                .map(|x| (x.since, x.ticks, x.why)),
            Some((written, max, NaiveEnd::Length))
        );

        let mut ahead = Store::new(BASE);
        assert!(load(&mut ahead, &text, &mut notes));
        assert_eq!(ahead.naive_tick(written - 5, stop, true), None);
        assert_eq!(ahead.naive(), Naive::Since(written - 5));

        let mut late = Store::new(BASE);
        assert!(load(&mut late, &text, &mut notes));
        let stopped = late.naive_tick(u32::MAX, stop, true).expect("long over");
        assert_eq!((stopped.why, stopped.removed), (NaiveEnd::Length, 1));

        let mut plain = Store::new(BASE);
        let other = frame_text(kind::CONFIG, SCHEMA, written, b"cache.flow_fields=true\n");
        assert!(load(&mut plain, &other, &mut notes));
        assert_eq!(plain.naive(), Naive::Off);
        assert!(notes.is_empty());
    }

    /// The span's length and stop bucket are runtime keys: a shorter length ends the span sooner,
    /// and a lower stop bucket lets it run on at a bucket that would have ended it.
    #[test]
    fn naive_limits_are_overridable() {
        let stop = BASE.cache.naive_stop_bucket;
        let mut s = ready_store();
        let short = 3;
        assert!(s.set("cache.naive_max_ticks", &short.to_string()).is_ok());
        naive_off(&mut s, &NAIVE_KEYS);
        assert_eq!(s.naive_tick(20, stop, true), None);
        assert_eq!(s.naive_tick(21, stop, true), None);
        assert_eq!(s.naive_tick(22, stop, true).map(|x| x.ticks), Some(short));
        let lower = stop / 2;
        assert!(s.set("cache.naive_stop_bucket", &lower.to_string()).is_ok());
        naive_off(&mut s, &["cache.structure_static"]);
        assert_eq!(s.naive_tick(30, stop - 1, true), None);
        assert_eq!(
            s.naive_tick(31, lower - 1, true).map(|x| x.why),
            Some(NaiveEnd::Bucket(lower - 1))
        );
    }

    /// A loop on the profile's values (`ran` false) ends nothing even when a running span is past
    /// its length or below the stop bucket; the next loop that runs naive ends it.
    #[test]
    fn a_cached_loop_ends_no_span() {
        let max = BASE.cache.naive_max_ticks;
        let stop = BASE.cache.naive_stop_bucket;
        let written = 70_000;
        let mut s = Store::new(BASE);
        let mut notes = Vec::new();
        let text = frame_text(kind::CONFIG, SCHEMA, written, b"cache.flow_fields=false\n");
        assert!(load(&mut s, &text, &mut notes));
        let t = written + max + 10;
        assert_eq!(s.naive_tick(t, stop, false), None);
        assert_eq!(s.naive_tick(t, 0, false), None);
        assert_eq!(s.naive(), Naive::Since(written));
        assert_eq!(
            s.naive_tick(t + 1, stop, true).map(|x| (x.last, x.why)),
            Some((t + 1, NaiveEnd::Length))
        );
    }

    /// A stop removes only the overrides that turn a cache off: a `true` cache override beside a
    /// `false` one stays, and only the removed one is counted.
    #[test]
    fn a_stop_keeps_true_cache_overrides() {
        let stop = BASE.cache.naive_stop_bucket;
        let mut s = ready_store();
        assert!(s.set("cache.movement_grid", "true").is_ok());
        naive_off(&mut s, &["cache.flow_fields"]);
        assert_eq!(s.naive_tick(100, stop, true), None);
        let stopped = s.naive_tick(101, 0, true).expect("bucket stop");
        assert_eq!(stopped.removed, 1);
        assert_eq!(
            s.overrides().get("cache.movement_grid"),
            Some(Value::Flag(true))
        );
        assert_eq!(s.overrides().get("cache.flow_fields"), None);
    }

    /// NAIVE_KEYS are exactly the cache flags (so the guard and `CacheConfig::naive` cannot drift
    /// apart), each one off makes the config naive, and no rule names one (so removing it is never
    /// refused and a stop cannot repeat every tick).
    #[test]
    fn naive_keys_are_the_cache_flags() {
        let flags: Vec<&str> = FIELDS
            .iter()
            .filter(|f| f.key.starts_with("cache.") && matches!(f.kind, Kind::Flag))
            .map(|f| f.key)
            .collect();
        assert_eq!(flags, NAIVE_KEYS);
        for key in NAIVE_KEYS {
            let mut c = BASE;
            assert!((by_key(key).set)(&mut c, Value::Flag(false)), "{key}");
            assert!(c.cache.naive(), "{key}");
        }
        assert!(
            RULES
                .iter()
                .all(|r| r.keys.iter().all(|k| !NAIVE_KEYS.contains(k)))
        );
    }
}
