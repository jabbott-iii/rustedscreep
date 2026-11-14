//! The profiler's text: the stats segment for `npm run stats` and the console lines. Integers only,
//! so no float formatting is linked into the wasm.
//!
//! The stats segment is one JSON object in ASCII (so its length in UTF-16 units, which the driver
//! limits, equals its byte length; V41), versioned by `v`:
//!
//! ```text
//! {"format":"rustedscreep-profiler","v":1,"unit":"1e-6 CPU",
//!  "series":["start","clock","snapshot","decide","act","end","calls","billed"],
//!  "stats":["n","sum","mean","p95","max","bad"],
//!  "written":<Game.time of this write>,"loops":<loops in this global>,"windowLoops":<window length>,
//!  "callBound":<act_call_bound, µCPU>,
//!  "flushes":<writes before this one>,"flushErrors":<failed writes before this one>,
//!  "lastFlush":<µCPU of the previous write, or null>,
//!  "cold":{"time":..,"limit":..,"bucket":..,"values":[<one per series, null when bad>]} or null,
//!  "windows":[<oldest first>{"first":<Game.time>,"last":<Game.time>,"loops":..,"gaps":..,"limit":..,
//!    "bucket":[<first>,<last>,<min>],"stats":[<one [n,sum,mean,p95,max,bad] per series>],
//!    "act":{"ticks":..,"calls":..,"net":<µCPU>}}]}
//! ```
//!
//! CPU series are in µCPU (`unit`); `calls` and `billed` are counts. `snapshot`, `decide` and `act`
//! each include one `getUsed` call (about the `clock` mean). The act cost per call is
//! `act.net / act.calls` (the act span minus `clock` and 0.2 CPU per billed intent; `ActCalls`), to
//! compare with `callBound`, a necessary bound (`act_call_bound`). A window's `gaps` are the ticks
//! without a loop between its `first` and `last`. Segment 99 outlives a deploy: match `cold.time`
//! with the `Game.time` of the boot log line, which carries the build ID, before using a segment as
//! evidence for a build.

use std::fmt::Write as _;

use super::{
    ActCalls, ColdLoop, Profiler, Series, SeriesStats, Values, WindowStats, act_call_bound,
};
use crate::config::{ProfilerConfig, SchedulerConfig};

/// The segment's `format` field.
pub const FORMAT: &str = "rustedscreep-profiler";
/// The segment's `v` field; a change to the layout raises it.
pub const FORMAT_VERSION: u32 = 1;
/// The order of the numbers in each `stats` entry of the segment.
pub const STATS_FIELDS: [&str; 6] = ["n", "sum", "mean", "p95", "max", "bad"];

/// Writes `micro` µCPU as CPU with four decimals (rounded half up), for example 1234 as `0.0012`.
pub fn write_cpu(out: &mut String, micro: u64) {
    let tenths_of_ms = micro / 100 + u64::from(micro % 100 >= 50);
    let _ = write!(
        out,
        "{}.{:04}",
        tenths_of_ms / 10_000,
        tenths_of_ms % 10_000
    );
}

/// [`write_cpu`] for a value that can be negative.
fn write_signed_cpu(out: &mut String, micro: i64) {
    if micro < 0 {
        out.push('-');
    }
    write_cpu(out, micro.unsigned_abs());
}

fn json_value(out: &mut String, value: Option<u32>) {
    match value {
        Some(v) => {
            let _ = write!(out, "{v}");
        }
        None => out.push_str("null"),
    }
}

fn json_values(out: &mut String, values: &Values) {
    out.push('[');
    for (i, v) in values.0.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_value(out, *v);
    }
    out.push(']');
}

fn json_stats(out: &mut String, s: &SeriesStats) {
    let _ = write!(
        out,
        "[{},{},{},{},{},{}]",
        s.n, s.sum, s.mean, s.p95, s.max, s.bad
    );
}

fn json_window(out: &mut String, w: &WindowStats) {
    let _ = write!(
        out,
        "{{\"first\":{},\"last\":{},\"loops\":{},\"gaps\":{},\"limit\":{},\"bucket\":[{},{},{}],\"stats\":[",
        w.first_time,
        w.last_time,
        w.loops,
        w.gaps,
        w.cpu_limit,
        w.bucket_first,
        w.bucket_last,
        w.bucket_min
    );
    for (i, s) in w.series.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_stats(out, s);
    }
    let ActCalls { ticks, calls, net } = w.act;
    let _ = write!(
        out,
        "],\"act\":{{\"ticks\":{ticks},\"calls\":{calls},\"net\":{net}}}}}"
    );
}

/// A value for the console: CPU with four decimals, a count as is, `?` when bad.
fn console_value(out: &mut String, series: Series, value: Option<u32>) {
    match value {
        None => out.push('?'),
        Some(v) if series.is_cpu() => write_cpu(out, u64::from(v)),
        Some(v) => {
            let _ = write!(out, "{v}");
        }
    }
}

/// `start 0.0123, clock …` for every series.
fn console_values(out: &mut String, values: &Values) {
    for (i, series) in Series::ALL.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(series.name());
        out.push(' ');
        console_value(out, *series, values.get(*series));
    }
}

/// `start a/b/c, …` (mean/p95/max) for the CPU series, then totals for the counts, then the
/// series with bad values.
fn console_window(out: &mut String, w: &WindowStats) {
    out.push_str("CPU mean/p95/max: ");
    let mut first = true;
    for series in Series::ALL.iter().filter(|s| s.is_cpu()) {
        if !first {
            out.push_str(", ");
        }
        first = false;
        let s = w.get(*series);
        out.push_str(series.name());
        out.push(' ');
        write_cpu(out, u64::from(s.mean));
        out.push('/');
        write_cpu(out, u64::from(s.p95));
        out.push('/');
        write_cpu(out, u64::from(s.max));
    }
    for series in Series::ALL.iter().filter(|s| !s.is_cpu()) {
        let s = w.get(*series);
        let _ = write!(out, "; {} total {} (max {})", series.name(), s.sum, s.max);
    }
    let mut bad = Series::ALL.iter().filter(|s| w.get(**s).bad > 0).peekable();
    if bad.peek().is_some() {
        out.push_str("; bad readings: ");
        for (i, series) in bad.enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{} {}", series.name(), w.get(*series).bad);
        }
    }
}

/// One line for the global's first loop, which the wasm logs once.
pub fn cold_line(cold: &ColdLoop) -> String {
    let mut out = format!(
        "rustedscreep profiler: first loop of this global at tick {} (cpu limit {}, bucket {}): ",
        cold.time, cold.cpu_limit, cold.bucket
    );
    console_values(&mut out, &cold.values);
    out.push_str(" (CPU values in CPU)");
    out
}

impl Profiler {
    /// Writes the stats segment (format in this module's docs) into `out`, which it appends to.
    ///
    /// Uses: [`act_call_bound`] for `callBound`.
    pub fn write_segment(
        &self,
        now: u32,
        cfg: &ProfilerConfig,
        sched: &SchedulerConfig,
        out: &mut String,
    ) {
        let _ = write!(
            out,
            "{{\"format\":\"{FORMAT}\",\"v\":{FORMAT_VERSION},\"unit\":\"1e-6 CPU\",\"series\":["
        );
        for (i, series) in Series::ALL.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "\"{}\"", series.name());
        }
        out.push_str("],\"stats\":[");
        for (i, field) in STATS_FIELDS.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "\"{field}\"");
        }
        let (flushes, errors) = self.flushes();
        let _ = write!(
            out,
            "],\"written\":{now},\"loops\":{},\"windowLoops\":{},\"callBound\":{},\"flushes\":{flushes},\"flushErrors\":{errors},\"lastFlush\":",
            self.loops(),
            cfg.window_loops,
            act_call_bound(sched),
        );
        json_value(out, self.last_flush());
        out.push_str(",\"cold\":");
        match self.cold() {
            Some(c) => {
                let _ = write!(
                    out,
                    "{{\"time\":{},\"limit\":{},\"bucket\":{},\"values\":",
                    c.time, c.cpu_limit, c.bucket
                );
                json_values(out, &c.values);
                out.push('}');
            }
            None => out.push_str("null"),
        }
        out.push_str(",\"windows\":[");
        for (i, w) in self.history().iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            json_window(out, w);
        }
        out.push_str("]}");
    }

    /// One line for `rustedscreep.profiler()`: the open window so far, the act cost per call, the
    /// last loop, the last closed window, the segment writes and the cold loop.
    pub fn summary(&self, now: u32, cfg: &ProfilerConfig, sched: &SchedulerConfig) -> String {
        if self.loops() == 0 {
            return String::from("rustedscreep profiler: no loop recorded yet in this global");
        }
        let mut out = format!(
            "rustedscreep profiler at tick {now}: loops {} in this global | open window {}/{} loops",
            self.loops(),
            self.open_loops(),
            cfg.window_loops
        );
        let bound = u64::from(act_call_bound(sched));
        match self.open_window() {
            Some(w) => {
                let _ = write!(out, " from tick {}: ", w.first_time);
                console_window(&mut out, &w);
                out.push_str(" | act per call ");
                match w.act.per_call() {
                    Some(c) => {
                        write_signed_cpu(&mut out, c);
                        let _ = write!(
                            out,
                            " CPU over {} calls in {} ticks",
                            w.act.calls, w.act.ticks
                        );
                    }
                    None => out.push_str("n/a (no calls yet)"),
                }
                out.push_str(" (bound ");
                write_cpu(&mut out, bound);
                out.push(')');
            }
            None => out.push_str(" (empty)"),
        }
        if let Some(last) = self.last() {
            let _ = write!(out, " | last loop at tick {}: ", last.time);
            console_values(&mut out, &last.values);
        }
        let _ = write!(
            out,
            " | windows closed {} (kept {})",
            self.closed(),
            self.history().len()
        );
        if let Some(w) = self.history().back() {
            let end = w.get(Series::End);
            let _ = write!(
                out,
                ", last from tick {} to {}: end mean/p95 ",
                w.first_time, w.last_time
            );
            write_cpu(&mut out, u64::from(end.mean));
            out.push('/');
            write_cpu(&mut out, u64::from(end.p95));
        }
        let (flushes, errors) = self.flushes();
        let _ = write!(
            out,
            " | segment {}: writes {flushes}, failed {errors}",
            cfg.stats_segment
        );
        if let Some(cost) = self.last_flush() {
            out.push_str(", last write ");
            write_cpu(&mut out, u64::from(cost));
        }
        if let Some(cold) = self.cold() {
            let _ = write!(out, " | cold loop at tick {}: ", cold.time);
            console_values(&mut out, &cold.values);
        }
        out
    }
}
