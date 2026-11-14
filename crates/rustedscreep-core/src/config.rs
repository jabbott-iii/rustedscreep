//! Tunable thresholds and the server profiles. Every threshold lives here and is mirrored field for
//! field in `docs/TUNING.md` (CLAUDE.md); the doc-sync test below fails when the two diverge.
//!
//! - Each subsystem's thresholds are one struct ([`LoaderConfig`], [`SchedulerConfig`], ...), and a
//!   [`Config`] holds one of each.
//! - A server [`Profile`] names a [`Config`], and [`PROFILES`] is the table (ROADMAP P1 module 10).
//!   The core track has [`MMO`]; the optional tracks add `private` (PS1) and `season` (SE2) as new
//!   rows, which changes no `mmo` value.
//! - [`FIELDS`] describes every field: its runtime key, its safe range (the clamp for runtime
//!   overrides and TUNING.md's safe-range column) and whether a runtime override may change it.
//!   [`RULES`] are the constraints between two fields that a range alone cannot state.
//! - [`overrides`]: runtime overrides from the console, validated and clamped, kept in a RawMemory
//!   segment across global resets.
//!
//! The loader's values are here too, although the loader is TypeScript: it runs before the wasm is
//! instantiated, so `npm run build` prints [`LOADER_MMO`] with the `loader_config` example and injects
//! the JSON into the bundle. They cannot be overridden at runtime.

pub mod overrides;

use core::fmt;

use rustedscreep_consts::CPU_TICK_LIMIT_MAX;

/// How the loader recovers after a trap or an aborted tick (ADR-003).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryMode {
    /// `Game.cpu.halt()` on the next tick: a global reset and a full recompile (ADR-003 option (a)).
    Halt,
    /// Re-instantiate from the cached `WebAssembly.Module` (ADR-003 option (b)), and halt after
    /// repeated failures. Off by default until `trap-test` shows it correct and cheaper.
    Reinstantiate,
}

impl RecoveryMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Halt => "halt",
            Self::Reinstantiate => "reinstantiate",
        }
    }
}

/// Loader thresholds; `docs/TUNING.md` gives each one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoaderConfig {
    /// Lowest bucket at which a boot (require, compile, instantiate) may start. Unit: CPU.
    pub boot_bucket_min: u32,
    /// Compile on one tick and instantiate on the next, so one tick never pays for both.
    pub boot_split_ticks: bool,
    /// Recovery after a trap or an aborted tick.
    pub recovery_mode: RecoveryMode,
    /// Incidents older than this many ticks do not count toward backoff. Unit: ticks.
    pub backoff_window_ticks: u32,
    /// Incidents within the window before the loader delays the next boot.
    pub backoff_incidents: u32,
    /// First boot delay once backoff starts; it doubles per further incident. Unit: ticks.
    pub backoff_base_ticks: u32,
    /// Cap on the boot delay, so the bot always retries without human action. Unit: ticks.
    pub backoff_max_ticks: u32,
    /// `groupInterval` passed to `Game.notify`. Unit: minutes.
    pub notify_group_minutes: u32,
    /// Most traps one `rustedscreep.trapTest(n)` console command may schedule.
    pub trap_test_max: u32,
}

/// Loader values for the official MMO.
pub const LOADER_MMO: LoaderConfig = LoaderConfig {
    boot_bucket_min: 1500,
    boot_split_ticks: true,
    recovery_mode: RecoveryMode::Halt,
    backoff_window_ticks: 1000,
    backoff_incidents: 3,
    backoff_base_ticks: 100,
    backoff_max_ticks: 3200,
    notify_group_minutes: 60,
    trap_test_max: 20,
};

impl LoaderConfig {
    /// The values as JSON with camelCase keys, for `npm run build` to inject into the bundle.
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                "{{\"bootBucketMin\":{},\"bootSplitTicks\":{},\"recoveryMode\":\"{}\",",
                "\"backoffWindowTicks\":{},\"backoffIncidents\":{},\"backoffBaseTicks\":{},",
                "\"backoffMaxTicks\":{},\"notifyGroupMinutes\":{},\"trapTestMax\":{}}}"
            ),
            self.boot_bucket_min,
            self.boot_split_ticks,
            self.recovery_mode.as_str(),
            self.backoff_window_ticks,
            self.backoff_incidents,
            self.backoff_base_ticks,
            self.backoff_max_ticks,
            self.notify_group_minutes,
            self.trap_test_max,
        )
    }
}

/// Scheduler thresholds (ROADMAP P1 module 6); `docs/TUNING.md` gives each one's rationale and safe
/// range, and `crate::scheduler::Budget::compute` states the formula they feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerConfig {
    /// Percent of `Game.cpu.tickLimit` that no tier may spend: it is left for work after the decide
    /// step (the act layer's calls, the flushes, the loader) and for the overshoot of the units run
    /// since the last clock sample. Unit: percent.
    pub tick_reserve_pct: u32,
    /// Lowest bucket at which the Standard tier runs. Unit: CPU.
    pub standard_bucket_min: u32,
    /// Lowest bucket at which the Optional tier runs. Unit: CPU.
    pub optional_bucket_min: u32,
    /// Bucket at which the Optional tier's cap equals `Game.cpu.limit`. Unit: CPU.
    pub bucket_target: u32,
    /// Bucket above the target raises the Optional cap by 1 CPU per this many CPU of surplus, and a
    /// bucket below it lowers the cap the same way, so the surplus (or deficit) closes over about
    /// this many ticks. Unit: ticks.
    pub surplus_spread_ticks: u32,
    /// Loops a new global (after a global reset or a re-instantiation) runs before the Optional tier
    /// may run. Unit: loops.
    pub optional_warmup_loops: u32,
    /// Most units (or steps) a time-sliced job runs between two clock samples. Unit: units.
    pub max_units_per_sample: u32,
    /// Bad clock readings (NaN, infinite or negative) in one tick after which the clock counts as
    /// broken and every tier, Critical included, stops for the rest of the tick. Unit: readings.
    pub bad_samples_to_break: u32,
    /// Most fault reports logged per window, across all jobs; further faults are counted, and the
    /// window's closing note names their jobs. Unit: reports.
    pub fault_reports_per_window: u32,
    /// Length of the fault-report window. Unit: ticks.
    pub fault_window_ticks: u32,
}

/// Scheduler values for the official MMO.
pub const SCHEDULER_MMO: SchedulerConfig = SchedulerConfig {
    tick_reserve_pct: 10,
    standard_bucket_min: 2000,
    optional_bucket_min: 3000,
    bucket_target: 9000,
    surplus_spread_ticks: 100,
    optional_warmup_loops: 1,
    max_units_per_sample: 16,
    bad_samples_to_break: 2,
    fault_reports_per_window: 10,
    fault_window_ticks: 100,
};

/// Profiler settings (ROADMAP P1 module 8), used only by a build with rustedscreep-wasm's `profiler`
/// feature; `docs/TUNING.md` gives each one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfilerConfig {
    /// Loops per statistics window; a global's first loop is kept apart. Unit: loops.
    pub window_loops: u32,
    /// Closed windows kept in the heap and written to the stats segment, the oldest dropped first.
    /// Unit: windows.
    pub history_windows: u32,
    /// The RawMemory segment the statistics are written to when a window closes (0 to 99,
    /// `rustedscreep_consts::extra::MEMORY_SEGMENT_COUNT`). Unit: segment id.
    pub stats_segment: u8,
}

/// Profiler values for the official MMO.
pub const PROFILER_MMO: ProfilerConfig = ProfilerConfig {
    window_loops: 1000,
    history_windows: 8,
    stats_segment: 99,
};

/// Segment manager settings (ROADMAP P1 module 9); `docs/TUNING.md` gives each one's rationale and
/// safe range. Writes and loads run only while the Standard tier does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersistConfig {
    /// Most segments the manager keeps active at once with `setActiveSegments`; the rest of the
    /// wanted loads follow as these complete. Unit: segments.
    pub max_active_segments: u8,
    /// Ticks a requested segment may be missing after its activation tick before the load fails and
    /// its owner rebuilds. Unit: ticks.
    pub load_retries: u8,
    /// Most segments written per tick. Unit: segments.
    pub max_writes_per_tick: u8,
    /// The segment the self-test (`rustedscreep.persistTest()`) writes and reads back (0 to 99,
    /// `rustedscreep_consts::extra::MEMORY_SEGMENT_COUNT`). Unit: segment id.
    pub self_test_segment: u8,
    /// The segment that keeps the runtime config overrides across global resets ([`overrides`];
    /// 0 to 99). Unit: segment id.
    pub config_segment: u8,
}

/// Segment manager values for the official MMO.
pub const PERSIST_MMO: PersistConfig = PersistConfig {
    max_active_segments: 8,
    load_retries: 3,
    max_writes_per_tick: 2,
    self_test_segment: 98,
    config_segment: 97,
};

/// How much the bot logs, least verbose first. rustedscreep-wasm maps it to `log::LevelFilter`
/// (`log` + `fern`, ADR-006). It starts at warn, with no "error" or "off": faults, failed JS calls
/// and refused records are warnings, and they always print (the rate limits bound them).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub const ALL: [Self; 4] = [Self::Warn, Self::Info, Self::Debug, Self::Trace];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// The level named `text` (lowercase, as [`Self::as_str`] prints it).
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == text)
    }
}

/// Logging settings (ROADMAP P1 module 10); `docs/TUNING.md` gives each one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogConfig {
    /// The most verbose level printed. Unit: level.
    pub level: LogLevel,
    /// The skipped-object log (`crate::snapshot::SkipLog`) prints at most one line per this many
    /// ticks, with the counts since the line before. Unit: ticks.
    pub skip_window_ticks: u32,
}

/// Logging values for the official MMO.
pub const LOG_MMO: LogConfig = LogConfig {
    level: LogLevel::Info,
    skip_window_ticks: 100,
};

/// Cache toggles. Each one off replaces its cache with a read every tick; all off is the naive
/// baseline that measures what the caches save (ROADMAP evidence conventions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheConfig {
    /// Keep each structure's type and position from the tick it is first seen (P1.5); off, the
    /// snapshot reads both every tick. Unit: flag.
    pub structure_static: bool,
}

/// Cache toggles for the official MMO: every cache on.
pub const CACHE_MMO: CacheConfig = CacheConfig {
    structure_static: true,
};

/// Every subsystem's thresholds: one server profile's values, or those values with the runtime
/// overrides applied ([`overrides::Store::config`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub loader: LoaderConfig,
    pub scheduler: SchedulerConfig,
    pub profiler: ProfilerConfig,
    pub persist: PersistConfig,
    pub log: LogConfig,
    pub cache: CacheConfig,
}

/// A server profile: a name and its values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: &'static str,
    pub config: Config,
}

/// The official MMO's profile (the core track's only one).
pub const MMO: Profile = Profile {
    name: "mmo",
    config: Config {
        loader: LOADER_MMO,
        scheduler: SCHEDULER_MMO,
        profiler: PROFILER_MMO,
        persist: PERSIST_MMO,
        log: LOG_MMO,
        cache: CACHE_MMO,
    },
};

/// Every profile, [`MMO`] first. TUNING.md's default column holds `MMO`'s values, and its profile
/// column names each other profile whose value differs (the doc-sync test checks both).
pub static PROFILES: [Profile; 1] = [MMO];

impl Profile {
    /// The profile named `name`.
    pub fn by_name(name: &str) -> Option<&'static Self> {
        PROFILES.iter().find(|p| p.name == name)
    }
}

/// A value as the field table reads and writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Number(u32),
    Flag(bool),
    Level(LogLevel),
    Recovery(RecoveryMode),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Flag(b) => f.write_str(if b { "true" } else { "false" }),
            Self::Level(l) => f.write_str(l.as_str()),
            Self::Recovery(r) => f.write_str(r.as_str()),
        }
    }
}

/// What a field holds, with its safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A whole number from `min` to `max` inclusive; a runtime override outside is clamped.
    Number { min: u32, max: u32 },
    /// `true` or `false`.
    Flag,
    /// Any [`LogLevel`] (warn to trace).
    Level,
    /// Any [`RecoveryMode`].
    Recovery,
}

impl fmt::Display for Kind {
    /// The safe range as TUNING.md's safe-range column starts: `5 to 50`, `true or false`, ...
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Number { min, max } => write!(f, "{min} to {max}"),
            Self::Flag => f.write_str("true or false"),
            Self::Level => f.write_str("warn, info, debug or trace"),
            Self::Recovery => f.write_str("halt or reinstantiate"),
        }
    }
}

/// One config field: how to find, check, read and change it.
pub struct Field {
    /// The runtime key, `group.field` (for example `scheduler.tick_reserve_pct`).
    pub key: &'static str,
    /// The field's name in TUNING.md (for example `SchedulerConfig::tick_reserve_pct`).
    pub tuning: &'static str,
    /// What it holds and its safe range.
    pub kind: Kind,
    /// Whether a runtime override may change it. The loader's values are built into the bundle; the
    /// profiler's and the segment ids are fixed for a global (a changed window length would split a
    /// statistics window, a changed id would orphan its segment).
    pub runtime: bool,
    /// Reads the field.
    pub get: fn(&Config) -> Value,
    /// Writes the field; false when the value is the wrong kind or does not fit the field's type
    /// (nothing is written then). It does not check the safe range.
    pub set: fn(&mut Config, Value) -> bool,
}

impl Field {
    /// Whether `value` is this field's kind and inside its safe range.
    pub fn accepts(&self, value: Value) -> bool {
        match (self.kind, value) {
            (Kind::Number { min, max }, Value::Number(n)) => (min..=max).contains(&n),
            (Kind::Flag, Value::Flag(_))
            | (Kind::Level, Value::Level(_))
            | (Kind::Recovery, Value::Recovery(_)) => true,
            _ => false,
        }
    }
}

/// The integer types config fields use, so one macro serves them all.
trait Num: Copy {
    fn widen(self) -> u32;
    fn narrow(n: u32) -> Option<Self>;
}

impl Num for u32 {
    fn widen(self) -> u32 {
        self
    }
    fn narrow(n: u32) -> Option<Self> {
        Some(n)
    }
}

impl Num for u8 {
    fn widen(self) -> u32 {
        u32::from(self)
    }
    fn narrow(n: u32) -> Option<Self> {
        u8::try_from(n).ok()
    }
}

macro_rules! number {
    ($group:ident, $strukt:literal, $name:ident, $min:expr, $max:expr, $runtime:expr) => {
        Field {
            key: concat!(stringify!($group), ".", stringify!($name)),
            tuning: concat!($strukt, "::", stringify!($name)),
            kind: Kind::Number {
                min: $min,
                max: $max,
            },
            runtime: $runtime,
            get: |c| Value::Number(Num::widen(c.$group.$name)),
            set: |c, v| match v {
                Value::Number(n) => match Num::narrow(n) {
                    Some(n) => {
                        c.$group.$name = n;
                        true
                    }
                    None => false,
                },
                _ => false,
            },
        }
    };
}

macro_rules! flag {
    ($group:ident, $strukt:literal, $name:ident, $runtime:expr) => {
        Field {
            key: concat!(stringify!($group), ".", stringify!($name)),
            tuning: concat!($strukt, "::", stringify!($name)),
            kind: Kind::Flag,
            runtime: $runtime,
            get: |c| Value::Flag(c.$group.$name),
            set: |c, v| match v {
                Value::Flag(b) => {
                    c.$group.$name = b;
                    true
                }
                _ => false,
            },
        }
    };
}

/// Fields in [`FIELDS`].
pub const FIELD_COUNT: usize = 30;

/// Every config field, in TUNING.md's order, with its safe range: the code constants that runtime
/// overrides are clamped to and that TUNING.md's safe-range column must start with. Where a range
/// also depends on another field, [`RULES`] states it.
///
/// Uses: rustedscreep_consts::{CPU_BUCKET_MAX (10000), extra::MEMORY_SEGMENT_COUNT (100),
/// MEMORY_SEGMENT_ACTIVE_LIMIT (10), extra::MEMORY_SEGMENT_SAVE_LIMIT (10)} for the bounds named
/// after them; the config tests check each one against its constant.
pub static FIELDS: [Field; FIELD_COUNT] = [
    number!(loader, "LoaderConfig", boot_bucket_min, 0, 10_000, false),
    flag!(loader, "LoaderConfig", boot_split_ticks, false),
    Field {
        key: "loader.recovery_mode",
        tuning: "LoaderConfig::recovery_mode",
        kind: Kind::Recovery,
        runtime: false,
        get: |c| Value::Recovery(c.loader.recovery_mode),
        set: |c, v| match v {
            Value::Recovery(r) => {
                c.loader.recovery_mode = r;
                true
            }
            _ => false,
        },
    },
    number!(
        loader,
        "LoaderConfig",
        backoff_window_ticks,
        100,
        20_000,
        false
    ),
    number!(loader, "LoaderConfig", backoff_incidents, 1, 20, false),
    number!(loader, "LoaderConfig", backoff_base_ticks, 1, 20_000, false),
    number!(loader, "LoaderConfig", backoff_max_ticks, 1, 20_000, false),
    number!(loader, "LoaderConfig", notify_group_minutes, 0, 1440, false),
    number!(loader, "LoaderConfig", trap_test_max, 10, 100, false),
    number!(scheduler, "SchedulerConfig", tick_reserve_pct, 5, 50, true),
    number!(
        scheduler,
        "SchedulerConfig",
        standard_bucket_min,
        CPU_TICK_LIMIT_MAX,
        10_000,
        true
    ),
    number!(
        scheduler,
        "SchedulerConfig",
        optional_bucket_min,
        CPU_TICK_LIMIT_MAX,
        10_000,
        true
    ),
    number!(
        scheduler,
        "SchedulerConfig",
        bucket_target,
        CPU_TICK_LIMIT_MAX,
        10_000,
        true
    ),
    number!(
        scheduler,
        "SchedulerConfig",
        surplus_spread_ticks,
        10,
        10_000,
        true
    ),
    number!(
        scheduler,
        "SchedulerConfig",
        optional_warmup_loops,
        1,
        100,
        true
    ),
    number!(
        scheduler,
        "SchedulerConfig",
        max_units_per_sample,
        1,
        256,
        true
    ),
    number!(
        scheduler,
        "SchedulerConfig",
        bad_samples_to_break,
        1,
        8,
        true
    ),
    number!(
        scheduler,
        "SchedulerConfig",
        fault_reports_per_window,
        1,
        100,
        true
    ),
    number!(
        scheduler,
        "SchedulerConfig",
        fault_window_ticks,
        10,
        10_000,
        true
    ),
    number!(
        profiler,
        "ProfilerConfig",
        window_loops,
        1000,
        10_000,
        false
    ),
    number!(profiler, "ProfilerConfig", history_windows, 1, 32, false),
    number!(profiler, "ProfilerConfig", stats_segment, 0, 99, false),
    number!(persist, "PersistConfig", max_active_segments, 1, 9, true),
    number!(persist, "PersistConfig", load_retries, 1, 20, true),
    number!(persist, "PersistConfig", max_writes_per_tick, 1, 10, true),
    number!(persist, "PersistConfig", self_test_segment, 0, 99, false),
    number!(persist, "PersistConfig", config_segment, 0, 99, false),
    Field {
        key: "log.level",
        tuning: "LogConfig::level",
        kind: Kind::Level,
        runtime: true,
        get: |c| Value::Level(c.log.level),
        set: |c, v| match v {
            Value::Level(l) => {
                c.log.level = l;
                true
            }
            _ => false,
        },
    },
    number!(log, "LogConfig", skip_window_ticks, 10, 10_000, true),
    flag!(cache, "CacheConfig", structure_static, true),
];

/// The field with runtime key `key`, and its index in [`FIELDS`].
pub fn field(key: &str) -> Option<(usize, &'static Field)> {
    FIELDS.iter().enumerate().find(|(_, f)| f.key == key)
}

/// A constraint between two fields that their ranges cannot state.
pub struct Rule {
    /// The runtime keys of both fields.
    pub keys: [&'static str; 2],
    /// The constraint as people read it.
    pub text: &'static str,
    /// Whether `Config` satisfies it.
    pub holds: fn(&Config) -> bool,
}

/// Every cross-field constraint. Every profile satisfies all of them (the config test checks it), and
/// runtime overrides that break one are refused ([`overrides`]). TUNING.md's safe-range cell of each
/// field in a rule names the other field (the doc-sync test checks it).
///
/// Uses: rustedscreep_consts::CPU_TICK_LIMIT_MAX. Standard's gate is at least the boot gate plus
/// `CPU_TICK_LIMIT_MAX`: Standard switches on from the bucket at the start of the tick, and one tick
/// is billed about tickLimit at most (the scheduler stops every tier at the ceiling and the reserve
/// covers the rest), so a Standard tick leaves the bucket at or above the boot gate (TUNING.md).
pub static RULES: [Rule; 7] = [
    Rule {
        keys: ["loader.backoff_base_ticks", "loader.backoff_max_ticks"],
        text: "loader.backoff_base_ticks must be at most loader.backoff_max_ticks",
        holds: |c| c.loader.backoff_base_ticks <= c.loader.backoff_max_ticks,
    },
    Rule {
        keys: ["loader.boot_bucket_min", "scheduler.standard_bucket_min"],
        text: "scheduler.standard_bucket_min must be at least loader.boot_bucket_min + CPU_TICK_LIMIT_MAX",
        holds: |c| {
            c.scheduler.standard_bucket_min
                >= c.loader.boot_bucket_min.saturating_add(CPU_TICK_LIMIT_MAX)
        },
    },
    Rule {
        keys: [
            "scheduler.standard_bucket_min",
            "scheduler.optional_bucket_min",
        ],
        text: "scheduler.standard_bucket_min must be at most scheduler.optional_bucket_min",
        holds: |c| c.scheduler.standard_bucket_min <= c.scheduler.optional_bucket_min,
    },
    Rule {
        keys: ["scheduler.optional_bucket_min", "scheduler.bucket_target"],
        text: "scheduler.optional_bucket_min must be at most scheduler.bucket_target",
        holds: |c| c.scheduler.optional_bucket_min <= c.scheduler.bucket_target,
    },
    Rule {
        keys: ["profiler.stats_segment", "persist.self_test_segment"],
        text: "profiler.stats_segment and persist.self_test_segment must differ",
        holds: |c| c.profiler.stats_segment != c.persist.self_test_segment,
    },
    Rule {
        keys: ["profiler.stats_segment", "persist.config_segment"],
        text: "profiler.stats_segment and persist.config_segment must differ",
        holds: |c| c.profiler.stats_segment != c.persist.config_segment,
    },
    Rule {
        keys: ["persist.self_test_segment", "persist.config_segment"],
        text: "persist.self_test_segment and persist.config_segment must differ",
        holds: |c| c.persist.self_test_segment != c.persist.config_segment,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use rustedscreep_consts::extra::{MEMORY_SEGMENT_COUNT, MEMORY_SEGMENT_SAVE_LIMIT};
    use rustedscreep_consts::{CPU_BUCKET_MAX, MEMORY_SEGMENT_ACTIVE_LIMIT};

    const TUNING: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/TUNING.md"));

    /// TUNING.md's rows of the field table, keyed by their first cell, with the seven cells of each.
    fn tuning_rows() -> Vec<(String, Vec<String>)> {
        TUNING
            .lines()
            .filter(|l| l.starts_with("| `") && l.contains("::"))
            .map(|l| {
                let cells: Vec<String> = l
                    .trim()
                    .trim_matches('|')
                    .split(" | ")
                    .map(|c| c.trim().to_string())
                    .collect();
                (cells.first().cloned().unwrap_or_default(), cells)
            })
            .collect()
    }

    fn by_key(key: &str) -> &'static Field {
        field(key).map(|(_, f)| f).expect("known key")
    }

    /// The runtime keys of one group's fields, in table order.
    fn group_keys(group: &str) -> Vec<&'static str> {
        FIELDS
            .iter()
            .filter(|f| f.key.split('.').next() == Some(group))
            .map(|f| f.key)
            .collect()
    }

    /// Exhaustive: adding a field to a config struct without a FIELDS entry fails this test, and the
    /// destructuring below stops compiling until the field is listed here.
    #[test]
    fn fields_cover_every_struct_field_once() {
        let LoaderConfig {
            boot_bucket_min: _,
            boot_split_ticks: _,
            recovery_mode: _,
            backoff_window_ticks: _,
            backoff_incidents: _,
            backoff_base_ticks: _,
            backoff_max_ticks: _,
            notify_group_minutes: _,
            trap_test_max: _,
        } = LOADER_MMO;
        let SchedulerConfig {
            tick_reserve_pct: _,
            standard_bucket_min: _,
            optional_bucket_min: _,
            bucket_target: _,
            surplus_spread_ticks: _,
            optional_warmup_loops: _,
            max_units_per_sample: _,
            bad_samples_to_break: _,
            fault_reports_per_window: _,
            fault_window_ticks: _,
        } = SCHEDULER_MMO;
        let ProfilerConfig {
            window_loops: _,
            history_windows: _,
            stats_segment: _,
        } = PROFILER_MMO;
        let PersistConfig {
            max_active_segments: _,
            load_retries: _,
            max_writes_per_tick: _,
            self_test_segment: _,
            config_segment: _,
        } = PERSIST_MMO;
        let LogConfig {
            level: _,
            skip_window_ticks: _,
        } = LOG_MMO;
        let CacheConfig {
            structure_static: _,
        } = CACHE_MMO;
        let Config {
            loader: _,
            scheduler: _,
            profiler: _,
            persist: _,
            log: _,
            cache: _,
        } = MMO.config;
        let expected: [(&str, &[&str]); 6] = [
            (
                "loader",
                &[
                    "boot_bucket_min",
                    "boot_split_ticks",
                    "recovery_mode",
                    "backoff_window_ticks",
                    "backoff_incidents",
                    "backoff_base_ticks",
                    "backoff_max_ticks",
                    "notify_group_minutes",
                    "trap_test_max",
                ],
            ),
            (
                "scheduler",
                &[
                    "tick_reserve_pct",
                    "standard_bucket_min",
                    "optional_bucket_min",
                    "bucket_target",
                    "surplus_spread_ticks",
                    "optional_warmup_loops",
                    "max_units_per_sample",
                    "bad_samples_to_break",
                    "fault_reports_per_window",
                    "fault_window_ticks",
                ],
            ),
            (
                "profiler",
                &["window_loops", "history_windows", "stats_segment"],
            ),
            (
                "persist",
                &[
                    "max_active_segments",
                    "load_retries",
                    "max_writes_per_tick",
                    "self_test_segment",
                    "config_segment",
                ],
            ),
            ("log", &["level", "skip_window_ticks"]),
            ("cache", &["structure_static"]),
        ];
        let mut total = 0;
        for (group, names) in expected {
            let want: Vec<String> = names.iter().map(|n| format!("{group}.{n}")).collect();
            assert_eq!(group_keys(group), want, "FIELDS for {group}");
            total += names.len();
        }
        assert_eq!(total, FIELD_COUNT, "FIELDS holds only these groups");
    }

    /// Every getter reads its own field and every setter writes only its own field.
    #[test]
    fn getters_and_setters_touch_their_own_field() {
        let base = MMO.config;
        for (i, f) in FIELDS.iter().enumerate() {
            let other = match (f.get)(&base) {
                Value::Number(n) => Value::Number(if n == 0 { 1 } else { n - 1 }),
                Value::Flag(b) => Value::Flag(!b),
                Value::Level(l) => Value::Level(if l == LogLevel::Warn {
                    LogLevel::Trace
                } else {
                    LogLevel::Warn
                }),
                Value::Recovery(r) => Value::Recovery(if r == RecoveryMode::Halt {
                    RecoveryMode::Reinstantiate
                } else {
                    RecoveryMode::Halt
                }),
            };
            let mut c = base;
            assert!((f.set)(&mut c, other), "{}", f.key);
            assert_eq!((f.get)(&c), other, "{}", f.key);
            for (j, g) in FIELDS.iter().enumerate() {
                if j != i {
                    assert_eq!((g.get)(&c), (g.get)(&base), "{} changed {}", f.key, g.key);
                }
            }
            // A value of another kind is refused and writes nothing.
            let wrong = match other {
                Value::Number(_) => Value::Flag(true),
                _ => Value::Number(1),
            };
            let mut d = base;
            assert!(!(f.set)(&mut d, wrong), "{}", f.key);
            assert_eq!(d, base, "{}", f.key);
        }
        // A number that does not fit a u8 field is refused.
        let mut c = base;
        assert!(!(by_key("persist.load_retries").set)(
            &mut c,
            Value::Number(256)
        ));
        assert_eq!(c, base);
    }

    /// Uses: rustedscreep_consts::{CPU_BUCKET_MAX, CPU_TICK_LIMIT_MAX, MEMORY_SEGMENT_ACTIVE_LIMIT,
    /// extra::MEMORY_SEGMENT_COUNT, extra::MEMORY_SEGMENT_SAVE_LIMIT}. The safe ranges that come from
    /// a constant equal it: bucket values reach `CPU_BUCKET_MAX`, the Standard-and-up gates start at
    /// `CPU_TICK_LIMIT_MAX`, segment ids end one below `MEMORY_SEGMENT_COUNT`, the active cap leaves
    /// one save key free and stays within the API's active limit, and writes fit the save cap.
    #[test]
    fn ranges_match_their_constants() {
        let range = |key| match by_key(key).kind {
            Kind::Number { min, max } => (min, max),
            _ => panic!("{key} is not a number"),
        };
        assert_eq!(range("loader.boot_bucket_min"), (0, CPU_BUCKET_MAX));
        for key in [
            "scheduler.standard_bucket_min",
            "scheduler.optional_bucket_min",
            "scheduler.bucket_target",
        ] {
            assert_eq!(range(key), (CPU_TICK_LIMIT_MAX, CPU_BUCKET_MAX), "{key}");
        }
        for key in [
            "profiler.stats_segment",
            "persist.self_test_segment",
            "persist.config_segment",
        ] {
            assert_eq!(
                range(key),
                (0, u32::from(MEMORY_SEGMENT_COUNT) - 1),
                "{key}"
            );
        }
        let (_, active) = range("persist.max_active_segments");
        assert_eq!(active, MEMORY_SEGMENT_SAVE_LIMIT - 1);
        assert!(active <= MEMORY_SEGMENT_ACTIVE_LIMIT);
        assert_eq!(
            range("persist.max_writes_per_tick").1,
            MEMORY_SEGMENT_SAVE_LIMIT
        );
        // Every number range is non-empty and every u8 field's range fits a u8.
        for f in &FIELDS {
            if let Kind::Number { min, max } = f.kind {
                assert!(min <= max, "{}", f.key);
                let mut c = MMO.config;
                assert!((f.set)(&mut c, Value::Number(max)), "{} max fits", f.key);
            }
        }
    }

    /// Every profile's values lie in their safe ranges and satisfy every rule; every rule names two
    /// known fields.
    #[test]
    fn every_profile_is_within_ranges_and_rules() {
        for p in &PROFILES {
            for f in &FIELDS {
                assert!(f.accepts((f.get)(&p.config)), "{}: {}", p.name, f.key);
            }
            for r in &RULES {
                assert!((r.holds)(&p.config), "{}: {}", p.name, r.text);
            }
        }
        for r in &RULES {
            for key in r.keys {
                assert!(field(key).is_some(), "rule key {key}");
            }
        }
        assert_eq!(PROFILES.first().map(|p| p.name), Some("mmo"));
        assert_eq!(Profile::by_name("mmo"), Some(&MMO));
        assert_eq!(Profile::by_name("nope"), None);
    }

    /// docs/TUNING.md mirrors FIELDS row for row (CLAUDE.md): each field has exactly one row, whose
    /// default is `MMO`'s value, whose safe range starts with the code's range (then the end of the
    /// cell, or prose after ", ", "; " or " ("), names the other field of each rule it is in, whose
    /// runtime-key cell is the key or `none`, and whose profile cell names each other profile with a
    /// different value (`none` when there is none). No row names a field that does not exist.
    #[test]
    fn tuning_md_mirrors_the_field_table() {
        let rows = tuning_rows();
        for (name, cells) in &rows {
            assert!(
                FIELDS.iter().any(|f| *name == format!("`{}`", f.tuning)),
                "TUNING.md row {name} has no field"
            );
            assert_eq!(cells.len(), 7, "TUNING.md row {name} has 7 cells");
        }
        for f in &FIELDS {
            let name = format!("`{}`", f.tuning);
            let matching: Vec<&Vec<String>> = rows
                .iter()
                .filter(|(n, _)| *n == name)
                .map(|(_, c)| c)
                .collect();
            assert_eq!(matching.len(), 1, "TUNING.md needs one row for {name}");
            let cells = matching.first().expect("one row");
            let cell = |i: usize| cells.get(i).map(String::as_str).unwrap_or_default();
            assert_eq!(cell(1), (f.get)(&MMO.config).to_string(), "{name} default");
            let range = f.kind.to_string();
            let safe = cell(4);
            let rest = safe
                .strip_prefix(range.as_str())
                .unwrap_or_else(|| panic!("{name} safe range must start with {range}: {safe}"));
            assert!(
                rest.is_empty()
                    || rest.starts_with(", ")
                    || rest.starts_with("; ")
                    || rest.starts_with(" ("),
                "{name} safe range: {safe}"
            );
            for r in RULES.iter().filter(|r| r.keys.contains(&f.key)) {
                for other in r.keys.iter().filter(|&&k| k != f.key) {
                    let other = format!("`{}`", by_key(other).tuning);
                    assert!(safe.contains(&other), "{name} safe range must name {other}");
                }
            }
            let key = if f.runtime {
                format!("`{}`", f.key)
            } else {
                String::from("none")
            };
            assert_eq!(cell(5), key, "{name} runtime key");
            let differing: Vec<String> = PROFILES
                .iter()
                .skip(1)
                .filter(|p| (f.get)(&p.config) != (f.get)(&MMO.config))
                .map(|p| format!("{}: {}", p.name, (f.get)(&p.config)))
                .collect();
            let profiles = if differing.is_empty() {
                String::from("none")
            } else {
                differing.join(", ")
            };
            assert_eq!(cell(6), profiles, "{name} profile overrides");
        }
    }

    /// Uses: rustedscreep_consts::{CPU_BUCKET_MAX, CPU_TICK_LIMIT_MAX}, LOADER_MMO.boot_bucket_min.
    /// The bucket gates are ordered and reachable, and every divisor and count is at least 1.
    ///
    /// Standard's gate is at least the boot gate plus `CPU_TICK_LIMIT_MAX`: Standard switches on from
    /// the bucket at the start of the tick, and one tick is billed about tickLimit at most (the
    /// scheduler stops every tier at the ceiling and the reserve covers the rest) while it refills
    /// `limit`, which is at least 0 (V37). So a Standard tick leaves the bucket at or above the boot
    /// gate, and a global reset in a CPU deficit can still boot at once. At the MMO's limit of 20 the
    /// refill also covers the up to 1 CPU of ceil billing (V106).
    #[test]
    fn scheduler_values_are_coherent() {
        let c = SCHEDULER_MMO;
        assert!((5..=50).contains(&c.tick_reserve_pct));
        assert!(
            c.standard_bucket_min >= LOADER_MMO.boot_bucket_min + CPU_TICK_LIMIT_MAX,
            "one Standard tick must not cross the loader's boot gate"
        );
        assert!(c.standard_bucket_min <= c.optional_bucket_min);
        assert!(c.optional_bucket_min <= c.bucket_target);
        assert!(c.bucket_target <= CPU_BUCKET_MAX);
        assert!(c.surplus_spread_ticks >= 1);
        assert!(
            c.optional_warmup_loops >= 1,
            "the first tick after a reset must not run Optional work (wasm-bot rule)"
        );
        assert!(c.max_units_per_sample >= 1);
        assert!(c.bad_samples_to_break >= 1);
        assert!(c.fault_reports_per_window >= 1 && c.fault_window_ticks >= 1);
        // The overridable ranges keep these properties: every lower bound is at least 1 where a
        // value divides or counts, and the warm-up cannot be overridden to 0.
        for key in [
            "scheduler.surplus_spread_ticks",
            "scheduler.optional_warmup_loops",
            "scheduler.max_units_per_sample",
            "scheduler.bad_samples_to_break",
            "scheduler.fault_reports_per_window",
            "scheduler.fault_window_ticks",
        ] {
            assert!(
                matches!(by_key(key).kind, Kind::Number { min, .. } if min >= 1),
                "{key}"
            );
        }
    }

    /// Uses: rustedscreep_consts::CPU_BUCKET_MAX. A boot gate above the bucket's maximum would never open.
    #[test]
    fn loader_values_are_coherent() {
        let c = LOADER_MMO;
        assert!(c.boot_bucket_min <= CPU_BUCKET_MAX);
        assert!(c.backoff_incidents >= 1);
        assert!(c.backoff_base_ticks >= 1 && c.backoff_base_ticks <= c.backoff_max_ticks);
        assert!(
            c.trap_test_max >= 10,
            "the P1 gate forces 10 traps in a row"
        );
        assert!(
            FIELDS
                .iter()
                .filter(|f| f.key.starts_with("loader."))
                .all(|f| !f.runtime),
            "the loader's values are built into the bundle"
        );
    }

    /// Uses: rustedscreep_consts::extra::MEMORY_SEGMENT_COUNT. The window meets the ROADMAP's
    /// evidence rule (mean and p95 over at least 1000 ticks), the history keeps at least one window,
    /// and the segment id exists. The segment's worst-case length is checked in the profiler's tests.
    #[test]
    fn profiler_values_are_coherent() {
        let c = PROFILER_MMO;
        assert!(
            (1000..=10_000).contains(&c.window_loops),
            "ROADMAP: CPU numbers are mean and p95 over at least 1000 ticks"
        );
        assert!((1..=32).contains(&c.history_windows));
        assert!(c.stats_segment < MEMORY_SEGMENT_COUNT);
    }

    /// Uses: rustedscreep_consts::{MEMORY_SEGMENT_ACTIVE_LIMIT, extra::MEMORY_SEGMENT_SAVE_LIMIT,
    /// extra::MEMORY_SEGMENT_COUNT}. The active cap leaves a save key for the profiler's segment, the
    /// manager can write, retries and writes are at least 1, and the self-test and config segments
    /// exist and differ from each other and from the profiler's.
    #[test]
    fn persist_values_are_coherent() {
        let c = PERSIST_MMO;
        assert!(c.max_active_segments >= 1);
        assert!(u32::from(c.max_active_segments) <= MEMORY_SEGMENT_ACTIVE_LIMIT);
        assert!(
            u32::from(c.max_active_segments) < MEMORY_SEGMENT_SAVE_LIMIT,
            "one save key stays free for the profiler's segment"
        );
        assert!((1..=20).contains(&c.load_retries));
        assert!((1..=MEMORY_SEGMENT_SAVE_LIMIT).contains(&u32::from(c.max_writes_per_tick)));
        assert!(c.self_test_segment < MEMORY_SEGMENT_COUNT);
        assert!(c.config_segment < MEMORY_SEGMENT_COUNT);
        assert_ne!(c.self_test_segment, PROFILER_MMO.stats_segment);
        assert_ne!(c.config_segment, PROFILER_MMO.stats_segment);
        assert_ne!(c.config_segment, c.self_test_segment);
    }

    #[test]
    fn log_levels_round_trip_in_order() {
        for (i, l) in LogLevel::ALL.into_iter().enumerate() {
            assert_eq!(LogLevel::parse(l.as_str()), Some(l));
            if let Some(&next) = LogLevel::ALL.get(i + 1) {
                assert!(l < next, "ALL runs from the most important level");
            }
        }
        assert_eq!(LogLevel::parse("off"), None);
        assert_eq!(LogLevel::parse("error"), None, "warnings always print");
        assert_eq!(LogLevel::parse("Info"), None);
    }

    #[test]
    fn json_has_every_field_once() {
        let json = LOADER_MMO.to_json();
        for key in [
            "bootBucketMin",
            "bootSplitTicks",
            "recoveryMode",
            "backoffWindowTicks",
            "backoffIncidents",
            "backoffBaseTicks",
            "backoffMaxTicks",
            "notifyGroupMinutes",
            "trapTestMax",
        ] {
            assert_eq!(
                json.matches(&format!("\"{key}\":")).count(),
                1,
                "{key} in {json}"
            );
        }
        assert!(json.contains("\"recoveryMode\":\"halt\""));
    }
}
