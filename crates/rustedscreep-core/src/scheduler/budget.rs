//! The per-tick CPU budget (ROADMAP P1 module 6): which tiers may run this tick, and up to what
//! CPU. It is plain arithmetic on the tick context the snapshot already read, so it costs no JS call.

use crate::config::SchedulerConfig;
use crate::snapshot::TickContext;

/// CPU tiers, in the order a tick runs them (CLAUDE.md "CPU tiers").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Spawning, defense, harvesting: runs every tick, stopped only by the ceiling.
    Critical,
    /// Hauling, upgrading, building, offense operations: runs unless the bucket is critically low.
    Standard,
    /// Planning, market, scouting, visuals, pixels, offense planning: only above bucket thresholds,
    /// never on the first loops of a new global.
    Optional,
}

impl Tier {
    pub const ALL: [Self; 3] = [Self::Critical, Self::Standard, Self::Optional];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Standard => "standard",
            Self::Optional => "optional",
        }
    }
}

/// Why a tier does not run this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Off {
    /// The bucket is below the tier's minimum.
    BucketLow,
    /// The global has not run its warm-up loops yet.
    WarmUp,
    /// The bucket is so far below the target that the Optional cap is not above 0.
    NoSurplus,
}

impl Off {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BucketLow => "bucket below minimum",
            Self::WarmUp => "warm-up",
            Self::NoSurplus => "no surplus",
        }
    }
}

/// A tier's cap for this tick: the tick's CPU (clock reading plus committed cost) at which the
/// tier stops, or why it does not run at all.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cap {
    On(f64),
    Off(Off),
}

/// This tick's caps. Every cap is a level of the whole tick's CPU, not a share for one tier: a tier
/// runs while the tick's CPU so far is below its cap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    /// Every tier stops here, Critical included (ADR-013). Running past `tickLimit` costs
    /// either way: a tick the engine kills by wall time at tickLimit + 5 ms (at least 30 ms; V105) is an
    /// aborted tick, which the loader recovers from with a global reset (ADR-003); and while the
    /// bucket is below `CPU_TICK_LIMIT_MAX − limit`, tickLimit is `bucket + limit`, so a tick that runs
    /// past it without being killed is billed into a negative bucket and the next tick is skipped
    /// (V37). Unit: CPU.
    pub ceiling: f64,
    /// `tick_limit − ceiling`: the CPU kept for the work after the decide step and for overshoot.
    /// The slicer also sizes its batches from it. Unit: CPU.
    pub reserve: f64,
    pub standard: Cap,
    pub optional: Cap,
}

impl Budget {
    /// The caps for one tick. `loops_before` is how many loops this global ran before this one.
    ///
    /// Uses: `CPU_TICK_LIMIT_MAX` and `CPU_BUCKET_MAX`, through [`TickContext::from_raw`]'s clamps.
    /// The engine sets `tickLimit = min(CPU_TICK_LIMIT_MAX, max(0, bucket) + limit)` before the tick
    /// and refills `bucket = min(CPU_BUCKET_MAX, bucket + limit − billed)` after it, where billed is
    /// `ceil(CPU + 0.2 × intents)` (VERIFICATION_LOG V37, V106).
    /// - `ceiling = tick_limit × (100 − tick_reserve_pct) / 100`, and `reserve = tick_limit − ceiling`.
    /// - Standard: on when `bucket ≥ standard_bucket_min`, capped at the ceiling.
    /// - Optional: on when `bucket ≥ optional_bucket_min` and `loops_before ≥ optional_warmup_loops`,
    ///   capped at `min(ceiling, limit + (bucket − bucket_target) / surplus_spread_ticks)`, and off
    ///   when that is not above 0. At the target the cap is `limit`; below it the cap is lower, so the
    ///   bucket refills; above it Optional work spends the surplus, which shrinks by a factor of about
    ///   `1 − 1/surplus_spread_ticks` per tick when the cap is filled. The cap is checked during the
    ///   decide step, so the work after it, the slicer's overshoot and ceil billing come on top: with
    ///   Optional demand that always fills the cap, the bucket settles slightly below the target, by
    ///   about `surplus_spread_ticks` CPU per CPU of that excess per tick.
    ///
    /// An unlimited user (a private server without CPU limits) reads `cpu_limit` 0 and `bucket` 0
    /// (V37, V100) and `tick_limit` `CPU_TICK_LIMIT_MAX`, so only Critical runs, under the full
    /// ceiling; ROADMAP_PRIVATE PS1's profile handles that case.
    ///
    /// The u32 to f64 conversions are lossless and each operation is correctly rounded, so the result
    /// is deterministic. A zero `surplus_spread_ticks` (no profile has one; module 10 clamps
    /// overrides) is read as 1 rather than dividing by zero.
    pub fn compute(ctx: &TickContext, cfg: &SchedulerConfig, loops_before: u32) -> Self {
        let tick_limit = f64::from(ctx.tick_limit);
        let kept = f64::from(100u32.saturating_sub(cfg.tick_reserve_pct.min(100)));
        let ceiling = tick_limit * kept / 100.0;

        let standard = if ctx.bucket >= cfg.standard_bucket_min {
            Cap::On(ceiling)
        } else {
            Cap::Off(Off::BucketLow)
        };

        let optional = if ctx.bucket < cfg.optional_bucket_min {
            Cap::Off(Off::BucketLow)
        } else if loops_before < cfg.optional_warmup_loops {
            Cap::Off(Off::WarmUp)
        } else {
            let surplus = f64::from(ctx.bucket) - f64::from(cfg.bucket_target);
            let spread = f64::from(cfg.surplus_spread_ticks.max(1));
            let cap = (f64::from(ctx.cpu_limit) + surplus / spread).min(ceiling);
            if cap > 0.0 {
                Cap::On(cap)
            } else {
                Cap::Off(Off::NoSurplus)
            }
        };

        Self {
            ceiling,
            reserve: tick_limit - ceiling,
            standard,
            optional,
        }
    }

    /// The cap of `tier` this tick, or `None` when it does not run. Critical always has the ceiling.
    pub fn cap(&self, tier: Tier) -> Option<f64> {
        match tier {
            Tier::Critical => Some(self.ceiling),
            Tier::Standard => cap_value(self.standard),
            Tier::Optional => cap_value(self.optional),
        }
    }

    /// Whether `tier` runs at all this tick.
    pub fn allows(&self, tier: Tier) -> bool {
        self.cap(tier).is_some()
    }
}

fn cap_value(cap: Cap) -> Option<f64> {
    match cap {
        Cap::On(v) => Some(v),
        Cap::Off(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SCHEDULER_MMO;
    use rustedscreep_consts::{CPU_BUCKET_MAX, CPU_TICK_LIMIT_MAX};

    /// A tick as the engine would report it: tickLimit from the driver's formula (V37).
    fn tick(limit: u32, bucket: u32) -> TickContext {
        let tick_limit = CPU_TICK_LIMIT_MAX.min(bucket + limit);
        TickContext::from_raw(
            1,
            limit,
            f64::from(tick_limit),
            i32::try_from(bucket).expect("fits"),
        )
    }

    fn ceiling_of(tick_limit: u32, cfg: &SchedulerConfig) -> f64 {
        f64::from(tick_limit) * f64::from(100 - cfg.tick_reserve_pct) / 100.0
    }

    /// Uses: CPU_TICK_LIMIT_MAX. The ceiling keeps the reserve below tickLimit for every limit and
    /// bucket, and never exceeds tickLimit.
    #[test]
    fn the_ceiling_keeps_the_reserve() {
        let cfg = SCHEDULER_MMO;
        for limit in [0, 1, 20, 300] {
            for bucket in [0, 1, 100, CPU_TICK_LIMIT_MAX, CPU_BUCKET_MAX] {
                let ctx = tick(limit, bucket);
                let b = Budget::compute(&ctx, &cfg, 10);
                assert_eq!(b.ceiling, ceiling_of(ctx.tick_limit, &cfg));
                assert!(b.ceiling <= f64::from(ctx.tick_limit));
                assert_eq!(b.cap(Tier::Critical), Some(b.ceiling));
                for tier in Tier::ALL {
                    if let Some(cap) = b.cap(tier) {
                        assert!(
                            cap > 0.0 || ctx.tick_limit == 0,
                            "{tier:?} {limit} {bucket}"
                        );
                        assert!(cap <= b.ceiling, "{tier:?} {limit} {bucket}");
                    }
                }
            }
        }
    }

    /// Uses: CPU_TICK_LIMIT_MAX. At an empty bucket tickLimit is the CPU limit (V37), and Critical
    /// still gets most of it.
    #[test]
    fn an_empty_bucket_still_runs_critical() {
        let cfg = SCHEDULER_MMO;
        let limit = 20;
        let b = Budget::compute(&tick(limit, 0), &cfg, 10);
        assert_eq!(b.ceiling, ceiling_of(limit, &cfg));
        assert_eq!(b.standard, Cap::Off(Off::BucketLow));
        assert_eq!(b.optional, Cap::Off(Off::BucketLow));
    }

    /// Standard switches on exactly at its minimum.
    #[test]
    fn standard_gate() {
        let cfg = SCHEDULER_MMO;
        let below = Budget::compute(&tick(20, cfg.standard_bucket_min - 1), &cfg, 10);
        let at = Budget::compute(&tick(20, cfg.standard_bucket_min), &cfg, 10);
        assert_eq!(below.standard, Cap::Off(Off::BucketLow));
        assert_eq!(at.standard, Cap::On(at.ceiling));
    }

    /// The first loops of a global never run Optional work, even with a full bucket.
    #[test]
    fn optional_waits_for_warm_up() {
        let cfg = SCHEDULER_MMO;
        let ctx = tick(20, CPU_BUCKET_MAX);
        for loops in 0..cfg.optional_warmup_loops {
            let b = Budget::compute(&ctx, &cfg, loops);
            assert_eq!(b.optional, Cap::Off(Off::WarmUp), "loop {loops}");
            assert!(b.allows(Tier::Standard) && b.allows(Tier::Critical));
        }
        let b = Budget::compute(&ctx, &cfg, cfg.optional_warmup_loops);
        assert!(b.allows(Tier::Optional));
    }

    /// Uses: CPU_BUCKET_MAX. The Optional cap is the CPU limit at the target, rises by 1 CPU per
    /// `surplus_spread_ticks` of surplus, falls the same way below it, and is clamped to the ceiling.
    #[test]
    fn optional_cap_follows_the_bucket() {
        let cfg = SCHEDULER_MMO;
        let limit = 20;
        let spread = f64::from(cfg.surplus_spread_ticks);
        for bucket in [
            cfg.optional_bucket_min,
            cfg.bucket_target - 1,
            cfg.bucket_target,
            cfg.bucket_target + 1,
            CPU_BUCKET_MAX,
        ] {
            let b = Budget::compute(&tick(limit, bucket), &cfg, 10);
            let raw =
                f64::from(limit) + (f64::from(bucket) - f64::from(cfg.bucket_target)) / spread;
            let expected = if raw > 0.0 {
                Cap::On(raw.min(b.ceiling))
            } else {
                Cap::Off(Off::NoSurplus)
            };
            assert_eq!(b.optional, expected, "bucket {bucket}");
        }
        let at_target = Budget::compute(&tick(limit, cfg.bucket_target), &cfg, 10);
        assert_eq!(at_target.optional, Cap::On(f64::from(limit)));
        // A limit of 300 at a full bucket would ask for more than the ceiling.
        let big = SchedulerConfig {
            surplus_spread_ticks: 1,
            ..cfg
        };
        let b = Budget::compute(&tick(300, CPU_BUCKET_MAX), &big, 10);
        assert_eq!(b.optional, Cap::On(b.ceiling));
    }

    /// The Optional cap never decreases as the bucket grows (monotone), over every bucket value.
    #[test]
    fn optional_cap_is_monotone_in_the_bucket() {
        let cfg = SCHEDULER_MMO;
        for limit in [1, 20, 300] {
            let mut last = 0.0f64;
            for bucket in 0..=CPU_BUCKET_MAX {
                let cap = Budget::compute(&tick(limit, bucket), &cfg, 10)
                    .cap(Tier::Optional)
                    .unwrap_or(0.0);
                assert!(cap >= last, "limit {limit} bucket {bucket}");
                assert!(!cap.is_nan());
                last = cap;
            }
        }
    }

    /// At the target the Optional cap is the limit, so a tick whose total use is exactly the cap
    /// leaves the bucket where it was under the driver's refill (V37): bucket + limit − used = bucket.
    /// (Work after the decide step and ceil billing come on top; see `Budget::compute`.)
    #[test]
    fn the_cap_equals_the_limit_at_the_target() {
        let cfg = SCHEDULER_MMO;
        let limit = 20u32;
        let b = Budget::compute(&tick(limit, cfg.bucket_target), &cfg, 10);
        let used = b.cap(Tier::Optional).expect("on");
        let refilled =
            (f64::from(cfg.bucket_target) + f64::from(limit) - used).min(f64::from(CPU_BUCKET_MAX));
        assert_eq!(refilled, f64::from(cfg.bucket_target));
    }

    /// Uses: CPU_TICK_LIMIT_MAX. While the bucket is below `CPU_TICK_LIMIT_MAX − limit`, tickLimit is
    /// `bucket + limit` (V37) and the reserve is that times `tick_reserve_pct` / 100: at an empty bucket
    /// only a tenth of the limit (2 CPU at the free tier) stands between the ceiling and a negative
    /// bucket.
    #[test]
    fn the_reserve_at_a_low_bucket() {
        let cfg = SCHEDULER_MMO;
        for limit in [1, 20, 300] {
            for bucket in [0, 1, 100, CPU_TICK_LIMIT_MAX.saturating_sub(limit + 1)] {
                let ctx = tick(limit, bucket);
                assert_eq!(ctx.tick_limit, bucket + limit);
                let b = Budget::compute(&ctx, &cfg, 10);
                let expected = f64::from(ctx.tick_limit) - ceiling_of(ctx.tick_limit, &cfg);
                assert_eq!(b.reserve, expected);
                assert!((b.ceiling + b.reserve - f64::from(bucket + limit)).abs() < 1e-9);
            }
        }
    }

    /// Uses: CPU_TICK_LIMIT_MAX. An unlimited user reads limit 0, bucket 0 and an Infinity tickLimit
    /// (V37, V100): only Critical runs, under the ceiling of the clamped tickLimit, until PS1's profile
    /// handles it.
    #[test]
    fn an_unlimited_user_runs_only_critical() {
        let cfg = SCHEDULER_MMO;
        let ctx = TickContext::from_raw(1, 0, f64::INFINITY, 0);
        let b = Budget::compute(&ctx, &cfg, 10);
        assert_eq!(b.ceiling, ceiling_of(CPU_TICK_LIMIT_MAX, &cfg));
        assert_eq!(b.standard, Cap::Off(Off::BucketLow));
        assert_eq!(b.optional, Cap::Off(Off::BucketLow));
    }

    /// A zero spread is read as 1: at the target the cap is still the limit, not NaN (which `min`
    /// would turn into the whole ceiling).
    #[test]
    fn a_zero_spread_is_read_as_one() {
        let cfg = SchedulerConfig {
            surplus_spread_ticks: 0,
            ..SCHEDULER_MMO
        };
        let limit = 20;
        let b = Budget::compute(&tick(limit, cfg.bucket_target), &cfg, 10);
        assert_eq!(b.optional, Cap::On(f64::from(limit)));
        let above = Budget::compute(&tick(limit, cfg.bucket_target + 5), &cfg, 10);
        assert_eq!(above.optional, Cap::On(f64::from(limit + 5)));
    }

    /// A bad override (module 10 clamps them) cannot divide by zero or push the ceiling above
    /// tickLimit.
    #[test]
    fn bad_values_stay_safe() {
        let cfg = SchedulerConfig {
            tick_reserve_pct: 250,
            surplus_spread_ticks: 0,
            ..SCHEDULER_MMO
        };
        let b = Budget::compute(&tick(20, CPU_BUCKET_MAX), &cfg, 10);
        assert_eq!(b.ceiling, 0.0);
        assert_eq!(b.optional, Cap::Off(Off::NoSurplus));
        let cfg = SchedulerConfig {
            tick_reserve_pct: 0,
            ..SCHEDULER_MMO
        };
        let b = Budget::compute(&tick(20, CPU_BUCKET_MAX), &cfg, 10);
        assert_eq!(b.ceiling, f64::from(CPU_TICK_LIMIT_MAX));
    }
}
