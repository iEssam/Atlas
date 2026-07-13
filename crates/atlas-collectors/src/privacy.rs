//! Privacy-capability usage from the Windows CapabilityAccessManager
//! ConsentStore (PRD §9.10, docs/phases.md M7).
//!
//! This is a **point-in-time read** of what Settings › Privacy shows: for each
//! app that has used the camera / microphone / location capability, Windows
//! records `LastUsedTimeStart` and `LastUsedTimeStop` (FILETIMEs) under the
//! ConsentStore. We read both the machine (HKLM) and user (HKCU) hives, both the
//! packaged apps (subkeys named by package-family moniker) and the `NonPackaged`
//! subtree (subkeys named by a `#`-escaped desktop-exe path).
//!
//! An app is *in use now* when it has a start with no matching stop
//! (`stop == 0`, or a stop that predates the start — a race we treat as still
//! in use). Continuous change-watching (RegNotify → recorded history) is a
//! separate nice-to-have handled by the service; this module is the current
//! snapshot only.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::ffi::{
    CloseDesktop, CloseHandle, CreateEventW, GetUserObjectInformationW, OpenInputDesktop,
    WaitForMultipleObjects, DWORD, HANDLE, HDESK, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, UOI_NAME,
    WAIT_FAILED, WAIT_TIMEOUT,
};
use crate::reg::{RegKey, RegValue};

/// Which privacy-sensitive capability a usage row is about. Mirrors the proto
/// `CapabilityKind`; the service maps this to the wire enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Camera,
    Microphone,
    Location,
}

impl Capability {
    /// The ConsentStore subkey name for this capability.
    fn store_key(self) -> &'static str {
        match self {
            Capability::Camera => "webcam",
            Capability::Microphone => "microphone",
            Capability::Location => "location",
        }
    }

    /// All three capabilities, in a stable order.
    pub fn all() -> [Capability; 3] {
        [
            Capability::Camera,
            Capability::Microphone,
            Capability::Location,
        ]
    }
}

/// One `(app, capability)` usage row read from the ConsentStore.
#[derive(Debug, Clone)]
pub struct PrivacyUsage {
    pub capability: Capability,
    /// The raw ConsentStore key (package-family moniker or the `#`-escaped
    /// desktop path), preserved for correlation.
    pub app_id: String,
    /// A friendly display name derived from `app_id` (unmunged exe path, or the
    /// package family name).
    pub display_name: String,
    /// Whether this came from the packaged subtree (vs. NonPackaged desktop).
    pub packaged: bool,
    /// Last start, Unix-epoch ms (0 when never recorded).
    pub last_start_ms: i64,
    /// Last stop, Unix-epoch ms (0 when currently in use / never recorded).
    pub last_stop_ms: i64,
    /// True when a start has no matching stop.
    pub in_use: bool,
}

/// Root of the ConsentStore under either hive.
const CONSENT_ROOT: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";

/// FILETIME epoch (1601-01-01) to Unix epoch (1970-01-01) in 100 ns ticks.
const FILETIME_UNIX_DELTA_100NS: i64 = 116_444_736_000_000_000;

/// Converts a FILETIME tick count (100 ns since 1601) to Unix-epoch ms. Returns
/// 0 for a zero/near-zero FILETIME (Windows stores 0 for "never").
fn filetime_to_unix_ms(ticks: u64) -> i64 {
    if ticks == 0 {
        return 0;
    }
    let unix_100ns = ticks as i64 - FILETIME_UNIX_DELTA_100NS;
    if unix_100ns <= 0 {
        return 0;
    }
    unix_100ns / 10_000
}

/// Whether the app is in use, given its start/stop FILETIME ticks. In use when a
/// start is present and either there is no stop or the stop predates the start.
fn compute_in_use(start_ticks: u64, stop_ticks: u64) -> bool {
    start_ticks != 0 && (stop_ticks == 0 || stop_ticks < start_ticks)
}

/// Derives a friendly display name from a ConsentStore app moniker.
///
/// - Packaged apps are keyed by their package-family moniker (contains `_`);
///   we surface the family name up to the first `_` (the human-recognisable
///   part, e.g. `Microsoft.WindowsCamera`).
/// - NonPackaged desktop apps are keyed by a `#`-escaped full path where `#`
///   stands in for the path separator (`C:#Program Files#App#app.exe`); we
///   unmunge it back to a real path and take the file name.
pub fn display_name_from_moniker(moniker: &str, packaged: bool) -> String {
    if packaged {
        // Package family name is `<name>_<publisherhash>`; the leading segment
        // before the first underscore is the recognisable family name.
        match moniker.split_once('_') {
            Some((family, _)) if !family.is_empty() => family.to_string(),
            _ => moniker.to_string(),
        }
    } else {
        let path = unmunge_nonpackaged(moniker);
        // Take the final path component as the display name.
        path.rsplit(['\\', '/'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&path)
            .to_string()
    }
}

/// Unmunges a NonPackaged ConsentStore moniker back into a real path. Windows
/// escapes the real exe path by replacing `\` with `#`; a literal `#` in the
/// path is itself doubled (`##`). We reverse both: `##` → `#`, lone `#` → `\`.
pub fn unmunge_nonpackaged(moniker: &str) -> String {
    let mut out = String::with_capacity(moniker.len());
    let mut chars = moniker.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' {
            if chars.peek() == Some(&'#') {
                out.push('#');
                chars.next();
            } else {
                out.push('\\');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Reads one capability's usage rows from a single hive root key (already opened
/// at the ConsentStore, e.g. `<root>\webcam`). Appends both packaged apps
/// (direct subkeys) and NonPackaged desktop apps (`NonPackaged\<moniker>`).
fn read_capability(cap_key: &RegKey, capability: Capability, out: &mut Vec<PrivacyUsage>) {
    // Direct subkeys = packaged apps. `NonPackaged` is a container, handled
    // separately below.
    for name in cap_key.subkey_names() {
        if name.eq_ignore_ascii_case("NonPackaged") {
            continue;
        }
        if let Some(app_key) = cap_key.open_subkey(&name) {
            if let Some(usage) = usage_from_app_key(&app_key, capability, &name, true) {
                out.push(usage);
            }
        }
    }
    // NonPackaged desktop apps.
    if let Some(np) = cap_key.open_subkey("NonPackaged") {
        for name in np.subkey_names() {
            if let Some(app_key) = np.open_subkey(&name) {
                if let Some(usage) = usage_from_app_key(&app_key, capability, &name, false) {
                    out.push(usage);
                }
            }
        }
    }
}

/// Builds a [`PrivacyUsage`] from an app subkey by reading its start/stop
/// FILETIMEs. Returns `None` when the key has neither timestamp (an app that was
/// granted the capability but never exercised it — nothing to show).
fn usage_from_app_key(
    app_key: &RegKey,
    capability: Capability,
    moniker: &str,
    packaged: bool,
) -> Option<PrivacyUsage> {
    let start_ticks = read_filetime_value(app_key, "LastUsedTimeStart");
    let stop_ticks = read_filetime_value(app_key, "LastUsedTimeStop");
    if start_ticks == 0 && stop_ticks == 0 {
        return None;
    }
    Some(PrivacyUsage {
        capability,
        app_id: moniker.to_string(),
        display_name: display_name_from_moniker(moniker, packaged),
        packaged,
        last_start_ms: filetime_to_unix_ms(start_ticks),
        last_stop_ms: filetime_to_unix_ms(stop_ticks),
        in_use: compute_in_use(start_ticks, stop_ticks),
    })
}

/// Reads a FILETIME-valued registry entry (stored as REG_QWORD 100 ns ticks).
/// Returns 0 when absent or the wrong type.
fn read_filetime_value(key: &RegKey, name: &str) -> u64 {
    match key.get_value(name) {
        Some(RegValue::Qword(v)) => v,
        // Some builds store it as an 8-byte REG_BINARY; accept that too.
        Some(RegValue::Binary(b)) if b.len() >= 8 => {
            u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
        }
        _ => 0,
    }
}

/// Enumerates all current privacy-capability usage across both hives and both
/// packaged/NonPackaged subtrees. Best-effort: a missing hive or capability key
/// simply contributes nothing. Only the capabilities in `wanted` are read
/// (empty slice → all three).
pub fn enumerate_privacy_usage(wanted: &[Capability]) -> Vec<PrivacyUsage> {
    let caps: Vec<Capability> = if wanted.is_empty() {
        Capability::all().to_vec()
    } else {
        wanted.to_vec()
    };
    let mut out = Vec::new();
    for &root in &[HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let Some(consent) = RegKey::open(root, CONSENT_ROOT, 0) else {
            continue;
        };
        for &cap in &caps {
            if let Some(cap_key) = consent.open_subkey(cap.store_key()) {
                read_capability(&cap_key, cap, &mut out);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// R2: ConsentStore change-watcher (advanced privacy alerts, PRD §9.10.3, the
// deferred M7 item). `RegNotifyChangeKeyValue` watches the CapabilityAccessManager
// ConsentStore under both hives; on a change signal we re-enumerate usage and
// diff it against the previous snapshot to derive per-(app, capability)
// transitions (a capability went in-use / stopped), each tagged with a
// foreground and a session-locked hint. The diff itself is a pure function
// (`diff_transitions`) so it is unit-testable without the registry.
// ---------------------------------------------------------------------------

/// One observed change in a capability's use by an app: it either went in-use
/// (`started`) or stopped. Carries best-effort foreground / session-locked hints
/// the watcher fills at observation time; the alert evaluator reads these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyTransition {
    pub capability: Capability,
    /// Raw ConsentStore moniker (package family or `#`-escaped desktop path).
    pub app_id: String,
    pub display_name: String,
    pub packaged: bool,
    /// true = the capability went in-use; false = it stopped.
    pub started: bool,
    /// Unix-epoch ms at which the transition was observed.
    pub ts_ms: i64,
    /// Best-effort: the app owned the foreground window when observed.
    pub foreground: bool,
    /// Best-effort: the interactive session was locked when observed.
    pub session_locked: bool,
    /// For a stop transition, how long the capability was active (seconds); 0
    /// for a start or when the start/stop timestamps don't bracket a span.
    pub active_seconds: u32,
}

/// Stable per-app key for diffing two usage sets: `(capability, app_id)`.
fn usage_key(u: &PrivacyUsage) -> (u8, &str) {
    let cap = match u.capability {
        Capability::Camera => 1,
        Capability::Microphone => 2,
        Capability::Location => 3,
    };
    (cap, u.app_id.as_str())
}

/// The executable base name to match a usage row against the foreground process.
/// Desktop (NonPackaged) apps unmunge to a real path whose final component is the
/// exe; packaged apps have no on-disk exe name, so the family display name is the
/// best available token.
pub fn exe_basename(app_id: &str, display_name: &str, packaged: bool) -> String {
    if packaged {
        display_name.to_string()
    } else {
        let path = unmunge_nonpackaged(app_id);
        path.rsplit(['\\', '/'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&path)
            .to_string()
    }
}

/// Whether an app's exe base name matches the current foreground process's exe
/// base name (case-insensitive). `None` foreground ⇒ not foreground.
pub fn foreground_matches(app_basename: &str, foreground_basename: Option<&str>) -> bool {
    match foreground_basename {
        Some(fg) => fg.eq_ignore_ascii_case(app_basename),
        None => false,
    }
}

/// Diffs two usage snapshots into transitions. A key that becomes in-use (absent
/// or idle before) yields a `started` transition; one that was in-use and is now
/// idle or gone yields a `stop`. Pure: foreground / session-locked hints are left
/// at their defaults (`false`) for the watcher to fill. `ts_ms` stamps each row.
pub fn diff_transitions(
    prev: &[PrivacyUsage],
    next: &[PrivacyUsage],
    ts_ms: i64,
) -> Vec<PrivacyTransition> {
    use std::collections::HashMap;
    let prev_use: HashMap<(u8, &str), bool> =
        prev.iter().map(|u| (usage_key(u), u.in_use)).collect();
    let next_keys: std::collections::HashSet<(u8, &str)> = next.iter().map(usage_key).collect();

    let mut out = Vec::new();
    for u in next {
        let was = prev_use.get(&usage_key(u)).copied().unwrap_or(false);
        if u.in_use && !was {
            out.push(PrivacyTransition {
                capability: u.capability,
                app_id: u.app_id.clone(),
                display_name: u.display_name.clone(),
                packaged: u.packaged,
                started: true,
                ts_ms,
                foreground: false,
                session_locked: false,
                active_seconds: 0,
            });
        } else if !u.in_use && was {
            let active_seconds =
                if u.last_stop_ms > 0 && u.last_start_ms > 0 && u.last_stop_ms >= u.last_start_ms {
                    ((u.last_stop_ms - u.last_start_ms) / 1000) as u32
                } else {
                    0
                };
            out.push(PrivacyTransition {
                capability: u.capability,
                app_id: u.app_id.clone(),
                display_name: u.display_name.clone(),
                packaged: u.packaged,
                started: false,
                ts_ms,
                foreground: false,
                session_locked: false,
                active_seconds,
            });
        }
    }
    // An app that was in-use but whose key vanished from `next` (its subkey was
    // pruned mid-use) is treated as a stop so the "in use" state can't get stuck.
    for u in prev {
        if u.in_use && !next_keys.contains(&usage_key(u)) {
            out.push(PrivacyTransition {
                capability: u.capability,
                app_id: u.app_id.clone(),
                display_name: u.display_name.clone(),
                packaged: u.packaged,
                started: false,
                ts_ms,
                foreground: false,
                session_locked: false,
                active_seconds: 0,
            });
        }
    }
    out
}

/// Unix-epoch ms now (watcher observation timestamp).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Best-effort: the base name of the process that currently owns the foreground
/// window, or `None` when there is no foreground window / it can't be resolved.
fn current_foreground_image() -> Option<String> {
    let pid = crate::policy::foreground_pid();
    if pid == 0 {
        return None;
    }
    let procs = crate::snapshot::snapshot_processes().ok()?;
    procs
        .into_iter()
        .find(|p| p.pid == pid)
        .map(|p| p.image_name)
}

/// Best-effort: whether the interactive session is locked. When locked, the input
/// desktop switches to the secure "Winlogon" desktop; an unprivileged process
/// either cannot open it (NULL) or reads a name other than "Default". Any read
/// failure is treated as "unlocked" so the hint never over-fires on a transient
/// error, except the access-denied NULL case which is the locked signal itself.
fn session_locked() -> bool {
    // Standard READ_CONTROL — enough to read the desktop's name.
    const READ_CONTROL: DWORD = 0x0002_0000;
    // SAFETY: OpenInputDesktop takes scalars only; a NULL return means no access.
    let hdesk: HDESK = unsafe { OpenInputDesktop(0, 0, READ_CONTROL) };
    if hdesk.is_null() {
        return true;
    }
    let mut buf = [0u16; 256];
    let mut needed: DWORD = 0;
    // SAFETY: buf is a live u16 array; length is its byte size; needed is a live
    // local. The handle is valid until CloseDesktop below.
    let ok = unsafe {
        GetUserObjectInformationW(
            hdesk as HANDLE,
            UOI_NAME,
            buf.as_mut_ptr().cast(),
            (buf.len() * 2) as DWORD,
            &mut needed,
        )
    };
    // SAFETY: hdesk came from a successful OpenInputDesktop and is closed once.
    unsafe {
        CloseDesktop(hdesk);
    }
    if ok == 0 {
        return false;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let name = String::from_utf16_lossy(&buf[..end]);
    !name.eq_ignore_ascii_case("Default")
}

/// An auto-reset Win32 event, closed on drop. Backs the per-key change signal.
struct Event(HANDLE);

impl Event {
    fn new() -> Option<Event> {
        // Auto-reset (bManualReset = FALSE), initially non-signaled, unnamed.
        // SAFETY: all-NULL attributes/name; a NULL return is the failure signal.
        let h = unsafe { CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null()) };
        if h.is_null() {
            None
        } else {
            Some(Event(h))
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: handle came from a successful CreateEventW, closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// The ConsentStore change-watcher: a background thread that arms
/// `RegNotifyChangeKeyValue` on the ConsentStore under both hives and, on each
/// change, diffs usage and emits [`PrivacyTransition`]s over a channel. Runs until
/// `stop` flips or the receiver is dropped. Windows-only.
pub struct PrivacyWatcher;

impl PrivacyWatcher {
    /// Spawns the watcher thread. Transitions are sent on `tx`; the thread exits
    /// when `stop` is set (checked at least every ~500 ms) or `tx`'s receiver is
    /// dropped. Returns the join handle so the host can join it on shutdown.
    pub fn spawn(
        stop: Arc<AtomicBool>,
        tx: Sender<PrivacyTransition>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("atlas-privacy-watch".into())
            .spawn(move || watch_loop(&stop, &tx))
            .expect("spawn privacy-watch thread")
    }
}

/// Watcher thread body: arm notifications, wait (with a 500 ms timeout so the
/// stop flag stays responsive), and on any signal re-enumerate + diff + emit.
fn watch_loop(stop: &AtomicBool, tx: &Sender<PrivacyTransition>) {
    // One (key, event) per hive root; a subtree watch on the ConsentStore root
    // covers all three capability subkeys and their app entries.
    let mut watches: Vec<(RegKey, Event)> = Vec::new();
    for &root in &[HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        if let Some(key) = RegKey::open(root, CONSENT_ROOT, 0) {
            if let Some(ev) = Event::new() {
                watches.push((key, ev));
            }
        }
    }
    if watches.is_empty() {
        return;
    }
    let arm_all = |watches: &[(RegKey, Event)]| {
        for (key, ev) in watches {
            key.arm_notify(ev.0, true);
        }
    };
    arm_all(&watches);

    let handles: Vec<HANDLE> = watches.iter().map(|(_, ev)| ev.0).collect();
    let mut prev = enumerate_privacy_usage(&[]);

    while !stop.load(Ordering::SeqCst) {
        // SAFETY: `handles` are live event handles owned by `watches`.
        let rc =
            unsafe { WaitForMultipleObjects(handles.len() as DWORD, handles.as_ptr(), 0, 500) };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if rc == WAIT_TIMEOUT {
            continue;
        }
        if rc == WAIT_FAILED {
            break;
        }
        // A watched key changed: re-read and diff. The registry may still be
        // settling, but any missed edge is caught by the next signal.
        let next = enumerate_privacy_usage(&[]);
        let mut transitions = diff_transitions(&prev, &next, now_ms());
        prev = next;
        if !transitions.is_empty() {
            let locked = session_locked();
            let fg = current_foreground_image();
            for t in &mut transitions {
                t.session_locked = locked;
                let base = exe_basename(&t.app_id, &t.display_name, t.packaged);
                t.foreground = foreground_matches(&base, fg.as_deref());
                if tx.send(t.clone()).is_err() {
                    return; // receiver gone — nothing to do
                }
            }
        }
        // Re-arm (one-shot notifications): re-registering every key is simplest
        // and safe — an already-pending event just triggers an empty next diff.
        arm_all(&watches);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(cap: Capability, app: &str, in_use: bool) -> PrivacyUsage {
        PrivacyUsage {
            capability: cap,
            app_id: app.to_string(),
            display_name: display_name_from_moniker(app, false),
            packaged: false,
            last_start_ms: 1_000,
            last_stop_ms: if in_use { 0 } else { 6_000 },
            in_use,
        }
    }

    #[test]
    fn diff_detects_start() {
        let prev = vec![usage(Capability::Microphone, "C:#app.exe", false)];
        let next = vec![usage(Capability::Microphone, "C:#app.exe", true)];
        let t = diff_transitions(&prev, &next, 42);
        assert_eq!(t.len(), 1);
        assert!(t[0].started);
        assert_eq!(t[0].ts_ms, 42);
        assert_eq!(t[0].capability, Capability::Microphone);
    }

    #[test]
    fn diff_detects_start_when_app_is_new() {
        // App absent from prev entirely, appears already in-use.
        let prev: Vec<PrivacyUsage> = vec![];
        let next = vec![usage(Capability::Camera, "C:#cam.exe", true)];
        let t = diff_transitions(&prev, &next, 0);
        assert_eq!(t.len(), 1);
        assert!(t[0].started);
    }

    #[test]
    fn diff_detects_stop_with_duration() {
        let prev = vec![usage(Capability::Camera, "C:#cam.exe", true)];
        let next = vec![usage(Capability::Camera, "C:#cam.exe", false)];
        let t = diff_transitions(&prev, &next, 0);
        assert_eq!(t.len(), 1);
        assert!(!t[0].started);
        // last_start_ms=1000, last_stop_ms=6000 → 5 s active.
        assert_eq!(t[0].active_seconds, 5);
    }

    #[test]
    fn diff_stop_when_key_vanishes() {
        let prev = vec![usage(Capability::Location, "C:#loc.exe", true)];
        let next: Vec<PrivacyUsage> = vec![];
        let t = diff_transitions(&prev, &next, 0);
        assert_eq!(t.len(), 1);
        assert!(!t[0].started);
    }

    #[test]
    fn diff_no_change_is_empty() {
        let prev = vec![usage(Capability::Microphone, "C:#app.exe", true)];
        let next = vec![usage(Capability::Microphone, "C:#app.exe", true)];
        assert!(diff_transitions(&prev, &next, 0).is_empty());
    }

    #[test]
    fn exe_basename_desktop_and_packaged() {
        assert_eq!(
            exe_basename("C:#Program Files#App#app.exe", "app.exe", false),
            "app.exe"
        );
        assert_eq!(
            exe_basename(
                "Microsoft.WindowsCamera_8we",
                "Microsoft.WindowsCamera",
                true
            ),
            "Microsoft.WindowsCamera"
        );
    }

    #[test]
    fn foreground_matches_is_case_insensitive() {
        assert!(foreground_matches("App.exe", Some("app.exe")));
        assert!(!foreground_matches("app.exe", Some("other.exe")));
        assert!(!foreground_matches("app.exe", None));
    }

    #[test]
    fn filetime_epoch_maps_to_zero_unix() {
        // The FILETIME representing exactly the Unix epoch → 0 ms.
        assert_eq!(filetime_to_unix_ms(FILETIME_UNIX_DELTA_100NS as u64), 0);
    }

    #[test]
    fn filetime_zero_is_never() {
        assert_eq!(filetime_to_unix_ms(0), 0);
    }

    #[test]
    fn filetime_one_second_after_epoch() {
        // Unix epoch + 1 s = delta + 10_000_000 ticks → 1000 ms.
        let ticks = FILETIME_UNIX_DELTA_100NS as u64 + 10_000_000;
        assert_eq!(filetime_to_unix_ms(ticks), 1000);
    }

    #[test]
    fn in_use_when_stop_zero() {
        assert!(compute_in_use(1_000, 0));
    }

    #[test]
    fn not_in_use_when_stop_after_start() {
        assert!(!compute_in_use(1_000, 2_000));
    }

    #[test]
    fn in_use_when_stop_before_start() {
        // A stop older than the start means the app started again since.
        assert!(compute_in_use(2_000, 1_000));
    }

    #[test]
    fn not_in_use_when_never_started() {
        assert!(!compute_in_use(0, 0));
        assert!(!compute_in_use(0, 5_000));
    }

    #[test]
    fn packaged_display_name_is_family() {
        assert_eq!(
            display_name_from_moniker("Microsoft.WindowsCamera_8wekyb3d8bbwe", true),
            "Microsoft.WindowsCamera"
        );
    }

    #[test]
    fn packaged_display_name_without_underscore() {
        assert_eq!(display_name_from_moniker("SomeApp", true), "SomeApp");
    }

    #[test]
    fn nonpackaged_unmunge_basic() {
        // C:#Program Files#App#app.exe  →  app.exe
        let moniker = "C:#Program Files#App#app.exe";
        assert_eq!(
            unmunge_nonpackaged(moniker),
            r"C:\Program Files\App\app.exe"
        );
        assert_eq!(display_name_from_moniker(moniker, false), "app.exe");
    }

    #[test]
    fn nonpackaged_unmunge_escaped_hash() {
        // A literal '#' in a path is doubled by Windows.
        let moniker = "C:#Weird##Dir#tool.exe";
        assert_eq!(unmunge_nonpackaged(moniker), r"C:\Weird#Dir\tool.exe");
        assert_eq!(display_name_from_moniker(moniker, false), "tool.exe");
    }
}
