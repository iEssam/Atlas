//! Boot analysis via the Diagnostics-Performance event log (PRD §9.8.4,
//! docs/phases.md Phase 2).
//!
//! Windows logs a boot-performance summary (event ID 100) to the
//! `Microsoft-Windows-Diagnostics-Performance/Operational` channel on every
//! boot. We `EvtQuery` that channel for event 100 newest-first, then `EvtNext` +
//! `EvtRender` each event to XML and parse the timings out of its `EventData`:
//! `BootTime` (total, ms), `MainPathBootTime` (ms), and `BootPostBootTime` (ms),
//! plus the boot moment (`BootStartTime`, falling back to the event's
//! `TimeCreated`). Each record is flagged `degraded` when its total exceeds the
//! rolling median of the returned set by a margin.
//!
//! Read-only. The channel is often readable only with elevation or membership in
//! a diagnostics group; when the query fails the collector returns
//! `available = false` with an honest reason rather than fabricating boots.

#![cfg(windows)]

use std::ptr;

use crate::ffi::{
    EvtClose, EvtNext, EvtQuery, EvtRender, GetLastError, ERROR_ACCESS_DENIED,
    ERROR_EVT_CHANNEL_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, EVT_HANDLE, EVT_QUERY_CHANNEL_PATH,
    EVT_QUERY_REVERSE_DIRECTION, EVT_RENDER_EVENT_XML,
};

/// One boot's performance summary. Times are milliseconds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BootRecord {
    /// When the boot occurred (Unix-epoch ms).
    pub boot_ms: i64,
    /// Total boot time.
    pub boot_duration_ms: u32,
    /// Main-path boot time (to the desktop being usable).
    pub main_path_ms: u32,
    /// Post-boot time (background work after the desktop appears).
    pub post_boot_ms: u32,
    /// Flagged slower than the rolling baseline of the returned set.
    pub degraded: bool,
}

/// The result of a boot-analysis read: either the records (newest first) or an
/// honest unavailable reason.
#[derive(Debug, Clone, Default)]
pub struct BootAnalysis {
    pub available: bool,
    pub unavailable_reason: String,
    pub boots: Vec<BootRecord>,
}

/// The diagnostics-performance channel and the boot-event query.
const CHANNEL: &str = "Microsoft-Windows-Diagnostics-Performance/Operational";
const QUERY: &str = "*[System[(EventID=100)]]";

/// Reads up to `limit` boot records (0 = a sensible default of 30), newest
/// first. Degraded flags are computed over the returned set's median.
pub fn analyze_boots(limit: u32) -> BootAnalysis {
    let cap = if limit == 0 { 30 } else { limit as usize };

    let path = to_wide(CHANNEL);
    let query = to_wide(QUERY);
    // SAFETY: NULL session = local; path/query are NUL-terminated; reverse
    // direction yields newest-first.
    let results = unsafe {
        EvtQuery(
            ptr::null_mut(),
            path.as_ptr(),
            query.as_ptr(),
            EVT_QUERY_CHANNEL_PATH | EVT_QUERY_REVERSE_DIRECTION,
        )
    };
    if results.is_null() {
        // SAFETY: GetLastError has no preconditions.
        let err = unsafe { GetLastError() };
        return BootAnalysis {
            available: false,
            unavailable_reason: query_error_reason(err),
            boots: Vec::new(),
        };
    }
    let _results_guard = EvtGuard(results);

    let mut boots = Vec::new();
    // Fetch events one at a time (simpler than batching; the set is small).
    loop {
        if boots.len() >= cap {
            break;
        }
        let mut event: EVT_HANDLE = ptr::null_mut();
        let mut returned: u32 = 0;
        // SAFETY: `event`/`returned` are live out-params; INFINITE-ish 0 timeout
        // returns immediately when nothing is buffered.
        let ok = unsafe { EvtNext(results, 1, &mut event, 0, 0, &mut returned) };
        if ok == 0 || returned == 0 || event.is_null() {
            break; // ERROR_NO_MORE_ITEMS or empty — done
        }
        let ev = EvtGuard(event);
        if let Some(xml) = render_event(ev.0) {
            if let Some(rec) = parse_boot(&xml) {
                boots.push(rec);
            }
        }
    }

    if boots.is_empty() {
        return BootAnalysis {
            available: true,
            unavailable_reason: String::new(),
            boots: Vec::new(),
        };
    }

    mark_degraded(&mut boots);
    BootAnalysis {
        available: true,
        unavailable_reason: String::new(),
        boots,
    }
}

/// Maps an `EvtQuery` failure code to an honest reason string.
fn query_error_reason(err: u32) -> String {
    match err {
        ERROR_EVT_CHANNEL_NOT_FOUND => {
            "diagnostics-performance log unavailable (channel not found)".to_string()
        }
        ERROR_ACCESS_DENIED => {
            "diagnostics-performance log unavailable (access denied — try elevated)".to_string()
        }
        other => format!("diagnostics-performance log unavailable (error {other})"),
    }
}

/// Flags each record slower than 1.5× the median total boot time. The median is
/// a robust baseline against a couple of unusually slow or fast boots.
fn mark_degraded(boots: &mut [BootRecord]) {
    let median = median_duration(boots);
    if median == 0 {
        return;
    }
    let threshold = median.saturating_mul(3) / 2; // 1.5×
    for b in boots.iter_mut() {
        b.degraded = b.boot_duration_ms > threshold;
    }
}

/// Median of the (non-zero) total boot durations.
fn median_duration(boots: &[BootRecord]) -> u32 {
    let mut v: Vec<u32> = boots
        .iter()
        .map(|b| b.boot_duration_ms)
        .filter(|&d| d > 0)
        .collect();
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

/// Renders one event handle to its XML string (two-call size pattern).
fn render_event(event: EVT_HANDLE) -> Option<String> {
    let mut used: u32 = 0;
    let mut props: u32 = 0;
    // First call: probe the byte size.
    // SAFETY: null buffer probe; out-params live.
    unsafe {
        EvtRender(
            ptr::null_mut(),
            event,
            EVT_RENDER_EVENT_XML,
            0,
            ptr::null_mut(),
            &mut used,
            &mut props,
        );
    }
    if used == 0 {
        // Only a truly-empty render is unexpected; a too-small buffer sets `used`.
        // SAFETY: GetLastError has no preconditions.
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
    }
    // `used` is in bytes; the XML is UTF-16.
    let mut buf = vec![0u8; used as usize];
    let mut used2: u32 = 0;
    // SAFETY: buf sized to `used`; out-params live.
    let ok = unsafe {
        EvtRender(
            ptr::null_mut(),
            event,
            EVT_RENDER_EVENT_XML,
            buf.len() as u32,
            buf.as_mut_ptr() as *mut _,
            &mut used2,
            &mut props,
        )
    };
    if ok == 0 {
        return None;
    }
    // Decode the UTF-16LE buffer (trim a trailing NUL).
    let units: Vec<u16> = buf[..used2 as usize]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    Some(String::from_utf16_lossy(&units[..end]))
}

/// Parses one event-100 XML fragment into a [`BootRecord`]. Requires at least
/// the total boot time; returns `None` if the payload is not a boot summary.
fn parse_boot(xml: &str) -> Option<BootRecord> {
    let boot_duration_ms = data_u32(xml, "BootTime")?;
    let main_path_ms = data_u32(xml, "MainPathBootTime").unwrap_or(0);
    let post_boot_ms = data_u32(xml, "BootPostBootTime").unwrap_or(0);

    // Prefer the recorded boot start; fall back to when the event was logged.
    let boot_ms = data_text(xml, "BootStartTime")
        .and_then(|s| parse_iso8601_ms(&s))
        .or_else(|| attr_value(xml, "TimeCreated", "SystemTime").and_then(|s| parse_iso8601_ms(&s)))
        .unwrap_or(0);

    Some(BootRecord {
        boot_ms,
        boot_duration_ms,
        main_path_ms,
        post_boot_ms,
        degraded: false,
    })
}

// --- tiny XML helpers (namespace/attribute tolerant) ------------------------

/// Inner text of the first `<Data Name="name">value</Data>`.
fn data_text(xml: &str, name: &str) -> Option<String> {
    let needle = format!("Name=\"{name}\"");
    let at = xml.find(&needle)?;
    // The value starts after the next '>'.
    let rest = &xml[at..];
    let gt = rest.find('>')? + 1;
    let close = rest[gt..].find("</Data>")? + gt;
    Some(rest[gt..close].trim().to_string())
}

/// A `<Data Name="name">` value parsed as u32.
fn data_u32(xml: &str, name: &str) -> Option<u32> {
    data_text(xml, name)?.trim().parse::<u32>().ok()
}

/// The value of `attr` on the first `<elem ...>` element.
fn attr_value(xml: &str, elem: &str, attr: &str) -> Option<String> {
    let el = format!("<{elem}");
    let at = xml.find(&el)?;
    let rest = &xml[at..];
    let end = rest.find('>')?;
    let open = &rest[..end];
    let key = format!("{attr}=\"");
    let ka = open.find(&key)? + key.len();
    let kb = open[ka..].find('"')? + ka;
    Some(open[ka..kb].to_string())
}

/// Parses an ISO-8601 timestamp (`YYYY-MM-DDTHH:MM:SS[.fffffff][Z]`, always UTC
/// in these logs) to Unix-epoch milliseconds.
fn parse_iso8601_ms(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;

    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, b),
        None => (time, ""),
    };
    let mut t = hms.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.parse().ok()?;

    // Fractional seconds → milliseconds (first 3 digits, zero-padded).
    let mut millis = 0i64;
    if !frac.is_empty() {
        let mut ms_digits = String::new();
        for c in frac.chars().take(3) {
            if c.is_ascii_digit() {
                ms_digits.push(c);
            }
        }
        while ms_digits.len() < 3 {
            ms_digits.push('0');
        }
        millis = ms_digits.parse().unwrap_or(0);
    }

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + min * 60 + sec;
    Some(secs * 1000 + millis)
}

/// Days since the Unix epoch for a civil (proleptic Gregorian) date. Howard
/// Hinnant's `days_from_civil` algorithm — exact, no leap-year edge cases.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// UTF-16, NUL-terminated, for a `*const u16` Win32 argument.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An `EVT_HANDLE` closed on drop.
struct EvtGuard(EVT_HANDLE);

impl Drop for EvtGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: self.0 came from EvtQuery/EvtNext and is closed once.
            unsafe {
                EvtClose(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_to_unix_ms() {
        // 1970-01-01T00:00:00Z → 0.
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00Z"), Some(0));
        // 1970-01-02T00:00:00Z → one day.
        assert_eq!(parse_iso8601_ms("1970-01-02T00:00:00Z"), Some(86_400_000));
        // A boot event timestamp with 7-digit fraction (100 ns units).
        // 2021-01-01T00:00:00Z is 1609459200 s.
        assert_eq!(
            parse_iso8601_ms("2021-01-01T00:00:00.5000000Z"),
            Some(1_609_459_200_000 + 500)
        );
    }

    #[test]
    fn civil_days_reference_points() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2021, 1, 1), 18628);
    }

    #[test]
    fn parse_event_xml_extracts_timings() {
        let xml = r#"<Event xmlns="http://schemas.microsoft.com/win/2004/08/events/event">
            <System><EventID>100</EventID>
            <TimeCreated SystemTime="2026-07-13T21:04:11.1234567Z"/></System>
            <EventData>
              <Data Name="BootTsVersion">2</Data>
              <Data Name="BootStartTime">2026-07-13T21:03:00.0000000Z</Data>
              <Data Name="BootTime">45231</Data>
              <Data Name="MainPathBootTime">21000</Data>
              <Data Name="BootPostBootTime">24231</Data>
            </EventData></Event>"#;
        let rec = parse_boot(xml).expect("parsed");
        assert_eq!(rec.boot_duration_ms, 45231);
        assert_eq!(rec.main_path_ms, 21000);
        assert_eq!(rec.post_boot_ms, 24231);
        // boot_ms from BootStartTime (21:03:00), not TimeCreated (21:04:11).
        assert_eq!(
            rec.boot_ms,
            parse_iso8601_ms("2026-07-13T21:03:00Z").unwrap()
        );
    }

    #[test]
    fn parse_falls_back_to_time_created() {
        let xml = r#"<Event><System>
            <TimeCreated SystemTime="2026-07-13T21:04:11.0000000Z"/></System>
            <EventData><Data Name="BootTime">30000</Data></EventData></Event>"#;
        let rec = parse_boot(xml).expect("parsed");
        assert_eq!(rec.boot_duration_ms, 30000);
        assert_eq!(rec.main_path_ms, 0);
        assert_eq!(
            rec.boot_ms,
            parse_iso8601_ms("2026-07-13T21:04:11Z").unwrap()
        );
    }

    #[test]
    fn no_boot_time_is_rejected() {
        let xml = r#"<Event><EventData><Data Name="Other">1</Data></EventData></Event>"#;
        assert!(parse_boot(xml).is_none());
    }

    #[test]
    fn degraded_flag_uses_median() {
        let mut boots = vec![
            BootRecord {
                boot_duration_ms: 20_000,
                ..Default::default()
            },
            BootRecord {
                boot_duration_ms: 22_000,
                ..Default::default()
            },
            BootRecord {
                boot_duration_ms: 21_000,
                ..Default::default()
            },
            // Well over 1.5× the ~21s median → degraded.
            BootRecord {
                boot_duration_ms: 60_000,
                ..Default::default()
            },
        ];
        mark_degraded(&mut boots);
        assert!(!boots[0].degraded);
        assert!(!boots[1].degraded);
        assert!(!boots[2].degraded);
        assert!(boots[3].degraded);
    }
}
