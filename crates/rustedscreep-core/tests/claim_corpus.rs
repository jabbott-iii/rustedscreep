//! Claim scoring (P8.2, ADR-041) on the terrain corpus: the shard3 rooms of the corpus, all taken
//! as neutral, scored from E7N57 (the live home) and planned to the end with their real terrain.
//! The ranking and the steps print with `cargo test --test claim_corpus -- --nocapture`.

mod common;

use common::corpus::{self, CorpusRoom};

use rustedscreep_consts::{ROOM_SIZE, RoomName};
use rustedscreep_core::config::{CLAIM_MMO, REMOTE_MMO};
use rustedscreep_core::economy::is_remote_room;
use rustedscreep_core::expansion::claim::source_yield;
use rustedscreep_core::expansion::{Atlas, Candidate, ClaimHome, Claims, MAX_SOURCES, RoomIntel};
use rustedscreep_core::movement::{TerrainKind, Tile};
use rustedscreep_core::planner::flood::Params;
use rustedscreep_core::scheduler::Step;
use rustedscreep_core::snapshot::{Controller, RoomStatus};
use rustedscreep_core::spawning::room_distance;

const NOW: u32 = 84_500_000;

fn home() -> RoomName {
    "E7N57".parse().expect("room")
}

/// The corpus's rooms but the home, each as the atlas would hold a neutral one.
fn atlas(rooms: &[CorpusRoom]) -> Atlas {
    let mut a = Atlas::default();
    for r in rooms.iter().filter(|r| r.name != home()) {
        let mut e = RoomIntel::new(r.name, NOW);
        e.known = true;
        e.controller = Controller::Neutral;
        e.controller_tile = Some(r.controller);
        for (slot, &s) in e.sources.iter_mut().zip(r.sources.iter().take(MAX_SOURCES)) {
            *slot = Some(s);
        }
        e.mineral = r
            .minerals
            .first()
            .map(|&(_, t)| rustedscreep_core::expansion::Deposit {
                mineral: rustedscreep_core::expansion::Mineral::Other,
                tile: t,
                density: 1,
            });
        a.put(e);
    }
    a
}

struct Run {
    claims: Claims,
    plans: Vec<RoomName>,
    steps: u32,
}

/// Scores and plans to the end, as the glue does a step a call.
fn run(rooms: &[CorpusRoom], a: &Atlas) -> Run {
    let homes = [ClaimHome {
        room: home(),
        status: RoomStatus::Normal,
    }];
    let owned = [home()];
    let cfg = CLAIM_MMO;
    let mut claims = Claims::default();
    claims.score(a, 0, &homes, &owned, NOW, REMOTE_MMO.max_rooms, &cfg);
    let mut plans = Vec::new();
    let mut steps = 0u32;
    loop {
        if claims.planning().is_none() {
            let Some(room) = claims.wanted(&cfg) else {
                break;
            };
            let terrain = rooms
                .iter()
                .find(|r| r.name == room)
                .map(CorpusRoom::terrain);
            claims.feed_terrain(room, terrain, a);
            plans.push(room);
        }
        while claims.step(Params::default(), a, &owned, &cfg) == Step::More {
            steps += 1;
            assert!(steps < 10_000, "the plans never end");
        }
        steps += 1;
    }
    Run {
        claims,
        plans,
        steps,
    }
}

fn line(c: &Candidate) -> String {
    let e = |v: i64| format!("{:.3}", v as f64 / 1000.0);
    format!(
        "{} from {} {} rooms, {} sources: {} e/t (sources {}, remotes {}, distance {}, foreign {}, \
         walks {}, ramparts {}){}",
        c.room,
        c.home,
        c.distance,
        c.sources,
        e(c.value()),
        e(c.terms.sources),
        e(c.terms.remotes),
        e(c.terms.distance),
        e(c.terms.foreign),
        e(c.terms.walks),
        e(c.terms.ramparts),
        c.plan
            .map(|f| format!(
                ", spawn ({}, {}), {} ramparts",
                f.spawn.x(),
                f.spawn.y(),
                f.ramparts
            ))
            .unwrap_or_default()
    )
}

#[test]
fn the_top_rooms_are_planned_and_no_unplanned_room_beats_them() {
    let rooms = corpus::load();
    let a = atlas(&rooms);
    let got = run(&rooms, &a);
    let cfg = CLAIM_MMO;
    let list = got.claims.candidates();
    for c in list.iter().take(8) {
        println!("{}", line(c));
    }
    println!(
        "{} candidates, {} plans ({:?}), {} steps",
        list.len(),
        got.plans.len(),
        got.plans,
        got.steps
    );
    // Every room of the corpus within reach that can have a controller is a candidate: the corpus
    // rooms all plan validly with no spawn (`planner_flood`'s fresh mode).
    let reach = rooms
        .iter()
        .filter(|r| {
            r.name != home()
                && is_remote_room(r.name)
                && room_distance(r.name, home()) <= cfg.max_distance
                && !r.sources.is_empty()
        })
        .count();
    assert_eq!(list.len(), reach);
    let top = usize::try_from(cfg.plan_count).expect("count");
    assert!(list.len() >= top, "the corpus has enough rooms");
    // The top `plan_count` are planned, the first is the best, and no unplanned room's upper bound
    // is above the last of them.
    assert!(list.iter().take(top).all(|c| c.plan.is_some()));
    assert_eq!(
        got.claims.best().map(|c| c.room),
        list.first().map(|c| c.room)
    );
    let floor = list.get(top - 1).map(Candidate::value).expect("top");
    for c in list.iter().filter(|c| c.plan.is_none()) {
        assert!(c.value() <= floor, "{} above the top", line(c));
    }
    // A plan was made only for rooms that reached the top while unplanned: at most one a room.
    let mut once = got.plans.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!(once.len(), got.plans.len());
    // Each plan's first spawn stands on walkable ground, off the edge.
    let last = ROOM_SIZE - 1;
    for c in list.iter().filter(|c| c.plan.is_some()) {
        let f = c.plan.expect("plan");
        let r = rooms
            .iter()
            .find(|r| r.name == c.room)
            .expect("corpus room");
        let t: Tile = f.spawn;
        assert_ne!(r.terrain().kind(t), TerrainKind::Wall, "{}", line(c));
        assert!(t.x() > 0 && t.y() > 0 && t.x() < last && t.y() < last);
        // The plan's terms only lower the value from its sources and remotes.
        assert!(c.terms.walks <= 0 && c.terms.ramparts <= 0);
        assert!(c.value() <= c.terms.sources + c.terms.remotes);
    }
    // Two-source rooms lead a ranking of rooms that are otherwise alike in reach.
    assert!(
        list.first()
            .is_some_and(|c| c.terms.sources >= source_yield())
    );
}

#[test]
fn the_ranking_and_its_plans_are_deterministic() {
    let rooms = corpus::load();
    let a = atlas(&rooms);
    let one = run(&rooms, &a);
    let two = run(&rooms, &a);
    assert_eq!(one.claims.candidates(), two.claims.candidates());
    assert_eq!(one.plans, two.plans);
    assert_eq!(one.steps, two.steps);
}
