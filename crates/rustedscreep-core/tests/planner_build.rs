//! The plan's placement (P4.3, `planner::build`) over the corpus: a plan built again over its own
//! containers and links (what a global reset finds) is the same plan; every plan builds up level
//! by level from its first spawn with no destroy, never past a level's count, and with every item
//! due built and reached at each level; the live room's standing extensions (E7N57, ADR-020)
//! against its plan under ADR-026's rules; the job's anchor choice after a global reset once later
//! spawns stand (`planner::job::plan_whole`); every road's needs by gate (`layout::RoadNeed`,
//! `Gates::due_level`); and the MMO's gates placing only what the bot uses.

mod common;

use common::harness;
use rustedscreep_consts::StructureType;
use rustedscreep_core::config::{MOVEMENT_MMO, PLANNER_MMO};
use rustedscreep_core::economy::placement::next_extension;
use rustedscreep_core::movement::{self, CostGrid, Dial, FlowField, Footprint, TerrainGrid, Tile};
use rustedscreep_core::planner::build::{self, Ask, Census, Gates, Ground, Next};
use rustedscreep_core::planner::flood::Params;
use rustedscreep_core::planner::job::{self, Facts};
use rustedscreep_core::planner::layout::{GATES, Gate, kind_rank};
use rustedscreep_core::planner::rules::{allowed, earliest_level};
use rustedscreep_core::planner::{
    Item, Layout, MAX_LEVEL, PlanInput, Scratch, bunker, flood, validate,
};
use rustedscreep_core::scheduler::Step;

fn tile(x: u8, y: u8) -> Tile {
    Tile::new(x, y).expect("in room")
}

const ALL: Gates = Gates {
    storage: true,
    towers: true,
    ramparts: true,
    late: true,
};

/// The ground from a list of what stands (the plan's first spawn among it).
fn ground_of(standing: &[(StructureType, Tile)], sites: &[(StructureType, Tile)]) -> Ground {
    let mut g = Ground::default();
    for &(k, t) in standing {
        g.structure(k, t);
    }
    for &(k, t) in sites {
        g.site(k, t);
    }
    g
}

fn ask<'a>(layout: &'a Layout, input: &'a PlanInput<'a>, anchor: Tile, level: u8) -> Ask<'a> {
    Ask {
        layout,
        terrain: input.terrain,
        anchor,
        level,
        gates: ALL,
        replace: true,
        max_sites: PLANNER_MMO.max_sites,
        holding: false,
        skip: &[],
        full: 0,
    }
}

/// The economy's containers and links (placed by `economy::mining`'s needs, not by
/// `build::next`): their tiles from the plan's roles, standing from their plan level.
fn economy_items(layout: &Layout, level: u8) -> Vec<(StructureType, Tile)> {
    let r = &layout.roles;
    let roles: Vec<Tile> = r
        .mining
        .iter()
        .copied()
        .chain(r.controller_container)
        .chain(r.controller_link)
        .chain(r.source_links.iter().flatten().copied())
        .collect();
    layout
        .items
        .iter()
        .filter(|i| i.level <= level && roles.contains(&i.tile))
        .filter(|i| matches!(i.kind, StructureType::Container | StructureType::Link))
        .map(|i| (i.kind, i.tile))
        .collect()
}

#[test]
fn adopting_its_own_containers_and_links_gives_the_same_plan() {
    harness::each(|room, mode, input| {
        let Ok(first) = flood::plan(input, &mut Scratch::default()) else {
            return;
        };
        let standing: Vec<(StructureType, Tile)> = first
            .items
            .iter()
            .filter(|i| matches!(i.kind, StructureType::Container | StructureType::Link))
            .map(|i| (i.kind, i.tile))
            .collect();
        let mut run = flood::Run::new(input);
        run.adopt(&standing);
        let mut s = Scratch::default();
        while run.step(input, &mut s) == Ok(Step::More) {}
        assert_eq!(first, run.finish(), "{} {mode:?}", room.name);
    });
}

/// A standing container on a neighbour two sources share (an interim ADR-023 container) is
/// adopted as the earlier source's mining tile, the source `economy::mining` reads it as (sources
/// in tile order, each the neighbour with a container, V182's reach), so the miner already on it
/// gets the plan's source link beside it. E1N55 (the P4.3 review's case: sources at (10, 37) and
/// (11, 37)) with containers on (11, 38) and (12, 37): in every mode the plan's mining tiles are
/// those two, each source's link is next to its mining tile, and the plan is valid.
///
/// Uses: `PlanInput::sources` order (the tile order the economy uses).
#[test]
fn a_shared_standing_container_is_the_earlier_sources_mining_tile() {
    let next_to = |a: Tile, b: Tile| a.neighbors().any(|(_, n)| n == b);
    let (shared, own) = (tile(11, 38), tile(12, 37));
    let standing = [
        (StructureType::Container, shared),
        (StructureType::Container, own),
    ];
    let mut modes = 0;
    harness::each(|room, mode, input| {
        if room.name != "E1N55" {
            return;
        }
        let sources = input.sources;
        assert_eq!(sources, [tile(10, 37), tile(11, 37)], "the review's room");
        assert!(
            sources.iter().all(|&s| next_to(shared, s)),
            "a shared neighbour"
        );
        assert!(next_to(own, sources[1]) && !next_to(own, sources[0]));
        let mut run = flood::Run::new(input);
        run.adopt(&standing);
        let mut s = Scratch::default();
        while run.step(input, &mut s) == Ok(Step::More) {}
        let layout = run.finish();
        assert_eq!(layout.roles.mining, [shared, own], "{mode:?}");
        for (i, link) in layout.roles.source_links.iter().enumerate() {
            if let (Some(l), Some(&m)) = (link, layout.roles.mining.get(i)) {
                assert!(next_to(*l, m), "{mode:?}: source {i}'s link by its miner");
            }
        }
        assert!(validate(input, &layout).valid(), "{mode:?}");
        modes += 1;
    });
    assert_eq!(modes, harness::MODES.len());
}

/// Uses: `CONTROLLER_STRUCTURES` (through `allowed`) for each level's count.
#[test]
fn every_plan_builds_up_level_by_level() {
    let mut rooms = 0;
    harness::each(|room, mode, input| {
        let Ok(layout) = flood::plan(input, &mut Scratch::default()) else {
            return;
        };
        let anchor = input.spawn.or_else(|| {
            layout
                .of(StructureType::Spawn)
                .find(|i| i.level == 1)
                .map(|i| i.tile)
        });
        let Some(anchor) = anchor else {
            panic!("{} {mode:?}: a first spawn", room.name);
        };
        rooms += 1;
        let mut standing = vec![(StructureType::Spawn, anchor)];
        let mut reach = build::Scratch::default();
        for level in 1..=MAX_LEVEL {
            for e in economy_items(&layout, level) {
                if !standing.contains(&e) {
                    standing.push(e);
                }
            }
            let mut census = Census::default();
            for _ in 0..=layout.items.len() {
                let g = ground_of(&standing, &[]);
                let (next, c) = build::next(&ask(&layout, input, anchor, level), &g, &mut reach);
                census = c;
                match next {
                    Next::Site { kind, tile } => {
                        assert!(!standing.contains(&(kind, tile)));
                        standing.push((kind, tile));
                    }
                    Next::Destroy { kind, tile } => {
                        panic!("{} {mode:?} L{level}: destroy {kind:?} {tile:?}", room.name)
                    }
                    Next::Idle => break,
                }
            }
            let what = format!("{} {mode:?} L{level}: {census:?}", room.name);
            assert_eq!(census.built, census.due, "{what}");
            assert_eq!(
                (census.capped, census.blocked, census.cut_off),
                (0, 0, 0),
                "{what}"
            );
            for &kind in rustedscreep_core::planner::layout::KINDS.iter() {
                let n = standing.iter().filter(|(k, _)| *k == kind).count() as u32;
                assert!(n <= allowed(kind, level), "{what}: {kind:?} {n}");
            }
        }
        assert_eq!(standing.len(), layout.items.len(), "{} {mode:?}", room.name);
    });
    assert!(rooms > 0);
}

/// E7N57 (RCL4, a spawn at (43, 21) and the 20 extensions ADR-020 gives it, replayed with
/// `next_extension`; the first five equal the live sites, `planner_cases.rs`).
fn e7n57() -> (common::corpus::CorpusRoom, Tile, Vec<Tile>) {
    let room = common::corpus::load()
        .into_iter()
        .find(|r| r.name == "E7N57")
        .expect("E7N57 in the corpus");
    let terrain = room.terrain();
    let spawn = tile(43, 21);
    let mineral = room.minerals.first().map(|&(_, t)| t);
    let statics: Vec<Tile> = room.sources.iter().copied().chain(mineral).collect();
    let profile = movement::profile(&MOVEMENT_MMO);
    let mut occupied: Vec<(Tile, bool)> = vec![(spawn, true), (room.controller, true)];
    occupied.extend(statics.iter().map(|&t| (t, true)));
    let mut grid = CostGrid::default();
    grid.build(
        &terrain,
        occupied.iter().map(|&(t, _)| (t, Footprint::Blocked)),
        profile,
    );
    let (mut field, mut dial) = (FlowField::default(), Dial::default());
    let want = allowed(StructureType::Extension, 4) as usize;
    let mut live = Vec::new();
    while live.len() < want {
        let Some(t) = next_extension(
            &grid,
            profile,
            spawn,
            Some(room.controller),
            &statics,
            &occupied,
            &[],
            &mut field,
            &mut dial,
        ) else {
            break;
        };
        occupied.push((t, true));
        live.push(t);
    }
    assert_eq!(live.len(), want);
    (room, spawn, live)
}

/// Runs `build::next` until it idles, building each site at once and applying each destroy, with
/// a destroy's replacement held open until its tile is sited (ADR-026), when the bot places it
/// now: as `economy::placement::due_replacement` reads it (the tile's structure, else its road,
/// whose gate is on and that is due). Returns the actions.
fn play(
    layout: &Layout,
    a: &Ask<'_>,
    standing: &mut Vec<(StructureType, Tile)>,
) -> (Vec<Next>, Census) {
    let mut out = Vec::new();
    let mut holding: Option<Tile> = None;
    let mut reach = build::Scratch::default();
    let mut census = Census::default();
    for _ in 0..=layout.items.len() * 2 {
        let g = ground_of(standing, &[]);
        let ask = Ask {
            holding: holding.is_some(),
            ..*a
        };
        let (next, c) = build::next(&ask, &g, &mut reach);
        census = c;
        match next {
            Next::Site { kind, tile } => {
                standing.push((kind, tile));
                if holding == Some(tile) {
                    holding = None;
                }
            }
            Next::Destroy { kind, tile } => {
                standing.retain(|&s| s != (kind, tile));
                // No economy need asks for a container or link here: only what `next` places.
                let placed = |i: &&rustedscreep_core::planner::Item| {
                    a.gates.places(i.kind, i.tile, &layout.roles)
                        && a.gates.due_level(layout, i).is_some_and(|l| l <= a.level)
                };
                let at = || {
                    layout
                        .items
                        .iter()
                        .filter(|i| i.tile == tile)
                        .filter(placed)
                };
                holding = at()
                    .find(|i| !matches!(i.kind, StructureType::Road | StructureType::Rampart))
                    .or_else(|| at().find(|i| i.kind == StructureType::Road))
                    .map(|_| tile);
            }
            Next::Idle => break,
        }
        out.push(next);
    }
    (out, census)
}

#[test]
fn the_live_room_keeps_its_extensions_until_a_due_structure_needs_one() {
    let (room, spawn, live) = e7n57();
    let terrain = room.terrain();
    let mineral = room.minerals.first().map(|&(_, t)| t);
    let input = PlanInput {
        terrain: &terrain,
        controller: room.controller,
        sources: &room.sources,
        mineral,
        spawn: Some(spawn),
    };
    let layout = flood::plan(&input, &mut Scratch::default()).expect("a plan");
    assert!(validate(&input, &layout).valid());
    let at = |t: Tile| {
        layout
            .items
            .iter()
            .find(|i| {
                i.tile == t && i.kind != StructureType::Road && i.kind != StructureType::Rampart
            })
            .map(|i| (i.kind, i.level))
    };
    let on_plan = live
        .iter()
        .filter(|&&t| at(t).is_some_and(|(k, _)| k == StructureType::Extension))
        .count() as u32;
    let start: Vec<(StructureType, Tile)> = std::iter::once((StructureType::Spawn, spawn))
        .chain(live.iter().map(|&t| (StructureType::Extension, t)))
        .chain(economy_items(&layout, 4))
        .collect();
    let level = 4;
    let extensions = allowed(StructureType::Extension, level);

    // The MMO's gates: the storage and the towers wait, so nothing is destroyed; the extensions
    // the plan has are kept, the planned slots beyond the level's count wait, the roads go out.
    let base = Ask {
        layout: &layout,
        terrain: &terrain,
        anchor: spawn,
        level,
        gates: Gates::from_config(&PLANNER_MMO),
        replace: PLANNER_MMO.replace,
        max_sites: PLANNER_MMO.max_sites,
        holding: false,
        skip: &[],
        full: 0,
    };
    let mut standing = start.clone();
    let (actions, census) = play(&layout, &base, &mut standing);
    assert!(actions.iter().all(|a| matches!(
        a,
        Next::Site {
            kind: StructureType::Road,
            ..
        }
    )));
    assert!(!actions.is_empty(), "the plan's roads are placed");
    assert_eq!(census.capped, extensions - on_plan, "{census:?}");
    assert_eq!((census.blocked, census.cut_off), (0, 0), "{census:?}");
    for &t in &live {
        assert!(standing.contains(&(StructureType::Extension, t)));
    }

    // With the storage's gate on, the extension on its tile goes first, then a planned extension
    // takes the freed count before the storage's own site; one destroy at a time.
    let storage = layout
        .of(StructureType::Storage)
        .next()
        .map(|i| i.tile)
        .expect("a storage");
    assert!(
        live.contains(&storage),
        "the storage stands on an ADR-020 extension"
    );
    let mut gates = base.gates;
    gates.storage = true;
    let mut standing = start.clone();
    let (actions, census) = play(&layout, &Ask { gates, ..base }, &mut standing);
    assert_eq!(
        actions.first(),
        Some(&Next::Destroy {
            kind: StructureType::Extension,
            tile: storage
        })
    );
    let destroys = actions
        .iter()
        .filter(|a| matches!(a, Next::Destroy { .. }))
        .count();
    assert_eq!(destroys, 1, "{actions:?}");
    assert!(standing.contains(&(StructureType::Storage, storage)));
    let count = |k: StructureType| standing.iter().filter(|(x, _)| *x == k).count() as u32;
    assert_eq!(count(StructureType::Extension), extensions);
    assert_eq!(census.cut_off, 0);

    // Every gate on at the top level: each off-plan extension under a planned structure goes, one
    // at a time; the two on ground the plan leaves open stay.
    let mut standing = start;
    let top = Ask {
        gates: ALL,
        level: MAX_LEVEL,
        ..base
    };
    let mut all = Vec::new();
    for e in economy_items(&layout, MAX_LEVEL) {
        if !standing.contains(&e) {
            standing.push(e);
        }
    }
    let (actions, census) = play(&layout, &top, &mut standing);
    all.extend(actions);
    let under = live
        .iter()
        .filter(|&&t| at(t).is_some_and(|(k, _)| k != StructureType::Extension))
        .count();
    let destroyed = all
        .iter()
        .filter(|a| matches!(a, Next::Destroy { .. }))
        .count();
    assert_eq!(destroyed, under, "{census:?}");
    assert_eq!(census.cut_off, 0, "{census:?}");
    let open = live.iter().filter(|&&t| at(t).is_none()).count() as u32;
    assert_eq!(census.capped, open, "{census:?}");
}

/// `v` in kind then tile order with no repeat, as `Facts::read` gives its lists.
fn by_kind(mut v: Vec<(StructureType, Tile)>) -> Vec<(StructureType, Tile)> {
    v.sort_unstable_by_key(|&(k, t)| (kind_rank(k), t.index()));
    v.dedup();
    v
}

/// The facts `Facts::read` would give for `input`'s room with `spawns` standing, own spawn sites
/// on `sites`, and `standing` and `own` as that function reads them.
fn facts_of(
    input: &PlanInput<'_>,
    mut spawns: Vec<Tile>,
    sites: &[Tile],
    standing: Vec<(StructureType, Tile)>,
    own: Vec<(StructureType, Tile)>,
) -> Facts {
    spawns.sort_unstable();
    spawns.dedup();
    let mut anchors: Vec<Tile> = spawns.iter().chain(sites).copied().collect();
    anchors.sort_unstable();
    anchors.dedup();
    Facts {
        terrain: input.terrain.clone(),
        controller: input.controller,
        sources: input.sources.to_vec(),
        mineral: input.mineral,
        spawns,
        anchors,
        standing: by_kind(standing),
        own: by_kind(own),
    }
}

/// Structures by kind and tile, as `Facts` lists them.
type Kinds = Vec<(StructureType, Tile)>;

/// What a room built from a plan shows a global reset at `level`.
#[derive(Clone, Copy, Debug)]
enum Seen {
    /// Every item due at the level but the roads, the ramparts and the walls stands (the roads
    /// and the walls are no own structures, so the snapshot holds neither); the economy's
    /// containers and links stand.
    Every,
    /// What `Game.structures` and the room's containers show with every gate on: every item but
    /// the roads and the walls, the ramparts among them; every container and link of the plan.
    EveryGate,
    /// What the MMO's gates built (`PLANNER_MMO`): the items `build::next` places (spawns and
    /// extensions) and the economy's containers and links.
    Mmo,
}

impl Seen {
    const ALL: [Self; 3] = [Self::Every, Self::EveryGate, Self::Mmo];

    /// The facts' `standing` and `own` (but the spawn sites) for `layout` at `level`.
    /// Uses: `PLANNER_MMO` (the gates of [`Seen::Mmo`]).
    fn of(self, layout: &Layout, level: u8) -> (Kinds, Kinds) {
        let up_to = layout.items.iter().filter(|i| i.level <= level);
        let not = |kinds: &'static [StructureType]| move |i: &&Item| !kinds.contains(&i.kind);
        let economy = economy_items(layout, level);
        match self {
            Self::Every => {
                let own = up_to
                    .filter(not(&[
                        StructureType::Road,
                        StructureType::Rampart,
                        StructureType::Wall,
                    ]))
                    .map(|i| (i.kind, i.tile))
                    .collect();
                (economy, own)
            }
            Self::EveryGate => {
                let standing = up_to
                    .clone()
                    .filter(|i| matches!(i.kind, StructureType::Container | StructureType::Link))
                    .map(|i| (i.kind, i.tile))
                    .collect();
                let own = up_to
                    .filter(not(&[StructureType::Road, StructureType::Wall]))
                    .map(|i| (i.kind, i.tile))
                    .collect();
                (standing, own)
            }
            Self::Mmo => {
                let gates = Gates::from_config(&PLANNER_MMO);
                let own = up_to
                    .filter(|i| {
                        i.kind != StructureType::Road && gates.places(i.kind, i.tile, &layout.roles)
                    })
                    .map(|i| (i.kind, i.tile))
                    .chain(economy.iter().copied())
                    .collect();
                (economy, own)
            }
        }
    }
}

/// The anchor rule the review found wanting (C5/C12), for the reset test's own check that the
/// corpus has rooms where the rules differ: the standing spawns alone in tile order, the first
/// whose valid plan's spawn tiles hold every standing spawn. Uses: `PLANNER_MMO`
/// (`Params::default`).
fn first_fit(facts: &Facts, s: &mut Scratch) -> Option<Layout> {
    for &sp in &facts.spawns {
        let input = facts.input(Some(sp));
        let mut run = flood::Run::with(&input, Params::default());
        run.adopt(&facts.standing);
        let finished = loop {
            match run.step(&input, s) {
                Ok(Step::More) => {}
                Ok(Step::Done) => break true,
                Err(_) => break false,
            }
        };
        if !finished {
            continue;
        }
        let layout = run.finish();
        let holds = facts
            .spawns
            .iter()
            .all(|&o| layout.of(StructureType::Spawn).any(|i| i.tile == o));
        if holds && validate(&input, &layout).valid() {
            return Some(layout);
        }
    }
    None
}

/// One corpus room and mode with a standing spawn, owned, so the reset cases run on threads.
struct Owned {
    what: String,
    terrain: TerrainGrid,
    controller: Tile,
    sources: Vec<Tile>,
    mineral: Option<Tile>,
    spawn: Tile,
}

/// What the reset cases of some rooms came to.
#[derive(Default)]
struct Tally {
    rooms: u32,
    cases: u32,
    no_plan: u32,
    other_anchor: u32,
    failures: Vec<String>,
    /// The cases where [`first_fit`] gives another plan than the room's.
    old_rule: Vec<String>,
    /// Room-levels where a later spawn sorts above the first one, so the job tries it before
    /// the first spawn (the anchors go from the highest tile down), and their cases.
    later_first: u32,
    later_first_cases: u32,
    room_levels: u32,
}

impl Tally {
    fn add(&mut self, o: Self) {
        self.rooms += o.rooms;
        self.cases += o.cases;
        self.no_plan += o.no_plan;
        self.other_anchor += o.other_anchor;
        self.failures.extend(o.failures);
        self.old_rule.extend(o.old_rule);
        self.later_first += o.later_first;
        self.later_first_cases += o.later_first_cases;
        self.room_levels += o.room_levels;
    }
}

/// The reset cases of one room and mode (the test below).
/// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`: the first spawn's level),
/// `PLANNER_MMO` (`Params::default`).
fn reset_cases(room: &Owned, levels: &[u8], s: &mut Scratch) -> Tally {
    let mut t = Tally::default();
    let input = PlanInput {
        terrain: &room.terrain,
        controller: room.controller,
        sources: &room.sources,
        mineral: room.mineral,
        spawn: Some(room.spawn),
    };
    let (a, what) = (room.spawn, &room.what);
    // The first plan: the first spawn alone, nothing else standing.
    let first = facts_of(
        &input,
        vec![a],
        &[],
        Vec::new(),
        vec![(StructureType::Spawn, a)],
    );
    let whole = job::plan_whole(first, Params::default(), s);
    let direct = flood::plan(&input, &mut Scratch::default())
        .ok()
        .filter(|l| validate(&input, l).valid());
    let layout = match (whole, direct) {
        (Ok((l, at)), Some(d)) => {
            if l != d {
                t.failures
                    .push(format!("{what}: the job's first plan is not flood::plan's"));
            }
            if at != a {
                t.failures
                    .push(format!("{what}: the first plan is anchored on {at:?}"));
            }
            l
        }
        (Err(_), None) => {
            t.no_plan += 1;
            return t;
        }
        (w, d) => {
            t.failures.push(format!(
                "{what}: the job gives {:?}, flood::plan a valid plan: {}",
                w.map(|x| x.1),
                d.is_some()
            ));
            return t;
        }
    };
    t.rooms += 1;
    let first_level = earliest_level(StructureType::Spawn, 0);
    if !layout
        .of(StructureType::Spawn)
        .any(|i| i.tile == a && Some(i.level) == first_level)
    {
        t.failures.push(format!(
            "{what}: the standing spawn is not the plan's first"
        ));
    }
    for &level in levels {
        let spawns: Vec<Tile> = std::iter::once(a)
            .chain(
                layout
                    .of(StructureType::Spawn)
                    .filter(|i| i.level <= level)
                    .map(|i| i.tile),
            )
            .collect();
        let later: Vec<Tile> = spawns.iter().copied().filter(|&x| x != a).collect();
        if later.is_empty() {
            t.failures
                .push(format!("{what} L{level}: the plan has no later spawn"));
            continue;
        }
        t.room_levels += 1;
        let tried_first = later.iter().any(|&x| x > a);
        if tried_first {
            t.later_first += 1;
        }
        for seen in Seen::ALL {
            let (standing, own) = seen.of(&layout, level);
            for gone in [false, true] {
                let (sp, sites) = if gone {
                    (later.clone(), vec![a])
                } else {
                    (spawns.clone(), Vec::new())
                };
                t.cases += 1;
                if tried_first {
                    t.later_first_cases += 1;
                }
                let facts = facts_of(&input, sp, &sites, standing.clone(), own.clone());
                let case = format!("{what} L{level} {seen:?} first spawn gone {gone}");
                if matches!(seen, Seen::Every)
                    && !gone
                    && first_fit(&facts, s).as_ref() != Some(&layout)
                {
                    t.old_rule.push(format!("{what} L{level}"));
                }
                match job::plan_whole(facts, Params::default(), s) {
                    Ok((l, at)) if l == layout => {
                        if at != a {
                            t.other_anchor += 1;
                        }
                    }
                    Ok((_, at)) => t.failures.push(format!(
                        "{case}: another plan, anchored on {at:?} (the first spawn {a:?})"
                    )),
                    Err(why) => t.failures.push(format!("{case}: no plan ({why})")),
                }
            }
        }
    }
    t
}

/// A global reset in a room built from its plan L up to a level with more than one spawn: every
/// standing spawn and own spawn site is tried as the anchor, and the job keeps L (C5/C12), with
/// the first spawn standing and with only its site left (the spawn lost and sited again), whether
/// the first spawn is tried first or a later one sorts above it and is tried before it (both
/// happen on the corpus, and both are asserted to). The rooms run on threads, merged in corpus
/// order.
///
/// Uses: `CONTROLLER_STRUCTURES` (through `allowed`: the levels that allow a second spawn),
/// `PLANNER_MMO` (the job's `Params::default`, and the gates of [`Seen::Mmo`]).
#[test]
fn a_global_reset_with_later_spawns_keeps_the_plan_the_room_was_built_from() {
    let levels: Vec<u8> = (1..=MAX_LEVEL)
        .filter(|&l| allowed(StructureType::Spawn, l) > 1)
        .collect();
    assert!(!levels.is_empty(), "some level allows a second spawn");
    let mut rooms: Vec<Owned> = Vec::new();
    harness::each(|room, mode, input| {
        if let Some(spawn) = input.spawn {
            rooms.push(Owned {
                what: format!("{} {mode:?}", room.name),
                terrain: input.terrain.clone(),
                controller: input.controller,
                sources: input.sources.to_vec(),
                mineral: input.mineral,
                spawn,
            });
        }
    });
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = rooms.len().div_ceil(threads).max(1);
    let levels = &levels;
    let mut t = Tally::default();
    std::thread::scope(|sc| {
        let handles: Vec<_> = rooms
            .chunks(chunk)
            .map(|part| {
                sc.spawn(move || {
                    let mut s = Scratch::default();
                    let mut t = Tally::default();
                    for room in part {
                        t.add(reset_cases(room, levels, &mut s));
                    }
                    t
                })
            })
            .collect();
        for h in handles {
            t.add(h.join().expect("a reset thread"));
        }
    });
    println!(
        "reset: {} rooms and modes with a standing spawn, {} planned, {} cases, {} with no valid \
         plan, {} the same plan from another anchor, {} failures",
        rooms.len(),
        t.rooms,
        t.cases,
        t.no_plan,
        t.other_anchor,
        t.failures.len()
    );
    println!(
        "reset: the first-fit rule over the standing spawns alone gives another plan in {} \
         cases: {:?}",
        t.old_rule.len(),
        t.old_rule
    );
    println!(
        "reset: a later spawn sorts above the first (tried before it) in {} of {} room-levels, \
         {} of the cases",
        t.later_first, t.room_levels, t.later_first_cases
    );
    assert!(t.cases > 0);
    assert!(
        t.later_first > 0 && t.later_first < t.room_levels,
        "both orders are exercised"
    );
    assert!(
        t.failures.is_empty(),
        "{} of {} cases:\n{}",
        t.failures.len(),
        t.cases,
        t.failures.join("\n")
    );
}

/// Whether `gates` hold `need`'s road due at `level`, read from its levels by gate directly.
fn needed(gates: &Gates, from: &[u8; GATES], level: u8) -> bool {
    Gate::ALL
        .iter()
        .any(|&g| gates.on(g) && from[g.index()] != u8::MAX && from[g.index()] <= level)
}

/// Every road's needs over the corpus, both candidates in every mode (C13): one need a road,
/// sorted by tile, the least level its item's; with every gate on each item is due from its own
/// level; with any gates, a road is due at a level exactly when a gate that is on needs it by
/// then, so one only gates that are off need is never due.
///
/// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`: the first level that allows a road),
/// `PLANNER_MMO` (the gates), `MAX_LEVEL`.
#[test]
fn every_road_is_due_from_the_least_level_a_gate_that_is_on_needs_it() {
    let mmo = Gates::from_config(&PLANNER_MMO);
    let combos: Vec<Gates> = (0u8..16)
        .map(|b| Gates {
            storage: b & 1 != 0,
            towers: b & 2 != 0,
            ramparts: b & 4 != 0,
            late: b & 8 != 0,
        })
        .collect();
    assert!(combos.contains(&mmo) && combos.contains(&ALL));
    let first_road = earliest_level(StructureType::Road, 0).expect("roads are allowed");
    let mut s = Scratch::default();
    // Per candidate: plans, roads, roads due at the top under the MMO's gates, roads no Base
    // structure needs, and of those the ones only Late structures need.
    let mut tally = [("flood", [0usize; 5]), ("bunker", [0usize; 5])];
    harness::each(|room, mode, input| {
        let candidates = [
            ("flood", flood::plan(input, &mut s)),
            ("bunker", bunker::plan(input, &mut s)),
        ];
        for (c, (name, plan)) in candidates.into_iter().enumerate() {
            let [plans, roads, due_top, off_only, late_only] = &mut tally[c].1;
            let Ok(layout) = plan else {
                continue;
            };
            let what = format!("{} {mode:?} {name}", room.name);
            *plans += 1;
            let road_items: Vec<_> = layout.of(StructureType::Road).collect();
            *roads += road_items.len();
            assert_eq!(
                layout.needs.len(),
                road_items.len(),
                "{what}: one need a road"
            );
            assert!(
                layout
                    .needs
                    .windows(2)
                    .all(|w| w[0].tile.index() < w[1].tile.index()),
                "{what}: the needs are sorted by tile, one a tile"
            );
            for n in &layout.needs {
                assert!(
                    road_items.iter().any(|i| i.tile == n.tile),
                    "{what}: a need with no road on {:?}",
                    n.tile
                );
                for &l in n.from.iter().filter(|&&l| l != u8::MAX) {
                    assert!(
                        (first_road..=MAX_LEVEL).contains(&l),
                        "{what}: {:?} needed from {l}",
                        n.tile
                    );
                }
            }
            for item in &road_items {
                let need = layout.road_need(item.tile);
                let least =
                    need.and_then(|n| n.from.iter().copied().filter(|&l| l != u8::MAX).min());
                assert_eq!(least, Some(item.level), "{what}: {:?} {need:?}", item.tile);
            }
            for item in &layout.items {
                assert_eq!(
                    ALL.due_level(&layout, item),
                    Some(item.level),
                    "{what}: every gate on, {item:?}"
                );
            }
            for gates in &combos {
                for item in &road_items {
                    let from = layout
                        .road_need(item.tile)
                        .map(|n| n.from)
                        .expect("every road has a need");
                    for level in 1..=MAX_LEVEL {
                        let due = gates.due_level(&layout, item).is_some_and(|l| l <= level);
                        assert_eq!(
                            due,
                            needed(gates, &from, level),
                            "{what} {gates:?} L{level}: {:?} {from:?}",
                            item.tile
                        );
                    }
                    if !needed(gates, &from, MAX_LEVEL) {
                        assert_eq!(gates.due_level(&layout, item), None, "{what} {gates:?}");
                    }
                }
            }
            for item in &road_items {
                let from = layout.road_need(item.tile).map(|n| n.from).expect("a need");
                if mmo.due_level(&layout, item).is_some_and(|l| l <= MAX_LEVEL) {
                    *due_top += 1;
                }
                let base = from[Gate::Base.index()];
                if base == u8::MAX {
                    *off_only += 1;
                    let others = Gate::ALL
                        .iter()
                        .filter(|&&g| g != Gate::Late && g != Gate::Base)
                        .all(|&g| from[g.index()] == u8::MAX);
                    if others {
                        *late_only += 1;
                    }
                }
            }
        }
    });
    for (name, [plans, roads, due_top, off_only, late_only]) in tally {
        println!(
            "roads, {name}: {plans} plans, {roads} roads, {due_top} due at RCL{MAX_LEVEL} under \
             the MMO's gates, {off_only} needed by no Base structure ({late_only} by Late ones \
             only)"
        );
        assert!(plans > 0 && roads > 0, "{name}");
        assert_eq!(due_top + off_only, roads, "{name}");
    }
}

/// The types the MMO's gates never place (`PLANNER_MMO`: the storage, the towers, the ramparts
/// and walls and the late structures off; the containers and links are the economy's or a gated
/// one's, the mineral container and the hub link).
const NOT_UNDER_MMO: [StructureType; 13] = [
    StructureType::Storage,
    StructureType::Tower,
    StructureType::Rampart,
    StructureType::Wall,
    StructureType::Terminal,
    StructureType::Extractor,
    StructureType::Lab,
    StructureType::Factory,
    StructureType::PowerSpawn,
    StructureType::Nuker,
    StructureType::Observer,
    StructureType::Container,
    StructureType::Link,
];

/// Under the MMO's gates every plan builds up level by level from its first spawn, with the
/// economy's containers and links standing as `economy::mining` places them, and `build::next`
/// offers only spawns, extensions and the roads due for them (C14): no destroy, no site of a gated
/// type or of the economy's, every item it places built at each level, and at the top the room
/// holds exactly those, the economy's and the first spawn.
///
/// Uses: `PLANNER_MMO` (the gates, `replace`, `max_sites`), `CONTROLLER_STRUCTURES` (through
/// `allowed`: each level's count), `MAX_LEVEL`.
#[test]
fn the_mmo_gates_place_only_spawns_extensions_and_their_roads() {
    let gates = Gates::from_config(&PLANNER_MMO);
    assert!(!gates.storage && !gates.towers && !gates.ramparts && !gates.late);
    let mut s = Scratch::default();
    let (mut rooms, mut sites) = (0u32, 0usize);
    harness::each(|room, mode, input| {
        let Ok(layout) = flood::plan(input, &mut s) else {
            return;
        };
        let anchor = input.spawn.or_else(|| {
            layout
                .of(StructureType::Spawn)
                .find(|i| Some(i.level) == earliest_level(StructureType::Spawn, 0))
                .map(|i| i.tile)
        });
        let Some(anchor) = anchor else {
            panic!("{} {mode:?}: a first spawn", room.name);
        };
        rooms += 1;
        let roles = &layout.roles;
        let mut standing = vec![(StructureType::Spawn, anchor)];
        for level in 1..=MAX_LEVEL {
            let what = format!("{} {mode:?} L{level}", room.name);
            for e in economy_items(&layout, level) {
                if !standing.contains(&e) {
                    standing.push(e);
                }
            }
            let a = Ask {
                layout: &layout,
                terrain: input.terrain,
                anchor,
                level,
                gates,
                replace: PLANNER_MMO.replace,
                max_sites: PLANNER_MMO.max_sites,
                holding: false,
                skip: &[],
                full: 0,
            };
            let (actions, census) = play(&layout, &a, &mut standing);
            assert!(
                actions.len() <= layout.items.len() * 2,
                "{what}: the build idles"
            );
            for n in &actions {
                match *n {
                    Next::Site { kind, tile } => {
                        sites += 1;
                        assert!(!NOT_UNDER_MMO.contains(&kind), "{what}: {kind:?} {tile:?}");
                        assert_eq!(Gate::of(kind, tile, roles), Gate::Base, "{what}");
                        assert!(
                            layout.items.iter().any(|i| i.kind == kind
                                && i.tile == tile
                                && gates.due_level(&layout, i).is_some_and(|l| l <= level)),
                            "{what}: {kind:?} {tile:?} is a planned item due"
                        );
                    }
                    Next::Destroy { kind, tile } => {
                        panic!("{what}: destroy {kind:?} {tile:?}")
                    }
                    Next::Idle => {}
                }
            }
            let what = format!("{what}: {census:?}");
            assert_eq!(census.built, census.due, "{what}");
            assert_eq!(
                (census.capped, census.blocked, census.cut_off),
                (0, 0, 0),
                "{what}"
            );
            for kind in [StructureType::Spawn, StructureType::Extension] {
                let n = standing.iter().filter(|(k, _)| *k == kind).count() as u32;
                assert!(n <= allowed(kind, level), "{what}: {kind:?} {n}");
            }
        }
        let want = by_kind(
            layout
                .items
                .iter()
                .filter(|i| {
                    gates.places(i.kind, i.tile, roles) && gates.due_level(&layout, i).is_some()
                })
                .map(|i| (i.kind, i.tile))
                .chain(economy_items(&layout, MAX_LEVEL))
                .chain(std::iter::once((StructureType::Spawn, anchor)))
                .collect(),
        );
        assert_eq!(
            by_kind(standing.clone()),
            want,
            "{} {mode:?}: the room at the top",
            room.name
        );
        assert_eq!(
            standing.len(),
            want.len(),
            "{} {mode:?}: no repeat",
            room.name
        );
    });
    println!("mmo gates: {rooms} rooms and modes, {sites} sites");
    assert!(rooms > 0);
}

/// The roads of `layout` due at `level` with `gates`, each with whether the hub joins it along
/// roads due then (every corpus plan has a road on its hub).
fn due_roads(layout: &Layout, gates: &Gates, level: u8) -> Vec<(Tile, bool)> {
    let due: Vec<Tile> = layout
        .of(StructureType::Road)
        .filter(|i| gates.due_level(layout, i).is_some_and(|l| l <= level))
        .map(|i| i.tile)
        .collect();
    let mut joined = vec![false; due.len()];
    let mut stack: Vec<Tile> = layout.roles.hub.into_iter().collect();
    while let Some(u) = stack.pop() {
        for (j, &t) in due.iter().enumerate() {
            if !joined[j] && u.range(t) <= 1 {
                joined[j] = true;
                stack.push(t);
            }
        }
    }
    due.into_iter().zip(joined).collect()
}

/// Under the MMO's gates the roads the base is served by still come (C13): every spawn and
/// extension with a road beside it in the whole plan has one due by the later of its own level
/// and that road's, and a road due that the whole plan's roads join to the hub at a level is
/// joined by the roads due then (the gates leave out only what serves gated structures alone).
///
/// Uses: `PLANNER_MMO` (the gates), `MAX_LEVEL`.
#[test]
fn the_mmo_gates_keep_the_roads_spawns_and_extensions_are_served_by() {
    let mmo = Gates::from_config(&PLANNER_MMO);
    let mut s = Scratch::default();
    let (mut plans, mut items, mut cut_levels) = (0u32, 0u32, 0u32);
    let mut failures: Vec<String> = Vec::new();
    harness::each(|room, mode, input| {
        let Ok(layout) = flood::plan(input, &mut s) else {
            return;
        };
        plans += 1;
        for i in layout
            .items
            .iter()
            .filter(|i| matches!(i.kind, StructureType::Spawn | StructureType::Extension))
        {
            let beside: Vec<_> = layout
                .of(StructureType::Road)
                .filter(|r| r.tile.range(i.tile) <= 1)
                .collect();
            let Some(whole) = beside.iter().map(|r| r.level).min() else {
                continue;
            };
            let gated = beside
                .iter()
                .filter_map(|r| mmo.due_level(&layout, r))
                .min();
            if !gated.is_some_and(|l| l <= whole.max(i.level)) {
                items += 1;
                failures.push(format!(
                    "{} {mode:?}: {:?} {:?} L{}: a road beside it from L{whole} in the whole \
                     plan, under the MMO's gates {gated:?}",
                    room.name, i.kind, i.tile, i.level
                ));
            }
        }
        for level in 1..=MAX_LEVEL {
            let whole = due_roads(&layout, &ALL, level);
            let gated = due_roads(&layout, &mmo, level);
            let cut = gated
                .iter()
                .filter(|&&(t, j)| !j && whole.iter().any(|&(u, wj)| u == t && wj))
                .count();
            if cut > 0 {
                cut_levels += 1;
                failures.push(format!(
                    "{} {mode:?} L{level}: {cut} roads due cut off from the hub under the MMO's \
                     gates",
                    room.name
                ));
            }
        }
    });
    println!(
        "mmo roads: {plans} plans, {items} spawns and extensions with no road due beside them, \
         {cut_levels} plan levels with roads due cut off from the hub"
    );
    assert!(plans > 0);
    assert!(
        failures.is_empty(),
        "{} failures over {plans} plans:\n{}",
        failures.len(),
        failures
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
