//! What the check found: expected values, findings and the counts, with their console text. Nothing
//! here formats a float with `core::fmt` (that would link core's float formatting into the wasm,
//! about 20 KB, P1.6); crate floats print through [`Fixed`].

use core::fmt;

use super::json::{Json, Num};

/// Most findings a report keeps; the counts include the rest.
pub const MAX_FINDINGS: usize = 64;

/// The most characters of a server value a finding quotes.
const QUOTE_MAX: usize = 48;

/// A value the crate gives for a constant (or one entry of a table).
#[derive(Clone, Debug, PartialEq)]
pub enum Want {
    /// An integer; the server's number must equal it exactly.
    Int(i64),
    /// An `f32` constant; the server's double must round to the same `f32`.
    F32(f32),
    /// An `f64` constant; the server's double must equal it.
    F64(f64),
    Str(String),
    Bool(bool),
    Null,
}

impl From<u32> for Want {
    fn from(v: u32) -> Self {
        Self::Int(i64::from(v))
    }
}

impl From<i32> for Want {
    fn from(v: i32) -> Self {
        Self::Int(i64::from(v))
    }
}

impl From<u8> for Want {
    fn from(v: u8) -> Self {
        Self::Int(i64::from(v))
    }
}

impl From<u16> for Want {
    fn from(v: u16) -> Self {
        Self::Int(i64::from(v))
    }
}

impl From<i8> for Want {
    fn from(v: i8) -> Self {
        Self::Int(i64::from(v))
    }
}

impl From<f32> for Want {
    fn from(v: f32) -> Self {
        Self::F32(v)
    }
}

impl From<f64> for Want {
    fn from(v: f64) -> Self {
        Self::F64(v)
    }
}

impl From<&str> for Want {
    fn from(v: &str) -> Self {
        Self::Str(v.into())
    }
}

impl From<String> for Want {
    fn from(v: String) -> Self {
        Self::Str(v)
    }
}

impl From<bool> for Want {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl fmt::Display for Want {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(n) => write!(f, "{n}"),
            Self::F32(v) => write!(f, "{}", Fixed(f64::from(*v))),
            Self::F64(v) => write!(f, "{}", Fixed(*v)),
            Self::Str(s) => write!(f, "\"{s}\""),
            Self::Bool(b) => f.write_str(if *b { "true" } else { "false" }),
            Self::Null => f.write_str("null"),
        }
    }
}

/// How a server value compares with what the crate wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Match,
    Mismatch,
    /// A number the reader could not take exactly (see `json`), so it is neither.
    Unreadable,
}

impl Want {
    /// Compares `found` with this value.
    pub fn compare(&self, found: &Json) -> Outcome {
        let same = match (self, found) {
            (Self::Int(_) | Self::F32(_) | Self::F64(_), Json::Num(Num { value: None, .. })) => {
                return Outcome::Unreadable;
            }
            // Exact for every |n| < 2^53; a larger crate integer cannot match a fast-path value.
            (Self::Int(n), Json::Num(Num { value: Some(x), .. })) => {
                *n > -(1 << 53) && *n < (1 << 53) && *x == *n as f64
            }
            (Self::F32(v), Json::Num(Num { value: Some(x), .. })) => {
                x.is_finite() && (*x as f32) == *v
            }
            (Self::F64(v), Json::Num(Num { value: Some(x), .. })) => x == v,
            (Self::Str(s), Json::Str(t)) => s == t,
            (Self::Bool(b), Json::Bool(c)) => b == c,
            (Self::Null, Json::Null) => true,
            _ => false,
        };
        if same {
            Outcome::Match
        } else {
            Outcome::Mismatch
        }
    }
}

/// A float printed with at most six decimals from integer arithmetic (trailing zeros dropped), for
/// crate values in findings. Values beyond 10^12 print as a bound; findings only need to be told
/// apart from the server's text, which they quote exactly.
pub struct Fixed(pub f64);

impl fmt::Display for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        if v.is_nan() {
            return f.write_str("NaN");
        }
        if v.is_infinite() || v.abs() >= 1e12 {
            return f.write_str(if v > 0.0 { ">1e12" } else { "<-1e12" });
        }
        const SCALE: u64 = 1_000_000;
        // |v| < 1e12, so the scaled value is below 1e18 and fits a u64.
        let scaled = (v.abs() * SCALE as f64 + 0.5).floor() as u64;
        let sign = if v < 0.0 && scaled > 0 { "-" } else { "" };
        let (whole, mut frac) = (scaled / SCALE, scaled % SCALE);
        if frac == 0 {
            return write!(f, "{sign}{whole}");
        }
        let mut width = 6usize;
        while frac % 10 == 0 {
            frac /= 10;
            width -= 1;
        }
        write!(f, "{sign}{whole}.{frac:0width$}")
    }
}

/// What a finding is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindingKind {
    /// The server's value differs from the crate's.
    Mismatch,
    /// The crate has a value the server lacks.
    Missing,
    /// The server has an entry in a checked table, or a member of a checked name family, that the
    /// crate lacks.
    Extra,
    /// The server's value cannot be read exactly (no JSON text, bad JSON, or a number off the
    /// reader's exact path).
    Unreadable,
    /// A server constant that no check reads and the list of known differences does not name.
    Unknown,
}

impl FindingKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mismatch => "mismatch",
            Self::Missing => "missing on the server",
            Self::Extra => "not in the crate",
            Self::Unreadable => "unreadable",
            Self::Unknown => "unknown constant",
        }
    }
}

/// One difference between the crate and the server.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub kind: FindingKind,
    /// Where: the constant and the keys into it, such as `CONTROLLER_STRUCTURES.extension.8`.
    pub path: String,
    /// The crate's value, if it has one.
    pub expected: Option<Want>,
    /// The server's value as its text shows it (a number's own digits, a quoted string, or what kind
    /// of value it is), at most 48 characters; empty when the server has none.
    pub server: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.path, self.kind.as_str())?;
        if let Some(want) = &self.expected {
            write!(f, ": crate {want}")?;
        }
        match (self.server.is_empty(), self.expected.is_some()) {
            (true, _) => Ok(()),
            (false, true) => write!(f, ", server {}", self.server),
            (false, false) => write!(f, ": server {}", self.server),
        }
    }
}

/// The server value `v` as a finding quotes it, from `text`, the JSON it was read from.
pub fn quote(v: &Json, text: &str) -> String {
    let mut out = match v {
        Json::Num(n) => n.at.of(text).unwrap_or("a number").to_owned(),
        Json::Str(s) => format!("\"{s}\""),
        Json::Bool(true) => "true".to_owned(),
        Json::Bool(false) => "false".to_owned(),
        other => other.kind().to_owned(),
    };
    if let Some((cut, _)) = out.char_indices().nth(QUOTE_MAX) {
        out.truncate(cut);
        out.push('…');
    }
    out
}

/// Every count the check makes, and the first findings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Upper-case own names of the server's global object (constants, and `JSON`).
    pub server_names: u32,
    /// Values compared (scalars and table entries).
    pub compared: u32,
    pub matched: u32,
    pub mismatched: u32,
    pub missing: u32,
    pub extra: u32,
    pub unreadable: u32,
    pub unknown: u32,
    /// Findings on the list of known differences (`expect::KNOWN`: names the crate does not mirror
    /// and gaps already recorded in VERIFICATION_LOG), counted here instead of reported.
    pub known: u32,
    /// Which known differences occurred, as indices into that list, in the order first seen.
    pub known_seen: Vec<u16>,
    /// The first [`MAX_FINDINGS`] findings, in the order the checks ran.
    pub findings: Vec<Finding>,
}

impl Report {
    /// All findings, kept or not.
    pub fn finding_count(&self) -> u32 {
        self.mismatched
            .saturating_add(self.missing)
            .saturating_add(self.extra)
            .saturating_add(self.unreadable)
            .saturating_add(self.unknown)
    }

    /// Counts a finding and keeps it while there is room.
    pub fn add(&mut self, kind: FindingKind, path: String, expected: Option<Want>, server: String) {
        let count = match kind {
            FindingKind::Mismatch => &mut self.mismatched,
            FindingKind::Missing => &mut self.missing,
            FindingKind::Extra => &mut self.extra,
            FindingKind::Unreadable => &mut self.unreadable,
            FindingKind::Unknown => &mut self.unknown,
        };
        *count = count.saturating_add(1);
        if self.findings.len() < MAX_FINDINGS {
            self.findings.push(Finding {
                kind,
                path,
                expected,
                server,
            });
        }
    }

    /// Counts a known difference (index `i` of the known list).
    pub fn add_known(&mut self, i: u16) {
        self.known = self.known.saturating_add(1);
        if !self.known_seen.contains(&i) {
            self.known_seen.push(i);
        }
    }

    /// The counts as one line: `423 names, 2210 values compared: 2210 match, 0 mismatched, ...`.
    pub fn counts(&self) -> Counts<'_> {
        Counts(self)
    }
}

/// [`Report::counts`].
pub struct Counts<'a>(&'a Report);

impl fmt::Display for Counts<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = self.0;
        write!(
            f,
            "{} server names, {} values compared: {} match, {} mismatched, {} missing on the server, \
             {} not in the crate, {} unreadable, {} unknown constants; {} known differences",
            r.server_names,
            r.compared,
            r.matched,
            r.mismatched,
            r.missing,
            r.extra,
            r.unreadable,
            r.unknown,
            r.known
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::json::parse;
    use super::*;

    fn cmp(want: Want, text: &str) -> Outcome {
        want.compare(&parse(text).expect("valid"))
    }

    #[test]
    fn integers_compare_exactly() {
        assert_eq!(cmp(Want::from(1500u32), "1500"), Outcome::Match);
        assert_eq!(cmp(Want::from(1500u32), "1500.5"), Outcome::Mismatch);
        assert_eq!(cmp(Want::from(1500u32), "\"1500\""), Outcome::Mismatch);
        assert_eq!(cmp(Want::from(-7i32), "-7"), Outcome::Match);
        assert_eq!(cmp(Want::from(0u32), "-0"), Outcome::Match);
        assert_eq!(
            cmp(Want::from(1u32), "0.30000000000000004"),
            Outcome::Unreadable
        );
        assert_eq!(
            cmp(Want::Int(1 << 53), "9007199254740992"),
            Outcome::Mismatch
        );
    }

    /// An f32 constant matches the server's double that rounds to it (0.2 is not exactly 0.2f32).
    #[test]
    fn f32_constants_match_by_rounding() {
        assert_eq!(cmp(Want::from(0.2f32), "0.2"), Outcome::Match);
        assert_eq!(cmp(Want::from(0.2f32), "0.21"), Outcome::Mismatch);
        assert_eq!(cmp(Want::from(0.2f64), "0.2"), Outcome::Match);
        assert_eq!(cmp(Want::from(f64::from(0.2f32)), "0.2"), Outcome::Mismatch);
        assert_eq!(cmp(Want::from(2.4f32), "2.4"), Outcome::Match);
    }

    #[test]
    fn other_kinds_compare_by_value() {
        assert_eq!(cmp(Want::from("work"), "\"work\""), Outcome::Match);
        assert_eq!(cmp(Want::from("work"), "\"move\""), Outcome::Mismatch);
        assert_eq!(cmp(Want::from(true), "true"), Outcome::Match);
        assert_eq!(cmp(Want::Null, "null"), Outcome::Match);
        assert_eq!(cmp(Want::Null, "0"), Outcome::Mismatch);
    }

    #[test]
    fn fixed_prints_short_decimals() {
        let cases: [(f64, &str); 9] = [
            (0.0, "0"),
            (1500.0, "1500"),
            (0.2, "0.2"),
            (f64::from(0.2f32), "0.2"),
            (0.05, "0.05"),
            (-2.4, "-2.4"),
            (0.000001, "0.000001"),
            (-0.0000001, "0"),
            (1e13, ">1e12"),
        ];
        for (v, text) in cases {
            assert_eq!(Fixed(v).to_string(), text, "{v}");
        }
    }

    #[test]
    fn findings_read_as_one_line() {
        let text = "1400";
        let found = parse(text).expect("valid");
        let mut r = Report::default();
        r.add(
            FindingKind::Mismatch,
            "CREEP_LIFE_TIME".into(),
            Some(Want::from(1500u32)),
            quote(&found, text),
        );
        r.add(
            FindingKind::Missing,
            "BODYPART_COST.work".into(),
            Some(Want::from(100u32)),
            String::new(),
        );
        r.add(
            FindingKind::Unknown,
            "NEW_THING".into(),
            None,
            "an object".into(),
        );
        let lines: Vec<String> = r.findings.iter().map(ToString::to_string).collect();
        assert_eq!(
            lines,
            [
                "CREEP_LIFE_TIME (mismatch): crate 1500, server 1400",
                "BODYPART_COST.work (missing on the server): crate 100",
                "NEW_THING (unknown constant): server an object",
            ]
        );
        assert_eq!(
            (r.mismatched, r.missing, r.unknown, r.finding_count()),
            (1, 1, 1, 3)
        );
    }

    #[test]
    fn reports_keep_the_first_findings_and_count_the_rest() {
        let mut r = Report::default();
        for i in 0..MAX_FINDINGS + 5 {
            r.add(FindingKind::Extra, format!("T.{i}"), None, "1".into());
        }
        assert_eq!(r.findings.len(), MAX_FINDINGS);
        assert_eq!(r.extra as usize, MAX_FINDINGS + 5);
    }

    #[test]
    fn quotes_are_short() {
        let text = format!("\"{}\"", "x".repeat(100));
        let q = quote(&parse(&text).expect("valid"), &text);
        assert_eq!(q.chars().count(), QUOTE_MAX + 1);
        assert!(q.ends_with('…'));
        assert_eq!(
            quote(&parse("{\"a\":1}").expect("valid"), "{\"a\":1}"),
            "an object"
        );
    }
}
