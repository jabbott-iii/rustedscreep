//! The controller signer's glue (ADR-030, over `rustedscreep_core::signing::Signing`): its two
//! scheduler jobs, the terrain its body sizing asks for, its record's load and save, its log lines,
//! and the `rustedscreep.sign()` console summary.
//!
//! - `sign` (Critical, as every job that requests creeps: a request must come before the spawn job,
//!   P2.3): the planned signer's request, made only on a tick that runs the Optional tier (the
//!   user's choice, ADR-030: the sign never takes CPU or spawn time from the economy).
//! - `sign work` (Optional), after the remote placement: the terrain the plan needs, then the
//!   extra signers' ends, the live signer's sign, trip and move, or the next signer's plan (one
//!   route search a tick, a heavy unit).
//!
//! JS calls of its own: for a towered target's plan, one room's terrain a tick while the plan sizes
//! the body (`Game.map.getRoomTerrain`, `getRawBuffer`, `length`, and js-sys `copy_to`'s `length`
//! check and copy: 5, V156), unless movement already holds the room's terrain. Its route searches
//! run through movement's installed PathFinder search; its reads come from the snapshot
//! (`push_sign`), and its sign and suicide go through the intent buffer.

use std::fmt::Write as _;

use js_sys::{JsString, Uint8Array};
use rustedscreep_consts::{ROOM_AREA, RoomName};
use rustedscreep_core::config::{MovementConfig, SignConfig};
use rustedscreep_core::intents::IntentBuffer;
use rustedscreep_core::movement::{Movement, TerrainGrid};
use rustedscreep_core::persist::frame::{DecodeError, Frame};
use rustedscreep_core::persist::{LoadError, kind};
use rustedscreep_core::scheduler::{Clock, Job, JobState, TickCpu, Tier};
use rustedscreep_core::signing::{self, Plan, RecordLoad, SignEvent, SignState, Signing};
use rustedscreep_core::snapshot::Snapshot;
use rustedscreep_core::spawning::Spawner;
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

#[wasm_bindgen]
extern "C" {
    /// A `Room.Terrain` from `Game.map.getRoomTerrain`, which needs no vision (V156).
    type MapTerrain;

    /// `Game.map.getRoomTerrain(roomName)`, bound with `catch`.
    #[wasm_bindgen(js_namespace = ["Game", "map"], js_name = getRoomTerrain, catch)]
    fn get_room_terrain(name: &JsString) -> Result<MapTerrain, JsValue>;

    /// `getRawBuffer()`: a new Uint8Array copy, row-major (V156).
    #[wasm_bindgen(method, catch, js_name = getRawBuffer)]
    fn raw_buffer(this: &MapTerrain) -> Result<Uint8Array, JsValue>;
}

/// The request runs with the other spawn requests, Critical; it waits for the Optional tier.
const PLAN_JOB: Job = Job {
    name: "sign",
    tier: Tier::Critical,
};

/// The signer's work and the next signer's plan: cosmetic, Optional.
const WORK_JOB: Job = Job {
    name: "sign work",
    tier: Tier::Optional,
};

/// The signer's glue state for one global.
#[derive(Default)]
pub struct SignGlue {
    core: Signing,
    plan_job: JobState,
    work_job: JobState,
    /// Terrain reads for the plan since the global started, and the ones that failed (fed as
    /// swamp, the slowest terrain).
    terrain_reads: u32,
    terrain_failures: u32,
}

impl SignGlue {
    /// Starts the tick: the homes, the signers, and what this tick's reads show (no JS call).
    pub fn begin(&mut self, snapshot: &Snapshot, cfg: &SignConfig) {
        self.core.begin(snapshot, cfg);
    }

    /// Whether the record should be loaded now (a home has reached `sign.min_level`).
    pub fn wants_record(&self, cfg: &SignConfig) -> bool {
        self.core.wants_record(cfg)
    }

    /// No segment keeps the record: start at once, and never write.
    pub fn no_record(&mut self) {
        self.core.no_record();
    }

    /// The Critical `sign` job: the planned signer's request (not run without one to make).
    pub fn plan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        spawner: &mut Spawner,
        cfg: &SignConfig,
    ) {
        let optional = tick.allows(Tier::Optional);
        let Self { core, plan_job, .. } = self;
        // With no request to make the job is not run at all, so an idle signer leaves no trace in the
        // scheduler (no clock read, no skip counted).
        if !(optional && core.wants_request(cfg)) {
            return;
        }
        tick.run_units(&PLAN_JOB, plan_job, 1, |_, _| {
            core.request(spawner, cfg, optional);
            Ok(())
        });
    }

    /// The Optional `sign work` job: one unit, and not run at all with nothing to do.
    #[allow(clippy::too_many_arguments)]
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &SignConfig,
        mcfg: &MovementConfig,
    ) {
        let Self {
            core,
            work_job,
            terrain_reads,
            terrain_failures,
            ..
        } = self;
        if !core.has_work(cfg, movement.has_router()) {
            return;
        }
        tick.run_units(&WORK_JOB, work_job, 1, |_, commit| {
            if let Some(room) = core.terrain_wanted() {
                commit.heavy();
                if let Some(t) = movement.room_terrain(room) {
                    core.feed_terrain(room, t, cfg);
                } else {
                    *terrain_reads = terrain_reads.saturating_add(1);
                    let grid = read_terrain(room).or_else(|| {
                        *terrain_failures = terrain_failures.saturating_add(1);
                        swamp()
                    });
                    if let Some(grid) = grid {
                        core.feed_terrain(room, &grid, cfg);
                    }
                }
            }
            core.work(snapshot, movement, buffer, cfg, mcfg, commit)
        });
    }

    /// Whether the record should be written: it is then counted as saved, and the caller marks its
    /// segment dirty.
    pub fn take_save(&mut self, cfg: &SignConfig) -> bool {
        self.core.take_save(cfg)
    }

    /// The record's payload, for the persistence writes.
    pub fn payload(&self, out: &mut Vec<u8>) {
        self.core.payload(out);
    }

    /// The record's load finished. Returns whether it was accepted (an empty segment is).
    pub fn loaded(&mut self, segment: u8, frame: Result<Frame<'_>, LoadError>) -> bool {
        let frame = frame.and_then(|f| {
            f.header
                .expect(kind::SIGN, signing::SCHEMA)
                .map_err(LoadError::Decode)
                .map(|()| f)
        });
        let load = match &frame {
            Ok(f) => RecordLoad::Payload(f.payload),
            Err(LoadError::Decode(DecodeError::Empty)) => RecordLoad::Empty,
            Err(LoadError::Unavailable { .. }) => RecordLoad::Unavailable,
            Err(_) => RecordLoad::Refused,
        };
        let result = self.core.loaded(load);
        match (&frame, result) {
            (Ok(f), Ok(())) => {
                log::info!(
                    "rustedscreep sign: record loaded from segment {segment} ({} rooms, written at {})",
                    self.core.book().len(),
                    f.header.written
                );
                true
            }
            (Ok(_), Err(why)) => {
                log::warn!(
                    "rustedscreep sign: the record in segment {segment} was not used ({why:?}); \
                     the tour starts again and the record is written again"
                );
                false
            }
            (Err(LoadError::Decode(DecodeError::Empty)), _) => true,
            (Err(e @ LoadError::Unavailable { .. }), _) => {
                log::warn!(
                    "rustedscreep sign: the record in segment {segment} was not read ({e}); the \
                     tour starts again and the segment is left for the next global"
                );
                false
            }
            (Err(e), _) => {
                log::warn!(
                    "rustedscreep sign: the record in segment {segment} was not used ({e}); the \
                     tour starts again and the record is written again"
                );
                false
            }
        }
    }

    /// The tick's events, as log lines.
    pub fn log_events(&self, cfg: &SignConfig) {
        for &e in self.core.events() {
            match e {
                SignEvent::Signed { room } => log::info!("rustedscreep sign: signed {room}"),
                // One room a tick while a walled-off area (a novice or respawn area, V168) is
                // looked through: debug for unreachable rooms, info for the rest.
                SignEvent::GaveUp { room, state } => {
                    let level = if state == SignState::Unreachable {
                        log::Level::Debug
                    } else {
                        log::Level::Info
                    };
                    log::log!(
                        level,
                        "rustedscreep sign: {room} is {} until {}",
                        state.as_str(),
                        self.core.book().due_at(room, cfg)
                    );
                }
                SignEvent::Died { room, deaths } => log::warn!(
                    "rustedscreep sign: the signer was lost in {room} ({deaths} lost there)"
                ),
                SignEvent::Retreated { room, towers } => log::info!(
                    "rustedscreep sign: {towers} tower(s) in {room}; the signer stepped back out"
                ),
                SignEvent::Planned {
                    room,
                    tough,
                    moves,
                    cost,
                } => log::info!(
                    "rustedscreep sign: next signer for {room}: {tough} TOUGH, {moves} MOVE \
                     ({cost} energy)"
                ),
                SignEvent::Done { room } => log::info!(
                    "rustedscreep sign: nothing left in reach from {room}; the signer ends"
                ),
            }
        }
    }

    /// One line for `rustedscreep.sign()`: the record's rooms by state, the signer and its trip,
    /// the plan, and the counts since the global started.
    pub fn summary(&self, snapshot: &Snapshot, cfg: &SignConfig) -> String {
        let core = &self.core;
        let now = snapshot.tick.time;
        let mut text = format!(
            "rustedscreep sign at tick {now}: {} | record {} rooms (",
            if cfg.enabled { "on" } else { "off" },
            core.book().len()
        );
        for (i, state) in SignState::ALL.into_iter().enumerate() {
            let n = core
                .book()
                .entries()
                .iter()
                .filter(|e| e.state == state)
                .count();
            let _ = write!(
                text,
                "{}{} {n}",
                if i == 0 { "" } else { ", " },
                state.as_str()
            );
        }
        let due = core
            .book()
            .entries()
            .iter()
            .filter(|e| e.due_at(cfg) <= now)
            .count();
        let _ = write!(
            text,
            "; {due} due), {}",
            if core.is_loaded() {
                "loaded"
            } else {
                "loading"
            }
        );
        match core.signer() {
            None => text.push_str(" | no signer"),
            Some(s) => {
                let _ = write!(
                    text,
                    " | signer in {} ({}, {}){}",
                    s.at.room_name(),
                    s.at.x().u8(),
                    s.at.y().u8(),
                    if s.spawning { " spawning" } else { "" }
                );
                if let Some(ttl) = s.ttl {
                    let _ = write!(text, ", ttl {ttl}");
                }
                match s.trip {
                    Some(t) => {
                        let _ = write!(
                            text,
                            ", to {} ({})",
                            t.room,
                            if t.look { "look" } else { "sign" }
                        );
                    }
                    None => text.push_str(", no trip"),
                }
            }
        }
        match core.plan() {
            Plan::None => text.push_str(" | plan none"),
            Plan::Sizing {
                trip, path, cursor, ..
            } => {
                let _ = write!(
                    text,
                    " | plan sizing {} ({cursor} of {} tiles)",
                    trip.room,
                    path.len()
                );
            }
            Plan::Ready {
                home, body, trip, ..
            } => {
                let _ = write!(
                    text,
                    " | plan ready from {home} for {} ({} parts, {} energy)",
                    trip.room,
                    body.len(),
                    body.cost()
                );
            }
        }
        let st = core.stats();
        let _ = write!(
            text,
            " | since the global: signed {}, searches {}, lost {}, retreats {}, planned {}, \
             terrain reads {} (failed {})",
            st.signed,
            st.searches,
            st.deaths,
            st.retreats,
            st.planned,
            self.terrain_reads,
            self.terrain_failures
        );
        text
    }
}

/// `room`'s terrain through `Game.map.getRoomTerrain` (no vision needed), or `None` when a call
/// throws or the buffer is not `ROOM_AREA` long (checked before the copy, whose length assertion
/// would panic).
fn read_terrain(room: RoomName) -> Option<TerrainGrid> {
    let terrain = get_room_terrain(&JsString::from(room)).ok()?;
    let raw = terrain.raw_buffer().ok()?;
    if usize::try_from(raw.length()).ok() != Some(ROOM_AREA) {
        return None;
    }
    let mut buf = vec![0u8; ROOM_AREA];
    raw.copy_to(&mut buf[..]);
    TerrainGrid::from_row_major(&buf)
}

/// A room of swamp: the slowest terrain, for a read that failed.
fn swamp() -> Option<TerrainGrid> {
    let fill = screeps::Terrain::Swamp as u8;
    TerrainGrid::from_row_major(&vec![fill; ROOM_AREA])
}
