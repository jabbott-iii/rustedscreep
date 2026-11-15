//! The server's constants as the wasm side read them: each upper-case own name of the global object
//! with the JSON text of its value. Lookups mark a constant as read, so the check can name the server
//! constants that nothing checked.

use core::cell::Cell;

use super::json::{self, Json, JsonError};

/// Whether `name` is shaped like a game constant: an upper-case letter, then upper-case letters,
/// digits and underscores (`OK`, `TOP_LEFT`, `RESOURCE_UTRIUM_HYDRIDE`). Every top-level key of the
/// engine's constants.js is (VERIFICATION_LOG, P1.11); `JSON` is the one built-in that is too.
pub fn is_constant_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z'))
        && bytes.all(|b| matches!(b, b'A'..=b'Z' | b'0'..=b'9' | b'_'))
}

/// One constant's value as read.
#[derive(Debug, PartialEq)]
pub enum Read {
    Json(Json),
    /// `JSON.stringify` gave no text: an accessor (never called), a function, undefined or a symbol.
    NoJson,
    /// The text is not JSON (which `JSON.stringify` never writes).
    Bad(JsonError),
}

/// A server constant.
#[derive(Debug)]
pub struct Const {
    pub name: String,
    /// The JSON text (empty for [`Read::NoJson`]); a number's span points into it.
    pub text: String,
    pub read: Read,
    used: Cell<bool>,
}

impl Const {
    pub fn json(&self) -> Option<&Json> {
        match &self.read {
            Read::Json(v) => Some(v),
            _ => None,
        }
    }
}

/// Every server constant read, looked up by name.
#[derive(Debug, Default)]
pub struct Server {
    consts: Vec<Const>,
    /// Indices into `consts`, sorted by name (filled by [`Self::seal`]).
    sorted: Vec<u32>,
}

impl Server {
    /// Adds a constant with the JSON text of its value (`None`: there was none).
    pub fn push(&mut self, name: String, text: Option<String>) {
        let (text, read) = match text {
            None => (String::new(), Read::NoJson),
            Some(t) => {
                let read = match json::parse(&t) {
                    Ok(v) => Read::Json(v),
                    Err(e) => Read::Bad(e),
                };
                (t, read)
            }
        };
        self.consts.push(Const {
            name,
            text,
            read,
            used: Cell::new(false),
        });
    }

    /// Builds the name index; call once every constant is in. A name read twice (the engine never
    /// gives one) keeps its first value.
    pub fn seal(&mut self) {
        let n = u32::try_from(self.consts.len()).unwrap_or(u32::MAX);
        self.sorted = (0..n).collect();
        let consts = &self.consts;
        self.sorted.sort_by(|&a, &b| {
            let name = |i: u32| {
                usize::try_from(i)
                    .ok()
                    .and_then(|i| consts.get(i))
                    .map_or("", |c| c.name.as_str())
            };
            name(a).cmp(name(b)).then(a.cmp(&b))
        });
        self.sorted.dedup_by(|later, first| {
            let name = |i: &mut u32| {
                usize::try_from(*i)
                    .ok()
                    .and_then(|i| consts.get(i))
                    .map(|c| c.name.as_str())
            };
            name(later) == name(first)
        });
    }

    fn at(&self, sorted_pos: usize) -> Option<&Const> {
        let i = usize::try_from(*self.sorted.get(sorted_pos)?).ok()?;
        self.consts.get(i)
    }

    /// The first position in the sorted index whose name is not below `name`.
    fn lower_bound(&self, name: &str) -> usize {
        self.sorted.partition_point(|&i| {
            match usize::try_from(i).ok().and_then(|i| self.consts.get(i)) {
                Some(c) => c.name.as_str() < name,
                None => false,
            }
        })
    }

    /// The constant `name`, marked as read.
    pub fn take(&self, name: &str) -> Option<&Const> {
        let c = self.at(self.lower_bound(name)).filter(|c| c.name == name)?;
        c.used.set(true);
        Some(c)
    }

    /// Whether the constant `name` was read (without marking it).
    pub fn has(&self, name: &str) -> bool {
        self.at(self.lower_bound(name))
            .is_some_and(|c| c.name == name)
    }

    /// Every constant whose name starts with `prefix`, in name order, marked as read.
    pub fn take_prefix<'s>(&'s self, prefix: &'s str) -> impl Iterator<Item = &'s Const> + 's {
        let start = self.lower_bound(prefix);
        (start..self.sorted.len())
            .map_while(move |p| self.at(p).filter(|c| c.name.starts_with(prefix)))
            .inspect(|c| c.used.set(true))
    }

    /// The constants nothing has read, in name order.
    pub fn unread(&self) -> impl Iterator<Item = &Const> {
        (0..self.sorted.len())
            .filter_map(|p| self.at(p))
            .filter(|c| !c.used.get())
    }

    /// Distinct constants (after [`Self::seal`]).
    pub fn len(&self) -> usize {
        self.sorted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_names_are_upper_case_words() {
        for ok in [
            "OK",
            "TOP_LEFT",
            "RESOURCE_UTRIUM_HYDRIDE",
            "JSON",
            "A",
            "X1",
        ] {
            assert!(is_constant_name(ok), "{ok}");
        }
        for not in [
            "", "Game", "Memory", "_", "NaN", "Infinity", "1A", "_OK", "OK-1", "Ö", "global",
        ] {
            assert!(!is_constant_name(not), "{not}");
        }
    }

    fn server(pairs: &[(&str, Option<&str>)]) -> Server {
        let mut s = Server::default();
        for (name, text) in pairs {
            s.push((*name).into(), text.map(Into::into));
        }
        s.seal();
        s
    }

    #[test]
    fn lookups_find_names_and_mark_them() {
        let s = server(&[
            ("WORK", Some("\"work\"")),
            ("STRUCTURE_SPAWN", Some("\"spawn\"")),
            ("STRUCTURE_ROAD", Some("\"road\"")),
            ("OK", Some("0")),
            ("BROKEN", Some("{")),
            ("FUNC", None),
            ("STRUCTURES", Some("1")),
        ]);
        assert_eq!(s.len(), 7);
        assert_eq!(
            s.take("WORK").and_then(Const::json).and_then(Json::as_str),
            Some("work")
        );
        assert!(s.take("MISSING").is_none());
        let family: Vec<&str> = s
            .take_prefix("STRUCTURE_")
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(family, ["STRUCTURE_ROAD", "STRUCTURE_SPAWN"]);
        assert!(matches!(
            s.take("BROKEN").map(|c| &c.read),
            Some(Read::Bad(_))
        ));
        assert_eq!(s.take("FUNC").map(|c| &c.read), Some(&Read::NoJson));
        let unread: Vec<&str> = s.unread().map(|c| c.name.as_str()).collect();
        assert_eq!(unread, ["OK", "STRUCTURES"]);
    }

    #[test]
    fn a_repeated_name_keeps_its_first_value() {
        let s = server(&[("A", Some("1")), ("B", Some("2")), ("A", Some("3"))]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.take("A").map(|c| c.text.as_str()), Some("1"));
        assert_eq!(s.unread().count(), 1);
    }

    #[test]
    fn an_empty_server_finds_nothing() {
        let s = server(&[]);
        assert!(s.is_empty());
        assert!(s.take("OK").is_none());
        assert_eq!(s.take_prefix("ERR_").count(), 0);
    }
}
