//! Pure output helpers: a tiny column-aligned table builder, byte/percent
//! humanizers, and open-enum → readable-name mappings.
//!
//! Everything here is pure (no I/O, no RPC) so the formatting is unit-testable
//! against fixtures with no live server.

use atlas_ipc::{
    Confidence, IncidentKind, L4Protocol, PriorityClass, ProcessRole, ServiceStartType,
    ServiceState, Severity, TcpState,
};

/// A minimal left-aligned, space-padded text table. Numeric columns are passed
/// as pre-formatted strings by the caller, so alignment is purely by width.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|h| h.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    /// Renders the table with two-space gutters between columns. The final
    /// column is not right-padded (keeps trailing whitespace out of output).
    pub fn render(&self) -> String {
        let ncols = self.headers.len();
        let mut widths = vec![0usize; ncols];
        for (i, h) in self.headers.iter().enumerate() {
            widths[i] = h.chars().count();
        }
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(ncols) {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }

        let mut out = String::new();
        render_row(&mut out, &self.headers, &widths);
        // Separator rule under the header.
        let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
        render_row(&mut out, &sep, &widths);
        for row in &self.rows {
            render_row(&mut out, row, &widths);
        }
        out
    }
}

fn render_row(out: &mut String, cells: &[String], widths: &[usize]) {
    let last = cells.len().saturating_sub(1);
    for (i, cell) in cells.iter().enumerate() {
        if i == last {
            out.push_str(cell);
        } else {
            let pad = widths[i].saturating_sub(cell.chars().count());
            out.push_str(cell);
            out.push_str(&" ".repeat(pad));
            out.push_str("  ");
        }
    }
    out.push('\n');
}

/// Humanizes a byte count to a compact unit (KiB/MiB/GiB), matching the app's
/// binary-unit convention.
pub fn bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

/// Formats a per-mille CPU share (0..=1000) as a percentage with one decimal.
pub fn permille_pct(permille: u32) -> String {
    format!("{:.1}%", permille as f64 / 10.0)
}

/// Formats an epoch-ms timestamp; 0 renders as "-" (never-run / unbounded).
pub fn ts(ms: i64) -> String {
    if ms == 0 {
        "-".to_string()
    } else {
        ms.to_string()
    }
}

// --- open-enum → readable name (i32 arrives on the wire) --------------------

pub fn role_name(v: i32) -> &'static str {
    ProcessRole::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("PROCESS_ROLE_UNSPECIFIED")
}
pub fn incident_kind_name(v: i32) -> &'static str {
    IncidentKind::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("INCIDENT_KIND_UNSPECIFIED")
}
pub fn severity_name(v: i32) -> &'static str {
    Severity::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("SEVERITY_UNSPECIFIED")
}
pub fn confidence_name(v: i32) -> &'static str {
    Confidence::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("CONFIDENCE_UNSPECIFIED")
}
pub fn service_state_name(v: i32) -> &'static str {
    ServiceState::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("SERVICE_STATE_UNSPECIFIED")
}
pub fn start_type_name(v: i32) -> &'static str {
    ServiceStartType::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("SERVICE_START_TYPE_UNSPECIFIED")
}
pub fn l4_name(v: i32) -> &'static str {
    L4Protocol::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("L4_PROTOCOL_UNSPECIFIED")
}
pub fn tcp_state_name(v: i32) -> &'static str {
    TcpState::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("TCP_STATE_UNSPECIFIED")
}
pub fn priority_name(v: i32) -> &'static str {
    PriorityClass::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("PRIORITY_CLASS_UNSPECIFIED")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_aligns_columns_and_has_rule() {
        let mut t = Table::new(&["PID", "NAME"]);
        t.push(vec!["1".into(), "system".into()]);
        t.push(vec!["1234".into(), "chrome.exe".into()]);
        let out = t.render();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "PID   NAME");
        assert_eq!(lines[1], "----  ----------");
        assert_eq!(lines[2], "1     system");
        assert_eq!(lines[3], "1234  chrome.exe");
    }

    #[test]
    fn bytes_humanizes_units() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn permille_and_ts_format() {
        assert_eq!(permille_pct(125), "12.5%");
        assert_eq!(permille_pct(1000), "100.0%");
        assert_eq!(ts(0), "-");
        assert_eq!(ts(42), "42");
    }
}
