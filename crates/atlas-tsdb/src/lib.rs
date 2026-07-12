//! Time-series storage (tech-stack.md §4.2).
//!
//! Interim implementation: an in-memory series head with the API shape the
//! persistent store will keep (push / range / min-max decimation). The
//! chunked, Gorilla-compressed, tiered on-disk store replaces the internals
//! at milestone M-TSDB (docs/phases.md) without changing this surface.

use std::collections::VecDeque;

/// Fixed-capacity ring of (timestamp_ms, value) points for one series.
pub struct SeriesRing {
    cap: usize,
    points: VecDeque<(i64, f64)>,
}

/// One decimation bucket; `min`/`max` preserve short spikes that plain
/// averaging would hide (PRD §11.3: "show peaks without hiding short events").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bucket {
    pub start_ms: i64,
    pub min: f64,
    pub max: f64,
}

impl SeriesRing {
    pub fn new(cap: usize) -> Self {
        assert!(cap > 0, "series capacity must be non-zero");
        Self {
            cap,
            points: VecDeque::with_capacity(cap),
        }
    }

    pub fn push(&mut self, ts_ms: i64, value: f64) {
        if self.points.len() == self.cap {
            self.points.pop_front();
        }
        self.points.push_back((ts_ms, value));
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn range(&self, from_ms: i64, to_ms: i64) -> impl Iterator<Item = (i64, f64)> + '_ {
        self.points
            .iter()
            .copied()
            .filter(move |(t, _)| *t >= from_ms && *t <= to_ms)
    }

    /// Buckets `[from, to)` into at most `buckets` min/max pairs — the UI
    /// never receives more points than pixels. Empty buckets are omitted
    /// (callers render them as missing data, not zero).
    pub fn decimate_minmax(&self, from_ms: i64, to_ms: i64, buckets: usize) -> Vec<Bucket> {
        if buckets == 0 || to_ms <= from_ms {
            return Vec::new();
        }
        let span = (to_ms - from_ms) as u128;
        let mut acc: Vec<Option<Bucket>> = vec![None; buckets];
        for (t, v) in self.range(from_ms, to_ms - 1) {
            let idx = (((t - from_ms) as u128) * buckets as u128 / span) as usize;
            let idx = idx.min(buckets - 1);
            let slot = &mut acc[idx];
            match slot {
                Some(b) => {
                    b.min = b.min.min(v);
                    b.max = b.max.max(v);
                }
                None => {
                    let width = (span / buckets as u128) as i64;
                    *slot = Some(Bucket {
                        start_ms: from_ms + idx as i64 * width,
                        min: v,
                        max: v,
                    });
                }
            }
        }
        acc.into_iter().flatten().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_evicts_oldest_at_capacity() {
        let mut s = SeriesRing::new(3);
        for i in 0..5 {
            s.push(i, i as f64);
        }
        assert_eq!(s.len(), 3);
        let pts: Vec<_> = s.range(i64::MIN, i64::MAX).collect();
        assert_eq!(pts, vec![(2, 2.0), (3, 3.0), (4, 4.0)]);
    }

    #[test]
    fn range_respects_bounds() {
        let mut s = SeriesRing::new(10);
        for i in 0..10 {
            s.push(i * 100, i as f64);
        }
        let pts: Vec<_> = s.range(200, 400).collect();
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0].0, 200);
        assert_eq!(pts[2].0, 400);
    }

    #[test]
    fn decimation_preserves_spikes() {
        let mut s = SeriesRing::new(1000);
        for i in 0..1000 {
            let v = if i == 500 { 99.0 } else { 1.0 };
            s.push(i, v);
        }
        let buckets = s.decimate_minmax(0, 1000, 10);
        assert_eq!(buckets.len(), 10);
        let spike = buckets.iter().find(|b| b.max == 99.0);
        assert!(spike.is_some(), "the single-sample spike must survive");
        assert_eq!(spike.unwrap().min, 1.0);
    }

    #[test]
    fn decimation_omits_empty_buckets() {
        let mut s = SeriesRing::new(10);
        s.push(0, 1.0);
        s.push(900, 2.0);
        let buckets = s.decimate_minmax(0, 1000, 10);
        assert_eq!(buckets.len(), 2, "8 empty buckets omitted");
    }
}
