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
    CPU_TICK_LIMIT_MAX, CREEP_LIFE_TIME, CREEP_SPAWN_TIME, LINK_CAPACITY, MAX_CONSTRUCTION_SITES,
    MAX_CREEP_SIZE, MOVE_COST_PLAIN, MOVE_COST_ROAD, MOVE_COST_SWAMP, RAMPART_DECAY_AMOUNT,
    RAMPART_DECAY_TIME, RAMPART_HITS_MAX_RCL2, ROOM_SIZE, TOWER_CAPACITY,
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
    /// The segment that keeps the planner's record across global resets: each owned room's plan
    /// anchor and adopted containers and links (`planner::record`, ADR-027; 0 to 99). Unit:
    /// segment id.
    pub planner_segment: u8,
    /// The segment that keeps the controller signer's record across global resets: per room the
    /// sign it showed, when it was seen, its controller's tile, the seen towers, and the signers
    /// lost there (`signing::Book`, ADR-030; 0 to 99). Unit: segment id.
    pub sign_segment: u8,
}

/// Segment manager values for the official MMO.
pub const PERSIST_MMO: PersistConfig = PersistConfig {
    max_active_segments: 8,
    load_retries: 3,
    max_writes_per_tick: 2,
    self_test_segment: 98,
    config_segment: 97,
    planner_segment: 96,
    sign_segment: 95,
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

/// Spawn queue thresholds (ROADMAP P2's minimal spawn path, P5's global queue); `docs/TUNING.md`
/// gives the rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnConfig {
    /// A spawn whose `spawnCreep` call the game refused (or that threw) takes no order for this
    /// long, so another spawn of its room can take the request. Unit: ticks.
    pub hold_ticks: u32,
    /// A request whose home is an owned room with no own spawn is served by a room with one at most
    /// this far away (`spawning::room_distance`, the larger of the two room-coordinate differences)
    /// whose capacity fits its body: the nearest that starts the tick full with a ready spawn while
    /// the Standard tier runs, else the nearest, where it waits. 0: no room spawns for another
    /// (P5.1). Default 3: a helped creep's best walk, `ROOM_SIZE` ticks a room, stays within a
    /// tenth of `CREEP_LIFE_TIME`; safe range 0 to 10. Unit: rooms.
    pub help_range: u32,
    /// Replace creeps that hold a post before they die (P5.2, `spawning::lead`): the miners, the
    /// haulers and the upgraders count a creep whose `ticksToLive` is within its successor's spawn
    /// time, walk and `replace_margin_ticks` as due and ask for the successor while it still works
    /// (the opening's workers, a pool, are not replaced early: measured, TUNING). Off, a job asks
    /// only once the creep is gone, and each handover leaves the post empty for the successor's
    /// spawn time and walk. Unit: flag.
    pub replace: bool,
    /// What a replacement's lead adds to the successor's spawn time and walk (P5.2): the ticks its
    /// order may wait for energy or a spawn, where the spawn puts the creep, and a miner's
    /// successor's step around the held mining tile (the walk itself is an upper bound on the
    /// plan's way, `economy::sizing::walk_ticks_max`). The successor's
    /// early ticks cost this much of its life (20 of `CREEP_LIFE_TIME`, 1.3% of a body); an order
    /// that waits longer leaves the post empty for the rest. The lead stays at most half a life
    /// whatever this is. Default 20; safe range 0 to 300. Unit: ticks.
    pub replace_margin_ticks: u32,
    /// While a room's spawn queue waits for energy (P5.3, `Spawner::waits_for_energy`), bring it
    /// the energy that lies in the room's stores: its workers fill the spawns and extensions even
    /// while haulers fill, taking from piles and containers, and while the spawns have more room
    /// than the haulers hold, an empty hauler with too little at its own mining tile for a load
    /// takes one from a mining tile no hauler takes from or from the controller container (never
    /// storing that load back). Off, the queue waits for what the haulers bring from their sources.
    /// Unit: flag.
    pub refill_from_stores: bool,
}

/// Spawn queue values for the official MMO.
pub const SPAWN_MMO: SpawnConfig = SpawnConfig {
    hold_ticks: 20,
    help_range: 3,
    replace: true,
    replace_margin_ticks: 20,
    refill_from_stores: true,
};

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
    /// tier, in a room the interim placement serves (`PlannerConfig::enabled` off, or a room with
    /// no valid plan). A room with a ready plan places its extensions from the plan whatever this
    /// says (P4.3, ADR-026). Off, such a room builds only sites placed by hand.
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
    /// this long: every check the game makes is modeled (V216), so a refusal means something the
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
    /// The most haulers one source gets, besides those due for replacement (P5.2: a due hauler no
    /// longer counts, so its successor is ordered while it still hauls): past it the source's
    /// container fills and its overflow piles, so the cap bounds the spawn time a long trip at a
    /// low capacity can take. Unit: creeps.
    pub max_per_source: u32,
    /// Spare carrying the sizing adds on top of the source's yield over a hauler's round, which is
    /// a lower bound (a walk can take a tick more per tile with rounding, and traffic). Unit:
    /// percent.
    pub margin_pct: u32,
    /// A tower is filled (by haulers, or by the opening's workers where they fill) once its spawns
    /// and extensions are full and the tower has at least this much room, so no trip is made for a
    /// shot or two (P7.2). Unit: energy.
    pub tower_refill_min: u32,
}

/// Hauling values for the official MMO.
pub const HAULING_MMO: HaulingConfig = HaulingConfig {
    enabled: true,
    place_container: true,
    container_level: 3,
    max_per_source: 4,
    margin_pct: 0,
    tower_refill_min: 200,
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
    /// last had one (spawning ones count): with `spawn.replace` off (P5.2), or a replacement whose
    /// order waits until the creep is gone, the replacement shows only from the tick after its
    /// order, so without it each death would hand the controller's energy back to the workers for
    /// a tick or more and rebuild both plans. Unit: ticks.
    pub crew_grace_ticks: u32,
}

/// Upgrading values for the official MMO.
pub const UPGRADING_MMO: UpgradingConfig = UpgradingConfig {
    enabled: true,
    free_tiles: 2,
    worker_repeats: 2,
    crew_grace_ticks: MAX_CREEP_SIZE * CREEP_SPAWN_TIME,
};

/// The base planner (ROADMAP P4.3, ADR-007, ADR-026): each owned room with a spawn is planned once a
/// global by `planner::flood` (Optional, a phase a step) and its sites are placed from the plan.
/// `docs/TUNING.md` gives each one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannerConfig {
    /// Plans each owned room with a spawn and places its sites from the plan
    /// (`planner::build`): the plan's spawns, extensions and roads, and the containers and links
    /// `economy::mining` asks for, on the plan's tiles. Off, the interim placement of ADR-020 and
    /// ADR-023 to ADR-025 runs as before; on, it waits for the room's plan and runs only for a room
    /// that has none.
    pub enabled: bool,
    /// Destroys an own structure that stands off the plan where a planned structure due at the
    /// room's level and placed by the bot needs its tile, or that cuts one off from the plan's
    /// first spawn (ADR-026): one at a time per room, never a spawn, never with a hostile creep in
    /// the room. A due road counts too (ADR-026's amendment, 2026-11-18), but clears only an
    /// off-plan extension (never a link, tower, storage or late structure, whose store or service a
    /// destroy would lose). Off, such a planned structure waits.
    pub replace: bool,
    /// Places the storage and the hub link (nothing uses them before P5 and P6; the user's choice,
    /// 2026-11-18). Like every gate here, off it also leaves out the plan's roads that serve only
    /// what it gates (`planner::layout::RoadNeed`).
    pub storage: bool,
    /// Places the towers (filled and fired since P7.2).
    pub towers: bool,
    /// Places the ramparts (when ramparts are built is P7's, ADR-007; on since P7.3, ADR-032, whose
    /// workers keep them at [`UpkeepConfig::rampart_permille`] of their level's hits).
    pub ramparts: bool,
    /// Places the terminal, the extractor and its container, the labs, the factory, the power spawn,
    /// the nuker and the observer (nothing uses them before P9).
    pub late: bool,
    /// The most own construction sites a room may hold before the placement waits: one site a
    /// tick goes out, first the economy's containers and links on the plan's tiles (ADR-023 to
    /// ADR-025's timing), then the plan's items by type (spawns, extensions, ... roads). Unit:
    /// sites.
    pub max_sites: u32,
    /// How long a room whose placement found nothing to do waits before it looks again when
    /// nothing it watches changed (the level, the room's structures, sites and containers, the
    /// number of footprints in movement's last scan of it). It also bounds three holds: the
    /// room's destroys after an `ERR_BUSY`, a type refused with `ERR_RCL_NOT_ENOUGH` (or until
    /// the level changes), and a destroy's replacement that never gets its site. Unit: ticks.
    pub verify_ticks: u32,
    /// The distance-transform value (over open, not-kept-clear ground) a hub needs in a fresh room
    /// (`planner::flood`). Unit: tiles (Chebyshev).
    pub hub_clearance: u8,
    /// The extra steps the fill counts to a slot within ranged reach of ground no rampart can close
    /// off (`planner::flood`). Unit: steps along the road network.
    pub exposed_penalty: u8,
}

/// Planner values for the official MMO.
pub const PLANNER_MMO: PlannerConfig = PlannerConfig {
    enabled: true,
    replace: true,
    storage: false,
    towers: true,
    ramparts: true,
    late: false,
    max_sites: 10,
    verify_ticks: 500,
    hub_clearance: 3,
    exposed_penalty: 8,
};

/// Remote mining (ROADMAP P6): the prices of the profit model (`economy::remote_profit`) and its
/// choice of a plan for a room (`economy::best_plan`), and the remote manager's limits (P6.2,
/// ADR-029, `economy::Remotes`). `docs/TUNING.md` gives each one's rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteConfig {
    /// Runs remote mining: the scouts, the choice of remote rooms and their creeps' requests. Off,
    /// no remote creep is requested, and the ones alive keep working until they die.
    pub enabled: bool,
    /// The most remote rooms one home mines at once. Unit: rooms.
    pub max_rooms: u32,
    /// The least controller level a home mines remote rooms at. Unit: RCL.
    pub min_level: u32,
    /// The most of each of the home's spawns' time its remote rooms' creeps may take, by the
    /// profit model's spawn ticks over a creep life. Unit: per mille of a spawn's time.
    pub spawn_permille: u32,
    /// The most own construction sites one remote room holds before its placement waits (one site
    /// a tick goes out: the containers, then the roads). Unit: sites.
    pub max_sites: u32,
    /// A remote miner repairs its container below this share of `CONTAINER_HITS`. Unit: percent.
    pub container_repair_pct: u32,
    /// The snapshot reads a remote room's roads (for their hits) every this many ticks, on the
    /// ticks divisible by it. Unit: ticks.
    pub road_read_ticks: u32,
    /// A candidate room seen longer ago than this, and not mined, is scouted again when the home
    /// wants another remote room. Unit: ticks.
    pub rescout_ticks: u32,
    /// A mined room where armed hostile creeps are seen has its creeps wait in the home this long
    /// after the last sight of them (P6.3; P7's defenders may end it sooner). Unit: ticks.
    pub evacuate_ticks: u32,
    /// An abandoned room is not chosen again for this long (P6.3), and the threats that abandon a
    /// room are counted within it. Unit: ticks.
    pub abandon_ticks: u32,
    /// This many threats within `abandon_ticks` abandon a room (P6.3). Unit: threats.
    pub contest_threats: u32,
    /// The window over which a mined room's delivered energy is set against its creeps' bodies
    /// (P6.3); a window that delivered less abandons the room, but not a room's first window in a
    /// global. Unit: ticks.
    pub audit_ticks: u32,
    /// The bot's own CPU for each remote creep each tick it is alive (its share of the snapshot,
    /// the decisions and the act step's calls), on top of `INTENT_CPU_COST` for each billed intent.
    /// Unit: µCPU (millionths of a CPU).
    pub creep_ucpu: u32,
    /// The ticks a remote's one-time builds (a container not standing yet, the road tiles not
    /// built yet) are spread over: what a remote is expected to be held. A longer horizon favors
    /// roads. Unit: ticks.
    pub horizon_ticks: u32,
    /// The best plan for a room is the one with the least CPU among those whose net energy is
    /// within this share of the best plan's gross yield of the best net (the model's walks are
    /// lower bounds). Unit: percent.
    pub slack_pct: u32,
}

/// Remote mining values for the official MMO.
pub const REMOTE_MMO: RemoteConfig = RemoteConfig {
    enabled: true,
    max_rooms: 2,
    min_level: 3,
    spawn_permille: 400,
    max_sites: 5,
    container_repair_pct: 80,
    road_read_ticks: 5,
    rescout_ticks: 20_000,
    evacuate_ticks: 1500,
    abandon_ticks: 20_000,
    contest_threats: 3,
    audit_ticks: 3000,
    creep_ucpu: 10_000,
    horizon_ticks: 30_000,
    slack_pct: 2,
};

/// The controller signer (ADR-030, `signing::Signing`): one cheap creep for the whole bot that signs
/// every controller it can reach with `signing::SIGN_TEXT`. `docs/TUNING.md` gives each one's
/// rationale and safe range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignConfig {
    /// Runs the signer: its requests, its trips and its signs. Off, no signer is requested, and one
    /// alive stands where it is until it dies.
    pub enabled: bool,
    /// The least controller level a home has before it sends signers: the opening's energy goes to
    /// the economy first (remote mining starts at the same level, `remote.min_level`). Unit: RCL.
    pub min_level: u32,
    /// The farthest a candidate room lies from a home, in rooms (the larger of the two room
    /// coordinate differences). The default, `CREEP_LIFE_TIME / (ROOM_SIZE - 1) + 1`, is the most
    /// rooms a straight walk crosses in one life; the route's length is the real bound. Unit: rooms.
    pub max_distance: u32,
    /// A room seen with the text is looked at again after this long, for a sign another player
    /// replaced. Unit: ticks.
    pub recheck_ticks: u32,
    /// A room given up on (no route, no body that survives its towers, the server's own sign, too
    /// many signers lost or signs replaced) is tried again after this long. Unit: ticks.
    pub retry_ticks: u32,
    /// Signers lost in one room before it waits `retry_ticks`. Unit: creeps.
    pub max_deaths: u32,
    /// Times the text was found replaced in one room before it waits `retry_ticks`. Unit: signs.
    pub max_overwrites: u32,
    /// Added to the modeled tower damage before a body is sized for a towered room. Unit: percent.
    pub damage_margin_pct: u32,
    /// Ticks of life a trip must leave spare by the walk's estimate. Unit: ticks.
    pub life_margin_ticks: u32,
    /// PathFinder's `maxOps` for a trip's route search (V163). Unit: ops.
    pub route_max_ops: u32,
    /// PathFinder's `maxRooms` for a trip's route search (at most 64, V163). Unit: rooms.
    pub route_max_rooms: u32,
    /// The record is written at most once per this many ticks while it has changes. Unit: ticks.
    pub save_ticks: u32,
}

/// Signer values for the official MMO.
pub const SIGN_MMO: SignConfig = SignConfig {
    enabled: true,
    min_level: 3,
    max_distance: CREEP_LIFE_TIME / (ROOM_SIZE as u32 - 1) + 1,
    recheck_ticks: 100_000,
    retry_ticks: 100_000,
    max_deaths: 2,
    max_overwrites: 2,
    damage_margin_pct: 25,
    life_margin_ticks: 50,
    route_max_ops: 10_000,
    route_max_rooms: 64,
    save_ticks: 100,
};

/// Defense (ROADMAP P7, `defense::Threats`). `docs/TUNING.md` gives each one's rationale and safe
/// range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefenseConfig {
    /// A room's threat ends once no dangerous hostile has been seen there for this long, so a
    /// hostile that steps out over the edge and back stays one threat. Unit: ticks.
    pub threat_end_ticks: u32,
    /// The least time between two log lines that a room's threat changed (its count of dangerous or
    /// boosted hostiles). Unit: ticks.
    pub threat_log_ticks: u32,
    /// The most ticks the combat sim may take to kill a hostile for the towers to fire at it (P7.2):
    /// a target that takes longer, or never dies (its heal matches the towers), is not shot, since
    /// each shot costs `TOWER_ENERGY_COST`. Unit: ticks.
    pub max_kill_ticks: u32,
    /// A tower heals own creeps only while it holds at least this, keeping the rest for shots
    /// (P7.2). Unit: energy.
    pub heal_reserve: u32,
    /// The towers stop firing at a creep whose hits came out above the sim's on this many ticks in
    /// a row (P7.2: a rampart on its tile, a power effect, an inactive tower). Unit: ticks.
    pub miss_ticks: u32,
}

/// Defense values for the official MMO.
pub const DEFENSE_MMO: DefenseConfig = DefenseConfig {
    threat_end_ticks: 20,
    threat_log_ticks: 50,
    max_kill_ticks: 50,
    heal_reserve: TOWER_CAPACITY / 2,
    miss_ticks: 3,
};

/// Upkeep (ROADMAP P7.3, ADR-032): the opening's workers repair the roads and own ramparts of their
/// owned rooms (the containers are [`RepairConfig`]'s). Towers repair nothing (the user's choice,
/// 2026-11-23: their energy is for fights). `docs/TUNING.md` gives each one's rationale and safe
/// range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpkeepConfig {
    /// Lets the workers repair roads and ramparts. Off, neither is read or repaired, and a new
    /// rampart is gone at its first decay (V304).
    pub enabled: bool,
    /// Every this many ticks the snapshot reads every road's and own rampart's hits in each owned
    /// room; between reads only the one a worker stands next to is read. Below
    /// `RAMPART_DECAY_TIME`, so no rampart takes two decay steps between reads. Unit: ticks.
    pub read_ticks: u32,
    /// A road is repaired once its hits fall below this share of its `hitsMax`, and then up to
    /// full, when the room has no site to build. Unit: percent.
    pub road_start_pct: u32,
    /// Below this share of its `hitsMax` a road is repaired before the room's sites are built.
    /// At most `road_start_pct`. Unit: percent.
    pub road_urgent_pct: u32,
    /// A rampart is kept at this share of `rampart_hits_max` at its room's level (V236). Unit:
    /// permille.
    pub rampart_permille: u32,
    /// A rampart is raised to its target once it falls below this share of it, when the room has
    /// no site to build. Unit: percent.
    pub rampart_start_pct: u32,
    /// Below this many hits (or its target, if lower) a rampart is repaired before the room's
    /// sites are built: a new one has `RAMPART_HITS` (1) and is gone at its first decay step,
    /// about `RAMPART_DECAY_TIME` ticks after it is built (V302, V304). Unit: hits.
    pub rampart_floor: u32,
}

/// Upkeep values for the official MMO.
pub const UPKEEP_MMO: UpkeepConfig = UpkeepConfig {
    enabled: true,
    read_ticks: 50,
    road_start_pct: 50,
    road_urgent_pct: 20,
    rampart_permille: 10,
    rampart_start_pct: 90,
    rampart_floor: 10 * RAMPART_DECAY_AMOUNT,
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
    pub planner: PlannerConfig,
    pub remote: RemoteConfig,
    pub sign: SignConfig,
    pub defense: DefenseConfig,
    pub upkeep: UpkeepConfig,
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
        planner: PLANNER_MMO,
        remote: REMOTE_MMO,
        sign: SIGN_MMO,
        defense: DEFENSE_MMO,
        upkeep: UPKEEP_MMO,
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
pub const FIELD_COUNT: usize = 139;

/// Every config field, in TUNING.md's order, with its safe range: the code constants that runtime
/// overrides are clamped to and that TUNING.md's safe-range column must start with. Where a range
/// also depends on another field, [`RULES`] states it.
///
/// Uses: rustedscreep_consts::{CPU_BUCKET_MAX (10000), extra::MEMORY_SEGMENT_COUNT (100),
/// MEMORY_SEGMENT_ACTIVE_LIMIT (10), extra::MEMORY_SEGMENT_SAVE_LIMIT (10), LINK_CAPACITY (800),
/// MAX_CONSTRUCTION_SITES (100)}
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
    number!(persist, "PersistConfig", planner_segment, 0, 99, false),
    number!(persist, "PersistConfig", sign_segment, 0, 99, false),
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
    number!(spawn, "SpawnConfig", help_range, 0, 10, true),
    flag!(spawn, "SpawnConfig", replace, true),
    number!(spawn, "SpawnConfig", replace_margin_ticks, 0, 300, true),
    flag!(spawn, "SpawnConfig", refill_from_stores, true),
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
    number!(
        hauling,
        "HaulingConfig",
        tower_refill_min,
        1,
        TOWER_CAPACITY,
        true
    ),
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
    flag!(planner, "PlannerConfig", enabled, true),
    flag!(planner, "PlannerConfig", replace, true),
    flag!(planner, "PlannerConfig", storage, true),
    flag!(planner, "PlannerConfig", towers, true),
    flag!(planner, "PlannerConfig", ramparts, true),
    flag!(planner, "PlannerConfig", late, true),
    number!(
        planner,
        "PlannerConfig",
        max_sites,
        1,
        MAX_CONSTRUCTION_SITES,
        true
    ),
    number!(planner, "PlannerConfig", verify_ticks, 10, 10_000, true),
    number!(planner, "PlannerConfig", hub_clearance, 2, 5, false),
    number!(planner, "PlannerConfig", exposed_penalty, 0, 50, false),
    flag!(remote, "RemoteConfig", enabled, true),
    number!(remote, "RemoteConfig", max_rooms, 1, 8, true),
    number!(remote, "RemoteConfig", min_level, 1, 8, true),
    number!(remote, "RemoteConfig", spawn_permille, 0, 1000, true),
    number!(
        remote,
        "RemoteConfig",
        max_sites,
        1,
        MAX_CONSTRUCTION_SITES,
        true
    ),
    number!(remote, "RemoteConfig", container_repair_pct, 1, 99, true),
    number!(remote, "RemoteConfig", road_read_ticks, 1, 100, true),
    number!(
        remote,
        "RemoteConfig",
        rescout_ticks,
        CREEP_LIFE_TIME,
        1_500_000,
        true
    ),
    number!(
        remote,
        "RemoteConfig",
        evacuate_ticks,
        1,
        CREEP_LIFE_TIME * 10,
        true
    ),
    number!(
        remote,
        "RemoteConfig",
        abandon_ticks,
        CREEP_LIFE_TIME,
        1_500_000,
        true
    ),
    number!(remote, "RemoteConfig", contest_threats, 1, 100, true),
    number!(
        remote,
        "RemoteConfig",
        audit_ticks,
        CREEP_LIFE_TIME,
        150_000,
        true
    ),
    number!(remote, "RemoteConfig", creep_ucpu, 0, 1_000_000, true),
    number!(
        remote,
        "RemoteConfig",
        horizon_ticks,
        CREEP_LIFE_TIME,
        1_500_000,
        true
    ),
    number!(remote, "RemoteConfig", slack_pct, 0, 25, true),
    flag!(sign, "SignConfig", enabled, true),
    number!(sign, "SignConfig", min_level, 1, 8, true),
    number!(sign, "SignConfig", max_distance, 1, 64, true),
    number!(
        sign,
        "SignConfig",
        recheck_ticks,
        CREEP_LIFE_TIME,
        10_000_000,
        true
    ),
    number!(
        sign,
        "SignConfig",
        retry_ticks,
        CREEP_LIFE_TIME,
        10_000_000,
        true
    ),
    number!(sign, "SignConfig", max_deaths, 1, 10, true),
    number!(sign, "SignConfig", max_overwrites, 1, 10, true),
    number!(sign, "SignConfig", damage_margin_pct, 0, 200, true),
    number!(
        sign,
        "SignConfig",
        life_margin_ticks,
        0,
        CREEP_LIFE_TIME / 2,
        true
    ),
    number!(sign, "SignConfig", route_max_ops, 1000, 100_000, true),
    number!(sign, "SignConfig", route_max_rooms, 1, 64, true),
    number!(sign, "SignConfig", save_ticks, 1, 10_000, true),
    number!(
        defense,
        "DefenseConfig",
        threat_end_ticks,
        1,
        CREEP_LIFE_TIME,
        true
    ),
    number!(defense, "DefenseConfig", threat_log_ticks, 1, 10_000, true),
    number!(
        defense,
        "DefenseConfig",
        max_kill_ticks,
        1,
        CREEP_LIFE_TIME / 3,
        true
    ),
    number!(
        defense,
        "DefenseConfig",
        heal_reserve,
        0,
        TOWER_CAPACITY,
        true
    ),
    number!(defense, "DefenseConfig", miss_ticks, 1, 100, true),
    flag!(upkeep, "UpkeepConfig", enabled, true),
    number!(
        upkeep,
        "UpkeepConfig",
        read_ticks,
        10,
        RAMPART_DECAY_TIME - 1,
        true
    ),
    number!(upkeep, "UpkeepConfig", road_start_pct, 5, 99, true),
    number!(upkeep, "UpkeepConfig", road_urgent_pct, 1, 99, true),
    number!(upkeep, "UpkeepConfig", rampart_permille, 1, 1000, true),
    number!(upkeep, "UpkeepConfig", rampart_start_pct, 10, 99, true),
    number!(
        upkeep,
        "UpkeepConfig",
        rampart_floor,
        RAMPART_DECAY_AMOUNT + 1,
        RAMPART_HITS_MAX_RCL2,
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
pub static RULES: [Rule; 17] = [
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
        keys: ["persist.planner_segment", "profiler.stats_segment"],
        text: "persist.planner_segment and profiler.stats_segment must differ",
        holds: |c| c.persist.planner_segment != c.profiler.stats_segment,
    },
    Rule {
        keys: ["persist.planner_segment", "persist.self_test_segment"],
        text: "persist.planner_segment and persist.self_test_segment must differ",
        holds: |c| c.persist.planner_segment != c.persist.self_test_segment,
    },
    Rule {
        keys: ["persist.planner_segment", "persist.config_segment"],
        text: "persist.planner_segment and persist.config_segment must differ",
        holds: |c| c.persist.planner_segment != c.persist.config_segment,
    },
    Rule {
        keys: ["persist.sign_segment", "profiler.stats_segment"],
        text: "persist.sign_segment and profiler.stats_segment must differ",
        holds: |c| c.persist.sign_segment != c.profiler.stats_segment,
    },
    Rule {
        keys: ["persist.sign_segment", "persist.self_test_segment"],
        text: "persist.sign_segment and persist.self_test_segment must differ",
        holds: |c| c.persist.sign_segment != c.persist.self_test_segment,
    },
    Rule {
        keys: ["persist.sign_segment", "persist.config_segment"],
        text: "persist.sign_segment and persist.config_segment must differ",
        holds: |c| c.persist.sign_segment != c.persist.config_segment,
    },
    Rule {
        keys: ["persist.sign_segment", "persist.planner_segment"],
        text: "persist.sign_segment and persist.planner_segment must differ",
        holds: |c| c.persist.sign_segment != c.persist.planner_segment,
    },
    Rule {
        keys: ["repair.walk_pct", "repair.start_pct"],
        text: "repair.walk_pct must be at most repair.start_pct",
        holds: |c| c.repair.walk_pct <= c.repair.start_pct,
    },
    Rule {
        keys: ["upkeep.road_urgent_pct", "upkeep.road_start_pct"],
        text: "upkeep.road_urgent_pct must be at most upkeep.road_start_pct",
        holds: |c| c.upkeep.road_urgent_pct <= c.upkeep.road_start_pct,
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
            planner_segment: _,
            sign_segment: _,
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
            planner: _,
            remote: _,
            sign: _,
            defense: _,
            upkeep: _,
        } = MMO.config;
        let SpawnConfig {
            hold_ticks: _,
            help_range: _,
            replace: _,
            replace_margin_ticks: _,
            refill_from_stores: _,
        } = SPAWN_MMO;
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
            tower_refill_min: _,
        } = HAULING_MMO;
        let UpgradingConfig {
            enabled: _,
            free_tiles: _,
            worker_repeats: _,
            crew_grace_ticks: _,
        } = UPGRADING_MMO;
        let PlannerConfig {
            enabled: _,
            replace: _,
            storage: _,
            towers: _,
            ramparts: _,
            late: _,
            max_sites: _,
            verify_ticks: _,
            hub_clearance: _,
            exposed_penalty: _,
        } = PLANNER_MMO;
        let RemoteConfig {
            enabled: _,
            max_rooms: _,
            min_level: _,
            spawn_permille: _,
            max_sites: _,
            container_repair_pct: _,
            road_read_ticks: _,
            rescout_ticks: _,
            evacuate_ticks: _,
            abandon_ticks: _,
            contest_threats: _,
            audit_ticks: _,
            creep_ucpu: _,
            horizon_ticks: _,
            slack_pct: _,
        } = REMOTE_MMO;
        let SignConfig {
            enabled: _,
            min_level: _,
            max_distance: _,
            recheck_ticks: _,
            retry_ticks: _,
            max_deaths: _,
            max_overwrites: _,
            damage_margin_pct: _,
            life_margin_ticks: _,
            route_max_ops: _,
            route_max_rooms: _,
            save_ticks: _,
        } = SIGN_MMO;
        let DefenseConfig {
            threat_end_ticks: _,
            threat_log_ticks: _,
            max_kill_ticks: _,
            heal_reserve: _,
            miss_ticks: _,
        } = DEFENSE_MMO;
        let UpkeepConfig {
            enabled: _,
            read_ticks: _,
            road_start_pct: _,
            road_urgent_pct: _,
            rampart_permille: _,
            rampart_start_pct: _,
            rampart_floor: _,
        } = UPKEEP_MMO;
        let expected: [(&str, &[&str]); 19] = [
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
                    "planner_segment",
                    "sign_segment",
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
            (
                "spawn",
                &[
                    "hold_ticks",
                    "help_range",
                    "replace",
                    "replace_margin_ticks",
                    "refill_from_stores",
                ],
            ),
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
                    "tower_refill_min",
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
            (
                "planner",
                &[
                    "enabled",
                    "replace",
                    "storage",
                    "towers",
                    "ramparts",
                    "late",
                    "max_sites",
                    "verify_ticks",
                    "hub_clearance",
                    "exposed_penalty",
                ],
            ),
            (
                "remote",
                &[
                    "enabled",
                    "max_rooms",
                    "min_level",
                    "spawn_permille",
                    "max_sites",
                    "container_repair_pct",
                    "road_read_ticks",
                    "rescout_ticks",
                    "evacuate_ticks",
                    "abandon_ticks",
                    "contest_threats",
                    "audit_ticks",
                    "creep_ucpu",
                    "horizon_ticks",
                    "slack_pct",
                ],
            ),
            (
                "sign",
                &[
                    "enabled",
                    "min_level",
                    "max_distance",
                    "recheck_ticks",
                    "retry_ticks",
                    "max_deaths",
                    "max_overwrites",
                    "damage_margin_pct",
                    "life_margin_ticks",
                    "route_max_ops",
                    "route_max_rooms",
                    "save_ticks",
                ],
            ),
            (
                "defense",
                &[
                    "threat_end_ticks",
                    "threat_log_ticks",
                    "max_kill_ticks",
                    "heal_reserve",
                    "miss_ticks",
                ],
            ),
            (
                "upkeep",
                &[
                    "enabled",
                    "read_ticks",
                    "road_start_pct",
                    "road_urgent_pct",
                    "rampart_permille",
                    "rampart_start_pct",
                    "rampart_floor",
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
        // A room can hold no more sites than the player's whole limit (V200).
        assert_eq!(range("planner.max_sites"), (1, MAX_CONSTRUCTION_SITES));
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
            "persist.planner_segment",
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
