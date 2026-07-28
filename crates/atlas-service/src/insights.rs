//! Deterministic, evidence-backed insight generation.
//!
//! This first vertical slice interprets the live resource snapshot together
//! with recorded threshold-duration incidents. It deliberately covers a small
//! set well: CPU pressure, memory pressure, GPU-memory pressure, and confirmed
//! GPU thermal throttling. Every conclusion carries measured evidence and an
//! explicit limitation; actions remain descriptive UI destinations only.

use atlas_ipc::{
    Confidence, ContributingFactor, EvidenceItem, GpuThrottleReason, Incident, IncidentKind,
    Insight, InsightKind, InsightRecommendation, InsightStatus, Severity, SnapshotReply, TimeRange,
};

const CPU_THRESHOLD_PERMILLE: u32 = 850;
const MEMORY_THRESHOLD: f64 = 0.90;
const GPU_MEMORY_THRESHOLD: f64 = 0.90;

pub const COVERAGE_SUMMARY: &str =
    "Supported checks: CPU pressure, memory pressure, GPU memory, and GPU thermal throttling. Results depend on available telemetry.";

pub struct InsightContext<'a> {
    pub now_ms: i64,
    pub snapshot: Option<&'a SnapshotReply>,
    pub incidents: &'a [Incident],
}

pub fn generate(context: &InsightContext<'_>) -> Vec<Insight> {
    let Some(snapshot) = context.snapshot else {
        return vec![limited_insight(context.now_ms)];
    };
    let Some(system) = snapshot.system.as_ref() else {
        return vec![limited_insight(context.now_ms)];
    };

    let active_cpu = newest_active_incident(context.incidents, IncidentKind::CpuSaturation);
    let active_memory = newest_active_incident(context.incidents, IncidentKind::MemoryPressure);
    let mut insights = Vec::new();

    if let Some(incident) = active_cpu {
        insights.push(cpu_incident_insight(context.now_ms, snapshot, incident));
    } else if system.cpu_permille >= CPU_THRESHOLD_PERMILLE {
        insights.push(emerging_cpu_insight(context.now_ms, snapshot));
    }

    let memory_ratio = ratio(system.mem_used, system.mem_total);
    if let Some(incident) = active_memory {
        insights.push(memory_incident_insight(context.now_ms, snapshot, incident));
    } else if memory_ratio >= MEMORY_THRESHOLD {
        insights.push(emerging_memory_insight(
            context.now_ms,
            snapshot,
            memory_ratio,
        ));
    }

    for adapter in &snapshot.gpu_adapters {
        let memory_ratio = ratio(adapter.dedicated_used, adapter.dedicated_budget);
        if memory_ratio >= GPU_MEMORY_THRESHOLD {
            insights.push(gpu_memory_insight(context.now_ms, adapter, memory_ratio));
        }

        if adapter.thermal_throttling == Some(true)
            || has_thermal_throttle(&adapter.throttle_reasons)
        {
            insights.push(gpu_thermal_insight(context.now_ms, adapter));
        }
    }

    if insights.is_empty() {
        insights.push(clear_resource_insight(context.now_ms, snapshot));
    }

    insights.sort_by(|left, right| {
        insight_rank(right)
            .cmp(&insight_rank(left))
            .then(right.updated_ms.cmp(&left.updated_ms))
    });
    insights
}

fn newest_active_incident(incidents: &[Incident], kind: IncidentKind) -> Option<&Incident> {
    incidents
        .iter()
        .filter(|incident| incident.kind == kind as i32 && incident.end_ms == 0)
        .max_by_key(|incident| incident.start_ms)
}

fn cpu_incident_insight(now_ms: i64, snapshot: &SnapshotReply, incident: &Incident) -> Insight {
    let system_cpu = snapshot
        .system
        .as_ref()
        .map(|system| system.cpu_permille)
        .unwrap_or(0);
    let top = snapshot
        .processes
        .iter()
        .max_by_key(|process| process.cpu_permille);
    let attribution = top
        .map(|process| ratio(process.cpu_permille as u64, system_cpu as u64))
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let attributed = top.filter(|_| attribution >= 0.35);
    let title = attributed
        .map(|process| format!("{} is driving sustained CPU pressure", process.image_name))
        .unwrap_or_else(|| "CPU pressure is affecting the system".into());

    let mut evidence_items = vec![
        evidence(
            format!("System CPU is currently {:.1}%.", system_cpu as f64 / 10.0),
            now_ms,
            "sys_cpu_percent",
            system_cpu as f64 / 10.0,
        ),
        evidence(
            format!(
                "The recorded incident peaked at {:.1}%.",
                incident.peak_value
            ),
            incident.start_ms,
            "incident_peak_percent",
            incident.peak_value,
        ),
    ];
    let factors = attributed
        .map(|process| {
            evidence_items.push(evidence(
                format!(
                    "{} is currently using {:.1}% CPU.",
                    process.image_name,
                    process.cpu_permille as f64 / 10.0
                ),
                now_ms,
                "process_cpu_percent",
                process.cpu_permille as f64 / 10.0,
            ));
            vec![ContributingFactor {
                description: format!(
                    "{} accounts for about {:.0}% of current measured CPU use.",
                    process.image_name,
                    attribution * 100.0
                ),
                confidence: if attribution >= 0.60 {
                    Confidence::High as i32
                } else {
                    Confidence::Medium as i32
                },
                pid: process.pid,
                image_name: process.image_name.clone(),
                attribution,
            }]
        })
        .unwrap_or_default();

    let (recommendation, alternatives) = match attributed {
        Some(process) => (
            process_recommendation(process.pid, process.create_time_100ns, &process.image_name),
            vec!["Other processes contribute the remaining CPU use.".into()],
        ),
        None => (
            generic_investigation_recommendation("activity"),
            vec!["No single process currently explains most of the pressure.".into()],
        ),
    };

    Insight {
        fingerprint: format!("cpu-pressure:incident:{}", incident.id),
        kind: InsightKind::CpuPressure as i32,
        status: InsightStatus::Active as i32,
        severity: incident.severity.max(Severity::Warning as i32),
        confidence: Confidence::High as i32,
        title,
        observation: format!(
            "CPU saturation has remained active for {}.",
            format_duration(now_ms.saturating_sub(incident.start_ms))
        ),
        significance: "Sustained CPU pressure can delay foreground work and make the system feel unresponsive.".into(),
        range: Some(TimeRange { from_ms: incident.start_ms, to_ms: now_ms }),
        evidence: evidence_items,
        factors,
        alternatives,
        limitations: vec!["Current process attribution is a live reading; it may have changed during the incident window.".into()],
        recommendation: Some(recommendation),
        updated_ms: now_ms,
    }
}

fn emerging_cpu_insight(now_ms: i64, snapshot: &SnapshotReply) -> Insight {
    let cpu = snapshot
        .system
        .as_ref()
        .map(|system| system.cpu_permille)
        .unwrap_or(0);
    Insight {
        fingerprint: "cpu-pressure:emerging".into(),
        kind: InsightKind::CpuPressure as i32,
        status: InsightStatus::Emerging as i32,
        severity: Severity::Info as i32,
        confidence: Confidence::Medium as i32,
        title: "CPU pressure may be developing".into(),
        observation: format!("System CPU is currently {:.1}%.", cpu as f64 / 10.0),
        significance: "A brief spike may be normal; Atlas waits for sustained pressure before calling it an incident.".into(),
        range: Some(TimeRange { from_ms: now_ms, to_ms: now_ms }),
        evidence: vec![evidence(
            format!("Current system CPU is {:.1}%.", cpu as f64 / 10.0),
            now_ms,
            "sys_cpu_percent",
            cpu as f64 / 10.0,
        )],
        factors: Vec::new(),
        alternatives: vec!["A short foreground workload can produce the same reading.".into()],
        limitations: vec!["The sustained-duration threshold has not been met.".into()],
        recommendation: Some(InsightRecommendation {
            text: "Watch Live Activity to see whether the pressure persists and which process remains dominant.".into(),
            risk: "No system change is required.".into(),
            reversibility: "Observation only.".into(),
            verification_plan: "Atlas will promote this finding if the duration threshold is met.".into(),
            destination: "activity".into(),
        }),
        updated_ms: now_ms,
    }
}

fn memory_incident_insight(now_ms: i64, snapshot: &SnapshotReply, incident: &Incident) -> Insight {
    let system = snapshot
        .system
        .as_ref()
        .expect("caller requires system gauges");
    let memory_ratio = ratio(system.mem_used, system.mem_total);
    let top = snapshot
        .processes
        .iter()
        .max_by_key(|process| process.private_bytes);
    let attribution = top
        .map(|process| ratio(process.private_bytes, system.mem_used))
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let factors = top
        .filter(|_| attribution >= 0.10)
        .map(|process| {
            vec![ContributingFactor {
                description: format!(
                    "{} has the largest current private allocation at {}.",
                    process.image_name,
                    format_bytes(process.private_bytes)
                ),
                confidence: Confidence::Low as i32,
                pid: process.pid,
                image_name: process.image_name.clone(),
                attribution,
            }]
        })
        .unwrap_or_default();
    let recommendation = top
        .map(|process| {
            process_recommendation(process.pid, process.create_time_100ns, &process.image_name)
        })
        .unwrap_or_else(|| generic_investigation_recommendation("activity"));

    Insight {
        fingerprint: format!("memory-pressure:incident:{}", incident.id),
        kind: InsightKind::MemoryPressure as i32,
        status: InsightStatus::Active as i32,
        severity: incident.severity.max(Severity::Warning as i32),
        confidence: Confidence::High as i32,
        title: "Memory pressure is active".into(),
        observation: format!(
            "Physical memory use is {:.1}% and the recorded incident peaked at {:.1}%.",
            memory_ratio * 100.0,
            incident.peak_value
        ),
        significance: "Sustained memory pressure can increase paging and reduce responsiveness."
            .into(),
        range: Some(TimeRange {
            from_ms: incident.start_ms,
            to_ms: now_ms,
        }),
        evidence: vec![
            evidence(
                format!("Physical memory use is {:.1}%.", memory_ratio * 100.0),
                now_ms,
                "sys_memory_percent",
                memory_ratio * 100.0,
            ),
            evidence(
                format!(
                    "Commit is {} of {}.",
                    format_bytes(system.commit_used),
                    format_bytes(system.commit_limit)
                ),
                now_ms,
                "sys_commit_used_bytes",
                system.commit_used as f64,
            ),
        ],
        factors,
        alternatives: vec![
            "The Windows file cache and several smaller processes may also contribute.".into(),
        ],
        limitations: vec![
            "Private allocation is not a complete ownership model for physical memory.".into(),
        ],
        recommendation: Some(recommendation),
        updated_ms: now_ms,
    }
}

fn emerging_memory_insight(now_ms: i64, snapshot: &SnapshotReply, memory_ratio: f64) -> Insight {
    let system = snapshot
        .system
        .as_ref()
        .expect("caller requires system gauges");
    Insight {
        fingerprint: "memory-pressure:emerging".into(),
        kind: InsightKind::MemoryPressure as i32,
        status: InsightStatus::Emerging as i32,
        severity: Severity::Info as i32,
        confidence: Confidence::Medium as i32,
        title: "Memory pressure may be developing".into(),
        observation: format!("Physical memory use is currently {:.1}%.", memory_ratio * 100.0),
        significance: "A short peak may recover naturally; Atlas waits for sustained pressure before declaring an incident.".into(),
        range: Some(TimeRange { from_ms: now_ms, to_ms: now_ms }),
        evidence: vec![evidence(
            format!("Memory use is {} of {}.", format_bytes(system.mem_used), format_bytes(system.mem_total)),
            now_ms,
            "sys_memory_percent",
            memory_ratio * 100.0,
        )],
        factors: Vec::new(),
        alternatives: vec!["Cached memory can be reclaimed by Windows when applications need it.".into()],
        limitations: vec!["The sustained-duration threshold has not been met.".into()],
        recommendation: Some(generic_investigation_recommendation("activity")),
        updated_ms: now_ms,
    }
}

fn gpu_memory_insight(
    now_ms: i64,
    adapter: &atlas_ipc::GpuAdapterTelemetry,
    memory_ratio: f64,
) -> Insight {
    Insight {
        fingerprint: format!("gpu-memory:{}", adapter.adapter_key),
        kind: InsightKind::GpuMemoryPressure as i32,
        status: InsightStatus::Active as i32,
        severity: Severity::Warning as i32,
        confidence: Confidence::Confirmed as i32,
        title: format!("{} is near its dedicated-memory budget", adapter.name),
        observation: format!(
            "Dedicated GPU memory is {} of {} ({:.1}%).",
            format_bytes(adapter.dedicated_used),
            format_bytes(adapter.dedicated_budget),
            memory_ratio * 100.0
        ),
        significance: "Approaching the memory budget can cause allocation failures or slower shared-memory fallback.".into(),
        range: Some(TimeRange { from_ms: now_ms, to_ms: now_ms }),
        evidence: vec![evidence(
            format!("Dedicated-memory use is {:.1}%.", memory_ratio * 100.0),
            now_ms,
            "gpu_dedicated_memory_percent",
            memory_ratio * 100.0,
        )],
        factors: Vec::new(),
        alternatives: vec!["A brief allocation peak can fall without intervention.".into()],
        limitations: vec!["This finding reports adapter pressure; process ownership should be checked on the Graphics page.".into()],
        recommendation: Some(generic_investigation_recommendation("graphics")),
        updated_ms: now_ms,
    }
}

fn gpu_thermal_insight(now_ms: i64, adapter: &atlas_ipc::GpuAdapterTelemetry) -> Insight {
    let temperature = adapter
        .temperatures
        .iter()
        .map(|sample| sample.celsius)
        .chain(adapter.temperature_c)
        .reduce(f64::max);
    let temperature_text = temperature
        .map(|value| format!(" The highest available temperature is {value:.1} C."))
        .unwrap_or_default();
    Insight {
        fingerprint: format!("gpu-thermal:{}", adapter.adapter_key),
        kind: InsightKind::GpuThermalLimit as i32,
        status: InsightStatus::Active as i32,
        severity: Severity::Warning as i32,
        confidence: Confidence::Confirmed as i32,
        title: format!("{} is thermally throttling", adapter.name),
        observation: format!("The adapter reports an active thermal throttle.{temperature_text}"),
        significance: "Thermal throttling reduces clock speed to keep the hardware within its operating limits.".into(),
        range: Some(TimeRange { from_ms: now_ms, to_ms: now_ms }),
        evidence: vec![evidence(
            "The GPU provider reports thermal throttling as active.".into(),
            now_ms,
            "gpu_thermal_throttling",
            1.0,
        )],
        factors: Vec::new(),
        alternatives: Vec::new(),
        limitations: vec!["Atlas reports the provider state but cannot inspect chassis airflow or ambient temperature.".into()],
        recommendation: Some(InsightRecommendation {
            text: "Open Graphics to inspect temperature, clocks, fan speed, power, and the active throttle reasons.".into(),
            risk: "Observation only; do not change hardware limits from this insight.".into(),
            reversibility: "No system change is made.".into(),
            verification_plan: "Confirm that the throttle clears and clocks recover after temperature falls.".into(),
            destination: "graphics".into(),
        }),
        updated_ms: now_ms,
    }
}

fn clear_resource_insight(now_ms: i64, snapshot: &SnapshotReply) -> Insight {
    let system = snapshot
        .system
        .as_ref()
        .expect("caller requires system gauges");
    Insight {
        fingerprint: "resource-state:clear".into(),
        kind: InsightKind::ResourceStateClear as i32,
        status: InsightStatus::Clear as i32,
        severity: Severity::Info as i32,
        confidence: Confidence::Confirmed as i32,
        title: "CPU and memory pressure are below alert levels".into(),
        observation: format!(
            "CPU is {:.1}% and physical memory use is {:.1}%.",
            system.cpu_permille as f64 / 10.0,
            ratio(system.mem_used, system.mem_total) * 100.0
        ),
        significance: "No covered CPU or memory condition requires attention right now.".into(),
        range: Some(TimeRange {
            from_ms: now_ms,
            to_ms: now_ms,
        }),
        evidence: vec![
            evidence(
                format!("Current CPU is {:.1}%.", system.cpu_permille as f64 / 10.0),
                now_ms,
                "sys_cpu_percent",
                system.cpu_permille as f64 / 10.0,
            ),
            evidence(
                format!(
                    "Current memory use is {:.1}%.",
                    ratio(system.mem_used, system.mem_total) * 100.0
                ),
                now_ms,
                "sys_memory_percent",
                ratio(system.mem_used, system.mem_total) * 100.0,
            ),
        ],
        factors: Vec::new(),
        alternatives: Vec::new(),
        limitations: vec!["This clear state covers current CPU and memory plus active recorded CPU or memory incidents. GPU findings appear only when relevant telemetry is available. It is not a general system-health verdict.".into()],
        recommendation: Some(InsightRecommendation {
            text: "No action is recommended.".into(),
            risk: "None.".into(),
            reversibility: "No system change is made.".into(),
            verification_plan: "Atlas will replace this message if a covered condition emerges."
                .into(),
            destination: String::new(),
        }),
        updated_ms: now_ms,
    }
}

fn limited_insight(now_ms: i64) -> Insight {
    Insight {
        fingerprint: "insight-data:warming-up".into(),
        kind: InsightKind::ResourceStateClear as i32,
        status: InsightStatus::Limited as i32,
        severity: Severity::Info as i32,
        confidence: Confidence::Insufficient as i32,
        title: "Insights are waiting for live measurements".into(),
        observation: "The service has not published a complete system snapshot yet.".into(),
        significance: "Atlas will not interpret missing measurements as a healthy system.".into(),
        range: Some(TimeRange {
            from_ms: now_ms,
            to_ms: now_ms,
        }),
        evidence: Vec::new(),
        factors: Vec::new(),
        alternatives: Vec::new(),
        limitations: vec!["Live CPU and memory measurements are required.".into()],
        recommendation: Some(InsightRecommendation {
            text: "Wait for the first live sample.".into(),
            risk: "None.".into(),
            reversibility: "No system change is made.".into(),
            verification_plan: "The insight list refreshes when measurements become available."
                .into(),
            destination: String::new(),
        }),
        updated_ms: now_ms,
    }
}

fn process_recommendation(
    pid: u32,
    create_time_100ns: i64,
    image_name: &str,
) -> InsightRecommendation {
    InsightRecommendation {
        text: format!("Inspect {image_name} before deciding whether to close or adjust it."),
        risk: "Closing a process can interrupt work or discard unsaved state.".into(),
        reversibility:
            "Inspection makes no change; a normally closed application can usually be reopened."
                .into(),
        verification_plan: "After any action, watch the driving metric for two minutes.".into(),
        destination: format!("process:{pid}:{create_time_100ns}:{image_name}"),
    }
}

fn generic_investigation_recommendation(destination: &str) -> InsightRecommendation {
    InsightRecommendation {
        text: "Inspect the related measurements before taking action.".into(),
        risk: "Observation only.".into(),
        reversibility: "No system change is made.".into(),
        verification_plan: "Compare the same measurements after any user-approved action.".into(),
        destination: destination.into(),
    }
}

fn evidence(text: String, ts_ms: i64, metric: &str, value: f64) -> EvidenceItem {
    EvidenceItem {
        text,
        ts_ms,
        metric: metric.into(),
        value,
    }
}

fn has_thermal_throttle(reasons: &[i32]) -> bool {
    reasons.iter().any(|reason| {
        matches!(
            GpuThrottleReason::try_from(*reason),
            Ok(GpuThrottleReason::GpuThrottleSoftwareThermal
                | GpuThrottleReason::GpuThrottleHardwareThermal)
        )
    })
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MB", bytes as f64 / MIB)
    }
}

fn format_duration(duration_ms: i64) -> String {
    let seconds = duration_ms.max(0) / 1000;
    if seconds >= 60 {
        format!("{} minutes", seconds / 60)
    } else {
        format!("{} seconds", seconds)
    }
}

fn insight_rank(insight: &Insight) -> i32 {
    let severity = insight.severity * 100;
    let status = match InsightStatus::try_from(insight.status) {
        Ok(InsightStatus::Active) => 30,
        Ok(InsightStatus::Emerging) => 20,
        Ok(InsightStatus::Limited) => 10,
        Ok(InsightStatus::Clear) => 0,
        _ => 5,
    };
    severity + status + insight.confidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_ipc::{GpuAdapterTelemetry, ProcessRow, SystemGauges};

    fn snapshot(cpu: u32, mem_used: u64, mem_total: u64) -> SnapshotReply {
        SnapshotReply {
            system: Some(SystemGauges {
                ts_ms: 10_000,
                cpu_permille: cpu,
                mem_used,
                mem_total,
                commit_used: mem_used,
                commit_limit: mem_total,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn active_cpu_incident_names_a_dominant_process_and_cites_evidence() {
        let mut snap = snapshot(900, 4, 16);
        snap.processes.push(ProcessRow {
            pid: 42,
            create_time_100ns: 99,
            image_name: "worker.exe".into(),
            cpu_permille: 650,
            ..Default::default()
        });
        let incidents = vec![Incident {
            id: 7,
            kind: IncidentKind::CpuSaturation as i32,
            start_ms: 1_000,
            end_ms: 0,
            severity: Severity::Warning as i32,
            peak_value: 96.0,
            summary: "CPU saturation".into(),
        }];
        let got = generate(&InsightContext {
            now_ms: 10_000,
            snapshot: Some(&snap),
            incidents: &incidents,
        });

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, InsightKind::CpuPressure as i32);
        assert!(got[0].title.contains("worker.exe"));
        assert!(!got[0].evidence.is_empty());
        assert_eq!(got[0].factors[0].pid, 42);
        assert!(got[0]
            .recommendation
            .as_ref()
            .unwrap()
            .destination
            .starts_with("process:42:99"));
    }

    #[test]
    fn brief_high_cpu_is_emerging_not_an_incident() {
        let snap = snapshot(880, 4, 16);
        let got = generate(&InsightContext {
            now_ms: 10_000,
            snapshot: Some(&snap),
            incidents: &[],
        });
        assert_eq!(got[0].status, InsightStatus::Emerging as i32);
        assert!(got[0].limitations[0].contains("duration"));
    }

    #[test]
    fn gpu_memory_and_thermal_findings_are_independent() {
        let mut snap = snapshot(200, 4, 16);
        snap.gpu_adapters.push(GpuAdapterTelemetry {
            adapter_key: "gpu0".into(),
            name: "Example GPU".into(),
            dedicated_used: 9_500,
            dedicated_budget: 10_000,
            thermal_throttling: Some(true),
            ..Default::default()
        });
        let got = generate(&InsightContext {
            now_ms: 10_000,
            snapshot: Some(&snap),
            incidents: &[],
        });
        assert_eq!(got.len(), 2);
        assert!(got
            .iter()
            .any(|item| item.kind == InsightKind::GpuMemoryPressure as i32));
        assert!(got
            .iter()
            .any(|item| item.kind == InsightKind::GpuThermalLimit as i32));
    }

    #[test]
    fn clear_state_is_narrow_and_discloses_coverage() {
        let snap = snapshot(200, 4, 16);
        let got = generate(&InsightContext {
            now_ms: 10_000,
            snapshot: Some(&snap),
            incidents: &[],
        });
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].status, InsightStatus::Clear as i32);
        assert!(got[0].title.contains("CPU and memory"));
        assert!(got[0].limitations[0].contains("not a general system-health verdict"));
    }

    #[test]
    fn missing_snapshot_is_limited_not_clear() {
        let got = generate(&InsightContext {
            now_ms: 10_000,
            snapshot: None,
            incidents: &[],
        });
        assert_eq!(got[0].status, InsightStatus::Limited as i32);
        assert_eq!(got[0].confidence, Confidence::Insufficient as i32);
    }
}
