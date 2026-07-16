//! Evidence-based diagnostics engine (docs/phases.md M8, PRD §9.15).
//!
//! [`diagnose`] explains a detected incident (or an ad-hoc range) **entirely
//! from recorded data** — no LLM, no fabrication. Every field is either a
//! measured fact (evidence), a ranked correlation over the window (a
//! contributing factor with a confidence drawn from the fixed PRD ladder), or a
//! templated recommendation. It never claims proof from temporal overlap
//! (PRD §3.2 — correlation is not causation).
//!
//! ## Confidence ladder (proto `Confidence`, PRD §9.15.2) — the exact mapping
//! [`classify_confidence`] implements it verbatim:
//! - **INSUFFICIENT** — thin data: fewer than [`MIN_SAMPLES`] system samples in
//!   the window. The engine states its limit rather than guessing.
//! - **CONFIRMED** — a *recorded fact* ties the process to the episode: it has a
//!   recorded abnormal exit (non-zero code / crash) inside the window.
//! - **HIGH** — sustained high attribution: the factor holds **≥ 70 %** of the
//!   saturated resource over **≥ 80 %** of the window.
//! - **MEDIUM** — moderate attribution (**≥ 40 %**) but not sustained-high.
//! - **LOW** — temporal overlap only: the process was active in the window but
//!   its share is small. Overlap alone can never exceed LOW.

use atlas_ipc::{
    Confidence, ContributingFactor, DiagnoseReply, Diagnosis, EvidenceItem, TimeRange,
};
use atlas_store::Store;
use atlas_tsdb::{Metric, SYSTEM_SCOPE};

use crate::detectors::{
    CPU_SATURATION_PCT, KIND_CPU_SATURATION, KIND_GPU_MEMORY_EXHAUSTION, KIND_GPU_SATURATION,
    KIND_GPU_THERMAL_THROTTLING, KIND_MEMORY_PRESSURE, MEM_PRESSURE_PCT,
};

/// Attribution at/above this share, sustained over most of the window, reaches
/// HIGH confidence.
const HIGH_ATTRIBUTION: f64 = 0.70;
/// The window-coverage fraction required alongside [`HIGH_ATTRIBUTION`] for HIGH.
const HIGH_COVERAGE: f64 = 0.80;
/// Attribution at/above this share (but not sustained-high) reaches MEDIUM.
const MEDIUM_ATTRIBUTION: f64 = 0.40;
/// Fewer than this many system samples in the window is "thin data".
pub const MIN_SAMPLES: usize = 3;
/// When the top two factors' attribution differs by less than this, the second
/// is surfaced as an alternative (the evidence does not cleanly separate them).
const ALTERNATIVE_MARGIN: f64 = 0.15;
/// How many contributing factors to rank and emit at most.
const MAX_FACTORS: usize = 3;

/// The resolved subject of a diagnosis: an incident's kind + window + peak, or a
/// range with an inferred kind. `end_ms == 0` means open/ongoing (use "now").
#[derive(Debug, Clone)]
pub struct DiagnoseContext {
    pub kind: i32,
    pub start_ms: i64,
    pub end_ms: i64,
    pub peak_value: f64,
}

/// Maps an attribution share + window coverage + sample count + recorded-crash
/// flag to a [`Confidence`], implementing the PRD ladder documented above.
pub fn classify_confidence(
    attribution: f64,
    coverage: f64,
    sample_count: usize,
    recorded_crash: bool,
) -> Confidence {
    if sample_count < MIN_SAMPLES {
        Confidence::Insufficient
    } else if recorded_crash {
        Confidence::Confirmed
    } else if attribution >= HIGH_ATTRIBUTION && coverage >= HIGH_COVERAGE {
        Confidence::High
    } else if attribution >= MEDIUM_ATTRIBUTION {
        Confidence::Medium
    } else {
        Confidence::Low
    }
}

/// Builds a diagnosis for `ctx` from recorded data. `now_ms` closes an open
/// window; `mem_total` (bytes, 0 if unknown) turns recorded memory bytes into a
/// percent for evidence and for inferring an ad-hoc range's kind. Returns
/// `available = false` with a reason when the window has too little data
/// (PRD §9.16.4 — the engine states its limits).
pub fn diagnose(
    store: &Store,
    ctx: &DiagnoseContext,
    now_ms: i64,
    mem_total: u64,
) -> anyhow::Result<DiagnoseReply> {
    let from = ctx.start_ms;
    let to = if ctx.end_ms == 0 {
        now_ms.max(ctx.start_ms)
    } else {
        ctx.end_ms
    };

    // System CPU series (percent) drives sample-count / peak / coverage.
    let sys_cpu = read_sys(store, Metric::SysCpuPermille, from, to, 0.1)?;
    let sys_mem_bytes = read_sys(store, Metric::SysMemUsed, from, to, 1.0)?;
    let sys_gpu = read_sys(store, Metric::SysGpuPermille, from, to, 0.1)?;
    let sys_gpu_mem = read_sys(store, Metric::SysGpuMemoryUsed, from, to, 1.0)?;
    let sample_count = sys_cpu.len().max(sys_mem_bytes.len()).max(sys_gpu.len());

    if sample_count == 0 {
        return Ok(unavailable(
            "no recorded samples in this window — monitoring was off or the window is outside retention",
        ));
    }
    if sample_count < MIN_SAMPLES {
        return Ok(unavailable(
            "insufficient evidence — too few samples in this window to diagnose",
        ));
    }

    // Resolve the resource kind (incident kinds are explicit; an ad-hoc range
    // infers from whichever resource crossed its threshold, defaulting to CPU).
    let kind = if matches!(
        ctx.kind,
        KIND_CPU_SATURATION
            | KIND_MEMORY_PRESSURE
            | KIND_GPU_SATURATION
            | KIND_GPU_MEMORY_EXHAUSTION
            | KIND_GPU_THERMAL_THROTTLING
    ) {
        ctx.kind
    } else {
        infer_kind(&sys_cpu, &sys_mem_bytes, mem_total)
    };

    let mut evidence: Vec<EvidenceItem> = Vec::new();

    // Peak system CPU is always a relevant fact.
    if let Some((ts, v)) = peak_point(&sys_cpu) {
        evidence.push(EvidenceItem {
            text: format!("Peak system CPU {v:.0}% during the window"),
            ts_ms: ts,
            metric: "sys_cpu_pct".into(),
            value: v,
        });
    }
    // Peak memory: percent when total is known, else raw bytes.
    if let Some((ts, bytes)) = peak_point(&sys_mem_bytes) {
        if mem_total > 0 {
            let pct = bytes / mem_total as f64 * 100.0;
            evidence.push(EvidenceItem {
                text: format!("Peak memory in use {pct:.0}% of total"),
                ts_ms: ts,
                metric: "sys_mem_pct".into(),
                value: pct,
            });
        } else {
            evidence.push(EvidenceItem {
                text: format!("Peak memory in use {:.1} GB", bytes / (1u64 << 30) as f64),
                ts_ms: ts,
                metric: "sys_mem_bytes".into(),
                value: bytes,
            });
        }
    }
    if matches!(
        kind,
        KIND_GPU_SATURATION | KIND_GPU_MEMORY_EXHAUSTION | KIND_GPU_THERMAL_THROTTLING
    ) {
        if let Some((ts, v)) = peak_point(&sys_gpu) {
            evidence.push(EvidenceItem {
                text: format!("Peak GPU activity {v:.0}% during the window"),
                ts_ms: ts,
                metric: "sys_gpu_pct".into(),
                value: v,
            });
        }
        if let Some((ts, bytes)) = peak_point(&sys_gpu_mem) {
            evidence.push(EvidenceItem {
                text: format!(
                    "Peak measured graphics memory {:.1} GB",
                    bytes / (1u64 << 30) as f64
                ),
                ts_ms: ts,
                metric: "sys_gpu_mem_bytes".into(),
                value: bytes,
            });
        }
    }

    // Recorded process lifecycle facts in the window (starts / exits; a non-zero
    // exit is a recorded crash — the CONFIRMED path). Fetch once, index by pid.
    let (events, _) = store.list_events(from, to, &[], 500)?;
    let mut crashed_pids = std::collections::HashSet::new();
    for e in &events {
        // kind 1 = stop; a recorded non-zero exit is an abnormal termination.
        if e.kind == 1 && e.has_exit_status && e.exit_status != 0 {
            crashed_pids.insert(e.pid);
            evidence.push(EvidenceItem {
                text: format!(
                    "{} (pid {}) exited with code {} at this time",
                    display_name(&e.image_name),
                    e.pid,
                    e.exit_status
                ),
                ts_ms: e.ts_ms,
                metric: "proc_exit_code".into(),
                value: e.exit_status as f64,
            });
        }
    }

    // Rank contributing processes over the window.
    let sys_cpu_avg_permille = mean(&sys_cpu) * 10.0; // percent -> permille
    let mem_used_peak = peak_point(&sys_mem_bytes).map(|(_, b)| b).unwrap_or(0.0);
    let tops = store.top_processes(from, to, 32)?;
    let sys_sample_count = sample_count as f64;

    struct Ranked {
        factor: ContributingFactor,
        attribution: f64,
    }
    let mut ranked: Vec<Ranked> = Vec::new();
    for t in &tops {
        if matches!(
            kind,
            KIND_GPU_SATURATION | KIND_GPU_MEMORY_EXHAUSTION | KIND_GPU_THERMAL_THROTTLING
        ) {
            continue; // no CPU/memory attribution is presented as GPU attribution
        }
        let (attribution, share_text) = if kind == KIND_MEMORY_PRESSURE {
            let denom = if mem_used_peak > 0.0 {
                mem_used_peak
            } else {
                mem_total.max(1) as f64
            };
            let a = (t.working_set_peak as f64 / denom).clamp(0.0, 1.0);
            (
                a,
                format!(
                    "held up to {:.1} GB working set (~{:.0}% of memory in use)",
                    t.working_set_peak as f64 / (1u64 << 30) as f64,
                    a * 100.0
                ),
            )
        } else {
            let a = if sys_cpu_avg_permille > 0.0 {
                (t.cpu_avg_permille / sys_cpu_avg_permille).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (
                a,
                format!(
                    "averaged {:.0}% CPU (peak {:.0}%), ~{:.0}% of active CPU",
                    t.cpu_avg_permille / 10.0,
                    t.cpu_peak_permille as f64 / 10.0,
                    a * 100.0
                ),
            )
        };

        // Coverage: how much of the window this process was actually sampled in.
        let coverage = if sys_sample_count > 0.0 {
            (t.windows as f64 / sys_sample_count).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let recorded_crash = crashed_pids.contains(&t.pid);
        let confidence = classify_confidence(attribution, coverage, sample_count, recorded_crash);

        // Drop clearly-irrelevant processes (tiny attribution and no crash) so
        // the factor list stays about the incident, not the whole machine.
        if attribution < 0.05 && !recorded_crash {
            continue;
        }
        ranked.push(Ranked {
            factor: ContributingFactor {
                description: format!(
                    "{} (pid {}) {}",
                    display_name(&t.image_name),
                    t.pid,
                    share_text
                ),
                confidence: confidence as i32,
                pid: t.pid,
                image_name: t.image_name.clone(),
                attribution,
            },
            attribution,
        });
    }
    // top_processes already sorts by the driving metric for CPU; re-sort by our
    // attribution so the memory case is ordered too, and cap the list.
    ranked.sort_by(|a, b| {
        b.attribution
            .partial_cmp(&a.attribution)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(MAX_FACTORS);

    // Alternatives: when the top two are within the margin, the evidence does
    // not cleanly separate them — surface the runner-up as a competing cause.
    let mut alternatives = Vec::new();
    if ranked.len() >= 2 && (ranked[0].attribution - ranked[1].attribution) < ALTERNATIVE_MARGIN {
        alternatives.push(format!(
            "Could also be {} — its share ({:.0}%) is close to the top factor's ({:.0}%).",
            display_name(&ranked[1].factor.image_name),
            ranked[1].attribution * 100.0,
            ranked[0].attribution * 100.0,
        ));
    }

    let factors: Vec<ContributingFactor> = ranked.into_iter().map(|r| r.factor).collect();

    // Overall confidence tracks the top factor; with no clear factor it is LOW
    // (we saw the pressure but cannot attribute it), never fabricated higher.
    let overall = factors
        .first()
        .map(|f| f.confidence)
        .unwrap_or(Confidence::Low as i32);

    let top_name = factors.first().map(|f| display_name(&f.image_name));
    let top_pid = factors.first().map(|f| f.pid).unwrap_or(0);
    let (recommendation, risk, reversibility, verification_plan) =
        templates(kind, top_name.as_deref(), top_pid);

    let observed = observed_text(kind, ctx.peak_value, from, to);

    let diagnosis = Diagnosis {
        observed,
        range: Some(TimeRange {
            from_ms: from,
            to_ms: to,
        }),
        evidence,
        factors,
        overall_confidence: overall,
        alternatives,
        recommendation,
        risk,
        reversibility,
        verification_plan,
    };

    Ok(DiagnoseReply {
        available: true,
        unavailable_reason: String::new(),
        diagnosis: Some(diagnosis),
    })
}

fn unavailable(reason: &str) -> DiagnoseReply {
    DiagnoseReply {
        available: false,
        unavailable_reason: reason.to_string(),
        diagnosis: None,
    }
}

/// Reads a system-scope metric over the window as `(ts, value*scale)`, sorted.
fn read_sys(
    store: &Store,
    metric: Metric,
    from: i64,
    to: i64,
    scale: f64,
) -> anyhow::Result<Vec<(i64, f64)>> {
    let mut pts = Vec::new();
    for blk in store.read_blocks(metric, Some(SYSTEM_SCOPE), from, to)? {
        for (ts, v) in blk.points {
            if ts >= from && ts <= to {
                pts.push((ts, v * scale));
            }
        }
    }
    pts.sort_by_key(|&(ts, _)| ts);
    Ok(pts)
}

fn peak_point(series: &[(i64, f64)]) -> Option<(i64, f64)> {
    series
        .iter()
        .copied()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
}

fn mean(series: &[(i64, f64)]) -> f64 {
    if series.is_empty() {
        return 0.0;
    }
    series.iter().map(|&(_, v)| v).sum::<f64>() / series.len() as f64
}

/// Picks CPU vs memory for an ad-hoc range by which resource crossed its
/// threshold by the wider margin; defaults to CPU.
fn infer_kind(sys_cpu: &[(i64, f64)], sys_mem_bytes: &[(i64, f64)], mem_total: u64) -> i32 {
    let cpu_peak = peak_point(sys_cpu).map(|(_, v)| v).unwrap_or(0.0);
    let cpu_over = cpu_peak - CPU_SATURATION_PCT;
    let mem_over = if mem_total > 0 {
        let mem_peak = peak_point(sys_mem_bytes)
            .map(|(_, b)| b / mem_total as f64 * 100.0)
            .unwrap_or(0.0);
        mem_peak - MEM_PRESSURE_PCT
    } else {
        f64::NEG_INFINITY
    };
    if mem_over > cpu_over && mem_over > 0.0 {
        KIND_MEMORY_PRESSURE
    } else {
        KIND_CPU_SATURATION
    }
}

/// Strips a trailing NT device path so evidence reads with the image name.
fn display_name(image: &str) -> String {
    if image.is_empty() {
        return "an unknown process".to_string();
    }
    image
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(image)
        .to_string()
}

fn observed_text(kind: i32, peak_value: f64, from: i64, to: i64) -> String {
    let dur_s = ((to - from).max(0)) as f64 / 1000.0;
    let resource = match kind {
        KIND_MEMORY_PRESSURE => "Memory",
        KIND_GPU_MEMORY_EXHAUSTION => "GPU memory",
        KIND_GPU_THERMAL_THROTTLING => "GPU thermal state",
        KIND_GPU_SATURATION => "GPU",
        _ => "CPU",
    };
    let peak = if peak_value > 0.0 {
        format!(" (peak {peak_value:.0}%)")
    } else {
        String::new()
    };
    format!(
        "{resource} was under sustained pressure for about {dur_s:.0}s{peak}. \
         The factors below are correlations over the same window, not proven causes."
    )
}

/// Per-kind templated recommendation / risk / reversibility / verification plan.
/// Hedged per PRD §3.2: it suggests an action to try and how to check it, never
/// asserts the process is the cause.
fn templates(kind: i32, top_name: Option<&str>, top_pid: u32) -> (String, String, String, String) {
    let who = match top_name {
        Some(name) => format!("{name} (pid {top_pid})"),
        None => "the top consumer".to_string(),
    };
    if matches!(
        kind,
        KIND_GPU_SATURATION | KIND_GPU_MEMORY_EXHAUSTION | KIND_GPU_THERMAL_THROTTLING
    ) {
        (
            "Inspect the GPU page and the processes active in this window. If a workload was unexpected, close it only after saving work, then watch whether the measured GPU pressure clears.".to_string(),
            "Closing a graphics workload can lose unsaved work. The timing is correlated evidence, not proof that one process caused the incident.".to_string(),
            "Reversible - the application can be relaunched afterwards.".to_string(),
            "Verify that GPU activity, graphics-memory pressure, or the hardware throttle state returns below the incident condition.".to_string(),
        )
    } else if kind == KIND_MEMORY_PRESSURE {
        (
            format!(
                "The largest memory holder in this window was {who}. If that memory use isn't \
                 expected, try closing or restarting {}, then watch whether memory pressure eases.",
                top_name.unwrap_or("it")
            ),
            "Closing or restarting an application can lose unsaved work in it. Correlation is not \
             proof — confirm the app is the real driver before relying on this."
                .to_string(),
            "Reversible — you can relaunch the application afterwards.".to_string(),
            format!(
                "After acting, watch memory in use over the next few minutes; a resolved pressure \
                 falls back below {:.0}% of total.",
                MEM_PRESSURE_PCT
            ),
        )
    } else {
        (
            format!(
                "The largest CPU consumer in this window was {who}. If that work isn't expected \
                 right now, try closing or restarting {}, then watch whether CPU settles.",
                top_name.unwrap_or("it")
            ),
            "Closing or restarting an application can lose unsaved work in it. Correlation is not \
             proof — confirm the app is the real driver before relying on this."
                .to_string(),
            "Reversible — you can relaunch the application afterwards.".to_string(),
            format!(
                "After acting, watch system CPU over the next few minutes; a resolved saturation \
                 drops back below {:.0}%.",
                CPU_SATURATION_PCT
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_insufficient_when_thin_data() {
        // Even with dominant attribution, too few samples => INSUFFICIENT.
        assert_eq!(
            classify_confidence(0.99, 1.0, MIN_SAMPLES - 1, false),
            Confidence::Insufficient
        );
    }

    #[test]
    fn ladder_confirmed_on_recorded_crash() {
        // A recorded crash outranks attribution-based bands.
        assert_eq!(
            classify_confidence(0.10, 0.10, 100, true),
            Confidence::Confirmed
        );
    }

    #[test]
    fn ladder_high_needs_sustained_and_dominant() {
        assert_eq!(classify_confidence(0.72, 0.85, 50, false), Confidence::High);
        // Dominant but not sustained over the window -> not HIGH.
        assert_eq!(
            classify_confidence(0.72, 0.50, 50, false),
            Confidence::Medium
        );
    }

    #[test]
    fn ladder_medium_and_low_bands() {
        assert_eq!(
            classify_confidence(0.45, 0.99, 50, false),
            Confidence::Medium
        );
        // Temporal overlap only (small share) caps at LOW.
        assert_eq!(classify_confidence(0.10, 0.99, 50, false), Confidence::Low);
    }

    #[test]
    fn insufficient_path_when_window_empty() {
        let store = Store::open_in_memory().unwrap();
        let ctx = DiagnoseContext {
            kind: KIND_CPU_SATURATION,
            start_ms: 0,
            end_ms: 10_000,
            peak_value: 0.0,
        };
        let reply = diagnose(&store, &ctx, 10_000, 0).unwrap();
        assert!(!reply.available);
        assert!(reply.diagnosis.is_none());
        assert!(!reply.unavailable_reason.is_empty());
    }

    #[test]
    fn display_name_strips_device_path() {
        assert_eq!(
            display_name(r"\Device\HarddiskVolume4\Windows\notepad.exe"),
            "notepad.exe"
        );
        assert_eq!(display_name("chrome.exe"), "chrome.exe");
        assert_eq!(display_name(""), "an unknown process");
    }
}
