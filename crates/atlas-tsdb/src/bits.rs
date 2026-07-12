//! Minimal MSB-first bit reader/writer for the Gorilla block codec.
//!
//! The writer accumulates bits into a `Vec<u8>`; the reader walks the same
//! bytes back. Bits are packed most-significant-first within each byte so a
//! block's bitstream is deterministic and endian-independent. No dependencies.

/// Appends bits MSB-first into a growable byte buffer.
#[derive(Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    /// Bits already filled in the in-progress final byte (0..=7). When 0 the
    /// last byte is complete (or the buffer is empty) and a new byte is pushed
    /// on the next write.
    nbits: u8,
}

impl BitWriter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Writes a single bit (`true` = 1).
    pub(crate) fn write_bit(&mut self, bit: bool) {
        if self.nbits == 0 {
            self.bytes.push(0);
        }
        if bit {
            let last = self.bytes.len() - 1;
            self.bytes[last] |= 1 << (7 - self.nbits);
        }
        self.nbits = (self.nbits + 1) & 7;
    }

    /// Writes the low `count` bits of `value`, most-significant of those first.
    /// `count` must be in 0..=64.
    pub(crate) fn write_bits(&mut self, value: u64, count: u32) {
        debug_assert!(count <= 64);
        for i in (0..count).rev() {
            self.write_bit((value >> i) & 1 == 1);
        }
    }

    /// Consumes the writer, returning the packed bytes. Trailing bits in the
    /// final partial byte are zero-padded.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Reads bits MSB-first from a byte slice. Reads past the end return an error
/// so a truncated/corrupt payload is a value, never a panic.
pub(crate) struct BitReader<'a> {
    bytes: &'a [u8],
    /// Absolute bit position from the start of `bytes`.
    pos: usize,
    /// Total readable bits.
    len_bits: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            len_bits: bytes.len() * 8,
        }
    }

    /// Reads one bit. `None` once the stream is exhausted.
    pub(crate) fn read_bit(&mut self) -> Option<bool> {
        if self.pos >= self.len_bits {
            return None;
        }
        let byte = self.bytes[self.pos / 8];
        let bit = (byte >> (7 - (self.pos % 8) as u8)) & 1 == 1;
        self.pos += 1;
        Some(bit)
    }

    /// Reads `count` bits (0..=64) into the low bits of a `u64`. `None` if the
    /// stream runs out before `count` bits are available.
    pub(crate) fn read_bits(&mut self, count: u32) -> Option<u64> {
        debug_assert!(count <= 64);
        let mut v = 0u64;
        for _ in 0..count {
            v = (v << 1) | self.read_bit()? as u64;
        }
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_bits() {
        let mut w = BitWriter::new();
        w.write_bit(true);
        w.write_bit(false);
        w.write_bits(0b1011, 4);
        w.write_bits(0xDEAD_BEEF, 32);
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bit(), Some(true));
        assert_eq!(r.read_bit(), Some(false));
        assert_eq!(r.read_bits(4), Some(0b1011));
        assert_eq!(r.read_bits(32), Some(0xDEAD_BEEF));
    }

    #[test]
    fn read_past_end_is_none() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(3), Some(0b101));
        // Only 3 meaningful bits were written; the byte's 5 pad bits are
        // readable as zeros, but the 9th bit is past the end.
        assert_eq!(r.read_bits(5), Some(0));
        assert_eq!(r.read_bit(), None);
    }

    #[test]
    fn zero_count_reads_zero() {
        let bytes = [0xFFu8];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(0), Some(0));
    }
}
