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

use rustedscreep_consts::{
    CPU_TICK_LIMIT_MAX, CREEP_LIFE_TIME, CREEP_SPAWN_TIME, LINK_CAPACITY, MAX_CREEP_SIZE,
    MOVE_COST_PLAIN, MOVE_COST_ROAD, MOVE_COST_SWAMP,
};

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
    // CPU_TICK_LIMIT_MAX − the free-tier limit of 20 (V35): from here every boot tick gets the full
    // tickLimit (V37). Set at the P1 gate from the MMO cold start (V149, V150; TUNING.md).
    boot_bucket_min: 480,
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
/// (`log` with a minimal logger, ADR-014). It starts at warn, with no "error" or "off": faults, failed JS calls
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
    /// Findings of the constants self-check (P1.11) printed one per line when it completes, once per
    /// global; the rest are counted in its summary line. Unit: lines.
    pub consts_findings: u32,
}

/// Logging values for the official MMO.
pub const LOG_MMO: LogConfig = LogConfig {
    level: LogLevel::Info,
    skip_window_ticks: 100,
    consts_findings: 20,
};

/// Cache toggles. Each one off replaces its cache with a read every tick; all off is the naive
/// baseline that measures what the caches save (ROADMAP evidence conventions). Only a runtime
/// override turns one off (every profile keeps them on), and the bot removes such overrides itself
/// after `naive_max_ticks` or below `naive_stop_bucket` (ADR-022, [`overrides::Store::naive_tick`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheConfig {
    /// Keep each structure's type and position from the tick it is first seen (P1.5); off, the
    /// snapshot reads both every tick. Unit: flag.
    pub structure_static: bool,
    /// Keep each room's terrain, scanned objects and cost grid (P2.1); off, the movement glue reads
    /// the terrain and scans the room every tick, and the grid is rebuilt every tick. Unit: flag.
    pub movement_grid: bool,
    /// Keep flow fields between requests (P2.1); off, every request builds its field again and
    /// nothing is kept. Unit: flag.
    pub flow_fields: bool,
    /// Longest naive span (ADR-022): after this many ticks with a cache override off, counted from
    /// the span's first loop, the bot removes those overrides itself. 1000 is one profiler window
    /// (`ProfilerConfig::window_loops`), so a span set on a window's last tick fills the next
    /// window exactly. Unit: ticks. Safe range 1 to 10000.
    pub naive_max_ticks: u32,
    /// The bucket below which a naive span ends at once (ADR-022). At the free tier's 20 CPU the
    /// surplus formula turns Optional off at `bucket_target` − limit × `surplus_spread_ticks` (7000
    /// with `SCHEDULER_MMO`), so 8000 ends a span while Optional work still runs (the config test
    /// checks it); a lower override lets a span run on with Optional off. Unit: CPU. Safe range 0 to
    /// `CPU_BUCKET_MAX` (10000).
    pub naive_stop_bucket: u32,
}

impl CacheConfig {
    /// Whether any cache is off: the tick runs (part of) the naive baseline.
    #[inline]
    pub fn naive(&self) -> bool {
        !(self.structure_static && self.movement_grid && self.flow_fields)
    }
}

/// Cache toggles for the official MMO: every cache on; a naive span lasts one profiler window at
/// most and ends below bucket 8000.
pub const CACHE_MMO: CacheConfig = CacheConfig {
    structure_static: true,
    movement_grid: true,
    flow_fields: true,
    naive_max_ticks: 1000,
    naive_stop_bucket: 8000,
};

/// Movement thresholds (ROADMAP P2, movement); `docs/TUNING.md` gives each one's rationale and safe
/// range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MovementConfig {
    /// Cost of stepping onto a road (a road on a wall tile included). Unit: cost per tile.
    pub road_cost: u8,
    /// Cost of stepping onto plain terrain. Unit: cost per tile.
    pub plain_cost: u8,
    /// Cost of stepping onto swamp terrain. Unit: cost per tile.
    pub swamp_cost: u8,
    /// A room with own creeps that is owned or had a move request within this many ticks has its
    /// objects scanned again this long after the last scan. Unit: ticks.
    pub scan_interval_ticks: u32,
    /// Earliest rescan after the last one when a move failed into a tile with no creep on it (the
    /// grid may be missing an object). Unit: ticks.
    pub rescan_min_ticks: u32,
    /// Most flow fields kept; the least recently used goes first. Unit: fields.
    pub field_cache_size: u32,
    /// A field unused for more than this many ticks is dropped, and so is a room no own creep has been
    /// in for longer. Unit: ticks.
    pub field_idle_ticks: u32,
    /// Failed moves in a row (or ticks the traffic manager kept a mover in place) before the creep
    /// searches a detour around the creeps in its way. Unit: ticks.
    pub stuck_ticks: u8,
    /// Most entries one detour search may pop before it gives up. Unit: entries.
    pub detour_max_pops: u32,
    /// How deep the traffic manager may push a chain of creeps out of a mover's way. Unit: creeps.
    pub shove_depth: u8,
    /// Most tiles one mover's chain search may try to claim. Unit: claims.
    pub shove_claims: u32,
    /// A creep whose move the game refused (no active MOVE part) is held as fixed this long before
    /// it is asked to move again. Unit: ticks.
    pub immobile_retry_ticks: u32,
    /// A tile moves kept failing into with no creep seen on it stays blocked this long. Unit: ticks.
    pub suspect_ticks: u32,
    /// Most nodes one inter-room route search (PathFinder's `maxOps`, V163) may expand before it
    /// gives up. Unit: ops.
    pub route_max_ops: u32,
    /// Most rooms one route search may enter (PathFinder's `maxRooms`; rooms its callback blocks do
    /// not count, V165). Unit: rooms.
    pub route_max_rooms: u8,
    /// A cached route exit is searched again this long after it was found, and a room a creep could
    /// not cross into is held closed this long. Unit: ticks.
    pub route_ttl_ticks: u32,
    /// A creep whose route search found nothing (or whose found exit it cannot reach) does not
    /// search again for the same target from the same room for this long. Unit: ticks.
    pub route_retry_ticks: u32,
    /// Most routes kept whole (each a search's room visits and exits, shared by creeps in its origin
    /// room); the least recently used goes first. Unit: routes.
    pub route_cache_size: u32,
}

/// Movement values for the official MMO. Uses: MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP
/// (the engine's fatigue rates per tile, V54, V154), which the config test checks fit a `u8`.
pub const MOVEMENT_MMO: MovementConfig = MovementConfig {
    road_cost: MOVE_COST_ROAD as u8,
    plain_cost: MOVE_COST_PLAIN as u8,
    swamp_cost: MOVE_COST_SWAMP as u8,
    scan_interval_ticks: 50,
    rescan_min_ticks: 10,
    field_cache_size: 32,
    field_idle_ticks: 300,
    stuck_ticks: 2,
    detour_max_pops: 5000,
    shove_depth: 2,
    shove_claims: 256,
    immobile_retry_ticks: 50,
    suspect_ticks: 10,
    route_max_ops: 4000,
    route_max_rooms: 16,
    route_ttl_ticks: CREEP_LIFE_TIME,
    route_retry_ticks: 100,
    route_cache_size: 64,
};

/// Spawn path thresholds (ROADMAP P2, the minimal spawn path); `docs/TUNING.md` gives the
/// rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnConfig {
    /// A spawn whose `spawnCreep` call the game refused (or that threw) takes no order for this
    /// long, so another spawn of its room can take the request. Unit: ticks.
    pub hold_ticks: u32,
}

/// Spawn path values for the official MMO.
pub const SPAWN_MMO: SpawnConfig = SpawnConfig { hold_ticks: 20 };

/// RCL1–3 opening thresholds (ROADMAP P2.4): worker counts, recovery, scavenging, the downgrade
/// guard and the stall watchdog; `docs/TUNING.md` gives the rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpeningConfig {
    /// Most workers per owned room, whatever its sources could keep busy: each costs about one
    /// work intent and one move a tick. Unit: creeps.
    pub max_workers: u32,
    /// Fewer workers than this that can work and new ones spawn with the energy the room holds,
    /// not its full capacity; the recovery after a zero-creep room ends at this count. At most
    /// `max_workers`. Unit: creeps.
    pub min_workers: u32,
    /// A worker request that waited this long, with no new worker, for the room to hold its
    /// full-capacity body caps the room's bodies at the most energy it held meanwhile, until the
    /// room is seen full. Unit: ticks.
    pub wait_ticks: u32,
    /// The least energy in a pile, tombstone, ruin or container that a gathering worker fetches
    /// (less when its store needs less). Unit: energy.
    pub scavenge_min: u32,
    /// Below this `ticksToDowngrade` one worker upgrades before building and upgrading by the
    /// others, until its store is empty; the guard ends `CONTROLLER_DOWNGRADE_RESTORE` above it.
    /// Unit: ticks.
    pub downgrade_guard_ticks: u32,
    /// Workers that upgrade while others build, once filling the spawns is covered; none while
    /// the room's upgraders take the controller's energy (P3.4). Unit: creeps.
    pub min_upgraders: u32,
    /// A room whose controller progress, sources' energy (harvesting) and construction progress all
    /// stood still this long while it had workers is reported and planned again. Unit: ticks.
    pub stall_ticks: u32,
    /// Places extension sites as the level allows (ADR-020's interim pattern) in the Optional
    /// tier. Off, the opening builds only sites placed by hand.
    pub place_extensions: bool,
    /// The worker solver (P3.6, `economy::sizing::worker_plan`) takes the fewest workers among the
    /// bodies whose value (energy put to use over a creep life, net of their bodies) is within this
    /// share of the room's yield and build work of the best one. Unit: percent.
    pub body_slack_pct: u32,
}

/// Opening values for the official MMO.
pub const OPENING_MMO: OpeningConfig = OpeningConfig {
    max_workers: 20,
    min_workers: 2,
    wait_ticks: 300,
    scavenge_min: 50,
    downgrade_guard_ticks: 4000,
    min_upgraders: 1,
    stall_ticks: 1500,
    place_extensions: true,
    body_slack_pct: 2,
};

/// Static mining switches (ROADMAP P3.1, ADR-023) and the P3 gate's harvest target;
/// `docs/TUNING.md` gives each one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiningConfig {
    /// Requests a static miner for each source of an owned room whose capacity pays for one that
    /// drains it (`economy::mining`). Off, no new miner is requested; live ones keep mining.
    pub enabled: bool,
    /// Places a container site on each source's mining tile (ADR-023) in the Optional tier, once
    /// the room's capacity pays for the source's miner. Off, miners drop their energy on a pile
    /// unless a container is there already.
    pub place_containers: bool,
    /// The P3 gate's harvest-efficiency target: per source, the energy harvested in each
    /// regeneration cycle against its capacity, averaged over the cycles of one miner life
    /// (`CREEP_LIFE_TIME / ENERGY_REGEN_TIME`). The gate reads it (TEST_PLAN §8); no tick does, so
    /// it has no runtime key. Unit: percent.
    pub harvest_efficiency_pct: u32,
}

/// Mining values for the official MMO.
pub const MINING_MMO: MiningConfig = MiningConfig {
    enabled: true,
    place_containers: true,
    harvest_efficiency_pct: 95,
};

/// The link network's switches and send threshold (ROADMAP P3.2, ADR-024); `docs/TUNING.md` gives
/// each one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkConfig {
    /// Runs the network (`economy::mining`): a miner beside a built source link gets a CARRY part
    /// and transfers its harvest into the link, and source links send to the controller link. Off,
    /// new miners have no CARRY, no miner transfers and no link sends; workers still take from the
    /// controller link.
    pub enabled: bool,
    /// Places link sites (ADR-024) in the Optional tier from the level that allows links: the
    /// controller link, then a source link beside each mining tile, farthest source first. Off,
    /// the network runs only links that are there already.
    pub place_links: bool,
    /// The most source links the bot places, farthest source from the spawn first; the other
    /// sources keep their containers, which feed the spawn and the extensions, until P4's storage
    /// link. Links placed by hand run whatever this says. Unit: links.
    pub source_links: u32,
    /// A source link sends once it can move at least this much to the controller link: its energy
    /// and the controller link's room both reach it. Unit: energy.
    pub send_min: u32,
    /// A source link whose `transferEnergy` call the game refused (or that threw) sends no more for
    /// this long: every check the game makes is modelled (V216), so a refusal means something the
    /// bot cannot see, and the hold keeps it (and its log line) from repeating every tick.
    /// Unit: ticks.
    pub hold_ticks: u32,
}

/// Link values for the official MMO.
pub const LINK_MMO: LinkConfig = LinkConfig {
    enabled: true,
    place_links: true,
    source_links: 1,
    send_min: LINK_CAPACITY / 4,
    hold_ticks: 20,
};

/// Repairs (ROADMAP P3.5): the workers repair the containers on the mining plan's tiles, the only
/// decaying structures the bot builds before P4 (V214); containers elsewhere are never repaired
/// (the user's choice, 2026-11-16). `docs/TUNING.md` gives each one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepairConfig {
    /// Lets the opening's workers repair the containers on their room's mining tiles
    /// (`economy::Opening`, in the Standard tier after filling, the downgrade guard and
    /// `opening.min_upgraders`, before building). Off, no container is repaired, and an owned
    /// room's container lasts about 24550 ticks (V214).
    pub enabled: bool,
    /// A container is repaired once its hits fall below this share of its `hitsMax`
    /// (`CONTAINER_HITS`, V223), and then up to full, by one worker at a time that is already
    /// within repair range of it. Unit: percent.
    pub start_pct: u32,
    /// Below this share of its `hitsMax` a container under repair also takes a worker that must walk
    /// to it (the nearest with energy); above it only a worker already within repair range repairs
    /// it, which at an unlinked source costs no walk (its gatherers take their loads there). At most
    /// `start_pct`. Unit: percent.
    pub walk_pct: u32,
}

/// Repair values for the official MMO.
pub const REPAIR_MMO: RepairConfig = RepairConfig {
    enabled: true,
    start_pct: 80,
    walk_pct: 50,
};

/// Haulers (ROADMAP P3.3, ADR-025): creeps that carry a mined source's energy from its mining tile
/// to the spawns and extensions, then to the controller container. `docs/TUNING.md` gives each
/// one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HaulingConfig {
    /// Requests haulers for each mined source that sends no energy through a link, once the
    /// room's controller container stands (`economy::hauling`). Off, no new hauler is requested;
    /// live ones keep hauling, and the opening's workers carry once none is left.
    pub enabled: bool,
    /// Places the controller container's site (ADR-025) in the Optional tier, after the mining
    /// tiles' containers, once a source's miner is paid for, the room is at `container_level` and
    /// haulers are on. Off, haulers start only once a container stands within upgrading range of
    /// the controller (placed by hand).
    pub place_container: bool,
    /// The controller level from which the controller container's site is placed: below it the
    /// room's energy goes to its level's extensions and the mining tiles' containers first.
    /// Unit: level.
    pub container_level: u32,
    /// The most haulers one source gets: past it the source's container fills and its overflow
    /// piles, so the cap bounds the spawn time a long trip at a low capacity can take. Unit:
    /// creeps.
    pub max_per_source: u32,
    /// Spare carrying the sizing adds on top of the source's yield over a hauler's round, which is
    /// a lower bound (a walk can take a tick more per tile with rounding, and traffic). Unit:
    /// percent.
    pub margin_pct: u32,
}

/// Hauling values for the official MMO.
pub const HAULING_MMO: HaulingConfig = HaulingConfig {
    enabled: true,
    place_container: true,
    container_level: 3,
    max_per_source: 4,
    margin_pct: 0,
};

/// Upgraders (ROADMAP P3.4): WORK-heavy creeps at posts next to the controller container and the
/// controller links, sized to the energy that reaches the controller. `docs/TUNING.md` gives each
/// one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpgradingConfig {
    /// Requests upgraders for a room whose controller container or controller link stands and gets
    /// a source's energy (hauled or sent), sized to that energy less every other creep's body
    /// (`economy::upgrading`); the opening's workers then size no worker for those sources. Off, no
    /// new upgrader is requested; live ones keep upgrading, and the workers upgrade again.
    pub enabled: bool,
    /// Posts next to the first fed store (the controller container while a counted source is
    /// hauled) that no upgrader takes, the ones nearest the spawn: room for the haulers that unload
    /// into the container and the workers that take from it. An upgrader at its post is never
    /// shoved off it. Unit: tiles.
    pub free_tiles: u32,
    /// The most WORK parts a worker gets while upgraders take the controller's energy: the room's
    /// workers then only build, repair and fill, their body is solved within it (P3.6), and their
    /// bodies come out of the upgraders' budget. One repeat of the former 1:1:1 pattern held one
    /// WORK part. Unit: WORK parts.
    pub worker_repeats: u32,
    /// A room's miner, or hauler, counts toward the upgraders' budget for this long after the room
    /// last had one (spawning ones count): a replacement is ordered only once the creep is gone,
    /// and shows from the tick after its order, so without it each death would hand the
    /// controller's energy back to the workers for a tick or more and rebuild both plans. Unit:
    /// ticks.
    pub crew_grace_ticks: u32,
}

/// Upgrading values for the official MMO.
pub const UPGRADING_MMO: UpgradingConfig = UpgradingConfig {
    enabled: true,
    free_tiles: 2,
    worker_repeats: 2,
    crew_grace_ticks: MAX_CREEP_SIZE * CREEP_SPAWN_TIME,
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
    pub movement: MovementConfig,
    pub spawn: SpawnConfig,
    pub opening: OpeningConfig,
    pub mining: MiningConfig,
    pub links: LinkConfig,
    pub repair: RepairConfig,
    pub hauling: HaulingConfig,
    pub upgrading: UpgradingConfig,
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
        movement: MOVEMENT_MMO,
        spawn: SPAWN_MMO,
        opening: OPENING_MMO,
        mining: MINING_MMO,
        links: LINK_MMO,
        repair: REPAIR_MMO,
        hauling: HAULING_MMO,
        upgrading: UPGRADING_MMO,
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
    /// statistics window, a changed id would orphan its segment); a gate's target is read by no tick.
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
pub const FIELD_COUNT: usize = 83;

/// Every config field, in TUNING.md's order, with its safe range: the code constants that runtime
/// overrides are clamped to and that TUNING.md's safe-range column must start with. Where a range
/// also depends on another field, [`RULES`] states it.
///
/// Uses: rustedscreep_consts::{CPU_BUCKET_MAX (10000), extra::MEMORY_SEGMENT_COUNT (100),
/// MEMORY_SEGMENT_ACTIVE_LIMIT (10), extra::MEMORY_SEGMENT_SAVE_LIMIT (10), LINK_CAPACITY (800)}
/// for the bounds named after them; the config tests check each one against its constant.
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
    number!(
        log,
        "LogConfig",
        consts_findings,
        0,
        crate::consts_check::MAX_FINDINGS as u32,
        true
    ),
    flag!(cache, "CacheConfig", structure_static, true),
    flag!(cache, "CacheConfig", movement_grid, true),
    flag!(cache, "CacheConfig", flow_fields, true),
    number!(cache, "CacheConfig", naive_max_ticks, 1, 10_000, true),
    number!(cache, "CacheConfig", naive_stop_bucket, 0, 10_000, true),
    number!(movement, "MovementConfig", road_cost, 1, 25, true),
    number!(movement, "MovementConfig", plain_cost, 1, 25, true),
    number!(movement, "MovementConfig", swamp_cost, 1, 25, true),
    number!(
        movement,
        "MovementConfig",
        scan_interval_ticks,
        1,
        1000,
        true
    ),
    number!(movement, "MovementConfig", rescan_min_ticks, 1, 1000, true),
    number!(movement, "MovementConfig", field_cache_size, 1, 256, true),
    number!(
        movement,
        "MovementConfig",
        field_idle_ticks,
        1,
        10_000,
        true
    ),
    number!(movement, "MovementConfig", stuck_ticks, 1, 20, true),
    number!(
        movement,
        "MovementConfig",
        detour_max_pops,
        100,
        20_000,
        true
    ),
    number!(movement, "MovementConfig", shove_depth, 0, 3, true),
    number!(movement, "MovementConfig", shove_claims, 16, 4096, true),
    number!(
        movement,
        "MovementConfig",
        immobile_retry_ticks,
        1,
        1000,
        true
    ),
    number!(movement, "MovementConfig", suspect_ticks, 1, 1000, true),
    number!(movement, "MovementConfig", route_max_ops, 500, 50_000, true),
    number!(movement, "MovementConfig", route_max_rooms, 2, 64, true),
    number!(
        movement,
        "MovementConfig",
        route_ttl_ticks,
        100,
        20_000,
        true
    ),
    number!(movement, "MovementConfig", route_retry_ticks, 1, 5000, true),
    number!(movement, "MovementConfig", route_cache_size, 4, 1024, true),
    number!(spawn, "SpawnConfig", hold_ticks, 1, 1500, true),
    number!(opening, "OpeningConfig", max_workers, 1, 50, true),
    number!(opening, "OpeningConfig", min_workers, 1, 10, true),
    number!(opening, "OpeningConfig", wait_ticks, 50, 3000, true),
    number!(opening, "OpeningConfig", scavenge_min, 1, 1000, true),
    number!(
        opening,
        "OpeningConfig",
        downgrade_guard_ticks,
        100,
        20_000,
        true
    ),
    number!(opening, "OpeningConfig", min_upgraders, 0, 10, true),
    number!(opening, "OpeningConfig", stall_ticks, 100, 20_000, true),
    flag!(opening, "OpeningConfig", place_extensions, true),
    number!(opening, "OpeningConfig", body_slack_pct, 0, 25, true),
    flag!(mining, "MiningConfig", enabled, true),
    flag!(mining, "MiningConfig", place_containers, true),
    number!(
        mining,
        "MiningConfig",
        harvest_efficiency_pct,
        50,
        100,
        false
    ),
    flag!(links, "LinkConfig", enabled, true),
    flag!(links, "LinkConfig", place_links, true),
    number!(links, "LinkConfig", source_links, 0, 5, true),
    number!(links, "LinkConfig", send_min, 1, LINK_CAPACITY / 2, true),
    number!(links, "LinkConfig", hold_ticks, 1, CREEP_LIFE_TIME, true),
    flag!(repair, "RepairConfig", enabled, true),
    number!(repair, "RepairConfig", start_pct, 10, 99, true),
    number!(repair, "RepairConfig", walk_pct, 5, 99, true),
    flag!(hauling, "HaulingConfig", enabled, true),
    flag!(hauling, "HaulingConfig", place_container, true),
    number!(hauling, "HaulingConfig", container_level, 1, 8, true),
    number!(hauling, "HaulingConfig", max_per_source, 1, 10, true),
    number!(hauling, "HaulingConfig", margin_pct, 0, 100, true),
    flag!(upgrading, "UpgradingConfig", enabled, true),
    number!(upgrading, "UpgradingConfig", free_tiles, 0, 8, true),
    number!(upgrading, "UpgradingConfig", worker_repeats, 1, 16, true),
    number!(
        upgrading,
        "UpgradingConfig",
        crew_grace_ticks,
        1,
        CREEP_LIFE_TIME,
        true
    ),
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
pub static RULES: [Rule; 9] = [
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
        keys: ["opening.min_workers", "opening.max_workers"],
        text: "opening.min_workers must be at most opening.max_workers",
        holds: |c| c.opening.min_workers <= c.opening.max_workers,
    },
    Rule {
        keys: ["persist.self_test_segment", "persist.config_segment"],
        text: "persist.self_test_segment and persist.config_segment must differ",
        holds: |c| c.persist.self_test_segment != c.persist.config_segment,
    },
    Rule {
        keys: ["repair.walk_pct", "repair.start_pct"],
        text: "repair.walk_pct must be at most repair.start_pct",
        holds: |c| c.repair.walk_pct <= c.repair.start_pct,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use rustedscreep_consts::extra::{MEMORY_SEGMENT_COUNT, MEMORY_SEGMENT_SAVE_LIMIT};
    use rustedscreep_consts::{CPU_BUCKET_MAX, MEMORY_SEGMENT_ACTIVE_LIMIT, StructureType};

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
            consts_findings: _,
        } = LOG_MMO;
        let CacheConfig {
            structure_static: _,
            movement_grid: _,
            flow_fields: _,
            naive_max_ticks: _,
            naive_stop_bucket: _,
        } = CACHE_MMO;
        let MovementConfig {
            road_cost: _,
            plain_cost: _,
            swamp_cost: _,
            scan_interval_ticks: _,
            rescan_min_ticks: _,
            field_cache_size: _,
            field_idle_ticks: _,
            stuck_ticks: _,
            detour_max_pops: _,
            shove_depth: _,
            shove_claims: _,
            immobile_retry_ticks: _,
            suspect_ticks: _,
            route_max_ops: _,
            route_max_rooms: _,
            route_ttl_ticks: _,
            route_retry_ticks: _,
            route_cache_size: _,
        } = MOVEMENT_MMO;
        let Config {
            loader: _,
            scheduler: _,
            profiler: _,
            persist: _,
            log: _,
            cache: _,
            movement: _,
            spawn: _,
            opening: _,
            mining: _,
            links: _,
            repair: _,
            hauling: _,
            upgrading: _,
        } = MMO.config;
        let SpawnConfig { hold_ticks: _ } = SPAWN_MMO;
        let OpeningConfig {
            max_workers: _,
            min_workers: _,
            wait_ticks: _,
            scavenge_min: _,
            downgrade_guard_ticks: _,
            min_upgraders: _,
            stall_ticks: _,
            place_extensions: _,
            body_slack_pct: _,
        } = OPENING_MMO;
        let MiningConfig {
            enabled: _,
            place_containers: _,
            harvest_efficiency_pct: _,
        } = MINING_MMO;
        let LinkConfig {
            enabled: _,
            place_links: _,
            source_links: _,
            send_min: _,
            hold_ticks: _,
        } = LINK_MMO;
        let RepairConfig {
            enabled: _,
            start_pct: _,
            walk_pct: _,
        } = REPAIR_MMO;
        let HaulingConfig {
            enabled: _,
            place_container: _,
            container_level: _,
            max_per_source: _,
            margin_pct: _,
        } = HAULING_MMO;
        let UpgradingConfig {
            enabled: _,
            free_tiles: _,
            worker_repeats: _,
            crew_grace_ticks: _,
        } = UPGRADING_MMO;
        let expected: [(&str, &[&str]); 14] = [
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
            ("log", &["level", "skip_window_ticks", "consts_findings"]),
            (
                "cache",
                &[
                    "structure_static",
                    "movement_grid",
                    "flow_fields",
                    "naive_max_ticks",
                    "naive_stop_bucket",
                ],
            ),
            (
                "movement",
                &[
                    "road_cost",
                    "plain_cost",
                    "swamp_cost",
                    "scan_interval_ticks",
                    "rescan_min_ticks",
                    "field_cache_size",
                    "field_idle_ticks",
                    "stuck_ticks",
                    "detour_max_pops",
                    "shove_depth",
                    "shove_claims",
                    "immobile_retry_ticks",
                    "suspect_ticks",
                    "route_max_ops",
                    "route_max_rooms",
                    "route_ttl_ticks",
                    "route_retry_ticks",
                    "route_cache_size",
                ],
            ),
            ("spawn", &["hold_ticks"]),
            (
                "opening",
                &[
                    "max_workers",
                    "min_workers",
                    "wait_ticks",
                    "scavenge_min",
                    "downgrade_guard_ticks",
                    "min_upgraders",
                    "stall_ticks",
                    "place_extensions",
                    "body_slack_pct",
                ],
            ),
            (
                "mining",
                &["enabled", "place_containers", "harvest_efficiency_pct"],
            ),
            (
                "links",
                &[
                    "enabled",
                    "place_links",
                    "source_links",
                    "send_min",
                    "hold_ticks",
                ],
            ),
            ("repair", &["enabled", "start_pct", "walk_pct"]),
            (
                "hauling",
                &[
                    "enabled",
                    "place_container",
                    "container_level",
                    "max_per_source",
                    "margin_pct",
                ],
            ),
            (
                "upgrading",
                &[
                    "enabled",
                    "free_tiles",
                    "worker_repeats",
                    "crew_grace_ticks",
                ],
            ),
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
        assert_eq!(range("cache.naive_stop_bucket"), (0, CPU_BUCKET_MAX));
        // Above half a link, a send needs a sender and a receiver that are both nearly at the end
        // of their store at once (the native scenario stalls at `LINK_CAPACITY`).
        assert_eq!(range("links.send_min"), (1, LINK_CAPACITY / 2));
        // Every link the top level allows but the controller link.
        assert_eq!(
            range("links.source_links"),
            (0, StructureType::Link.controller_structures(8) - 1)
        );
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

    /// Uses: rustedscreep_consts::CPU_TICK_LIMIT_MAX. tickLimit = min(CPU_TICK_LIMIT_MAX, bucket + limit)
    /// (V37), and the free-tier limit is 20 (V35). The first boot tick (require, decode, CRC-32,
    /// compile) runs outside the loader's `running` flag, so a kill there is no incident: the gate gives
    /// that tick the full tickLimit, and any higher gate would only defer the boot (V150).
    #[test]
    fn boot_gate_is_the_lowest_bucket_with_the_full_tick_limit() {
        let limit = 20;
        let tick_limit = |bucket: u32| CPU_TICK_LIMIT_MAX.min(bucket + limit);
        let gate = LOADER_MMO.boot_bucket_min;
        assert_eq!(tick_limit(gate), CPU_TICK_LIMIT_MAX);
        assert!(tick_limit(gate - 1) < CPU_TICK_LIMIT_MAX);
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

    /// Uses: PROFILER_MMO.window_loops, SCHEDULER_MMO (the Optional budget, `Budget::compute`),
    /// rustedscreep_consts::CPU_TICK_LIMIT_MAX (ADR-022). Every profile runs with every cache on
    /// (only an override turns one off, which the naive guard can remove); the `mmo` naive span
    /// fills one profiler window; at its stop bucket, at the free tier's limit of 20 (V35) and after
    /// the warm-up, the Optional tier still runs, so a span ends before Optional work stops.
    #[test]
    fn cache_values_are_coherent() {
        for p in &PROFILES {
            assert!(
                !p.config.cache.naive(),
                "{}: a profile keeps every cache on",
                p.name
            );
        }
        let c = CACHE_MMO;
        assert_eq!(c.naive_max_ticks, PROFILER_MMO.window_loops);
        let limit = 20;
        let ctx = crate::snapshot::TickContext::from_raw(
            1,
            limit,
            f64::from(CPU_TICK_LIMIT_MAX.min(c.naive_stop_bucket + limit)),
            i32::try_from(c.naive_stop_bucket).expect("fits"),
        );
        let budget = crate::scheduler::Budget::compute(
            &ctx,
            &SCHEDULER_MMO,
            SCHEDULER_MMO.optional_warmup_loops,
        );
        assert!(budget.allows(crate::scheduler::Tier::Optional));
        for (s, m, f) in [
            (false, true, true),
            (true, false, true),
            (true, true, false),
            (false, false, false),
        ] {
            let c = CacheConfig {
                structure_static: s,
                movement_grid: m,
                flow_fields: f,
                ..CACHE_MMO
            };
            assert!(c.naive(), "{s} {m} {f}");
        }
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

    /// Uses: MOVE_COST_ROAD, MOVE_COST_PLAIN, MOVE_COST_SWAMP, ROOM_AREA. The default costs are
    /// the engine's fatigue rates (V54, V154) and fit a `u8`; the highest cost allowed keeps every
    /// in-room distance below the flow field's `u16` "unreached" value; a detour may pop at least
    /// one entry per tile.
    #[test]
    fn movement_values_are_coherent() {
        let c = MOVEMENT_MMO;
        assert_eq!(u32::from(c.road_cost), MOVE_COST_ROAD);
        assert_eq!(u32::from(c.plain_cost), MOVE_COST_PLAIN);
        assert_eq!(u32::from(c.swamp_cost), MOVE_COST_SWAMP);
        let max_cost = match by_key("movement.swamp_cost").kind {
            Kind::Number { max, .. } => max,
            _ => unreachable!("a number"),
        };
        for key in ["movement.road_cost", "movement.plain_cost"] {
            assert_eq!(
                by_key(key).kind,
                by_key("movement.swamp_cost").kind,
                "{key}"
            );
        }
        let area = u32::try_from(rustedscreep_consts::ROOM_AREA).expect("fits");
        assert!(
            (area - 1) * max_cost < u32::from(u16::MAX),
            "a field's distance fits below UNREACHED"
        );
        assert!(c.rescan_min_ticks <= c.scan_interval_ticks);
        assert!(c.stuck_ticks >= 1 && c.field_cache_size >= 1);
        assert!(
            c.detour_max_pops >= area / 2,
            "a detour can cover a good part of a room"
        );
        assert!(c.shove_depth <= 3, "the chain search grows as 8^depth");
        // At the default depth the whole chain search fits the claim budget, so the budget only
        // binds at a deeper setting: a claim at depth d may relocate its occupant to 8 tiles, each a
        // claim at depth d - 1, so claims(d) = 1 + 8 claims(d - 1) = (8^(d+1) - 1) / 7, and a mover
        // tries 3 steps.
        let full = 3 * (8u32.pow(u32::from(c.shove_depth) + 1) - 1) / 7;
        assert_eq!(full, 219, "depth 2");
        assert!(c.shove_claims >= full, "{} < {full}", c.shove_claims);
        // PathFinder's own limits (V163): maxRooms is clamped to 64, and fewer than 2 rooms is no
        // inter-room route. A route lasts about one traveller's life.
        let rooms = match by_key("movement.route_max_rooms").kind {
            Kind::Number { min, max } => (min, max),
            _ => unreachable!("a number"),
        };
        assert_eq!(rooms, (2, 64));
        assert_eq!(c.route_ttl_ticks, CREEP_LIFE_TIME);
        assert!(c.route_retry_ticks <= c.route_ttl_ticks);
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
