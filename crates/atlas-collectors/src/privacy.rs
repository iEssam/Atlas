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

use crate::ffi::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
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

#[cfg(test)]
mod tests {
    use super::*;

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
