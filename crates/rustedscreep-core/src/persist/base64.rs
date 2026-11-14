//! Base64 text for RawMemory segments (ADR-004): the RFC 4648 section 4 alphabet, without padding,
//! decoded strictly. Every character is ASCII, so one character is one UTF-16 code unit: the
//! segment limit, which the driver checks as JS string `.length` (V41), counts characters here, and
//! the text takes the wasm-bindgen glue's ASCII fast path into wasm memory (VERIFICATION_LOG V123).
//!
//! Strict decoding refuses padding, characters outside the alphabet, a length of 1 mod 4 and
//! non-zero bits after the last byte, so every byte string has exactly one accepted text.

/// RFC 4648 section 4, table 1 (<https://www.rfc-editor.org/rfc/rfc4648#section-4>).
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Marks a byte outside the alphabet in [`DECODE`].
const INVALID: u8 = 0xff;

/// The value of character `b` in [`ALPHABET`], or [`INVALID`].
const fn value_of(b: u8) -> u8 {
    match b {
        b'A'..=b'Z' => b - b'A',
        b'a'..=b'z' => b - b'a' + 26,
        b'0'..=b'9' => b - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => INVALID,
    }
}

/// [`value_of`] for every byte (every byte >= 0x80 is [`INVALID`]), so decoding is one load per
/// character. Built without indexing: each slot is the head of the remaining slice.
const DECODE: [u8; 256] = {
    let mut table = [INVALID; 256];
    let mut rest: &mut [u8] = &mut table;
    let mut b: u8 = 0;
    while let Some((slot, tail)) = rest.split_first_mut() {
        *slot = value_of(b);
        rest = tail;
        b = b.wrapping_add(1);
    }
    table
};

/// Why a text is not strict base64.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base64Error {
    /// The character at this index (in bytes of the text) is outside the alphabet.
    Char(usize),
    /// The length is 1 mod 4, which no byte string encodes to.
    Length,
    /// The last character carries non-zero bits after the last byte (not the canonical text).
    TrailingBits,
}

/// Characters (= UTF-16 code units) that `bytes` bytes encode to: 4 per 3 bytes, plus 2 for one
/// remaining byte and 3 for two.
pub const fn encoded_len(bytes: usize) -> usize {
    bytes / 3 * 4
        + match bytes % 3 {
            0 => 0,
            1 => 2,
            _ => 3,
        }
}

/// Most bytes that `chars` characters can carry: the inverse of [`encoded_len`], rounded down.
pub const fn decoded_max(chars: usize) -> usize {
    chars / 4 * 3
        + match chars % 4 {
            0 | 1 => 0,
            2 => 1,
            _ => 2,
        }
}

/// The character for the low 6 bits of `v`.
#[inline]
fn symbol(v: u32) -> char {
    // The mask keeps the index below 64, so the fallback never runs.
    char::from(ALPHABET.get((v & 63) as usize).copied().unwrap_or(b'A'))
}

/// The value of character `b`, or [`INVALID`].
#[inline]
fn value(b: u8) -> u8 {
    DECODE.get(usize::from(b)).copied().unwrap_or(INVALID)
}

/// A streaming encoder: [`Encoder::push`] any number of byte slices, then [`Encoder::finish`]. The
/// text equals the encoding of the slices joined, so a header and a payload need no joined copy.
#[derive(Debug, Default)]
pub struct Encoder {
    /// Up to two bytes waiting for a full group of three.
    carry: [u8; 2],
    carried: usize,
}

impl Encoder {
    /// Appends the characters for `bytes` (after any carried bytes) to `out`.
    pub fn push(&mut self, mut bytes: &[u8], out: &mut String) {
        // Complete a carried group first.
        while self.carried > 0 {
            let Some((&b, rest)) = bytes.split_first() else {
                return;
            };
            bytes = rest;
            if self.carried == 1 {
                self.carry[1] = b;
                self.carried = 2;
            } else {
                let [a, b1] = self.carry;
                group(a, b1, b, out);
                self.carried = 0;
            }
        }
        let (groups, rest) = bytes.as_chunks::<3>();
        for &[a, b, c] in groups {
            group(a, b, c, out);
        }
        match *rest {
            [a] => {
                self.carry = [a, 0];
                self.carried = 1;
            }
            [a, b] => {
                self.carry = [a, b];
                self.carried = 2;
            }
            _ => {}
        }
    }

    /// Appends the characters for the carried bytes, if any, without padding.
    pub fn finish(self, out: &mut String) {
        let [a, b] = self.carry;
        let n = (u32::from(a) << 16) | (u32::from(b) << 8);
        match self.carried {
            1 => {
                out.push(symbol(n >> 18));
                out.push(symbol(n >> 12));
            }
            2 => {
                out.push(symbol(n >> 18));
                out.push(symbol(n >> 12));
                out.push(symbol(n >> 6));
            }
            _ => {}
        }
    }
}

#[inline]
fn group(a: u8, b: u8, c: u8, out: &mut String) {
    let n = (u32::from(a) << 16) | (u32::from(b) << 8) | u32::from(c);
    out.push(symbol(n >> 18));
    out.push(symbol(n >> 12));
    out.push(symbol(n >> 6));
    out.push(symbol(n));
}

/// Appends the encoding of `bytes` to `out`.
pub fn encode(bytes: &[u8], out: &mut String) {
    let mut enc = Encoder::default();
    enc.push(bytes, out);
    enc.finish(out);
}

/// Appends the bytes `text` encodes to `out`. On an error `out` keeps what was appended so far.
pub fn decode(text: &[u8], out: &mut Vec<u8>) -> Result<(), Base64Error> {
    if text.len() % 4 == 1 {
        return Err(Base64Error::Length);
    }
    out.reserve(decoded_max(text.len()));
    let (quads, rest) = text.as_chunks::<4>();
    let mut at = 0;
    for quad in quads {
        let v = quad.map(value);
        if v.contains(&INVALID) {
            return Err(first_invalid(quad, at));
        }
        let [a, b, c, d] = v.map(u32::from);
        let n = (a << 18) | (b << 12) | (c << 6) | d;
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
        at += 4;
    }
    if rest.iter().any(|&b| value(b) == INVALID) {
        return Err(first_invalid(rest, at));
    }
    match *rest {
        [a, b] => {
            let (a, b) = (value(a), value(b));
            let n = (u32::from(a) << 18) | (u32::from(b) << 12);
            if n & 0xffff != 0 {
                return Err(Base64Error::TrailingBits);
            }
            out.push((n >> 16) as u8);
        }
        [a, b, c] => {
            let (a, b, c) = (value(a), value(b), value(c));
            let n = (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6);
            if n & 0xff != 0 {
                return Err(Base64Error::TrailingBits);
            }
            out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8]);
        }
        _ => {}
    }
    Ok(())
}

#[cold]
fn first_invalid(chunk: &[u8], at: usize) -> Base64Error {
    let i = chunk.iter().position(|&b| value(b) == INVALID).unwrap_or(0);
    Base64Error::Char(at + i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(bytes: &[u8]) -> String {
        let mut s = String::new();
        encode(bytes, &mut s);
        s
    }

    fn dec(text: &str) -> Result<Vec<u8>, Base64Error> {
        let mut v = Vec::new();
        decode(text.as_bytes(), &mut v).map(|()| v)
    }

    /// RFC 4648 section 10 test vectors, with the padding removed (this encoder writes none).
    #[test]
    fn rfc_4648_vectors() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(enc(plain.as_bytes()), coded);
            assert_eq!(dec(coded).as_deref(), Ok(plain.as_bytes()));
        }
    }

    /// Every byte value at every position of a group survives, and the lengths follow the formulas.
    #[test]
    fn round_trips_every_byte_at_every_offset() {
        for len in 0..=9usize {
            for byte in 0..=255u8 {
                let data: Vec<u8> = (0..len)
                    .map(|i| byte.wrapping_add((i as u8).wrapping_mul(37)))
                    .collect();
                let text = enc(&data);
                assert_eq!(text.len(), encoded_len(len));
                assert!(text.is_ascii());
                assert_eq!(dec(&text).as_deref(), Ok(data.as_slice()));
                assert_eq!(decoded_max(text.len()), len);
            }
        }
    }

    /// Pushing a byte string in pieces gives the same text as pushing it whole.
    #[test]
    fn the_streaming_encoder_ignores_piece_boundaries() {
        let data: Vec<u8> = (0..50u8).map(|i| i.wrapping_mul(97)).collect();
        let whole = enc(&data);
        for a in 0..data.len() {
            for b in a..data.len() {
                let mut s = String::new();
                let mut e = Encoder::default();
                e.push(&data[..a], &mut s);
                e.push(&data[a..b], &mut s);
                e.push(&data[b..], &mut s);
                e.finish(&mut s);
                assert_eq!(s, whole, "split at {a} and {b}");
            }
        }
    }

    #[test]
    fn decoding_is_strict() {
        // Padding, the URL-safe alphabet, whitespace and non-ASCII are refused where they stand.
        assert_eq!(dec("Zg=="), Err(Base64Error::Char(2)));
        assert_eq!(dec("Zm9-"), Err(Base64Error::Char(3)));
        assert_eq!(dec("Zm9v_A"), Err(Base64Error::Char(4)));
        assert_eq!(dec("Zm 9"), Err(Base64Error::Char(2)));
        assert_eq!(dec("Zm9vé"), Err(Base64Error::Char(4)));
        // A length of 1 mod 4 encodes nothing.
        assert_eq!(dec("Z"), Err(Base64Error::Length));
        assert_eq!(dec("Zm9vY"), Err(Base64Error::Length));
        // "Zh" and "Zm9" with bits after the last byte are not canonical ("Zg" and "Zm8" are).
        assert_eq!(dec("Zh"), Err(Base64Error::TrailingBits));
        assert_eq!(dec("Zm9"), Err(Base64Error::TrailingBits));
        assert_eq!(dec("Zg").as_deref(), Ok(&b"f"[..]));
    }

    /// Every character outside the alphabet is refused, and only the 64 inside are accepted.
    #[test]
    fn the_decode_table_matches_the_alphabet() {
        let accepted: Vec<u8> = (0..=255u8).filter(|&b| value(b) != INVALID).collect();
        assert!((0..=255u8).all(|b| value(b) == value_of(b)));
        let mut alphabet = ALPHABET.to_vec();
        alphabet.sort_unstable();
        assert_eq!(accepted, alphabet);
        for (i, &b) in ALPHABET.iter().enumerate() {
            assert_eq!(usize::from(value(b)), i);
        }
    }
}
