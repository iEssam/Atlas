//! Gorilla-compressed sample block: encoder, decoder, and on-wire format.
//!
//! A block is a self-describing byte payload holding one series' (ts_ms, value)
//! points in time order. `atlas-tsdb` owns the entire format so a future
//! file-based chunk store can swap in under the same API — nothing outside this
//! crate parses the bytes.
//!
//! ## Wire format (all integers little-endian)
//! ```text
//!   offset  size  field
//!   0       4     magic  = b"ATB1"
//!   4       4     u32    point count
//!   8       8     i64    start_ms (first timestamp; 0 if empty)
//!   16      8     i64    end_ms   (last timestamp;  0 if empty)
//!   24      4     u32    bitstream length in bytes
//!   28      N     ..     bitstream (delta-of-delta ts + Gorilla XOR values)
//!   28+N    4     u32    checksum over bytes [0, 28+N) (little-endian)
//! ```
//! The bitstream, per point:
//!   * point 0: ts = 64-bit raw i64; value = 64-bit raw f64 bits.
//!   * point 1: ts delta = zigzag varint (dod baseline is delta itself); value
//!     = Gorilla XOR against previous.
//!   * point k≥2: ts delta-of-delta via the Gorilla bucketed code below; value
//!     = Gorilla XOR.
//!
//! Timestamp delta-of-delta (Gorilla/Prometheus bucketed scheme). `dod` is
//! zigzagged, then:
//!   * `0`                → dod == 0 (steady cadence: one bit).
//!   * `10`  + 7  bits    → dod in the 7-bit range.
//!   * `110` + 9  bits    → dod in the 9-bit range.
//!   * `1110`+ 12 bits    → dod in the 12-bit range.
//!   * `1111`+ 64 bits    → anything else.
//!
//! Gorilla XOR value scheme (Facebook Gorilla paper):
//!   * xor == 0            → control bit `0`.
//!   * xor != 0, fits in the previous leading/trailing window → `10` + meaningful bits.
//!   * otherwise           → `11` + 5-bit leading zeros + 6-bit meaningful length + bits.

use crate::bits::{BitReader, BitWriter};

/// Block magic + format version tag.
const MAGIC: [u8; 4] = *b"ATB1";
/// Fixed header length before the bitstream: magic(4) + count(4) + start(8) +
/// end(8) + bitlen(4).
const HEADER_LEN: usize = 28;

/// Errors from parsing/validating a block. A corrupt or truncated payload is
/// always one of these values, never a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    /// Payload shorter than the fixed header (or than header+declared body).
    Truncated,
    /// Magic bytes did not match — not an Atlas block.
    BadMagic,
    /// Declared bitstream length overflows the payload.
    BadLength,
    /// Stored checksum does not match the recomputed one.
    BadChecksum,
    /// The bitstream ended before `count` points were decoded.
    UnexpectedEnd,
}

impl std::fmt::Display for BlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BlockError::Truncated => "block payload truncated",
            BlockError::BadMagic => "block magic mismatch",
            BlockError::BadLength => "block declared length overflows payload",
            BlockError::BadChecksum => "block checksum mismatch",
            BlockError::UnexpectedEnd => "block bitstream ended early",
        };
        f.write_str(s)
    }
}

impl std::error::Error for BlockError {}

/// A hand-rolled CRC-32 (IEEE polynomial, reflected) — no external deps. Used
/// only for corruption detection, not security. `pub(crate)` so the roll-up
/// container ([`crate::rollup`]) frames itself with the same checksum.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    // Standard reflected CRC-32 (poly 0xEDB88320), computed without a table.
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Zigzag-encode a signed integer to unsigned (small magnitudes → small values).
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// Inverse of [`zigzag`].
fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Writes a timestamp delta-of-delta using the Gorilla bucketed code. `dod` is
/// the signed delta-of-delta; a steady cadence (dod == 0) costs a single bit.
fn write_dod(w: &mut BitWriter, dod: i64) {
    if dod == 0 {
        w.write_bit(false);
        return;
    }
    let z = zigzag(dod);
    // Bucket by how many bits the zigzagged value needs.
    if z < (1 << 7) {
        w.write_bits(0b10, 2);
        w.write_bits(z, 7);
    } else if z < (1 << 9) {
        w.write_bits(0b110, 3);
        w.write_bits(z, 9);
    } else if z < (1 << 12) {
        w.write_bits(0b1110, 4);
        w.write_bits(z, 12);
    } else {
        w.write_bits(0b1111, 4);
        w.write_bits(z, 64);
    }
}

/// Reads a timestamp delta-of-delta written by [`write_dod`]. `None` on early
/// end of stream.
fn read_dod(r: &mut BitReader) -> Option<i64> {
    if !r.read_bit()? {
        return Some(0);
    }
    // Count the run of leading 1s already consumed (we read the first as part of
    // the `if` above) to select the bucket.
    let bits = if !r.read_bit()? {
        7 // prefix 10
    } else if !r.read_bit()? {
        9 // prefix 110
    } else if !r.read_bit()? {
        12 // prefix 1110
    } else {
        64 // prefix 1111
    };
    Some(unzigzag(r.read_bits(bits)?))
}

/// Builds a Gorilla block from time-ordered (ts_ms, value) appends.
///
/// Appends must be non-decreasing in timestamp; out-of-order appends are
/// rejected (see [`BlockBuilder::append`]). Duplicate timestamps are permitted
/// and stored verbatim (delta 0) — the store keeps every sample it is given;
/// de-duplication, if ever wanted, is a query-layer concern.
pub struct BlockBuilder {
    count: u32,
    start_ms: i64,
    last_ts: i64,
    last_delta: i64,
    last_bits: u64,
    last_leading: u32,
    last_trailing: u32,
    have_prev: bool,
    bits: BitWriter,
}

impl Default for BlockBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockBuilder {
    pub fn new() -> Self {
        Self {
            count: 0,
            start_ms: 0,
            last_ts: 0,
            last_delta: 0,
            last_bits: 0,
            last_leading: u32::MAX,
            last_trailing: 0,
            have_prev: false,
            bits: BitWriter::new(),
        }
    }

    /// Number of points appended so far.
    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The timestamp of the last appended point (for callers deciding when a
    /// head is stale). Meaningless when empty.
    pub fn last_ts_ms(&self) -> i64 {
        self.last_ts
    }

    /// Appends one point. Returns `false` (and ignores the point) if `ts_ms`
    /// is strictly earlier than the previous timestamp — the codec depends on
    /// non-decreasing time, and silently reordering would corrupt attribution.
    /// Equal timestamps are accepted (stored as a zero delta).
    #[must_use]
    pub fn append(&mut self, ts_ms: i64, value: f64) -> bool {
        if self.have_prev && ts_ms < self.last_ts {
            return false;
        }
        let vbits = value.to_bits();
        if !self.have_prev {
            // First point: raw ts + raw value bits.
            self.start_ms = ts_ms;
            self.bits.write_bits(ts_ms as u64, 64);
            self.bits.write_bits(vbits, 64);
            self.last_ts = ts_ms;
            self.last_delta = 0;
            self.last_bits = vbits;
            self.have_prev = true;
            self.count = 1;
            return true;
        }

        // Timestamp: delta-of-delta via the Gorilla bucketed code. For point 1
        // `last_delta` is 0, so the baseline dod is the delta itself.
        let delta = ts_ms - self.last_ts;
        let dod = delta - self.last_delta;
        write_dod(&mut self.bits, dod);
        self.last_ts = ts_ms;
        self.last_delta = delta;

        // Value: Gorilla XOR.
        self.encode_value(vbits);
        self.last_bits = vbits;
        self.count += 1;
        true
    }

    fn encode_value(&mut self, vbits: u64) {
        let xor = vbits ^ self.last_bits;
        if xor == 0 {
            self.bits.write_bit(false);
            return;
        }
        self.bits.write_bit(true);
        let leading = xor.leading_zeros();
        let trailing = xor.trailing_zeros();
        // Cap leading at 31 so it fits the 5-bit field used in the new-window arm.
        let leading = leading.min(31);

        if self.last_leading != u32::MAX
            && leading >= self.last_leading
            && trailing >= self.last_trailing
        {
            // Reuse the previous window: control bit 0, then the meaningful bits.
            self.bits.write_bit(false);
            let mbits = 64 - self.last_leading - self.last_trailing;
            self.bits.write_bits(xor >> self.last_trailing, mbits);
        } else {
            // New window: control bit 1, 5-bit leading, 6-bit length, then bits.
            self.bits.write_bit(true);
            let mbits = 64 - leading - trailing;
            self.bits.write_bits(leading as u64, 5);
            // `mbits` is 1..=64; store as 6 bits with 64 encoded as 0.
            self.bits.write_bits((mbits & 0x3F) as u64, 6);
            self.bits.write_bits(xor >> trailing, mbits);
            self.last_leading = leading;
            self.last_trailing = trailing;
        }
    }

    /// Finishes the block, producing the framed payload (header + bitstream +
    /// checksum). The builder is consumed.
    pub fn finish(self) -> Vec<u8> {
        let end_ms = self.last_ts;
        let start_ms = self.start_ms;
        let count = self.count;
        let body = self.bits.into_bytes();

        let mut out = Vec::with_capacity(HEADER_LEN + body.len() + 4);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&start_ms.to_le_bytes());
        out.extend_from_slice(&end_ms.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        let checksum = crc32(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }
}

/// Parsed block header metadata (cheap to read without decoding points).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    pub count: u32,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Parses and validates a block payload, then iterates its points.
///
/// Construction validates magic, framing, and checksum up front, so a
/// successfully-constructed reader is guaranteed to decode `count` points
/// (barring a bitstream that is internally short, surfaced as
/// [`BlockError::UnexpectedEnd`] mid-iteration).
#[derive(Debug)]
pub struct BlockReader<'a> {
    header: BlockHeader,
    body: &'a [u8],
}

impl<'a> BlockReader<'a> {
    /// Validates and wraps a payload. Verifies the checksum — a single flipped
    /// bit anywhere in the frame is rejected here.
    pub fn parse(payload: &'a [u8]) -> Result<Self, BlockError> {
        if payload.len() < HEADER_LEN + 4 {
            return Err(BlockError::Truncated);
        }
        if payload[0..4] != MAGIC {
            return Err(BlockError::BadMagic);
        }
        let count = u32::from_le_bytes(payload[4..8].try_into().unwrap());
        let start_ms = i64::from_le_bytes(payload[8..16].try_into().unwrap());
        let end_ms = i64::from_le_bytes(payload[16..24].try_into().unwrap());
        let bitlen = u32::from_le_bytes(payload[24..28].try_into().unwrap()) as usize;

        let body_end = HEADER_LEN
            .checked_add(bitlen)
            .ok_or(BlockError::BadLength)?;
        // Need body_end bytes for the frame + 4 for the trailing checksum.
        if body_end.checked_add(4).ok_or(BlockError::BadLength)? > payload.len() {
            return Err(BlockError::BadLength);
        }

        let stored = u32::from_le_bytes(payload[body_end..body_end + 4].try_into().unwrap());
        let computed = crc32(&payload[..body_end]);
        if stored != computed {
            return Err(BlockError::BadChecksum);
        }

        Ok(Self {
            header: BlockHeader {
                count,
                start_ms,
                end_ms,
            },
            body: &payload[HEADER_LEN..body_end],
        })
    }

    pub fn header(&self) -> BlockHeader {
        self.header
    }

    /// Decodes every point into a `Vec`. Returns [`BlockError::UnexpectedEnd`]
    /// if the bitstream is internally short for the declared count.
    pub fn points(&self) -> Result<Vec<(i64, f64)>, BlockError> {
        let mut out = Vec::with_capacity(self.header.count as usize);
        let mut r = BitReader::new(self.body);
        if self.header.count == 0 {
            return Ok(out);
        }

        // First point: raw ts + raw value bits.
        let ts0 = r.read_bits(64).ok_or(BlockError::UnexpectedEnd)? as i64;
        let vbits0 = r.read_bits(64).ok_or(BlockError::UnexpectedEnd)?;
        out.push((ts0, f64::from_bits(vbits0)));

        let mut last_ts = ts0;
        let mut last_delta = 0i64;
        let mut last_bits = vbits0;
        let mut leading = 0u32;
        let mut trailing = 0u32;

        for _ in 1..self.header.count {
            let dod = read_dod(&mut r).ok_or(BlockError::UnexpectedEnd)?;
            let delta = last_delta + dod;
            let ts = last_ts + delta;
            last_ts = ts;
            last_delta = delta;

            let vbits = decode_value(&mut r, last_bits, &mut leading, &mut trailing)?;
            last_bits = vbits;
            out.push((ts, f64::from_bits(vbits)));
        }
        Ok(out)
    }
}

/// Decodes one Gorilla-XOR value given the previous bits and the running
/// leading/trailing window (updated in place on a new-window control code).
fn decode_value(
    r: &mut BitReader,
    last_bits: u64,
    leading: &mut u32,
    trailing: &mut u32,
) -> Result<u64, BlockError> {
    let changed = r.read_bit().ok_or(BlockError::UnexpectedEnd)?;
    if !changed {
        return Ok(last_bits);
    }
    let new_window = r.read_bit().ok_or(BlockError::UnexpectedEnd)?;
    if new_window {
        *leading = r.read_bits(5).ok_or(BlockError::UnexpectedEnd)? as u32;
        let raw = r.read_bits(6).ok_or(BlockError::UnexpectedEnd)? as u32;
        // Length was stored mod 64; 0 means a full 64-bit meaningful window.
        let mbits = if raw == 0 { 64 } else { raw };
        *trailing = 64 - *leading - mbits;
    }
    let mbits = 64 - *leading - *trailing;
    let meaningful = r.read_bits(mbits).ok_or(BlockError::UnexpectedEnd)?;
    Ok(last_bits ^ (meaningful << *trailing))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(points: &[(i64, f64)]) -> Vec<(i64, f64)> {
        let mut b = BlockBuilder::new();
        for &(t, v) in points {
            assert!(b.append(t, v), "append rejected {t}");
        }
        let payload = b.finish();
        let r = BlockReader::parse(&payload).expect("parse");
        r.points().expect("points")
    }

    fn approx_eq(a: &[(i64, f64)], b: &[(i64, f64)]) {
        assert_eq!(a.len(), b.len(), "length mismatch");
        for (x, y) in a.iter().zip(b) {
            assert_eq!(x.0, y.0, "ts mismatch");
            assert_eq!(x.1.to_bits(), y.1.to_bits(), "value bits mismatch");
        }
    }

    #[test]
    fn empty_block_roundtrips() {
        let b = BlockBuilder::new();
        let payload = b.finish();
        let r = BlockReader::parse(&payload).unwrap();
        assert_eq!(r.header().count, 0);
        assert!(r.points().unwrap().is_empty());
    }

    #[test]
    fn single_point_roundtrips() {
        let pts = vec![(1_700_000_000_000, 42.5)];
        approx_eq(&roundtrip(&pts), &pts);
    }

    #[test]
    fn constant_series_roundtrips() {
        let pts: Vec<_> = (0..500).map(|i| (1000 + i * 1000, 7.0)).collect();
        approx_eq(&roundtrip(&pts), &pts);
    }

    #[test]
    fn regular_cadence_random_walk_roundtrips() {
        // Deterministic pseudo-random walk (xorshift) at a steady 1 s cadence —
        // the common shape for a CPU/WS series.
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut v = 500.0f64;
        let pts: Vec<_> = (0..2000)
            .map(|i| {
                let step = (next() % 21) as f64 - 10.0;
                v = (v + step).clamp(0.0, 1000.0);
                (1_700_000_000_000 + i * 1000, v)
            })
            .collect();
        approx_eq(&roundtrip(&pts), &pts);
    }

    #[test]
    fn spiky_series_roundtrips() {
        let pts: Vec<_> = (0..1000)
            .map(|i| {
                let v = if i % 97 == 0 { 999.0 } else { 0.0 };
                (1000 + i * 1000, v)
            })
            .collect();
        approx_eq(&roundtrip(&pts), &pts);
    }

    #[test]
    fn duplicate_timestamps_are_kept() {
        // Duplicate ts is accepted and stored verbatim (delta 0), documented
        // behaviour: the store keeps every sample handed to it.
        let pts = vec![(1000, 1.0), (1000, 2.0), (1000, 3.0), (2000, 4.0)];
        approx_eq(&roundtrip(&pts), &pts);
    }

    #[test]
    fn backwards_timestamp_is_rejected() {
        let mut b = BlockBuilder::new();
        assert!(b.append(2000, 1.0));
        assert!(!b.append(1999, 2.0), "earlier ts must be rejected");
        // The rejected point left no trace; the block still holds just the one.
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn irregular_cadence_roundtrips() {
        // Adaptive cadence: 1 s, then 5 s, then 15 s gaps mixed together.
        let gaps = [1000i64, 1000, 5000, 5000, 15000, 1000, 15000, 5000];
        let mut t = 1_700_000_000_000i64;
        let mut pts = Vec::new();
        for (i, g) in gaps.iter().cycle().take(300).enumerate() {
            t += g;
            pts.push((t, (i as f64 * 3.5) % 128.0));
        }
        approx_eq(&roundtrip(&pts), &pts);
    }

    #[test]
    fn special_float_values_roundtrip() {
        let pts = vec![
            (1000, 0.0),
            (2000, -0.0),
            (3000, f64::INFINITY),
            (4000, f64::NEG_INFINITY),
            (5000, f64::MAX),
            (6000, f64::MIN_POSITIVE),
            (7000, 1.0 / 3.0),
        ];
        let got = roundtrip(&pts);
        approx_eq(&got, &pts);
    }

    #[test]
    fn nan_roundtrips_by_bits() {
        let mut b = BlockBuilder::new();
        assert!(b.append(1000, 1.0));
        assert!(b.append(2000, f64::NAN));
        let payload = b.finish();
        let got = BlockReader::parse(&payload).unwrap().points().unwrap();
        assert!(got[1].1.is_nan());
    }

    #[test]
    fn corrupt_magic_rejected() {
        let mut payload = {
            let mut b = BlockBuilder::new();
            assert!(b.append(1, 1.0));
            b.finish()
        };
        payload[0] ^= 0xFF;
        assert_eq!(
            BlockReader::parse(&payload).unwrap_err(),
            BlockError::BadMagic
        );
    }

    #[test]
    fn flipped_body_bit_fails_checksum() {
        let mut payload = {
            let mut b = BlockBuilder::new();
            for i in 0..50 {
                assert!(b.append(1000 + i * 1000, i as f64 * 1.5));
            }
            b.finish()
        };
        // Flip a bit inside the bitstream body.
        let mid = HEADER_LEN + (payload.len() - HEADER_LEN) / 2;
        payload[mid] ^= 0x01;
        assert_eq!(
            BlockReader::parse(&payload).unwrap_err(),
            BlockError::BadChecksum
        );
    }

    #[test]
    fn truncated_payload_rejected() {
        let payload = {
            let mut b = BlockBuilder::new();
            for i in 0..50 {
                assert!(b.append(1000 + i * 1000, i as f64));
            }
            b.finish()
        };
        // Chop the checksum + tail off.
        let short = &payload[..payload.len() - 8];
        let err = BlockReader::parse(short).unwrap_err();
        assert!(matches!(
            err,
            BlockError::BadLength | BlockError::BadChecksum | BlockError::Truncated
        ));
    }

    #[test]
    fn tiny_payload_rejected() {
        assert_eq!(BlockReader::parse(&[]).unwrap_err(), BlockError::Truncated);
        assert_eq!(
            BlockReader::parse(&[0u8; 10]).unwrap_err(),
            BlockError::Truncated
        );
    }

    #[test]
    fn bad_length_field_rejected() {
        let mut payload = {
            let mut b = BlockBuilder::new();
            assert!(b.append(1, 1.0));
            b.finish()
        };
        // Set an absurd bitstream length that overflows the payload.
        payload[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            BlockReader::parse(&payload).unwrap_err(),
            BlockError::BadLength
        );
    }

    #[test]
    fn header_reports_bounds() {
        let mut b = BlockBuilder::new();
        for i in 0..10 {
            assert!(b.append(1000 + i * 250, i as f64));
        }
        let payload = b.finish();
        let h = BlockReader::parse(&payload).unwrap().header();
        assert_eq!(h.count, 10);
        assert_eq!(h.start_ms, 1000);
        assert_eq!(h.end_ms, 1000 + 9 * 250);
    }

    #[test]
    fn zigzag_roundtrips() {
        for v in [0i64, 1, -1, 2, -2, i64::MAX, i64::MIN, 123456, -123456] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
    }
}
