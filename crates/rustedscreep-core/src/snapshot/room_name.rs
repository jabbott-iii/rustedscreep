//! Room names parsed from bytes, with the semantics of screeps-game-api 0.23.5's
//! `RoomName::from_str` (VERIFICATION_LOG V114), which the snapshot used before. That parser
//! slices the `str`, which links `core::str::slice_error_fail` and with it Unicode and `Debug`
//! tables into the release wasm (8139 bytes of it, V114). This one reads bytes with `get`, so it
//! links neither and cannot panic. The tests compare it with the crate's parser on every canonical
//! name, edge cases and seeded random strings, so a crate bump that changes the format fails here.

use rustedscreep_consts::RoomName;
use rustedscreep_consts::extra::HALF_WORLD_SIZE as HALF_WORLD;

/// Parses a room name such as `W1N1` or `e0s0`, or returns `None` where `RoomName::from_str`
/// returns an error.
///
/// The format, as the crate parses it (V114): `E`/`e` or `W`/`w`; then the text from the second
/// character up to the first `N`/`n`/`S`/`s` at byte 2 or later, read as an `i32`; that letter;
/// then the rest, read as an `i32`. `W`n and `N`n map to -n-1, `E`n and `S`n to n, and both
/// coordinates must lie in -128..128. As with `i32::from_str`, a number may carry one leading `+`
/// or `-` and any number of leading zeros (so `E-5N5` is `W4N5`). `sim` is a name only when the
/// crate's `sim` feature is on, which no build enables yet (V87), so it is refused here as there; a
/// build that turns that feature on must make this return `RoomName::from_packed(0)` for `sim`
/// (the crate's W127N127, V114).
///
/// One input differs in a way that never ships: for `i32::MIN` under `W` or `N` (`W-2147483648N0`)
/// the crate's `-n - 1` overflows, so it panics with overflow checks on (the test profile) and
/// returns an error in release, which is what the wasm build uses. This returns `None` in both.
///
/// A byte search gives the same split as the crate's search over chars: bytes of a multi-byte
/// character are all 0x80 or above, so they never match an ASCII letter, and such a character
/// cannot be part of a number.
pub fn parse_room_name(name: &[u8]) -> Option<RoomName> {
    let east = match name.first()? {
        b'E' | b'e' => true,
        b'W' | b'w' => false,
        _ => return None,
    };
    let split = name
        .iter()
        .enumerate()
        .skip(2)
        .find(|&(_, b)| matches!(b, b'N' | b'n' | b'S' | b's'))
        .map(|(i, _)| i)?;
    let south = matches!(name.get(split), Some(b'S' | b's'));
    let x = coordinate(name.get(1..split)?)?;
    let y = coordinate(name.get(split + 1..)?)?;
    let room_x = if east { x } else { -x - 1 };
    let room_y = if south { y } else { -y - 1 };
    let byte = |c: i32| u16::try_from(c + HALF_WORLD).ok().filter(|&b| b < 256);
    Some(RoomName::from_packed((byte(room_x)? << 8) | byte(room_y)?))
}

/// A number as `i32::from_str` reads it, or `None` when that fails or the value cannot be a room
/// coordinate. Either way the crate's parse fails: a value outside -128..=127 is out of bounds for
/// both directions (`W`n gives -n-1), so its exact size does not matter and the digits are only
/// checked, not summed, once it is past that.
fn coordinate(text: &[u8]) -> Option<i32> {
    let (negative, digits) = match text {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, text),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut value = 0i32;
    for &d in digits {
        value = value * 10 + i32::from(d - b'0');
        if value > HALF_WORLD {
            return None;
        }
    }
    let value = if negative { -value } else { value };
    (-HALF_WORLD..HALF_WORLD).contains(&value).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crate_parse(s: &str) -> Option<RoomName> {
        s.parse::<RoomName>().ok()
    }

    fn same(s: &str) {
        assert_eq!(parse_room_name(s.as_bytes()), crate_parse(s), "{s:?}");
    }

    /// Every room the crate can represent, by its canonical name in both cases: the same name as
    /// the crate gives, and it prints back to the same string.
    #[test]
    fn every_canonical_name_matches_the_crate() {
        for rx in -HALF_WORLD..HALF_WORLD {
            for ry in -HALF_WORLD..HALF_WORLD {
                let (h, x) = if rx < 0 { ('W', -rx - 1) } else { ('E', rx) };
                let (v, y) = if ry < 0 { ('N', -ry - 1) } else { ('S', ry) };
                let name = format!("{h}{x}{v}{y}");
                let parsed = parse_room_name(name.as_bytes()).expect("canonical");
                assert_eq!(Some(parsed), crate_parse(&name), "{name}");
                assert_eq!(parsed.to_string(), name);
                same(&name.to_lowercase());
            }
        }
    }

    #[test]
    fn edge_cases_match_the_crate() {
        let cases = [
            "",
            "E",
            "W",
            "N",
            "E5",
            "E5N",
            "EN5",
            "ES5",
            "E5N5",
            "W0N0",
            "E0S0",
            "w127n127",
            "E127S127",
            "W127N127",
            "E128S0",
            "W128N0",
            "E0S128",
            "W0N128",
            "E129S0",
            "E-1S0",
            "E-128S0",
            "E-129S0",
            "W-1N0",
            "W-128N-128",
            "E+5N+5",
            "E++5N5",
            "E+-5N5",
            "E-N5",
            "E+N5",
            "E5N-",
            "E5N+",
            "E05N005",
            "E0000000000005N5",
            "E99999999999N5",
            "E2147483647N5",
            "E2147483648N5",
            "E-2147483648N5",
            "E5N5x",
            "E5X5",
            "5N5",
            "€5N5",
            "E€N5",
            "E5N€",
            "E5€N5",
            " E5N5",
            "E5N5 ",
            "E5 N5",
            "E5N 5",
            "sim",
            "SIM",
            "E5N5\0",
            "E5NS5",
            "E5SN5",
            "E5N5N5",
            "EE5N5",
            "W1N1W1N1",
            "E5n5",
            "e5S5",
            "En",
            "Es5",
        ];
        for s in cases {
            same(s);
        }
    }

    /// Where the crate's `-n - 1` overflows (it panics in the test profile, so it is not called
    /// here), this parser refuses the name, as the crate's release build does (V114).
    #[test]
    fn i32_min_under_w_or_n_is_refused() {
        for s in [
            "W-2147483648N0",
            "w-2147483648n0",
            "E0N-2147483648",
            "W-2147483648N-2147483648",
            "E-2147483648S0",
        ] {
            assert_eq!(parse_room_name(s.as_bytes()), None, "{s}");
        }
    }

    /// Seeded random strings compared with the crate's parser: half are free strings over the
    /// characters the format uses (and some it does not), half are shaped like a name (a letter, a
    /// number-like token, `N` or `S`, a number-like token), so that many parse and their values are
    /// compared too.
    #[test]
    fn random_strings_match_the_crate() {
        const ALPHABET: [&str; 20] = [
            "E", "W", "N", "S", "e", "w", "n", "s", "0", "1", "2", "7", "9", "+", "-", " ", "x",
            "€", "😀", "\0",
        ];
        const TOKEN: [&str; 16] = [
            "0", "1", "5", "9", "12", "127", "128", "129", "0127", "00", "+", "-", "x", "€", "",
            "N",
        ];
        let mut state = 0x5eed_u64;
        let mut next = |n: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(state >> 33).expect("fits") % n
        };
        let mut accepted = 0;
        for i in 0..200_000 {
            let mut s = String::new();
            if i % 2 == 0 {
                for _ in 0..next(9) {
                    s.push_str(ALPHABET[next(ALPHABET.len())]);
                }
            } else {
                s.push_str(["E", "W", "e", "w"][next(4)]);
                for _ in 0..=next(2) {
                    s.push_str(TOKEN[next(TOKEN.len())]);
                }
                s.push_str(["N", "S", "n", "s"][next(4)]);
                for _ in 0..=next(2) {
                    s.push_str(TOKEN[next(TOKEN.len())]);
                }
            }
            same(&s);
            accepted += usize::from(crate_parse(&s).is_some());
        }
        assert!(
            accepted > 10_000,
            "enough strings parse to compare values ({accepted})"
        );
    }
}
