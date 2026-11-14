//! Scheduler glue (ROADMAP P1 module 6): the game clock for `rustedscreep_core::scheduler`, the
//! rate-limited fault log, and the `rustedscreep.scheduler()` console summary.
//!
//! JS calls per tick: none for the budget (it reads the snapshot's tick context), one
//! `Game.cpu.getUsed()` per clock sample (only while a time-sliced job runs; no job exists before
//! P2), and one `console.log` per fault report the rate limit lets through.

use std::fmt::{self, Write as _};

use rustedscreep_core::scheduler::{Cap, Clock, Scheduler, Suppressed};
use rustedscreep_core::snapshot::TickContext;
use screeps::game;
use wasm_bindgen::prelude::wasm_bindgen;

#[wasm_bindgen]
extern "C" {
    /// `console.log`, looked up on every call: Screeps builds a new console before every tick (V96).
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn console_log(message: &str);
}

/// `Game.cpu.getUsed()`: isolate CPU time since the tick started plus 0.2 per intent made so far
/// (V106). The core sanitizes what it returns.
pub struct GameClock;

impl Clock for GameClock {
    fn used(&mut self) -> f64 {
        game::cpu::get_used()
    }
}

/// Logs the fault reports the rate limit let through since the last call, and the count of those
/// it held back. Formats only what it prints, which the limit bounds per window.
pub fn log_faults(sched: &mut Scheduler) {
    let (reports, suppressed) = sched.faults_mut().drain();
    for r in reports {
        console_log(&format!(
            "rustedscreep fault at tick {}: job {} unit {}: {}",
            r.time, r.job, r.fault.unit, r.fault.what
        ));
    }
    if let Some(Suppressed {
        count,
        window_start,
        jobs,
        more_jobs,
    }) = suppressed
    {
        let mut text = format!(
            "rustedscreep: {count} more fault(s) in the window from tick {window_start} were counted, not logged; jobs:"
        );
        for job in jobs.iter().flatten() {
            let _ = write!(text, " {job}");
        }
        if more_jobs {
            text.push_str(" and more");
        }
        console_log(&text);
    }
}

/// A CPU value printed with one or two decimals from an integer, so the wasm does not link core's
/// float formatting (20409 bytes of release wasm when this module used `{:.1}`, 2026-11-14).
struct Cpu(f64, Decimals);

#[derive(Clone, Copy)]
enum Decimals {
    One,
    Two,
}

impl fmt::Display for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(v, decimals) = *self;
        if v.is_nan() {
            return f.write_str("NaN");
        }
        if v.is_infinite() {
            return f.write_str(if v > 0.0 { "inf" } else { "-inf" });
        }
        let (scale, width) = match decimals {
            Decimals::One => (10u64, 1usize),
            Decimals::Two => (100u64, 2usize),
        };
        let scaled = (v.abs() * scale as f64 + 0.5).floor();
        // CPU values are at most a few hundred; anything this large is shown as a bound.
        if scaled >= 1e15 {
            return f.write_str(if v > 0.0 { ">1e13" } else { "<-1e13" });
        }
        let n = scaled as u64;
        let sign = if v < 0.0 && n > 0 { "-" } else { "" };
        write!(f, "{sign}{}.{:0width$}", n / scale, n % scale)
    }
}

fn cap_text(cap: Cap) -> String {
    match cap {
        Cap::On(v) => format!("on (cap {})", Cpu(v, Decimals::One)),
        Cap::Off(reason) => format!("off ({})", reason.as_str()),
    }
}

/// One line for `rustedscreep.scheduler()`: this tick's caps, what the scheduler did, faults, and one
/// clock reading taken now (so the console exercises the same `getUsed` binding the jobs use).
pub fn summary(sched: &Scheduler, ctx: &TickContext) -> String {
    let Some(rec) = sched.last_tick() else {
        return String::from("rustedscreep scheduler: no tick finished yet in this global");
    };
    let s = rec.stats;
    let mut text = format!(
        "rustedscreep scheduler at tick {} (loop {} of this global): cpu limit {}, tickLimit {}, bucket {} | \
         ceiling {}; critical on; standard {}; optional {} | samples {} (bad {}), units {}, \
         budget stops {}, tier-off skips {}, out of order {}, faults {}",
        rec.time,
        rec.loop_index,
        ctx.cpu_limit,
        ctx.tick_limit,
        ctx.bucket,
        Cpu(rec.budget.ceiling, Decimals::One),
        cap_text(rec.budget.standard),
        cap_text(rec.budget.optional),
        s.samples,
        s.bad_samples,
        s.units,
        s.budget_stops,
        s.tier_off,
        s.out_of_order,
        s.faults,
    );
    if s.samples > 0 {
        let _ = write!(
            text,
            ", spent {} (committed {})",
            Cpu(rec.spent, Decimals::Two),
            Cpu(rec.committed, Decimals::Two)
        );
    }
    if rec.clock_broken {
        text.push_str(", CLOCK BROKEN");
    }
    let _ = write!(
        text,
        " | clock now {}",
        Cpu(GameClock.used(), Decimals::Two)
    );
    let faults = sched.faults();
    let _ = write!(
        text,
        " | faults since boot {} (this window {})",
        faults.total(),
        faults.in_window()
    );
    if let Some(last) = faults.last() {
        let _ = write!(
            text,
            ", last at tick {}: job {} unit {}: {}",
            last.time, last.job, last.fault.unit, last.fault.what
        );
    }
    text
}
