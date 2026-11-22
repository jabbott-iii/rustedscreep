//! CRC-32 for segment frames (ADR-004): the CRC-32/ISO-HDLC parameters (the zlib and PNG CRC):
//! polynomial 0x04C11DB7 reflected (0xEDB88320), initial value and final XOR 0xFFFFFFFF, reflected
//! input and output, check value 0xCBF43926 over the ASCII bytes "123456789"
//! (<https://reveng.sourceforge.io/crc-catalogue/17plus.htm#crc.cat.crc-32-iso-hdlc>). The loader's
//! `js/src/crc32.ts` and the tools' `js/tools/lib/crc32.mjs` compute the same CRC, so a JS tool can
//! check a segment frame the bot wrote.
//!
//! Byte-at-a-time with one 1 KiB table: about 2.6 µs per KiB in wasm under Node 18 (ADR-004
//! evidence), which is a small part of a segment write.

/// The reflected polynomial.
const POLY: u32 = 0xEDB8_8320;

/// The table entry for byte value `n`: eight reflected polynomial steps.
const fn entry(n: u32) -> u32 {
    let mut c = n;
    let mut k = 0;
    while k < 8 {
        c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
        k += 1;
    }
    c
}

/// [`entry`] for every byte value. Built without indexing: each slot is the head of the remaining slice.
const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut rest: &mut [u32] = &mut table;
    let mut n = 0u32;
    while let Some((slot, tail)) = rest.split_first_mut() {
        *slot = entry(n);
        rest = tail;
        n += 1;
    }
    table
};

/// A running CRC-32: [`Crc32::update`] with any number of slices, then [`Crc32::finish`].
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self(!0)
    }
}

impl Crc32 {
    pub fn update(&mut self, bytes: &[u8]) {
        let mut c = self.0;
        for &b in bytes {
            // The index is masked to 8 bits, so the fallback never runs.
            let t = TABLE
                .get(((c ^ u32::from(b)) & 0xff) as usize)
                .copied()
                .unwrap_or(0);
            c = t ^ (c >> 8);
        }
        self.0 = c;
    }

    pub fn finish(self) -> u32 {
        !self.0
    }
}

/// CRC-32 of `bytes`.
pub fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::default();
    crc.update(bytes);
    crc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog's check value, and the empty input (0, since the final XOR undoes the initial value).
    #[test]
    fn catalogue_check_value() {
        assert_eq!(checksum(b"123456789"), 0xCBF4_3926);
        assert_eq!(checksum(b""), 0);
    }

    /// Feeding the bytes in pieces gives the same CRC as feeding them whole.
    #[test]
    fn updates_compose() {
        let data: Vec<u8> = (0..=255u8).collect();
        let whole = checksum(&data);
        for split in 0..data.len() {
            let mut crc = Crc32::default();
            crc.update(&data[..split]);
            crc.update(&data[split..]);
            assert_eq!(crc.finish(), whole);
        }
    }

    /// A CRC with a degree-32 polynomial detects every single-bit error and every burst of up to 32
    /// bits; checked here for every bit of a 64-byte message.
    #[test]
    fn every_single_bit_flip_changes_the_crc() {
        let data: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(29)).collect();
        let good = checksum(&data);
        for byte in 0..data.len() {
            for bit in 0..8 {
                let mut bad = data.clone();
                bad[byte] ^= 1 << bit;
                assert_ne!(checksum(&bad), good, "bit {bit} of byte {byte}");
            }
        }
    }
}
