//! The CPU profiler (ROADMAP P1 module 8): sparse `Game.cpu.getUsed()` spans per subsystem, exact
//! statistics per window of loops, and the stats segment that `npm run stats` reads. All pure Rust:
//! rustedscreep-wasm takes the readings, only in a build with its `profiler` feature, and writes the
//! segment. Without the feature none of this is linked into the wasm.
//!
//! One loop, as rustedscreep-wasm drives it (six readings, each one JS call):
//! 1. `start`, then `clock`, back to back as the loop's first statements. Every reading is the tick's
//!    CPU so far (VERIFICATION_LOG V106), so `start` is what the tick spent before the loop (the
//!    engine's setup and the loader), and `clock − start` is one `getUsed` call.
//! 2. `snapshot` after the snapshot, `decide` after the scheduler's tick, `act` after the act step,
//!    and `end` after the fault log, as the loop's last reading.
//! 3. [`Profiler::record`] turns them into one value per [`Series`] and adds it to the open window.
//!
//! A span runs from one reading to the next, so `snapshot`, `decide` and `act` each also contain one
//! whole `getUsed` call (the end of the first call and the start of the second), just as `clock`
//! does with no work in between. A step's own cost is its series minus `clock`; on an empty bot the
//! reading can be most of `decide`. No series covers the fault log alone (`end` minus the other
//! spans' sum).
//!
//! A value needs good readings: finite, at least 0, and, for a span, not lower than the reading
//! before it. A bad value is counted (`bad`) and left out of the statistics.
//!
//! Statistics: the first loop of a global is kept apart as the cold loop (it pays for the first full
//! snapshot, and its `start` includes the instantiation when the loader runs both on one tick). The
//! loops after it fill windows of `ProfilerConfig::window_loops` loops. A closed window keeps, per
//! series, the exact count, sum, mean (rounded half up), nearest-rank p95 and maximum
//! ([`SeriesStats::of`]); the last `ProfilerConfig::history_windows` windows are kept and written to the
//! stats segment. A window counts loops, not game ticks: ticks without a loop between its first and
//! last loop (skipped ticks, V107) are its `gaps`; those before its first loop follow from the
//! previous window's `last_time` (or the cold loop's time).
//!
//! Act-step cost per call ([`ActCalls`]): a reading includes 0.2 CPU per billed intent (V106), so the
//! act span minus that and minus the loop's `clock` (the reading inside the span), over the calls the
//! step made, is what one call costs after the decide step, where only the tick reserve covers it. It
//! still includes the act step's fixed work per tick (planning and closing the counters), which the
//! reserve pays for too, so it reads high when a tick has few calls. [`act_call_bound`] is the most a
//! call may cost under that bound's assumptions (TUNING.md `tick_reserve_pct`).
//!
//! Units: CPU values are integers in µCPU (1e-6 CPU; CPU is ms of isolate time, V106, so 1 ns). The
//! profiler never formats a float, so it does not link core's float formatting into the wasm.
//!
//! Cost per loop (P1 build): the six readings plus eight `Vec` pushes into capacity reserved once per
//! global; a closed window adds eight O(n) selections, the segment text (a few KB) and its write.
//! The heap holds eight `u32` per loop of the open window (32 KB at 1000 loops) plus the history.

mod text;

use std::collections::VecDeque;

use crate::config::{ProfilerConfig, SchedulerConfig};
use crate::snapshot::TickContext;
use rustedscreep_consts::INTENT_CPU_COST;

pub use text::{FORMAT, FORMAT_VERSION, STATS_FIELDS, cold_line, write_cpu};

/// µCPU per CPU.
pub const MICRO_PER_CPU: f64 = 1_000_000.0;

/// Number of [`Series`].
pub const SERIES_COUNT: usize = 8;

/// What the profiler records per loop, in segment order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Series {
    /// CPU the tick used before the loop's first reading (absolute). Unit: µCPU.
    Start,
    /// One `Game.cpu.getUsed()` call: the second of two back-to-back readings minus the first.
    /// Unit: µCPU.
    Clock,
    /// The snapshot, plus one `getUsed` call (see the module docs). Unit: µCPU.
    Snapshot,
    /// The decide step: opening the intent buffer and the scheduler's tick, every tier; plus one
    /// `getUsed` call. Unit: µCPU.
    Decide,
    /// The act step: its JS calls plus 0.2 CPU per billed intent (V106); plus one `getUsed` call.
    /// Unit: µCPU.
    Act,
    /// The tick's CPU at the loop's last reading (absolute). Unit: µCPU.
    End,
    /// JS calls the act step made. Unit: calls.
    Calls,
    /// Intents the engine bills: OK calls of methods that are not free (V34). Unit: intents.
    Billed,
}

impl Series {
    /// Every series, in segment order.
    pub const ALL: [Series; SERIES_COUNT] = [
        Self::Start,
        Self::Clock,
        Self::Snapshot,
        Self::Decide,
        Self::Act,
        Self::End,
        Self::Calls,
        Self::Billed,
    ];

    /// The name the segment and the console use.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Clock => "clock",
            Self::Snapshot => "snapshot",
            Self::Decide => "decide",
            Self::Act => "act",
            Self::End => "end",
            Self::Calls => "calls",
            Self::Billed => "billed",
        }
    }

    /// Whether the series is CPU (µCPU) rather than a count.
    pub const fn is_cpu(self) -> bool {
        !matches!(self, Self::Calls | Self::Billed)
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// One value per [`Series`], `None` where a reading it needs was bad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Values(pub [Option<u32>; SERIES_COUNT]);

impl Values {
    pub fn get(&self, series: Series) -> Option<u32> {
        self.0.get(series.index()).copied().flatten()
    }
}

/// One loop's `Game.cpu.getUsed()` readings, in the order the loop takes them. Unit: CPU. A reading
/// that was not taken is NaN (the default), which makes it bad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marks {
    pub start: f64,
    pub clock: f64,
    pub snapshot: f64,
    pub decide: f64,
    pub act: f64,
    pub end: f64,
}

impl Default for Marks {
    fn default() -> Self {
        Self {
            start: f64::NAN,
            clock: f64::NAN,
            snapshot: f64::NAN,
            decide: f64::NAN,
            act: f64::NAN,
            end: f64::NAN,
        }
    }
}

/// One profiled loop: its readings and the act step's counters (`IssueStats::issued` and `billed`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub marks: Marks,
    pub calls: u32,
    pub billed: u32,
}

impl Sample {
    /// The loop's value of each series.
    pub fn values(&self) -> Values {
        let m = &self.marks;
        Values([
            micro(m.start),
            span(m.start, m.clock),
            span(m.clock, m.snapshot),
            span(m.snapshot, m.decide),
            span(m.decide, m.act),
            micro(m.end),
            Some(self.calls),
            Some(self.billed),
        ])
    }
}

/// `cpu` in µCPU, rounded to the nearest and saturating at `u32::MAX` (about 4295 CPU, far above
/// `CPU_TICK_LIMIT_MAX`), or `None` unless it is finite and at least 0. Unit: µCPU.
pub fn micro(cpu: f64) -> Option<u32> {
    if !cpu.is_finite() || cpu < 0.0 {
        return None;
    }
    let v = (cpu * MICRO_PER_CPU).round();
    Some(if v >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        // In 0..u32::MAX here, and already rounded.
        v as u32
    })
}

/// The CPU between two good readings of one tick, or `None` if either is bad or `to` is below
/// `from`. A reading falls only after a `cancelOrder` (V106), which the bot does not call, so a fall
/// is counted as bad rather than as 0. Unit: µCPU.
pub fn span(from: f64, to: f64) -> Option<u32> {
    micro(from)?;
    micro(to)?;
    if to < from {
        return None;
    }
    micro(to - from)
}

/// The most one act-step call may cost if the tick reserve is to cover a tick whose committed CPU
/// is all billed intents, one call each. A necessary condition, not a sufficient one: it gives the
/// whole reserve to those calls and counts no free calls (`say`, `pull`, which commit nothing but
/// are calls too), while the reserve also covers the slicer's overshoot and the work after the loop
/// (TUNING.md `tick_reserve_pct`), so the safe cost per call is lower. Unit: µCPU, rounded down.
///
/// Uses: `rustedscreep_consts::INTENT_CPU_COST`, `SchedulerConfig::tick_reserve_pct`. With the
/// reserve share p = pct / 100, no tier spends past the ceiling tickLimit × (1 − p), so a tick
/// commits at most tickLimit × (1 − p) / INTENT_CPU_COST billed intents. Each one is an act-step
/// call after the decide step, which only the reserve tickLimit × p covers:
/// c × tickLimit × (1 − p) / INTENT_CPU_COST ≤ tickLimit × p, so
/// c ≤ INTENT_CPU_COST × pct / (100 − pct): 22222 µCPU (about 0.022 CPU) at 10 (TUNING.md). A
/// `tick_reserve_pct` of 100 or more, outside its safe range, is read as 99.
pub fn act_call_bound(sched: &SchedulerConfig) -> u32 {
    let pct = u64::from(sched.tick_reserve_pct.min(99));
    let intent = u64::from(micro(INTENT_CPU_COST).unwrap_or(0));
    u32::try_from(intent * pct / (100 - pct)).unwrap_or(u32::MAX)
}

/// Exact statistics of one series over a window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeriesStats {
    /// Good values.
    pub n: u32,
    pub sum: u64,
    /// `sum / n`, rounded half up (0 when `n` is 0).
    pub mean: u32,
    /// Nearest-rank 95th percentile (0 when `n` is 0).
    pub p95: u32,
    pub max: u32,
    /// Loops whose value was bad.
    pub bad: u32,
}

impl SeriesStats {
    /// The statistics of `values` (reordered in place). The p95 is the value at 1-based rank
    /// ⌈0.95 × n⌉ in ascending order, so at least 95% of the values are at or below it and it is
    /// always one of them.
    pub fn of(values: &mut [u32], bad: u32) -> Self {
        let Some(max) = values.iter().copied().max() else {
            return Self {
                bad,
                ..Self::default()
            };
        };
        let len = values.len();
        let n = len as u64;
        let sum: u64 = values.iter().map(|&v| u64::from(v)).sum();
        let mean = u32::try_from((sum + n / 2) / n).unwrap_or(max);
        // 1 ≤ rank ≤ len, so the index is in bounds.
        let rank = (len * 95).div_ceil(100).clamp(1, len);
        let (_, &mut p95, _) = values.select_nth_unstable(rank - 1);
        Self {
            n: u32::try_from(len).unwrap_or(u32::MAX),
            sum,
            mean,
            p95,
            max,
            bad,
        }
    }
}

/// The act step's calls over a window, for the cost per call (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActCalls {
    /// Loops in which the act step made at least one call and both its span and `clock` were good.
    pub ticks: u32,
    /// Calls in those loops: every JS call the act step made (`IssueStats::issued`), refused and
    /// free ones included.
    pub calls: u64,
    /// The act span minus the loop's `clock` minus `INTENT_CPU_COST` per billed intent, summed over
    /// those loops. Noise in `clock` can make it slightly negative. Unit: µCPU.
    pub net: i64,
}

impl ActCalls {
    /// Mean cost per call net of intent CPU and of the profiler's reading, rounded toward zero, or
    /// `None` with no calls. Unit: µCPU.
    pub fn per_call(&self) -> Option<i64> {
        self.net.checked_div(i64::try_from(self.calls).ok()?)
    }
}

/// One window's statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowStats {
    /// `Game.time` of the window's first and last loop.
    pub first_time: u32,
    pub last_time: u32,
    pub loops: u32,
    /// Game ticks between the window's first and last loop that ran no loop (not those before its
    /// first loop). Unit: ticks.
    pub gaps: u32,
    /// `Game.cpu.limit` at the last loop. Unit: CPU.
    pub cpu_limit: u32,
    /// The bucket at the first and the last loop, and its lowest value in between. Unit: CPU.
    pub bucket_first: u32,
    pub bucket_last: u32,
    pub bucket_min: u32,
    pub series: [SeriesStats; SERIES_COUNT],
    pub act: ActCalls,
}

impl WindowStats {
    pub fn get(&self, series: Series) -> SeriesStats {
        self.series.get(series.index()).copied().unwrap_or_default()
    }
}

/// The first loop of a global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColdLoop {
    pub time: u32,
    pub cpu_limit: u32,
    pub bucket: u32,
    pub values: Values,
}

/// The latest loop, for the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastLoop {
    pub time: u32,
    pub values: Values,
}

/// What [`Profiler::record`] did with a loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// It was the global's first loop, kept as the cold loop.
    Cold,
    /// It went into the open window.
    Open,
    /// It closed the window: write the stats segment now.
    Closed,
}

/// The open window's accumulators.
#[derive(Default)]
struct Window {
    loops: u32,
    first_time: u32,
    last_time: u32,
    gaps: u32,
    cpu_limit: u32,
    bucket_first: u32,
    bucket_last: u32,
    bucket_min: u32,
    values: [Vec<u32>; SERIES_COUNT],
    bad: [u32; SERIES_COUNT],
    act: ActCalls,
}

impl Window {
    fn push(&mut self, ctx: &TickContext, values: &Values, sample: &Sample, capacity: usize) {
        if self.loops == 0 {
            self.first_time = ctx.time;
            self.bucket_first = ctx.bucket;
            self.bucket_min = ctx.bucket;
            self.gaps = 0;
            // Once per global: clear() keeps the capacity for the next windows.
            for v in &mut self.values {
                v.reserve_exact(capacity.saturating_sub(v.len()));
            }
        } else if ctx.time > self.last_time {
            self.gaps = self.gaps.saturating_add(ctx.time - self.last_time - 1);
        }
        self.loops = self.loops.saturating_add(1);
        self.last_time = ctx.time;
        self.cpu_limit = ctx.cpu_limit;
        self.bucket_last = ctx.bucket;
        self.bucket_min = self.bucket_min.min(ctx.bucket);
        for ((value, list), bad) in values
            .0
            .iter()
            .zip(self.values.iter_mut())
            .zip(self.bad.iter_mut())
        {
            match value {
                Some(v) => list.push(*v),
                None => *bad = bad.saturating_add(1),
            }
        }
        if sample.calls > 0
            && let (Some(act), Some(clock)) = (values.get(Series::Act), values.get(Series::Clock))
        {
            let intent = i64::from(micro(INTENT_CPU_COST).unwrap_or(0));
            let a = &mut self.act;
            a.ticks = a.ticks.saturating_add(1);
            a.calls = a.calls.saturating_add(u64::from(sample.calls));
            a.net = a.net.saturating_add(
                i64::from(act) - i64::from(clock) - i64::from(sample.billed) * intent,
            );
        }
    }

    /// Everything but the per-series statistics.
    fn header(&self) -> WindowStats {
        WindowStats {
            first_time: self.first_time,
            last_time: self.last_time,
            loops: self.loops,
            gaps: self.gaps,
            cpu_limit: self.cpu_limit,
            bucket_first: self.bucket_first,
            bucket_last: self.bucket_last,
            bucket_min: self.bucket_min,
            series: [SeriesStats::default(); SERIES_COUNT],
            act: self.act,
        }
    }

    /// Closes the window: its statistics, computed in place, and an empty window with the same
    /// capacity.
    fn close(&mut self) -> WindowStats {
        let mut out = self.header();
        for ((stats, list), &bad) in out
            .series
            .iter_mut()
            .zip(self.values.iter_mut())
            .zip(&self.bad)
        {
            *stats = SeriesStats::of(list, bad);
            list.clear();
        }
        self.loops = 0;
        self.bad = [0; SERIES_COUNT];
        self.act = ActCalls::default();
        out
    }

    /// The open window's statistics so far, from a copy (allocates; for the console).
    fn peek(&self) -> WindowStats {
        let mut out = self.header();
        let mut scratch = Vec::new();
        for ((stats, list), &bad) in out.series.iter_mut().zip(&self.values).zip(&self.bad) {
            scratch.clear();
            scratch.extend_from_slice(list);
            *stats = SeriesStats::of(&mut scratch, bad);
        }
        out
    }
}

/// The profiler's state for one global, in the heap only: a global reset loses the open window
/// and the history (the stats segment keeps what was last written).
#[derive(Default)]
pub struct Profiler {
    loops: u32,
    cold: Option<ColdLoop>,
    last: Option<LastLoop>,
    window: Window,
    history: VecDeque<WindowStats>,
    closed: u32,
    flushes: u32,
    flush_errors: u32,
    last_flush: Option<u32>,
}

impl Profiler {
    /// Records one loop. Call it once per loop, after the loop's last reading.
    pub fn record(&mut self, ctx: &TickContext, sample: &Sample, cfg: &ProfilerConfig) -> Recorded {
        let values = sample.values();
        let first = self.loops == 0;
        self.loops = self.loops.saturating_add(1);
        self.last = Some(LastLoop {
            time: ctx.time,
            values,
        });
        if first {
            self.cold = Some(ColdLoop {
                time: ctx.time,
                cpu_limit: ctx.cpu_limit,
                bucket: ctx.bucket,
                values,
            });
            return Recorded::Cold;
        }
        let window_loops = cfg.window_loops.max(1);
        let capacity = usize::try_from(window_loops).unwrap_or(0);
        self.window.push(ctx, &values, sample, capacity);
        if self.window.loops < window_loops {
            return Recorded::Open;
        }
        self.close_window(cfg);
        Recorded::Closed
    }

    /// Closes the open window into the history. Once per window, so kept out of line: inlined, it
    /// would grow the wasm's per-tick `loop` function by its statistics code.
    #[cold]
    #[inline(never)]
    fn close_window(&mut self, cfg: &ProfilerConfig) {
        let stats = self.window.close();
        let keep = usize::try_from(cfg.history_windows.max(1)).unwrap_or(1);
        while self.history.len() >= keep {
            self.history.pop_front();
        }
        self.history.push_back(stats);
        self.closed = self.closed.saturating_add(1);
    }

    /// Records a stats-segment write: whether it succeeded and what it cost from the loop's last
    /// reading (the closing statistics, the text and the write). Unit of `cost`: µCPU.
    pub fn note_flush(&mut self, ok: bool, cost: Option<u32>) {
        if ok {
            self.flushes = self.flushes.saturating_add(1);
        } else {
            self.flush_errors = self.flush_errors.saturating_add(1);
        }
        self.last_flush = cost;
    }

    /// Loops recorded in this global, the cold loop included.
    pub fn loops(&self) -> u32 {
        self.loops
    }

    pub fn cold(&self) -> Option<&ColdLoop> {
        self.cold.as_ref()
    }

    pub fn last(&self) -> Option<&LastLoop> {
        self.last.as_ref()
    }

    /// Closed windows kept, oldest first.
    pub fn history(&self) -> &VecDeque<WindowStats> {
        &self.history
    }

    /// Windows closed in this global, including those no longer kept.
    pub fn closed(&self) -> u32 {
        self.closed
    }

    /// Loops in the open window.
    pub fn open_loops(&self) -> u32 {
        self.window.loops
    }

    /// The open window's statistics so far, or `None` while it is empty. Allocates a scratch copy,
    /// so it is for the console, not the loop.
    pub fn open_window(&self) -> Option<WindowStats> {
        (self.window.loops > 0).then(|| self.window.peek())
    }

    /// Successful and failed stats-segment writes in this global.
    pub fn flushes(&self) -> (u32, u32) {
        (self.flushes, self.flush_errors)
    }

    /// The cost of the last write, if it could be measured. Unit: µCPU.
    pub fn last_flush(&self) -> Option<u32> {
        self.last_flush
    }
}

#[cfg(test)]
mod tests;
