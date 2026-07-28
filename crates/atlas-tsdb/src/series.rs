//! Series identity and in-memory head accumulators.
//!
//! A series is identified by `(metric, scope)`: `metric` is a small enum of the
//! numeric quantities Atlas records, `scope` is the entity the metric belongs to
//! (a process row id, or 0 for system-wide gauges). `HeadBlocks` holds one
//! open [`BlockBuilder`] per live series and seals them into [`EncodedBlock`]s
//! for the store when they grow large or old enough.

use std::collections::HashMap;

use crate::block::BlockBuilder;

/// The numeric quantities Atlas stores as time series. Encoded as a `u16` so it
/// is stable in the SQLite `sample_block.metric` column and cheap to key on.
///
/// `Sys*` variants are system-wide gauges (scope 0); the rest are per-process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Metric {
    /// Per-process CPU share, permille (0..=1000).
    CpuPermille = 0,
    /// Per-process working set, bytes.
    WorkingSet = 1,
    /// Per-process private bytes, bytes.
    PrivateBytes = 2,
    /// Per-process disk read rate, bytes/s.
    ReadBps = 3,
    /// Per-process disk write rate, bytes/s.
    WriteBps = 4,
    /// Per-process busiest GPU engine, permille.
    GpuPermille = 5,
    /// Per-process dedicated GPU memory, bytes.
    GpuDedicatedBytes = 6,
    /// Per-process shared GPU memory, bytes.
    GpuSharedBytes = 7,

    /// System CPU, permille.
    SysCpuPermille = 100,
    /// System memory used, bytes.
    SysMemUsed = 101,
    /// System commit used, bytes.
    SysCommitUsed = 102,
    /// System process count.
    SysProcessCount = 103,
    /// System thread count.
    SysThreadCount = 104,
    /// System handle count.
    SysHandleCount = 105,
    SysGpuPermille = 106,
    SysGpuDedicatedUsed = 107,
    SysGpuSharedUsed = 108,
    SysGpuMemoryUsed = 109,
    SysGpuMemoryBudget = 110,
    SysGpuThrottling = 111,

    // Per-adapter series; scope is the gpu_adapter row id.
    GpuAdapterPermille = 200,
    GpuAdapterDedicatedUsed = 201,
    GpuAdapterSharedUsed = 202,
    GpuAdapterTemperatureC = 203,
    GpuAdapterPowerW = 204,
    GpuAdapterCoreClockMhz = 205,
    GpuAdapterMemoryClockMhz = 206,
    GpuAdapterFanRpm = 207,
    GpuAdapterThrottling = 208,
    GpuAdapterPowerPercent = 209,
    GpuAdapterFanPercent = 210,
    GpuAdapterMemoryTemperatureC = 211,
    GpuAdapterHotspotTemperatureC = 212,
}

impl Metric {
    /// The raw `u16` discriminant stored in SQLite.
    pub fn as_u16(self) -> u16 {
        self as u16
    }

    /// Reconstructs a `Metric` from its stored discriminant, if valid.
    pub fn from_u16(v: u16) -> Option<Metric> {
        Some(match v {
            0 => Metric::CpuPermille,
            1 => Metric::WorkingSet,
            2 => Metric::PrivateBytes,
            3 => Metric::ReadBps,
            4 => Metric::WriteBps,
            5 => Metric::GpuPermille,
            6 => Metric::GpuDedicatedBytes,
            7 => Metric::GpuSharedBytes,
            100 => Metric::SysCpuPermille,
            101 => Metric::SysMemUsed,
            102 => Metric::SysCommitUsed,
            103 => Metric::SysProcessCount,
            104 => Metric::SysThreadCount,
            105 => Metric::SysHandleCount,
            106 => Metric::SysGpuPermille,
            107 => Metric::SysGpuDedicatedUsed,
            108 => Metric::SysGpuSharedUsed,
            109 => Metric::SysGpuMemoryUsed,
            110 => Metric::SysGpuMemoryBudget,
            111 => Metric::SysGpuThrottling,
            200 => Metric::GpuAdapterPermille,
            201 => Metric::GpuAdapterDedicatedUsed,
            202 => Metric::GpuAdapterSharedUsed,
            203 => Metric::GpuAdapterTemperatureC,
            204 => Metric::GpuAdapterPowerW,
            205 => Metric::GpuAdapterCoreClockMhz,
            206 => Metric::GpuAdapterMemoryClockMhz,
            207 => Metric::GpuAdapterFanRpm,
            208 => Metric::GpuAdapterThrottling,
            209 => Metric::GpuAdapterPowerPercent,
            210 => Metric::GpuAdapterFanPercent,
            211 => Metric::GpuAdapterMemoryTemperatureC,
            212 => Metric::GpuAdapterHotspotTemperatureC,
            _ => return None,
        })
    }
}

/// The scope value used for system-wide gauges (no owning process).
pub const SYSTEM_SCOPE: i64 = 0;

/// Identity of one time series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeriesKey {
    pub metric: Metric,
    /// The owning entity: a `process_instance` row id, or [`SYSTEM_SCOPE`] (0)
    /// for system gauges.
    pub scope: i64,
}

impl SeriesKey {
    pub fn new(metric: Metric, scope: i64) -> Self {
        Self { metric, scope }
    }

    /// A system-scoped series for `metric`.
    pub fn system(metric: Metric) -> Self {
        Self {
            metric,
            scope: SYSTEM_SCOPE,
        }
    }
}

/// A sealed, encoded block ready for persistence. The `payload` is the framed
/// Gorilla block from [`BlockBuilder::finish`]; the rest are denormalised
/// header fields the store indexes on so range queries never decode a block to
/// decide whether it overlaps.
#[derive(Debug, Clone)]
pub struct EncodedBlock {
    pub key: SeriesKey,
    pub start_ms: i64,
    pub end_ms: i64,
    pub points: u32,
    pub payload: Vec<u8>,
}

/// One open head per series plus the running point count. The count is tracked
/// alongside the builder so sealing decisions don't re-walk the block.
struct Head {
    builder: BlockBuilder,
    points: u32,
    /// Wall-clock-ms timestamp of the first point in the open head, for age.
    start_ms: i64,
}

/// In-memory per-series accumulators. Appends route to the matching head;
/// [`HeadBlocks::drain_sealed`] seals heads that have reached the point cap or
/// age limit, and [`HeadBlocks::drain_all`] seals everything (shutdown).
#[derive(Default)]
pub struct HeadBlocks {
    heads: HashMap<SeriesKey, Head>,
}

impl HeadBlocks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of open series heads.
    pub fn series_count(&self) -> usize {
        self.heads.len()
    }

    /// Appends one point to `key`'s head. Ignored (returns `false`) if the
    /// timestamp is earlier than the head's last point — same rule as
    /// [`BlockBuilder::append`], so a corrupt out-of-order sample can't wedge a
    /// head.
    #[must_use]
    pub fn append(&mut self, key: SeriesKey, ts_ms: i64, value: f64) -> bool {
        let head = self.heads.entry(key).or_insert_with(|| Head {
            builder: BlockBuilder::new(),
            points: 0,
            start_ms: ts_ms,
        });
        if head.builder.append(ts_ms, value) {
            head.points += 1;
            true
        } else {
            false
        }
    }

    /// Seals and returns every head that has reached `max_points` OR whose span
    /// (last_ts − first_ts) has reached `max_age_ms`, leaving smaller/younger
    /// heads open. The sealed head is replaced by a fresh empty one on the next
    /// append (its entry is removed here).
    pub fn drain_sealed(&mut self, max_points: u32, max_age_ms: i64) -> Vec<EncodedBlock> {
        let ready: Vec<SeriesKey> = self
            .heads
            .iter()
            .filter(|(_, h)| {
                h.points >= max_points
                    || (h.points > 0 && h.builder.last_ts_ms() - h.start_ms >= max_age_ms)
            })
            .map(|(k, _)| *k)
            .collect();
        ready.into_iter().filter_map(|k| self.seal(k)).collect()
    }

    /// Seals and returns every non-empty head (final drain on shutdown or on a
    /// series going away, e.g. a process exiting).
    pub fn drain_all(&mut self) -> Vec<EncodedBlock> {
        let keys: Vec<SeriesKey> = self.heads.keys().copied().collect();
        keys.into_iter().filter_map(|k| self.seal(k)).collect()
    }

    /// Seals and returns just the heads for `scope` (a process exiting: flush
    /// its series so nothing is lost, then forget them).
    pub fn drain_scope(&mut self, scope: i64) -> Vec<EncodedBlock> {
        let keys: Vec<SeriesKey> = self
            .heads
            .keys()
            .copied()
            .filter(|k| k.scope == scope)
            .collect();
        keys.into_iter().filter_map(|k| self.seal(k)).collect()
    }

    /// Removes a head and encodes it, or `None` if absent/empty.
    fn seal(&mut self, key: SeriesKey) -> Option<EncodedBlock> {
        let head = self.heads.remove(&key)?;
        if head.points == 0 {
            return None;
        }
        let start_ms = head.start_ms;
        let end_ms = head.builder.last_ts_ms();
        let points = head.points;
        let payload = head.builder.finish();
        Some(EncodedBlock {
            key,
            start_ms,
            end_ms,
            points,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockReader;

    #[test]
    fn metric_discriminants_roundtrip() {
        for m in [
            Metric::CpuPermille,
            Metric::WorkingSet,
            Metric::PrivateBytes,
            Metric::ReadBps,
            Metric::WriteBps,
            Metric::SysCpuPermille,
            Metric::SysMemUsed,
            Metric::SysCommitUsed,
            Metric::SysProcessCount,
            Metric::SysThreadCount,
            Metric::SysHandleCount,
            Metric::GpuAdapterPowerPercent,
            Metric::GpuAdapterFanPercent,
            Metric::GpuAdapterMemoryTemperatureC,
            Metric::GpuAdapterHotspotTemperatureC,
        ] {
            assert_eq!(Metric::from_u16(m.as_u16()), Some(m));
        }
        assert_eq!(Metric::from_u16(9999), None);
        assert_eq!(Metric::GpuAdapterPowerPercent.as_u16(), 209);
        assert_eq!(Metric::GpuAdapterFanPercent.as_u16(), 210);
        assert_eq!(Metric::GpuAdapterMemoryTemperatureC.as_u16(), 211);
        assert_eq!(Metric::GpuAdapterHotspotTemperatureC.as_u16(), 212);
    }

    #[test]
    fn seals_on_point_cap() {
        let mut hb = HeadBlocks::new();
        let key = SeriesKey::new(Metric::CpuPermille, 5);
        for i in 0..120 {
            assert!(hb.append(key, 1000 + i * 1000, i as f64));
        }
        let sealed = hb.drain_sealed(120, i64::MAX);
        assert_eq!(sealed.len(), 1);
        let blk = &sealed[0];
        assert_eq!(blk.points, 120);
        assert_eq!(blk.key, key);
        assert_eq!(blk.start_ms, 1000);
        assert_eq!(blk.end_ms, 1000 + 119 * 1000);
        // The head is gone until the next append.
        assert_eq!(hb.series_count(), 0);
        // Payload decodes back.
        let pts = BlockReader::parse(&blk.payload).unwrap().points().unwrap();
        assert_eq!(pts.len(), 120);
    }

    #[test]
    fn seals_on_age() {
        let mut hb = HeadBlocks::new();
        let key = SeriesKey::system(Metric::SysCpuPermille);
        // Only 3 points but spanning 3 minutes → age-sealed at a 2-min limit.
        assert!(hb.append(key, 0, 1.0));
        assert!(hb.append(key, 90_000, 2.0));
        assert!(hb.append(key, 180_000, 3.0));
        let sealed = hb.drain_sealed(120, 120_000);
        assert_eq!(sealed.len(), 1);
        assert_eq!(sealed[0].points, 3);
    }

    #[test]
    fn young_small_head_is_not_sealed() {
        let mut hb = HeadBlocks::new();
        let key = SeriesKey::new(Metric::WorkingSet, 7);
        assert!(hb.append(key, 1000, 1.0));
        assert!(hb.append(key, 2000, 2.0));
        let sealed = hb.drain_sealed(120, 120_000);
        assert!(sealed.is_empty(), "small young head must stay open");
        assert_eq!(hb.series_count(), 1);
    }

    #[test]
    fn drain_all_flushes_everything() {
        let mut hb = HeadBlocks::new();
        assert!(hb.append(SeriesKey::new(Metric::CpuPermille, 1), 1000, 1.0));
        assert!(hb.append(SeriesKey::new(Metric::WorkingSet, 1), 1000, 2.0));
        assert!(hb.append(SeriesKey::system(Metric::SysCpuPermille), 1000, 3.0));
        let sealed = hb.drain_all();
        assert_eq!(sealed.len(), 3);
        assert_eq!(hb.series_count(), 0);
    }

    #[test]
    fn drain_scope_flushes_only_that_scope() {
        let mut hb = HeadBlocks::new();
        assert!(hb.append(SeriesKey::new(Metric::CpuPermille, 1), 1000, 1.0));
        assert!(hb.append(SeriesKey::new(Metric::WorkingSet, 1), 1000, 2.0));
        assert!(hb.append(SeriesKey::new(Metric::CpuPermille, 2), 1000, 3.0));
        let sealed = hb.drain_scope(1);
        assert_eq!(sealed.len(), 2, "both series for scope 1");
        assert!(sealed.iter().all(|b| b.key.scope == 1));
        assert_eq!(hb.series_count(), 1, "scope 2 remains open");
    }

    #[test]
    fn out_of_order_append_is_rejected_not_wedging() {
        let mut hb = HeadBlocks::new();
        let key = SeriesKey::new(Metric::CpuPermille, 1);
        assert!(hb.append(key, 2000, 1.0));
        assert!(!hb.append(key, 1999, 2.0), "earlier ts rejected");
        assert!(hb.append(key, 2001, 3.0), "later ts still accepted");
        let sealed = hb.drain_all();
        assert_eq!(sealed[0].points, 2);
    }
}
