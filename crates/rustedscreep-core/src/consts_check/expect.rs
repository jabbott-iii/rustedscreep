//! What the crate says each server constant holds, as the check's comparison groups, and the list of
//! known differences.
//!
//! Sources: screeps-game-api 0.23.5 `src/constants` (through `rustedscreep_consts`), compared with
//! @screeps/common lib/constants.js; the engine's shapes and keys are those of common 2.16.0 (the
//! fixture) and the crate's representation choices are recorded in VERIFICATION_LOG (P1.11).
//!
//! Conventions:
//! - A crate constant compares with the server constant of the same name. Integers compare exactly,
//!   the crate's `f32` constants by rounding the server's double to `f32`, so a server change smaller
//!   than `f32` resolution (about 1e-7 relative) is not seen.
//! - Name maps for the crate's numeric enums use exhaustive matches, so a crate bump that adds a
//!   variant fails to compile here. The string enums are `#[non_exhaustive]` (wasm-bindgen adds a
//!   hidden `__Invalid` variant), so their lists are written out and the server comparison is the
//!   guard: a value the list lacks shows as "not in the crate".
//! - Name families (`ERR_*`, `FIND_*`, `STRUCTURE_*`, ...) are swept by prefix, so a server constant
//!   added to a family shows as "not in the crate" even when nothing names it.
//! - Tables compare entry by entry over the crate's key domain; a server key outside it is "not in
//!   the crate". Where the engine omits zero entries (`CONTROLLER_STRUCTURES`, `RAMPART_HITS_MAX`), a
//!   missing key equals a crate 0; where the crate answers `None` for a level the engine lists as 0
//!   (the invader-core tables), a server 0 equals the crate's `None`.

use rustedscreep_consts::{
    self as c, Boost, Color, Density, Direction, ErrorCode, ExitDirection, Find,
    IntershardResourceType, NaturalEffectType, Part, PowerCreepClass, PowerType, RESOURCES_ALL,
    StructureType, Terrain,
};

use super::json::{Json, Num};
use super::report::{FindingKind, Outcome, Report, Want, quote};
use super::server::{Const, Read, Server};

/// A difference the check expects, which it counts as known instead of reporting.
pub struct Known {
    pub kind: FindingKind,
    /// The finding's path.
    pub path: &'static str,
    /// The server value the finding quotes, when only that value is known.
    pub server: Option<&'static str>,
    pub reason: &'static str,
}

const fn known(kind: FindingKind, path: &'static str, reason: &'static str) -> Known {
    Known {
        kind,
        path,
        server: None,
        reason,
    }
}

const LOOK: &str = "the crate's Look strings are private (no Display) and the facade hides `look`; the bot reads no LOOK constant";
const ORDER: &str = "the crate's OrderType strings are private (no Display); the market (P9) checks them when it uses them";
const EVENT: &str = "the crate's event types live in its objects module (room.rs), outside the facade; checked when the bot reads the event log";

/// Server constants the crate does not mirror and gaps already recorded, each with its reason
/// (VERIFICATION_LOG V131 to V133, P1.11), by exact name, so a constant the server adds to a family
/// is still reported. Everything else the check finds is reported.
pub static KNOWN: [Known; 50] = [
    known(
        FindingKind::Unknown,
        "JSON",
        "the JavaScript built-in, not a game constant",
    ),
    known(FindingKind::Unknown, "LOOK_CREEPS", LOOK),
    known(FindingKind::Unknown, "LOOK_ENERGY", LOOK),
    known(FindingKind::Unknown, "LOOK_RESOURCES", LOOK),
    known(FindingKind::Unknown, "LOOK_SOURCES", LOOK),
    known(FindingKind::Unknown, "LOOK_MINERALS", LOOK),
    known(FindingKind::Unknown, "LOOK_DEPOSITS", LOOK),
    known(FindingKind::Unknown, "LOOK_STRUCTURES", LOOK),
    known(FindingKind::Unknown, "LOOK_FLAGS", LOOK),
    known(FindingKind::Unknown, "LOOK_CONSTRUCTION_SITES", LOOK),
    known(FindingKind::Unknown, "LOOK_NUKES", LOOK),
    known(FindingKind::Unknown, "LOOK_TERRAIN", LOOK),
    known(FindingKind::Unknown, "LOOK_TOMBSTONES", LOOK),
    known(FindingKind::Unknown, "LOOK_POWER_CREEPS", LOOK),
    known(FindingKind::Unknown, "LOOK_RUINS", LOOK),
    known(FindingKind::Unknown, "ORDER_SELL", ORDER),
    known(FindingKind::Unknown, "ORDER_BUY", ORDER),
    known(FindingKind::Unknown, "EVENT_ATTACK", EVENT),
    known(FindingKind::Unknown, "EVENT_OBJECT_DESTROYED", EVENT),
    known(FindingKind::Unknown, "EVENT_ATTACK_CONTROLLER", EVENT),
    known(FindingKind::Unknown, "EVENT_BUILD", EVENT),
    known(FindingKind::Unknown, "EVENT_HARVEST", EVENT),
    known(FindingKind::Unknown, "EVENT_HEAL", EVENT),
    known(FindingKind::Unknown, "EVENT_REPAIR", EVENT),
    known(FindingKind::Unknown, "EVENT_RESERVE_CONTROLLER", EVENT),
    known(FindingKind::Unknown, "EVENT_UPGRADE_CONTROLLER", EVENT),
    known(FindingKind::Unknown, "EVENT_EXIT", EVENT),
    known(FindingKind::Unknown, "EVENT_POWER", EVENT),
    known(FindingKind::Unknown, "EVENT_TRANSFER", EVENT),
    known(FindingKind::Unknown, "EVENT_ATTACK_TYPE_MELEE", EVENT),
    known(FindingKind::Unknown, "EVENT_ATTACK_TYPE_RANGED", EVENT),
    known(FindingKind::Unknown, "EVENT_ATTACK_TYPE_RANGED_MASS", EVENT),
    known(FindingKind::Unknown, "EVENT_ATTACK_TYPE_DISMANTLE", EVENT),
    known(FindingKind::Unknown, "EVENT_ATTACK_TYPE_HIT_BACK", EVENT),
    known(FindingKind::Unknown, "EVENT_ATTACK_TYPE_NUKE", EVENT),
    known(FindingKind::Unknown, "EVENT_HEAL_TYPE_MELEE", EVENT),
    known(FindingKind::Unknown, "EVENT_HEAL_TYPE_RANGED", EVENT),
    known(
        FindingKind::Unknown,
        "POWER_INFO",
        "not implemented in the crate (numbers.rs:819)",
    ),
    known(
        FindingKind::Unknown,
        "LAB_COOLDOWN",
        "omitted by the crate on purpose (numbers.rs:491); the engine marks it unused",
    ),
    known(
        FindingKind::Unknown,
        "WORLD_WIDTH",
        "deprecated (Game.map.getWorldSize() replaces it)",
    ),
    known(
        FindingKind::Unknown,
        "WORLD_HEIGHT",
        "deprecated (Game.map.getWorldSize() replaces it)",
    ),
    known(FindingKind::Unknown, "SIGN_NOVICE_AREA", "deprecated"),
    known(FindingKind::Unknown, "SIGN_RESPAWN_AREA", "deprecated"),
    known(
        FindingKind::Unknown,
        "MODE_SIMULATION",
        "null, and unused by the engine",
    ),
    known(
        FindingKind::Unknown,
        "MODE_WORLD",
        "null, and unused by the engine",
    ),
    known(
        FindingKind::Unknown,
        "SUBSCRIPTION_TOKEN",
        "omitted by the crate on purpose (types.rs:225, no longer used)",
    ),
    Known {
        kind: FindingKind::Extra,
        path: "INTERSHARD_RESOURCES",
        server: Some("\"token\""),
        reason: "omitted by the crate on purpose (types.rs:225, no longer used)",
    },
    known(
        FindingKind::Extra,
        "TERRAIN_MASK_LAVA",
        "omitted by the crate on purpose (small_enums.rs:442, unimplemented in game)",
    ),
    known(
        FindingKind::Extra,
        "PWR_DISRUPT_SOURCE",
        "the crate's PowerType skips 11 and its conversions panic on it (VERIFICATION_LOG V132); read no power effects before it is fixed",
    ),
    known(
        FindingKind::Missing,
        "ERR_ACCESS_DENIED",
        "the engine added it in common 49c0fc6 (2026-04-19); common 2.16.0 lacks it",
    ),
];

/// The known difference a finding is, if any.
fn known_index(kind: FindingKind, path: &str, server: &str) -> Option<u16> {
    KNOWN
        .iter()
        .position(|k| k.kind == kind && k.path == path && k.server.is_none_or(|s| s == server))
        .and_then(|i| u16::try_from(i).ok())
}

/// The comparison groups, one per step.
type Group = fn(&mut Ctx<'_>);

static GROUPS: [Group; 15] = [
    scalars,
    return_codes,
    directions_and_colors,
    find_codes,
    terrain_and_density,
    powers,
    parts,
    structures,
    controller_structures,
    resources,
    controller_tables,
    invader_tables,
    reactions,
    boosts,
    commodities,
];

pub fn group_count() -> u32 {
    u32::try_from(GROUPS.len()).unwrap_or(u32::MAX)
}

/// Runs comparison group `i` (nothing past the last).
pub fn run_group(i: u32, server: &Server, report: &mut Report) {
    if let Some(group) = usize::try_from(i).ok().and_then(|i| GROUPS.get(i)) {
        group(&mut Ctx {
            s: server,
            r: report,
        });
    }
}

/// After every group: each server constant nothing read is unknown, unless it is a known one.
pub fn finish(server: &Server, report: &mut Report) {
    let mut x = Ctx {
        s: server,
        r: report,
    };
    for c in server.unread() {
        let quoted = match &c.read {
            Read::Json(v) => quote(v, &c.text),
            Read::NoJson => "no JSON value".to_owned(),
            Read::Bad(e) => e.to_string(),
        };
        x.finding(FindingKind::Unknown, At::Name(&c.name), None, quoted);
    }
}

/// How a table treats entries the two sides leave out.
#[derive(Clone, Copy, Default)]
struct Rule {
    /// A key the server lacks equals a crate 0.
    absent_is_zero: bool,
    /// A server 0 equals a crate `None`.
    zero_is_none: bool,
}

const ABSENT_IS_ZERO: Rule = Rule {
    absent_is_zero: true,
    zero_is_none: false,
};

const ZERO_IS_NONE: Rule = Rule {
    absent_is_zero: false,
    zero_is_none: true,
};

/// Entries of one table level: each key with the crate's value, `None` where the crate has none.
/// Keys borrow from static text or from a name list made once per group.
type Entries<'k> = Vec<(&'k str, Option<Want>)>;

/// The keys of the per-level tables (controller levels 0 to 8, densities 1 to 4, core levels 0 to 5).
const KEYS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// Entries keyed `0..=last` (at most 9) from `f`.
fn numbered(last: u32, f: &dyn Fn(u32) -> Option<Want>) -> Entries<'static> {
    (0..=last)
        .filter_map(|k| Some((*KEYS.get(usize::try_from(k).ok()?)?, f(k))))
        .collect()
}

fn is_zero(v: &Json) -> bool {
    matches!(v, Json::Num(Num { value: Some(x), .. }) if *x == 0.0)
}

/// Where a finding is: built into text only when there is a finding.
#[derive(Clone, Copy)]
enum At<'p> {
    /// A constant (or a whole table path).
    Name(&'p str),
    /// An entry of a table: `path.key`.
    Key(&'p str, &'p str),
    /// A constant compared again for another crate item: `NAME via item`.
    Via(&'p str, &'p str),
}

impl At<'_> {
    fn render(self) -> String {
        match self {
            Self::Name(name) => name.to_owned(),
            Self::Key(path, key) => format!("{path}.{key}"),
            Self::Via(name, via) => format!("{name} via {via}"),
        }
    }
}

/// A top-level constant as read.
enum Top<'a> {
    Value(&'a Json, &'a str),
    Missing,
    /// Reported already.
    Unreadable,
}

struct Ctx<'a> {
    s: &'a Server,
    r: &'a mut Report,
}

// The helpers are called from many places with small arguments; kept out of line, each exists once in
// the wasm (inlined, the groups held over 100 KB of copies, P1.11).
impl<'a> Ctx<'a> {
    #[inline(never)]
    fn finding(&mut self, kind: FindingKind, at: At<'_>, expected: Option<&Want>, server: String) {
        let path = at.render();
        match known_index(kind, &path, &server) {
            Some(i) => self.r.add_known(i),
            None => self.r.add(kind, path, expected.cloned(), server),
        }
    }

    fn matched(&mut self) {
        self.r.compared = self.r.compared.saturating_add(1);
        self.r.matched = self.r.matched.saturating_add(1);
    }

    #[inline(never)]
    fn missing(&mut self, at: At<'_>, want: Option<&Want>) {
        self.r.compared = self.r.compared.saturating_add(1);
        self.finding(FindingKind::Missing, at, want, String::new());
    }

    #[inline(never)]
    fn extra(&mut self, at: At<'_>, found: &Json, text: &str) {
        self.finding(FindingKind::Extra, at, None, quote(found, text));
    }

    #[inline(never)]
    fn no_json(&mut self, name: &str) {
        self.finding(
            FindingKind::Unreadable,
            At::Name(name),
            None,
            "no JSON value".to_owned(),
        );
    }

    /// A value of the wrong shape (an array where the crate has an object, ...).
    #[inline(never)]
    fn shape(&mut self, path: &str, expected: &str, found: &Json) {
        self.r.compared = self.r.compared.saturating_add(1);
        let server = format!("{}, not {expected}", found.kind());
        self.finding(FindingKind::Mismatch, At::Name(path), None, server);
    }

    /// Compares a value the server has.
    #[inline(never)]
    fn compare(&mut self, at: At<'_>, want: &Want, found: &Json, text: &str) {
        self.r.compared = self.r.compared.saturating_add(1);
        let kind = match want.compare(found) {
            Outcome::Match => {
                self.r.matched = self.r.matched.saturating_add(1);
                return;
            }
            Outcome::Mismatch => FindingKind::Mismatch,
            Outcome::Unreadable => FindingKind::Unreadable,
        };
        self.finding(kind, at, Some(want), quote(found, text));
    }

    /// Reads the top-level constant `name` (marking it read) and reports it if unreadable.
    #[inline(never)]
    fn top(&mut self, name: &str) -> Top<'a> {
        let s: &'a Server = self.s;
        let Some(c) = s.take(name) else {
            return Top::Missing;
        };
        match &c.read {
            Read::Json(v) => Top::Value(v, c.text.as_str()),
            Read::NoJson => {
                self.no_json(name);
                Top::Unreadable
            }
            Read::Bad(e) => {
                let why = e.to_string();
                self.finding(FindingKind::Unreadable, At::Name(name), None, why);
                Top::Unreadable
            }
        }
    }

    /// A top-level constant that must be there, or `None` once it is reported missing or unreadable.
    fn required(&mut self, name: &str) -> Option<(&'a Json, &'a str)> {
        match self.top(name) {
            Top::Value(v, text) => Some((v, text)),
            Top::Missing => {
                self.missing(At::Name(name), None);
                None
            }
            Top::Unreadable => None,
        }
    }

    /// A scalar constant.
    #[inline(never)]
    fn scalar(&mut self, name: &str, want: &Want) {
        match self.top(name) {
            Top::Value(v, text) => self.compare(At::Name(name), want, v, text),
            Top::Missing => self.missing(At::Name(name), Some(want)),
            Top::Unreadable => {}
        }
    }

    /// A scalar constant the crate gives under another name (`via`).
    #[inline(never)]
    fn scalar_via(&mut self, name: &str, via: &str, want: &Want) {
        match self.top(name) {
            Top::Value(v, text) => self.compare(At::Via(name, via), want, v, text),
            Top::Missing => self.missing(At::Via(name, via), Some(want)),
            Top::Unreadable => {}
        }
    }

    /// The finding for a member of a family whose value did not read as JSON.
    fn unreadable_member(&mut self, c: &Const) {
        match &c.read {
            Read::Bad(e) => {
                let why = e.to_string();
                self.finding(FindingKind::Unreadable, At::Name(&c.name), None, why);
            }
            _ => self.no_json(&c.name),
        }
    }

    /// A second comparison against a constant another check reads, for a crate item that gives the
    /// same value another way (`via`). A missing or unreadable constant is reported by that check.
    #[inline(never)]
    fn also(&mut self, name: &str, via: &str, want: &Want) {
        let s: &'a Server = self.s;
        if let Some(c) = s.take(name)
            && let Read::Json(v) = &c.read
        {
            self.compare(At::Via(name, via), want, v, &c.text);
        }
    }

    /// One more comparison of the entry `key` of the table `table` (read by another check), for the
    /// crate constant `name`.
    #[inline(never)]
    fn entry(&mut self, table: &str, key: &str, name: &str, want: &Want) {
        let s: &'a Server = self.s;
        if let Some(c) = s.take(table)
            && let Read::Json(v) = &c.read
        {
            match v.get(key) {
                Some(found) => self.compare(At::Via(name, table), want, found, &c.text),
                None => self.missing(At::Via(name, table), Some(want)),
            }
        }
    }

    /// A family of scalars named `prefix`*: the `named` ones compare by name, any other is not in
    /// the crate.
    #[inline(never)]
    fn family(&mut self, prefix: &str, named: &[(&str, Want)]) {
        for (name, want) in named {
            self.scalar(name, want);
        }
        let s: &'a Server = self.s;
        for c in s.take_prefix(prefix) {
            if named.iter().any(|(n, _)| *n == c.name) {
                continue;
            }
            match &c.read {
                Read::Json(v) => self.extra(At::Name(&c.name), v, &c.text),
                _ => self.unreadable_member(c),
            }
        }
    }

    /// A family named `prefix`* compared as a set of values: each crate value no member holds is
    /// missing, each member whose value the crate lacks is not in the crate.
    #[inline(never)]
    fn family_set(&mut self, prefix: &str, values: &[Want]) {
        let s: &'a Server = self.s;
        let members: Vec<_> = s.take_prefix(prefix).collect();
        let family = format!("{prefix}*");
        for want in values {
            if members
                .iter()
                .any(|c| c.json().is_some_and(|v| want.compare(v) == Outcome::Match))
            {
                self.matched();
            } else {
                self.missing(At::Name(&family), Some(want));
            }
        }
        for c in members {
            match c.json() {
                Some(v) if values.iter().all(|w| w.compare(v) != Outcome::Match) => {
                    self.extra(At::Name(&c.name), v, &c.text);
                }
                Some(_) => {}
                None => self.unreadable_member(c),
            }
        }
    }

    /// An array constant compared as a set (order ignored).
    #[inline(never)]
    fn set(&mut self, name: &str, values: &[Want]) {
        let Some((v, text)) = self.required(name) else {
            return;
        };
        let Some(items) = v.elements() else {
            return self.shape(name, "an array", v);
        };
        for want in values {
            if items.iter().any(|e| want.compare(e) == Outcome::Match) {
                self.matched();
            } else {
                self.missing(At::Name(name), Some(want));
            }
        }
        for e in items {
            if values.iter().all(|w| w.compare(e) != Outcome::Match) {
                self.extra(At::Name(name), e, text);
            }
        }
    }

    /// One table level: `v` (at `path`) against `want`. Keys in `skip` are compared by the caller.
    #[inline(never)]
    fn entries(
        &mut self,
        path: &str,
        v: &Json,
        text: &str,
        want: &[(&str, Option<Want>)],
        rule: Rule,
        skip: &[&str],
    ) {
        let Some(members) = v.members() else {
            return self.shape(path, "an object", v);
        };
        for (key, w) in want {
            match (v.get(key), w) {
                (Some(found), Some(w)) => self.compare(At::Key(path, key), w, found, text),
                (Some(found), None) if rule.zero_is_none && is_zero(found) => self.matched(),
                (Some(found), None) => self.extra(At::Key(path, key), found, text),
                (None, Some(Want::Int(0))) if rule.absent_is_zero => self.matched(),
                (None, Some(w)) => self.missing(At::Key(path, key), Some(w)),
                (None, None) => {}
            }
        }
        for (key, found) in members {
            if !skip.contains(&key.as_str()) && !want.iter().any(|(k, _)| k == key) {
                self.extra(At::Key(path, key), found, text);
            }
        }
    }

    /// A one-level table constant.
    #[inline(never)]
    fn table(&mut self, name: &str, want: &[(&str, Option<Want>)], rule: Rule) {
        if let Some((v, text)) = self.required(name) {
            self.entries(name, v, text, want, rule, &[]);
        }
    }

    /// A two-level table level: `v` (at `path`) maps each outer key to a one-level table.
    #[inline(never)]
    fn nested(
        &mut self,
        path: &str,
        v: &Json,
        text: &str,
        want: &[(&str, Entries<'_>)],
        rule: Rule,
    ) {
        let Some(members) = v.members() else {
            return self.shape(path, "an object", v);
        };
        for (key, inner) in want {
            match v.get(key) {
                Some(found) => {
                    let inner_path = format!("{path}.{key}");
                    self.entries(&inner_path, found, text, inner, rule, &[]);
                }
                // A missing row is all zeros under ABSENT_IS_ZERO.
                None if rule.absent_is_zero
                    && inner
                        .iter()
                        .all(|(_, w)| matches!(w, None | Some(Want::Int(0)))) =>
                {
                    for _ in inner.iter().filter(|(_, w)| w.is_some()) {
                        self.matched();
                    }
                }
                None => self.missing(At::Key(path, key), None),
            }
        }
        for (key, found) in members {
            if !want.iter().any(|(k, _)| k == key) {
                self.extra(At::Key(path, key), found, text);
            }
        }
    }
}

/// Adds `(outer, inner, value)` to a two-level expectation, keeping first-seen order.
fn put<'k>(want: &mut Vec<(&'k str, Entries<'k>)>, outer: &'k str, inner: &'k str, value: Want) {
    let row = match want.iter().position(|(k, _)| *k == outer) {
        Some(i) => want.get_mut(i),
        None => {
            want.push((outer, Vec::new()));
            want.last_mut()
        }
    };
    if let Some((_, entries)) = row {
        entries.push((inner, Some(value)));
    }
}

/// The display names of `items`, made once per group so table keys can borrow them.
fn display_names<T: ToString>(items: &[T]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

macro_rules! scalars {
    ($x:ident; $($name:ident),* $(,)?) => {
        $( $x.scalar(stringify!($name), &Want::from(c::$name)); )*
    };
}

/// Every crate constant whose engine counterpart is a scalar of the same name (screeps-game-api
/// 0.23.5 numbers.rs; RAMPART_HITS_MAX_RCL* and NUKE_DAMAGE_RANGE_* are table entries, compared in
/// [`controller_tables`]). The crate's extra.rs values have no engine counterpart.
fn scalars(x: &mut Ctx<'_>) {
    scalars!(x;
        CREEP_LIFE_TIME, CREEP_CLAIM_LIFE_TIME, CREEP_CORPSE_RATE, CREEP_PART_MAX_ENERGY,
        CARRY_CAPACITY, HARVEST_POWER, HARVEST_MINERAL_POWER, HARVEST_DEPOSIT_POWER, REPAIR_POWER,
        DISMANTLE_POWER, BUILD_POWER, ATTACK_POWER, UPGRADE_CONTROLLER_POWER, RANGED_ATTACK_POWER,
        HEAL_POWER, RANGED_HEAL_POWER, REPAIR_COST, DISMANTLE_COST, RAMPART_DECAY_AMOUNT,
        RAMPART_DECAY_TIME, RAMPART_HITS, ENERGY_REGEN_TIME, ENERGY_DECAY, SPAWN_HITS,
        SPAWN_ENERGY_START, SPAWN_ENERGY_CAPACITY, CREEP_SPAWN_TIME, SPAWN_RENEW_RATIO,
        SOURCE_ENERGY_CAPACITY, SOURCE_ENERGY_NEUTRAL_CAPACITY, SOURCE_ENERGY_KEEPER_CAPACITY,
        WALL_HITS, WALL_HITS_MAX, EXTENSION_HITS, ROAD_HITS, ROAD_WEAROUT, ROAD_WEAROUT_POWER_CREEP,
        ROAD_DECAY_AMOUNT, ROAD_DECAY_TIME, LINK_HITS, LINK_CAPACITY, LINK_COOLDOWN, LINK_LOSS_RATIO,
        STORAGE_CAPACITY, STORAGE_HITS, CONSTRUCTION_COST_ROAD_SWAMP_RATIO,
        CONSTRUCTION_COST_ROAD_WALL_RATIO, CONTROLLER_DOWNGRADE_RESTORE,
        CONTROLLER_DOWNGRADE_SAFEMODE_THRESHOLD, CONTROLLER_CLAIM_DOWNGRADE, CONTROLLER_RESERVE,
        CONTROLLER_RESERVE_MAX, CONTROLLER_MAX_UPGRADE_PER_TICK, CONTROLLER_ATTACK_BLOCKED_UPGRADE,
        CONTROLLER_NUKE_BLOCKED_UPGRADE, SAFE_MODE_DURATION, SAFE_MODE_COOLDOWN, SAFE_MODE_COST,
        TOWER_HITS, TOWER_CAPACITY, TOWER_ENERGY_COST, TOWER_POWER_ATTACK, TOWER_POWER_HEAL,
        TOWER_POWER_REPAIR, TOWER_OPTIMAL_RANGE, TOWER_FALLOFF_RANGE, TOWER_FALLOFF, OBSERVER_HITS,
        OBSERVER_RANGE, POWER_BANK_HITS, POWER_BANK_CAPACITY_MAX, POWER_BANK_CAPACITY_MIN,
        POWER_BANK_CAPACITY_CRIT, POWER_BANK_DECAY, POWER_BANK_HIT_BACK, POWER_SPAWN_HITS,
        POWER_SPAWN_ENERGY_CAPACITY, POWER_SPAWN_POWER_CAPACITY, POWER_SPAWN_ENERGY_RATIO,
        EXTRACTOR_HITS, EXTRACTOR_COOLDOWN, LAB_HITS, LAB_MINERAL_CAPACITY, LAB_ENERGY_CAPACITY,
        LAB_BOOST_ENERGY, LAB_BOOST_MINERAL, LAB_REACTION_AMOUNT, LAB_UNBOOST_ENERGY,
        LAB_UNBOOST_MINERAL, GCL_POW, GCL_MULTIPLY, GCL_NOVICE, MAX_CONSTRUCTION_SITES,
        MAX_CREEP_SIZE, MINERAL_REGEN_TIME, MINERAL_RANDOM_FACTOR, MINERAL_DENSITY_CHANGE,
        DEPOSIT_EXHAUST_MULTIPLY, DEPOSIT_EXHAUST_POW, DEPOSIT_DECAY_TIME, TERMINAL_HITS,
        TERMINAL_CAPACITY, TERMINAL_SEND_COST, TERMINAL_MIN_SEND, TERMINAL_COOLDOWN, CONTAINER_HITS,
        CONTAINER_CAPACITY, CONTAINER_DECAY, CONTAINER_DECAY_TIME, CONTAINER_DECAY_TIME_OWNED,
        NUKER_HITS, NUKER_COOLDOWN, NUKER_ENERGY_CAPACITY, NUKER_GHODIUM_CAPACITY, NUKE_LAND_TIME,
        NUKE_RANGE, FACTORY_HITS, FACTORY_CAPACITY, TOMBSTONE_DECAY_PER_PART,
        TOMBSTONE_DECAY_POWER_CREEP, RUIN_DECAY, PORTAL_DECAY, MARKET_FEE, MARKET_MAX_ORDERS,
        MARKET_ORDER_LIFE_TIME, FLAGS_LIMIT, PIXEL_CPU_COST, PORTAL_UNSTABLE, PORTAL_MIN_TIMEOUT,
        PORTAL_MAX_TIMEOUT, POWER_BANK_RESPAWN_TIME, INVADERS_ENERGY_GOAL, SYSTEM_USERNAME,
        SIGN_PLANNED_AREA, POWER_LEVEL_MULTIPLY, POWER_LEVEL_POW, POWER_CREEP_SPAWN_COOLDOWN,
        POWER_CREEP_DELETE_COOLDOWN, POWER_CREEP_MAX_LEVEL, POWER_CREEP_LIFE_TIME, INVADER_CORE_HITS,
        INVADER_CORE_CONTROLLER_POWER, INVADER_CORE_CONTROLLER_DOWNGRADE, STRONGHOLD_DECAY_TICKS,
    );
    // The engine sets a new link's hits from LINK_HITS and its hitsMax from LINK_HITS_MAX (engine 4.3.0
    // processor/intents/creeps/build.js:146-147); the crate has only LINK_HITS for both.
    x.scalar_via("LINK_HITS_MAX", "LINK_HITS", &Want::from(c::LINK_HITS));
}

/// Every `ErrorCode` variant.
const ERRORS: [ErrorCode; 15] = [
    ErrorCode::NotOwner,
    ErrorCode::NoPath,
    ErrorCode::NameExists,
    ErrorCode::Busy,
    ErrorCode::NotFound,
    ErrorCode::NotEnough,
    ErrorCode::InvalidTarget,
    ErrorCode::Full,
    ErrorCode::NotInRange,
    ErrorCode::InvalidArgs,
    ErrorCode::Tired,
    ErrorCode::NoBodypart,
    ErrorCode::RclNotEnough,
    ErrorCode::GclNotEnough,
    ErrorCode::AccessDenied,
];

/// The engine's name for each code (exhaustive: a new crate variant fails to compile here).
const fn error_name(e: ErrorCode) -> &'static str {
    match e {
        ErrorCode::NotOwner => "ERR_NOT_OWNER",
        ErrorCode::NoPath => "ERR_NO_PATH",
        ErrorCode::NameExists => "ERR_NAME_EXISTS",
        ErrorCode::Busy => "ERR_BUSY",
        ErrorCode::NotFound => "ERR_NOT_FOUND",
        ErrorCode::NotEnough => "ERR_NOT_ENOUGH_ENERGY",
        ErrorCode::InvalidTarget => "ERR_INVALID_TARGET",
        ErrorCode::Full => "ERR_FULL",
        ErrorCode::NotInRange => "ERR_NOT_IN_RANGE",
        ErrorCode::InvalidArgs => "ERR_INVALID_ARGS",
        ErrorCode::Tired => "ERR_TIRED",
        ErrorCode::NoBodypart => "ERR_NO_BODYPART",
        ErrorCode::RclNotEnough => "ERR_RCL_NOT_ENOUGH",
        ErrorCode::GclNotEnough => "ERR_GCL_NOT_ENOUGH",
        ErrorCode::AccessDenied => "ERR_ACCESS_DENIED",
    }
}

/// `OK` and the `ERR_*` family. A server code the crate lacks matters: the crate's per-method error
/// enums unwrap the code conversion, so an unknown code would trap (VERIFICATION_LOG, P1.11).
fn return_codes(x: &mut Ctx<'_>) {
    // The crate's return-code conversion maps 0 to Ok (screeps-game-api 0.23.5 small_enums.rs:49);
    // ErrorCode has no variant for it.
    x.scalar("OK", &Want::Int(0));
    let mut named: Vec<(&str, Want)> = ERRORS
        .iter()
        .map(|&e| (error_name(e), Want::from(e as i8)))
        .collect();
    // The engine has three names for -6 (constants.js:7-9), all ErrorCode::NotEnough.
    for alias in ["ERR_NOT_ENOUGH_RESOURCES", "ERR_NOT_ENOUGH_EXTENSIONS"] {
        named.push((alias, Want::from(ErrorCode::NotEnough as i8)));
    }
    x.family("ERR_", &named);
}

const fn direction_name(d: Direction) -> &'static str {
    match d {
        Direction::Top => "TOP",
        Direction::TopRight => "TOP_RIGHT",
        Direction::Right => "RIGHT",
        Direction::BottomRight => "BOTTOM_RIGHT",
        Direction::Bottom => "BOTTOM",
        Direction::BottomLeft => "BOTTOM_LEFT",
        Direction::Left => "LEFT",
        Direction::TopLeft => "TOP_LEFT",
    }
}

const COLORS: [Color; 10] = [
    Color::Red,
    Color::Purple,
    Color::Blue,
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Orange,
    Color::Brown,
    Color::Grey,
    Color::White,
];

const fn color_name(col: Color) -> &'static str {
    match col {
        Color::Red => "COLOR_RED",
        Color::Purple => "COLOR_PURPLE",
        Color::Blue => "COLOR_BLUE",
        Color::Cyan => "COLOR_CYAN",
        Color::Green => "COLOR_GREEN",
        Color::Yellow => "COLOR_YELLOW",
        Color::Orange => "COLOR_ORANGE",
        Color::Brown => "COLOR_BROWN",
        Color::Grey => "COLOR_GREY",
        Color::White => "COLOR_WHITE",
    }
}

fn directions_and_colors(x: &mut Ctx<'_>) {
    for &d in Direction::iter() {
        x.scalar(direction_name(d), &Want::from(d as u8));
    }
    let named: Vec<(&str, Want)> = COLORS
        .iter()
        .map(|&col| (color_name(col), Want::from(col as u8)))
        .collect();
    x.family("COLOR_", &named);
    let all: Vec<Want> = COLORS.iter().map(|&col| Want::from(col as u8)).collect();
    x.set("COLORS_ALL", &all);
}

/// Every `Find` variant the MMO has (the seasonal ones exist in every build, find.rs:70-82).
const FINDS: [Find; 28] = [
    Find::ExitTop,
    Find::ExitRight,
    Find::ExitBottom,
    Find::ExitLeft,
    Find::Exit,
    Find::Creeps,
    Find::MyCreeps,
    Find::HostileCreeps,
    Find::SourcesActive,
    Find::Sources,
    Find::DroppedResources,
    Find::Structures,
    Find::MyStructures,
    Find::HostileStructures,
    Find::Flags,
    Find::ConstructionSites,
    Find::MySpawns,
    Find::HostileSpawns,
    Find::MyConstructionSites,
    Find::HostileConstructionSites,
    Find::Minerals,
    Find::Nukes,
    Find::Tombstones,
    Find::PowerCreeps,
    Find::MyPowerCreeps,
    Find::HostilePowerCreeps,
    Find::Deposits,
    Find::Ruins,
];

/// The engine's name for each find code; `None` for the seasonal codes (no MMO global).
const fn find_name(f: Find) -> Option<&'static str> {
    Some(match f {
        Find::ExitTop => "FIND_EXIT_TOP",
        Find::ExitRight => "FIND_EXIT_RIGHT",
        Find::ExitBottom => "FIND_EXIT_BOTTOM",
        Find::ExitLeft => "FIND_EXIT_LEFT",
        Find::Exit => "FIND_EXIT",
        Find::Creeps => "FIND_CREEPS",
        Find::MyCreeps => "FIND_MY_CREEPS",
        Find::HostileCreeps => "FIND_HOSTILE_CREEPS",
        Find::SourcesActive => "FIND_SOURCES_ACTIVE",
        Find::Sources => "FIND_SOURCES",
        Find::DroppedResources => "FIND_DROPPED_RESOURCES",
        Find::Structures => "FIND_STRUCTURES",
        Find::MyStructures => "FIND_MY_STRUCTURES",
        Find::HostileStructures => "FIND_HOSTILE_STRUCTURES",
        Find::Flags => "FIND_FLAGS",
        Find::ConstructionSites => "FIND_CONSTRUCTION_SITES",
        Find::MySpawns => "FIND_MY_SPAWNS",
        Find::HostileSpawns => "FIND_HOSTILE_SPAWNS",
        Find::MyConstructionSites => "FIND_MY_CONSTRUCTION_SITES",
        Find::HostileConstructionSites => "FIND_HOSTILE_CONSTRUCTION_SITES",
        Find::Minerals => "FIND_MINERALS",
        Find::Nukes => "FIND_NUKES",
        Find::Tombstones => "FIND_TOMBSTONES",
        Find::PowerCreeps => "FIND_POWER_CREEPS",
        Find::MyPowerCreeps => "FIND_MY_POWER_CREEPS",
        Find::HostilePowerCreeps => "FIND_HOSTILE_POWER_CREEPS",
        Find::Deposits => "FIND_DEPOSITS",
        Find::Ruins => "FIND_RUINS",
        Find::ScoreContainers
        | Find::ScoreCollectors
        | Find::SymbolContainers
        | Find::SymbolDecoders
        | Find::Reactors
        | Find::Score => return None,
    })
}

const fn exit_name(e: ExitDirection) -> &'static str {
    match e {
        ExitDirection::Top => "FIND_EXIT_TOP",
        ExitDirection::Right => "FIND_EXIT_RIGHT",
        ExitDirection::Bottom => "FIND_EXIT_BOTTOM",
        ExitDirection::Left => "FIND_EXIT_LEFT",
    }
}

/// The `FIND_*` family (the snapshot passes `FIND_HOSTILE_CREEPS` through the crate), and
/// `ExitDirection`, which gives the four exit codes again.
fn find_codes(x: &mut Ctx<'_>) {
    let named: Vec<(&str, Want)> = FINDS
        .iter()
        .filter_map(|&f| find_name(f).map(|name| (name, Want::from(f as u16))))
        .collect();
    x.family("FIND_", &named);
    for e in [
        ExitDirection::Top,
        ExitDirection::Right,
        ExitDirection::Bottom,
        ExitDirection::Left,
    ] {
        x.also(exit_name(e), "ExitDirection", &Want::from(e as u8));
    }
}

const DENSITIES: [Density; 4] = [
    Density::Low,
    Density::Moderate,
    Density::High,
    Density::Ultra,
];

const fn density_name(d: Density) -> &'static str {
    match d {
        Density::Low => "DENSITY_LOW",
        Density::Moderate => "DENSITY_MODERATE",
        Density::High => "DENSITY_HIGH",
        Density::Ultra => "DENSITY_ULTRA",
    }
}

/// The engine's name for each terrain mask; Plain has none (exhaustive).
const fn terrain_name(t: Terrain) -> Option<&'static str> {
    match t {
        Terrain::Plain => None,
        Terrain::Wall => Some("TERRAIN_MASK_WALL"),
        Terrain::Swamp => Some("TERRAIN_MASK_SWAMP"),
    }
}

/// `TERRAIN_MASK_*`, `DENSITY_*`, `MINERAL_DENSITY` and its probabilities.
fn terrain_and_density(x: &mut Ctx<'_>) {
    let named: Vec<(&str, Want)> = [Terrain::Plain, Terrain::Wall, Terrain::Swamp]
        .into_iter()
        .filter_map(|t| terrain_name(t).map(|n| (n, Want::from(t as u8))))
        .collect();
    x.family("TERRAIN_MASK_", &named);

    let named: Vec<(&str, Want)> = DENSITIES
        .iter()
        .map(|&d| (density_name(d), Want::from(d as u8)))
        .collect();
    x.family("DENSITY_", &named);
    let key = |d: Density| KEYS.get(usize::from(d as u8)).copied().unwrap_or("?");
    let amounts: Entries<'_> = DENSITIES
        .iter()
        .map(|&d| (key(d), Some(Want::from(d.amount()))))
        .collect();
    x.table("MINERAL_DENSITY", &amounts, Rule::default());
    let odds: Entries<'_> = DENSITIES
        .iter()
        .map(|&d| (key(d), Some(Want::from(d.probability()))))
        .collect();
    x.table("MINERAL_DENSITY_PROBABILITY", &odds, Rule::default());
}

const POWERS: [PowerType; 18] = [
    PowerType::GenerateOps,
    PowerType::OperateSpawn,
    PowerType::OperateTower,
    PowerType::OperateStorage,
    PowerType::OperateLab,
    PowerType::OperateExtension,
    PowerType::OperateObserver,
    PowerType::OperateTerminal,
    PowerType::DisruptSpawn,
    PowerType::DisruptTower,
    PowerType::Shield,
    PowerType::RegenSource,
    PowerType::RegenMineral,
    PowerType::DisruptTerminal,
    PowerType::OperatePower,
    PowerType::Fortify,
    PowerType::OperateController,
    PowerType::OperateFactory,
];

const fn power_name(p: PowerType) -> &'static str {
    match p {
        PowerType::GenerateOps => "PWR_GENERATE_OPS",
        PowerType::OperateSpawn => "PWR_OPERATE_SPAWN",
        PowerType::OperateTower => "PWR_OPERATE_TOWER",
        PowerType::OperateStorage => "PWR_OPERATE_STORAGE",
        PowerType::OperateLab => "PWR_OPERATE_LAB",
        PowerType::OperateExtension => "PWR_OPERATE_EXTENSION",
        PowerType::OperateObserver => "PWR_OPERATE_OBSERVER",
        PowerType::OperateTerminal => "PWR_OPERATE_TERMINAL",
        PowerType::DisruptSpawn => "PWR_DISRUPT_SPAWN",
        PowerType::DisruptTower => "PWR_DISRUPT_TOWER",
        PowerType::Shield => "PWR_SHIELD",
        PowerType::RegenSource => "PWR_REGEN_SOURCE",
        PowerType::RegenMineral => "PWR_REGEN_MINERAL",
        PowerType::DisruptTerminal => "PWR_DISRUPT_TERMINAL",
        PowerType::OperatePower => "PWR_OPERATE_POWER",
        PowerType::Fortify => "PWR_FORTIFY",
        PowerType::OperateController => "PWR_OPERATE_CONTROLLER",
        PowerType::OperateFactory => "PWR_OPERATE_FACTORY",
    }
}

const EFFECTS: [NaturalEffectType; 2] = [
    NaturalEffectType::Invulnerability,
    NaturalEffectType::CollapseTimer,
];

const fn effect_name(e: NaturalEffectType) -> &'static str {
    match e {
        NaturalEffectType::Invulnerability => "EFFECT_INVULNERABILITY",
        NaturalEffectType::CollapseTimer => "EFFECT_COLLAPSE_TIMER",
    }
}

/// `PWR_*`, `EFFECT_*` and `POWER_CLASS` (whose key `OPERATOR` is the engine's, constants.js:814).
fn powers(x: &mut Ctx<'_>) {
    let named: Vec<(&str, Want)> = POWERS
        .iter()
        .map(|&p| (power_name(p), Want::from(p as u32)))
        .collect();
    x.family("PWR_", &named);
    let named: Vec<(&str, Want)> = EFFECTS
        .iter()
        .map(|&e| (effect_name(e), Want::from(e as u32)))
        .collect();
    x.family("EFFECT_", &named);
    let class = [(
        "OPERATOR",
        Some(Want::from(PowerCreepClass::Operator.to_string())),
    )];
    x.table("POWER_CLASS", &class, Rule::default());
}

/// Every body part, in the engine's BODYPARTS_ALL order.
const PARTS: [Part; 8] = [
    Part::Move,
    Part::Work,
    Part::Carry,
    Part::Attack,
    Part::RangedAttack,
    Part::Tough,
    Part::Heal,
    Part::Claim,
];

fn part_name(p: Part) -> Option<&'static str> {
    Some(match p {
        Part::Move => "MOVE",
        Part::Work => "WORK",
        Part::Carry => "CARRY",
        Part::Attack => "ATTACK",
        Part::RangedAttack => "RANGED_ATTACK",
        Part::Tough => "TOUGH",
        Part::Heal => "HEAL",
        Part::Claim => "CLAIM",
        _ => return None,
    })
}

/// The part names, `BODYPART_COST` and `BODYPARTS_ALL`.
fn parts(x: &mut Ctx<'_>) {
    let part_names = display_names(&PARTS);
    let all: Vec<Want> = part_names.iter().map(|n| Want::from(n.as_str())).collect();
    for ((&p, n), want) in PARTS.iter().zip(&part_names).zip(&all) {
        match part_name(p) {
            Some(name) => x.scalar(name, want),
            None => x.missing(At::Via(n, "Part"), Some(want)),
        }
    }
    let costs: Entries<'_> = PARTS
        .iter()
        .zip(&part_names)
        .map(|(&p, n)| (n.as_str(), Some(Want::from(p.cost()))))
        .collect();
    x.table("BODYPART_COST", &costs, Rule::default());
    x.set("BODYPARTS_ALL", &all);
}

/// Every structure type.
const STRUCTURES: [StructureType; 21] = [
    StructureType::Spawn,
    StructureType::Extension,
    StructureType::Road,
    StructureType::Wall,
    StructureType::Rampart,
    StructureType::KeeperLair,
    StructureType::Portal,
    StructureType::Controller,
    StructureType::Link,
    StructureType::Storage,
    StructureType::Tower,
    StructureType::Observer,
    StructureType::PowerBank,
    StructureType::PowerSpawn,
    StructureType::Extractor,
    StructureType::Lab,
    StructureType::Terminal,
    StructureType::Container,
    StructureType::Nuker,
    StructureType::Factory,
    StructureType::InvaderCore,
];

/// The scalar each structure's `initial_hits` mirrors.
fn hits_name(s: StructureType) -> Option<&'static str> {
    Some(match s {
        StructureType::Spawn => "SPAWN_HITS",
        StructureType::Extension => "EXTENSION_HITS",
        StructureType::Road => "ROAD_HITS",
        StructureType::Wall => "WALL_HITS",
        StructureType::Rampart => "RAMPART_HITS",
        StructureType::Link => "LINK_HITS",
        StructureType::Storage => "STORAGE_HITS",
        StructureType::Tower => "TOWER_HITS",
        StructureType::Observer => "OBSERVER_HITS",
        StructureType::PowerBank => "POWER_BANK_HITS",
        StructureType::PowerSpawn => "POWER_SPAWN_HITS",
        StructureType::Extractor => "EXTRACTOR_HITS",
        StructureType::Lab => "LAB_HITS",
        StructureType::Terminal => "TERMINAL_HITS",
        StructureType::Container => "CONTAINER_HITS",
        StructureType::Nuker => "NUKER_HITS",
        StructureType::Factory => "FACTORY_HITS",
        StructureType::InvaderCore => "INVADER_CORE_HITS",
        _ => return None,
    })
}

/// Object types in OBSTACLE_OBJECT_TYPES that are not structures (constants.js:84); the crate has
/// only `StructureType::is_obstacle`, so these have nothing to compare with.
const NON_STRUCTURE_OBSTACLES: [&str; 5] = ["creep", "powerCreep", "source", "mineral", "deposit"];

/// `STRUCTURE_*`, `CONSTRUCTION_COST`, `RUIN_DECAY_STRUCTURES`, `OBSTACLE_OBJECT_TYPES` and each
/// structure's initial hits against its `*_HITS` scalar.
fn structures(x: &mut Ctx<'_>) {
    const OBSTACLES: &str = "OBSTACLE_OBJECT_TYPES";
    let names = display_names(&STRUCTURES);
    let values: Vec<Want> = names.iter().map(|s| Want::from(s.as_str())).collect();
    x.family_set("STRUCTURE_", &values);
    let cost: Entries<'_> = STRUCTURES
        .iter()
        .zip(&names)
        .map(|(s, n)| (n.as_str(), s.construction_cost().map(Want::from)))
        .collect();
    x.table("CONSTRUCTION_COST", &cost, Rule::default());
    let ruin: Entries<'_> = STRUCTURES
        .iter()
        .zip(&names)
        .map(|(&s, n)| (n.as_str(), c::ruin_decay_structures(s).map(Want::from)))
        .collect();
    x.table("RUIN_DECAY_STRUCTURES", &ruin, Rule::default());

    for (&s, n) in STRUCTURES.iter().zip(&names) {
        let Some(hits) = s.initial_hits() else {
            continue;
        };
        let via = format!("initial_hits({n})");
        match hits_name(s) {
            Some(name) => x.also(name, &via, &Want::from(hits)),
            None => x.missing(At::Name(&via), Some(&Want::from(hits))),
        }
    }

    let Some((v, text)) = x.required(OBSTACLES) else {
        return;
    };
    let Some(items) = v.elements() else {
        return x.shape(OBSTACLES, "an array", v);
    };
    for ((&s, n), want) in STRUCTURES.iter().zip(&names).zip(&values) {
        let listed = items.iter().find(|e| e.as_str() == Some(n.as_str()));
        match (s.is_obstacle(), listed) {
            (true, Some(_)) | (false, None) => x.matched(),
            (true, None) => x.missing(At::Name(OBSTACLES), Some(want)),
            (false, Some(e)) => {
                x.r.compared = x.r.compared.saturating_add(1);
                x.extra(At::Name(OBSTACLES), e, text);
            }
        }
    }
    for e in items {
        let known = e
            .as_str()
            .is_some_and(|t| names.iter().any(|n| n == t) || NON_STRUCTURE_OBSTACLES.contains(&t));
        if !known {
            x.extra(At::Name(OBSTACLES), e, text);
        }
    }
}

/// `CONTROLLER_STRUCTURES`: per structure type, the count at each controller level 0 to 8 (the
/// engine leaves out the zero rows and most level-0 keys).
fn controller_structures(x: &mut Ctx<'_>) {
    const NAME: &str = "CONTROLLER_STRUCTURES";
    let names = display_names(&STRUCTURES);
    let want: Vec<(&str, Entries<'_>)> = STRUCTURES
        .iter()
        .zip(&names)
        .map(|(&s, n)| {
            let count = |rcl| Some(Want::from(s.controller_structures(rcl)));
            (n.as_str(), numbered(8, &count))
        })
        .collect();
    if let Some((v, text)) = x.required(NAME) {
        x.nested(NAME, v, text, &want, ABSENT_IS_ZERO);
    }
}

fn intershard_name(r: IntershardResourceType) -> Option<&'static str> {
    Some(match r {
        IntershardResourceType::CpuUnlock => "CPU_UNLOCK",
        IntershardResourceType::Pixel => "PIXEL",
        IntershardResourceType::AccessKey => "ACCESS_KEY",
        _ => return None,
    })
}

const INTERSHARD: [IntershardResourceType; 3] = [
    IntershardResourceType::CpuUnlock,
    IntershardResourceType::Pixel,
    IntershardResourceType::AccessKey,
];

/// `RESOURCE_*` and `RESOURCES_ALL` against the crate's `RESOURCES_ALL` (sets: the orders differ),
/// the intershard resources, `MINERAL_MIN_AMOUNT` and `REACTION_TIME`.
fn resources(x: &mut Ctx<'_>) {
    let names = display_names(RESOURCES_ALL);
    let values: Vec<Want> = names.iter().map(|s| Want::from(s.as_str())).collect();
    x.family_set("RESOURCE_", &values);
    x.set("RESOURCES_ALL", &values);
    let shard_names = display_names(&INTERSHARD);
    let shard: Vec<Want> = shard_names.iter().map(|n| Want::from(n.as_str())).collect();
    for ((&r, n), want) in INTERSHARD.iter().zip(&shard_names).zip(&shard) {
        match intershard_name(r) {
            Some(name) => x.scalar(name, want),
            None => x.missing(At::Via(n, "IntershardResourceType"), Some(want)),
        }
    }
    x.set("INTERSHARD_RESOURCES", &shard);
    let min: Entries<'_> = RESOURCES_ALL
        .iter()
        .zip(&names)
        .map(|(&r, n)| (n.as_str(), c::mineral_min_amount(r).map(Want::from)))
        .collect();
    x.table("MINERAL_MIN_AMOUNT", &min, Rule::default());
    let time: Entries<'_> = RESOURCES_ALL
        .iter()
        .zip(&names)
        .map(|(r, n)| (n.as_str(), r.reaction_time().map(Want::from)))
        .collect();
    x.table("REACTION_TIME", &time, Rule::default());
}

/// The per-level tables: `CONTROLLER_LEVELS`, `CONTROLLER_DOWNGRADE`, `EXTENSION_ENERGY_CAPACITY`,
/// `RAMPART_HITS_MAX` (with the crate's `RAMPART_HITS_MAX_RCL*`) and `NUKE_DAMAGE` (with
/// `NUKE_DAMAGE_RANGE_*`), each over controller levels 0 to 8.
fn controller_tables(x: &mut Ctx<'_>) {
    x.table(
        "CONTROLLER_LEVELS",
        &numbered(8, &|rcl| c::controller_levels(rcl).map(Want::from)),
        Rule::default(),
    );
    x.table(
        "CONTROLLER_DOWNGRADE",
        &numbered(8, &|rcl| {
            u8::try_from(rcl)
                .ok()
                .and_then(c::controller_downgrade)
                .map(Want::from)
        }),
        Rule::default(),
    );
    x.table(
        "EXTENSION_ENERGY_CAPACITY",
        &numbered(8, &|rcl| {
            Some(Want::from(c::extension_energy_capacity(rcl)))
        }),
        Rule::default(),
    );
    x.table(
        "RAMPART_HITS_MAX",
        &numbered(8, &|rcl| Some(Want::from(c::rampart_hits_max(rcl)))),
        ABSENT_IS_ZERO,
    );
    let rcl_consts = [
        ("2", "RAMPART_HITS_MAX_RCL2", c::RAMPART_HITS_MAX_RCL2),
        ("3", "RAMPART_HITS_MAX_RCL3", c::RAMPART_HITS_MAX_RCL3),
        ("4", "RAMPART_HITS_MAX_RCL4", c::RAMPART_HITS_MAX_RCL4),
        ("5", "RAMPART_HITS_MAX_RCL5", c::RAMPART_HITS_MAX_RCL5),
        ("6", "RAMPART_HITS_MAX_RCL6", c::RAMPART_HITS_MAX_RCL6),
        ("7", "RAMPART_HITS_MAX_RCL7", c::RAMPART_HITS_MAX_RCL7),
        ("8", "RAMPART_HITS_MAX_RCL8", c::RAMPART_HITS_MAX_RCL8),
    ];
    for (key, name, value) in rcl_consts {
        x.entry("RAMPART_HITS_MAX", key, name, &Want::from(value));
    }
    let nuke = [
        ("0", Some(Want::from(c::NUKE_DAMAGE_RANGE_0))),
        ("2", Some(Want::from(c::NUKE_DAMAGE_RANGE_2))),
    ];
    x.table("NUKE_DAMAGE", &nuke, Rule::default());
}

/// The invader-core tables over core levels 0 to 5. The crate answers `None` at level 0 where the
/// engine lists 0 (INVADER_CORE_CREEP_SPAWN_TIME, STRONGHOLD_RAMPART_HITS); the engine treats a 0
/// as "none" (it skips a falsy spawn time, engine 4.3.0 invader-core/create-creep.js:10).
fn invader_tables(x: &mut Ctx<'_>) {
    x.table(
        "INVADER_CORE_CREEP_SPAWN_TIME",
        &numbered(5, &|level| {
            c::invader_core_creep_spawn_time(level).map(Want::from)
        }),
        ZERO_IS_NONE,
    );
    x.table(
        "INVADER_CORE_EXPAND_TIME",
        &numbered(5, &|level| {
            c::invader_core_expand_time(level).map(Want::from)
        }),
        ZERO_IS_NONE,
    );
    x.table(
        "STRONGHOLD_RAMPART_HITS",
        &numbered(5, &|level| {
            c::stronghold_rampart_hits(level).map(Want::from)
        }),
        ZERO_IS_NONE,
    );
}

/// `REACTIONS`: each reagent pair, both ways round, gives the product whose components the crate
/// lists.
fn reactions(x: &mut Ctx<'_>) {
    const NAME: &str = "REACTIONS";
    let names = display_names(RESOURCES_ALL);
    let name_of = |r| {
        RESOURCES_ALL
            .iter()
            .position(|&q| q == r)
            .and_then(|i| names.get(i))
            .map_or("?", String::as_str)
    };
    let mut want: Vec<(&str, Entries<'_>)> = Vec::new();
    for (&r, product) in RESOURCES_ALL.iter().zip(&names) {
        if let Some([a, b]) = r.reaction_components() {
            let (a, b) = (name_of(a), name_of(b));
            put(&mut want, a, b, Want::from(product.as_str()));
            put(&mut want, b, a, Want::from(product.as_str()));
        }
    }
    if let Some((v, text)) = x.required(NAME) {
        x.nested(NAME, v, text, &want, Rule::default());
    }
}

/// The engine's part and action keys for each boost (constants.js:616-731).
fn boost_keys(b: Boost) -> (Part, &'static [&'static str], Want) {
    match b {
        Boost::Harvest(n) => (Part::Work, &["harvest"], Want::from(n)),
        Boost::BuildAndRepair(f) => (Part::Work, &["build", "repair"], Want::from(f)),
        Boost::Dismantle(n) => (Part::Work, &["dismantle"], Want::from(n)),
        Boost::UpgradeController(f) => (Part::Work, &["upgradeController"], Want::from(f)),
        Boost::Attack(n) => (Part::Attack, &["attack"], Want::from(n)),
        Boost::RangedAttack(n) => (
            Part::RangedAttack,
            &["rangedAttack", "rangedMassAttack"],
            Want::from(n),
        ),
        Boost::Heal(n) => (Part::Heal, &["heal", "rangedHeal"], Want::from(n)),
        Boost::Carry(n) => (Part::Carry, &["capacity"], Want::from(n)),
        Boost::Move(n) => (Part::Move, &["fatigue"], Want::from(n)),
        Boost::Tough(f) => (Part::Tough, &["damage"], Want::from(f)),
    }
}

/// `BOOSTS`: part, then compound, then each action's multiplier.
fn boosts(x: &mut Ctx<'_>) {
    const NAME: &str = "BOOSTS";
    let names = display_names(RESOURCES_ALL);
    let part_names = display_names(&PARTS);
    let part_of = |p| {
        PARTS
            .iter()
            .position(|&q| q == p)
            .and_then(|i| part_names.get(i))
            .map_or("?", String::as_str)
    };
    // part -> compound -> actions
    let mut want: Vec<(&str, Vec<(&str, Entries<'_>)>)> = Vec::new();
    for (&r, compound) in RESOURCES_ALL.iter().zip(&names) {
        let Some(b) = r.boost() else {
            continue;
        };
        let (part, actions, value) = boost_keys(b);
        let part = part_of(part);
        let row = match want.iter().position(|(k, _)| *k == part) {
            Some(i) => want.get_mut(i),
            None => {
                want.push((part, Vec::new()));
                want.last_mut()
            }
        };
        if let Some((_, compounds)) = row {
            let entries = actions.iter().map(|&a| (a, Some(value.clone()))).collect();
            compounds.push((compound.as_str(), entries));
        }
    }
    let Some((v, text)) = x.required(NAME) else {
        return;
    };
    let Some(members) = v.members() else {
        return x.shape(NAME, "an object", v);
    };
    for (part, compounds) in &want {
        match v.get(part) {
            Some(found) => {
                let path = format!("{NAME}.{part}");
                x.nested(&path, found, text, compounds, Rule::default());
            }
            None => x.missing(At::Key(NAME, part), None),
        }
    }
    for (part, found) in members {
        if !want.iter().any(|(k, _)| k == part) {
            x.extra(At::Key(NAME, part), found, text);
        }
    }
}

/// `COMMODITIES`: each product's amount, cooldown, factory level (none for the basic ones) and
/// components.
fn commodities(x: &mut Ctx<'_>) {
    const NAME: &str = "COMMODITIES";
    let Some((v, text)) = x.required(NAME) else {
        return;
    };
    let Some(members) = v.members() else {
        return x.shape(NAME, "an object", v);
    };
    let names = display_names(RESOURCES_ALL);
    let mut products: Vec<&str> = Vec::new();
    for (&r, product) in RESOURCES_ALL.iter().zip(&names) {
        let Some(recipe) = r.commodity_recipe() else {
            continue;
        };
        products.push(product);
        let Some(found) = v.get(product) else {
            x.missing(At::Key(NAME, product), None);
            continue;
        };
        let path = format!("{NAME}.{product}");
        let fields = [
            ("amount", Some(Want::from(recipe.amount))),
            ("cooldown", Some(Want::from(recipe.cooldown))),
            ("level", recipe.level.map(Want::from)),
        ];
        x.entries(
            &path,
            found,
            text,
            &fields,
            Rule::default(),
            &["components"],
        );
        // Looked up in RESOURCES_ALL order, so findings never follow hash-map order.
        let components: Entries<'_> = RESOURCES_ALL
            .iter()
            .zip(&names)
            .map(|(q, n)| (n.as_str(), recipe.components.get(q).map(|&k| Want::from(k))))
            .collect();
        match found.get("components") {
            Some(comp) => {
                let comp_path = format!("{path}.components");
                x.entries(&comp_path, comp, text, &components, Rule::default(), &[]);
            }
            None => x.missing(At::Key(&path, "components"), None),
        }
    }
    for (product, found) in members {
        if !products.contains(&product.as_str()) {
            x.extra(At::Key(NAME, product), found, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known differences match by kind, exact path or prefix, and quoted value.
    #[test]
    fn known_differences_match_precisely() {
        for (i, k) in KNOWN.iter().enumerate() {
            assert!(!k.path.is_empty() && !k.reason.is_empty(), "{}", k.path);
            let first = KNOWN
                .iter()
                .position(|j| j.kind == k.kind && j.path == k.path);
            assert_eq!(first, Some(i), "{} is listed twice", k.path);
        }
        assert!(known_index(FindingKind::Unknown, "LOOK_CREEPS", "\"creep\"").is_some());
        assert!(known_index(FindingKind::Extra, "INTERSHARD_RESOURCES", "\"token\"").is_some());
        assert_eq!(
            known_index(FindingKind::Extra, "INTERSHARD_RESOURCES", "\"tokens\""),
            None
        );
        assert_eq!(known_index(FindingKind::Mismatch, "JSON", ""), None);
        // Exact names only: a constant added to a family is reported.
        assert_eq!(known_index(FindingKind::Unknown, "LOOK", ""), None);
        assert_eq!(known_index(FindingKind::Unknown, "LOOK_NEW", "\"x\""), None);
        assert_eq!(known_index(FindingKind::Unknown, "EVENT_NEW", "13"), None);
    }

    /// The name maps give each list member its own name.
    #[test]
    fn name_maps_are_one_to_one() {
        fn distinct(names: &[&str]) {
            let mut sorted = names.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), names.len(), "{names:?}");
        }
        distinct(&ERRORS.map(error_name));
        distinct(&COLORS.map(color_name));
        distinct(&FINDS.map(|f| find_name(f).unwrap_or("seasonal")));
        distinct(&POWERS.map(power_name));
        distinct(&DENSITIES.map(density_name));
        distinct(&EFFECTS.map(effect_name));
        distinct(&PARTS.map(|p| part_name(p).unwrap_or("none")));
        distinct(&INTERSHARD.map(|r| intershard_name(r).unwrap_or("none")));
        let hits: Vec<&str> = STRUCTURES.iter().filter_map(|&s| hits_name(s)).collect();
        distinct(&hits);
        let directions: Vec<&str> = Direction::iter().map(|&d| direction_name(d)).collect();
        assert_eq!(directions.len(), 8);
        distinct(&directions);
    }
}
