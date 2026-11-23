//! Defense's glue (ROADMAP P7, over `rustedscreep_core::defense::Threats`): the tick's threat
//! assessment and its log lines.
//!
//! - The assessment (P7.1) runs every tick a hostile creep stands in an owned room or a threat is
//!   open, right after the snapshot, as the other `begin` steps do: it is Critical work (defense)
//!   that needs no CPU gate, since it is bounded by the few hostiles in the bot's rooms (at most
//!   `MAX_CREEP_SIZE` parts each, plus a short search for a boosted creep's effective hits). The
//!   jobs that answer a threat (towers, defenders, safe mode; P7.2 on) are scheduler jobs. On a
//!   tick with neither, nothing runs, and no clock sample is taken.
//! - The log lines report each threat's start and changes (warn) and its end (info), the live
//!   record the P7 gate asks for.
//!
//! JS calls of its own: on a tick a threat starts or changes, each of the room's dangerous
//! hostiles' `owner` and its `username` (4 JS calls a creep in the generated glue: the two getters,
//! the object check, and the string copy), for the line alone and only while warn is on (V268, as
//! the remote threat line does). Everything else comes from the snapshot.

use rustedscreep_core::config::DefenseConfig;
use rustedscreep_core::defense::{RoomThreat, ThreatEvent, Threats};
use rustedscreep_core::snapshot::Snapshot;
use wasm_bindgen::JsCast;

use crate::remote::{NameOf, OwnerOf};
use crate::snapshot::JsWorld;

/// Defense's glue state for one global.
#[derive(Default)]
pub struct DefenseGlue {
    core: Threats,
}

impl DefenseGlue {
    /// Assesses this tick's hostile creeps in owned rooms and updates the rooms' threats (module
    /// docs). Call after the snapshot, before the scheduler's jobs.
    pub fn begin(&mut self, snapshot: &Snapshot, cfg: &DefenseConfig) {
        if self.core.needed(snapshot) {
            self.core.assess(snapshot, cfg);
        } else {
            self.core.clear();
        }
    }

    /// Logs this tick's threat events.
    pub fn log_events(&self, snapshot: &Snapshot, js: &JsWorld) {
        for event in self.core.events() {
            match *event {
                ThreatEvent::Started { room } => {
                    if !log::log_enabled!(log::Level::Warn) {
                        continue;
                    }
                    let Some(r) = self.core.room(room) else {
                        continue;
                    };
                    log::warn!(
                        "rustedscreep defense: {room} threatened by {}",
                        self.describe(r, snapshot, js)
                    );
                }
                ThreatEvent::Changed { room, since } => {
                    if !log::log_enabled!(log::Level::Warn) {
                        continue;
                    }
                    let Some(r) = self.core.room(room) else {
                        continue;
                    };
                    log::warn!(
                        "rustedscreep defense: {room} threat (since tick {since}) now {}",
                        self.describe(r, snapshot, js)
                    );
                }
                ThreatEvent::Ended { room, since, last } => {
                    log::info!(
                        "rustedscreep defense: {room} threat over: no dangerous hostile seen since \
                         tick {last} (it started at tick {since})"
                    );
                }
            }
        }
    }

    /// A room's threat for a log line: its dangerous hostiles and their owners, then the room's sums.
    /// Reads each dangerous hostile's `owner.username` (module docs).
    fn describe(&self, r: &RoomThreat, snapshot: &Snapshot, js: &JsWorld) -> String {
        let mut owners: Vec<String> = Vec::new();
        for h in self.core.hostiles(r) {
            if !h.power.dangerous() {
                continue;
            }
            let row = usize::try_from(h.row).unwrap_or(usize::MAX);
            // The rows are this tick's, so the JS object is the creep the row describes.
            if snapshot.creeps.handles().get(row).is_none() {
                continue;
            }
            let owner = js.creeps.get(row).and_then(|creep| {
                let o = creep.unchecked_ref::<OwnerOf>().owner_value();
                o.is_object()
                    .then(|| o.unchecked_ref::<NameOf>().username_value().as_string())
                    .flatten()
            });
            let name = owner.unwrap_or_else(|| String::from("unknown"));
            if !owners.contains(&name) {
                owners.push(name);
            }
        }
        let p = r.power;
        let unread = if r.unread > 0 {
            format!("; {} bodies unread", r.unread)
        } else {
            String::new()
        };
        format!(
            "{} dangerous hostile creep(s) of {} ({} in the room, {} boosted): melee {}, ranged {}, \
             heal {}, dismantle {} a tick, {} CLAIM; hits {}, effective {}{unread}",
            r.dangerous,
            owners.join(", "),
            r.hostiles,
            r.boosted,
            p.melee_damage(),
            p.ranged_damage(),
            p.heal_adjacent(),
            p.dismantle_damage(),
            p.claim,
            r.hits,
            r.effective_hits,
        )
    }
}
