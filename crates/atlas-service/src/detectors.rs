//! Threshold+duration incident detectors (docs/phases.md M8, PRD §9.3.7).
//!
//! Two detectors run over the recorded system series:
//! - **CPU saturation** — system CPU at/above [`CPU_SATURATION_PCT`] for at
//!   least [`CPU_MIN_DURATION_MS`].
//! - **Memory pressure** — `mem_used / mem_total` at/above [`MEM_PRESSURE_PCT`]
//!   for at least [`MEM_MIN_DURATION_MS`].
//!
//! **Disk latency is deferred.** The proto defines `DISK_LATENCY` but Atlas
//! records no disk-latency metric yet — only per-process read/write *throughput*
//! (bytes/s), which is not latency. A latency threshold would have to be
//! fabricated from data we do not have, so disk incidents are intentionally not
//! detected until a real latency metric exists (brief: do NOT fabricate a
//! metric). The kind stays reserved so it can light up additively later.
//!
//! The [`detect`] core is pure and generic over `(ts, value)` samples so it can
//! be unit-tested exhaustively without a store; [`run_detection_pass`] wires it
//! to the recorded series and the idempotent `incident` upsert.

use atlas_store::Store;
use atlas_tsdb::{Metric, SYSTEM_SCOPE};

/// Incident-kind discriminants, matching the proto `IncidentKind`.
pub const KIND_CPU_SATURATION: i32 = 1;
pub const KIND_MEMORY_PRESSURE: i32 = 2;
/// Reserved (proto `DISK_LATENCY`); not detected — see module docs.
pub const KIND_DISK_LATENCY: i32 = 3;
pub const KIND_GPU_SATURATION: i32 = 4;
pub const KIND_GPU_MEMORY_EXHAUSTION: i32 = 5;
pub const KIND_GPU_THERMAL_THROTTLING: i32 = 6;

/// Severity discriminants, matching the proto `Severity`.
pub const SEV_INFO: i32 = 1;
pub const SEV_WARNING: i32 = 2;
pub const SEV_CRITICAL: i32 = 3;

/// System CPU at/above this percent counts as saturating.
pub const CPU_SATURATION_PCT: f64 = 85.0;
/// A CPU saturation must be sustained at least this long to be an incident.
pub const CPU_MIN_DURATION_MS: i64 = 10_000;
/// Memory used (as a percent of total) at/above this counts as pressure.
pub const MEM_PRESSURE_PCT: f64 = 90.0;
/// A memory pressure must be sustained at least this long to be an incident.
pub const MEM_MIN_DURATION_MS: i64 = 10_000;
pub const GPU_SATURATION_PCT: f64 = 85.0;
pub const GPU_MEMORY_PCT: f64 = 90.0;
pub const GPU_MIN_DURATION_MS: i64 = 10_000;

/// Samples more than this far apart never belong to the same incident: a gap
/// this large is missing data (a writer stall / monitoring off), and merging
/// across it would invent a continuity we never observed (PRD §11.3). Sized
/// generously above the slow adaptive cadence tier (15 s) so ordinary cadence
/// widening does not fragment a real incident.
pub const MAX_GAP_MS: i64 = 20_000;

/// One detected run over a series: the timestamp of the first at/above-threshold
/// sample, the end, and the peak value seen in the run. `end_ms == 0` means the
/// run reached the last sample in the slice — the incident was still ongoing at
/// the last observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Run {
    pub start_ms: i64,
    pub end_ms: i64,
    pub peak: f64,
}

/// The pure detection core (unit-tested exhaustively). Scans `samples` — assumed
/// ascending by timestamp — for maximal runs of consecutive at/above-threshold
/// points and returns those whose span reaches `min_duration_ms`.
///
/// Rules:
/// - `value >= threshold` is in-incident (**at**-threshold is included).
/// - A run ends when a below-threshold sample appears **or** the gap to the next
///   sample exceeds `max_gap_ms` (data gaps never merge).
/// - A run whose span `(last_ts - start_ts)` is shorter than `min_duration_ms`
///   is ignored (a transient spike is not an incident).
/// - A run that reaches the final sample is returned with `end_ms == 0`
///   (ongoing); otherwise `end_ms` is the last at/above-threshold timestamp.
pub fn detect(
    samples: &[(i64, f64)],
    threshold: f64,
    min_duration_ms: i64,
    max_gap_ms: i64,
) -> Vec<Run> {
    let mut out = Vec::new();
    let n = samples.len();
    let mut i = 0;
    while i < n {
        let (start, first_v) = samples[i];
        if first_v < threshold {
            i += 1;
            continue;
        }
        // Extend a run of consecutive at/above-threshold samples with no gap
        // larger than `max_gap_ms`.
        let mut last = start;
        let mut peak = first_v;
        let mut j = i + 1;
        while j < n {
            let (nts, nv) = samples[j];
            if nts - last > max_gap_ms || nv < threshold {
                break;
            }
            last = nts;
            peak = peak.max(nv);
            j += 1;
        }
        // Reached the end of the slice without a terminating sample/gap => the
        // incident was still ongoing at the last observation.
        let reached_end = j == n;
        if last - start >= min_duration_ms {
            out.push(Run {
                start_ms: start,
                end_ms: if reached_end { 0 } else { last },
                peak,
            });
        }
        // Resume after this run; `j >= i + 1` always, so this makes progress.
        i = j;
    }
    out
}

/// Reads the recorded CPU and memory system series over `[from_ms, to_ms]` and
/// upserts any detected incidents into the store. `mem_total` is the machine's
/// physical memory in bytes, used to turn recorded `SysMemUsed` bytes into a
/// percent; pass 0 to skip memory detection (total unknown). Returns the number
/// of incident rows written or updated.
///
/// Detection is idempotent: the store keys incidents by `(kind, start_ms)`, so
/// re-running this over overlapping windows (each flush, and again at shutdown)
/// extends the same episode rather than duplicating it.
pub fn run_detection_pass(
    store: &Store,
    from_ms: i64,
    to_ms: i64,
    mem_total: u64,
) -> anyhow::Result<usize> {
    let mut count = 0;

    // CPU saturation: SysCpuPermille (0..=1000) -> percent (permille / 10).
    let cpu = load_series(store, Metric::SysCpuPermille, from_ms, to_ms, 0.1)?;
    for run in detect(&cpu, CPU_SATURATION_PCT, CPU_MIN_DURATION_MS, MAX_GAP_MS) {
        store.upsert_incident(
            KIND_CPU_SATURATION,
            run.start_ms,
            end_opt(run.end_ms),
            cpu_severity(run.peak),
            run.peak,
            &cpu_summary(&run),
        )?;
        count += 1;
    }

    // Memory pressure: SysMemUsed (bytes) -> percent of total.
    if mem_total > 0 {
        let scale = 100.0 / mem_total as f64;
        let mem = load_series(store, Metric::SysMemUsed, from_ms, to_ms, scale)?;
        for run in detect(&mem, MEM_PRESSURE_PCT, MEM_MIN_DURATION_MS, MAX_GAP_MS) {
            store.upsert_incident(
                KIND_MEMORY_PRESSURE,
                run.start_ms,
                end_opt(run.end_ms),
                mem_severity(run.peak),
                run.peak,
                &mem_summary(&run),
            )?;
            count += 1;
        }
    }

    let gpu = load_series(store, Metric::SysGpuPermille, from_ms, to_ms, 0.1)?;
    for run in detect(&gpu, GPU_SATURATION_PCT, GPU_MIN_DURATION_MS, MAX_GAP_MS) {
        store.upsert_incident(
            KIND_GPU_SATURATION, run.start_ms, end_opt(run.end_ms),
            cpu_severity(run.peak), run.peak,
            &format!("GPU saturation ({}): the busiest graphics engine held at or above {:.0}% (peak {:.0}%).",
                if run.end_ms == 0 { "ongoing" } else { "resolved" }, GPU_SATURATION_PCT, run.peak),
        )?;
        count += 1;
    }

    let used = load_series(store, Metric::SysGpuMemoryUsed, from_ms, to_ms, 1.0)?;
    let budgets: std::collections::HashMap<i64, f64> =
        load_series(store, Metric::SysGpuMemoryBudget, from_ms, to_ms, 1.0)?.into_iter().collect();
    let memory_pct: Vec<_> = used.into_iter().filter_map(|(ts, value)| {
        let budget = budgets.get(&ts).copied().unwrap_or(0.0);
        (budget > 0.0).then_some((ts, value / budget * 100.0))
    }).collect();
    for run in detect(&memory_pct, GPU_MEMORY_PCT, GPU_MIN_DURATION_MS, MAX_GAP_MS) {
        store.upsert_incident(
            KIND_GPU_MEMORY_EXHAUSTION, run.start_ms, end_opt(run.end_ms),
            mem_severity(run.peak), run.peak,
            &format!("GPU memory pressure ({}): measured graphics memory held at or above {:.0}% of the reported budget (peak {:.0}%).",
                if run.end_ms == 0 { "ongoing" } else { "resolved" }, GPU_MEMORY_PCT, run.peak),
        )?;
        count += 1;
    }

    // This series exists only when a vendor provider explicitly reports a
    // throttle state; no temperature threshold is guessed.
    let throttling = load_series(store, Metric::SysGpuThrottling, from_ms, to_ms, 1.0)?;
    for run in detect(&throttling, 0.5, 0, MAX_GAP_MS) {
        store.upsert_incident(
            KIND_GPU_THERMAL_THROTTLING, run.start_ms, end_opt(run.end_ms), SEV_WARNING, 1.0,
            &format!("GPU thermal throttling {}: a hardware sensor provider reported an active throttle state.",
                if run.end_ms == 0 { "is ongoing" } else { "was reported" }),
        )?;
        count += 1;
    }
    Ok(count)
}

/// `end_ms == 0` (ongoing) maps to a NULL end in the store.
fn end_opt(end_ms: i64) -> Option<i64> {
    if end_ms == 0 {
        None
    } else {
        Some(end_ms)
    }
}

/// Loads a system-scope metric series over the window as `(ts, value * scale)`,
/// sorted ascending by timestamp. Points outside the window are dropped.
fn load_series(
    store: &Store,
    metric: Metric,
    from_ms: i64,
    to_ms: i64,
    scale: f64,
) -> anyhow::Result<Vec<(i64, f64)>> {
    let mut pts = Vec::new();
    for blk in store.read_blocks(metric, Some(SYSTEM_SCOPE), from_ms, to_ms)? {
        for (ts, v) in blk.points {
            if ts >= from_ms && ts <= to_ms {
                pts.push((ts, v * scale));
            }
        }
    }
    pts.sort_by_key(|&(ts, _)| ts);
    Ok(pts)
}

/// Maps a CPU peak (percent) to a severity band.
pub fn cpu_severity(peak_pct: f64) -> i32 {
    if peak_pct >= 95.0 {
        SEV_CRITICAL
    } else if peak_pct >= 90.0 {
        SEV_WARNING
    } else {
        SEV_INFO
    }
}

/// Maps a memory peak (percent) to a severity band.
pub fn mem_severity(peak_pct: f64) -> i32 {
    if peak_pct >= 97.0 {
        SEV_CRITICAL
    } else if peak_pct >= 94.0 {
        SEV_WARNING
    } else {
        SEV_INFO
    }
}

fn cpu_summary(run: &Run) -> String {
    let state = if run.end_ms == 0 {
        "ongoing"
    } else {
        "resolved"
    };
    format!(
        "CPU saturation ({state}): system CPU held at or above {:.0}% (peak {:.0}%).",
        CPU_SATURATION_PCT, run.peak
    )
}

fn mem_summary(run: &Run) -> String {
    let state = if run.end_ms == 0 {
        "ongoing"
    } else {
        "resolved"
    };
    format!(
        "Memory pressure ({state}): memory in use held at or above {:.0}% of total (peak {:.0}%).",
        MEM_PRESSURE_PCT, run.peak
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Sensible fixed knobs for the core tests: threshold 85, sustain 10 s,
    // gap 20 s. Samples are (ms, value) at ~1 s cadence unless a gap is noted.
    const T: f64 = 85.0;
    const MIN: i64 = 10_000;
    const GAP: i64 = 20_000;

    fn series(vals: &[(i64, f64)]) -> Vec<(i64, f64)> {
        vals.to_vec()
    }

    #[test]
    fn all_below_threshold_yields_nothing() {
        let s = series(&[(0, 10.0), (1_000, 50.0), (2_000, 84.999)]);
        assert!(detect(&s, T, MIN, GAP).is_empty());
    }

    #[test]
    fn at_threshold_counts_as_in_incident() {
        // Exactly at threshold for 12 s (>= 10 s) is an incident.
        let s: Vec<(i64, f64)> = (0..=12).map(|k| (k * 1_000, 85.0)).collect();
        let runs = detect(&s, T, MIN, GAP);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].start_ms, 0);
        // Reaches the last sample -> ongoing.
        assert_eq!(runs[0].end_ms, 0);
        assert_eq!(runs[0].peak, 85.0);
    }

    #[test]
    fn spike_shorter_than_min_duration_is_ignored() {
        // Above threshold for only 3 s, then back down. 3 s < 10 s -> ignored.
        let s = series(&[
            (0, 10.0),
            (1_000, 90.0),
            (2_000, 95.0),
            (3_000, 92.0),
            (4_000, 10.0),
            (5_000, 10.0),
        ]);
        assert!(detect(&s, T, MIN, GAP).is_empty());
    }

    #[test]
    fn sustained_run_that_resolves_has_concrete_end_and_peak() {
        // Above from 1 s..=13 s (12 s span), then drops -> resolved at 13_000.
        let mut v = vec![(0i64, 10.0)];
        for k in 1..=13 {
            v.push((k * 1_000, if k == 7 { 99.0 } else { 90.0 }));
        }
        v.push((14_000, 20.0));
        v.push((15_000, 20.0));
        let runs = detect(&v, T, MIN, GAP);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].start_ms, 1_000);
        assert_eq!(runs[0].end_ms, 13_000, "concrete end at last above-sample");
        assert_eq!(runs[0].peak, 99.0, "peak preserved");
    }

    #[test]
    fn ongoing_incident_reports_end_zero() {
        // Rises above at 2 s and stays above through the final sample.
        let mut v = vec![(0i64, 10.0), (1_000, 20.0)];
        for k in 2..=15 {
            v.push((k * 1_000, 91.0));
        }
        let runs = detect(&v, T, MIN, GAP);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].start_ms, 2_000);
        assert_eq!(runs[0].end_ms, 0, "still ongoing at last observation");
    }

    #[test]
    fn back_to_back_incidents_are_separate() {
        // Run A: 0..=11 s above; one sample below at 12 s; Run B: 13..=25 s above.
        let mut v = Vec::new();
        for k in 0..=11 {
            v.push((k * 1_000, 90.0));
        }
        v.push((12_000, 10.0)); // dip below separates the two
        for k in 13..=25 {
            v.push((k * 1_000, 92.0));
        }
        let runs = detect(&v, T, MIN, GAP);
        assert_eq!(runs.len(), 2, "two distinct incidents");
        assert_eq!(runs[0].start_ms, 0);
        assert_eq!(runs[0].end_ms, 11_000);
        assert_eq!(runs[1].start_ms, 13_000);
        assert_eq!(runs[1].end_ms, 0, "second run reaches the end");
    }

    #[test]
    fn data_gap_does_not_merge_across() {
        // Above 0..=11 s, then a 30 s data gap (> 20 s), then above again.
        // The first run is a resolved incident; the post-gap stretch is only
        // 6 s long (< 10 s) so it is NOT its own incident — proving the gap
        // split (a merge would have made one long incident).
        let mut v = Vec::new();
        for k in 0..=11 {
            v.push((k * 1_000, 90.0));
        }
        // Gap: next sample 30 s after the last (41_000 - 11_000 = 30_000).
        for k in 0..=6 {
            v.push((41_000 + k * 1_000, 90.0));
        }
        let runs = detect(&v, T, MIN, GAP);
        assert_eq!(runs.len(), 1, "gap split; short tail is not an incident");
        assert_eq!(runs[0].start_ms, 0);
        assert_eq!(runs[0].end_ms, 11_000, "first run ends before the gap");
    }

    #[test]
    fn gap_split_can_yield_two_full_incidents() {
        // Two 12 s incidents separated by a > 20 s gap: both qualify, neither
        // merges with the other.
        let mut v = Vec::new();
        for k in 0..=12 {
            v.push((k * 1_000, 88.0));
        }
        for k in 0..=12 {
            v.push((60_000 + k * 1_000, 93.0));
        }
        let runs = detect(&v, T, MIN, GAP);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].start_ms, 0);
        assert_eq!(runs[0].end_ms, 12_000);
        assert_eq!(runs[1].start_ms, 60_000);
        assert_eq!(runs[1].end_ms, 0);
    }

    #[test]
    fn empty_series_is_empty() {
        assert!(detect(&[], T, MIN, GAP).is_empty());
    }

    #[test]
    fn single_above_sample_is_not_an_incident() {
        // One point has zero span -> below min duration.
        assert!(detect(&[(5_000, 99.0)], T, MIN, GAP).is_empty());
    }

    #[test]
    fn severity_bands() {
        assert_eq!(cpu_severity(86.0), SEV_INFO);
        assert_eq!(cpu_severity(91.0), SEV_WARNING);
        assert_eq!(cpu_severity(99.0), SEV_CRITICAL);
        assert_eq!(mem_severity(91.0), SEV_INFO);
        assert_eq!(mem_severity(95.0), SEV_WARNING);
        assert_eq!(mem_severity(98.0), SEV_CRITICAL);
    }
}
