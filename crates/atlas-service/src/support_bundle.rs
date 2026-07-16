//! Remote support bundle assembly + redaction + formatting (docs/phases.md R3,
//! PRD §9.18/§18.3).
//!
//! A single, self-contained diagnostic document a user can hand to IT/support,
//! assembled from data Atlas already has: device info, current health, recent
//! incidents + their diagnoses, system changes, correlated crashes, the
//! service/startup inventories, and Atlas's own overhead.
//!
//! **Redaction is the whole point** (§9.18: "enough evidence for support
//! without exposing unrelated personal activity"). Every textual field of every
//! section runs through the shared [`Redactor`] (the same one M8's report export
//! uses) *before* any formatter runs — so a bundle can never leak more than a
//! single report would, and all three formats redact identically. The
//! incidents section reuses `report::redact` verbatim so a diagnosis inside a
//! bundle is redacted exactly like a standalone report.
//!
//! No new dependencies: JSON via the already-present `serde_json`; HTML/TEXT are
//! hand-rendered with explicit escaping (mirroring `report.rs`).

use atlas_ipc::{
    CrashRecord, Diagnosis, Incident, RedactionOptions, ReportFormat, ServiceEntry, StartupEntry,
    SupportBundleReply, SupportBundleSection, SystemChange,
};

use crate::report::{self, Redactor};

// ---------------------------------------------------------------------------
// Section selection (pure): map the request's repeated SupportBundleSection to
// a set of booleans. An empty request list means "all sections".
// ---------------------------------------------------------------------------

/// Which sections a bundle should contain. Assembly reads this to decide what to
/// gather; an empty request selects everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionSet {
    pub device: bool,
    pub health: bool,
    pub incidents: bool,
    pub changes: bool,
    pub crashes: bool,
    pub services: bool,
    pub startup: bool,
    pub self_metrics: bool,
}

impl SectionSet {
    /// Every section on.
    pub fn all() -> Self {
        Self {
            device: true,
            health: true,
            incidents: true,
            changes: true,
            crashes: true,
            services: true,
            startup: true,
            self_metrics: true,
        }
    }

    /// Every section off.
    fn none() -> Self {
        Self {
            device: false,
            health: false,
            incidents: false,
            changes: false,
            crashes: false,
            services: false,
            startup: false,
            self_metrics: false,
        }
    }
}

/// Resolves the requested sections. An empty list selects all (per the proto:
/// "empty = all"); an unknown/UNSPECIFIED discriminant is ignored.
pub fn selected(sections: &[i32]) -> SectionSet {
    if sections.is_empty() {
        return SectionSet::all();
    }
    let mut s = SectionSet::none();
    for &disc in sections {
        match SupportBundleSection::try_from(disc) {
            Ok(SupportBundleSection::BundleDeviceInfo) => s.device = true,
            Ok(SupportBundleSection::BundleHealth) => s.health = true,
            Ok(SupportBundleSection::BundleIncidents) => s.incidents = true,
            Ok(SupportBundleSection::BundleSystemChanges) => s.changes = true,
            Ok(SupportBundleSection::BundleCrashes) => s.crashes = true,
            Ok(SupportBundleSection::BundleServices) => s.services = true,
            Ok(SupportBundleSection::BundleStartup) => s.startup = true,
            Ok(SupportBundleSection::BundleSelfMetrics) => s.self_metrics = true,
            _ => {} // UNSPECIFIED / unknown: ignore
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Data model: the assembled (pre-redaction) bundle. Only the requested sections
// are `Some`. The IPC layer fills this from the store + collectors; formatting
// and redaction here are pure and unit-tested.
// ---------------------------------------------------------------------------

/// Device facts (PRD §9.18 device section). `hostname` is the only redactable
/// field; the rest are numbers or the Atlas version.
#[derive(Debug, Clone, Default)]
pub struct DeviceSection {
    pub os_major: u32,
    pub os_minor: u32,
    pub os_build: u32,
    pub hostname: String,
    pub logical_cpus: u32,
    pub p_core_count: u32,
    pub e_core_count: u32,
    pub heterogeneous: bool,
    pub ram_total_bytes: u64,
    pub atlas_version: String,
    pub uptime_ms: u64,
}

/// One top consumer in the health section.
#[derive(Debug, Clone, Default)]
pub struct ConsumerRow {
    pub pid: u32,
    pub image_name: String,
    pub cpu_permille: u32,
    pub working_set: u64,
    pub private_bytes: u64,
}

/// Current health: a fresh gauge reading plus the top-N consumers.
#[derive(Debug, Clone, Default)]
pub struct HealthSection {
    pub ts_ms: i64,
    pub cpu_permille: u32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub commit_used: u64,
    pub commit_limit: u64,
    pub process_count: u32,
    pub thread_count: u32,
    pub handle_count: u32,
    pub gpu_permille: u32,
    pub gpu_dedicated_used: u64,
    pub gpu_dedicated_budget: u64,
    pub gpu_shared_used: u64,
    pub gpu_shared_budget: u64,
    pub gpu_details: Vec<String>,
    pub top: Vec<ConsumerRow>,
}

/// One incident and its diagnosis (or the honest reason a diagnosis is
/// unavailable). Mirrors the report export's incident+diagnosis pairing.
#[derive(Debug, Clone)]
pub struct IncidentEntry {
    pub incident: Incident,
    pub diagnosis: Option<Diagnosis>,
    pub unavailable_reason: String,
}

/// The crashes section carries the scanner's availability so an unreadable
/// reliability log degrades honestly rather than looking empty.
#[derive(Debug, Clone, Default)]
pub struct CrashesSection {
    pub available: bool,
    pub unavailable_reason: String,
    pub crashes: Vec<CrashRecord>,
}

/// Atlas's own overhead over the last flush window (PRD §12.2 — the product
/// shows its own cost).
#[derive(Debug, Clone, Default)]
pub struct SelfMetricsSection {
    pub ts_ms: i64,
    pub cpu_permille: u32,
    pub working_set: u64,
    pub tick_duration_us_avg: u64,
    pub tick_duration_us_max: u64,
    pub ticks: u32,
}

/// The fully-assembled (pre-redaction) bundle. Only requested sections are set.
#[derive(Debug, Clone, Default)]
pub struct BundleData {
    pub range_from_ms: i64,
    pub range_to_ms: i64,
    pub device: Option<DeviceSection>,
    pub health: Option<HealthSection>,
    pub incidents: Option<Vec<IncidentEntry>>,
    pub changes: Option<Vec<SystemChange>>,
    pub crashes: Option<CrashesSection>,
    pub services: Option<Vec<ServiceEntry>>,
    pub startup: Option<Vec<StartupEntry>>,
    pub self_metrics: Option<SelfMetricsSection>,
}

// ---------------------------------------------------------------------------
// Redaction: run every textual field of every present section through the
// shared Redactor, returning a redacted copy the formatters consume.
// ---------------------------------------------------------------------------

/// The redaction category labels for the toggles that are on. Populates the
/// reply's `redaction_applied` — an honest echo of what was stripped.
pub fn applied_categories(opts: &RedactionOptions) -> Vec<String> {
    let mut v = Vec::new();
    if opts.redact_paths {
        v.push("paths".to_string());
    }
    if opts.redact_user_names {
        v.push("user_names".to_string());
    }
    if opts.redact_computer_name {
        v.push("computer_name".to_string());
    }
    if opts.redact_command_lines {
        v.push("command_lines".to_string());
    }
    v
}

/// Returns a copy of `data` with every textual field passed through `r`. This is
/// the single redaction pass the formatters rely on (PRD §9.18: redact before
/// formatting, so every format is redacted identically).
pub fn redact_data(data: &BundleData, r: &Redactor) -> BundleData {
    let ap = |s: &str| r.apply(s);
    BundleData {
        range_from_ms: data.range_from_ms,
        range_to_ms: data.range_to_ms,
        device: data.device.as_ref().map(|d| DeviceSection {
            hostname: ap(&d.hostname),
            ..d.clone()
        }),
        health: data.health.as_ref().map(|h| HealthSection {
            top: h
                .top
                .iter()
                .map(|c| ConsumerRow {
                    image_name: ap(&c.image_name),
                    ..c.clone()
                })
                .collect(),
            gpu_details: h.gpu_details.iter().map(|line| ap(line)).collect(),
            ..h.clone()
        }),
        incidents: data.incidents.as_ref().map(|list| {
            list.iter()
                .map(|e| match &e.diagnosis {
                    // Reuse the report export's exact redaction for a diagnosis.
                    Some(diag) => {
                        let (inc, diag) = report::redact(&e.incident, diag, r);
                        IncidentEntry {
                            incident: inc,
                            diagnosis: Some(diag),
                            unavailable_reason: ap(&e.unavailable_reason),
                        }
                    }
                    None => IncidentEntry {
                        incident: Incident {
                            summary: ap(&e.incident.summary),
                            ..e.incident.clone()
                        },
                        diagnosis: None,
                        unavailable_reason: ap(&e.unavailable_reason),
                    },
                })
                .collect()
        }),
        changes: data.changes.as_ref().map(|list| {
            list.iter()
                .map(|c| SystemChange {
                    subject: ap(&c.subject),
                    detail: ap(&c.detail),
                    publisher: ap(&c.publisher),
                    responsible: ap(&c.responsible),
                    ..c.clone()
                })
                .collect()
        }),
        crashes: data.crashes.as_ref().map(|c| CrashesSection {
            available: c.available,
            unavailable_reason: ap(&c.unavailable_reason),
            crashes: c
                .crashes
                .iter()
                .map(|cr| CrashRecord {
                    subject: ap(&cr.subject),
                    fault: ap(&cr.fault),
                    context: cr.context.iter().map(|l| ap(l)).collect(),
                    ..cr.clone()
                })
                .collect(),
        }),
        services: data.services.as_ref().map(|list| {
            list.iter()
                .map(|s| ServiceEntry {
                    display_name: ap(&s.display_name),
                    description: ap(&s.description),
                    account: ap(&s.account),
                    binary_path: ap(&s.binary_path),
                    ..s.clone()
                })
                .collect()
        }),
        startup: data.startup.as_ref().map(|list| {
            list.iter()
                .map(|e| StartupEntry {
                    name: ap(&e.name),
                    command: ap(&e.command),
                    publisher: ap(&e.publisher),
                    ..e.clone()
                })
                .collect()
        }),
        self_metrics: data.self_metrics.clone(),
    }
}

// ---------------------------------------------------------------------------
// Top-level entry: redact, format, and package into a SupportBundleReply.
// ---------------------------------------------------------------------------

/// Redacts `data`, renders it in `format`, and returns the reply with a suggested
/// filename (`atlas-support-<date-from-range-end>.<ext>`) and the applied
/// redaction categories. The redactor's user/host needles come from the
/// environment, exactly like `report::render_report`.
pub fn build_bundle(
    data: BundleData,
    format: ReportFormat,
    redaction: &RedactionOptions,
) -> SupportBundleReply {
    let r = Redactor::from_env(*redaction);
    let redacted = redact_data(&data, &r);
    let (content, content_type, ext) = render(&redacted, format);
    SupportBundleReply {
        content,
        content_type,
        filename: format!("atlas-support-{}.{}", date_string(data.range_to_ms), ext),
        redaction_applied: applied_categories(redaction),
    }
}

/// Renders the already-redacted bundle in `format`, returning
/// `(content, content_type, extension)`. HTML/JSON/TEXT are the supported
/// formats; CSV degrades to TEXT (a multi-section bundle has no single table).
fn render(data: &BundleData, format: ReportFormat) -> (String, String, &'static str) {
    match format {
        ReportFormat::ReportHtml => (render_html(data), "text/html".to_string(), "html"),
        ReportFormat::ReportJson => (render_json(data), "application/json".to_string(), "json"),
        _ => (render_text(data), "text/plain".to_string(), "txt"),
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers.
// ---------------------------------------------------------------------------

/// Bytes as a human string (GiB when ≥1 GiB, else MiB).
fn human_bytes(b: u64) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    const MIB: f64 = (1u64 << 20) as f64;
    let f = b as f64;
    if f >= GIB {
        format!("{:.1} GiB", f / GIB)
    } else {
        format!("{:.0} MiB", f / MIB)
    }
}

/// Permille (0..=1000) as a percentage string.
fn pct(permille: u32) -> String {
    format!("{:.1}%", permille as f64 / 10.0)
}

/// Milliseconds of uptime as `Nd HHh MMm`.
fn human_uptime(ms: u64) -> String {
    let secs = ms / 1000;
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else {
        format!("{hours}h {mins}m")
    }
}

/// A one-line memory/CPU pressure summary from the health gauges (factual, no
/// alarm language — a change is information).
fn pressure_summary(h: &HealthSection) -> String {
    let mem_pct = if h.mem_total > 0 {
        h.mem_used as f64 / h.mem_total as f64 * 100.0
    } else {
        0.0
    };
    let commit_pct = if h.commit_limit > 0 {
        h.commit_used as f64 / h.commit_limit as f64 * 100.0
    } else {
        0.0
    };
    let mut notes = Vec::new();
    if h.cpu_permille >= 850 {
        notes.push("CPU under sustained load".to_string());
    }
    if mem_pct >= 90.0 {
        notes.push("physical memory nearly full".to_string());
    }
    if commit_pct >= 90.0 {
        notes.push("commit charge nearly at the limit".to_string());
    }
    if h.gpu_permille >= 850 {
        notes.push("GPU under sustained load".to_string());
    }
    let head = format!(
        "CPU {}, GPU {}, memory {:.0}%, commit {:.0}%",
        pct(h.cpu_permille),
        pct(h.gpu_permille),
        mem_pct,
        commit_pct
    );
    if notes.is_empty() {
        format!("{head} — no sustained pressure")
    } else {
        format!("{head} — {}", notes.join("; "))
    }
}

/// OS version as `major.minor build NNNNN`.
fn os_string(d: &DeviceSection) -> String {
    format!("Windows {}.{} build {}", d.os_major, d.os_minor, d.os_build)
}

// ---------------------------------------------------------------------------
// Filename date derivation: civil date from epoch milliseconds, no chrono.
// ---------------------------------------------------------------------------

/// `YYYY-MM-DD` (UTC) for the epoch-millisecond timestamp. Used for the
/// suggested filename; derived from the range end so tests need no wall clock.
fn date_string(ms: i64) -> String {
    let (y, m, d) = ymd_from_ms(ms);
    format!("{y:04}-{m:02}-{d:02}")
}

fn ymd_from_ms(ms: i64) -> (i64, u32, u32) {
    civil_from_days(ms.div_euclid(86_400_000))
}

/// Howard Hinnant's `civil_from_days`: days-since-Unix-epoch → (year, month,
/// day). Proleptic Gregorian, exact for the whole i64 range.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d)
}

// ---------------------------------------------------------------------------
// TEXT rendering.
// ---------------------------------------------------------------------------

fn render_text(data: &BundleData) -> String {
    let mut s = String::new();
    s.push_str("================ Atlas support bundle ================\n");
    s.push_str(&format!(
        "Window: {} .. {}\n\n",
        data.range_from_ms, data.range_to_ms
    ));

    if let Some(d) = &data.device {
        s.push_str("---- Device ----\n");
        s.push_str(&format!("OS: {}\n", os_string(d)));
        s.push_str(&format!("Host: {}\n", d.hostname));
        s.push_str(&format!(
            "CPU: {} logical{}\n",
            d.logical_cpus,
            if d.heterogeneous {
                format!(" ({} P / {} E cores)", d.p_core_count, d.e_core_count)
            } else {
                String::new()
            }
        ));
        s.push_str(&format!("RAM: {}\n", human_bytes(d.ram_total_bytes)));
        s.push_str(&format!("Uptime: {}\n", human_uptime(d.uptime_ms)));
        s.push_str(&format!("Atlas version: {}\n\n", d.atlas_version));
    }

    if let Some(h) = &data.health {
        s.push_str("---- Health ----\n");
        s.push_str(&format!("{}\n", pressure_summary(h)));
        s.push_str(&format!(
            "Processes {}, threads {}, handles {}\n",
            h.process_count, h.thread_count, h.handle_count
        ));
        s.push_str(&format!(
            "GPU memory dedicated {}/{}, shared {}/{}\n",
            human_bytes(h.gpu_dedicated_used),
            human_bytes(h.gpu_dedicated_budget),
            human_bytes(h.gpu_shared_used),
            human_bytes(h.gpu_shared_budget),
        ));
        for detail in &h.gpu_details {
            s.push_str(&format!("GPU: {detail}\n"));
        }
        s.push_str("Top consumers:\n");
        if h.top.is_empty() {
            s.push_str("  (none)\n");
        }
        for c in &h.top {
            s.push_str(&format!(
                "  {:>6}  {:<28} CPU {:>6}  WS {}\n",
                c.pid,
                c.image_name,
                pct(c.cpu_permille),
                human_bytes(c.working_set)
            ));
        }
        s.push('\n');
    }

    if let Some(list) = &data.incidents {
        s.push_str("---- Incidents ----\n");
        if list.is_empty() {
            s.push_str("  (none in range)\n");
        }
        for e in list {
            let inc = &e.incident;
            s.push_str(&format!(
                "#{} {} [{}] window {}..{} peak {:.0}%\n",
                inc.id,
                report::kind_label(inc.kind),
                report::severity_label(inc.severity),
                inc.start_ms,
                if inc.end_ms == 0 {
                    "ongoing".to_string()
                } else {
                    inc.end_ms.to_string()
                },
                inc.peak_value
            ));
            s.push_str(&format!("  Summary: {}\n", inc.summary));
            match &e.diagnosis {
                Some(diag) => {
                    s.push_str(&format!("  Observed: {}\n", diag.observed));
                    s.push_str(&format!(
                        "  Overall confidence: {}\n",
                        report::confidence_label(diag.overall_confidence)
                    ));
                    s.push_str("  Contributing factors (correlation, not proof):\n");
                    if diag.factors.is_empty() {
                        s.push_str("    (no single process dominated)\n");
                    }
                    for (i, f) in diag.factors.iter().enumerate() {
                        s.push_str(&format!(
                            "    {}. [{}] {} (attribution {:.0}%)\n",
                            i + 1,
                            report::confidence_label(f.confidence),
                            f.description,
                            f.attribution * 100.0
                        ));
                    }
                    s.push_str(&format!("  Recommendation: {}\n", diag.recommendation));
                }
                None => {
                    s.push_str(&format!(
                        "  Diagnosis unavailable: {}\n",
                        e.unavailable_reason
                    ));
                }
            }
            s.push('\n');
        }
    }

    if let Some(list) = &data.changes {
        s.push_str("---- System changes ----\n");
        if list.is_empty() {
            s.push_str("  (none in range)\n");
        }
        for c in list {
            s.push_str(&format!(
                "  [{}] {}: {}{}\n",
                c.ts_ms,
                c.subject,
                c.detail,
                if c.publisher.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", c.publisher)
                }
            ));
        }
        s.push('\n');
    }

    if let Some(cs) = &data.crashes {
        s.push_str("---- Crashes ----\n");
        if !cs.available {
            s.push_str(&format!("  (unavailable: {})\n", cs.unavailable_reason));
        } else if cs.crashes.is_empty() {
            s.push_str("  (none in range)\n");
        }
        for c in &cs.crashes {
            s.push_str(&format!(
                "  [{}] {}{}{}\n",
                c.ts_ms,
                c.subject,
                if c.fault.is_empty() {
                    String::new()
                } else {
                    format!(" fault={}", c.fault)
                },
                if c.exception_code.is_empty() {
                    String::new()
                } else {
                    format!(" {}", c.exception_code)
                }
            ));
            for line in &c.context {
                s.push_str(&format!("      - {line}\n"));
            }
        }
        s.push('\n');
    }

    if let Some(list) = &data.services {
        s.push_str("---- Services ----\n");
        s.push_str(&format!("  {} service(s)\n", list.len()));
        for sv in list {
            s.push_str(&format!(
                "  {:<32} state={} start={} pid={}\n",
                sv.name, sv.state, sv.start_type, sv.pid
            ));
        }
        s.push('\n');
    }

    if let Some(list) = &data.startup {
        s.push_str("---- Startup ----\n");
        s.push_str(&format!("  {} startup entr(y/ies)\n", list.len()));
        for e in list {
            s.push_str(&format!(
                "  {:<32} {} [{}]{}\n",
                e.name,
                e.command,
                e.scope,
                if e.enabled { "" } else { " (disabled)" }
            ));
        }
        s.push('\n');
    }

    if let Some(m) = &data.self_metrics {
        s.push_str("---- Atlas self-metrics ----\n");
        s.push_str(&format!(
            "  CPU {} | WS {} | tick avg {} us / max {} us over {} ticks\n\n",
            pct(m.cpu_permille),
            human_bytes(m.working_set),
            m.tick_duration_us_avg,
            m.tick_duration_us_max,
            m.ticks
        ));
    }

    s.push_str("======================================================\n");
    s
}

// ---------------------------------------------------------------------------
// JSON rendering (serde_json).
// ---------------------------------------------------------------------------

fn render_json(data: &BundleData) -> String {
    use serde_json::{json, Value};
    let mut root = serde_json::Map::new();
    root.insert(
        "range".into(),
        json!({ "from_ms": data.range_from_ms, "to_ms": data.range_to_ms }),
    );

    if let Some(d) = &data.device {
        root.insert(
            "device".into(),
            json!({
                "os": os_string(d),
                "os_build": d.os_build,
                "hostname": d.hostname,
                "logical_cpus": d.logical_cpus,
                "p_core_count": d.p_core_count,
                "e_core_count": d.e_core_count,
                "heterogeneous": d.heterogeneous,
                "ram_total_bytes": d.ram_total_bytes,
                "atlas_version": d.atlas_version,
                "uptime_ms": d.uptime_ms,
            }),
        );
    }

    if let Some(h) = &data.health {
        let top: Vec<Value> = h
            .top
            .iter()
            .map(|c| {
                json!({
                    "pid": c.pid,
                    "image_name": c.image_name,
                    "cpu_permille": c.cpu_permille,
                    "working_set": c.working_set,
                    "private_bytes": c.private_bytes,
                })
            })
            .collect();
        root.insert(
            "health".into(),
            json!({
                "ts_ms": h.ts_ms,
                "cpu_permille": h.cpu_permille,
                "mem_used": h.mem_used,
                "mem_total": h.mem_total,
                "commit_used": h.commit_used,
                "commit_limit": h.commit_limit,
                "process_count": h.process_count,
                "thread_count": h.thread_count,
                "handle_count": h.handle_count,
                "gpu_permille": h.gpu_permille,
                "gpu_dedicated_used": h.gpu_dedicated_used,
                "gpu_dedicated_budget": h.gpu_dedicated_budget,
                "gpu_shared_used": h.gpu_shared_used,
                "gpu_shared_budget": h.gpu_shared_budget,
                "gpu_details": h.gpu_details,
                "pressure_summary": pressure_summary(h),
                "top": top,
            }),
        );
    }

    if let Some(list) = &data.incidents {
        let incidents: Vec<Value> = list
            .iter()
            .map(|e| {
                let inc = &e.incident;
                let diagnosis = e.diagnosis.as_ref().map(|diag| {
                    let factors: Vec<Value> = diag
                        .factors
                        .iter()
                        .map(|f| {
                            json!({
                                "description": f.description,
                                "confidence": report::confidence_label(f.confidence),
                                "pid": f.pid,
                                "image_name": f.image_name,
                                "attribution": f.attribution,
                            })
                        })
                        .collect();
                    json!({
                        "observed": diag.observed,
                        "overall_confidence": report::confidence_label(diag.overall_confidence),
                        "factors": factors,
                        "alternatives": diag.alternatives,
                        "recommendation": diag.recommendation,
                        "risk": diag.risk,
                        "reversibility": diag.reversibility,
                        "verification_plan": diag.verification_plan,
                    })
                });
                json!({
                    "id": inc.id,
                    "kind": report::kind_label(inc.kind),
                    "severity": report::severity_label(inc.severity),
                    "start_ms": inc.start_ms,
                    "end_ms": if inc.end_ms == 0 { Value::Null } else { json!(inc.end_ms) },
                    "ongoing": inc.end_ms == 0,
                    "peak_value": inc.peak_value,
                    "summary": inc.summary,
                    "diagnosis": diagnosis,
                    "unavailable_reason": e.unavailable_reason,
                })
            })
            .collect();
        root.insert("incidents".into(), Value::Array(incidents));
    }

    if let Some(list) = &data.changes {
        let changes: Vec<Value> = list
            .iter()
            .map(|c| {
                json!({
                    "ts_ms": c.ts_ms,
                    "subject": c.subject,
                    "detail": c.detail,
                    "publisher": c.publisher,
                    "responsible": c.responsible,
                    "reversible": c.reversible,
                })
            })
            .collect();
        root.insert("system_changes".into(), Value::Array(changes));
    }

    if let Some(cs) = &data.crashes {
        let crashes: Vec<Value> = cs
            .crashes
            .iter()
            .map(|c| {
                json!({
                    "ts_ms": c.ts_ms,
                    "subject": c.subject,
                    "fault": c.fault,
                    "exception_code": c.exception_code,
                    "context": c.context,
                })
            })
            .collect();
        root.insert(
            "crashes".into(),
            json!({
                "available": cs.available,
                "unavailable_reason": cs.unavailable_reason,
                "records": crashes,
            }),
        );
    }

    if let Some(list) = &data.services {
        let services: Vec<Value> = list
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "display_name": s.display_name,
                    "state": s.state,
                    "start_type": s.start_type,
                    "pid": s.pid,
                    "account": s.account,
                    "binary_path": s.binary_path,
                })
            })
            .collect();
        root.insert("services".into(), Value::Array(services));
    }

    if let Some(list) = &data.startup {
        let startup: Vec<Value> = list
            .iter()
            .map(|e| {
                json!({
                    "name": e.name,
                    "command": e.command,
                    "publisher": e.publisher,
                    "enabled": e.enabled,
                    "scope": e.scope,
                })
            })
            .collect();
        root.insert("startup".into(), Value::Array(startup));
    }

    if let Some(m) = &data.self_metrics {
        root.insert(
            "self_metrics".into(),
            json!({
                "ts_ms": m.ts_ms,
                "cpu_permille": m.cpu_permille,
                "working_set": m.working_set,
                "tick_duration_us_avg": m.tick_duration_us_avg,
                "tick_duration_us_max": m.tick_duration_us_max,
                "ticks": m.ticks,
            }),
        );
    }

    serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// HTML rendering (self-contained, inline CSS, table of contents, no external
// references — a support engineer opens it straight in a browser).
// ---------------------------------------------------------------------------

fn render_html(data: &BundleData) -> String {
    let esc = report::html_escape;
    // Build the TOC + body from the present sections only.
    let mut toc = String::from("<ul class=\"toc\">");
    let mut body = String::new();

    let section = |id: &str, title: &str, inner: String, toc: &mut String, body: &mut String| {
        toc.push_str(&format!("<li><a href=\"#{id}\">{}</a></li>", esc(title)));
        body.push_str(&format!(
            "<section id=\"{id}\"><h2>{}</h2>{inner}</section>",
            esc(title)
        ));
    };

    if let Some(d) = &data.device {
        let inner = format!(
            "<table>\
             <tr><th>OS</th><td>{}</td></tr>\
             <tr><th>Host</th><td>{}</td></tr>\
             <tr><th>CPU</th><td>{} logical{}</td></tr>\
             <tr><th>RAM</th><td>{}</td></tr>\
             <tr><th>Uptime</th><td>{}</td></tr>\
             <tr><th>Atlas version</th><td>{}</td></tr>\
             </table>",
            esc(&os_string(d)),
            esc(&d.hostname),
            d.logical_cpus,
            if d.heterogeneous {
                format!(" ({} P / {} E cores)", d.p_core_count, d.e_core_count)
            } else {
                String::new()
            },
            esc(&human_bytes(d.ram_total_bytes)),
            esc(&human_uptime(d.uptime_ms)),
            esc(&d.atlas_version),
        );
        section("device", "Device", inner, &mut toc, &mut body);
    }

    if let Some(h) = &data.health {
        let mut rows = String::new();
        for c in &h.top {
            rows.push_str(&format!(
                "<tr><td class=\"num\">{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                c.pid,
                esc(&c.image_name),
                esc(&pct(c.cpu_permille)),
                esc(&human_bytes(c.working_set)),
            ));
        }
        if h.top.is_empty() {
            rows.push_str("<tr><td colspan=\"4\"><em>none</em></td></tr>");
        }
        let gpu_details = if h.gpu_details.is_empty() {
            "<li><em>No GPU adapter details available</em></li>".to_string()
        } else {
            h.gpu_details
                .iter()
                .map(|line| format!("<li>{}</li>", esc(line)))
                .collect::<String>()
        };
        let inner = format!(
            "<p class=\"summary\">{}</p>\
             <p class=\"k\">Processes {}, threads {}, handles {}</p>\
             <p class=\"k\">GPU memory: dedicated {} / {}, shared {} / {}</p>\
             <ul>{gpu_details}</ul>\
             <table><thead><tr><th>PID</th><th>Process</th><th class=\"num\">CPU</th><th class=\"num\">Working set</th></tr></thead>\
             <tbody>{rows}</tbody></table>",
            esc(&pressure_summary(h)),
            h.process_count,
            h.thread_count,
            h.handle_count,
            esc(&human_bytes(h.gpu_dedicated_used)),
            esc(&human_bytes(h.gpu_dedicated_budget)),
            esc(&human_bytes(h.gpu_shared_used)),
            esc(&human_bytes(h.gpu_shared_budget)),
        );
        section("health", "Health", inner, &mut toc, &mut body);
    }

    if let Some(list) = &data.incidents {
        let mut inner = String::new();
        if list.is_empty() {
            inner.push_str("<p><em>none in range</em></p>");
        }
        for e in list {
            let inc = &e.incident;
            inner.push_str(&format!(
                "<div class=\"card\"><p><strong>#{} {}</strong> \
                 <span class=\"badge sev-{}\">{}</span> \
                 <span class=\"k\">window {}..{} · peak {:.0}%</span></p><p>{}</p>",
                inc.id,
                esc(report::kind_label(inc.kind)),
                report::severity_label(inc.severity).to_ascii_lowercase(),
                esc(report::severity_label(inc.severity)),
                inc.start_ms,
                if inc.end_ms == 0 {
                    "ongoing".to_string()
                } else {
                    inc.end_ms.to_string()
                },
                inc.peak_value,
                esc(&inc.summary),
            ));
            match &e.diagnosis {
                Some(diag) => {
                    inner.push_str(&format!(
                        "<p>{}</p><p><strong>Overall confidence:</strong> {}</p>",
                        esc(&diag.observed),
                        report::confidence_label(diag.overall_confidence),
                    ));
                    inner.push_str(
                        "<p class=\"k\">Contributing factors (correlation, not proof):</p><ul>",
                    );
                    if diag.factors.is_empty() {
                        inner.push_str("<li><em>no single process dominated</em></li>");
                    }
                    for f in &diag.factors {
                        inner.push_str(&format!(
                            "<li>[{}] {} <span class=\"k\">(attribution {:.0}%)</span></li>",
                            report::confidence_label(f.confidence),
                            esc(&f.description),
                            f.attribution * 100.0,
                        ));
                    }
                    inner.push_str(&format!(
                        "</ul><p><span class=\"k\">Recommendation:</span> {}</p>",
                        esc(&diag.recommendation)
                    ));
                }
                None => {
                    inner.push_str(&format!(
                        "<p class=\"k\">Diagnosis unavailable: {}</p>",
                        esc(&e.unavailable_reason)
                    ));
                }
            }
            inner.push_str("</div>");
        }
        section("incidents", "Incidents", inner, &mut toc, &mut body);
    }

    if let Some(list) = &data.changes {
        let mut rows = String::new();
        for c in list {
            rows.push_str(&format!(
                "<tr><td class=\"num\">{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                c.ts_ms,
                esc(&c.subject),
                esc(&c.detail),
                esc(&c.publisher),
            ));
        }
        if list.is_empty() {
            rows.push_str("<tr><td colspan=\"4\"><em>none in range</em></td></tr>");
        }
        let inner = format!(
            "<table><thead><tr><th>When (ms)</th><th>Subject</th><th>Detail</th><th>Publisher</th></tr></thead><tbody>{rows}</tbody></table>"
        );
        section("changes", "System changes", inner, &mut toc, &mut body);
    }

    if let Some(cs) = &data.crashes {
        let mut inner = String::new();
        if !cs.available {
            inner.push_str(&format!(
                "<p><em>unavailable: {}</em></p>",
                esc(&cs.unavailable_reason)
            ));
        } else if cs.crashes.is_empty() {
            inner.push_str("<p><em>none in range</em></p>");
        }
        for c in &cs.crashes {
            inner.push_str(&format!(
                "<div class=\"card\"><p><strong>{}</strong> <span class=\"k\">{} {}</span></p>",
                esc(&c.subject),
                esc(&c.fault),
                esc(&c.exception_code),
            ));
            inner.push_str("<ul>");
            for line in &c.context {
                inner.push_str(&format!("<li>{}</li>", esc(line)));
            }
            inner.push_str("</ul></div>");
        }
        section("crashes", "Crashes", inner, &mut toc, &mut body);
    }

    if let Some(list) = &data.services {
        let mut rows = String::new();
        for s in list {
            rows.push_str(&format!(
                "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                esc(&s.name),
                s.state,
                s.start_type,
                s.pid,
            ));
        }
        let inner = format!(
            "<p class=\"k\">{} service(s)</p>\
             <table><thead><tr><th>Name</th><th class=\"num\">State</th><th class=\"num\">Start</th><th class=\"num\">PID</th></tr></thead><tbody>{rows}</tbody></table>",
            list.len()
        );
        section("services", "Services", inner, &mut toc, &mut body);
    }

    if let Some(list) = &data.startup {
        let mut rows = String::new();
        for e in list {
            rows.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{}</td></tr>",
                esc(&e.name),
                esc(&e.command),
                esc(&e.scope),
                if e.enabled { "on" } else { "off" },
            ));
        }
        let inner = format!(
            "<p class=\"k\">{} startup entr(y/ies)</p>\
             <table><thead><tr><th>Name</th><th>Command</th><th>Scope</th><th class=\"num\">Enabled</th></tr></thead><tbody>{rows}</tbody></table>",
            list.len()
        );
        section("startup", "Startup", inner, &mut toc, &mut body);
    }

    if let Some(m) = &data.self_metrics {
        let inner = format!(
            "<table>\
             <tr><th>CPU</th><td>{}</td></tr>\
             <tr><th>Working set</th><td>{}</td></tr>\
             <tr><th>Tick duration</th><td>avg {} µs / max {} µs over {} ticks</td></tr>\
             </table>",
            esc(&pct(m.cpu_permille)),
            esc(&human_bytes(m.working_set)),
            m.tick_duration_us_avg,
            m.tick_duration_us_max,
            m.ticks,
        );
        section(
            "self-metrics",
            "Atlas self-metrics",
            inner,
            &mut toc,
            &mut body,
        );
    }

    toc.push_str("</ul>");

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Atlas support bundle</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font: 15px/1.5 system-ui, sans-serif; max-width: 900px; margin: 2rem auto; padding: 0 1rem; }}
  h1 {{ font-size: 1.6rem; margin-bottom: .25rem; }}
  h2 {{ font-size: 1.2rem; margin-top: 2rem; border-bottom: 1px solid #8884; padding-bottom: .25rem; }}
  .sub {{ color: #888; margin-top: 0; }}
  .k {{ color: #888; }}
  .summary {{ font-weight: 600; }}
  ul.toc {{ columns: 2; margin: 1rem 0; }}
  table {{ border-collapse: collapse; width: 100%; margin: .5rem 0 1rem; }}
  th, td {{ text-align: left; padding: .35rem .6rem; border-bottom: 1px solid #8884; vertical-align: top; }}
  td.num, th.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
  .card {{ border: 1px solid #8884; border-radius: .6rem; padding: .5rem .9rem; margin: .75rem 0; }}
  .badge {{ display: inline-block; padding: .05rem .45rem; border-radius: .5rem; font-size: .78rem; font-weight: 600; }}
  .sev-critical {{ background: #b00020; color: #fff; }}
  .sev-warning {{ background: #a86500; color: #fff; }}
  .sev-info {{ background: #33559b; color: #fff; }}
</style>
</head>
<body>
<h1>Atlas support bundle</h1>
<p class="sub">Window {from} .. {to} · redacted diagnostic document (PRD §9.18)</p>
<nav>{toc}</nav>
{body}
</body>
</html>
"#,
        from = data.range_from_ms,
        to = data.range_to_ms,
        toc = toc,
        body = body,
    )
}

// ---------------------------------------------------------------------------
// Tests: pure assembly/format helpers on fixture data (no store, no OS).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use atlas_ipc::{Confidence, ContributingFactor, IncidentKind, Severity};

    fn opts(u: bool, h: bool, p: bool, c: bool) -> RedactionOptions {
        RedactionOptions {
            redact_user_names: u,
            redact_computer_name: h,
            redact_paths: p,
            redact_command_lines: c,
        }
    }

    fn sample_data() -> BundleData {
        BundleData {
            range_from_ms: 1_700_000_000_000,
            // 2023-11-14 (UTC) — used for the filename date derivation.
            range_to_ms: 1_700_000_100_000,
            device: Some(DeviceSection {
                os_major: 10,
                os_minor: 0,
                os_build: 26_200,
                hostname: "WORKPC".into(),
                logical_cpus: 16,
                p_core_count: 8,
                e_core_count: 8,
                heterogeneous: true,
                ram_total_bytes: 34_359_738_368,
                atlas_version: "0.1.0".into(),
                uptime_ms: 90_061_000,
            }),
            health: Some(HealthSection {
                ts_ms: 1_700_000_099_000,
                cpu_permille: 420,
                mem_used: 20_000_000_000,
                mem_total: 34_359_738_368,
                commit_used: 22_000_000_000,
                commit_limit: 40_000_000_000,
                process_count: 300,
                thread_count: 4000,
                handle_count: 90000,
                gpu_permille: 510,
                gpu_dedicated_used: 2_000_000_000,
                gpu_dedicated_budget: 8_000_000_000,
                gpu_shared_used: 100_000_000,
                gpu_shared_budget: 8_000_000_000,
                gpu_details: vec!["RTX fixture source=NVIDIA NVML".into()],
                top: vec![ConsumerRow {
                    pid: 4242,
                    image_name: r"game.exe from C:\Users\alice\game.exe".into(),
                    cpu_permille: 380,
                    working_set: 1_500_000_000,
                    private_bytes: 1_200_000_000,
                }],
            }),
            incidents: Some(vec![IncidentEntry {
                incident: Incident {
                    id: 7,
                    kind: IncidentKind::CpuSaturation as i32,
                    start_ms: 1_700_000_000_000,
                    end_ms: 0,
                    severity: Severity::Critical as i32,
                    peak_value: 96.0,
                    summary: r"CPU saturated by C:\Users\alice\game.exe".into(),
                },
                diagnosis: Some(Diagnosis {
                    observed: "CPU under pressure".into(),
                    range: None,
                    evidence: vec![],
                    factors: vec![ContributingFactor {
                        description: r"game.exe ran from C:\Users\alice\game.exe".into(),
                        confidence: Confidence::High as i32,
                        pid: 4242,
                        image_name: "game.exe".into(),
                        attribution: 0.82,
                    }],
                    overall_confidence: Confidence::High as i32,
                    alternatives: vec![],
                    recommendation: "close game.exe".into(),
                    risk: "loses unsaved work".into(),
                    reversibility: "reversible".into(),
                    verification_plan: "watch CPU".into(),
                }),
                unavailable_reason: String::new(),
            }]),
            changes: Some(vec![SystemChange {
                id: 1,
                ts_ms: 1_700_000_050_000,
                kind: 0,
                subject: "Acme Reader".into(),
                detail: "1.2.3 → 1.2.4".into(),
                publisher: "Acme".into(),
                responsible: String::new(),
                reversible: false,
            }]),
            crashes: Some(CrashesSection {
                available: true,
                unavailable_reason: String::new(),
                crashes: vec![CrashRecord {
                    id: 9,
                    ts_ms: 1_700_000_060_000,
                    kind: 0,
                    subject: r"app.exe at C:\Users\alice\app.exe".into(),
                    fault: "app.dll".into(),
                    exception_code: "0xc0000005".into(),
                    context: vec!["peak memory 82% before this event".into()],
                }],
            }),
            services: Some(vec![ServiceEntry {
                name: "Spooler".into(),
                display_name: "Print Spooler".into(),
                description: "Manages print jobs".into(),
                state: 4,
                start_type: 2,
                pid: 1234,
                account: r"WORKPC\alice".into(),
                binary_path: r"C:\Windows\System32\spoolsv.exe".into(),
                delayed_auto_start: false,
            }]),
            startup: Some(vec![StartupEntry {
                name: "GameLauncher".into(),
                source: 0,
                command: r"C:\Users\alice\game.exe --fast".into(),
                publisher: "Acme".into(),
                enabled: true,
                scope: "user".into(),
            }]),
            self_metrics: Some(SelfMetricsSection {
                ts_ms: 1_700_000_099_000,
                cpu_permille: 3,
                working_set: 13_000_000,
                tick_duration_us_avg: 800,
                tick_duration_us_max: 2100,
                ticks: 15,
            }),
        }
    }

    #[test]
    fn selection_empty_is_all() {
        assert_eq!(selected(&[]), SectionSet::all());
    }

    #[test]
    fn selection_picks_only_requested() {
        let s = selected(&[
            SupportBundleSection::BundleHealth as i32,
            SupportBundleSection::BundleIncidents as i32,
        ]);
        assert!(s.health && s.incidents);
        assert!(!s.device && !s.changes && !s.crashes && !s.services && !s.startup);
    }

    #[test]
    fn filename_date_derived_from_range_end() {
        let reply = build_bundle(
            sample_data(),
            ReportFormat::ReportHtml,
            &opts(false, false, false, false),
        );
        assert_eq!(reply.filename, "atlas-support-2023-11-14.html");
        let reply_json = build_bundle(
            sample_data(),
            ReportFormat::ReportJson,
            &opts(false, false, false, false),
        );
        assert_eq!(reply_json.filename, "atlas-support-2023-11-14.json");
    }

    #[test]
    fn path_toggle_strips_paths_in_every_format_output() {
        // Path redaction is env-independent, so it proves the pass reaches every
        // format: the alice path must never survive in any rendering.
        for fmt in [
            ReportFormat::ReportText,
            ReportFormat::ReportJson,
            ReportFormat::ReportHtml,
        ] {
            let reply = build_bundle(sample_data(), fmt, &opts(false, false, true, false));
            assert!(
                !reply.content.contains(r"C:\Users\alice"),
                "path leaked in {fmt:?}: {}",
                reply.content
            );
            assert!(
                reply.content.contains("<PATH>") || reply.content.contains("&lt;PATH&gt;"),
                "redaction placeholder missing in {fmt:?}"
            );
            assert_eq!(reply.redaction_applied, vec!["paths".to_string()]);
        }
    }

    #[test]
    fn host_and_user_toggles_strip_named_fields() {
        // Explicit needles so the test does not depend on the host environment.
        let r = Redactor::with(
            opts(true, true, false, false),
            Some("alice".into()),
            Some("WORKPC".into()),
        );
        let redacted = redact_data(&sample_data(), &r);
        // Host in device section replaced.
        assert_eq!(redacted.device.unwrap().hostname, "<HOST>");
        // User inside the service account replaced.
        let acct = &redacted.services.as_ref().unwrap()[0].account;
        assert!(
            !acct.to_ascii_lowercase().contains("alice"),
            "account: {acct}"
        );
        assert!(acct.contains("<USER>"));
    }

    #[test]
    fn redaction_applied_lists_every_enabled_category() {
        let reply = build_bundle(
            sample_data(),
            ReportFormat::ReportText,
            &opts(true, true, true, true),
        );
        assert_eq!(
            reply.redaction_applied,
            vec![
                "paths".to_string(),
                "user_names".to_string(),
                "computer_name".to_string(),
                "command_lines".to_string(),
            ]
        );
    }

    #[test]
    fn html_is_self_contained_no_external_refs() {
        let reply = build_bundle(
            sample_data(),
            ReportFormat::ReportHtml,
            &opts(false, false, false, false),
        );
        assert_eq!(reply.content_type, "text/html");
        assert!(reply.content.contains("<!doctype html>"));
        // No external references of any kind.
        assert!(!reply.content.contains("http://"), "http ref present");
        assert!(!reply.content.contains("https://"), "https ref present");
        assert!(!reply.content.contains("//cdn"), "cdn ref present");
        assert!(!reply.content.contains("src=\"http"));
        assert!(!reply.content.contains("<link"), "external stylesheet link");
        assert!(!reply.content.contains("<script"), "script tag present");
        // Table of contents present.
        assert!(reply.content.contains("class=\"toc\""));
    }

    #[test]
    fn section_selection_omits_unrequested_sections() {
        // Only health assembled -> other sections absent from all formats.
        let data = BundleData {
            range_from_ms: 0,
            range_to_ms: 1_700_000_100_000,
            health: sample_data().health,
            ..BundleData::default()
        };
        let json = render_json(&data);
        assert!(json.contains("\"health\""));
        assert!(!json.contains("\"device\""));
        assert!(!json.contains("\"incidents\""));
        assert!(!json.contains("\"services\""));

        let html = render_html(&data);
        assert!(html.contains("id=\"health\""));
        assert!(!html.contains("id=\"device\""));
        assert!(!html.contains("id=\"services\""));
    }

    #[test]
    fn json_is_valid_and_carries_sections() {
        let reply = build_bundle(
            sample_data(),
            ReportFormat::ReportJson,
            &opts(false, false, false, false),
        );
        assert_eq!(reply.content_type, "application/json");
        let v: serde_json::Value = serde_json::from_str(&reply.content).unwrap();
        assert_eq!(v["device"]["logical_cpus"], 16);
        assert_eq!(v["incidents"][0]["id"], 7);
        assert_eq!(v["incidents"][0]["diagnosis"]["overall_confidence"], "HIGH");
        assert_eq!(v["crashes"]["available"], true);
    }

    #[test]
    fn no_redaction_leaves_text_intact() {
        let reply = build_bundle(
            sample_data(),
            ReportFormat::ReportText,
            &opts(false, false, false, false),
        );
        assert!(reply.redaction_applied.is_empty());
        assert!(reply.content.contains(r"C:\Users\alice\game.exe"));
    }
}
