//! Tunable thresholds. Every threshold lives here and is mirrored field for field in `docs/TUNING.md`
//! (CLAUDE.md). P1 module 10 adds the server-profile table, runtime overrides and the doc-sync test.
//!
//! The loader's values are here too, although the loader is TypeScript: it runs before the wasm is
//! instantiated, so `npm run build` prints [`LOADER_MMO`] with the `loader_config` example and injects
//! the JSON into the bundle. They cannot be overridden at runtime.

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

/// Loader values for the official MMO (the only profile until P1 module 10).
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

/// Scheduler values for the official MMO (the only profile until P1 module 10).
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

/// Profiler values for the official MMO (the only profile until P1 module 10).
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
}

/// Segment manager values for the official MMO (the only profile until P1 module 10).
pub const PERSIST_MMO: PersistConfig = PersistConfig {
    max_active_segments: 8,
    load_retries: 3,
    max_writes_per_tick: 2,
    self_test_segment: 98,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Finds the rows of one config struct in TUNING.md and checks each field has exactly one row
    /// whose default equals the value in config.rs.
    fn assert_tuning_rows(tuning: &str, strukt: &str, expected: &[(&str, String)]) {
        let start = format!("| `{strukt}::");
        let rows: Vec<&str> = tuning.lines().filter(|l| l.starts_with(&start)).collect();
        assert_eq!(rows.len(), expected.len(), "{strukt} rows in TUNING.md");
        for (field, default) in expected {
            let prefix = format!("| `{strukt}::{field}` | {default} |");
            assert_eq!(
                rows.iter().filter(|r| r.starts_with(&prefix)).count(),
                1,
                "TUNING.md needs one row starting {prefix}"
            );
        }
    }

    const TUNING: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/TUNING.md"));

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
            c.standard_bucket_min
                >= LOADER_MMO.boot_bucket_min + rustedscreep_consts::CPU_TICK_LIMIT_MAX,
            "one Standard tick must not cross the loader's boot gate"
        );
        assert!(c.standard_bucket_min <= c.optional_bucket_min);
        assert!(c.optional_bucket_min <= c.bucket_target);
        assert!(c.bucket_target <= rustedscreep_consts::CPU_BUCKET_MAX);
        assert!(c.surplus_spread_ticks >= 1);
        assert!(
            c.optional_warmup_loops >= 1,
            "the first tick after a reset must not run Optional work (wasm-bot rule)"
        );
        assert!(c.max_units_per_sample >= 1);
        assert!(c.bad_samples_to_break >= 1);
        assert!(c.fault_reports_per_window >= 1 && c.fault_window_ticks >= 1);
    }

    /// docs/TUNING.md mirrors SCHEDULER_MMO field for field (CLAUDE.md).
    #[test]
    fn tuning_md_mirrors_the_scheduler_config() {
        // Exhaustive: adding a field without a row here fails to compile.
        let SchedulerConfig {
            tick_reserve_pct,
            standard_bucket_min,
            optional_bucket_min,
            bucket_target,
            surplus_spread_ticks,
            optional_warmup_loops,
            max_units_per_sample,
            bad_samples_to_break,
            fault_reports_per_window,
            fault_window_ticks,
        } = SCHEDULER_MMO;
        let expected = [
            ("tick_reserve_pct", tick_reserve_pct.to_string()),
            ("standard_bucket_min", standard_bucket_min.to_string()),
            ("optional_bucket_min", optional_bucket_min.to_string()),
            ("bucket_target", bucket_target.to_string()),
            ("surplus_spread_ticks", surplus_spread_ticks.to_string()),
            ("optional_warmup_loops", optional_warmup_loops.to_string()),
            ("max_units_per_sample", max_units_per_sample.to_string()),
            ("bad_samples_to_break", bad_samples_to_break.to_string()),
            (
                "fault_reports_per_window",
                fault_reports_per_window.to_string(),
            ),
            ("fault_window_ticks", fault_window_ticks.to_string()),
        ];
        assert_tuning_rows(TUNING, "SchedulerConfig", &expected);
    }

    /// Uses: rustedscreep_consts::CPU_BUCKET_MAX. A boot gate above the bucket's maximum would never open.
    #[test]
    fn loader_values_are_coherent() {
        let c = LOADER_MMO;
        assert!(c.boot_bucket_min <= rustedscreep_consts::CPU_BUCKET_MAX);
        assert!(c.backoff_incidents >= 1);
        assert!(c.backoff_base_ticks >= 1 && c.backoff_base_ticks <= c.backoff_max_ticks);
        assert!(
            c.trap_test_max >= 10,
            "the P1 gate forces 10 traps in a row"
        );
    }

    /// docs/TUNING.md mirrors this file field for field (CLAUDE.md). Each `LoaderConfig` field has exactly
    /// one row whose default equals `LOADER_MMO`. P1 module 10 extends this to every config struct.
    #[test]
    fn tuning_md_mirrors_the_loader_config() {
        // Exhaustive: adding a field without a row here fails to compile.
        let LoaderConfig {
            boot_bucket_min,
            boot_split_ticks,
            recovery_mode,
            backoff_window_ticks,
            backoff_incidents,
            backoff_base_ticks,
            backoff_max_ticks,
            notify_group_minutes,
            trap_test_max,
        } = LOADER_MMO;
        let expected = [
            ("boot_bucket_min", boot_bucket_min.to_string()),
            ("boot_split_ticks", boot_split_ticks.to_string()),
            ("recovery_mode", recovery_mode.as_str().to_string()),
            ("backoff_window_ticks", backoff_window_ticks.to_string()),
            ("backoff_incidents", backoff_incidents.to_string()),
            ("backoff_base_ticks", backoff_base_ticks.to_string()),
            ("backoff_max_ticks", backoff_max_ticks.to_string()),
            ("notify_group_minutes", notify_group_minutes.to_string()),
            ("trap_test_max", trap_test_max.to_string()),
        ];
        assert_tuning_rows(TUNING, "LoaderConfig", &expected);
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
        assert!(c.stats_segment < rustedscreep_consts::extra::MEMORY_SEGMENT_COUNT);
    }

    /// docs/TUNING.md mirrors PROFILER_MMO field for field (CLAUDE.md).
    #[test]
    fn tuning_md_mirrors_the_profiler_config() {
        // Exhaustive: adding a field without a row here fails to compile.
        let ProfilerConfig {
            window_loops,
            history_windows,
            stats_segment,
        } = PROFILER_MMO;
        let expected = [
            ("window_loops", window_loops.to_string()),
            ("history_windows", history_windows.to_string()),
            ("stats_segment", stats_segment.to_string()),
        ];
        assert_tuning_rows(TUNING, "ProfilerConfig", &expected);
    }

    /// Uses: rustedscreep_consts::{MEMORY_SEGMENT_ACTIVE_LIMIT, extra::MEMORY_SEGMENT_SAVE_LIMIT,
    /// extra::MEMORY_SEGMENT_COUNT}. The active cap leaves a save key for the profiler's segment, the
    /// manager can write, retries and writes are at least 1, and the self-test segment exists and is
    /// not the profiler's.
    #[test]
    fn persist_values_are_coherent() {
        let c = PERSIST_MMO;
        assert!(c.max_active_segments >= 1);
        assert!(
            u32::from(c.max_active_segments) <= rustedscreep_consts::MEMORY_SEGMENT_ACTIVE_LIMIT
        );
        assert!(
            u32::from(c.max_active_segments)
                < rustedscreep_consts::extra::MEMORY_SEGMENT_SAVE_LIMIT,
            "one save key stays free for the profiler's segment"
        );
        assert!((1..=20).contains(&c.load_retries));
        assert!(
            (1..=rustedscreep_consts::extra::MEMORY_SEGMENT_SAVE_LIMIT)
                .contains(&u32::from(c.max_writes_per_tick))
        );
        assert!(c.self_test_segment < rustedscreep_consts::extra::MEMORY_SEGMENT_COUNT);
        assert_ne!(c.self_test_segment, PROFILER_MMO.stats_segment);
    }

    /// docs/TUNING.md mirrors PERSIST_MMO field for field (CLAUDE.md).
    #[test]
    fn tuning_md_mirrors_the_persist_config() {
        // Exhaustive: adding a field without a row here fails to compile.
        let PersistConfig {
            max_active_segments,
            load_retries,
            max_writes_per_tick,
            self_test_segment,
        } = PERSIST_MMO;
        let expected = [
            ("max_active_segments", max_active_segments.to_string()),
            ("load_retries", load_retries.to_string()),
            ("max_writes_per_tick", max_writes_per_tick.to_string()),
            ("self_test_segment", self_test_segment.to_string()),
        ];
        assert_tuning_rows(TUNING, "PersistConfig", &expected);
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
