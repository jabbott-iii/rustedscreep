//! The check against the engine's own constants (the fixture), and against injected drift.

use super::*;
use crate::scheduler::Step;
use rustedscreep_consts as c;

/// @screeps/common 2.16.0 lib/constants.js as `NAME<TAB>JSON` lines (js/tools/consts-fixture.mjs).
const FIXTURE: &str = include_str!("../../tests/fixtures/constants-common-2.16.0.tsv");

/// Mixed-case globals the real runtime also has (engine game.js:470-511); the check never reads them.
const OTHER_GLOBALS: [&str; 6] = ["Game", "Memory", "RawMemory", "_", "Object", "module"];

fn fixture() -> Vec<(String, Option<String>)> {
    FIXTURE
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (name, json) = l.split_once('\t').expect("NAME<TAB>JSON");
            (name.to_owned(), Some(json.to_owned()))
        })
        .collect()
}

/// A global object: own names in order, each with its JSON text (`None`: no JSON text), plus the
/// built-in `JSON` and some mixed-case globals.
struct Fake {
    globals: Vec<(String, Option<String>)>,
    calls: u32,
    fail_names: bool,
    fail_value_at: Option<u32>,
}

impl Fake {
    fn new(constants: Vec<(String, Option<String>)>) -> Self {
        let mut globals: Vec<(String, Option<String>)> = OTHER_GLOBALS
            .iter()
            .map(|n| ((*n).to_owned(), Some("{}".to_owned())))
            .collect();
        globals.push(("JSON".to_owned(), Some("{}".to_owned())));
        globals.extend(constants);
        Self {
            globals,
            calls: 0,
            fail_names: false,
            fail_value_at: None,
        }
    }

    fn set(&mut self, name: &str, json: Option<&str>) {
        let value = json.map(str::to_owned);
        match self.globals.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = value,
            None => self.globals.push((name.to_owned(), value)),
        }
    }

    fn remove(&mut self, name: &str) {
        self.globals.retain(|(n, _)| n != name);
    }

    fn json_of(&self, name: &str) -> String {
        self.globals
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, j)| j.clone())
            .expect("present")
    }
}

impl Globals for Fake {
    fn names(&mut self) -> Result<Option<String>, &'static str> {
        self.calls += 1;
        if self.fail_names {
            return Err("getOwnPropertyNames threw");
        }
        let quoted: Vec<String> = self
            .globals
            .iter()
            .map(|(n, _)| format!("\"{n}\""))
            .collect();
        Ok(Some(format!("[{}]", quoted.join(","))))
    }

    fn value(&mut self, index: u32) -> Result<Option<String>, &'static str> {
        self.calls += 1;
        if self.fail_value_at == Some(index) {
            return Err("JSON.stringify threw");
        }
        Ok(self
            .globals
            .get(index as usize)
            .and_then(|(_, j)| j.clone()))
    }
}

/// Runs the check to the end; returns it and the steps it took.
fn run(fake: &mut Fake) -> (Check, u32, Vec<Fault>) {
    let mut check = Check::default();
    let (mut steps, mut faults) = (0u32, Vec::new());
    loop {
        steps += 1;
        assert!(steps < 10_000, "the check does not end");
        match check.step(fake) {
            Ok(Step::More) => {}
            Ok(Step::Done) => break,
            Err(f) => faults.push(f),
        }
        if check.is_finished() && !faults.is_empty() {
            // A failed check answers Done from then on.
            assert_eq!(check.step(fake), Ok(Step::Done));
            break;
        }
    }
    (check, steps, faults)
}

fn report_of(fake: &mut Fake) -> Report {
    let (check, _, faults) = run(fake);
    assert!(faults.is_empty(), "{faults:?}");
    check.report().cloned().expect("done")
}

fn lines(r: &Report) -> Vec<String> {
    r.findings.iter().map(ToString::to_string).collect()
}

fn known_paths(r: &Report) -> Vec<&'static str> {
    r.known_seen
        .iter()
        .map(|&i| KNOWN.get(usize::from(i)).expect("an index into KNOWN").path)
        .collect()
}

/// The engine's own constants match the crate everywhere but at the known differences, and every
/// engine constant is read by a check or named in the list. This is the native form of the live
/// check: a crate bump that changes a value fails here.
#[test]
fn the_engine_constants_match_the_crate() {
    let mut fake = Fake::new(fixture());
    let (check, steps, faults) = run(&mut fake);
    assert!(faults.is_empty(), "{faults:?}");
    let r = check.report().expect("done");
    assert_eq!(lines(r), Vec::<String>::new(), "{}", r.counts());
    assert_eq!(r.finding_count(), 0);
    // 422 engine constants and the built-in JSON; the mixed-case globals are not read.
    let constants = fixture().len();
    assert_eq!(r.server_names as usize, constants + 1);
    // Every known difference occurs against common 2.16.0, each once per name it covers.
    let mut seen = known_paths(r);
    seen.sort_unstable();
    let mut all: Vec<&str> = KNOWN.iter().map(|k| k.path).collect();
    all.sort_unstable();
    assert_eq!(seen, all);
    // Every comparison matched but one: the crate's ERR_ACCESS_DENIED, which 2.16.0 lacks (known).
    assert!(r.compared > 1000, "{}", r.counts());
    assert_eq!(r.matched + 1, r.compared, "{}", r.counts());
    // One step for the names, one per constant read, one to seal, one per group, one to finish.
    let groups = expect::group_count();
    assert_eq!(steps, 1 + (constants as u32 + 1) + 1 + groups + 1);
    assert_eq!(fake.calls, 1 + constants as u32 + 1);
}

/// What the live MMO is expected to add over common 2.16.0: ERR_ACCESS_DENIED (common 49c0fc6),
/// which the crate has.
#[test]
fn err_access_denied_matches_when_the_server_has_it() {
    let mut fake = Fake::new(fixture());
    fake.set(
        "ERR_ACCESS_DENIED",
        Some(&(c::ErrorCode::AccessDenied as i8).to_string()),
    );
    let r = report_of(&mut fake);
    assert_eq!(r.finding_count(), 0, "{:?}", lines(&r));
    assert!(!known_paths(&r).contains(&"ERR_ACCESS_DENIED"));
    fake.set("ERR_ACCESS_DENIED", Some("-17"));
    let r = report_of(&mut fake);
    assert_eq!(
        lines(&r),
        [format!(
            "ERR_ACCESS_DENIED (mismatch): crate {}, server -17",
            c::ErrorCode::AccessDenied as i8
        )]
    );
}

/// Each kind of drift shows as its own finding, with the crate's value and the server's own text.
#[test]
fn drift_is_reported() {
    let mut fake = Fake::new(fixture());
    let life = c::CREEP_LIFE_TIME - 100;
    fake.set("CREEP_LIFE_TIME", Some(&life.to_string()));
    fake.set("CREEP_CORPSE_RATE", Some("0.25"));
    fake.set("FIND_HOSTILE_CREEPS", Some("104"));
    fake.set("ERR_SOMETHING_NEW", Some("-20"));
    fake.set("STRUCTURE_SHRINE", Some("\"shrine\""));
    fake.set("NEW_CONSTANT", Some("{\"a\":1}"));
    fake.set("TOWER_FALLOFF", None);
    fake.set("SAFE_MODE_COST", Some("{"));
    fake.remove("RAMPART_DECAY_TIME");
    // BODYPART_COST without "work" and with "wings"; CONTROLLER_STRUCTURES.extension at RCL 8 off.
    let cost = fake
        .json_of("BODYPART_COST")
        .replace("\"work\":100,", "")
        .replace('}', ",\"wings\":200}");
    fake.set("BODYPART_COST", Some(&cost));
    let ext8 = c::StructureType::Extension.controller_structures(8);
    let structures = fake.json_of("CONTROLLER_STRUCTURES").replacen(
        &format!("\"8\":{ext8}}}"),
        &format!("\"8\":{}}}", ext8 + 10),
        1,
    );
    fake.set("CONTROLLER_STRUCTURES", Some(&structures));
    let r = report_of(&mut fake);
    assert_eq!(
        lines(&r),
        [
            // The scalars, in the crate's order.
            format!(
                "CREEP_LIFE_TIME (mismatch): crate {}, server {life}",
                c::CREEP_LIFE_TIME
            ),
            format!(
                "CREEP_CORPSE_RATE (mismatch): crate {}, server 0.25",
                report::Fixed(f64::from(c::CREEP_CORPSE_RATE))
            ),
            format!(
                "RAMPART_DECAY_TIME (missing on the server): crate {}",
                c::RAMPART_DECAY_TIME
            ),
            "SAFE_MODE_COST (unreadable): server the text ends inside a value at byte 1".to_owned(),
            "TOWER_FALLOFF (unreadable): server no JSON value".to_owned(),
            // Then the groups that follow.
            "ERR_SOMETHING_NEW (not in the crate): server -20".to_owned(),
            format!(
                "FIND_HOSTILE_CREEPS (mismatch): crate {}, server 104",
                c::Find::HostileCreeps as u16
            ),
            format!(
                "BODYPART_COST.work (missing on the server): crate {}",
                c::Part::Work.cost()
            ),
            "BODYPART_COST.wings (not in the crate): server 200".to_owned(),
            "STRUCTURE_SHRINE (not in the crate): server \"shrine\"".to_owned(),
            format!(
                "CONTROLLER_STRUCTURES.extension.8 (mismatch): crate {ext8}, server {}",
                ext8 + 10
            ),
            "NEW_CONSTANT (unknown constant): server an object".to_owned(),
        ]
    );
    assert_eq!(
        (r.mismatched, r.missing, r.extra, r.unreadable, r.unknown),
        (4, 2, 3, 2, 1)
    );
}

/// The nested tables report the exact entry that moved.
#[test]
fn table_drift_names_the_entry() {
    let mut fake = Fake::new(fixture());
    // REACTIONS: hydrogen and oxygen make something else, one way round only.
    let reactions = fake
        .json_of("REACTIONS")
        .replacen("\"O\":\"OH\"", "\"O\":\"HO\"", 1);
    fake.set("REACTIONS", Some(&reactions));
    // BOOSTS: UO harvests 4 instead of the crate's value.
    let Some(c::Boost::Harvest(uo)) = c::ResourceType::UtriumOxide.boost() else {
        panic!("UO boosts harvest");
    };
    let boosts = fake.json_of("BOOSTS").replacen(
        &format!("\"UO\":{{\"harvest\":{uo}}}"),
        "\"UO\":{\"harvest\":4}",
        1,
    );
    fake.set("BOOSTS", Some(&boosts));
    // COMMODITIES: a battery gains a level.
    let commodities =
        fake.json_of("COMMODITIES")
            .replacen("\"battery\":{", "\"battery\":{\"level\":1,", 1);
    fake.set("COMMODITIES", Some(&commodities));
    let r = report_of(&mut fake);
    assert_eq!(
        lines(&r),
        [
            // The fixture's H + O product is the crate's Hydroxide (pinned in the fixture text above).
            format!(
                "REACTIONS.H.O (mismatch): crate \"{}\", server \"HO\"",
                c::ResourceType::Hydroxide
            ),
            format!("BOOSTS.work.UO.harvest (mismatch): crate {uo}, server 4"),
            "COMMODITIES.battery.level (not in the crate): server 1".to_owned(),
        ]
    );
}

/// A global without game constants is one failure, not a finding per constant.
#[test]
fn a_global_without_constants_fails_once() {
    let mut fake = Fake::new(Vec::new());
    let (check, _, faults) = run(&mut fake);
    assert_eq!(
        check.phase(),
        Phase::Failed("the global object holds no game constants")
    );
    assert_eq!(faults.len(), 1);
    assert!(check.report().is_none());
}

/// A throwing JS call fails the check for this global, with a fault for the scheduler's log.
#[test]
fn js_failures_end_the_check() {
    let mut fake = Fake::new(fixture());
    fake.fail_names = true;
    let (check, _, faults) = run(&mut fake);
    assert_eq!(check.phase(), Phase::Failed("getOwnPropertyNames threw"));
    assert_eq!(faults, [Fault::new("getOwnPropertyNames threw", 0)]);

    let mut fake = Fake::new(fixture());
    fake.fail_value_at = Some(50);
    let (check, _, faults) = run(&mut fake);
    assert_eq!(check.phase(), Phase::Failed("JSON.stringify threw"));
    assert_eq!(faults, [Fault::new("JSON.stringify threw", 50)]);
    // Nothing is retried: the next step is Done without a JS call.
    let calls = fake.calls;
    let mut check = check;
    assert_eq!(check.step(&mut fake), Ok(Step::Done));
    assert_eq!(fake.calls, calls);
}

/// Names that are not a JSON array of strings fail the check.
#[test]
fn bad_names_fail_the_check() {
    struct Names(&'static str);
    impl Globals for Names {
        fn names(&mut self) -> Result<Option<String>, &'static str> {
            Ok(Some(self.0.to_owned()))
        }
        fn value(&mut self, _: u32) -> Result<Option<String>, &'static str> {
            Ok(None)
        }
    }
    for text in ["{}", "[1]", "[\"OK\",null]", "nope"] {
        let mut check = Check::default();
        assert!(check.step(&mut Names(text)).is_err(), "{text}");
        assert!(matches!(check.phase(), Phase::Failed(_)), "{text}");
    }
}

/// Phases advance as the steps run, and reading needs no comparison yet.
#[test]
fn phases_advance() {
    let mut fake = Fake::new(fixture());
    let mut check = Check::default();
    assert_eq!(check.phase(), Phase::Waiting);
    assert_eq!(check.step(&mut fake), Ok(Step::More));
    let of = (fixture().len() + 1) as u32;
    assert_eq!(check.phase(), Phase::Reading { read: 0, of });
    assert_eq!(check.step(&mut fake), Ok(Step::More));
    assert_eq!(check.phase(), Phase::Reading { read: 1, of });
    assert!(!check.is_finished() && check.report().is_none());
}

/// A server that differs everywhere keeps the first findings and counts the rest.
#[test]
fn findings_are_capped() {
    let constants = fixture()
        .into_iter()
        .map(|(n, j)| {
            let off = j
                .filter(|t| t.parse::<u32>().is_ok())
                .map(|t| format!("{t}1"));
            (n, off.or(Some("{}".to_owned())))
        })
        .collect();
    let mut fake = Fake::new(constants);
    let r = report_of(&mut fake);
    assert_eq!(r.findings.len(), MAX_FINDINGS);
    assert!(r.finding_count() as usize > MAX_FINDINGS);
}

/// Removes the member `key` (whose value has no nested object) from the JSON object text `json`.
fn without_member(json: &str, key: &str) -> String {
    let start = json
        .find(&format!("\"{key}\":"))
        .expect("the member is there");
    let end = start + json[start..].find('}').expect("its value ends") + 1;
    let end = if json[end..].starts_with(',') {
        end + 1
    } else {
        end
    };
    format!("{}{}", &json[..start], &json[end..])
}

/// The tolerances (a missing zero row, a server 0 for a crate None, the second comparisons, sets,
/// shapes, a family member that is not JSON) report drift instead of absorbing it.
#[test]
fn tolerances_do_not_absorb_drift() {
    use c::StructureType::Spawn;
    let mut fake = Fake::new(fixture());
    let hits = c::SPAWN_HITS + 1;
    fake.set("SPAWN_HITS", Some(&hits.to_string()));
    fake.set("LINK_HITS_MAX", Some(&(c::LINK_HITS + 1).to_string()));
    fake.set("MINERAL_DENSITY", Some("[1,2]"));
    let parts = fake.json_of("BODYPARTS_ALL").replace(']', ",\"wings\"]");
    fake.set("BODYPARTS_ALL", Some(&parts));
    fake.set("STRUCTURE_SPAWN", Some("{"));
    let spawn = Spawn.to_string();
    let obstacles = fake
        .json_of("OBSTACLE_OBJECT_TYPES")
        .replacen(&format!("\"{spawn}\","), "", 1);
    fake.set("OBSTACLE_OBJECT_TYPES", Some(&obstacles));
    let structures = without_member(&fake.json_of("CONTROLLER_STRUCTURES"), &spawn);
    fake.set("CONTROLLER_STRUCTURES", Some(&structures));
    let rcl5 = c::RAMPART_HITS_MAX_RCL5;
    let ramparts = fake.json_of("RAMPART_HITS_MAX").replacen(
        &format!("\"5\":{rcl5}"),
        &format!("\"5\":{}", rcl5 + 1),
        1,
    );
    fake.set("RAMPART_HITS_MAX", Some(&ramparts));
    // The crate has no level-0 spawn time; the engine lists 0 there (V131). A nonzero one is drift.
    assert_eq!(c::invader_core_creep_spawn_time(0), None);
    let spawn_time = fake
        .json_of("INVADER_CORE_CREEP_SPAWN_TIME")
        .replacen("\"0\":0", "\"0\":5", 1);
    fake.set("INVADER_CORE_CREEP_SPAWN_TIME", Some(&spawn_time));
    let r = report_of(&mut fake);
    assert_eq!(
        lines(&r),
        [
            // scalars
            format!(
                "SPAWN_HITS (mismatch): crate {}, server {hits}",
                c::SPAWN_HITS
            ),
            format!(
                "LINK_HITS_MAX via LINK_HITS (mismatch): crate {}, server {}",
                c::LINK_HITS,
                c::LINK_HITS + 1
            ),
            // terrain and density
            "MINERAL_DENSITY (mismatch): server an array, not an object".to_owned(),
            // parts
            "BODYPARTS_ALL (not in the crate): server \"wings\"".to_owned(),
            // structures: the family, initial hits, obstacles
            format!("STRUCTURE_* (missing on the server): crate \"{spawn}\""),
            "STRUCTURE_SPAWN (unreadable): server the text ends inside a value at byte 1"
                .to_owned(),
            format!(
                "SPAWN_HITS via initial_hits({spawn}) (mismatch): crate {}, server {hits}",
                Spawn.initial_hits().expect("a spawn has hits")
            ),
            format!("OBSTACLE_OBJECT_TYPES (missing on the server): crate \"{spawn}\""),
            // CONTROLLER_STRUCTURES: a missing row is not all zeros for the spawn
            format!("CONTROLLER_STRUCTURES.{spawn} (missing on the server)"),
            // the per-level tables and their second comparison
            format!(
                "RAMPART_HITS_MAX.5 (mismatch): crate {rcl5}, server {}",
                rcl5 + 1
            ),
            format!(
                "RAMPART_HITS_MAX_RCL5 via RAMPART_HITS_MAX (mismatch): crate {rcl5}, server {}",
                rcl5 + 1
            ),
            "INVADER_CORE_CREEP_SPAWN_TIME.0 (not in the crate): server 5".to_owned(),
        ]
    );
    // A missing row of zeros is not a finding: drop a row the crate has only zeros for.
    let mut fake = Fake::new(fixture());
    let lair = c::StructureType::KeeperLair;
    assert!((0..=8).all(|rcl| lair.controller_structures(rcl) == 0));
    let r = report_of(&mut fake);
    assert!(
        !fake
            .json_of("CONTROLLER_STRUCTURES")
            .contains(&format!("\"{lair}\""))
    );
    assert_eq!(r.finding_count(), 0);
}
