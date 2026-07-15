//! Crash / reliability reader via the Windows Event Log (PRD §9.14,
//! docs/phases.md Phase 3).
//!
//! Windows records crash-class events across two channels: the `Application`
//! log (application faults, hangs, and Windows Error Reporting buckets) and the
//! `System` log (service-control failures, bug checks, and unexpected power
//! loss). We reuse the same `wevtapi` pattern as the boot collector — `EvtQuery`
//! a channel with a provider/EventID XPath filter, newest-first, then `EvtNext`
//! + `EvtRender` each event to XML and pull the fields out of its `EventData`.
//!
//! Each source is best-effort. The whole scan reports `available = false` only
//! when *none* of the primary channels can be opened (e.g. access denied); if at
//! least one channel opens we return `available = true` with whatever was
//! gathered (possibly empty) rather than fabricating crashes.
//!
//! Read-only: no channel is ever opened for write and no event is cleared.

#![cfg(windows)]

use std::ptr;

use crate::ffi::{
    EvtClose, EvtNext, EvtQuery, EvtRender, GetLastError, ERROR_ACCESS_DENIED,
    ERROR_EVT_CHANNEL_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, EVT_HANDLE, EVT_QUERY_CHANNEL_PATH,
    EVT_QUERY_REVERSE_DIRECTION, EVT_RENDER_EVENT_XML,
};

/// `CrashKind` discriminants, matching the FROZEN proto `atlas.v0` enum.
pub mod crash_kind {
    pub const APP_CRASH: i32 = 1;
    pub const APP_HANG: i32 = 2;
    pub const BUGCHECK: i32 = 3;
    pub const SERVICE_FAILURE: i32 = 4;
    pub const UNEXPECTED_SHUTDOWN: i32 = 5;
}

/// One raw crash/reliability event read from the logs (pre-correlation). Maps to
/// proto `CrashRecord` minus `id` and the service-assembled `context`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCrash {
    /// A [`crash_kind`] value.
    pub kind: i32,
    /// Event time, Unix-epoch milliseconds.
    pub ts_ms: i64,
    /// Faulting app exe / service name / "BugCheck" etc.
    pub subject: String,
    /// Faulting module / bugcheck code / stop-code; may be "".
    pub fault: String,
    /// e.g. "0xc0000005"; may be "".
    pub exception_code: String,
}

/// Result of a crash scan with honest availability (proto `ListCrashesReply`
/// shape).
#[derive(Debug, Clone)]
pub struct CrashScan {
    pub available: bool,
    /// Set only when `available == false`.
    pub unavailable_reason: String,
    pub crashes: Vec<RawCrash>,
}

// --- channel / XPath queries -------------------------------------------------

const CH_APPLICATION: &str = "Application";
const CH_SYSTEM: &str = "System";

/// "Application Error" 1000 — a process fault (subject = faulting app,
/// fault = faulting module, exception code from the payload).
const XPATH_APP_ERROR: &str = "*[System[(Provider[@Name='Application Error']) and (EventID=1000)]]";
/// "Application Hang" 1002 — a UI hang (subject = hanging app).
const XPATH_APP_HANG: &str = "*[System[(Provider[@Name='Application Hang']) and (EventID=1002)]]";
/// "Windows Error Reporting" 1001 — a WER bucket (kept light; helps when the
/// matching 1000 is absent). Included as `APP_CRASH`; the service de-dups by
/// (subject, ts within ~5s) if needed — we do not de-dup here.
const XPATH_WER: &str = "*[System[(Provider[@Name='Windows Error Reporting']) and (EventID=1001)]]";
/// Service Control Manager 7031/7034 — a service terminated unexpectedly.
const XPATH_SCM: &str =
    "*[System[(Provider[@Name='Service Control Manager']) and (EventID=7031 or EventID=7034)]]";
/// WER-SystemErrorReporting 1001 — a bug check (BSOD) summary.
const XPATH_BUGCHECK: &str =
    "*[System[(Provider[@Name='Microsoft-Windows-WER-SystemErrorReporting']) and (EventID=1001)]]";
/// Kernel-Power 41 — the system rebooted without a clean shutdown.
const XPATH_KERNEL_POWER: &str =
    "*[System[(Provider[@Name='Microsoft-Windows-Kernel-Power']) and (EventID=41)]]";

/// A sensible default cap when the caller passes `max == 0`.
const DEFAULT_MAX: usize = 200;

/// One reliability-event source: (channel, XPath filter, event→`RawCrash` mapper).
type CrashSource = (&'static str, &'static str, fn(&str) -> Option<RawCrash>);

/// Reads crash/reliability events at or after `since_ms`, newest first, up to
/// `max` total (0 = a default of 200). See the module docs for the source list.
/// Returns `available = false` with an honest reason only when *both* primary
/// channels fail to open.
pub fn read_crashes(since_ms: i64, max: usize) -> CrashScan {
    let cap = if max == 0 { DEFAULT_MAX } else { max };

    // (channel, xpath, mapper). Application channel first, then System.
    let sources: [CrashSource; 6] = [
        (CH_APPLICATION, XPATH_APP_ERROR, map_app_error),
        (CH_APPLICATION, XPATH_APP_HANG, map_app_hang),
        (CH_APPLICATION, XPATH_WER, map_wer),
        (CH_SYSTEM, XPATH_SCM, map_scm),
        (CH_SYSTEM, XPATH_BUGCHECK, map_bugcheck),
        (CH_SYSTEM, XPATH_KERNEL_POWER, map_kernel_power),
    ];

    let mut all: Vec<RawCrash> = Vec::new();
    let mut any_ok = false;
    let mut first_err: Option<u32> = None;

    for (channel, xpath, map) in sources {
        match scan_channel(channel, xpath, since_ms, cap, map) {
            Ok(rows) => {
                any_ok = true;
                all.extend(rows);
            }
            Err(err) => {
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
    }

    if !any_ok {
        return CrashScan {
            available: false,
            unavailable_reason: query_error_reason(first_err.unwrap_or(0)),
            crashes: Vec::new(),
        };
    }

    // Merge: newest first, then truncate to the requested total.
    all.sort_by_key(|c| std::cmp::Reverse(c.ts_ms));
    all.truncate(cap);

    CrashScan {
        available: true,
        unavailable_reason: String::new(),
        crashes: all,
    }
}

/// Queries `channel` with `xpath` newest-first, mapping each event's XML with
/// `map`. Stops early once an event older than `since_ms` is seen (reverse
/// direction = newest first) or `cap` rows are collected. Returns
/// `Err(win32_err)` only when `EvtQuery` itself fails.
fn scan_channel<F>(
    channel: &str,
    xpath: &str,
    since_ms: i64,
    cap: usize,
    map: F,
) -> Result<Vec<RawCrash>, u32>
where
    F: Fn(&str) -> Option<RawCrash>,
{
    let path = to_wide(channel);
    let query = to_wide(xpath);
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
        return Err(unsafe { GetLastError() });
    }
    let _results_guard = EvtGuard(results);

    let mut rows = Vec::new();
    loop {
        if rows.len() >= cap {
            break;
        }
        let mut event: EVT_HANDLE = ptr::null_mut();
        let mut returned: u32 = 0;
        // SAFETY: `event`/`returned` are live out-params; 0 timeout returns
        // immediately when nothing more is buffered.
        let ok = unsafe { EvtNext(results, 1, &mut event, 0, 0, &mut returned) };
        if ok == 0 || returned == 0 || event.is_null() {
            break; // ERROR_NO_MORE_ITEMS or empty — done
        }
        let ev = EvtGuard(event);
        if let Some(xml) = render_event(ev.0) {
            if let Some(rec) = map(&xml) {
                // Reverse direction = newest first: the first older-than-window
                // event means everything after it is older too.
                if rec.ts_ms < since_ms {
                    break;
                }
                rows.push(rec);
            }
        }
    }
    Ok(rows)
}

/// Maps an `EvtQuery` failure code to an honest reason string.
fn query_error_reason(err: u32) -> String {
    match err {
        ERROR_EVT_CHANNEL_NOT_FOUND => {
            "reliability/WER log unavailable (channel not found)".to_string()
        }
        ERROR_ACCESS_DENIED => {
            "reliability/WER log unavailable (access denied — try elevated)".to_string()
        }
        other => format!("reliability/WER log unavailable (error {other})"),
    }
}

// --- per-source mappers ------------------------------------------------------

/// "Application Error" 1000: unnamed positional `<Data>` list —
/// [0]=app name, [1]=app version, [2]=app timestamp, [3]=faulting module,
/// [4]=module version, [5]=module timestamp, [6]=exception code, [7]=offset, …
fn map_app_error(xml: &str) -> Option<RawCrash> {
    let ts_ms = event_ts(xml)?;
    let subject = nth_data(xml, 0).unwrap_or_default();
    let fault = nth_data(xml, 3).unwrap_or_default();
    let exception_code = nth_data(xml, 6)
        .map(|c| normalize_exception_code(&c))
        .unwrap_or_default();
    Some(RawCrash {
        kind: crash_kind::APP_CRASH,
        ts_ms,
        subject,
        fault,
        exception_code,
    })
}

/// "Application Hang" 1002: Data[0] = app name.
fn map_app_hang(xml: &str) -> Option<RawCrash> {
    let ts_ms = event_ts(xml)?;
    let subject = nth_data(xml, 0).unwrap_or_default();
    Some(RawCrash {
        kind: crash_kind::APP_HANG,
        ts_ms,
        subject,
        fault: "hang".to_string(),
        exception_code: String::new(),
    })
}

/// "Windows Error Reporting" 1001: named `<Data Name='…'>`. Prefer AppName, then
/// the event/bucket name for the subject; keep the bucket as the fault.
fn map_wer(xml: &str) -> Option<RawCrash> {
    let ts_ms = event_ts(xml)?;
    let subject = named_data(xml, "AppName")
        .or_else(|| named_data(xml, "EventName"))
        .or_else(|| named_data(xml, "Bucket"))
        .or_else(|| nth_data(xml, 0))
        .unwrap_or_default();
    let fault = named_data(xml, "Bucket")
        .or_else(|| named_data(xml, "EventName"))
        .unwrap_or_default();
    Some(RawCrash {
        kind: crash_kind::APP_CRASH,
        ts_ms,
        subject,
        fault,
        exception_code: String::new(),
    })
}

/// Service Control Manager 7031/7034: `<Data Name='param1'>` (or positional [0])
/// = the service display name.
fn map_scm(xml: &str) -> Option<RawCrash> {
    let ts_ms = event_ts(xml)?;
    let subject = named_data(xml, "param1")
        .or_else(|| nth_data(xml, 0))
        .unwrap_or_default();
    Some(RawCrash {
        kind: crash_kind::SERVICE_FAILURE,
        ts_ms,
        subject,
        fault: "unexpected service termination".to_string(),
        exception_code: String::new(),
    })
}

/// WER-SystemErrorReporting 1001 (bug check): positional [0] = the bugcheck code
/// + parameters string.
fn map_bugcheck(xml: &str) -> Option<RawCrash> {
    let ts_ms = event_ts(xml)?;
    let fault = nth_data(xml, 0).unwrap_or_default();
    Some(RawCrash {
        kind: crash_kind::BUGCHECK,
        ts_ms,
        subject: "BugCheck".to_string(),
        fault,
        exception_code: String::new(),
    })
}

/// Kernel-Power 41 (unexpected shutdown): the bugcheck code, when present, is the
/// fault.
fn map_kernel_power(xml: &str) -> Option<RawCrash> {
    let ts_ms = event_ts(xml)?;
    let fault = named_data(xml, "BugcheckCode").unwrap_or_default();
    Some(RawCrash {
        kind: crash_kind::UNEXPECTED_SHUTDOWN,
        ts_ms,
        subject: "Unexpected shutdown".to_string(),
        fault,
        exception_code: String::new(),
    })
}

/// The event's `TimeCreated SystemTime` attribute as Unix-epoch ms, or `None`
/// when it is missing/unparseable (the event is then skipped).
fn event_ts(xml: &str) -> Option<i64> {
    attr_value(xml, "TimeCreated", "SystemTime").and_then(|s| parse_iso8601_ms(&s))
}

// --- correlation helpers (PURE) ---------------------------------------------

/// Counts how many crashes in `crashes` share `subject` (case-insensitive) with
/// an event time inside the window ending at `at_ms`:
/// `ts <= at_ms && ts >= at_ms - window_ms`. Used for repeated-restart
/// detection.
pub fn count_repeated_restarts(
    crashes: &[RawCrash],
    subject: &str,
    at_ms: i64,
    window_ms: i64,
) -> usize {
    let lo = at_ms - window_ms;
    crashes
        .iter()
        .filter(|c| c.subject.eq_ignore_ascii_case(subject) && c.ts_ms <= at_ms && c.ts_ms >= lo)
        .count()
}

/// Given recent changes as `(ts_ms, kind_label, subject)` and a crash time,
/// returns hedged, factual note strings for each change within `window_ms`
/// *before* the crash (`change_ts <= crash_ms && change_ts >= crash_ms -
/// window_ms`), most-recent-first, e.g.
/// `'Foo' app_updated 2h before this crash (correlation, not proof)`.
pub fn recent_change_notes(
    changes: &[(i64, String, String)],
    crash_ms: i64,
    window_ms: i64,
) -> Vec<String> {
    let lo = crash_ms - window_ms;
    let mut hits: Vec<&(i64, String, String)> = changes
        .iter()
        .filter(|(ts, _, _)| *ts <= crash_ms && *ts >= lo)
        .collect();
    // Most recent first.
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    hits.iter()
        .map(|(ts, label, subject)| {
            let gap = humanize_gap(crash_ms - *ts);
            format!("'{subject}' {label} {gap} before this crash (correlation, not proof)")
        })
        .collect()
}

/// Formats a non-negative millisecond gap with its largest whole unit:
/// `"3d"`, `"2h"`, `"35m"`, `"12s"`.
fn humanize_gap(ms: i64) -> String {
    let secs = (ms / 1000).max(0);
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Prefixes a bare hex-looking exception code with `0x`; leaves already-prefixed
/// or non-hex strings untouched. Empty in → empty out.
fn normalize_exception_code(code: &str) -> String {
    let c = code.trim();
    if c.is_empty() {
        return String::new();
    }
    if c.starts_with("0x") || c.starts_with("0X") {
        return c.to_string();
    }
    if c.chars().all(|ch| ch.is_ascii_hexdigit()) {
        format!("0x{c}")
    } else {
        c.to_string()
    }
}

// --- tiny XML helpers (namespace/attribute tolerant) ------------------------

/// Inner text of the `n`-th `<Data>` element (0-based), regardless of whether it
/// carries a `Name` attribute. Self-closing `<Data/>` counts as an empty slot.
fn nth_data(xml: &str, n: usize) -> Option<String> {
    let mut idx = 0usize;
    let mut pos = 0usize;
    loop {
        let rel = xml[pos..].find("<Data")?;
        let start = pos + rel;
        // Ensure this is a `<Data>`/`<Data …>` tag, not `<DataItem>` etc.
        let boundary = xml.as_bytes().get(start + 5).copied();
        match boundary {
            Some(b'>') | Some(b' ') | Some(b'/') | Some(b'\t') | Some(b'\r') | Some(b'\n') => {}
            _ => {
                pos = start + 5;
                continue;
            }
        }
        let gt = xml[start..].find('>')? + start;
        let open_end = gt + 1;
        // Self-closing element: an empty positional slot.
        if xml[start..open_end].ends_with("/>") {
            if idx == n {
                return Some(String::new());
            }
            idx += 1;
            pos = open_end;
            continue;
        }
        let close = xml[open_end..].find("</Data>")? + open_end;
        if idx == n {
            return Some(xml[open_end..close].trim().to_string());
        }
        idx += 1;
        pos = close + "</Data>".len();
    }
}

/// Inner text of the first `<Data Name='name'>value</Data>`. Tolerates single- or
/// double-quoted `Name` attributes — `EvtRender` emits single quotes, while the
/// unit tests use double quotes.
fn named_data(xml: &str, name: &str) -> Option<String> {
    let at = xml
        .find(&format!("Name='{name}'"))
        .or_else(|| xml.find(&format!("Name=\"{name}\"")))?;
    let rest = &xml[at..];
    let gt = rest.find('>')? + 1;
    let close = rest[gt..].find("</Data>")? + gt;
    Some(rest[gt..close].trim().to_string())
}

/// The value of `attr` on the first `<elem …>` element. Tolerates single- or
/// double-quoted attribute values — `EvtRender` emits single quotes (e.g.
/// `SystemTime='…'`), while the unit tests use double quotes.
fn attr_value(xml: &str, elem: &str, attr: &str) -> Option<String> {
    let el = format!("<{elem}");
    let at = xml.find(&el)?;
    let rest = &xml[at..];
    let end = rest.find('>')?;
    let open = &rest[..end];
    let key = format!("{attr}=");
    let ka = open.find(&key)? + key.len();
    // The delimiter is whichever quote follows `attr=`.
    let quote = open.as_bytes().get(ka).copied()?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let val_start = ka + 1;
    let kb = open[val_start..].find(quote as char)? + val_start;
    Some(open[val_start..kb].to_string())
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
        // A too-small buffer sets `used`; only a truly-empty render is unexpected.
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

    fn crash(subject: &str, ts_ms: i64) -> RawCrash {
        RawCrash {
            kind: crash_kind::APP_CRASH,
            ts_ms,
            subject: subject.to_string(),
            fault: String::new(),
            exception_code: String::new(),
        }
    }

    #[test]
    fn repeated_restarts_none() {
        let crashes = [crash("other.exe", 1_000)];
        assert_eq!(
            count_repeated_restarts(&crashes, "app.exe", 10_000, 5_000),
            0
        );
    }

    #[test]
    fn repeated_restarts_counts_in_window_case_insensitively() {
        let crashes = [
            crash("App.exe", 9_000),
            crash("APP.EXE", 8_000),
            crash("app.exe", 7_000),
            crash("other.exe", 8_500),
        ];
        // Window [5000, 10000]: all three app.exe variants, not other.exe.
        assert_eq!(
            count_repeated_restarts(&crashes, "app.exe", 10_000, 5_000),
            3
        );
    }

    #[test]
    fn repeated_restarts_excludes_outside_window_and_after_at() {
        let crashes = [
            crash("app.exe", 4_999),  // just before the window start → excluded
            crash("app.exe", 5_000),  // exactly at at_ms - window_ms → included
            crash("app.exe", 10_000), // exactly at at_ms → included
            crash("app.exe", 10_001), // after at_ms → excluded
        ];
        assert_eq!(
            count_repeated_restarts(&crashes, "app.exe", 10_000, 5_000),
            2
        );
    }

    #[test]
    fn change_notes_only_within_window_most_recent_first() {
        let changes = vec![
            (1_000i64, "app_installed".to_string(), "Old".to_string()), // too old
            (6_000, "app_updated".to_string(), "Foo".to_string()),
            (9_000, "driver_updated".to_string(), "Bar".to_string()),
            (11_000, "app_updated".to_string(), "After".to_string()), // after crash
        ];
        let notes = recent_change_notes(&changes, 10_000, 5_000);
        assert_eq!(notes.len(), 2);
        // Most recent (9_000, gap 1s) first, then (6_000, gap 4s).
        assert!(notes[0].starts_with("'Bar' driver_updated 1s before this crash"));
        assert!(notes[1].starts_with("'Foo' app_updated 4s before this crash"));
    }

    #[test]
    fn change_notes_are_hedged() {
        let changes = vec![(9_500i64, "app_updated".to_string(), "Foo".to_string())];
        let notes = recent_change_notes(&changes, 10_000, 5_000);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("correlation"));
        assert!(notes[0].contains("not proof"));
    }

    #[test]
    fn humanize_gap_units() {
        assert_eq!(humanize_gap(12_000), "12s");
        assert_eq!(humanize_gap(35 * 60_000), "35m");
        assert_eq!(humanize_gap(2 * 3_600_000), "2h");
        assert_eq!(humanize_gap(3 * 86_400_000), "3d");
        // Sub-second and negative gaps clamp to "0s".
        assert_eq!(humanize_gap(400), "0s");
        assert_eq!(humanize_gap(-5), "0s");
    }

    #[test]
    fn nth_data_reads_positional_unnamed_data() {
        let xml = r#"<Event><EventData>
            <Data>app.exe</Data>
            <Data>1.2.3.4</Data>
            <Data>0a1b2c3d</Data>
            <Data>mod.dll</Data>
            <Data>4.5.6.7</Data>
            <Data>8e9f0000</Data>
            <Data>c0000005</Data>
            <Data>00007ff0</Data>
        </EventData></Event>"#;
        assert_eq!(nth_data(xml, 0).as_deref(), Some("app.exe"));
        assert_eq!(nth_data(xml, 3).as_deref(), Some("mod.dll"));
        assert_eq!(nth_data(xml, 6).as_deref(), Some("c0000005"));
        assert_eq!(nth_data(xml, 8), None);
    }

    #[test]
    fn nth_data_handles_self_closing_slot() {
        let xml = r#"<EventData><Data>first</Data><Data/><Data>third</Data></EventData>"#;
        assert_eq!(nth_data(xml, 0).as_deref(), Some("first"));
        assert_eq!(nth_data(xml, 1).as_deref(), Some(""));
        assert_eq!(nth_data(xml, 2).as_deref(), Some("third"));
    }

    #[test]
    fn named_data_reads_by_name() {
        let xml = r#"<EventData>
            <Data Name="AppName">contoso.exe</Data>
            <Data Name="Bucket">12345</Data>
        </EventData>"#;
        assert_eq!(named_data(xml, "AppName").as_deref(), Some("contoso.exe"));
        assert_eq!(named_data(xml, "Bucket").as_deref(), Some("12345"));
        assert_eq!(named_data(xml, "Missing"), None);
    }

    #[test]
    fn normalize_exception_code_prefixes_hex() {
        assert_eq!(normalize_exception_code("c0000005"), "0xc0000005");
        assert_eq!(normalize_exception_code("0xC0000005"), "0xC0000005");
        assert_eq!(normalize_exception_code(""), "");
        // Non-hex text is left untouched.
        assert_eq!(normalize_exception_code("N/A"), "N/A");
    }

    #[test]
    fn event_ts_parses_time_created() {
        let xml = r#"<Event><System>
            <TimeCreated SystemTime="2026-07-13T21:04:11.0000000Z"/>
            </System></Event>"#;
        assert_eq!(event_ts(xml), parse_iso8601_ms("2026-07-13T21:04:11Z"));
    }

    // Regression: `EvtRender` emits attributes with SINGLE quotes
    // (`SystemTime='…'`, `Name='AppName'`). The parsers must accept both so real
    // event XML is read, not just the double-quoted test fixtures.
    #[test]
    fn parsers_tolerate_single_quoted_evtrender_xml() {
        let xml = "<Event><System>\
            <TimeCreated SystemTime='2026-07-13T21:04:11.0000000Z'/></System>\
            <EventData>\
            <Data Name='AppName'>contoso.exe</Data>\
            <Data Name='FaultingModuleName'>mod.dll</Data>\
            <Data Name='Bucket'>12345</Data>\
            </EventData></Event>";
        assert_eq!(event_ts(xml), parse_iso8601_ms("2026-07-13T21:04:11Z"));
        assert_eq!(named_data(xml, "AppName").as_deref(), Some("contoso.exe"));
        assert_eq!(named_data(xml, "Bucket").as_deref(), Some("12345"));
        // Positional access still works over named, single-quoted elements.
        assert_eq!(nth_data(xml, 0).as_deref(), Some("contoso.exe"));
        assert_eq!(nth_data(xml, 1).as_deref(), Some("mod.dll"));
    }
}
