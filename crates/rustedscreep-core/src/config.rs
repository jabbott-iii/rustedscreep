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

#[cfg(test)]
mod tests {
    use super::*;

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
        let tuning = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/TUNING.md"));
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
        let rows: Vec<&str> = tuning
            .lines()
            .filter(|l| l.starts_with("| `LoaderConfig::"))
            .collect();
        assert_eq!(rows.len(), expected.len(), "LoaderConfig rows in TUNING.md");
        for (field, default) in expected {
            let prefix = format!("| `LoaderConfig::{field}` | {default} |");
            assert_eq!(
                rows.iter().filter(|r| r.starts_with(&prefix)).count(),
                1,
                "TUNING.md needs one row starting {prefix}"
            );
        }
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
