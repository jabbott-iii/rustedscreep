//! The tick's time and CPU numbers (ROADMAP P1 module 5), read once at the start of the tick.

use rustedscreep_consts::{CPU_BUCKET_MAX, CPU_TICK_LIMIT_MAX};

/// `Game.time` and the three CPU numbers the scheduler budgets from. `tick_limit` and `bucket` are
/// clamped to what the engine allows; `time` and `cpu_limit` pass through as read.
///
/// On a server where the user has no CPU limit, the engine reports `limit` and `bucket` as
/// Infinity, and screeps-game-api's integer getters turn both into 0 (VERIFICATION_LOG V37, V100),
/// so this reads `cpu_limit` 0, `bucket` 0 and `tick_limit` `CPU_TICK_LIMIT_MAX`. The MMO always
/// reports finite values; the private-server track handles that case (ROADMAP_PRIVATE PS1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickContext {
    /// `Game.time`. Unit: ticks.
    pub time: u32,
    /// `Game.cpu.limit`: the CPU the bucket gains per tick before this tick's use; 0 can also mean
    /// an unlimited user (see above). Unit: CPU (ms).
    pub cpu_limit: u32,
    /// `Game.cpu.tickLimit`, floored and clamped to `0..=CPU_TICK_LIMIT_MAX`. Unit: CPU (ms).
    pub tick_limit: u32,
    /// `Game.cpu.bucket`, clamped to `0..=CPU_BUCKET_MAX`; 0 can also mean an unlimited user
    /// (see above). Unit: CPU (ms).
    pub bucket: u32,
}

impl TickContext {
    /// Builds the context from the raw values with the types screeps-game-api 0.23.5 returns
    /// (`time() -> u32`, `limit() -> u32`, `tick_limit() -> f64`, `bucket() -> i32`).
    ///
    /// Uses: `CPU_TICK_LIMIT_MAX`, `CPU_BUCKET_MAX`. The driver sets
    /// `tickLimit = min(CPU_TICK_LIMIT_MAX, max(0, bucket) + limit)` (VERIFICATION_LOG V37), so a
    /// finite value passes through unchanged (Infinity does not; see [`TickContext`]). The rest is
    /// defensive:
    /// - `tick_limit`: the simulator reports `+Infinity` (crate docs), which becomes
    ///   `CPU_TICK_LIMIT_MAX`; values above that cap are clamped to it; fractions are floored and
    ///   negatives become 0, both erring low because overrunning tickLimit kills the tick. NaN, which
    ///   no source produces, becomes `min(cpu_limit, CPU_TICK_LIMIT_MAX)`, a lower bound of the
    ///   driver's formula.
    /// - `bucket`: negatives become 0 and values above `CPU_BUCKET_MAX` are clamped to it.
    pub fn from_raw(time: u32, cpu_limit: u32, tick_limit: f64, bucket: i32) -> Self {
        let max_tick = f64::from(CPU_TICK_LIMIT_MAX);
        let tick_limit = if tick_limit.is_nan() {
            cpu_limit.min(CPU_TICK_LIMIT_MAX)
        } else if tick_limit >= max_tick {
            CPU_TICK_LIMIT_MAX
        } else if tick_limit <= 0.0 {
            0
        } else {
            // In range (0, CPU_TICK_LIMIT_MAX) here, so the saturating cast is exact after floor().
            tick_limit.floor() as u32
        };
        let bucket = u32::try_from(bucket).unwrap_or(0).min(CPU_BUCKET_MAX);
        Self {
            time,
            cpu_limit,
            tick_limit,
            bucket,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uses: CPU_TICK_LIMIT_MAX, CPU_BUCKET_MAX. Every combination the driver's formula produces
    /// passes through unchanged (V37: tickLimit = min(CPU_TICK_LIMIT_MAX, max(0, bucket) + limit)).
    #[test]
    fn engine_values_pass_through() {
        let limits = [0, 1, 20, 300];
        let buckets = [
            0,
            1,
            CPU_TICK_LIMIT_MAX / 2,
            CPU_TICK_LIMIT_MAX,
            CPU_BUCKET_MAX,
        ];
        for limit in limits {
            for bucket in buckets {
                let tick_limit = CPU_TICK_LIMIT_MAX.min(bucket + limit);
                let raw_bucket = i32::try_from(bucket).expect("fits");
                let ctx = TickContext::from_raw(7, limit, f64::from(tick_limit), raw_bucket);
                assert_eq!(
                    ctx,
                    TickContext {
                        time: 7,
                        cpu_limit: limit,
                        tick_limit,
                        bucket,
                    }
                );
            }
        }
    }

    /// Uses: CPU_TICK_LIMIT_MAX. The simulator's Infinity and anything above the cap clamp to it.
    #[test]
    fn tick_limit_clamps_to_the_cap() {
        let max = f64::from(CPU_TICK_LIMIT_MAX);
        for raw in [f64::INFINITY, max, max + 0.5, max * 2.0, f64::MAX] {
            assert_eq!(
                TickContext::from_raw(0, 20, raw, 0).tick_limit,
                CPU_TICK_LIMIT_MAX,
                "{raw}"
            );
        }
    }

    /// Uses: CPU_TICK_LIMIT_MAX. Fractions floor and negatives become 0, both erring low.
    #[test]
    fn tick_limit_errs_low() {
        let below_cap = f64::from(CPU_TICK_LIMIT_MAX - 1);
        assert_eq!(
            TickContext::from_raw(0, 20, below_cap + 0.9, 0).tick_limit,
            CPU_TICK_LIMIT_MAX - 1
        );
        for raw in [0.0, -0.0, -1.0, f64::NEG_INFINITY, 0.4] {
            assert_eq!(TickContext::from_raw(0, 20, raw, 0).tick_limit, 0, "{raw}");
        }
    }

    /// Uses: CPU_TICK_LIMIT_MAX. NaN falls back to the formula's lower bound, min(limit, cap).
    #[test]
    fn nan_tick_limit_uses_the_lower_bound() {
        let limit = 20;
        assert_eq!(
            TickContext::from_raw(0, limit, f64::NAN, 0).tick_limit,
            limit.min(CPU_TICK_LIMIT_MAX)
        );
        let above_cap = CPU_TICK_LIMIT_MAX + 1;
        assert_eq!(
            TickContext::from_raw(0, above_cap, f64::NAN, 0).tick_limit,
            CPU_TICK_LIMIT_MAX
        );
    }

    /// Uses: CPU_BUCKET_MAX. The bucket is clamped into 0..=CPU_BUCKET_MAX.
    #[test]
    fn bucket_clamps() {
        let max = i32::try_from(CPU_BUCKET_MAX).expect("fits");
        let cases = [
            (i32::MIN, 0),
            (-1, 0),
            (0, 0),
            (max, CPU_BUCKET_MAX),
            (max + 1, CPU_BUCKET_MAX),
            (i32::MAX, CPU_BUCKET_MAX),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                TickContext::from_raw(0, 20, 0.0, raw).bucket,
                expected,
                "{raw}"
            );
        }
    }
}
