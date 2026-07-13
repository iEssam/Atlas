//! Startup inventory (PRD §9.8.1 core sources, docs/phases.md M7).
//!
//! Enumerates the classic auto-run surfaces a user can inspect without a boot
//! trace:
//!
//! - **Run / RunOnce keys** under HKLM and HKCU, in both the native 64-bit view
//!   and the 32-bit WOW6432Node view (a 32-bit installer writes there).
//! - **Startup folders** — the machine (`%ProgramData%\...\Startup`) and user
//!   (`%AppData%\...\Startup`) shortcut directories.
//! - **StartupApproved** state (`...\Explorer\StartupApproved\Run`) — the blob
//!   Task Manager writes when you disable a Run entry; byte 0 even == enabled.
//!
//! Services and scheduled tasks are also startup *sources* in the proto enum;
//! services are inventoried separately (`services.rs`) and are not duplicated
//! here, and Task Scheduler enumeration (COM) is deferred this round (noted on
//! [`enumerate_startup`]). Everything here is a plain user-mode registry / file
//! read — no elevation.

#![cfg(windows)]

use std::path::Path;

use crate::ffi::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
use crate::reg::{RegKey, RegValue};

/// Where a startup entry was found. Mirrors the proto `StartupSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartupSource {
    RunKeyMachine,
    RunKeyUser,
    StartupFolderMachine,
    StartupFolderUser,
    ScheduledTask,
    Service,
    PackagedTask,
}

/// Whether an entry applies machine-wide or per-user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Machine,
    User,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Machine => "machine",
            Scope::User => "user",
        }
    }
}

/// One startup entry.
#[derive(Debug, Clone)]
pub struct StartupEntry {
    pub name: String,
    pub source: StartupSource,
    pub command: String,
    /// Best-effort publisher; empty when not cheaply determinable.
    pub publisher: String,
    pub enabled: bool,
    pub scope: Scope,
}

/// The Run key relative path.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// StartupApproved\Run — the enabled/disabled blob store.
const APPROVED_RUN_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Parses a StartupApproved blob into an "enabled" flag. Task Manager stores a
/// 12-byte little-endian struct whose first byte encodes the state: an **even**
/// low byte means enabled, **odd** means disabled (bit 0 set). An empty/short
/// blob is treated as enabled (the default when no override exists).
pub fn approved_blob_enabled(blob: &[u8]) -> bool {
    match blob.first() {
        Some(b) => b & 1 == 0,
        None => true,
    }
}

/// Maps a hive root + folder scope onto the [`StartupSource`]/[`Scope`] pair.
/// Extracted so the scope mapping is unit-testable without touching the
/// registry.
pub fn run_key_source(scope: Scope) -> StartupSource {
    match scope {
        Scope::Machine => StartupSource::RunKeyMachine,
        Scope::User => StartupSource::RunKeyUser,
    }
}

/// Maps a folder scope onto the folder [`StartupSource`].
pub fn folder_source(scope: Scope) -> StartupSource {
    match scope {
        Scope::Machine => StartupSource::StartupFolderMachine,
        Scope::User => StartupSource::StartupFolderUser,
    }
}

/// Reads the StartupApproved\Run overrides for a hive into a lookup of
/// value-name → enabled. Missing key → empty map (everything enabled).
fn approved_map(root: crate::ffi::HKEY, view: u32) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    if let Some(key) = RegKey::open(root, APPROVED_RUN_KEY, view) {
        for (name, value) in key.values() {
            if let RegValue::Binary(b) = value {
                out.push((name, approved_blob_enabled(&b)));
            } else {
                // A non-binary override is unusual; treat presence as enabled.
                out.push((name, true));
            }
        }
    }
    out
}

/// Looks up an entry's enabled state from a StartupApproved map (case-insensitive
/// on the value name). Absent → enabled.
fn is_enabled(approved: &[(String, bool)], name: &str) -> bool {
    approved
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, e)| *e)
        .unwrap_or(true)
}

/// Enumerates one hive's Run key in one registry view, tagging each entry with
/// its enabled state from the corresponding StartupApproved map.
fn read_run_key(
    root: crate::ffi::HKEY,
    scope: Scope,
    view: u32,
    approved: &[(String, bool)],
    out: &mut Vec<StartupEntry>,
) {
    let Some(key) = RegKey::open(root, RUN_KEY, view) else {
        return;
    };
    for (name, value) in key.values() {
        let command = match value {
            RegValue::Str(s) => s,
            _ => continue,
        };
        out.push(StartupEntry {
            name: name.clone(),
            source: run_key_source(scope),
            command,
            publisher: String::new(),
            enabled: is_enabled(approved, &name),
            scope,
        });
    }
}

/// Enumerates the shortcut files in a Startup folder. The command is the raw
/// shortcut path (resolving the .lnk target needs the shell COM interface, out
/// of scope this round — the file name is the entry name, the full path the
/// command).
fn read_startup_folder(dir: &Path, scope: Scope, out: &mut Vec<StartupEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Skip the desktop.ini bookkeeping file.
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.eq_ignore_ascii_case("desktop.ini") => n.to_string(),
            _ => continue,
        };
        out.push(StartupEntry {
            name,
            source: folder_source(scope),
            command: path.to_string_lossy().into_owned(),
            publisher: String::new(),
            // A shortcut present in the Startup folder is enabled by definition
            // (StartupApproved\StartupFolder disable state is not read here).
            enabled: true,
            scope,
        });
    }
}

/// Enumerates the classic startup surfaces (Run keys in both views + Startup
/// folders). Scheduled tasks and packaged StartupTasks are **not** covered this
/// round (Task Scheduler COM deferred); services are inventoried by
/// `services.rs` and intentionally not duplicated here.
pub fn enumerate_startup() -> Vec<StartupEntry> {
    let mut out = Vec::new();

    // Run keys — machine (HKLM) and user (HKCU), 64-bit and 32-bit views. The
    // StartupApproved override lives per hive; we read it once per hive and
    // apply to both views (the approved store is not view-split).
    let hklm_approved = approved_map(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY);
    read_run_key(
        HKEY_LOCAL_MACHINE,
        Scope::Machine,
        KEY_WOW64_64KEY,
        &hklm_approved,
        &mut out,
    );
    read_run_key(
        HKEY_LOCAL_MACHINE,
        Scope::Machine,
        KEY_WOW64_32KEY,
        &hklm_approved,
        &mut out,
    );

    let hkcu_approved = approved_map(HKEY_CURRENT_USER, KEY_WOW64_64KEY);
    read_run_key(
        HKEY_CURRENT_USER,
        Scope::User,
        KEY_WOW64_64KEY,
        &hkcu_approved,
        &mut out,
    );
    read_run_key(
        HKEY_CURRENT_USER,
        Scope::User,
        KEY_WOW64_32KEY,
        &hkcu_approved,
        &mut out,
    );

    // Startup folders. The machine folder sits under %ProgramData%; the user
    // folder under %AppData% (Roaming).
    if let Some(pd) = std::env::var_os("ProgramData") {
        let dir = Path::new(&pd).join(r"Microsoft\Windows\Start Menu\Programs\Startup");
        read_startup_folder(&dir, Scope::Machine, &mut out);
    }
    if let Some(ad) = std::env::var_os("APPDATA") {
        let dir = Path::new(&ad).join(r"Microsoft\Windows\Start Menu\Programs\Startup");
        read_startup_folder(&dir, Scope::User, &mut out);
    }

    dedup(out)
}

/// Removes duplicate entries. The 64-bit and 32-bit registry views alias the
/// same physical key for hives without WOW6432 redirection (notably HKCU's Run
/// key), so reading both views would list an entry twice. Dedup by the identity
/// tuple `(source, name, command)`, preserving first-seen order.
fn dedup(entries: Vec<StartupEntry>) -> Vec<StartupEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let key = (e.source, e.name.clone(), e.command.clone());
        if seen.insert(key) {
            out.push(e);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_even_first_byte_is_enabled() {
        // Task Manager "enabled" blob: first byte 0x02.
        let blob = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(approved_blob_enabled(&blob));
    }

    #[test]
    fn approved_odd_first_byte_is_disabled() {
        // Task Manager "disabled" blob: first byte 0x03.
        let blob = [0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(!approved_blob_enabled(&blob));
    }

    #[test]
    fn approved_empty_defaults_enabled() {
        assert!(approved_blob_enabled(&[]));
    }

    #[test]
    fn run_key_scope_mapping() {
        assert_eq!(run_key_source(Scope::Machine), StartupSource::RunKeyMachine);
        assert_eq!(run_key_source(Scope::User), StartupSource::RunKeyUser);
    }

    #[test]
    fn folder_scope_mapping() {
        assert_eq!(
            folder_source(Scope::Machine),
            StartupSource::StartupFolderMachine
        );
        assert_eq!(folder_source(Scope::User), StartupSource::StartupFolderUser);
    }

    #[test]
    fn is_enabled_case_insensitive_and_default() {
        let approved = vec![("OneDrive".to_string(), false)];
        assert!(!is_enabled(&approved, "onedrive"));
        // Absent name defaults to enabled.
        assert!(is_enabled(&approved, "SomethingElse"));
    }

    #[test]
    fn scope_str() {
        assert_eq!(Scope::Machine.as_str(), "machine");
        assert_eq!(Scope::User.as_str(), "user");
    }

    #[test]
    fn dedup_removes_view_aliased_duplicates() {
        let mk = |name: &str| StartupEntry {
            name: name.to_string(),
            source: StartupSource::RunKeyUser,
            command: format!("C:\\{name}.exe"),
            publisher: String::new(),
            enabled: true,
            scope: Scope::User,
        };
        // Same entry seen twice (64-bit + 32-bit view alias) plus a distinct one.
        let input = vec![mk("Steam"), mk("Steam"), mk("Discord")];
        let out = dedup(input);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "Steam");
        assert_eq!(out[1].name, "Discord");
    }
}
