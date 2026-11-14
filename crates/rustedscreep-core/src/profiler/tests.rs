use super::*;
use crate::config::{PROFILER_MMO, SCHEDULER_MMO};
use rustedscreep_consts::{CPU_TICK_LIMIT_MAX, MEMORY_SEGMENT_SIZE_LIMIT};

/// A seeded xorshift64, so every run sees the same values.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn ctx(time: u32, bucket: u32) -> TickContext {
    TickContext {
        time,
        cpu_limit: 20,
        tick_limit: CPU_TICK_LIMIT_MAX,
        bucket,
    }
}

/// Readings that give each span the CPU in `spans` (start, clock, snapshot, decide, act) and an end
/// `tail` CPU after the act reading. Unit: CPU.
fn marks(spans: [f64; 5], tail: f64) -> Marks {
    let [start, clock, snapshot, decide, act] = spans;
    let mut m = Marks {
        start,
        ..Marks::default()
    };
    m.clock = m.start + clock;
    m.snapshot = m.clock + snapshot;
    m.decide = m.snapshot + decide;
    m.act = m.decide + act;
    m.end = m.act + tail;
    m
}

fn micro_of(cpu: f64) -> u32 {
    micro(cpu).expect("a good reading")
}

/// Uses: MICRO_PER_CPU. Rounds to the nearest µCPU, saturates, and refuses what no clock reading
/// can be.
#[test]
fn micro_rounds_saturates_and_refuses_bad_readings() {
    let unit = 1.0 / MICRO_PER_CPU;
    assert_eq!(micro(0.0), Some(0));
    assert_eq!(micro(unit * 0.49), Some(0));
    assert_eq!(micro(unit * 0.51), Some(1));
    assert_eq!(micro(1.0), Some(1_000_000));
    assert_eq!(
        micro(f64::from(CPU_TICK_LIMIT_MAX)),
        Some(CPU_TICK_LIMIT_MAX * 1_000_000)
    );
    assert_eq!(micro(f64::from(u32::MAX)), Some(u32::MAX), "saturates");
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -unit, -1274.0] {
        assert_eq!(micro(bad), None, "{bad}");
    }
}

/// Each series reads the readings it needs: a bad reading spoils the spans next to it and nothing
/// else, and a falling reading is bad rather than 0.
#[test]
fn sample_values_follow_the_readings() {
    let spans = [1.5, 0.004, 0.25, 0.5, 0.125];
    let good = Sample {
        marks: marks(spans, 0.01),
        calls: 3,
        billed: 2,
    };
    let v = good.values();
    let [start, clock, snapshot, decide, act] = spans;
    assert_eq!(v.get(Series::Start), Some(micro_of(start)));
    assert_eq!(v.get(Series::Clock), Some(micro_of(clock)));
    assert_eq!(v.get(Series::Snapshot), Some(micro_of(snapshot)));
    assert_eq!(v.get(Series::Decide), Some(micro_of(decide)));
    assert_eq!(v.get(Series::Act), Some(micro_of(act)));
    assert_eq!(v.get(Series::End), Some(micro_of(good.marks.end)));
    assert_eq!(v.get(Series::Calls), Some(3));
    assert_eq!(v.get(Series::Billed), Some(2));

    // A NaN snapshot reading spoils snapshot and decide only.
    let mut nan = good;
    nan.marks.snapshot = f64::NAN;
    let v = nan.values();
    for s in Series::ALL {
        let spoiled = matches!(s, Series::Snapshot | Series::Decide);
        assert_eq!(v.get(s).is_none(), spoiled, "{}", s.name());
    }

    // A reading below the one before is bad for that span only.
    let mut fall = good;
    fall.marks.act = fall.marks.decide - 0.2;
    assert_eq!(fall.values().get(Series::Act), None);
    assert_eq!(fall.values().get(Series::Decide), Some(micro_of(decide)));
    assert_eq!(
        fall.values().get(Series::End),
        good.values().get(Series::End)
    );

    // A negative first reading (reported on the MMO, V106) spoils start and clock only.
    let mut negative = good;
    negative.marks.start = -1274.0;
    let v = negative.values();
    for s in Series::ALL {
        let spoiled = matches!(s, Series::Start | Series::Clock);
        assert_eq!(v.get(s).is_none(), spoiled, "{}", s.name());
    }
    assert_eq!(span(-1274.0, 0.5), None);
    assert_eq!(span(0.25, -1.0), None);
    assert_eq!(span(0.25, 0.25), Some(0));

    // The default marks are all bad; the counts are always good.
    let empty = Sample::default().values();
    for s in Series::ALL {
        assert_eq!(empty.get(s).is_some(), !s.is_cpu(), "{}", s.name());
    }
}

/// The statistics a naive sort gives: exact sum, mean rounded half up, the nearest-rank p95 (the
/// value at 1-based rank ⌈0.95 n⌉), and the maximum.
fn naive(values: &[u32], bad: u32) -> SeriesStats {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    if n == 0 {
        return SeriesStats {
            bad,
            ..SeriesStats::default()
        };
    }
    let sum: u128 = sorted.iter().map(|&v| u128::from(v)).sum();
    let n128 = n as u128;
    // Half up: floor(sum / n + 1/2) = floor((2 sum + n) / 2n).
    let mean = (2 * sum + n128) / (2 * n128);
    let rank = (95 * n).div_ceil(100);
    SeriesStats {
        n: n as u32,
        sum: sum as u64,
        mean: mean as u32,
        p95: sorted[rank - 1],
        max: sorted[n - 1],
        bad,
    }
}

/// `SeriesStats::of` equals the naive model on edge cases and on 2000 seeded lists (lengths up to
/// 1100, heavy duplicates in half of them), and its p95 has the nearest-rank property.
#[test]
fn series_stats_match_a_naive_sort() {
    let mut cases: Vec<Vec<u32>> = vec![
        vec![],
        vec![7],
        vec![u32::MAX; 3],
        vec![1, 2],
        vec![2, 1, 3],
        (1..=20).collect(),
        (1..=100).rev().collect(),
        (0..1000).collect(),
    ];
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for i in 0..2000 {
        let len = rng.below(1101) as usize;
        let range = if i % 2 == 0 {
            5
        } else {
            u64::from(u32::MAX) + 1
        };
        cases.push((0..len).map(|_| rng.below(range) as u32).collect());
    }
    for (i, case) in cases.iter().enumerate() {
        let bad = (i % 3) as u32;
        let mut work = case.clone();
        let got = SeriesStats::of(&mut work, bad);
        assert_eq!(got, naive(case, bad), "case {i}");
        if let Some(n) = (!case.is_empty()).then_some(case.len()) {
            let rank = (95 * n).div_ceil(100);
            let at_or_below = case.iter().filter(|&&v| v <= got.p95).count();
            let below = case.iter().filter(|&&v| v < got.p95).count();
            assert!(
                at_or_below >= rank && below < rank,
                "case {i}: nearest rank"
            );
            assert!(case.contains(&got.p95));
        }
    }
}

/// Uses: INTENT_CPU_COST, SCHEDULER_MMO.tick_reserve_pct, CPU_TICK_LIMIT_MAX. The bound is
/// INTENT_CPU_COST × pct / (100 − pct), about 0.022 CPU at the MMO's 10% (TUNING.md), and it is the
/// largest whole µCPU per call at which the reserve covers a ceiling's worth of intent calls.
#[test]
fn act_call_bound_is_what_the_reserve_covers() {
    let pct = f64::from(SCHEDULER_MMO.tick_reserve_pct);
    let want = (INTENT_CPU_COST * MICRO_PER_CPU * pct / (100.0 - pct)).floor();
    assert_eq!(f64::from(act_call_bound(&SCHEDULER_MMO)), want);
    assert_eq!((want / 1000.0).round(), 22.0, "TUNING.md: about 0.022 CPU");

    let tick_limit = u64::from(CPU_TICK_LIMIT_MAX) * 1_000_000;
    let intent = u64::from(micro_of(INTENT_CPU_COST));
    for pct in 5..=50u32 {
        let cfg = SchedulerConfig {
            tick_reserve_pct: pct,
            ..SCHEDULER_MMO
        };
        let bound = u64::from(act_call_bound(&cfg));
        let p = u64::from(pct);
        let ceiling = tick_limit * (100 - p) / 100;
        let reserve = tick_limit * p / 100;
        let calls = ceiling / intent;
        assert!(bound * calls <= reserve, "pct {pct}: the bound fits");
        assert!(
            (bound + 1) * calls > reserve,
            "pct {pct}: one µCPU more does not"
        );
    }
    // Outside the safe range, a reserve of 100% or more reads as 99% instead of dividing by 0.
    let at = |pct| {
        act_call_bound(&SchedulerConfig {
            tick_reserve_pct: pct,
            ..SCHEDULER_MMO
        })
    };
    for pct in [100, 101, u32::MAX] {
        assert_eq!(at(pct), at(99), "pct {pct}");
    }
}

/// `per_call` is the net over the calls, rounded toward zero, and absent without calls.
#[test]
fn act_per_call_rounds_toward_zero() {
    let a = |calls, net| ActCalls {
        ticks: 1,
        calls,
        net,
    };
    assert_eq!(a(0, 5).per_call(), None);
    assert_eq!(a(3, 10).per_call(), Some(3));
    assert_eq!(a(3, -10).per_call(), Some(-3));
    assert_eq!(a(4, 22_222 * 4).per_call(), Some(22_222));
}

/// Uses: INTENT_CPU_COST. A small window over scripted loops: the first loop is cold, the window
/// closes at exactly `window_loops` loops, gaps, the bucket range and the act net (the act span
/// minus the loop's clock and INTENT_CPU_COST per billed intent, over the loops with calls) are
/// recorded, and the history keeps the newest `history_windows` windows.
#[test]
fn record_keeps_the_cold_loop_apart_and_closes_windows() {
    let cfg = ProfilerConfig {
        window_loops: 4,
        history_windows: 2,
        stats_segment: 99,
    };
    let mut p = Profiler::default();
    let act_cpu = 0.5;
    let sample = |calls, billed| Sample {
        marks: marks([1.0, 0.002, 0.3, 0.1, act_cpu], 0.01),
        calls,
        billed,
    };
    assert_eq!(
        p.record(&ctx(100, 9000), &sample(0, 0), &cfg),
        Recorded::Cold
    );
    assert_eq!(p.cold().map(|c| (c.time, c.bucket)), Some((100, 9000)));
    assert_eq!(p.open_loops(), 0);
    assert_eq!(p.open_window(), None);

    // Ticks 101, 102, 105 (2 skipped), 106; calls on two of them.
    let steps = [
        (101, 8000, 0, 0),
        (102, 7000, 2, 1),
        (105, 7500, 0, 0),
        (106, 7600, 3, 0),
    ];
    let mut outcomes = Vec::new();
    for (t, bucket, calls, billed) in steps {
        outcomes.push(p.record(&ctx(t, bucket), &sample(calls, billed), &cfg));
    }
    assert_eq!(
        outcomes,
        [
            Recorded::Open,
            Recorded::Open,
            Recorded::Open,
            Recorded::Closed
        ]
    );
    assert_eq!(p.open_loops(), 0);
    assert_eq!(p.closed(), 1);
    let w = *p.history().back().expect("a closed window");
    assert_eq!(
        (w.first_time, w.last_time, w.loops, w.gaps),
        (101, 106, 4, 2)
    );
    assert_eq!(
        (w.bucket_first, w.bucket_last, w.bucket_min),
        (8000, 7600, 7000)
    );
    assert_eq!(w.cpu_limit, 20);
    let calls = w.get(Series::Calls);
    assert_eq!((calls.n, calls.sum, calls.max), (4, 5, 3));
    assert_eq!(w.get(Series::Billed).sum, 1);
    let v = sample(0, 0).values();
    let act = i64::from(v.get(Series::Act).expect("good"));
    let clock = i64::from(v.get(Series::Clock).expect("good"));
    assert_eq!(act, i64::from(micro_of(act_cpu)));
    let intent = i64::from(micro_of(INTENT_CPU_COST));
    assert_eq!(
        w.act,
        ActCalls {
            ticks: 2,
            calls: 5,
            net: 2 * (act - clock) - intent
        }
    );
    for s in Series::ALL {
        assert_eq!(w.get(s).n, 4, "{}", s.name());
    }

    // Two more windows: the history keeps the newest two.
    for t in 107..115 {
        p.record(&ctx(t, 9000), &sample(0, 0), &cfg);
    }
    assert_eq!(p.closed(), 3);
    let firsts: Vec<u32> = p.history().iter().map(|w| w.first_time).collect();
    assert_eq!(firsts, [107, 111]);
    assert_eq!(p.loops(), 13);
    assert_eq!(p.last().map(|l| l.time), Some(114));
}

/// A loop as the model sees it.
struct ModelLoop {
    time: u32,
    values: Values,
    bucket: u32,
    calls: u32,
    billed: u32,
}

/// Uses: INTENT_CPU_COST. The naive statistics of a window.
fn model_window(loops: &[ModelLoop]) -> WindowStats {
    let mut series = [SeriesStats::default(); SERIES_COUNT];
    for (slot, s) in series.iter_mut().zip(Series::ALL) {
        let good: Vec<u32> = loops.iter().filter_map(|l| l.values.get(s)).collect();
        *slot = naive(&good, (loops.len() - good.len()) as u32);
    }
    let first = loops.first().expect("a loop");
    let last = loops.last().expect("a loop");
    let intent = i64::from(micro_of(INTENT_CPU_COST));
    let with_calls: Vec<&ModelLoop> = loops
        .iter()
        .filter(|l| {
            l.calls > 0
                && l.values.get(Series::Act).is_some()
                && l.values.get(Series::Clock).is_some()
        })
        .collect();
    WindowStats {
        first_time: first.time,
        last_time: last.time,
        loops: loops.len() as u32,
        gaps: loops.windows(2).map(|w| w[1].time - w[0].time - 1).sum(),
        cpu_limit: 20,
        bucket_first: first.bucket,
        bucket_last: last.bucket,
        bucket_min: loops.iter().map(|l| l.bucket).min().expect("a loop"),
        series,
        act: ActCalls {
            ticks: with_calls.len() as u32,
            calls: with_calls.iter().map(|l| u64::from(l.calls)).sum(),
            net: with_calls
                .iter()
                .map(|l| {
                    i64::from(l.values.get(Series::Act).expect("good"))
                        - i64::from(l.values.get(Series::Clock).expect("good"))
                        - i64::from(l.billed) * intent
                })
                .sum(),
        },
    }
}

/// A long seeded run with PROFILER_MMO against a naive model of the windows: random spans, random
/// bad readings (NaN, a fall, a negative reading), skipped ticks, act calls and billed intents.
/// Every closed window, and the open one so far, equals the model's.
#[test]
fn windows_match_a_naive_model() {
    let cfg = PROFILER_MMO;
    let mut p = Profiler::default();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut time = 1_000_000u32;
    let mut open: Vec<ModelLoop> = Vec::new();
    let mut closed = Vec::new();
    let unit = 1.0 / MICRO_PER_CPU;
    let window = cfg.window_loops as usize;
    for i in 0..(window * 3 + 18) {
        time += 1 + u32::from(rng.below(20) == 0) * 3;
        let mut cpu = |max| rng.below(max) as f64 * unit;
        let spans = [
            cpu(3_000_000),
            cpu(10_000),
            cpu(2_000_000),
            cpu(500_000),
            cpu(1_000_000),
        ];
        let tail = cpu(100_000);
        let mut s = Sample {
            marks: marks(spans, tail),
            calls: if rng.below(4) == 0 {
                rng.below(30) as u32
            } else {
                0
            },
            billed: 0,
        };
        s.billed = s.calls.min(rng.below(30) as u32);
        match rng.below(50) {
            0 => s.marks.decide = f64::NAN,
            1 => s.marks.act = s.marks.decide - 0.2,
            2 => s.marks.start = -1274.0,
            _ => {}
        }
        let bucket = rng.below(10_001) as u32;
        let got = p.record(&ctx(time, bucket), &s, &cfg);
        if i == 0 {
            assert_eq!(got, Recorded::Cold);
            continue;
        }
        open.push(ModelLoop {
            time,
            values: s.values(),
            bucket,
            calls: s.calls,
            billed: s.billed,
        });
        let close = open.len() == window;
        assert_eq!(got == Recorded::Closed, close, "loop {i}");
        if close {
            closed.push(model_window(&open));
            open.clear();
        }
    }
    let kept: Vec<WindowStats> = p.history().iter().copied().collect();
    assert_eq!(closed.len(), 3);
    assert_eq!(kept, closed);
    assert!(
        closed
            .iter()
            .any(|w| w.act.calls > 0 && w.series.iter().any(|s| s.bad > 0))
    );
    assert_eq!(p.open_window(), Some(model_window(&open)));
    assert_eq!(p.open_loops(), 17);
}

/// Uses: INTENT_CPU_COST. The act net over a window is the act span minus the loop's clock minus
/// INTENT_CPU_COST per billed intent, summed over the loops with calls only; a loop whose act span
/// or clock is bad is left out.
#[test]
fn act_net_counts_only_good_loops_with_calls() {
    let cfg = ProfilerConfig {
        window_loops: 4,
        ..PROFILER_MMO
    };
    let mut p = Profiler::default();
    let act_cpu = 0.6;
    let make = |calls, billed, bad: Option<&str>| {
        let mut s = Sample {
            marks: marks([1.0, 0.001, 0.1, 0.1, act_cpu], 0.0),
            calls,
            billed,
        };
        match bad {
            Some("act") => s.marks.act = f64::NAN,
            Some("clock") => s.marks.clock = f64::NAN,
            _ => {}
        }
        s
    };
    p.record(&ctx(1, 5000), &make(9, 9, None), &cfg); // cold: never in a window
    p.record(&ctx(2, 5000), &make(2, 2, None), &cfg);
    p.record(&ctx(3, 5000), &make(4, 1, Some("act")), &cfg);
    p.record(&ctx(4, 5000), &make(3, 0, Some("clock")), &cfg);
    assert_eq!(
        p.record(&ctx(5, 5000), &make(0, 0, None), &cfg),
        Recorded::Closed
    );
    let w = p.history().back().copied().expect("closed");
    let v = make(2, 2, None).values();
    let act = i64::from(v.get(Series::Act).expect("good"));
    let clock = i64::from(v.get(Series::Clock).expect("good"));
    let intent = i64::from(micro_of(INTENT_CPU_COST));
    assert_eq!(
        w.act,
        ActCalls {
            ticks: 1,
            calls: 2,
            net: act - clock - 2 * intent
        }
    );
    assert_eq!(w.get(Series::Act).bad, 1);
    // A NaN clock reading spoils clock and snapshot; the end reading was always taken.
    assert_eq!(
        (
            w.get(Series::Clock).bad,
            w.get(Series::Snapshot).bad,
            w.get(Series::End).bad
        ),
        (1, 1, 0)
    );
    assert_eq!(w.get(Series::Calls).sum, 9, "calls count in every loop");
}

/// Uses: MICRO_PER_CPU. Four decimals, rounded half up, never a float.
#[test]
fn write_cpu_prints_four_decimals() {
    let text = |micro| {
        let mut s = String::new();
        write_cpu(&mut s, micro);
        s
    };
    assert_eq!(text(0), "0.0000");
    assert_eq!(text(49), "0.0000");
    assert_eq!(text(50), "0.0001");
    assert_eq!(text(1234), "0.0012");
    assert_eq!(text(u64::from(act_call_bound(&SCHEDULER_MMO))), "0.0222");
    assert_eq!(text(MICRO_PER_CPU as u64), "1.0000");
    assert_eq!(text(999_950), "1.0000", "carries into the whole part");
    assert!(!text(u64::MAX).is_empty());
}

/// The exact segment text for a small scripted profiler: the format, the series and stats names,
/// a cold loop with a bad value as null, one window, the flush counters and the call bound.
#[test]
fn segment_text_is_pinned() {
    let cfg = ProfilerConfig {
        window_loops: 2,
        history_windows: 1,
        stats_segment: 99,
    };
    let mut p = Profiler::default();
    let mut cold = Sample {
        marks: marks([2.0, 0.001, 1.0, 0.5, 0.0], 0.0),
        calls: 0,
        billed: 0,
    };
    cold.marks.decide = f64::NAN;
    p.record(&ctx(10, 10_000), &cold, &cfg);
    let s = |act, calls, billed| Sample {
        marks: marks([1.0, 0.003, 0.25, 0.125, act], 0.0),
        calls,
        billed,
    };
    p.record(&ctx(11, 10_000), &s(0.0, 0, 0), &cfg);
    assert_eq!(
        p.record(&ctx(13, 9990), &s(0.5, 2, 1), &cfg),
        Recorded::Closed
    );
    p.note_flush(true, Some(1500));
    let mut out = String::new();
    p.write_segment(13, &cfg, &SCHEDULER_MMO, &mut out);
    let want = concat!(
        r#"{"format":"rustedscreep-profiler","v":1,"unit":"1e-6 CPU","#,
        r#""series":["start","clock","snapshot","decide","act","end","calls","billed"],"#,
        r#""stats":["n","sum","mean","p95","max","bad"],"#,
        r#""written":13,"loops":3,"windowLoops":2,"callBound":22222,"flushes":1,"flushErrors":0,"lastFlush":1500,"#,
        r#""cold":{"time":10,"limit":20,"bucket":10000,"values":[2000000,1000,1000000,null,null,3501000,0,0]},"#,
        r#""windows":[{"first":11,"last":13,"loops":2,"gaps":1,"limit":20,"bucket":[10000,9990,9990],"stats":["#,
        r#"[2,2000000,1000000,1000000,1000000,0],[2,6000,3000,3000,3000,0],[2,500000,250000,250000,250000,0],"#,
        r#"[2,250000,125000,125000,125000,0],[2,500000,250000,500000,500000,0],"#,
        r#"[2,3256000,1628000,1878000,1878000,0],[2,2,1,2,2,0],[2,1,1,1,1,0]],"#,
        r#""act":{"ticks":1,"calls":2,"net":297000}}]}"#,
    );
    assert_eq!(out, want);
}

/// Uses: MEMORY_SEGMENT_SIZE_LIMIT. With every number at its longest and the history at the top
/// of its safe range (32, TUNING.md), the segment is ASCII and fits one segment (V41: the limit
/// counts UTF-16 units, which for ASCII equal bytes).
#[test]
fn worst_case_segment_fits() {
    let widest = SeriesStats {
        n: u32::MAX,
        sum: u64::MAX,
        mean: u32::MAX,
        p95: u32::MAX,
        max: u32::MAX,
        bad: u32::MAX,
    };
    let window = WindowStats {
        first_time: u32::MAX,
        last_time: u32::MAX,
        loops: u32::MAX,
        gaps: u32::MAX,
        cpu_limit: u32::MAX,
        bucket_first: u32::MAX,
        bucket_last: u32::MAX,
        bucket_min: u32::MAX,
        series: [widest; SERIES_COUNT],
        act: ActCalls {
            ticks: u32::MAX,
            calls: u64::MAX,
            net: i64::MIN,
        },
    };
    let mut p = Profiler {
        loops: u32::MAX,
        cold: Some(ColdLoop {
            time: u32::MAX,
            cpu_limit: u32::MAX,
            bucket: u32::MAX,
            values: Values([Some(u32::MAX); SERIES_COUNT]),
        }),
        flushes: u32::MAX,
        flush_errors: u32::MAX,
        last_flush: Some(u32::MAX),
        ..Profiler::default()
    };
    let history = 32;
    p.history.extend(std::iter::repeat_n(window, history));
    let cfg = ProfilerConfig {
        window_loops: u32::MAX,
        history_windows: history as u32,
        stats_segment: 99,
    };
    let sched = SchedulerConfig {
        tick_reserve_pct: 50,
        ..SCHEDULER_MMO
    };
    let mut out = String::new();
    p.write_segment(u32::MAX, &cfg, &sched, &mut out);
    assert!(out.is_ascii());
    assert!(
        out.len() <= MEMORY_SEGMENT_SIZE_LIMIT as usize,
        "{} > {MEMORY_SEGMENT_SIZE_LIMIT}",
        out.len()
    );
    assert!(PROFILER_MMO.history_windows as usize <= history);
}

/// The console line names the open window, the act cost per call against the bound, the last loop,
/// the last closed window, the segment writes and the cold loop; before any loop it says so.
#[test]
fn summary_reports_each_part() {
    let cfg = ProfilerConfig {
        window_loops: 2,
        history_windows: 2,
        stats_segment: 99,
    };
    let mut p = Profiler::default();
    assert_eq!(
        p.summary(5, &cfg, &SCHEDULER_MMO),
        "rustedscreep profiler: no loop recorded yet in this global"
    );
    let s = |act, calls| Sample {
        marks: marks([1.0, 0.003, 0.25, 0.125, act], 0.01),
        calls,
        billed: 0,
    };
    p.record(&ctx(5, 10_000), &s(0.0, 0), &cfg);
    let cold_only = p.summary(5, &cfg, &SCHEDULER_MMO);
    assert!(
        cold_only.contains("loops 1 in this global | open window 0/2 loops (empty)"),
        "{cold_only}"
    );
    assert!(
        cold_only.contains(" | cold loop at tick 5: start 1.0000, clock 0.0030, snapshot 0.2500"),
        "{cold_only}"
    );

    p.record(&ctx(6, 10_000), &s(0.05, 2), &cfg);
    let open = p.summary(6, &cfg, &SCHEDULER_MMO);
    for part in [
        "rustedscreep profiler at tick 6: loops 2 in this global | open window 1/2 loops from tick 6: ",
        "CPU mean/p95/max: start 1.0000/1.0000/1.0000, clock 0.0030/0.0030/0.0030,",
        "; calls total 2 (max 2); billed total 0 (max 0)",
        " | act per call 0.0235 CPU over 2 calls in 1 ticks (bound 0.0222)",
        " | last loop at tick 6: start 1.0000, clock 0.0030, snapshot 0.2500, decide 0.1250, act 0.0500, end 1.4380, calls 2, billed 0",
        " | windows closed 0 (kept 0) | segment 99: writes 0, failed 0 | cold loop at tick 5:",
    ] {
        assert!(open.contains(part), "missing {part:?} in {open}");
    }

    let mut bad = s(0.0, 0);
    bad.marks.clock = f64::NAN;
    assert_eq!(p.record(&ctx(7, 10_000), &bad, &cfg), Recorded::Closed);
    p.note_flush(false, Some(2500));
    let closed = p.summary(7, &cfg, &SCHEDULER_MMO);
    for part in [
        "open window 0/2 loops (empty)",
        "windows closed 1 (kept 1), last from tick 6 to 7: end mean/p95 ",
        " | segment 99: writes 0, failed 1, last write 0.0025",
        "last loop at tick 7: start 1.0000, clock ?, snapshot ?,",
    ] {
        assert!(closed.contains(part), "missing {part:?} in {closed}");
    }
    let w = p.history().back().copied().expect("closed");
    assert_eq!(
        (w.get(Series::Clock).bad, w.get(Series::Snapshot).bad),
        (1, 1)
    );
}

/// The cold line carries the tick, the CPU numbers and every series, with `?` for a bad value.
#[test]
fn cold_line_lists_every_series() {
    let mut p = Profiler::default();
    let mut s = Sample {
        marks: marks([12.5, 0.002, 4.0, 0.25, 0.0], 0.0),
        calls: 0,
        billed: 0,
    };
    s.marks.act = f64::NAN;
    p.record(&ctx(42, 777), &s, &PROFILER_MMO);
    let line = cold_line(p.cold().expect("cold"));
    assert_eq!(
        line,
        "rustedscreep profiler: first loop of this global at tick 42 (cpu limit 20, bucket 777): \
         start 12.5000, clock 0.0020, snapshot 4.0000, decide 0.2500, act ?, end 16.7520, calls 0, billed 0 \
         (CPU values in CPU)"
    );
}

/// The string the console prints for a window's CPU series and counts, built here from the
/// window's statistics independently of `console_window`.
fn expected_window_text(w: &WindowStats) -> String {
    let cpu = |v: u32| {
        let mut s = String::new();
        write_cpu(&mut s, u64::from(v));
        s
    };
    let parts: Vec<String> = Series::ALL
        .iter()
        .filter(|s| s.is_cpu())
        .map(|s| {
            let st = w.get(*s);
            format!(
                "{} {}/{}/{}",
                s.name(),
                cpu(st.mean),
                cpu(st.p95),
                cpu(st.max)
            )
        })
        .collect();
    let mut out = format!("CPU mean/p95/max: {}", parts.join(", "));
    for s in [Series::Calls, Series::Billed] {
        let st = w.get(s);
        out.push_str(&format!("; {} total {} (max {})", s.name(), st.sum, st.max));
    }
    let bad: Vec<String> = Series::ALL
        .iter()
        .filter(|s| w.get(**s).bad > 0)
        .map(|s| format!("{} {}", s.name(), w.get(*s).bad))
        .collect();
    if !bad.is_empty() {
        out.push_str(&format!("; bad readings: {}", bad.join(", ")));
    }
    out
}

/// A loop whose CPU values, calls and billed intents vary with `i`; `bad` spoils its clock reading.
fn varied(i: u32, bad: bool) -> Sample {
    let f = f64::from(i);
    let mut s = Sample {
        marks: marks(
            [
                1.0 + f * 0.01,
                0.001 + f * 0.0001,
                0.2 + f * 0.003,
                0.05,
                0.01 + f * 0.002,
            ],
            0.004 * f,
        ),
        calls: if i.is_multiple_of(3) { i % 7 } else { 0 },
        billed: u32::from(i.is_multiple_of(6)),
    };
    if bad {
        s.marks.clock = f64::NAN;
    }
    s
}

/// The console text is exact for varied windows: the open window's mean/p95/max per CPU series (a
/// window of 25, so p95 differs from max), the counts' totals and maxima, the bad readings, the
/// last closed window's end mean and p95, and closed against kept windows.
#[test]
fn summary_text_is_exact_for_varied_windows() {
    let cfg = ProfilerConfig {
        window_loops: 25,
        history_windows: 1,
        stats_segment: 99,
    };
    let mut p = Profiler::default();
    let mut t = 1000;
    p.record(&ctx(t, 10_000), &varied(0, false), &cfg);
    // Two closed windows (the history keeps one), then 22 loops of an open one with one bad clock.
    for i in 0..(25 * 2 + 22) {
        t += 1;
        p.record(&ctx(t, 10_000 - i), &varied(i % 25 + i / 25, i == 60), &cfg);
    }
    assert_eq!((p.closed(), p.history().len(), p.open_loops()), (2, 1, 22));
    let open = p.open_window().expect("open");
    let start = open.get(Series::Start);
    assert!(
        start.mean != start.p95 && start.p95 != start.max,
        "{start:?}"
    );
    let calls = open.get(Series::Calls);
    assert_ne!(calls.sum, u64::from(calls.max));
    let text = p.summary(t, &cfg, &SCHEDULER_MMO);
    let want = format!(
        "loops from tick {}: {} | act per call ",
        open.first_time,
        expected_window_text(&open)
    );
    assert!(text.contains(&want), "missing {want:?} in {text}");
    assert!(
        text.contains("; bad readings: clock 1, snapshot 1 |"),
        "{text}"
    );

    let last = *p.history().back().expect("kept");
    let end = last.get(Series::End);
    assert_ne!(end.mean, end.p95);
    assert_ne!(end, last.get(Series::Start));
    let mut want = format!(
        " | windows closed 2 (kept 1), last from tick {} to {}: end mean/p95 ",
        last.first_time, last.last_time
    );
    write_cpu(&mut want, u64::from(end.mean));
    want.push('/');
    write_cpu(&mut want, u64::from(end.p95));
    want.push_str(" | segment 99:");
    assert!(text.contains(&want), "missing {want:?} in {text}");
}

/// A negative act cost per call (more intent CPU subtracted than measured) prints with its sign.
#[test]
fn summary_prints_a_negative_cost_per_call() {
    let cfg = ProfilerConfig {
        window_loops: 5,
        ..PROFILER_MMO
    };
    let mut p = Profiler::default();
    let s = Sample {
        marks: marks([1.0, 0.003, 0.25, 0.125, 0.05], 0.0),
        calls: 2,
        billed: 1,
    };
    p.record(&ctx(1, 10_000), &s, &cfg);
    p.record(&ctx(2, 10_000), &s, &cfg);
    let w = p.open_window().expect("open");
    let per = w.act.per_call().expect("calls");
    assert!(per < 0, "{per}");
    let mut want = String::from(" | act per call -");
    write_cpu(&mut want, per.unsigned_abs());
    want.push_str(" CPU over 2 calls in 1 ticks");
    let text = p.summary(2, &cfg, &SCHEDULER_MMO);
    assert!(text.contains(&want), "missing {want:?} in {text}");
}

/// The segment before any write carries `"lastFlush":null`, separates its windows with a comma,
/// and writes each window's bucket as [first,last,min] and each series as [n,sum,mean,p95,max,bad]
/// (two windows of 25 loops, so p95 differs from max and last from min).
#[test]
fn segment_lists_windows_in_order_with_their_fields() {
    let cfg = ProfilerConfig {
        window_loops: 25,
        history_windows: 2,
        stats_segment: 99,
    };
    let mut p = Profiler::default();
    let mut t = 500;
    p.record(&ctx(t, 9000), &varied(0, false), &cfg);
    for i in 0..50u32 {
        t += 1;
        // The bucket falls for 20 loops of each window, then climbs by 3 per loop.
        let k = i % 25;
        let bucket = if k < 20 {
            9000 - k * 10
        } else {
            9000 - 190 + (k - 19) * 3
        };
        p.record(&ctx(t, bucket), &varied(i, i == 30), &cfg);
    }
    assert_eq!(p.closed(), 2);
    let mut out = String::new();
    p.write_segment(t, &cfg, &SCHEDULER_MMO, &mut out);
    assert!(
        out.contains(r#""flushes":0,"flushErrors":0,"lastFlush":null,"#),
        "{out}"
    );
    assert!(
        out.contains(r#"}},{"first":"#),
        "a comma between windows: {out}"
    );
    let mut any_p95_below_max = false;
    for w in p.history() {
        assert_ne!(w.bucket_last, w.bucket_min);
        let bucket = format!(
            r#""bucket":[{},{},{}],"#,
            w.bucket_first, w.bucket_last, w.bucket_min
        );
        assert!(out.contains(&bucket), "missing {bucket}");
        let stats: Vec<String> = w
            .series
            .iter()
            .map(|s| {
                format!(
                    "[{},{},{},{},{},{}]",
                    s.n, s.sum, s.mean, s.p95, s.max, s.bad
                )
            })
            .collect();
        let want = format!(r#""stats":[{}]"#, stats.join(","));
        assert!(out.contains(&want), "missing {want}");
        any_p95_below_max |= w.series.iter().any(|s| s.p95 < s.max);
    }
    assert!(any_p95_below_max);
    assert!(out.ends_with("}]}"));
}
