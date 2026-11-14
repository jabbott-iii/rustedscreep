//! The segment frame (ADR-004, container format 1): what one RawMemory segment holds.
//!
//! ```text
//! text    = "rs" version encoding body       (ASCII; one character = one UTF-16 code unit)
//! version = "1"                              (this layout)
//! encoding= "b"                              (body is base64: RFC 4648 section 4, no padding)
//! body    = base64(header || payload)
//! header  = kind:u16 schema:u16 written:u32 length:u32 crc:u32     (16 bytes, little-endian)
//! ```
//!
//! - `kind` names the record's owner (see [`super::kind`]); a segment that holds another owner's
//!   record is refused, so a change to the segment table is a rebuild, never a misread.
//! - `schema` is the owner's payload layout version; the owner decides what a mismatch means.
//! - `written` is `Game.time` of the write, for staleness checks and the console.
//! - `length` is the payload length in bytes, so a truncated text is refused before the CRC.
//! - `crc` is CRC-32/ISO-HDLC over the first 12 header bytes and then the payload.
//!
//! The tag stays outside the base64, so the console shows which format a segment holds and a
//! decoder knows the encoding before it decodes; another encoding or header layout gets another
//! letter or digit, and old text stays readable or is refused, never misread.

use super::base64::{self, Base64Error};
use super::crc32::Crc32;
use rustedscreep_consts::MEMORY_SEGMENT_SIZE_LIMIT;

/// The first two characters of every frame.
pub const MAGIC: &str = "rs";
/// Container format version (this module's layout).
pub const VERSION: u8 = b'1';
/// The body encoding: base64 (ADR-004).
pub const ENCODING_BASE64: u8 = b'b';
/// `MAGIC`, `VERSION` and `ENCODING_BASE64`: the start of every frame this module writes.
pub const TAG: &str = "rs1b";
/// Header bytes before the payload.
pub const HEADER_LEN: usize = 16;
/// Header bytes the CRC covers (all but the CRC itself).
const CRC_COVERED: usize = 12;

/// Most UTF-16 code units a segment may hold (V41: the driver checks `.length <= 100 * 1024`).
///
/// Uses: `rustedscreep_consts::MEMORY_SEGMENT_SIZE_LIMIT`.
pub const MAX_TEXT_UNITS: usize = MEMORY_SEGMENT_SIZE_LIMIT as usize;

/// Most payload bytes one frame carries: what base64 fits after the tag, less the header.
/// 76781 bytes at the 102400-unit limit.
///
/// Uses: `rustedscreep_consts::MEMORY_SEGMENT_SIZE_LIMIT`.
pub const MAX_PAYLOAD: usize = base64::decoded_max(MAX_TEXT_UNITS - TAG.len()) - HEADER_LEN;

/// Code units of the frame for a payload of `payload` bytes.
pub const fn text_units(payload: usize) -> usize {
    TAG.len() + base64::encoded_len(HEADER_LEN + payload)
}

/// The decoded header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: u16,
    pub schema: u16,
    pub written: u32,
    /// Payload length in bytes.
    pub length: u32,
}

impl Header {
    /// Refuses a frame that holds another owner's record or another payload layout.
    pub fn expect(&self, kind: u16, schema: u16) -> Result<(), DecodeError> {
        if self.kind != kind {
            return Err(DecodeError::Kind {
                expected: kind,
                found: self.kind,
            });
        }
        if self.schema != schema {
            return Err(DecodeError::Schema {
                expected: schema,
                found: self.schema,
            });
        }
        Ok(())
    }
}

/// A payload too large for one segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLarge {
    pub payload: usize,
}

/// Why a segment's text is not a usable frame. Every case means the owner rebuilds (ROADMAP P1.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The text is empty: a segment never written, or written as "" (the driver cannot tell them
    /// apart, V117).
    Empty,
    /// The text does not start with [`MAGIC`] or is shorter than the tag: not one of ours.
    NotOurs,
    /// A container version this build cannot read.
    Version(u8),
    /// A body encoding this build cannot read.
    Encoding(u8),
    /// The body is not strict base64.
    Base64(Base64Error),
    /// The body is shorter than the header.
    Short,
    /// The header's length differs from the payload's.
    Length { header: u32, actual: usize },
    /// The CRC differs: the text was changed after it was written.
    Checksum { header: u32, actual: u32 },
    /// Another owner's record.
    Kind { expected: u16, found: u16 },
    /// Another payload layout of the same owner.
    Schema { expected: u16, found: u16 },
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Empty => f.write_str("empty (never written or cleared)"),
            Self::NotOurs => f.write_str("not a rustedscreep frame"),
            Self::Version(v) => {
                f.write_str("unknown container version ")?;
                byte(f, v)
            }
            Self::Encoding(e) => {
                f.write_str("unknown encoding ")?;
                byte(f, e)
            }
            Self::Base64(Base64Error::Char(at)) => {
                write!(f, "invalid base64 at body byte {at}")
            }
            Self::Base64(Base64Error::Length) => f.write_str("base64 body of impossible length"),
            Self::Base64(Base64Error::TrailingBits) => f.write_str("non-canonical base64 end"),
            Self::Short => f.write_str("body shorter than the header"),
            Self::Length { header, actual } => {
                write!(f, "length {header} in the header, {actual} in the body")
            }
            Self::Checksum { header, actual } => {
                write!(
                    f,
                    "CRC-32 {header:08x} in the header, {actual:08x} computed"
                )
            }
            Self::Kind { expected, found } => write!(f, "kind {found}, expected {expected}"),
            Self::Schema { expected, found } => {
                write!(f, "schema {found}, expected {expected}")
            }
        }
    }
}

/// Writes a tag byte as `'c'` when it is printable ASCII and as `0xNN` otherwise. (`{:?}` on a `char`
/// would link the Unicode escape tables into the wasm.)
fn byte(f: &mut core::fmt::Formatter<'_>, b: u8) -> core::fmt::Result {
    if b.is_ascii_graphic() {
        write!(f, "'{}'", char::from(b))
    } else {
        write!(f, "0x{b:02x}")
    }
}

/// A decoded frame: the header and the payload, borrowed from the decode buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub header: Header,
    pub payload: &'a [u8],
}

fn header_bytes(kind: u16, schema: u16, written: u32, length: u32) -> [u8; CRC_COVERED] {
    let [k0, k1] = kind.to_le_bytes();
    let [s0, s1] = schema.to_le_bytes();
    let [w0, w1, w2, w3] = written.to_le_bytes();
    let [l0, l1, l2, l3] = length.to_le_bytes();
    [k0, k1, s0, s1, w0, w1, w2, w3, l0, l1, l2, l3]
}

/// Writes the frame for `payload` into `out` (cleared first) and returns its length in UTF-16
/// code units, which is at most [`MAX_TEXT_UNITS`]. A payload over [`MAX_PAYLOAD`] leaves `out`
/// empty. `out` keeps its capacity, so a reused buffer allocates only when a frame outgrows it.
pub fn encode(
    kind: u16,
    schema: u16,
    written: u32,
    payload: &[u8],
    out: &mut String,
) -> Result<usize, TooLarge> {
    out.clear();
    let length = match u32::try_from(payload.len()) {
        Ok(n) if payload.len() <= MAX_PAYLOAD => n,
        _ => {
            return Err(TooLarge {
                payload: payload.len(),
            });
        }
    };
    let head = header_bytes(kind, schema, written, length);
    let mut crc = Crc32::default();
    crc.update(&head);
    crc.update(payload);
    let units = text_units(payload.len());
    out.reserve(units);
    out.push_str(TAG);
    let mut enc = base64::Encoder::default();
    enc.push(&head, out);
    enc.push(&crc.finish().to_le_bytes(), out);
    enc.push(payload, out);
    enc.finish(out);
    Ok(units)
}

fn le_u16(b: &[u8], at: usize) -> u16 {
    let mut a = [0u8; 2];
    if let Some(s) = b.get(at..at + 2) {
        a.copy_from_slice(s);
    }
    u16::from_le_bytes(a)
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    if let Some(s) = b.get(at..at + 4) {
        a.copy_from_slice(s);
    }
    u32::from_le_bytes(a)
}

/// Decodes a segment's text into `buf` (cleared first; header and payload) and checks the tag,
/// the length and the CRC. The owner then checks kind and schema with [`Header::expect`].
pub fn decode<'a>(text: &str, buf: &'a mut Vec<u8>) -> Result<Frame<'a>, DecodeError> {
    buf.clear();
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return Err(DecodeError::Empty);
    }
    let Some((tag, body)) = bytes.split_at_checked(TAG.len()) else {
        return Err(DecodeError::NotOurs);
    };
    let [m0, m1, version, encoding] = *tag else {
        return Err(DecodeError::NotOurs);
    };
    if [m0, m1] != *MAGIC.as_bytes() {
        return Err(DecodeError::NotOurs);
    }
    if version != VERSION {
        return Err(DecodeError::Version(version));
    }
    if encoding != ENCODING_BASE64 {
        return Err(DecodeError::Encoding(encoding));
    }
    base64::decode(body, buf).map_err(DecodeError::Base64)?;
    let Some((head, payload)) = buf.split_at_checked(HEADER_LEN) else {
        return Err(DecodeError::Short);
    };
    let header = Header {
        kind: le_u16(head, 0),
        schema: le_u16(head, 2),
        written: le_u32(head, 4),
        length: le_u32(head, 8),
    };
    if usize::try_from(header.length).ok() != Some(payload.len()) {
        return Err(DecodeError::Length {
            header: header.length,
            actual: payload.len(),
        });
    }
    let stored = le_u32(head, CRC_COVERED);
    let mut crc = Crc32::default();
    crc.update(head.get(..CRC_COVERED).unwrap_or_default());
    crc.update(payload);
    let actual = crc.finish();
    if actual != stored {
        return Err(DecodeError::Checksum {
            header: stored,
            actual,
        });
    }
    Ok(Frame { header, payload })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8]) -> String {
        let mut s = String::new();
        encode(7, 3, 123_456, payload, &mut s).expect("fits");
        s
    }

    fn pattern(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    /// Uses: `MEMORY_SEGMENT_SIZE_LIMIT`. The largest payload fills the limit exactly or within the
    /// base64 rounding, one more byte would not fit, and the bound matches the formula's inverse.
    #[test]
    fn max_payload_fills_a_segment() {
        let limit = MEMORY_SEGMENT_SIZE_LIMIT as usize;
        assert!(text_units(MAX_PAYLOAD) <= limit);
        assert!(text_units(MAX_PAYLOAD + 1) > limit);
        assert!(
            limit - text_units(MAX_PAYLOAD) < 4,
            "within one base64 group"
        );
        assert_eq!(MAX_TEXT_UNITS, limit);
        let mut s = String::new();
        assert_eq!(
            encode(1, 1, 0, &vec![0xa5; MAX_PAYLOAD + 1], &mut s),
            Err(TooLarge {
                payload: MAX_PAYLOAD + 1
            })
        );
        assert!(s.is_empty());
    }

    /// Round trips at every small size, around each base64 group boundary, and at the maximum; the
    /// text is ASCII, starts with the tag, and its length is `text_units`.
    #[test]
    fn round_trips() {
        let sizes: Vec<usize> = (0..=40).chain(MAX_PAYLOAD - 5..=MAX_PAYLOAD).collect();
        let mut buf = Vec::new();
        for (i, &len) in sizes.iter().enumerate() {
            let payload = pattern(len, i as u32 + 1);
            let mut text = String::new();
            let units = encode(42, 9, u32::MAX - i as u32, &payload, &mut text).expect("fits");
            assert_eq!(units, text.len());
            assert_eq!(units, text_units(len));
            assert!(units <= MAX_TEXT_UNITS);
            assert!(text.is_ascii() && text.starts_with(TAG));
            let f = decode(&text, &mut buf).expect("decodes");
            assert_eq!(f.payload, payload.as_slice());
            assert_eq!(
                f.header,
                Header {
                    kind: 42,
                    schema: 9,
                    written: u32::MAX - i as u32,
                    length: len as u32
                }
            );
            assert_eq!(f.header.expect(42, 9), Ok(()));
        }
    }

    /// The pinned bytes of a small frame: the tag, then base64 of the little-endian header with the
    /// CRC-32 of its first 12 bytes and the payload.
    #[test]
    fn the_layout_is_pinned() {
        let text = frame(b"hi");
        let mut head = vec![7, 0, 3, 0];
        head.extend_from_slice(&123_456u32.to_le_bytes());
        head.extend_from_slice(&2u32.to_le_bytes());
        let mut covered = head.clone();
        covered.extend_from_slice(b"hi");
        let crc = super::super::crc32::checksum(&covered);
        let mut raw = head;
        raw.extend_from_slice(&crc.to_le_bytes());
        raw.extend_from_slice(b"hi");
        let mut expected = String::from("rs1b");
        base64::encode(&raw, &mut expected);
        assert_eq!(text, expected);
    }

    /// The corrupt-segment test (ROADMAP P1 gate): every change of one character, every truncation
    /// and every appended character is refused; nothing decodes to a different payload.
    #[test]
    fn every_corruption_is_refused() {
        let payload = pattern(23, 5);
        let text = frame(&payload);
        let mut buf = Vec::new();
        let bytes = text.as_bytes();
        let replacements: Vec<u8> = (0x20..0x7f).collect();
        for at in 0..bytes.len() {
            for &r in &replacements {
                if r == bytes[at] {
                    continue;
                }
                let mut bad = bytes.to_vec();
                bad[at] = r;
                let bad = String::from_utf8(bad).expect("ascii");
                assert!(
                    decode(&bad, &mut buf).is_err(),
                    "char {at} -> {:?}",
                    char::from(r)
                );
            }
        }
        for cut in 0..bytes.len() {
            assert!(
                decode(&text[..cut], &mut buf).is_err(),
                "truncated to {cut}"
            );
        }
        for extra in ["A", "AA", "AAA", "AAAA", "=", " "] {
            assert!(
                decode(&format!("{text}{extra}"), &mut buf).is_err(),
                "+{extra}"
            );
        }
        assert_eq!(
            decode(&text, &mut buf).map(|f| f.payload.to_vec()),
            Ok(payload)
        );
    }

    /// Each refusal names its cause, in the order the decoder checks them.
    #[test]
    fn refusals_name_their_cause() {
        let mut buf = Vec::new();
        let text = frame(b"payload");
        assert_eq!(decode("", &mut buf), Err(DecodeError::Empty));
        // The profiler's stats segment and other foreign text (P1.8 writes JSON to segment 99).
        assert_eq!(decode("{\"v\":1}", &mut buf), Err(DecodeError::NotOurs));
        assert_eq!(decode("rs1", &mut buf), Err(DecodeError::NotOurs));
        assert_eq!(decode("xx1bAAAA", &mut buf), Err(DecodeError::NotOurs));
        let v2 = text.replacen("rs1b", "rs2b", 1);
        assert_eq!(decode(&v2, &mut buf), Err(DecodeError::Version(b'2')));
        let utf15 = text.replacen("rs1b", "rs1u", 1);
        assert_eq!(decode(&utf15, &mut buf), Err(DecodeError::Encoding(b'u')));
        assert_eq!(
            decode("rs1b!", &mut buf),
            Err(DecodeError::Base64(Base64Error::Length))
        );
        assert_eq!(
            decode("rs1bAA==", &mut buf),
            Err(DecodeError::Base64(Base64Error::Char(2)))
        );
        // A non-ASCII character is refused, not decoded (it is not in the alphabet).
        assert_eq!(
            decode("rs1bAAé", &mut buf),
            Err(DecodeError::Base64(Base64Error::Char(2)))
        );
        let mut short = String::from("rs1b");
        base64::encode(&[0; HEADER_LEN - 1], &mut short);
        assert_eq!(decode(&short, &mut buf), Err(DecodeError::Short));

        // A header whose length disagrees with the body (built by hand: a valid CRC, wrong length).
        let mut raw = header_bytes(1, 1, 0, 5).to_vec();
        let crc = super::super::crc32::checksum(&[&raw[..], b"abcd"].concat());
        raw.extend_from_slice(&crc.to_le_bytes());
        raw.extend_from_slice(b"abcd");
        let mut wrong_len = String::from(TAG);
        base64::encode(&raw, &mut wrong_len);
        assert_eq!(
            decode(&wrong_len, &mut buf),
            Err(DecodeError::Length {
                header: 5,
                actual: 4
            })
        );

        // A payload changed in place under a valid base64 body fails the CRC.
        let mut raw = Vec::new();
        base64::decode(&text.as_bytes()[TAG.len()..], &mut raw).expect("base64");
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let stored = le_u32(&raw, CRC_COVERED);
        let mut flipped = String::from(TAG);
        base64::encode(&raw, &mut flipped);
        assert!(matches!(
            decode(&flipped, &mut buf),
            Err(DecodeError::Checksum { header, .. }) if header == stored
        ));

        let h = Header {
            kind: 7,
            schema: 3,
            written: 0,
            length: 0,
        };
        assert_eq!(
            h.expect(8, 3),
            Err(DecodeError::Kind {
                expected: 8,
                found: 7
            })
        );
        assert_eq!(
            h.expect(7, 4),
            Err(DecodeError::Schema {
                expected: 4,
                found: 3
            })
        );
        // Every cause has a message.
        for e in [
            DecodeError::Empty,
            DecodeError::NotOurs,
            DecodeError::Version(b'2'),
            DecodeError::Encoding(b'u'),
            DecodeError::Base64(Base64Error::Char(1)),
            DecodeError::Base64(Base64Error::Length),
            DecodeError::Base64(Base64Error::TrailingBits),
            DecodeError::Short,
            DecodeError::Length {
                header: 1,
                actual: 2,
            },
            DecodeError::Checksum {
                header: 1,
                actual: 2,
            },
            DecodeError::Kind {
                expected: 1,
                found: 2,
            },
            DecodeError::Schema {
                expected: 1,
                found: 2,
            },
        ] {
            assert!(!e.to_string().is_empty());
        }
    }
}
