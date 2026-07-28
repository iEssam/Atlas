//! Pure before/after comparison math. Storage and RPC concerns stay in `ipc`;
//! this module only turns measured period evidence into an honest result.

use std::collections::BTreeSet;

use atlas_store::RangeBucketRow;

#[derive(Debug, Clone, PartialEq)]
pub struct PeriodEvidence {
    pub buckets: Vec<RangeBucketRow>,
    pub process_starts: BTreeSet<String>,
    pub events_truncated: bool,
    pub crashes: usize,
    pub crashes_truncated: bool,
    pub system_changes: usize,
    pub changes_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeriodSummary {
    pub average: f64,
    pub peak: f64,
    pub duration_above_threshold_ms: u64,
    pub samples: u64,
    pub populated_buckets: u32,
    pub crashes: u32,
    pub system_changes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Insufficient,
    Improved,
    Regressed,
    NoClearChange,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub baseline: PeriodSummary,
    pub followup: PeriodSummary,
    pub verdict: Verdict,
    pub delta_percent: f64,
    pub new_processes: Vec<String>,
    pub removed_processes: Vec<String>,
    pub incomplete: bool,
    pub data_quality: String,
}

pub fn compare(
    baseline: &PeriodEvidence,
    followup: &PeriodEvidence,
    baseline_span_ms: i64,
    followup_span_ms: i64,
    threshold: f64,
    target_buckets: u32,
) -> Comparison {
    let baseline_summary = summarize(baseline, baseline_span_ms, threshold, target_buckets);
    let followup_summary = summarize(followup, followup_span_ms, threshold, target_buckets);
    let incomplete = baseline.events_truncated
        || followup.events_truncated
        || baseline.crashes_truncated
        || followup.crashes_truncated
        || baseline.changes_truncated
        || followup.changes_truncated;

    let enough = baseline_summary.samples >= 10
        && followup_summary.samples >= 10
        && baseline_summary.populated_buckets >= 3
        && followup_summary.populated_buckets >= 3;
    let delta_percent = if baseline_summary.average.abs() > f64::EPSILON {
        (followup_summary.average - baseline_summary.average) / baseline_summary.average * 100.0
    } else {
        0.0
    };
    // Supported experiment metrics are resource pressure measures, where lower
    // is better. A 5% deadband avoids declaring normal measurement noise a win.
    let verdict = if !enough {
        Verdict::Insufficient
    } else if delta_percent <= -5.0 {
        Verdict::Improved
    } else if delta_percent >= 5.0 {
        Verdict::Regressed
    } else {
        Verdict::NoClearChange
    };

    let new_processes = followup
        .process_starts
        .difference(&baseline.process_starts)
        .cloned()
        .collect();
    let removed_processes = baseline
        .process_starts
        .difference(&followup.process_starts)
        .cloned()
        .collect();
    let data_quality = if !enough {
        "Insufficient retained samples: each period needs at least 10 samples across 3 buckets."
            .to_string()
    } else if incomplete {
        "Metric evidence is sufficient, but one or more event lists were truncated.".to_string()
    } else {
        "Both periods contain sufficient retained metric and event evidence.".to_string()
    };

    Comparison {
        baseline: baseline_summary,
        followup: followup_summary,
        verdict,
        delta_percent,
        new_processes,
        removed_processes,
        incomplete,
        data_quality,
    }
}

fn summarize(
    evidence: &PeriodEvidence,
    span_ms: i64,
    threshold: f64,
    target_buckets: u32,
) -> PeriodSummary {
    let samples = evidence
        .buckets
        .iter()
        .map(|b| b.samples as u64)
        .sum::<u64>();
    let weighted = evidence
        .buckets
        .iter()
        .map(|b| b.avg * b.samples as f64)
        .sum::<f64>();
    let average = if samples == 0 {
        0.0
    } else {
        weighted / samples as f64
    };
    let peak = evidence
        .buckets
        .iter()
        .map(|b| b.max)
        .reduce(f64::max)
        .unwrap_or(0.0);
    let bucket_ms = span_ms.max(0) as u64 / target_buckets.max(1) as u64;
    // A bucket counts only when its mean clears the threshold. This conservative
    // estimate avoids turning one spike in a wide bucket into a full interval.
    let above = evidence
        .buckets
        .iter()
        .filter(|b| b.avg >= threshold)
        .count() as u64;
    PeriodSummary {
        average,
        peak,
        duration_above_threshold_ms: above.saturating_mul(bucket_ms),
        samples,
        populated_buckets: evidence.buckets.len() as u32,
        crashes: evidence.crashes.min(u32::MAX as usize) as u32,
        system_changes: evidence.system_changes.min(u32::MAX as usize) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(avg: f64, names: &[&str]) -> PeriodEvidence {
        PeriodEvidence {
            buckets: (0..4)
                .map(|i| RangeBucketRow {
                    start_ms: i * 1_000,
                    min: avg - 1.0,
                    max: avg + 2.0,
                    avg,
                    samples: 3,
                })
                .collect(),
            process_starts: names.iter().map(|s| (*s).to_string()).collect(),
            events_truncated: false,
            crashes: 0,
            crashes_truncated: false,
            system_changes: 0,
            changes_truncated: false,
        }
    }

    #[test]
    fn lower_followup_is_improved_and_process_sets_are_differenced() {
        let result = compare(
            &evidence(60.0, &["a.exe", "old.exe"]),
            &evidence(48.0, &["a.exe", "new.exe"]),
            4_000,
            4_000,
            50.0,
            4,
        );
        assert_eq!(result.verdict, Verdict::Improved);
        assert_eq!(result.new_processes, vec!["new.exe"]);
        assert_eq!(result.removed_processes, vec!["old.exe"]);
        assert_eq!(result.baseline.duration_above_threshold_ms, 4_000);
        assert_eq!(result.followup.duration_above_threshold_ms, 0);
    }

    #[test]
    fn sparse_data_never_claims_success() {
        let mut sparse = evidence(20.0, &[]);
        sparse.buckets.truncate(2);
        let result = compare(&evidence(40.0, &[]), &sparse, 4_000, 4_000, 30.0, 4);
        assert_eq!(result.verdict, Verdict::Insufficient);
    }

    #[test]
    fn truncation_is_reported_without_discarding_metric_result() {
        let baseline = evidence(40.0, &[]);
        let mut followup = evidence(50.0, &[]);
        followup.events_truncated = true;
        let result = compare(&baseline, &followup, 4_000, 4_000, 45.0, 4);
        assert_eq!(result.verdict, Verdict::Regressed);
        assert!(result.incomplete);
    }
}
