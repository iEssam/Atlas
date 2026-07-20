//! Diagnosis report export with a redaction pass (docs/phases.md M8, PRD §9.18).
//!
//! Renders an [`Incident`] + its [`Diagnosis`] to TEXT, JSON, CSV, or HTML. A
//! single [`Redactor`] pass rewrites the textual fields **before** any formatter
//! runs, so every format is redacted identically — the redaction is not per-
//! renderer. Placeholders are stable (`<USER>`, `<HOST>`, `<PATH>`,
//! `<CMD-ARGS>`) so a redacted report is still diff-able and readable.
//!
//! No new dependencies: JSON is built with the already-present `serde_json`;
//! CSV/HTML/TEXT are hand-rendered with explicit escaping.

use atlas_ipc::{
    Confidence, DiagnoseReply, Diagnosis, Incident, IncidentKind, RedactionOptions, ReportFormat,
    Severity,
};

/// Redacts personal / machine-identifying substrings from rendered text. Each
/// [`RedactionOptions`] toggle removes one class; the pass is ordered so path
/// redaction runs before user/host name substitution (a path often contains the
/// user name, and collapsing the whole path first avoids a half-redacted path).
pub struct Redactor {
    opts: RedactionOptions,
    user: Option<String>,
    host: Option<String>,
}

impl Redactor {
    /// Reads the current user (`USERNAME`) and computer (`COMPUTERNAME`) from the
    /// environment as the needles for name redaction.
    pub fn from_env(opts: RedactionOptions) -> Self {
        Self::with(
            opts,
            std::env::var("USERNAME").ok().filter(|s| !s.is_empty()),
            std::env::var("COMPUTERNAME").ok().filter(|s| !s.is_empty()),
        )
    }

    /// Explicit needles (used by tests so redaction does not depend on the host
    /// environment).
    pub fn with(opts: RedactionOptions, user: Option<String>, host: Option<String>) -> Self {
        Self { opts, user, host }
    }

    /// Applies every enabled redaction to `s`.
    pub fn apply(&self, s: &str) -> String {
        let mut out = s.to_string();
        if self.opts.redact_paths {
            out = redact_paths(&out);
        }
        if self.opts.redact_command_lines {
            out = redact_command_lines(&out);
        }
        if self.opts.redact_user_names {
            if let Some(u) = &self.user {
                out = replace_ci(&out, u, "<USER>");
            }
        }
        if self.opts.redact_computer_name {
            if let Some(h) = &self.host {
                out = replace_ci(&out, h, "<HOST>");
            }
        }
        out
    }
}

/// Replaces Windows path runs with `<PATH>`. Recognises drive paths (`C:\...`,
/// `C:/...`) and UNC paths (`\\server\share`), consuming until whitespace or a
/// quote. Unquoted paths with spaces are redacted only up to the first space
/// (documented limitation — recorded image names are single tokens, e.g. NT
/// device paths, which redact cleanly).
fn redact_paths(s: &str) -> String {
    let bytes: Vec<char> = s.chars().collect();
    let n = bytes.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < n {
        let is_drive = bytes[i].is_ascii_alphabetic()
            && i + 2 < n
            && bytes[i + 1] == ':'
            && (bytes[i + 2] == '\\' || bytes[i + 2] == '/');
        let is_unc = bytes[i] == '\\' && i + 1 < n && bytes[i + 1] == '\\';
        if is_drive || is_unc {
            // Consume the path token until whitespace or a quote.
            let mut j = i;
            while j < n && !bytes[j].is_whitespace() && bytes[j] != '"' {
                j += 1;
            }
            out.push_str("<PATH>");
            i = j;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// Redacts the arguments following an executable token: on each line, once a
/// token ending in an executable extension is seen, the remaining tokens on that
/// line are replaced with `<CMD-ARGS>`. The executable name itself is kept.
fn redact_command_lines(s: &str) -> String {
    let exts = [".exe", ".com", ".bat", ".cmd", ".ps1", ".scr"];
    s.split('\n')
        .map(|line| {
            let mut toks = line.split(' ');
            let mut rebuilt: Vec<String> = Vec::new();
            let mut redacted = false;
            for tok in toks.by_ref() {
                rebuilt.push(tok.to_string());
                let lower = tok.to_ascii_lowercase();
                if exts.iter().any(|e| lower.ends_with(e)) {
                    // Everything after the executable token on this line is args.
                    if toks.clone().any(|t| !t.is_empty()) {
                        rebuilt.push("<CMD-ARGS>".to_string());
                        redacted = true;
                    }
                    break;
                }
            }
            if redacted {
                rebuilt.join(" ")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Case-insensitive replace of `needle` with `replacement` (needle non-empty).
fn replace_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let hay_l = haystack.to_ascii_lowercase();
    let need_l = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if hay_l[i..].starts_with(&need_l) {
            out.push_str(replacement);
            i += needle.len();
        } else {
            // Advance by one char (respecting UTF-8 boundaries).
            let ch = haystack[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Applies the redactor to every textual field of the incident + diagnosis,
/// returning redacted copies the formatters consume. Shared with the support
/// bundle's incidents section (docs/phases.md R3) so both redact identically.
pub(crate) fn redact(incident: &Incident, diag: &Diagnosis, r: &Redactor) -> (Incident, Diagnosis) {
    let inc = Incident {
        summary: r.apply(&incident.summary),
        ..incident.clone()
    };
    let diag = Diagnosis {
        observed: r.apply(&diag.observed),
        range: diag.range,
        evidence: diag
            .evidence
            .iter()
            .map(|e| {
                let mut e = e.clone();
                e.text = r.apply(&e.text);
                e
            })
            .collect(),
        factors: diag
            .factors
            .iter()
            .map(|f| {
                let mut f = f.clone();
                f.description = r.apply(&f.description);
                f.image_name = r.apply(&f.image_name);
                f
            })
            .collect(),
        overall_confidence: diag.overall_confidence,
        alternatives: diag.alternatives.iter().map(|a| r.apply(a)).collect(),
        recommendation: r.apply(&diag.recommendation),
        risk: r.apply(&diag.risk),
        reversibility: r.apply(&diag.reversibility),
        verification_plan: r.apply(&diag.verification_plan),
    };
    (inc, diag)
}

/// Renders a full report for `format`, applying `redaction` first. When the
/// diagnosis is unavailable, a short report states the reason instead. Returns
/// `(content, content_type)`.
pub fn render_report(
    incident: &Incident,
    reply: &DiagnoseReply,
    format: ReportFormat,
    redaction: &RedactionOptions,
) -> (String, String) {
    let r = Redactor::from_env(*redaction);
    match &reply.diagnosis {
        Some(diag) if reply.available => {
            let (inc, diag) = redact(incident, diag, &r);
            match format {
                ReportFormat::ReportJson => (render_json(&inc, &diag), "application/json".into()),
                ReportFormat::ReportCsv => (render_csv(&inc, &diag), "text/csv".into()),
                ReportFormat::ReportHtml => (render_html(&inc, &diag), "text/html".into()),
                // TEXT and UNSPECIFIED both render plain text.
                _ => (render_text(&inc, &diag), "text/plain".into()),
            }
        }
        _ => {
            let reason = r.apply(&reply.unavailable_reason);
            render_unavailable(format, &reason)
        }
    }
}

fn render_unavailable(format: ReportFormat, reason: &str) -> (String, String) {
    match format {
        ReportFormat::ReportJson => (
            serde_json::json!({ "available": false, "unavailable_reason": reason }).to_string(),
            "application/json".into(),
        ),
        ReportFormat::ReportCsv => (
            format!(
                "section,key,value\ndiagnosis,available,false\ndiagnosis,reason,{}\n",
                csv_field(reason)
            ),
            "text/csv".into(),
        ),
        ReportFormat::ReportHtml => (
            format!(
                "<!doctype html><meta charset=\"utf-8\"><title>Atlas diagnosis</title>\
                 <body><h1>Diagnosis unavailable</h1><p>{}</p></body>",
                html_escape(reason)
            ),
            "text/html".into(),
        ),
        _ => (
            format!("Atlas diagnosis\n\nDiagnosis unavailable: {reason}\n"),
            "text/plain".into(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Label helpers (proto discriminants -> human strings).
// ---------------------------------------------------------------------------

pub(crate) fn kind_label(kind: i32) -> &'static str {
    match IncidentKind::try_from(kind) {
        Ok(IncidentKind::CpuSaturation) => "CPU saturation",
        Ok(IncidentKind::MemoryPressure) => "Memory pressure",
        Ok(IncidentKind::DiskLatency) => "Disk latency",
        Ok(IncidentKind::GpuSaturation) => "GPU saturation",
        Ok(IncidentKind::GpuMemoryExhaustion) => "GPU memory pressure",
        Ok(IncidentKind::GpuThermalThrottling) => "GPU thermal throttling",
        Ok(IncidentKind::SystemThermalLimit) => "System thermal limit",
        _ => "Unspecified",
    }
}

pub(crate) fn severity_label(sev: i32) -> &'static str {
    match Severity::try_from(sev) {
        Ok(Severity::Info) => "Info",
        Ok(Severity::Warning) => "Warning",
        Ok(Severity::Critical) => "Critical",
        _ => "Unspecified",
    }
}

pub(crate) fn confidence_label(c: i32) -> &'static str {
    match Confidence::try_from(c) {
        Ok(Confidence::Insufficient) => "INSUFFICIENT",
        Ok(Confidence::Low) => "LOW",
        Ok(Confidence::Medium) => "MEDIUM",
        Ok(Confidence::High) => "HIGH",
        Ok(Confidence::Confirmed) => "CONFIRMED",
        _ => "UNSPECIFIED",
    }
}

fn end_label(end_ms: i64) -> String {
    if end_ms == 0 {
        "ongoing".to_string()
    } else {
        end_ms.to_string()
    }
}

// ---------------------------------------------------------------------------
// TEXT
// ---------------------------------------------------------------------------

fn render_text(inc: &Incident, diag: &Diagnosis) -> String {
    let mut s = String::new();
    s.push_str("======== Atlas incident report ========\n");
    s.push_str(&format!(
        "Incident #{}  {}  [{}]\n",
        inc.id,
        kind_label(inc.kind),
        severity_label(inc.severity)
    ));
    s.push_str(&format!(
        "Window: {} .. {}   peak {:.0}%\n",
        inc.start_ms,
        end_label(inc.end_ms),
        inc.peak_value
    ));
    s.push_str(&format!("Summary: {}\n\n", inc.summary));

    s.push_str(&format!("Observed: {}\n\n", diag.observed));

    s.push_str("Evidence (measured facts):\n");
    if diag.evidence.is_empty() {
        s.push_str("  (none)\n");
    }
    for e in &diag.evidence {
        s.push_str(&format!("  - {} [{}={:.1}]\n", e.text, e.metric, e.value));
    }
    s.push('\n');

    s.push_str("Contributing factors (ranked; correlation, not proof):\n");
    if diag.factors.is_empty() {
        s.push_str("  (no single process dominated)\n");
    }
    for (i, f) in diag.factors.iter().enumerate() {
        s.push_str(&format!(
            "  {}. [{}] {} (attribution {:.0}%)\n",
            i + 1,
            confidence_label(f.confidence),
            f.description,
            f.attribution * 100.0
        ));
    }
    s.push('\n');

    if !diag.alternatives.is_empty() {
        s.push_str("Alternatives:\n");
        for a in &diag.alternatives {
            s.push_str(&format!("  - {a}\n"));
        }
        s.push('\n');
    }

    s.push_str(&format!(
        "Overall confidence: {}\n\n",
        confidence_label(diag.overall_confidence)
    ));
    s.push_str(&format!("Recommendation: {}\n", diag.recommendation));
    s.push_str(&format!("Risk: {}\n", diag.risk));
    s.push_str(&format!("Reversibility: {}\n", diag.reversibility));
    s.push_str(&format!("Verification plan: {}\n", diag.verification_plan));
    s.push_str("=======================================\n");
    s
}

// ---------------------------------------------------------------------------
// JSON (serde_json)
// ---------------------------------------------------------------------------

fn render_json(inc: &Incident, diag: &Diagnosis) -> String {
    let evidence: Vec<serde_json::Value> = diag
        .evidence
        .iter()
        .map(|e| {
            serde_json::json!({
                "text": e.text,
                "ts_ms": e.ts_ms,
                "metric": e.metric,
                "value": e.value,
            })
        })
        .collect();
    let factors: Vec<serde_json::Value> = diag
        .factors
        .iter()
        .map(|f| {
            serde_json::json!({
                "description": f.description,
                "confidence": confidence_label(f.confidence),
                "pid": f.pid,
                "image_name": f.image_name,
                "attribution": f.attribution,
            })
        })
        .collect();
    let range = diag
        .range
        .as_ref()
        .map(|r| serde_json::json!({ "from_ms": r.from_ms, "to_ms": r.to_ms }));
    let v = serde_json::json!({
        "incident": {
            "id": inc.id,
            "kind": kind_label(inc.kind),
            "severity": severity_label(inc.severity),
            "start_ms": inc.start_ms,
            "end_ms": if inc.end_ms == 0 { serde_json::Value::Null } else { serde_json::json!(inc.end_ms) },
            "ongoing": inc.end_ms == 0,
            "peak_value": inc.peak_value,
            "summary": inc.summary,
        },
        "diagnosis": {
            "observed": diag.observed,
            "range": range,
            "overall_confidence": confidence_label(diag.overall_confidence),
            "evidence": evidence,
            "factors": factors,
            "alternatives": diag.alternatives,
            "recommendation": diag.recommendation,
            "risk": diag.risk,
            "reversibility": diag.reversibility,
            "verification_plan": diag.verification_plan,
        }
    });
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// CSV (section,key,value)
// ---------------------------------------------------------------------------

fn render_csv(inc: &Incident, diag: &Diagnosis) -> String {
    let mut rows: Vec<[String; 3]> = Vec::new();
    let mut push = |a: &str, b: &str, c: String| rows.push([a.to_string(), b.to_string(), c]);

    push("incident", "id", inc.id.to_string());
    push("incident", "kind", kind_label(inc.kind).to_string());
    push(
        "incident",
        "severity",
        severity_label(inc.severity).to_string(),
    );
    push("incident", "start_ms", inc.start_ms.to_string());
    push("incident", "end_ms", end_label(inc.end_ms));
    push("incident", "peak_value", format!("{:.2}", inc.peak_value));
    push("incident", "summary", inc.summary.clone());

    push("diagnosis", "observed", diag.observed.clone());
    push(
        "diagnosis",
        "overall_confidence",
        confidence_label(diag.overall_confidence).to_string(),
    );
    push("diagnosis", "recommendation", diag.recommendation.clone());
    push("diagnosis", "risk", diag.risk.clone());
    push("diagnosis", "reversibility", diag.reversibility.clone());
    push(
        "diagnosis",
        "verification_plan",
        diag.verification_plan.clone(),
    );

    for e in &diag.evidence {
        push(
            "evidence",
            &e.metric,
            format!("{} (value {:.2}, ts {})", e.text, e.value, e.ts_ms),
        );
    }
    for f in &diag.factors {
        push(
            "factor",
            &format!("{} (pid {})", f.image_name, f.pid),
            format!(
                "{} | confidence {} | attribution {:.0}%",
                f.description,
                confidence_label(f.confidence),
                f.attribution * 100.0
            ),
        );
    }
    for (i, a) in diag.alternatives.iter().enumerate() {
        push("alternative", &(i + 1).to_string(), a.clone());
    }

    let mut out = String::from("section,key,value\n");
    for r in rows {
        out.push_str(&format!(
            "{},{},{}\n",
            csv_field(&r[0]),
            csv_field(&r[1]),
            csv_field(&r[2])
        ));
    }
    out
}

/// CSV-escapes a field: wraps in quotes and doubles internal quotes when it
/// contains a comma, quote, or newline (RFC 4180).
fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// HTML (self-contained, inline CSS)
// ---------------------------------------------------------------------------

fn render_html(inc: &Incident, diag: &Diagnosis) -> String {
    let mut ev = String::new();
    for e in &diag.evidence {
        ev.push_str(&format!(
            "<tr><td>{}</td><td class=\"num\">{:.1}</td><td>{}</td></tr>",
            html_escape(&e.text),
            e.value,
            html_escape(&e.metric)
        ));
    }
    if diag.evidence.is_empty() {
        ev.push_str("<tr><td colspan=\"3\"><em>none</em></td></tr>");
    }

    let mut fac = String::new();
    for f in &diag.factors {
        fac.push_str(&format!(
            "<tr><td>{}</td><td><span class=\"conf conf-{}\">{}</span></td><td class=\"num\">{:.0}%</td></tr>",
            html_escape(&f.description),
            confidence_label(f.confidence).to_ascii_lowercase(),
            confidence_label(f.confidence),
            f.attribution * 100.0
        ));
    }
    if diag.factors.is_empty() {
        fac.push_str("<tr><td colspan=\"3\"><em>no single process dominated</em></td></tr>");
    }

    let mut alts = String::new();
    if !diag.alternatives.is_empty() {
        alts.push_str("<h2>Alternatives</h2><ul>");
        for a in &diag.alternatives {
            alts.push_str(&format!("<li>{}</li>", html_escape(a)));
        }
        alts.push_str("</ul>");
    }

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Atlas incident #{id} report</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font: 15px/1.5 system-ui, sans-serif; max-width: 820px; margin: 2rem auto; padding: 0 1rem; }}
  h1 {{ font-size: 1.5rem; margin-bottom: .25rem; }}
  .sub {{ color: #888; margin-top: 0; }}
  .badge {{ display: inline-block; padding: .1rem .5rem; border-radius: .5rem; font-size: .8rem; font-weight: 600; }}
  .sev-critical {{ background: #b00020; color: #fff; }}
  .sev-warning {{ background: #a86500; color: #fff; }}
  .sev-info {{ background: #33559b; color: #fff; }}
  table {{ border-collapse: collapse; width: 100%; margin: .5rem 0 1.25rem; }}
  th, td {{ text-align: left; padding: .4rem .6rem; border-bottom: 1px solid #8884; vertical-align: top; }}
  td.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
  .conf {{ font-size: .75rem; font-weight: 700; padding: .05rem .4rem; border-radius: .4rem; }}
  .conf-high, .conf-confirmed {{ background: #1b5e20; color: #fff; }}
  .conf-medium {{ background: #a86500; color: #fff; }}
  .conf-low, .conf-insufficient {{ background: #555; color: #fff; }}
  .card {{ border: 1px solid #8884; border-radius: .6rem; padding: .75rem 1rem; margin: 1rem 0; }}
  .k {{ color: #888; }}
</style>
</head>
<body>
<h1>Incident #{id}: {kind}</h1>
<p class="sub"><span class="badge sev-{sev_class}">{sev}</span>
  &nbsp;window {start} .. {end} &nbsp;•&nbsp; peak {peak:.0}%</p>
<p><strong>{summary}</strong></p>
<p>{observed}</p>

<h2>Evidence <span class="k">(measured facts)</span></h2>
<table><thead><tr><th>Fact</th><th class="num">Value</th><th>Metric</th></tr></thead>
<tbody>{ev}</tbody></table>

<h2>Contributing factors <span class="k">(correlation, not proof)</span></h2>
<table><thead><tr><th>Factor</th><th>Confidence</th><th class="num">Attribution</th></tr></thead>
<tbody>{fac}</tbody></table>
{alts}
<p><strong>Overall confidence:</strong> {overall}</p>

<div class="card">
  <p><span class="k">Recommendation:</span> {rec}</p>
  <p><span class="k">Risk:</span> {risk}</p>
  <p><span class="k">Reversibility:</span> {rev}</p>
  <p><span class="k">Verification plan:</span> {verify}</p>
</div>
</body>
</html>
"#,
        id = inc.id,
        kind = html_escape(kind_label(inc.kind)),
        sev = severity_label(inc.severity),
        sev_class = severity_label(inc.severity).to_ascii_lowercase(),
        start = inc.start_ms,
        end = html_escape(&end_label(inc.end_ms)),
        peak = inc.peak_value,
        summary = html_escape(&inc.summary),
        observed = html_escape(&diag.observed),
        ev = ev,
        fac = fac,
        alts = alts,
        overall = confidence_label(diag.overall_confidence),
        rec = html_escape(&diag.recommendation),
        risk = html_escape(&diag.risk),
        rev = html_escape(&diag.reversibility),
        verify = html_escape(&diag.verification_plan),
    )
}

/// Minimal HTML-escaping for text nodes/attribute-free content.
pub(crate) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(u: bool, h: bool, p: bool, c: bool) -> RedactionOptions {
        RedactionOptions {
            redact_user_names: u,
            redact_computer_name: h,
            redact_paths: p,
            redact_command_lines: c,
        }
    }

    #[test]
    fn redact_user_and_host_are_case_insensitive() {
        let r = Redactor::with(
            opts(true, true, false, false),
            Some("alice".into()),
            Some("WORKPC".into()),
        );
        let s = "User Alice on host workpc did X; alice again";
        let out = r.apply(s);
        assert!(
            !out.to_ascii_lowercase().contains("alice"),
            "user removed: {out}"
        );
        assert!(
            !out.to_ascii_lowercase().contains("workpc"),
            "host removed: {out}"
        );
        assert!(out.contains("<USER>") && out.contains("<HOST>"));
    }

    #[test]
    fn redact_paths_collapses_drive_and_unc() {
        let r = Redactor::with(opts(false, false, true, false), None, None);
        let out = r.apply(r"opened C:\Users\bob\secret.txt and \\srv\share\f.dat done");
        assert!(!out.contains("secret.txt"), "drive path removed: {out}");
        assert!(!out.contains("srv"), "unc path removed: {out}");
        assert_eq!(out.matches("<PATH>").count(), 2);
    }

    #[test]
    fn redact_paths_leaves_plain_text_alone() {
        let r = Redactor::with(opts(false, false, true, false), None, None);
        let s = "chrome.exe averaged 40% CPU (peak 60%)";
        assert_eq!(r.apply(s), s, "no path -> unchanged");
    }

    #[test]
    fn redact_command_lines_keeps_exe_drops_args() {
        let r = Redactor::with(opts(false, false, false, true), None, None);
        let out = r.apply("app.exe --token ABCDEF --user bob");
        assert!(out.starts_with("app.exe"));
        assert!(out.contains("<CMD-ARGS>"));
        assert!(!out.contains("ABCDEF"), "args removed: {out}");
    }

    #[test]
    fn each_toggle_is_independent() {
        // Only user redaction on: host/path/cmd untouched.
        let r = Redactor::with(
            opts(true, false, false, false),
            Some("bob".into()),
            Some("PC".into()),
        );
        let out = r.apply(r"bob ran app.exe --x on PC at C:\tmp\a");
        assert!(out.contains("<USER>"));
        assert!(out.contains("PC"), "host NOT redacted when toggle off");
        assert!(
            out.contains(r"C:\tmp\a"),
            "path NOT redacted when toggle off"
        );
        assert!(out.contains("--x"), "cmd NOT redacted when toggle off");
    }

    fn sample() -> (Incident, DiagnoseReply) {
        let inc = Incident {
            id: 7,
            kind: IncidentKind::CpuSaturation as i32,
            start_ms: 1_000,
            end_ms: 0,
            severity: Severity::Critical as i32,
            peak_value: 96.0,
            summary: r"CPU saturated by C:\Users\alice\game.exe".into(),
        };
        let diag = Diagnosis {
            observed: "CPU under pressure".into(),
            range: Some(atlas_ipc::TimeRange {
                from_ms: 1_000,
                to_ms: 20_000,
            }),
            evidence: vec![EvidenceItemStub::cpu()],
            factors: vec![atlas_ipc::ContributingFactor {
                description: r"game.exe (pid 42) ran from C:\Users\alice\game.exe".into(),
                confidence: Confidence::High as i32,
                pid: 42,
                image_name: "game.exe".into(),
                attribution: 0.82,
            }],
            overall_confidence: Confidence::High as i32,
            alternatives: vec![],
            recommendation: "close game.exe".into(),
            risk: "loses unsaved work".into(),
            reversibility: "reversible".into(),
            verification_plan: "watch CPU".into(),
        };
        (
            inc,
            DiagnoseReply {
                available: true,
                unavailable_reason: String::new(),
                diagnosis: Some(diag),
            },
        )
    }

    // Tiny helper so the sample stays readable.
    struct EvidenceItemStub;
    impl EvidenceItemStub {
        fn cpu() -> atlas_ipc::EvidenceItem {
            atlas_ipc::EvidenceItem {
                text: "Peak system CPU 96%".into(),
                ts_ms: 5_000,
                metric: "sys_cpu_pct".into(),
                value: 96.0,
            }
        }
    }

    #[test]
    fn redaction_applies_across_every_format() {
        let (inc, reply) = sample();
        // Path redaction is deterministic (no env dependency), so it proves the
        // pass reaches every format: the alice path must never survive.
        let redo = opts(false, false, true, false);
        for fmt in [
            ReportFormat::ReportText,
            ReportFormat::ReportJson,
            ReportFormat::ReportCsv,
            ReportFormat::ReportHtml,
        ] {
            let (content, ct) = render_report(&inc, &reply, fmt, &redo);
            assert!(!ct.is_empty());
            // The raw path must never survive in any format.
            assert!(
                !content.contains(r"C:\Users\alice"),
                "path leaked in {:?}: {content}",
                fmt
            );
            // The placeholder is present — HTML-escaped to &lt;PATH&gt; in the
            // HTML rendering, which is correct (the escaping is applied after
            // redaction), so accept either form.
            assert!(
                content.contains("<PATH>") || content.contains("&lt;PATH&gt;"),
                "redaction placeholder missing in {:?}: {content}",
                fmt
            );
        }
    }

    #[test]
    fn formats_have_expected_shape() {
        let (inc, reply) = sample();
        let none = opts(false, false, false, false);
        let (json, ct) = render_report(&inc, &reply, ReportFormat::ReportJson, &none);
        assert_eq!(ct, "application/json");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["incident"]["id"], 7);
        assert_eq!(parsed["diagnosis"]["overall_confidence"], "HIGH");

        let (csv, ct) = render_report(&inc, &reply, ReportFormat::ReportCsv, &none);
        assert_eq!(ct, "text/csv");
        assert!(csv.starts_with("section,key,value\n"));

        let (html, ct) = render_report(&inc, &reply, ReportFormat::ReportHtml, &none);
        assert_eq!(ct, "text/html");
        assert!(html.contains("<!doctype html>"));
        assert!(html.contains("Incident #7"));

        let (text, ct) = render_report(&inc, &reply, ReportFormat::ReportText, &none);
        assert_eq!(ct, "text/plain");
        assert!(text.contains("Contributing factors"));
    }

    #[test]
    fn unavailable_renders_in_every_format() {
        let inc = sample().0;
        let reply = DiagnoseReply {
            available: false,
            unavailable_reason: "insufficient evidence".into(),
            diagnosis: None,
        };
        for fmt in [
            ReportFormat::ReportText,
            ReportFormat::ReportJson,
            ReportFormat::ReportCsv,
            ReportFormat::ReportHtml,
        ] {
            let (content, _) = render_report(&inc, &reply, fmt, &opts(false, false, false, false));
            assert!(content
                .to_ascii_lowercase()
                .contains("insufficient evidence"));
        }
    }
}
