//! Leak-detection soak analysis (docs/phases.md M9; tech-stack §10 "Soak test
//! slope + gate", PRD §12.2 — the tool watches itself).
//!
//! The [`soak`](crate) command runs the real record pipeline for N minutes and
//! samples its OWN working set + handle count periodically. This module holds
//! the PURE math that turns that time series into a PASS/FAIL verdict: a linear
//! least-squares fit of RSS against time (extrapolated to MB/hour) plus the peak
//! handle growth over the run. Keeping it free of any Windows/IO dependency lets
//! the slope/verdict logic be unit-tested with synthetic series (flat = pass,
//! rising = fail) — the live pipeline is exercised separately by the command.

/// One periodic self-observation during a soak run.
#[derive(Debug, Clone, Copy)]
pub struct SoakSample {
    /// Seconds since the run started.
    pub t_s: f64,
    /// Own-process working set (resident set) in bytes at this instant.
    pub rss_bytes: u64,
    /// Own-process handle count at this instant.
    pub handles: u32,
}

/// The verdict computed from a soak series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoakVerdict {
    /// Total self-samples collected during the run (including warmup).
    pub samples: usize,
    /// Post-warmup samples the slope/growth were actually computed over.
    pub analyzed_samples: usize,
    /// The warmup window (seconds) excluded from the fit.
    pub warmup_s: f64,
    /// Least-squares slope of RSS vs time, extrapolated to MB per hour. Positive
    /// means the working set is trending up (potential leak); near-zero or
    /// negative is healthy.
    pub rss_slope_mb_per_hour: f64,
    /// Absolute RSS rise the fit predicts over the analyzed window (MB). The
    /// materiality gate: a leak must both exceed the slope threshold and predict
    /// a rise above [`DEFAULT_MIN_RSS_RISE_MB`] over the observed window.
    pub fitted_rise_mb: f64,
    /// First (baseline) RSS in MB, for context.
    pub rss_first_mb: f64,
    /// Peak RSS in MB observed during the run.
    pub rss_peak_mb: f64,
    /// Handle count of the first sample (baseline).
    pub handles_first: u32,
    /// Peak handle count observed during the run.
    pub handles_peak: u32,
    /// Peak handle growth over baseline (`handles_peak - handles_first`), floored
    /// at 0 — a run that sheds handles is not a leak.
    pub peak_handle_growth: i64,
    /// The RSS-slope threshold (MB/hour) the verdict was graded against.
    pub slope_threshold_mb_per_hour: f64,
    /// The handle-growth threshold the verdict was graded against.
    pub handle_growth_threshold: i64,
    /// Overall PASS (`true`) when neither the RSS slope nor the handle growth
    /// exceeds its threshold. FAIL when either does.
    pub pass: bool,
    /// True when there were too few samples to fit a slope (`< 2`): the slope is
    /// reported as 0 and cannot fail on its own, but the verdict is flagged so a
    /// caller/CI can treat an empty run as inconclusive rather than a clean pass.
    pub insufficient: bool,
}

/// Default RSS-slope failure threshold: flag a leak if the working set trends up
/// by more than ~5 MB/hour extrapolated (tunable per the M9 bullet). Chosen so a
/// flat/steady-state service passes comfortably while a genuine monotonic climb
/// over a multi-minute window trips it.
pub const DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR: f64 = 5.0;

/// Default peak-handle-growth failure threshold: a healthy service reaches a
/// stable handle count quickly, so sustained growth beyond this many handles
/// over the run is treated as a leak signal.
pub const DEFAULT_HANDLE_GROWTH_THRESHOLD: i64 = 200;

/// Materiality floor (MB): the slope only counts as a leak if the fit predicts
/// at least this much absolute RSS rise *over the window actually observed*. A
/// short run amplifies sub-MB settling noise into a large extrapolated MB/hour
/// figure; requiring a material fitted rise means a few-minute CI run can only
/// trip on a genuinely fast leak (which accumulates real MB even in minutes),
/// while a 72 h run — where even a slow 5 MB/hour leak accumulates hundreds of
/// MB — still catches slow leaks. This is why the same 5 MB/hour slope threshold
/// works for both the quick CI gate and the nightly long soak (tech-stack §10).
pub const DEFAULT_MIN_RSS_RISE_MB: f64 = 5.0;

/// Default warmup window (seconds) excluded from the slope fit. The record
/// pipeline's working set ramps for the first ~30-60 s as the store opens, the
/// process-id cache fills, and the first head blocks build before they start
/// sealing at the point cap. Fitting a trend through that one-time ramp would
/// extrapolate to a huge false leak slope on a short run, so those early samples
/// are dropped before fitting. Negligible for a 72 h soak; sized so a few-minute
/// CI run still keeps a usable post-warmup window.
pub const DEFAULT_WARMUP_SECS: f64 = 45.0;

const BYTES_PER_MB: f64 = 1024.0 * 1024.0;

/// Least-squares slope of `y` against `x` (the classic
/// `Σ(x-x̄)(y-ȳ) / Σ(x-x̄)²`). Returns `None` when there is no spread in `x`
/// (fewer than two distinct points), which the caller reports as a zero slope.
fn least_squares_slope(points: &[(f64, f64)]) -> Option<f64> {
    let n = points.len();
    if n < 2 {
        return None;
    }
    let nf = n as f64;
    let mean_x = points.iter().map(|p| p.0).sum::<f64>() / nf;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / nf;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    for &(x, y) in points {
        let dx = x - mean_x;
        sxx += dx * dx;
        sxy += dx * (y - mean_y);
    }
    if sxx <= f64::EPSILON {
        return None;
    }
    Some(sxy / sxx)
}

/// Analyse a soak series against the given thresholds, producing the verdict.
///
/// Samples earlier than `warmup_s` are excluded from every figure so the
/// one-time startup RSS ramp does not masquerade as a leak (see
/// [`DEFAULT_WARMUP_SECS`]). The RSS slope is then fit in (hours, MB) space so
/// the raw slope is already MB/hour; handle growth is peak-over-baseline. Fewer
/// than two post-warmup samples cannot establish a trend and yield an
/// `insufficient` verdict (slope reported as 0, not a clean pass).
pub fn analyze(
    samples: &[SoakSample],
    warmup_s: f64,
    slope_threshold_mb_per_hour: f64,
    handle_growth_threshold: i64,
) -> SoakVerdict {
    // Drop the warmup window before fitting anything.
    let analyzed: Vec<&SoakSample> = samples.iter().filter(|s| s.t_s >= warmup_s).collect();

    let points: Vec<(f64, f64)> = analyzed
        .iter()
        .map(|s| (s.t_s / 3600.0, s.rss_bytes as f64 / BYTES_PER_MB))
        .collect();

    let slope = least_squares_slope(&points);
    let insufficient = slope.is_none();
    let rss_slope_mb_per_hour = slope.unwrap_or(0.0);

    let rss_first_mb = analyzed
        .first()
        .map(|s| s.rss_bytes as f64 / BYTES_PER_MB)
        .unwrap_or(0.0);
    let rss_peak_mb = analyzed
        .iter()
        .map(|s| s.rss_bytes as f64 / BYTES_PER_MB)
        .fold(0.0_f64, f64::max);

    let handles_first = analyzed.first().map(|s| s.handles).unwrap_or(0);
    let handles_peak = analyzed.iter().map(|s| s.handles).max().unwrap_or(0);
    let peak_handle_growth = (handles_peak as i64 - handles_first as i64).max(0);

    // Duration of the analyzed (post-warmup) window, and the absolute RSS rise
    // the fit predicts over exactly that window. The latter is the materiality
    // gate: a large extrapolated MB/hour that corresponds to a sub-MB rise over
    // the observed window is settling noise, not a leak.
    let window_hours = match (analyzed.first(), analyzed.last()) {
        (Some(a), Some(b)) => (b.t_s - a.t_s) / 3600.0,
        _ => 0.0,
    };
    let fitted_rise_mb = rss_slope_mb_per_hour * window_hours;

    // An insufficient window cannot fail on slope (there is no trend to judge).
    // Slope must exceed the threshold AND predict a material rise over the
    // observed window to count as a leak.
    let rss_fail = !insufficient
        && rss_slope_mb_per_hour > slope_threshold_mb_per_hour
        && fitted_rise_mb > DEFAULT_MIN_RSS_RISE_MB;
    let handle_fail = peak_handle_growth > handle_growth_threshold;
    let pass = !rss_fail && !handle_fail;

    SoakVerdict {
        samples: samples.len(),
        analyzed_samples: analyzed.len(),
        warmup_s,
        rss_slope_mb_per_hour,
        fitted_rise_mb,
        rss_first_mb,
        rss_peak_mb,
        handles_first,
        handles_peak,
        peak_handle_growth,
        slope_threshold_mb_per_hour,
        handle_growth_threshold,
        pass,
        insufficient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t_s: f64, rss_mb: f64, handles: u32) -> SoakSample {
        SoakSample {
            t_s,
            rss_bytes: (rss_mb * BYTES_PER_MB) as u64,
            handles,
        }
    }

    #[test]
    fn flat_series_passes() {
        // Constant ~50 MB, constant handles over 10 minutes.
        let series: Vec<SoakSample> = (0..=10)
            .map(|i| sample(i as f64 * 60.0, 50.0, 300))
            .collect();
        let v = analyze(
            &series,
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(v.pass, "flat series should pass: {v:?}");
        assert!(v.rss_slope_mb_per_hour.abs() < 1e-6, "slope ~0: {v:?}");
        assert_eq!(v.peak_handle_growth, 0);
        assert!(!v.insufficient);
    }

    #[test]
    fn rising_rss_fails() {
        // Climbs 10 MB over 10 minutes => 60 MB/hour, well over 5 MB/hr.
        let series: Vec<SoakSample> = (0..=10)
            .map(|i| sample(i as f64 * 60.0, 50.0 + i as f64, 300))
            .collect();
        let v = analyze(
            &series,
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(!v.pass, "rising RSS should fail: {v:?}");
        assert!(
            (v.rss_slope_mb_per_hour - 60.0).abs() < 1e-3,
            "slope should be ~60 MB/hr, got {}",
            v.rss_slope_mb_per_hour
        );
    }

    #[test]
    fn small_upward_drift_within_threshold_passes() {
        // ~2 MB/hour drift — under the 5 MB/hr threshold.
        let series: Vec<SoakSample> = (0..=60)
            .map(|i| sample(i as f64 * 60.0, 50.0 + i as f64 * (2.0 / 60.0), 300))
            .collect();
        let v = analyze(
            &series,
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(v.pass, "2 MB/hr drift should pass: {v:?}");
        assert!(v.rss_slope_mb_per_hour < DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR);
    }

    #[test]
    fn handle_leak_fails_even_with_flat_rss() {
        // RSS flat, but handles climb past the growth threshold.
        let series: Vec<SoakSample> = (0..=10)
            .map(|i| sample(i as f64 * 60.0, 50.0, 300 + i as u32 * 30))
            .collect();
        let v = analyze(
            &series,
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert_eq!(v.peak_handle_growth, 300);
        assert!(!v.pass, "handle leak should fail: {v:?}");
    }

    #[test]
    fn empty_series_is_insufficient_not_a_clean_pass() {
        let v = analyze(
            &[],
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(v.insufficient);
        assert_eq!(v.samples, 0);
        assert_eq!(v.rss_slope_mb_per_hour, 0.0);
    }

    #[test]
    fn single_sample_cannot_establish_trend() {
        let v = analyze(
            &[sample(0.0, 50.0, 300)],
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(v.insufficient);
        assert_eq!(v.rss_slope_mb_per_hour, 0.0);
    }

    #[test]
    fn warmup_ramp_is_excluded_from_the_fit() {
        // A steep startup ramp for the first 40 s, then dead flat. The warmup
        // exclusion must drop the ramp so the fit sees only the flat tail.
        let mut series: Vec<SoakSample> = Vec::new();
        for i in 0..=4 {
            // 0,10,20,30,40 s: ramp 10 -> 18 MB.
            series.push(sample(i as f64 * 10.0, 10.0 + i as f64 * 2.0, 300));
        }
        for i in 5..=17 {
            // 50..170 s: flat at 18 MB.
            series.push(sample(i as f64 * 10.0, 18.0, 300));
        }
        // With warmup exclusion only the flat tail is fit: slope ~0, PASS.
        let v = analyze(
            &series,
            DEFAULT_WARMUP_SECS,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(v.pass, "post-warmup flat tail should pass: {v:?}");
        assert!(v.rss_slope_mb_per_hour.abs() < 1e-6, "tail slope ~0: {v:?}");
        assert_eq!(v.analyzed_samples, 13, "only the flat tail is analyzed");
        assert_eq!(v.samples, 18);
        // Including the warmup inflates the slope well above the flat-tail fit.
        let raw = analyze(
            &series,
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(
            raw.rss_slope_mb_per_hour > v.rss_slope_mb_per_hour,
            "warmup ramp inflates the raw slope: raw={} filtered={}",
            raw.rss_slope_mb_per_hour,
            v.rss_slope_mb_per_hour
        );
    }

    #[test]
    fn materiality_gate_ignores_sub_mb_drift_on_short_runs() {
        // A real 3-minute run: post-warmup RSS drifts < 1 MB but that drift
        // extrapolates to > 5 MB/hour. The fit predicts < 5 MB rise over the
        // observed window, so the materiality gate keeps it a PASS (this is the
        // false positive that broke the first live soak run).
        let series: Vec<SoakSample> = (0..14)
            .map(|i| {
                let t = 45.0 + i as f64 * 10.0; // 45..175 s (post-warmup window)
                sample(t, 17.1 + i as f64 * 0.06, 135) // ~0.8 MB rise total
            })
            .collect();
        let v = analyze(
            &series,
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(
            v.rss_slope_mb_per_hour > DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            "the extrapolated slope alone exceeds the threshold: {v:?}"
        );
        assert!(
            v.fitted_rise_mb < DEFAULT_MIN_RSS_RISE_MB,
            "but the predicted rise over the window is immaterial: {v:?}"
        );
        assert!(v.pass, "short-run sub-MB drift must not fail: {v:?}");
    }

    #[test]
    fn fast_leak_fails_even_on_a_short_window() {
        // A genuine fast leak: RSS climbs 2 MB every 10 s over ~2 minutes. Both
        // the slope and the material-rise gate trip.
        let series: Vec<SoakSample> = (0..13)
            .map(|i| sample(45.0 + i as f64 * 10.0, 20.0 + i as f64 * 2.0, 300))
            .collect();
        let v = analyze(
            &series,
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(
            v.fitted_rise_mb > DEFAULT_MIN_RSS_RISE_MB,
            "material rise: {v:?}"
        );
        assert!(
            !v.pass,
            "a fast leak should fail even on a short run: {v:?}"
        );
    }

    #[test]
    fn warmup_leaving_too_few_samples_is_insufficient() {
        let series: Vec<SoakSample> = (0..=3)
            .map(|i| sample(i as f64 * 10.0, 18.0, 300))
            .collect();
        // Warmup past the whole series leaves nothing to fit.
        let v = analyze(
            &series,
            100.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(v.insufficient);
        assert_eq!(v.analyzed_samples, 0);
    }

    #[test]
    fn noisy_flat_series_passes() {
        // Sawtooth noise around a flat mean must not read as a trend.
        let series: Vec<SoakSample> = (0..=20)
            .map(|i| {
                sample(
                    i as f64 * 30.0,
                    50.0 + if i % 2 == 0 { 1.0 } else { -1.0 },
                    300,
                )
            })
            .collect();
        let v = analyze(
            &series,
            0.0,
            DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR,
            DEFAULT_HANDLE_GROWTH_THRESHOLD,
        );
        assert!(v.pass, "noisy-but-flat series should pass: {v:?}");
        assert!(v.rss_slope_mb_per_hour.abs() < DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR);
    }
}
