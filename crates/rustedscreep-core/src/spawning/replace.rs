//! Pre-spawn replacement (ROADMAP P5.2): a job replaces a creep before it dies, so the successor
//! reaches the creep's post by the tick the creep is gone.
//!
//! The timing, in `Game.time` ticks:
//! - An order accepted on tick `O` sets the spawn's `spawnTime` to `O + N`, `N` the body's spawn
//!   time (create-creep.js L49-63, V170, V173). The spawn places the creep while it processes tick
//!   `spawnTime - 1` (spawns/tick.js, V174), so the creep stands outside on tick `O + N` and may move
//!   then.
//! - It walks `W` ticks to its post and works there from tick `O + N + W`.
//! - A creep that shows `ticksToLive` `t` on tick `T` is gone by tick `T + t`, its `ageTime`
//!   (game/creeps.js L77, V198). It dies while tick `T + t - 1` is processed (creeps/tick.js L93-95,
//!   V170), after that tick's intents (V202, inferred), so `T + t - 1` is the last tick it acts.
//!
//! So an order on a tick on which the old creep shows `t ≥ N + W` puts the successor at the post
//! by the tick the old creep is gone, and no tick goes by without one. A job counts a creep whose
//! `ticksToLive` is at most its [`lead`] as due ([`is_due`]). A due creep no longer fills its
//! slot, so the job asks for its successor while the creep still works. The lead is `N + W` plus
//! `spawn.replace_margin_ticks`. A miner's, hauler's and upgrader's walk `W` is an upper bound on
//! its plan's way (`economy::sizing::walk_ticks_max`; an upgrader's adds an estimated step round
//! the store to its post), so the margin covers what the order may wait for (energy, a busy
//! spawn), where the spawn puts the creep, and a miner's successor's step from the tile it waits on
//! (another tile next to the source than the mining tile). The opening's
//! workers are not replaced early: they hold no post, and as a pool one short for a spawn time
//! costs less than the early bodies (measured in the world model, `SpawnConfig::replace`).
//! A creep whose `ticksToLive` is not known (spawning, or not read yet) is never due.
//!
//! The lead is capped at half the body's life. A successor leaves its spawn with a whole life, so it
//! becomes due at least a life less the lead after that, and its predecessor is gone at most the
//! lead after it became due. With the cap, a successor is never due while its predecessor lives,
//! and a slot never holds more than two creeps. Without the cap, a long walk or a large margin would
//! make each creep due as it leaves the spawn, and every new creep would order another.

use super::body::Body;
use crate::config::SpawnConfig;

/// The lead time for replacing a creep whose successor has `body` and walks `walk` ticks from its
/// spawn to its post: the body's spawn time, the walk and `margin`, at most half the body's life
/// (module docs). Unit: ticks.
///
/// Uses: `CREEP_SPAWN_TIME` ([`Body::spawn_time`]); `CREEP_LIFE_TIME` and `CREEP_CLAIM_LIFE_TIME`
/// ([`Body::life`]).
pub fn lead_ticks(body: &Body, walk: u32, margin: u32) -> u32 {
    body.spawn_time()
        .saturating_add(walk)
        .saturating_add(margin)
        .min(body.life() / 2)
}

/// [`lead_ticks`] with `cfg.replace_margin_ticks`, or 0 while `cfg.replace` is off: no creep is
/// then due ([`is_due`]), and each job orders a successor only once the creep is gone. Unit: ticks.
///
/// Uses: [`lead_ticks`].
pub fn lead(cfg: &SpawnConfig, body: &Body, walk: u32) -> u32 {
    if cfg.replace {
        lead_ticks(body, walk, cfg.replace_margin_ticks)
    } else {
        0
    }
}

/// Whether a creep that shows `ticks_to_live` (`None` while it is not known) is due for
/// replacement under `lead` (module docs). Never for a lead of 0.
pub fn is_due(ticks_to_live: Option<u32>, lead: u32) -> bool {
    lead > 0 && ticks_to_live.is_some_and(|t| t <= lead)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SPAWN_MMO;
    use rustedscreep_consts::{
        CREEP_CLAIM_LIFE_TIME, CREEP_LIFE_TIME, CREEP_SPAWN_TIME, MAX_CREEP_SIZE, Part,
    };

    fn body(parts: usize, claim: bool) -> Body {
        let mut p = vec![Part::Move; parts];
        if claim && let Some(first) = p.first_mut() {
            *first = Part::Claim;
        }
        Body::new(&p).expect("body")
    }

    /// Uses: CREEP_SPAWN_TIME, CREEP_LIFE_TIME. The lead is `CREEP_SPAWN_TIME` a part, the walk
    /// and the margin; switched off it is 0, and nothing is due under a lead of 0.
    #[test]
    fn the_lead_is_spawn_time_walk_and_margin() {
        let b = body(8, false);
        let (walk, margin) = (17, SPAWN_MMO.replace_margin_ticks);
        assert_eq!(
            lead_ticks(&b, walk, margin),
            8 * CREEP_SPAWN_TIME + walk + margin
        );
        assert_eq!(
            lead(&SPAWN_MMO, &b, walk),
            8 * CREEP_SPAWN_TIME + walk + margin
        );
        let off = SpawnConfig {
            replace: false,
            ..SPAWN_MMO
        };
        assert_eq!(lead(&off, &b, walk), 0);
        assert!(!is_due(Some(1), 0));
        assert!(!is_due(Some(0), 0));
        let l = lead_ticks(&b, walk, margin);
        assert!(is_due(Some(l), l));
        assert!(is_due(Some(1), l));
        assert!(!is_due(Some(l + 1), l));
        assert!(!is_due(None, l), "spawning, or not read yet");
        assert!(l < CREEP_LIFE_TIME / 2);
    }

    /// Uses: CREEP_LIFE_TIME, CREEP_CLAIM_LIFE_TIME, MAX_CREEP_SIZE, CREEP_SPAWN_TIME. Whatever the
    /// body, walk and margin, the lead is at most half the body's life, so a creep that has just
    /// left its spawn with a whole life is never due.
    #[test]
    fn the_lead_is_at_most_half_a_life() {
        for parts in 1..=MAX_CREEP_SIZE as usize {
            for claim in [false, true] {
                let b = body(parts, claim);
                let life = if claim {
                    CREEP_CLAIM_LIFE_TIME
                } else {
                    CREEP_LIFE_TIME
                };
                for walk in [0, 50, 500, u32::MAX] {
                    for margin in [0, 20, 300, u32::MAX] {
                        let l = lead_ticks(&b, walk, margin);
                        assert!(l <= life / 2, "{parts} {claim} {walk} {margin}");
                        assert!(!is_due(Some(life), l));
                        let full = b.spawn_time().saturating_add(walk).saturating_add(margin);
                        assert_eq!(l, full.min(life / 2));
                    }
                }
            }
        }
        assert_eq!(
            lead_ticks(&body(MAX_CREEP_SIZE as usize, false), 0, 0),
            MAX_CREEP_SIZE * CREEP_SPAWN_TIME
        );
    }

    /// One post's creeps over several lives, with the engine's timing (module docs): each creep
    /// is out of its spawn `spawn_time` ticks after its order, walks `walk` ticks, and is gone a
    /// life after it left the spawn. A successor is ordered `wait` ticks (for a spawn and the
    /// energy) after the first tick its predecessor is due ([`is_due`]) or gone, whichever comes
    /// first (under a lead of 0, replacement off, only gone). Returns, per handover, the ticks the
    /// post stood empty (0 when the successor was there in time) and the most creeps alive at
    /// once.
    fn chain(b: &Body, walk: u32, lead: u32, wait: u32, lives: u32) -> (Vec<u32>, usize) {
        let life = b.life();
        // Each creep: the tick it is out of the spawn, the tick it is at its post, the tick it is
        // gone by.
        let mut creeps: Vec<(u32, u32, u32)> = Vec::new();
        let first_out = b.spawn_time();
        creeps.push((first_out, first_out + walk, first_out + life));
        let mut most = 1;
        let mut ordered_for: Option<usize> = None;
        let mut due_since: Option<u32> = None;
        let mut t = 0;
        while creeps.len() < lives as usize + 1 {
            let last = creeps.len() - 1;
            let (out, _, gone) = creeps[last];
            // The newest creep's ticksToLive once it is out (V198).
            let ttl = (t >= out && t < gone).then(|| gone - t);
            // Due under the lead, or gone: a job whose creep is gone asks for one whatever the
            // lead (its count dropped).
            let due = is_due(ttl, lead) || t >= gone;
            if ordered_for != Some(last) && due {
                let since = *due_since.get_or_insert(t);
                if t >= since + wait {
                    let o = t + b.spawn_time();
                    creeps.push((o, o + walk, o + life));
                    ordered_for = Some(last);
                    due_since = None;
                }
            }
            let alive = creeps.iter().filter(|c| t < c.2).count();
            most = most.max(alive);
            t += 1;
            assert!(t < 10 * life * (lives + 1), "the chain runs on");
        }
        let gaps = creeps
            .windows(2)
            .map(|pair| pair[1].1.saturating_sub(pair[0].2))
            .collect();
        (gaps, most)
    }

    /// Uses: CREEP_SPAWN_TIME, CREEP_LIFE_TIME (through `chain`). Ordered on the first due tick,
    /// a successor is at the post by the tick its predecessor is gone, for every life, with the
    /// default margin and with none: no handover gap, and at most two creeps at once.
    #[test]
    fn a_successor_ordered_when_due_is_there_in_time() {
        for (parts, walk) in [(1, 0), (8, 12), (25, 40), (50, 100)] {
            let b = body(parts, false);
            for margin in [0, SPAWN_MMO.replace_margin_ticks] {
                let (gaps, most) = chain(&b, walk, lead_ticks(&b, walk, margin), 0, 5);
                assert_eq!(
                    gaps,
                    vec![0; 5],
                    "{parts} parts, walk {walk}, margin {margin}"
                );
                assert!(most <= 2, "{most}");
            }
        }
    }

    /// Uses: CREEP_SPAWN_TIME, CREEP_LIFE_TIME (through `chain`). An order that waits `wait`
    /// ticks after the creep became due leaves the post empty for whatever of the wait the margin
    /// does not cover; with the margin at 0 the gap is the wait itself.
    #[test]
    fn a_wait_past_the_margin_is_the_gap() {
        let b = body(8, false);
        let walk = 12;
        let margin = SPAWN_MMO.replace_margin_ticks;
        for wait in [0, margin / 2, margin, margin + 7, 3 * margin] {
            let (gaps, _) = chain(&b, walk, lead_ticks(&b, walk, margin), wait, 3);
            assert_eq!(gaps, vec![wait.saturating_sub(margin); 3], "wait {wait}");
            let (gaps, _) = chain(&b, walk, lead_ticks(&b, walk, 0), wait, 3);
            assert_eq!(gaps, vec![wait; 3], "no margin, wait {wait}");
        }
    }

    /// Uses: CREEP_SPAWN_TIME, CREEP_LIFE_TIME (through `chain`). With replacement off (a lead of
    /// 0) the successor is ordered once the creep is gone, so each handover leaves the post empty
    /// for the successor's spawn time and walk: what replacement saves. One creep at a time.
    #[test]
    fn without_replacement_the_gap_is_spawn_time_and_walk() {
        let off = SpawnConfig {
            replace: false,
            ..SPAWN_MMO
        };
        for (parts, walk) in [(1, 0), (8, 12), (50, 100)] {
            let b = body(parts, false);
            assert_eq!(lead(&off, &b, walk), 0);
            let (gaps, most) = chain(&b, walk, 0, 0, 3);
            assert_eq!(gaps, vec![parts as u32 * CREEP_SPAWN_TIME + walk; 3]);
            assert_eq!(most, 1);
        }
    }
}
