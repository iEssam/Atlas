//! Roll-up (downsampling) encoding for extended retention tiers (tech-stack
//! §4.2, PRD §9.3.1/§13.5).
//!
//! A raw ([`crate::block`], `ATB1`) series keeps every 1 s sample for the recent
//! window. Older data is *rolled up* into coarser buckets to bound the retained
//! footprint over 30–90 day windows while **preserving peaks**: each coarse
//! bucket stores `min`, `max`, a time/count-weighted `avg`, and the finer sample
//! `count`. The `max` (and `min`) are stored explicitly, so a one-sample spike
//! inside a bucket is never averaged away — it survives as that bucket's `max`.
//!
//! Two roll-up tiers are used (tech-stack §4.2):
//!   * **T1** — 10 s buckets (recent history, ~14 d).
//!   * **T2** — 60 s buckets (long history, 30–90 d).
//!
//! ## Encoding — reuse the Gorilla machinery
//! A [`RollupBlock`] is four parallel `ATB1` sub-blocks (`min`, `max`, `avg`,
//! `count`), each a standard [`BlockBuilder`] stream keyed on the bucket start
//! timestamp. This reuses the entire delta-of-delta + XOR codec (and its tests):
//! the bucket starts share one steady cadence (cheap dod), and the aggregate
//! values compress with the same XOR scheme as raw samples. The four sub-blocks
//! are wrapped in a small self-describing container:
//! ```text
//!   0   4   magic  = b"ARU1"
//!   4   4   u32    bucket_secs (10 for T1, 60 for T2)
//!   8   4   u32    bucket count
//!   12  ..  four length-prefixed ATB1 sub-blocks: [u32 len | bytes] × (min,max,avg,count)
//!   end 4   u32    crc32 over [0, end)
//! ```
//! Each inner sub-block carries its own CRC; the outer CRC covers the container
//! framing. A corrupt or truncated payload is always a [`BlockError`], never a
//! panic — the same contract as the raw block reader.

use std::collections::BTreeMap;

use crate::block::{crc32, BlockBuilder, BlockError, BlockReader};
use crate::series::{EncodedBlock, SeriesKey};

/// T1 coarse-bucket width, seconds (tech-stack §4.2).
pub const T1_BUCKET_SECS: i64 = 10;
/// T2 coarse-bucket width, seconds (tech-stack §4.2).
pub const T2_BUCKET_SECS: i64 = 60;

/// Tier discriminants stored in the `sample_block.tier` column.
pub const TIER_RAW: u8 = 0;
/// First roll-up tier (10 s buckets).
pub const TIER_T1: u8 = 1;
/// Second roll-up tier (60 s buckets).
pub const TIER_T2: u8 = 2;

/// Container magic + format version tag.
const ROLLUP_MAGIC: [u8; 4] = *b"ARU1";
/// Fixed container header before the sub-blocks: magic(4) + bucket_secs(4) +
/// count(4).
const ROLLUP_HEADER_LEN: usize = 12;

/// The coarse-bucket width in **milliseconds** for a roll-up tier, or `None`
/// for a tier with no coarser bucketing (raw, or an unknown tier).
pub fn tier_bucket_ms(tier: u8) -> Option<i64> {
    match tier {
        TIER_T1 => Some(T1_BUCKET_SECS * 1000),
        TIER_T2 => Some(T2_BUCKET_SECS * 1000),
        _ => None,
    }
}

/// One coarse bucket. `min`/`max` preserve peaks (a spike survives as `max`);
/// `avg` is the time/count-weighted mean of the finer samples; `count` is how
/// many finer samples folded in. `start_ms` is aligned to the tier bucket width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RollupBucket {
    pub start_ms: i64,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub count: u32,
}

/// Rolls raw `(ts_ms, value)` points into coarse buckets of `bucket_ms`.
///
/// * `min`/`max` are the extremes of the raw values in the bucket — **peaks are
///   never averaged away**.
/// * `avg` is time-weighted: each sample is weighted by the gap to the next
///   sample (its representative duration), capped at the bucket width so a data
///   gap can't over-weight the sample before it; the final sample gets a
///   nominal 1 s. This mirrors the weighting used by the top-N aggregation.
/// * `count` is the raw sample count.
///
/// Points are assumed time-ordered (block decode yields ascending time, and the
/// store concatenates a series' blocks in start order). Buckets come out
/// ascending by `start_ms`. An empty input yields no buckets.
pub fn rollup_raw(points: &[(i64, f64)], bucket_ms: i64) -> Vec<RollupBucket> {
    assert!(bucket_ms > 0, "bucket width must be positive");
    let weight_cap_s = bucket_ms as f64 / 1000.0;

    struct Acc {
        min: f64,
        max: f64,
        wsum: f64,
        weight: f64,
        count: u32,
    }
    let mut map: BTreeMap<i64, Acc> = BTreeMap::new();

    for i in 0..points.len() {
        let (ts, v) = points[i];
        // Representative duration of this sample: gap to the next sample,
        // clamped to (0, bucket_width]; nominal 1 s for the last sample.
        let w = match points.get(i + 1) {
            Some(&(nts, _)) => {
                let dt = (nts - ts).max(0) as f64 / 1000.0;
                if dt > 0.0 {
                    dt.min(weight_cap_s)
                } else {
                    1.0
                }
            }
            None => 1.0,
        };
        let bstart = ts.div_euclid(bucket_ms) * bucket_ms;
        let acc = map.entry(bstart).or_insert(Acc {
            min: v,
            max: v,
            wsum: 0.0,
            weight: 0.0,
            count: 0,
        });
        acc.min = acc.min.min(v);
        acc.max = acc.max.max(v);
        acc.wsum += v * w;
        acc.weight += w;
        acc.count += 1;
    }

    map.into_iter()
        .map(|(start_ms, a)| RollupBucket {
            start_ms,
            min: a.min,
            max: a.max,
            avg: if a.weight > 0.0 {
                a.wsum / a.weight
            } else {
                a.min
            },
            count: a.count,
        })
        .collect()
}

/// Rolls finer roll-up buckets into coarser ones (the T1 → T2 step). Combining
/// is associative and lossless for the aggregates:
///   * `min` = min of the finer mins, `max` = max of the finer maxes (**peaks
///     survive the second roll-up too**),
///   * `count` = sum of the finer counts,
///   * `avg` = count-weighted mean of the finer avgs (so `avg` still reflects
///     the underlying raw samples, not the buckets).
///
/// Because folding is associative, a coarse bucket assembled from two roll-up
/// passes over disjoint finer buckets reconstructs the same aggregate as one
/// pass — this is what lets the compaction job roll up incrementally without
/// double-counting at a bucket boundary.
pub fn rollup_buckets(finer: &[RollupBucket], bucket_ms: i64) -> Vec<RollupBucket> {
    assert!(bucket_ms > 0, "bucket width must be positive");

    struct Acc {
        min: f64,
        max: f64,
        avg_wsum: f64,
        count: u64,
        avg_fallback_sum: f64,
        n: u32,
    }
    let mut map: BTreeMap<i64, Acc> = BTreeMap::new();

    for b in finer {
        let bstart = b.start_ms.div_euclid(bucket_ms) * bucket_ms;
        let acc = map.entry(bstart).or_insert(Acc {
            min: b.min,
            max: b.max,
            avg_wsum: 0.0,
            count: 0,
            avg_fallback_sum: 0.0,
            n: 0,
        });
        acc.min = acc.min.min(b.min);
        acc.max = acc.max.max(b.max);
        acc.avg_wsum += b.avg * b.count as f64;
        acc.count += b.count as u64;
        acc.avg_fallback_sum += b.avg;
        acc.n += 1;
    }

    map.into_iter()
        .map(|(start_ms, a)| RollupBucket {
            start_ms,
            min: a.min,
            max: a.max,
            // Count-weighted mean; fall back to a plain mean of the bucket avgs
            // if every finer bucket somehow had a zero count.
            avg: if a.count > 0 {
                a.avg_wsum / a.count as f64
            } else if a.n > 0 {
                a.avg_fallback_sum / a.n as f64
            } else {
                a.min
            },
            count: a.count.min(u32::MAX as u64) as u32,
        })
        .collect()
}

/// Encodes coarse buckets into the framed `ARU1` container. `bucket_secs` is the
/// tier bucket width (10 or 60) stored in the header for self-description.
pub fn encode_rollup(buckets: &[RollupBucket], bucket_secs: i64) -> Vec<u8> {
    let mut mins = BlockBuilder::new();
    let mut maxs = BlockBuilder::new();
    let mut avgs = BlockBuilder::new();
    let mut cnts = BlockBuilder::new();
    for b in buckets {
        // Bucket starts are strictly ascending, so append never rejects.
        let _ = mins.append(b.start_ms, b.min);
        let _ = maxs.append(b.start_ms, b.max);
        let _ = avgs.append(b.start_ms, b.avg);
        let _ = cnts.append(b.start_ms, b.count as f64);
    }
    let subs = [mins.finish(), maxs.finish(), avgs.finish(), cnts.finish()];

    let mut out =
        Vec::with_capacity(ROLLUP_HEADER_LEN + subs.iter().map(|s| s.len() + 4).sum::<usize>() + 4);
    out.extend_from_slice(&ROLLUP_MAGIC);
    out.extend_from_slice(&(bucket_secs as u32).to_le_bytes());
    out.extend_from_slice(&(buckets.len() as u32).to_le_bytes());
    for s in &subs {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s);
    }
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// Builds a sealed [`EncodedBlock`] for `key` from coarse buckets, ready for the
/// tiered store. `start_ms`/`end_ms` denormalise the covered span (last bucket
/// end = start + one bucket width) so the store's overlap index never decodes a
/// payload. Returns `None` for an empty bucket set (nothing to persist).
pub fn encoded_rollup_block(
    key: SeriesKey,
    buckets: &[RollupBucket],
    bucket_secs: i64,
) -> Option<EncodedBlock> {
    if buckets.is_empty() {
        return None;
    }
    let start_ms = buckets.first().unwrap().start_ms;
    let last = buckets.last().unwrap().start_ms;
    let end_ms = last + bucket_secs * 1000 - 1;
    Some(EncodedBlock {
        key,
        start_ms,
        end_ms,
        points: buckets.len() as u32,
        payload: encode_rollup(buckets, bucket_secs),
    })
}

/// A parsed, validated roll-up container. Construction verifies magic, framing,
/// and the outer + inner checksums, then decodes the four aggregate streams and
/// zips them back into [`RollupBucket`]s.
#[derive(Debug, Clone)]
pub struct RollupReader {
    pub bucket_secs: u32,
    buckets: Vec<RollupBucket>,
}

impl RollupReader {
    /// Validates and decodes a container payload.
    pub fn parse(payload: &[u8]) -> Result<RollupReader, BlockError> {
        if payload.len() < ROLLUP_HEADER_LEN + 4 {
            return Err(BlockError::Truncated);
        }
        if payload[0..4] != ROLLUP_MAGIC {
            return Err(BlockError::BadMagic);
        }
        let bucket_secs = u32::from_le_bytes(payload[4..8].try_into().unwrap());
        let count = u32::from_le_bytes(payload[8..12].try_into().unwrap()) as usize;

        // Read the four length-prefixed sub-blocks.
        let mut off = ROLLUP_HEADER_LEN;
        let mut subs: [&[u8]; 4] = [&[]; 4];
        for sub in subs.iter_mut() {
            if off + 4 > payload.len() {
                return Err(BlockError::Truncated);
            }
            let len = u32::from_le_bytes(payload[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            let end = off.checked_add(len).ok_or(BlockError::BadLength)?;
            if end + 4 > payload.len() {
                // + 4 leaves room for the trailing outer CRC.
                return Err(BlockError::BadLength);
            }
            *sub = &payload[off..end];
            off = end;
        }
        // Outer CRC covers everything up to the trailing 4 bytes.
        if off + 4 > payload.len() {
            return Err(BlockError::Truncated);
        }
        let stored = u32::from_le_bytes(payload[off..off + 4].try_into().unwrap());
        if stored != crc32(&payload[..off]) {
            return Err(BlockError::BadChecksum);
        }

        let mins = BlockReader::parse(subs[0])?.points()?;
        let maxs = BlockReader::parse(subs[1])?.points()?;
        let avgs = BlockReader::parse(subs[2])?.points()?;
        let cnts = BlockReader::parse(subs[3])?.points()?;
        if mins.len() != count || maxs.len() != count || avgs.len() != count || cnts.len() != count
        {
            return Err(BlockError::UnexpectedEnd);
        }

        let mut buckets = Vec::with_capacity(count);
        for i in 0..count {
            // The four streams share the same bucket timestamps; a mismatch is a
            // corrupt payload.
            if mins[i].0 != maxs[i].0 || mins[i].0 != avgs[i].0 || mins[i].0 != cnts[i].0 {
                return Err(BlockError::UnexpectedEnd);
            }
            buckets.push(RollupBucket {
                start_ms: mins[i].0,
                min: mins[i].1,
                max: maxs[i].1,
                avg: avgs[i].1,
                count: cnts[i].1.round().max(0.0) as u32,
            });
        }
        Ok(RollupReader {
            bucket_secs,
            buckets,
        })
    }

    /// The decoded buckets, ascending by start.
    pub fn buckets(&self) -> &[RollupBucket] {
        &self.buckets
    }

    /// Consumes the reader, returning the buckets.
    pub fn into_buckets(self) -> Vec<RollupBucket> {
        self.buckets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::series::Metric;

    fn key() -> SeriesKey {
        SeriesKey::new(Metric::CpuPermille, 7)
    }

    #[test]
    fn rollup_raw_buckets_and_counts() {
        // 1 s cadence, 25 s of data → three 10 s buckets (0–9, 10–19, 20–24).
        let pts: Vec<(i64, f64)> = (0..25).map(|i| (i * 1000, i as f64)).collect();
        let b = rollup_raw(&pts, 10_000);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].start_ms, 0);
        assert_eq!(b[0].count, 10);
        assert_eq!(b[0].min, 0.0);
        assert_eq!(b[0].max, 9.0);
        assert_eq!(b[1].start_ms, 10_000);
        assert_eq!(b[2].start_ms, 20_000);
        assert_eq!(b[2].count, 5);
        assert_eq!(b[2].max, 24.0);
    }

    #[test]
    fn peak_survives_rollup() {
        // A single 1 s spike of 999 inside an otherwise-flat 10 s bucket must
        // appear as that bucket's max — never averaged away.
        let mut pts: Vec<(i64, f64)> = (0..10).map(|i| (i * 1000, 5.0)).collect();
        pts[4] = (4000, 999.0);
        let b = rollup_raw(&pts, 10_000);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].max, 999.0, "the spike survives as the bucket max");
        assert_eq!(b[0].min, 5.0);
        // Avg is pulled up a little but nowhere near the peak — the peak is
        // preserved *separately*, exactly the point of storing max explicitly.
        assert!(b[0].avg > 5.0 && b[0].avg < 999.0);
    }

    #[test]
    fn peak_survives_second_rollup() {
        // T0 → T1 → T2: the spike must still be the T2 bucket's max.
        let mut pts: Vec<(i64, f64)> = (0..60).map(|i| (i * 1000, 1.0)).collect();
        pts[37] = (37_000, 500.0);
        let t1 = rollup_raw(&pts, T1_BUCKET_SECS * 1000);
        let t2 = rollup_buckets(&t1, T2_BUCKET_SECS * 1000);
        assert_eq!(t2.len(), 1);
        assert_eq!(t2[0].max, 500.0, "peak survives the T1→T2 roll-up");
        assert_eq!(t2[0].count, 60, "all 60 raw samples accounted for");
    }

    #[test]
    fn empty_and_partial_buckets() {
        assert!(rollup_raw(&[], 10_000).is_empty());
        // Points landing only in the first and third bucket → no empty bucket in
        // between is emitted (gaps are omitted, rendered as missing data).
        let pts = [(0i64, 1.0), (1000, 2.0), (25_000, 9.0)];
        let b = rollup_raw(&pts, 10_000);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].start_ms, 0);
        assert_eq!(b[1].start_ms, 20_000);
    }

    #[test]
    fn constant_series_avg_equals_value() {
        let pts: Vec<(i64, f64)> = (0..10).map(|i| (i * 1000, 42.0)).collect();
        let b = rollup_raw(&pts, 10_000);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].min, 42.0);
        assert_eq!(b[0].max, 42.0);
        assert!((b[0].avg - 42.0).abs() < 1e-9);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let pts: Vec<(i64, f64)> = (0..120)
            .map(|i| (i * 1000, (i % 30) as f64 * 3.5))
            .collect();
        let buckets = rollup_raw(&pts, T1_BUCKET_SECS * 1000);
        let payload = encode_rollup(&buckets, T1_BUCKET_SECS);
        let rr = RollupReader::parse(&payload).expect("parse");
        assert_eq!(rr.bucket_secs, T1_BUCKET_SECS as u32);
        assert_eq!(rr.buckets(), buckets.as_slice());
    }

    #[test]
    fn encoded_block_span_and_points() {
        let pts: Vec<(i64, f64)> = (0..25).map(|i| (i * 1000, i as f64)).collect();
        let buckets = rollup_raw(&pts, T1_BUCKET_SECS * 1000);
        let blk = encoded_rollup_block(key(), &buckets, T1_BUCKET_SECS).expect("block");
        assert_eq!(blk.points, 3);
        assert_eq!(blk.start_ms, 0);
        assert_eq!(blk.end_ms, 20_000 + T1_BUCKET_SECS * 1000 - 1);
        assert_eq!(blk.key, key());
        // Payload decodes back to the same buckets.
        let got = RollupReader::parse(&blk.payload).unwrap().into_buckets();
        assert_eq!(got, buckets);
    }

    #[test]
    fn empty_buckets_produce_no_block() {
        assert!(encoded_rollup_block(key(), &[], T1_BUCKET_SECS).is_none());
    }

    #[test]
    fn compression_beats_raw_encoding() {
        // A 10-minute 1 s series (600 samples) rolled to T1 (60 buckets) must be
        // markedly smaller than the raw block — that footprint reduction is the
        // whole point of the tier.
        let pts: Vec<(i64, f64)> = (0..600)
            .map(|i| (i * 1000, 300.0 + (i as f64 * 0.37).sin() * 50.0))
            .collect();
        let mut raw = BlockBuilder::new();
        for &(t, v) in &pts {
            assert!(raw.append(t, v));
        }
        let raw_bytes = raw.finish().len();
        let t1 = rollup_raw(&pts, T1_BUCKET_SECS * 1000);
        let roll_bytes = encode_rollup(&t1, T1_BUCKET_SECS).len();
        assert!(
            roll_bytes * 2 < raw_bytes,
            "roll-up ({roll_bytes} B) should be well under half the raw block ({raw_bytes} B)"
        );
    }

    #[test]
    fn corrupt_rollup_rejected_not_panicking() {
        let pts: Vec<(i64, f64)> = (0..25).map(|i| (i * 1000, i as f64)).collect();
        let buckets = rollup_raw(&pts, T1_BUCKET_SECS * 1000);
        let mut payload = encode_rollup(&buckets, T1_BUCKET_SECS);
        // Flip a byte in the body → outer or inner checksum rejects it.
        let mid = payload.len() / 2;
        payload[mid] ^= 0xFF;
        assert!(RollupReader::parse(&payload).is_err());
        // Bad magic.
        let mut bad = encode_rollup(&buckets, T1_BUCKET_SECS);
        bad[0] ^= 0xFF;
        assert_eq!(RollupReader::parse(&bad).unwrap_err(), BlockError::BadMagic);
        // Truncated.
        assert_eq!(
            RollupReader::parse(&[0u8; 4]).unwrap_err(),
            BlockError::Truncated
        );
    }

    #[test]
    fn rollup_buckets_are_time_disjoint_and_associative() {
        // Rolling up two disjoint halves separately, then merging, equals rolling
        // up the whole — the property the incremental compaction relies on.
        let pts: Vec<(i64, f64)> = (0..120)
            .map(|i| (i * 1000, (i as f64).cos().abs() * 10.0))
            .collect();
        let whole = rollup_raw(&pts, T1_BUCKET_SECS * 1000);
        let (a, b) = pts.split_at(63);
        let mut parts = rollup_raw(a, T1_BUCKET_SECS * 1000);
        parts.extend(rollup_raw(b, T1_BUCKET_SECS * 1000));
        // Merge same-start partial buckets the way the query layer folds them.
        let merged = rollup_buckets(&parts, T1_BUCKET_SECS * 1000);
        assert_eq!(merged.len(), whole.len());
        for (m, w) in merged.iter().zip(&whole) {
            assert_eq!(m.start_ms, w.start_ms);
            assert_eq!(m.count, w.count, "no sample lost or double counted");
            assert_eq!(m.max, w.max, "peak preserved across the split");
            assert_eq!(m.min, w.min);
        }
    }
}
