//! Runs a candidate planner (P4.2) over the corpus in three modes and checks it: a fresh room (the
//! planner places the first spawn), a room with a standing spawn on open ground (a synthetic one a
//! player might have placed, as a live room has, E7N57 among them), and one with a standing spawn
//! on the ground the economy keeps clear by the controller (the game allows it, V237).

use std::fmt::Write as _;
use std::time::Instant;

use rustedscreep_consts::{
    ENERGY_REGEN_TIME, RAMPART_DECAY_AMOUNT, RAMPART_DECAY_TIME, REPAIR_COST, ROOM_AREA,
    SOURCE_ENERGY_CAPACITY, StructureType,
};
use rustedscreep_core::economy::placement::CONTROLLER_CLEAR;
use rustedscreep_core::movement::Tile;
use rustedscreep_core::planner::rules::{blocks, earliest_level, protected};
use rustedscreep_core::planner::{
    Board, Item, Layout, MAX_LEVEL, PlanError, PlanInput, Report, Scratch, defence,
    distance_transform, sites, validate,
};

use super::corpus::{self, CorpusRoom};

/// A planner's whole run.
pub type PlanFn = fn(&PlanInput<'_>, &mut Scratch) -> Result<Layout, PlanError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Fresh,
    Spawned,
    /// A standing spawn within `CONTROLLER_CLEAR` of the controller.
    Clear,
}

pub const MODES: [Mode; 3] = [Mode::Fresh, Mode::Spawned, Mode::Clear];

/// One room, one mode.
pub struct Outcome {
    pub room: String,
    pub mode: Mode,
    pub result: Result<Judged, PlanError>,
    /// The layout with its ramparts replaced by the minimum cut's for its protected structures
    /// ([`with_min_cut`]), checked: ADR-007's hybrid (option c) for a candidate that places
    /// ramparts another way, so the layouts compare under one rampart policy.
    pub hybrid: Option<Judged>,
    /// The room's source count and standing spawn, for the measures.
    pub sources: usize,
    pub spawn: Option<Tile>,
}

/// A first spawn a player might have placed: the open tile with a 5 x 5 of open ground around it
/// (outside the ground the economy keeps clear) nearest the sources and the controller by summed
/// range, lowest index on a tie.
pub fn synthetic_spawn(input: &PlanInput<'_>, s: &mut Scratch) -> Option<Tile> {
    let mut board = Board::new(input);
    sites::reserve(&mut board, input);
    distance_transform(|t| board.open(t) && !board.is_clear(t), &mut s.dt);
    let score = |t: Tile| -> u32 {
        input
            .sources
            .iter()
            .map(|&x| u32::from(x.range(t)))
            .sum::<u32>()
            + u32::from(input.controller.range(t))
    };
    (0..ROOM_AREA)
        .filter_map(Tile::from_index)
        .filter(|t| s.dt.get(t.index()).is_some_and(|&d| d >= 3))
        .min_by_key(|&t| (score(t), t.index()))
}

/// A first spawn on the ground kept clear for the upgraders: the open tile within
/// `CONTROLLER_CLEAR` of the controller nearest it, with the 3 x 3 around it open (so the spawn
/// alone splits no ground), lowest index on a tie; `None` where there is none.
pub fn clear_spawn(input: &PlanInput<'_>, s: &mut Scratch) -> Option<Tile> {
    let mut board = Board::new(input);
    sites::reserve(&mut board, input);
    distance_transform(|t| board.open(t), &mut s.dt);
    let natural_near = |t: Tile| {
        input
            .sources
            .iter()
            .chain(input.mineral.iter())
            .any(|&x| x.range(t) <= 1)
    };
    (0..ROOM_AREA)
        .filter_map(Tile::from_index)
        .filter(|&t| t.range(input.controller) <= CONTROLLER_CLEAR && !natural_near(t))
        .filter(|t| s.dt.get(t.index()).is_some_and(|&d| d >= 2))
        .min_by_key(|&t| (t.range(input.controller), t.index()))
}

/// `layout` with its ramparts replaced by the minimum cut for its protected structures, at the
/// levels `defence::apply` places them (`defence::levels`: the line from the first rampart level).
pub fn with_min_cut(
    input: &PlanInput<'_>,
    layout: &Layout,
    s: &mut Scratch,
) -> Result<Layout, PlanError> {
    let cut = defence::plan(input, &protected_of(layout), s)?;
    let first = earliest_level(StructureType::Rampart, 0).ok_or(PlanError::NoRoom("rampart"))?;
    let level_of = |t: Tile| {
        layout
            .items
            .iter()
            .find(|i| {
                i.tile == t && !matches!(i.kind, StructureType::Road | StructureType::Rampart)
            })
            .map(|i| i.level)
    };
    let mut items: Vec<Item> = layout
        .items
        .iter()
        .filter(|i| i.kind != StructureType::Rampart)
        .copied()
        .collect();
    for (t, level) in defence::levels(input, &cut, first, level_of) {
        items.push(Item {
            kind: StructureType::Rampart,
            tile: t,
            level,
        });
    }
    let mut out = Layout {
        items,
        roles: layout.roles.clone(),
        needs: layout.needs.clone(),
    };
    out.sort();
    Ok(out)
}

/// The layout's protected structures, sorted (the rule `defence::protected_tiles` uses on a board).
pub fn protected_of(layout: &Layout) -> Vec<Tile> {
    let r = &layout.roles;
    let remote: Vec<Tile> = r
        .controller_link
        .into_iter()
        .chain(r.source_links.iter().flatten().copied())
        .collect();
    let mut out: Vec<Tile> = layout
        .items
        .iter()
        .filter(|i| {
            i.kind != StructureType::Rampart
                && blocks(i.kind)
                && protected(i.kind, remote.contains(&i.tile))
        })
        .map(|i| i.tile)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Calls `f` for every corpus room in every mode, with the room's input (a room with no ground
/// for [`clear_spawn`] skips that mode).
pub fn each(mut f: impl FnMut(&CorpusRoom, Mode, &PlanInput<'_>)) {
    let mut s = Scratch::default();
    for room in corpus::load() {
        let terrain = room.terrain();
        let mineral = room.minerals.first().map(|&(_, t)| t);
        let fresh = PlanInput {
            terrain: &terrain,
            controller: room.controller,
            sources: &room.sources,
            mineral,
            spawn: None,
        };
        f(&room, Mode::Fresh, &fresh);
        let spawn = synthetic_spawn(&fresh, &mut s)
            .unwrap_or_else(|| panic!("{}: no spot for a spawn", room.name));
        let spawned = PlanInput {
            spawn: Some(spawn),
            ..fresh
        };
        f(&room, Mode::Spawned, &spawned);
        if let Some(spawn) = clear_spawn(&fresh, &mut s) {
            let clear = PlanInput {
                spawn: Some(spawn),
                ..fresh
            };
            f(&room, Mode::Clear, &clear);
        }
    }
}

/// Runs `plan` everywhere and checks each layout.
pub fn run_all(plan: PlanFn) -> Vec<Outcome> {
    let mut s = Scratch::default();
    let mut out = Vec::new();
    each(|room, mode, input| {
        let result = plan(input, &mut s).map(|layout| {
            let report = validate(input, &layout);
            (layout, report)
        });
        let hybrid = result.as_ref().ok().and_then(|(layout, _)| {
            with_min_cut(input, layout, &mut s).ok().map(|h| {
                let report = validate(input, &h);
                (h, report)
            })
        });
        out.push(Outcome {
            room: room.name.to_string(),
            mode,
            result,
            hybrid,
            sources: input.sources.len(),
            spawn: input.spawn,
        });
    });
    out
}

/// One line per room and mode that failed, with why: the plan, or the plan with the minimum
/// cut's ramparts (the hybrid).
pub fn failures(outcomes: &[Outcome]) -> Vec<String> {
    let problems = |what: &str, o: &Outcome, r: &Report| {
        let shown: Vec<String> = r
            .problems
            .iter()
            .take(6)
            .map(|p| format!("{p:?}"))
            .collect();
        format!(
            "{} {:?}{what}: {} problems: {}",
            o.room,
            o.mode,
            r.problems.len(),
            shown.join("; ")
        )
    };
    outcomes
        .iter()
        .filter_map(|o| match (&o.result, &o.hybrid) {
            (Err(e), _) => Some(format!("{} {:?}: {e:?}", o.room, o.mode)),
            (Ok((_, r)), _) if !r.valid() => Some(problems("", o, r)),
            (Ok(_), None) => Some(format!("{} {:?}: no hybrid", o.room, o.mode)),
            (Ok(_), Some((_, h))) if !h.valid() => Some(problems(" with min-cut ramparts", o, h)),
            (Ok(_), Some(_)) => None,
        })
        .collect()
}

/// Asserts that every room gets a valid plan in every mode, also with the minimum cut's
/// ramparts.
pub fn assert_valid(outcomes: &[Outcome]) {
    let f = failures(outcomes);
    assert!(
        f.is_empty(),
        "{} of {} plans fail:\n{}",
        f.len(),
        outcomes.len(),
        f.join("\n")
    );
}

/// Asserts that planning again with a fresh scratch gives the same layout everywhere.
pub fn assert_deterministic(plan: PlanFn) {
    let mut warm = Scratch::default();
    each(|room, mode, input| {
        let a = plan(input, &mut warm);
        let b = plan(input, &mut Scratch::default());
        assert_eq!(a, b, "{} {mode:?}", room.name);
    });
}

fn stats(values: &mut [f64]) -> (f64, f64, f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    (values[0], values[n / 2], mean, values[n - 1])
}

/// A layout with its report.
pub type Judged = (Layout, Report);

/// A valid plan's outcome, the plan, its report and its hybrid.
type Valid<'a> = (&'a Outcome, &'a Layout, &'a Report, Option<&'a Judged>);

/// One measure of a valid plan (with its room and hybrid), or `None` where it has none.
type Measure<'a> = &'a dyn Fn(&Outcome, &Layout, &Report, Option<&Judged>) -> Option<f64>;

/// The energy a tick decay takes from ramparts by creep repair, per rampart (V236, V222).
///
/// Uses: `RAMPART_DECAY_AMOUNT`, `RAMPART_DECAY_TIME`, `REPAIR_COST`.
pub fn rampart_upkeep() -> f64 {
    f64::from(RAMPART_DECAY_AMOUNT) / f64::from(RAMPART_DECAY_TIME) * f64::from(REPAIR_COST)
}

/// The energy a tick a room's own sources give.
///
/// Uses: `SOURCE_ENERGY_CAPACITY`, `ENERGY_REGEN_TIME`.
pub fn own_income(sources: usize) -> f64 {
    sources as f64 * f64::from(SOURCE_ENERGY_CAPACITY) / f64::from(ENERGY_REGEN_TIME)
}

/// A table of the measures over the valid plans, per mode: least, median, mean, most.
pub fn summary(name: &str, outcomes: &[Outcome]) -> String {
    let mut out = String::new();
    for mode in MODES {
        let ok: Vec<Valid<'_>> = outcomes
            .iter()
            .filter(|o| o.mode == mode)
            .filter_map(|o| {
                o.result
                    .as_ref()
                    .ok()
                    .filter(|(_, r)| r.valid())
                    .map(|(l, r)| (o, l, r, o.hybrid.as_ref()))
            })
            .collect();
        let total = outcomes.iter().filter(|o| o.mode == mode).count();
        let hybrid_ok = ok
            .iter()
            .filter(|(_, _, _, h)| h.is_some_and(|(_, r)| r.valid()))
            .count();
        let _ = writeln!(
            out,
            "{name} {mode:?}: {} of {total} valid ({hybrid_ok} valid with min-cut ramparts)",
            ok.len()
        );
        let mut row = |label: &str, f: Measure<'_>| {
            let mut v: Vec<f64> = ok
                .iter()
                .filter_map(|&(o, l, r, h)| f(o, l, r, h))
                .collect();
            if v.is_empty() {
                return;
            }
            let (lo, med, mean, hi) = stats(&mut v);
            let _ = writeln!(
                out,
                "  {label:<31} least {lo:>9.1}  median {med:>9.1}  mean {mean:>9.1}  most {hi:>9.1}"
            );
        };
        row("ramparts", &|_, _, r, _| {
            Some(f64::from(r.metrics.ramparts))
        });
        for level in 2..MAX_LEVEL {
            row(
                &format!("ramparts standing at RCL{level}"),
                &|_, l, _, _| Some(f64::from(l.count(StructureType::Rampart, level))),
            );
        }
        for level in [2, MAX_LEVEL] {
            row(&format!("upkeep % of income, RCL{level}"), &|o, l, _, _| {
                let ramparts = f64::from(l.count(StructureType::Rampart, level));
                Some(100.0 * ramparts * rampart_upkeep() / own_income(o.sources))
            });
        }
        row("hub range to the standing spawn", &|o, l, _, _| {
            let (spawn, hub) = (o.spawn?, l.roles.hub?);
            Some(f64::from(hub.range(spawn)))
        });
        row("roads", &|_, _, r, _| Some(f64::from(r.metrics.roads)));
        row("road energy", &|_, _, r, _| {
            Some(f64::from(r.metrics.road_energy))
        });
        row("hub to sources (sum)", &|_, _, r, _| {
            Some(f64::from(r.metrics.to_sources.iter().sum::<u32>()))
        });
        row("hub to controller", &|_, _, r, _| {
            r.metrics.to_controller.map(f64::from)
        });
        row("hub to mineral", &|_, _, r, _| {
            r.metrics.to_mineral.map(f64::from)
        });
        row("extension mean cost", &|_, _, r, _| {
            Some(f64::from(r.metrics.extension_mean_x100) / 100.0)
        });
        row("extension most cost", &|_, _, r, _| {
            Some(f64::from(r.metrics.extension_max))
        });
        row("upgrade posts", &|_, _, r, _| {
            Some(f64::from(r.metrics.posts))
        });
        row("weakest rampart damage", &|_, _, r, _| {
            r.metrics.weakest_rampart_damage.map(f64::from)
        });
        row("inside tiles", &|_, _, r, _| {
            Some(f64::from(r.metrics.inside))
        });
        row("source links", &|_, _, r, _| {
            Some(f64::from(r.metrics.source_links))
        });
        // The same layout under the shared rampart policy (the minimum cut).
        row("min-cut: ramparts", &|_, _, _, h| {
            h.map(|(_, h)| f64::from(h.metrics.ramparts))
        });
        for level in 2..MAX_LEVEL {
            row(
                &format!("min-cut: standing at RCL{level}"),
                &|_, _, _, h| h.map(|(l, _)| f64::from(l.count(StructureType::Rampart, level))),
            );
        }
        for level in [2, MAX_LEVEL] {
            row(&format!("min-cut: upkeep %, RCL{level}"), &|o, _, _, h| {
                let ramparts = f64::from(h?.0.count(StructureType::Rampart, level));
                Some(100.0 * ramparts * rampart_upkeep() / own_income(o.sources))
            });
        }
        row("min-cut: weakest damage", &|_, _, _, h| {
            h.and_then(|(_, h)| h.metrics.weakest_rampart_damage.map(f64::from))
        });
        row("min-cut: inside tiles", &|_, _, _, h| {
            h.map(|(_, h)| f64::from(h.metrics.inside))
        });
    }
    out
}

/// What a candidate's run reports: per step the phase it did and how long it took, and whether
/// the run finished with a plan.
pub type Steps = (Vec<(&'static str, f64)>, bool);

/// Times a candidate's steps over the corpus in every mode (`run` plans one room with the kept
/// `Scratch` and returns each step's phase and milliseconds), the best of `passes` runs per step,
/// and the minimum cut on each finished layout; a table per phase of the median, the 90th
/// percentile and the most, with the room and mode of the most. Native wall time: the ratios
/// between phases carry over to wasm, the numbers do not.
pub fn step_costs(
    name: &str,
    passes: usize,
    plan: PlanFn,
    mut run: impl FnMut(&PlanInput<'_>, &mut Scratch) -> Steps,
) -> String {
    let mut s = Scratch::default();
    let mut by_phase: Vec<(&'static str, Vec<(f64, String)>)> = Vec::new();
    let mut push = |phase: &'static str, ms: f64, at: String| match by_phase
        .iter_mut()
        .find(|(p, _)| *p == phase)
    {
        Some((_, v)) => v.push((ms, at)),
        None => by_phase.push((phase, vec![(ms, at)])),
    };
    let mut failed = Vec::new();
    each(|room, mode, input| {
        let at = format!("{} {mode:?}", room.name);
        let (mut best, ok) = run(input, &mut s);
        if !ok {
            let ms: f64 = best.iter().map(|x| x.1).sum();
            failed.push(format!("{at} {ms:.3} ms"));
            return;
        }
        for _ in 1..passes {
            let (again, _) = run(input, &mut s);
            let phases = |v: &[(&'static str, f64)]| v.iter().map(|x| x.0).collect::<Vec<_>>();
            assert_eq!(phases(&again), phases(&best), "{at}: the passes differ");
            for (b, (_, ms)) in best.iter_mut().zip(again) {
                b.1 = b.1.min(ms);
            }
        }
        let mut whole = 0.0;
        for &(phase, ms) in &best {
            whole += ms;
            push(phase, ms, at.clone());
        }
        push("(whole plan)", whole, at.clone());
        if let Ok(layout) = plan(input, &mut s) {
            let protect = protected_of(&layout);
            let mut cut = f64::MAX;
            for _ in 0..passes {
                let t0 = Instant::now();
                let _ = defence::plan(input, &protect, &mut s);
                cut = cut.min(t0.elapsed().as_secs_f64() * 1e3);
            }
            push("(min-cut on the layout)", cut, at);
        }
    });
    let mut out = format!(
        "{name}: per step, ms, best of {passes}; runs with no plan, left out (one pass each): \
         {failed:?}\n"
    );
    for (phase, v) in &mut by_phase {
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        let n = v.len();
        let pick = |q: f64| {
            v.get(((n as f64 - 1.0) * q).round() as usize)
                .map_or(0.0, |x| x.0)
        };
        let (most, at) = v.last().map_or((0.0, ""), |(m, a)| (*m, a.as_str()));
        let _ = writeln!(
            out,
            "  {phase:<24} n {n:>4}  median {:>7.3}  p90 {:>7.3}  most {most:>7.3}  ({at})",
            pick(0.5),
            pick(0.9),
        );
    }
    out
}
