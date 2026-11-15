//! Deadlock watchdogs for the opening (ROADMAP P2.4): one per owned room, fed once per tick with
//! what the room did. It starts and ends a recovery when the room has no worker that can work,
//! keeps an upgrader on a controller that nears its downgrade, caps the bodies of a room that waited
//! too long for its full capacity at the most energy it was seen holding, and reports a room whose
//! economy stopped moving (the opening then plans the room again from scratch). Pure state; the glue
//! logs the events.

use rustedscreep_consts::CONTROLLER_DOWNGRADE_RESTORE;

use crate::config::OpeningConfig;
use crate::snapshot::ControllerState;

/// Something the glue reports (rate-limited by the transitions themselves: each fires once per
/// change of state).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// No worker in the room can work (none alive, or every one still spawning) while its plan
    /// wants some: spawns size to the energy the room holds until `min_workers` can work again.
    RecoveryStarted,
    /// `min_workers` workers can work again, `ticks` after the recovery started.
    Recovered { ticks: u32 },
    /// `ticksToDowngrade` fell below `downgrade_guard_ticks`: one worker upgrades before anything
    /// but filling the spawns, and keeps upgrading until its store is empty (before filling, too).
    GuardOn { downgrade_ticks: u32 },
    /// It is back at `downgrade_guard_ticks + CONTROLLER_DOWNGRADE_RESTORE` or above.
    GuardOff,
    /// A worker request waited `ticks` for the room to hold its full-capacity body: from now on
    /// the room's bodies are capped at `energy`, the most it was seen holding (raised whenever it
    /// holds more), until it is seen full.
    WaitFallback { ticks: u32, energy: u32 },
    /// Nothing moved for `ticks` while the room had workers that could work: no controller
    /// progress, no harvest (the sources' energy) and no construction progress. The spawns' own
    /// regeneration does not count. The opening drops the room's plan and its workers' tasks.
    Stalled { ticks: u32 },
    /// Something moved again, `ticks` after the stall was reported.
    StallEnded { ticks: u32 },
}

/// What a room did this tick, as the watchdog reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub time: u32,
    /// Workers alive, spawning ones included. Unit: creeps.
    pub alive: u32,
    /// Workers that can work: alive, not spawning, with a store. Unit: creeps.
    pub working: u32,
    /// Workers the room's plan wants (0 without a source to harvest). Unit: creeps.
    pub want: u32,
    pub level: u8,
    pub controller: ControllerState,
    /// `Room.energyAvailable` and `energyCapacityAvailable`. Unit: energy.
    pub energy: u32,
    pub capacity: u32,
    /// The energy left in the room's sources: only harvesting lowers it (V183, V184). Unit: energy.
    pub sources: u32,
    /// The sum of the room's own sites' progress. Unit: energy.
    pub building: u32,
    /// A worker request is open this tick.
    pub requesting: bool,
}

/// One room's watchdog. A global reset starts it over: a recovery or a stall in progress is
/// reported again (once) when it is seen again.
#[derive(Clone, Copy, Debug, Default)]
pub struct Watchdog {
    recovering_since: Option<u32>,
    guard: bool,
    /// What the room looked like when something last moved, and when.
    mark: Option<(u8, u32, u32, u32)>,
    moved_at: u32,
    stalled_at: Option<u32>,
    /// Since when a worker request has been open with no new worker, the count then, and the most
    /// energy the room held since.
    waiting_since: Option<u32>,
    alive_then: u32,
    peak: u32,
    /// The cap on bodies after a wait fallback, and the capacity it was set at.
    cap: Option<(u32, u32)>,
}

impl Watchdog {
    /// Notes this tick; pushes what changed to `events`. Returns whether the room just stalled
    /// (the caller re-plans it).
    pub fn observe(
        &mut self,
        o: &Observation,
        cfg: &OpeningConfig,
        events: &mut Vec<Event>,
    ) -> bool {
        let now = o.time;
        // Recovery: no worker can work.
        match self.recovering_since {
            None if o.working == 0 && o.want > 0 => {
                self.recovering_since = Some(now);
                events.push(Event::RecoveryStarted);
            }
            Some(since) if o.working >= cfg.min_workers.max(1) => {
                self.recovering_since = None;
                events.push(Event::Recovered {
                    ticks: now.wrapping_sub(since),
                });
            }
            _ => {}
        }
        // The downgrade guard, on below the threshold and off one upgrade tick's restore above it
        // (V189), so one upgrade does not turn it off again at once.
        let ticks = o.controller.downgrade_ticks;
        let release = cfg
            .downgrade_guard_ticks
            .saturating_add(CONTROLLER_DOWNGRADE_RESTORE);
        let guard = if o.level == 0 {
            false
        } else if self.guard {
            ticks < release
        } else {
            ticks < cfg.downgrade_guard_ticks
        };
        if guard != self.guard {
            self.guard = guard;
            events.push(if guard {
                Event::GuardOn {
                    downgrade_ticks: ticks,
                }
            } else {
                Event::GuardOff
            });
        }
        // The cap: dropped once the room is seen full or its capacity changes; raised when the room
        // holds more than it.
        self.cap = match self.cap {
            Some((_, at)) if at != o.capacity => None,
            _ if o.capacity > 0 && o.energy >= o.capacity => None,
            Some((c, at)) => Some((c.max(o.energy), at)),
            None => None,
        };
        // A request waiting for the full capacity (while no cap is set: with one, the request
        // waits for the cap's body, which the room is known to reach).
        if !o.requesting || self.cap.is_some() {
            self.waiting_since = None;
        } else if self.waiting_since.is_none() || o.alive > self.alive_then {
            self.waiting_since = Some(now);
            self.peak = o.energy;
        }
        self.alive_then = o.alive;
        self.peak = self.peak.max(o.energy);
        if let Some(since) = self.waiting_since {
            let waited = now.wrapping_sub(since);
            if waited >= cfg.wait_ticks {
                self.waiting_since = None;
                self.cap = Some((self.peak, o.capacity));
                events.push(Event::WaitFallback {
                    ticks: waited,
                    energy: self.peak,
                });
            }
        }
        // Stalls: anything workers move resets the clock.
        let mark = (o.level, o.controller.progress, o.sources, o.building);
        if self.mark != Some(mark) || o.working == 0 {
            if self.mark.is_some()
                && let Some(at) = self.stalled_at.take()
            {
                events.push(Event::StallEnded {
                    ticks: now.wrapping_sub(at),
                });
            }
            self.mark = Some(mark);
            self.moved_at = now;
            return false;
        }
        let still = now.wrapping_sub(self.moved_at);
        if self.stalled_at.is_none() && still >= cfg.stall_ticks {
            self.stalled_at = Some(now);
            events.push(Event::Stalled { ticks: still });
            return true;
        }
        false
    }

    /// A recovery is in progress: spawn at the energy the room holds.
    pub fn recovering(&self) -> bool {
        self.recovering_since.is_some()
    }

    /// The controller is below the downgrade guard.
    pub fn guard(&self) -> bool {
        self.guard
    }

    /// After a wait fallback, until the room is seen full: the most energy its bodies may cost.
    pub fn cap(&self) -> Option<u32> {
        self.cap.map(|(c, _)| c)
    }

    /// A stall is reported and not over yet.
    pub fn stalled(&self) -> bool {
        self.stalled_at.is_some()
    }

    /// When the recovery in progress started.
    pub fn recovering_since(&self) -> Option<u32> {
        self.recovering_since
    }
}
