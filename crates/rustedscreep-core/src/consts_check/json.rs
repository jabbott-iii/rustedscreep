//! A strict reader for JSON text as `JSON.stringify` writes it (RFC 8259,
//! <https://www.rfc-editor.org/rfc/rfc8259>), for the constants self-check: the wasm side hands
//! over each server constant as the text of `JSON.stringify(value)`.
//!
//! - Numbers keep their text span (findings quote the server's own text) and an exact `f64` where
//!   Clinger's fast path gives one: an integer significand of at most 2^53 times or divided by an
//!   exact power of ten up to 10^22, one correctly rounded IEEE operation on exact operands, so the
//!   result is the decimal's correctly rounded value. `JSON.stringify` writes the shortest decimal
//!   that round-trips, so every integer below 2^53 and every short decimal (0.2, 0.05, 2.4) reads
//!   exactly; any other number reads as `None`, which the check reports as unreadable instead of
//!   guessing. This keeps core's general float parser out of the wasm.
//! - Nesting is limited to [`MAX_DEPTH`] levels, so a broken text cannot overflow the stack.
//! - Objects keep their members in text order; [`Json::get`] scans them (constant tables are small).

use core::fmt;

/// Deepest nesting accepted (the deepest engine constant, `BOOSTS`, has 3 levels).
pub const MAX_DEPTH: u32 = 16;

/// A byte range of the text a value was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    /// The spanned text, if `text` is the one the span was read from.
    pub fn of<'t>(&self, text: &'t str) -> Option<&'t str> {
        let start = usize::try_from(self.start).ok()?;
        let end = usize::try_from(self.end).ok()?;
        text.get(start..end)
    }
}

/// A JSON number: its exact value when the fast path gives one, and where its text is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Num {
    pub value: Option<f64>,
    pub at: Span,
}

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(Num),
    Str(String),
    Arr(Vec<Json>),
    /// Members in text order.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// The member `key` of an object; `None` for a missing key or a value that is not an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The members of an object.
    pub fn members(&self) -> Option<&[(String, Json)]> {
        match self {
            Self::Obj(members) => Some(members),
            _ => None,
        }
    }

    /// The elements of an array.
    pub fn elements(&self) -> Option<&[Json]> {
        match self {
            Self::Arr(items) => Some(items),
            _ => None,
        }
    }

    /// The string, for a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// What kind of value this is, for findings.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "a boolean",
            Self::Num(_) => "a number",
            Self::Str(_) => "a string",
            Self::Arr(_) => "an array",
            Self::Obj(_) => "an object",
        }
    }
}

/// Why a text is not JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonErrorKind {
    /// The text ended inside a value.
    End,
    /// A byte that cannot start or continue a value here.
    Unexpected,
    /// Text after the value.
    Trailing,
    /// Nesting deeper than [`MAX_DEPTH`].
    TooDeep,
    /// A malformed number (a leading zero, a missing digit).
    Number,
    /// A bad escape or a raw control character in a string.
    Escape,
}

/// Why a text is not JSON, and the byte index where reading stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonError {
    pub kind: JsonErrorKind,
    pub at: u32,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            JsonErrorKind::End => "the text ends inside a value",
            JsonErrorKind::Unexpected => "unexpected character",
            JsonErrorKind::Trailing => "text after the value",
            JsonErrorKind::TooDeep => "nested too deeply",
            JsonErrorKind::Number => "malformed number",
            JsonErrorKind::Escape => "bad string escape or control character",
        };
        write!(f, "{what} at byte {}", self.at)
    }
}

/// Reads `text` as one JSON value (surrounding whitespace allowed).
pub fn parse(text: &str) -> Result<Json, JsonError> {
    let mut r = Reader {
        bytes: text.as_bytes(),
        pos: 0,
    };
    r.skip_ws();
    let value = r.value(0)?;
    r.skip_ws();
    if r.pos < r.bytes.len() {
        return Err(r.error(JsonErrorKind::Trailing));
    }
    Ok(value)
}

/// 10^0 to 10^22, every one exact in an `f64` (5^22 < 2^53).
const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// The largest significand the fast path takes: 2^53, the last integer of an unbroken run that
/// `f64` holds exactly.
const MAX_EXACT: u64 = 1 << 53;

/// The value of `significand` × 10^`exp10`, when Clinger's fast path gives it exactly rounded.
///
/// Source: W. D. Clinger, "How to Read Floating Point Numbers Accurately" (PLDI 1990), the fast
/// path also used by Rust's `core::num::dec2flt`: with both operands exact, one IEEE operation
/// rounds the exact result correctly.
fn fast_path(significand: u64, exp10: i64, negative: bool) -> Option<f64> {
    if significand > MAX_EXACT {
        return None;
    }
    // Exact: significand <= 2^53.
    let m = significand as f64;
    let magnitude = if exp10 >= 0 {
        m * *POW10.get(usize::try_from(exp10).ok()?)?
    } else {
        m / *POW10.get(usize::try_from(exp10.checked_neg()?).ok()?)?
    };
    Some(if negative { -magnitude } else { magnitude })
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn error(&self, kind: JsonErrorKind) -> JsonError {
        JsonError {
            kind,
            at: u32::try_from(self.pos).unwrap_or(u32::MAX),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect_word(&mut self, word: &[u8]) -> Result<(), JsonError> {
        let end = self.pos.saturating_add(word.len());
        if self.bytes.get(self.pos..end) == Some(word) {
            self.pos = end;
            Ok(())
        } else if end > self.bytes.len() {
            Err(self.error(JsonErrorKind::End))
        } else {
            Err(self.error(JsonErrorKind::Unexpected))
        }
    }

    fn value(&mut self, depth: u32) -> Result<Json, JsonError> {
        match self.peek() {
            None => Err(self.error(JsonErrorKind::End)),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.expect_word(b"true").map(|()| Json::Bool(true)),
            Some(b'f') => self.expect_word(b"false").map(|()| Json::Bool(false)),
            Some(b'n') => self.expect_word(b"null").map(|()| Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number().map(Json::Num),
            Some(_) => Err(self.error(JsonErrorKind::Unexpected)),
        }
    }

    fn nested(&self, depth: u32) -> Result<u32, JsonError> {
        let inner = depth.saturating_add(1);
        if inner > MAX_DEPTH {
            Err(self.error(JsonErrorKind::TooDeep))
        } else {
            Ok(inner)
        }
    }

    fn object(&mut self, depth: u32) -> Result<Json, JsonError> {
        let inner = self.nested(depth)?;
        self.pos += 1; // '{'
        let mut members = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(members));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.unexpected_or_end());
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.unexpected_or_end());
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value(inner)?;
            members.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(members));
                }
                _ => return Err(self.unexpected_or_end()),
            }
        }
    }

    fn array(&mut self, depth: u32) -> Result<Json, JsonError> {
        let inner = self.nested(depth)?;
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(inner)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(self.unexpected_or_end()),
            }
        }
    }

    fn unexpected_or_end(&self) -> JsonError {
        if self.peek().is_none() {
            self.error(JsonErrorKind::End)
        } else {
            self.error(JsonErrorKind::Unexpected)
        }
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            // Copy the run of plain bytes up to the next quote, backslash or control character. The
            // text is a `str`, and the run stops only at ASCII bytes, so it is whole UTF-8.
            let start = self.pos;
            while matches!(self.peek(), Some(b) if b != b'"' && b != b'\\' && b >= 0x20) {
                self.pos += 1;
            }
            if let Some(run) = self.bytes.get(start..self.pos) {
                out.push_str(
                    core::str::from_utf8(run).map_err(|_| self.error(JsonErrorKind::Escape))?,
                );
            }
            match self.bump() {
                Some(b'"') => return Ok(out),
                Some(b'\\') => self.escape(&mut out)?,
                Some(_) => {
                    self.pos -= 1;
                    return Err(self.error(JsonErrorKind::Escape));
                }
                None => return Err(self.error(JsonErrorKind::End)),
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), JsonError> {
        let c = match self.bump() {
            None => return Err(self.error(JsonErrorKind::End)),
            Some(b'"') => '"',
            Some(b'\\') => '\\',
            Some(b'/') => '/',
            Some(b'b') => '\u{8}',
            Some(b'f') => '\u{c}',
            Some(b'n') => '\n',
            Some(b'r') => '\r',
            Some(b't') => '\t',
            Some(b'u') => {
                let unit = self.hex4()?;
                if (0xD800..0xDC00).contains(&unit) {
                    // A high surrogate: a low one must follow to make a code point.
                    let save = self.pos;
                    if self.bytes.get(self.pos..self.pos.saturating_add(2)) == Some(b"\\u") {
                        self.pos += 2;
                        let low = self.hex4()?;
                        if (0xDC00..0xE000).contains(&low) {
                            let cp = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                            char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER)
                        } else {
                            self.pos = save;
                            char::REPLACEMENT_CHARACTER
                        }
                    } else {
                        char::REPLACEMENT_CHARACTER
                    }
                } else {
                    // A lone low surrogate has no `char`; JSON.stringify escapes one as \udcXX.
                    char::from_u32(unit).unwrap_or(char::REPLACEMENT_CHARACTER)
                }
            }
            Some(_) => {
                self.pos -= 1;
                return Err(self.error(JsonErrorKind::Escape));
            }
        };
        out.push(c);
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let mut unit = 0u32;
        for _ in 0..4 {
            let digit = match self.peek() {
                Some(b @ b'0'..=b'9') => u32::from(b - b'0'),
                Some(b @ b'a'..=b'f') => u32::from(b - b'a') + 10,
                Some(b @ b'A'..=b'F') => u32::from(b - b'A') + 10,
                Some(_) => return Err(self.error(JsonErrorKind::Escape)),
                None => return Err(self.error(JsonErrorKind::End)),
            };
            self.pos += 1;
            unit = unit * 16 + digit;
        }
        Ok(unit)
    }

    /// Digits from here: their count, and the significand they extend `acc` to (saturating once it
    /// passes [`MAX_EXACT`], which the fast path then refuses).
    fn digits(&mut self, acc: &mut u64) -> u32 {
        let mut count = 0u32;
        while let Some(b @ b'0'..=b'9') = self.peek() {
            self.pos += 1;
            count = count.saturating_add(1);
            *acc = if *acc > MAX_EXACT {
                *acc
            } else {
                acc.saturating_mul(10).saturating_add(u64::from(b - b'0'))
            };
        }
        count
    }

    fn number(&mut self) -> Result<Num, JsonError> {
        let start = self.pos;
        let negative = self.peek() == Some(b'-');
        if negative {
            self.pos += 1;
        }
        let mut significand = 0u64;
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error(JsonErrorKind::Number));
                }
            }
            Some(b'1'..=b'9') => {
                self.digits(&mut significand);
            }
            Some(_) => return Err(self.error(JsonErrorKind::Number)),
            None => return Err(self.error(JsonErrorKind::End)),
        }
        let mut exp10: i64 = 0;
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let fraction = self.digits(&mut significand);
            if fraction == 0 {
                return Err(self.unexpected_number());
            }
            exp10 -= i64::from(fraction);
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            let negative_exp = match self.peek() {
                Some(b'-') => {
                    self.pos += 1;
                    true
                }
                Some(b'+') => {
                    self.pos += 1;
                    false
                }
                _ => false,
            };
            let mut e = 0u64;
            if self.digits(&mut e) == 0 {
                return Err(self.unexpected_number());
            }
            // `digits` saturates past 2^53, far beyond any exponent the fast path takes.
            let e = i64::try_from(e).unwrap_or(i64::MAX);
            exp10 = if negative_exp {
                exp10.saturating_sub(e)
            } else {
                exp10.saturating_add(e)
            };
        }
        let at = Span {
            start: u32::try_from(start).unwrap_or(u32::MAX),
            end: u32::try_from(self.pos).unwrap_or(u32::MAX),
        };
        Ok(Num {
            value: fast_path(significand, exp10, negative),
            at,
        })
    }

    fn unexpected_number(&self) -> JsonError {
        if self.peek().is_none() {
            self.error(JsonErrorKind::End)
        } else {
            self.error(JsonErrorKind::Number)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(text: &str) -> Option<f64> {
        match parse(text) {
            Ok(Json::Num(n)) => n.value,
            other => panic!("{text}: {other:?}"),
        }
    }

    /// Integers and short decimals read exactly: the same `f64` as the standard parser.
    #[test]
    fn numbers_read_exactly_on_the_fast_path() {
        for text in [
            "0",
            "-0",
            "1",
            "1500",
            "-1",
            "-12",
            "0.2",
            "0.05",
            "2.4",
            "1.5",
            "0.1",
            "0.35",
            "10000000",
            "9007199254740992",
            "1e21",
            "1e+21",
            "1e-7",
            "1.5e-7",
            "123.456",
            "-0.5",
            "1E3",
            "2e22",
        ] {
            let want: f64 = text.parse().expect("std parses it");
            let got = num(text).unwrap_or_else(|| panic!("{text} left the fast path"));
            assert_eq!(got.to_bits(), want.to_bits(), "{text}");
        }
    }

    /// Exhaustive over small significands and every exponent the fast path takes: always the
    /// standard parser's bits.
    #[test]
    fn the_fast_path_matches_the_standard_parser() {
        for m in (0u64..2000).chain([MAX_EXACT - 1, MAX_EXACT]) {
            for e in -22i64..=22 {
                let text = format!("{m}e{e}");
                let want: f64 = text.parse().expect("std parses it");
                assert_eq!(num(&text).map(f64::to_bits), Some(want.to_bits()), "{text}");
            }
        }
    }

    /// What the fast path cannot round exactly reads as `None`, never as a wrong value.
    #[test]
    fn numbers_off_the_fast_path_are_none() {
        for text in [
            "9007199254740993",
            "0.30000000000000004",
            "1e23",
            "1e-23",
            "1e400",
            "123456789012345678901",
        ] {
            assert_eq!(num(text), None, "{text}");
        }
    }

    #[test]
    fn numbers_keep_their_text() {
        let text = "[1500,0.2,-3e5]";
        let parsed = parse(text).expect("valid");
        let spans: Vec<&str> = parsed
            .elements()
            .expect("array")
            .iter()
            .map(|v| match v {
                Json::Num(n) => n.at.of(text).expect("in the text"),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(spans, ["1500", "0.2", "-3e5"]);
    }

    #[test]
    fn values_nest_and_keep_member_order() {
        let text =
            r#"{"work":{"UO":{"harvest":3}},"list":["a",true,false,null],"empty":{},"none":[]}"#;
        let v = parse(text).expect("valid");
        let members: Vec<&str> = v
            .members()
            .expect("object")
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(members, ["work", "list", "empty", "none"]);
        let harvest = v
            .get("work")
            .and_then(|w| w.get("UO"))
            .and_then(|u| u.get("harvest"));
        assert!(matches!(harvest, Some(Json::Num(Num { value: Some(x), .. })) if *x == 3.0));
        assert_eq!(
            v.get("list").and_then(Json::elements),
            Some(
                &[
                    Json::Str("a".into()),
                    Json::Bool(true),
                    Json::Bool(false),
                    Json::Null
                ][..]
            )
        );
        assert_eq!(v.get("empty"), Some(&Json::Obj(Vec::new())));
        assert_eq!(v.get("none"), Some(&Json::Arr(Vec::new())));
        assert_eq!(v.get("missing"), None);
        assert_eq!(Json::Null.get("x"), None);
    }

    #[test]
    fn strings_unescape() {
        let v = parse(r#""a\"b\\c\/d\b\f\n\r\t\u0041\u00e9\ud83d\ude00é""#).expect("valid");
        assert_eq!(v.as_str(), Some("a\"b\\c/d\u{8}\u{c}\n\r\tAé😀é"));
        // Lone surrogates (JSON.stringify escapes them since ES2019) become U+FFFD.
        assert_eq!(
            parse(r#""\udc00x""#).expect("valid").as_str(),
            Some("\u{fffd}x")
        );
        assert_eq!(
            parse(r#""\ud800x""#).expect("valid").as_str(),
            Some("\u{fffd}x")
        );
        assert_eq!(
            parse(r#""\ud800\u0041""#).expect("valid").as_str(),
            Some("\u{fffd}A")
        );
    }

    #[test]
    fn whitespace_around_tokens_is_allowed() {
        let v = parse(" { \"a\" : [ 1 , 2 ] }\n").expect("valid");
        assert_eq!(
            v.get("a").and_then(Json::elements).map(<[Json]>::len),
            Some(2)
        );
    }

    #[test]
    fn malformed_text_is_refused_with_a_reason() {
        use JsonErrorKind::*;
        let cases: [(&str, JsonErrorKind); 22] = [
            ("", End),
            ("   ", End),
            ("{", End),
            ("[1,", End),
            ("\"abc", End),
            ("tru", End),
            ("1 2", Trailing),
            ("{}x", Trailing),
            ("01", Number),
            ("-", End),
            ("-x", Number),
            ("1.", End),
            ("1.e5", Number),
            ("1e", End),
            ("1e+", End),
            ("{\"a\" 1}", Unexpected),
            ("{1:2}", Unexpected),
            ("[1;2]", Unexpected),
            ("\"\\x\"", Escape),
            ("\"\\u12g4\"", Escape),
            ("\"a\u{1}b\"", Escape),
            ("undefined", Unexpected),
        ];
        for (text, kind) in cases {
            assert_eq!(parse(text).map_err(|e| e.kind), Err(kind), "{text:?}");
        }
    }

    #[test]
    fn nesting_is_limited() {
        let ok = format!(
            "{}{}",
            "[".repeat(MAX_DEPTH as usize),
            "]".repeat(MAX_DEPTH as usize)
        );
        assert!(parse(&ok).is_ok());
        let deep = format!(
            "{}{}",
            "[".repeat(MAX_DEPTH as usize + 1),
            "]".repeat(MAX_DEPTH as usize + 1)
        );
        assert_eq!(
            parse(&deep).map_err(|e| e.kind),
            Err(JsonErrorKind::TooDeep)
        );
        let objects = "{\"a\":".repeat(MAX_DEPTH as usize + 1);
        assert_eq!(
            parse(&objects).map_err(|e| e.kind),
            Err(JsonErrorKind::TooDeep)
        );
    }

    #[test]
    fn errors_name_the_byte() {
        let e = parse("[1,x]").expect_err("invalid");
        assert_eq!(
            e,
            JsonError {
                kind: JsonErrorKind::Unexpected,
                at: 3
            }
        );
        assert_eq!(e.to_string(), "unexpected character at byte 3");
    }
}
