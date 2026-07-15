//! System-change detection (PRD §9.13, docs/phases.md Phase 3 / R3).
//!
//! Two complementary strategies:
//!
//! - **State-diff** — [`collect_inventory`] snapshots the user-inspectable
//!   configuration surface (installed apps, Win32 services, startup entries,
//!   scheduled tasks, the active power plan, default protocol/extension
//!   handlers) into a serializable [`Inventory`]. The store persists a snapshot
//!   between passes and feeds consecutive snapshots to the pure
//!   [`diff_inventories`], which emits one [`DetectedChange`] per observed
//!   difference. The diff is the unit-tested core: it touches no OS API.
//!
//! - **Event-sourced** — [`windows_update_history`] reads the Windows Update
//!   Agent (WUA) install history through the `IUpdateSearcher` COM object,
//!   late-bound via `IDispatch` (`GetIDsOfNames` + `Invoke`) so no interface
//!   vtable slot beyond `IDispatch`'s own is guessed. Any failure (COM
//!   unavailable, empty history) degrades to an empty list.
//!
//! Every OS-touching collector is best-effort: a source that fails yields an
//! empty sub-inventory and never panics. COM is confined to the calling thread
//! of [`windows_update_history`] (`CoInitializeEx` at entry, `CoUninitialize`
//! at exit). All reads — nothing here writes the registry, the SCM, or COM
//! state.

#![cfg(windows)]

use std::collections::HashMap;
use std::ptr::null_mut;

use crate::ffi::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, FreeLibrary, GetProcAddress, IUnknownVtbl,
    LoadLibraryW, LocalFree, SysFreeString, VariantClear, BSTR, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, DWORD, GUID, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HLOCAL,
    KEY_WOW64_32KEY, KEY_WOW64_64KEY, PVOID, RPC_E_CHANGED_MODE, S_FALSE, S_OK, VARIANT, VT_I4,
};
use crate::reg::{to_wide, RegKey, RegValue};

/// `SystemChangeKind` discriminants, matching the FROZEN proto `atlas.v0` enum.
/// Kept as free constants (not a Rust enum) so the wire values are explicit and
/// a `DetectedChange::kind` is trivially comparable across crate boundaries.
pub mod change_kind {
    pub const APP_INSTALLED: i32 = 1;
    pub const APP_UPDATED: i32 = 2;
    pub const APP_REMOVED: i32 = 3;
    pub const DRIVER_INSTALLED: i32 = 4;
    pub const DRIVER_UPDATED: i32 = 5;
    pub const WINDOWS_UPDATE: i32 = 6;
    pub const SERVICE_INSTALLED: i32 = 7;
    pub const SERVICE_CONFIG_CHANGED: i32 = 8;
    pub const SERVICE_REMOVED: i32 = 9;
    pub const STARTUP_ADDED: i32 = 10;
    pub const STARTUP_REMOVED: i32 = 11;
    pub const SCHEDULED_TASK_ADDED: i32 = 12;
    pub const SCHEDULED_TASK_REMOVED: i32 = 13;
    pub const POWER_PLAN_CHANGED: i32 = 14;
    pub const DEFAULT_APP_CHANGED: i32 = 15;
}

/// One detected change (maps 1:1 to proto `SystemChange` minus `id`/`ts_ms`,
/// which the store assigns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedChange {
    /// A [`change_kind`] discriminant.
    pub kind: i32,
    pub subject: String,
    /// Human-readable before→after summary.
    pub detail: String,
    pub publisher: String,
    pub responsible: String,
    pub reversible: bool,
}

impl DetectedChange {
    /// Builds a change with the fields this milestone always leaves empty
    /// (`responsible`) or constant (`reversible == false`).
    fn new(kind: i32, subject: String, detail: String, publisher: String) -> Self {
        DetectedChange {
            kind,
            subject,
            detail,
            publisher,
            responsible: String::new(),
            reversible: false,
        }
    }
}

// --- Inventory sub-entries ---------------------------------------------------

/// One installed application (registry Uninstall row).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppEntry {
    pub name: String,
    pub version: String,
    pub publisher: String,
}

/// One Win32 service, reduced to the config fields a diff compares.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SvcEntry {
    pub name: String,
    pub display_name: String,
    pub start_type: String,
    pub account: String,
    pub binary_path: String,
}

/// One startup entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StartupItem {
    pub name: String,
    pub source: String,
    pub scope: String,
    pub command: String,
    pub publisher: String,
}

/// One scheduled task, reduced to its identity + primary action.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskItem {
    pub path: String,
    pub action: String,
    pub enabled: bool,
}

/// One default-handler association. `kind` is e.g. `"http"`, `".html"`,
/// `"mailto"`; `handler` is the resolved ProgId.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DefaultAppEntry {
    pub kind: String,
    pub handler: String,
}

/// The full snapshot the detector persists between passes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Inventory {
    pub apps: Vec<AppEntry>,
    pub services: Vec<SvcEntry>,
    pub startup: Vec<StartupItem>,
    pub tasks: Vec<TaskItem>,
    /// Active power scheme GUID string (or `""` when unavailable).
    pub power_plan: String,
    pub default_apps: Vec<DefaultAppEntry>,
}

// ---------------------------------------------------------------------------
// Live inventory collection (best-effort; a failing source yields an empty
// sub-vec / empty string, never a panic).
// ---------------------------------------------------------------------------

/// Collects the full current inventory from the OS.
pub fn collect_inventory() -> Inventory {
    Inventory {
        apps: collect_apps(),
        services: collect_services(),
        startup: collect_startup(),
        tasks: collect_tasks(),
        power_plan: collect_power_plan(),
        default_apps: collect_default_apps(),
    }
}

/// Registry Uninstall key (relative to a hive root).
const UNINSTALL_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

/// Reads a REG_SZ value as an owned `String`, or `""` when absent / not a string.
fn reg_str(key: &RegKey, name: &str) -> String {
    match key.get_value(name) {
        Some(v) => v.as_str().unwrap_or("").to_string(),
        None => String::new(),
    }
}

/// Enumerates installed apps from the Uninstall keys of HKLM (both registry
/// views) and HKCU, filtering the usual noise (empty name, `SystemComponent`,
/// child components with a `ParentKeyName`).
fn collect_apps() -> Vec<AppEntry> {
    let mut out = Vec::new();
    read_uninstall(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY, &mut out);
    read_uninstall(HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY, &mut out);
    read_uninstall(HKEY_CURRENT_USER, 0, &mut out);

    // Dedup by (name, version): sort into that grouping then drop adjacent dups.
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.version.cmp(&b.version))
    });
    out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name) && a.version == b.version);
    out
}

/// Reads one Uninstall key in one registry view into `out`.
fn read_uninstall(root: HKEY, view: DWORD, out: &mut Vec<AppEntry>) {
    let Some(key) = RegKey::open(root, UNINSTALL_KEY, view) else {
        return;
    };
    for name in key.subkey_names() {
        let Some(sub) = key.open_subkey(&name) else {
            continue;
        };
        let display_name = reg_str(&sub, "DisplayName");
        if display_name.is_empty() {
            continue;
        }
        // Hide OS/component rows and per-component children.
        if matches!(sub.get_value("SystemComponent"), Some(RegValue::Dword(1))) {
            continue;
        }
        if !reg_str(&sub, "ParentKeyName").is_empty() {
            continue;
        }
        out.push(AppEntry {
            name: display_name,
            version: reg_str(&sub, "DisplayVersion"),
            publisher: reg_str(&sub, "Publisher"),
        });
    }
}

/// Maps the live service inventory into [`SvcEntry`] rows, sorted by name.
fn collect_services() -> Vec<SvcEntry> {
    let mut out: Vec<SvcEntry> = crate::services::enumerate_services("")
        .into_iter()
        .map(|e| SvcEntry {
            name: e.name,
            display_name: e.display_name,
            start_type: format!("{:?}", e.start_type),
            account: e.account,
            binary_path: e.binary_path,
        })
        .collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Maps the live startup inventory into [`StartupItem`] rows, sorted by
/// (source, name, command).
fn collect_startup() -> Vec<StartupItem> {
    let mut out: Vec<StartupItem> = crate::startup::enumerate_startup()
        .into_iter()
        .map(|e| StartupItem {
            name: e.name,
            source: format!("{:?}", e.source),
            scope: e.scope.as_str().to_string(),
            command: e.command,
            publisher: e.publisher,
        })
        .collect();
    out.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.command.cmp(&b.command))
    });
    out
}

/// Maps the live task inventory into [`TaskItem`] rows, sorted by path.
fn collect_tasks() -> Vec<TaskItem> {
    let mut out: Vec<TaskItem> = crate::tasks::enumerate_tasks("")
        .into_iter()
        .map(|t| TaskItem {
            path: t.path,
            action: t.action,
            enabled: t.enabled,
        })
        .collect();
    out.sort_by_key(|a| a.path.to_lowercase());
    out
}

/// `PowerGetActiveScheme` (powrprof.dll) — dynamically resolved.
type PowerGetActiveSchemeFn = unsafe extern "system" fn(PVOID, *mut *mut GUID) -> DWORD;

/// Reads the active power scheme GUID via `PowerGetActiveScheme`, dynamically
/// loading powrprof.dll. Returns `""` on any failure. Guarded end-to-end.
fn collect_power_plan() -> String {
    let dll_name = to_wide("powrprof.dll");
    // SAFETY: dll_name is a live NUL-terminated UTF-16 buffer; every returned
    // handle/pointer is null-checked before use and freed exactly once.
    unsafe {
        let dll = LoadLibraryW(dll_name.as_ptr());
        if dll.is_null() {
            return String::new();
        }
        let proc = GetProcAddress(dll, c"PowerGetActiveScheme".as_ptr().cast());
        if proc.is_null() {
            FreeLibrary(dll);
            return String::new();
        }
        let power_get_active_scheme = std::mem::transmute::<PVOID, PowerGetActiveSchemeFn>(proc);
        let mut guid_ptr: *mut GUID = null_mut();
        let rc = power_get_active_scheme(null_mut(), &mut guid_ptr);
        let result = if rc == 0 && !guid_ptr.is_null() {
            format_guid(&*guid_ptr)
        } else {
            String::new()
        };
        if !guid_ptr.is_null() {
            LocalFree(guid_ptr as HLOCAL);
        }
        FreeLibrary(dll);
        result
    }
}

/// Formats a `GUID` as the canonical `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`
/// string (lowercase, as Windows renders power-scheme GUIDs).
fn format_guid(g: &GUID) -> String {
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        g.Data1,
        g.Data2,
        g.Data3,
        g.Data4[0],
        g.Data4[1],
        g.Data4[2],
        g.Data4[3],
        g.Data4[4],
        g.Data4[5],
        g.Data4[6],
        g.Data4[7],
    )
}

/// URL-protocol association root (UserChoice ProgId per protocol).
const URL_ASSOC_KEY: &str = r"SOFTWARE\Microsoft\Windows\Shell\Associations\UrlAssociations";
/// File-extension association root (UserChoice ProgId per extension).
const FILE_EXTS_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\FileExts";

/// Reads the current default handler (ProgId) for a small fixed set of
/// protocols and file extensions from the per-user UserChoice keys.
fn collect_default_apps() -> Vec<DefaultAppEntry> {
    let mut out = Vec::new();
    for proto in ["http", "https", "mailto"] {
        let sub = format!(r"{URL_ASSOC_KEY}\{proto}\UserChoice");
        if let Some(key) = RegKey::open(HKEY_CURRENT_USER, &sub, 0) {
            let handler = reg_str(&key, "ProgId");
            if !handler.is_empty() {
                out.push(DefaultAppEntry {
                    kind: proto.to_string(),
                    handler,
                });
            }
        }
    }
    for ext in [".html", ".pdf"] {
        let sub = format!(r"{FILE_EXTS_KEY}\{ext}\UserChoice");
        if let Some(key) = RegKey::open(HKEY_CURRENT_USER, &sub, 0) {
            let handler = reg_str(&key, "ProgId");
            if !handler.is_empty() {
                out.push(DefaultAppEntry {
                    kind: ext.to_string(),
                    handler,
                });
            }
        }
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind));
    out
}

// ---------------------------------------------------------------------------
// Pure diff — the unit-tested core.
// ---------------------------------------------------------------------------

/// Compares two inventories and emits the changes between them. Deterministic:
/// categories in a fixed order (apps, services, startup, tasks, power plan,
/// default apps) and, within each category, sorted by subject. Pure — no OS
/// call, so it is exhaustively unit-tested.
pub fn diff_inventories(prev: &Inventory, next: &Inventory) -> Vec<DetectedChange> {
    let mut out = Vec::new();
    diff_apps(&prev.apps, &next.apps, &mut out);
    diff_services(&prev.services, &next.services, &mut out);
    diff_startup(&prev.startup, &next.startup, &mut out);
    diff_tasks(&prev.tasks, &next.tasks, &mut out);
    diff_power_plan(&prev.power_plan, &next.power_plan, &mut out);
    diff_default_apps(&prev.default_apps, &next.default_apps, &mut out);
    out
}

/// Sorts a batch of changes by subject (case-insensitive), then kind, for a
/// stable within-category order, and appends them to `out`.
fn push_sorted(mut batch: Vec<DetectedChange>, out: &mut Vec<DetectedChange>) {
    batch.sort_by(|a, b| {
        a.subject
            .to_lowercase()
            .cmp(&b.subject.to_lowercase())
            .then_with(|| a.kind.cmp(&b.kind))
    });
    out.extend(batch);
}

/// Apps matched by `name` (case-insensitive): missing in prev → installed,
/// missing in next → removed, same name / different version → updated.
fn diff_apps(prev: &[AppEntry], next: &[AppEntry], out: &mut Vec<DetectedChange>) {
    let prev_by: HashMap<String, &AppEntry> =
        prev.iter().map(|a| (a.name.to_lowercase(), a)).collect();
    let next_by: HashMap<String, &AppEntry> =
        next.iter().map(|a| (a.name.to_lowercase(), a)).collect();

    let mut batch = Vec::new();
    for (key, na) in &next_by {
        match prev_by.get(key) {
            None => batch.push(DetectedChange::new(
                change_kind::APP_INSTALLED,
                na.name.clone(),
                na.version.clone(),
                na.publisher.clone(),
            )),
            Some(pa) if pa.version != na.version => batch.push(DetectedChange::new(
                change_kind::APP_UPDATED,
                na.name.clone(),
                format!("{} → {}", pa.version, na.version),
                na.publisher.clone(),
            )),
            Some(_) => {}
        }
    }
    for (key, pa) in &prev_by {
        if !next_by.contains_key(key) {
            batch.push(DetectedChange::new(
                change_kind::APP_REMOVED,
                pa.name.clone(),
                pa.version.clone(),
                pa.publisher.clone(),
            ));
        }
    }
    push_sorted(batch, out);
}

/// Services matched by `name` (case-insensitive): install / remove, or
/// config-changed when `start_type` | `account` | `binary_path` differ.
fn diff_services(prev: &[SvcEntry], next: &[SvcEntry], out: &mut Vec<DetectedChange>) {
    let prev_by: HashMap<String, &SvcEntry> =
        prev.iter().map(|s| (s.name.to_lowercase(), s)).collect();
    let next_by: HashMap<String, &SvcEntry> =
        next.iter().map(|s| (s.name.to_lowercase(), s)).collect();

    let mut batch = Vec::new();
    for (key, ns) in &next_by {
        match prev_by.get(key) {
            None => batch.push(DetectedChange::new(
                change_kind::SERVICE_INSTALLED,
                ns.name.clone(),
                ns.display_name.clone(),
                String::new(),
            )),
            Some(ps) => {
                let mut parts = Vec::new();
                if ps.start_type != ns.start_type {
                    parts.push(format!("start_type {} → {}", ps.start_type, ns.start_type));
                }
                if ps.account != ns.account {
                    parts.push(format!("account {} → {}", ps.account, ns.account));
                }
                if ps.binary_path != ns.binary_path {
                    parts.push(format!(
                        "binary_path {} → {}",
                        ps.binary_path, ns.binary_path
                    ));
                }
                if !parts.is_empty() {
                    batch.push(DetectedChange::new(
                        change_kind::SERVICE_CONFIG_CHANGED,
                        ns.name.clone(),
                        parts.join("; "),
                        String::new(),
                    ));
                }
            }
        }
    }
    for (key, ps) in &prev_by {
        if !next_by.contains_key(key) {
            batch.push(DetectedChange::new(
                change_kind::SERVICE_REMOVED,
                ps.name.clone(),
                ps.display_name.clone(),
                String::new(),
            ));
        }
    }
    push_sorted(batch, out);
}

/// Startup entries matched by the full identity tuple (source, scope, name,
/// command): added / removed.
fn diff_startup(prev: &[StartupItem], next: &[StartupItem], out: &mut Vec<DetectedChange>) {
    let startup_key = |i: &StartupItem| {
        (
            i.source.clone(),
            i.scope.clone(),
            i.name.clone(),
            i.command.clone(),
        )
    };
    let prev_by: HashMap<_, &StartupItem> = prev.iter().map(|i| (startup_key(i), i)).collect();
    let next_by: HashMap<_, &StartupItem> = next.iter().map(|i| (startup_key(i), i)).collect();

    let mut batch = Vec::new();
    for (key, ni) in &next_by {
        if !prev_by.contains_key(key) {
            batch.push(DetectedChange::new(
                change_kind::STARTUP_ADDED,
                ni.name.clone(),
                ni.command.clone(),
                ni.publisher.clone(),
            ));
        }
    }
    for (key, pi) in &prev_by {
        if !next_by.contains_key(key) {
            batch.push(DetectedChange::new(
                change_kind::STARTUP_REMOVED,
                pi.name.clone(),
                pi.command.clone(),
                pi.publisher.clone(),
            ));
        }
    }
    push_sorted(batch, out);
}

/// Tasks matched by `path`: added / removed.
fn diff_tasks(prev: &[TaskItem], next: &[TaskItem], out: &mut Vec<DetectedChange>) {
    let prev_by: HashMap<&str, &TaskItem> = prev.iter().map(|t| (t.path.as_str(), t)).collect();
    let next_by: HashMap<&str, &TaskItem> = next.iter().map(|t| (t.path.as_str(), t)).collect();

    let mut batch = Vec::new();
    for (path, nt) in &next_by {
        if !prev_by.contains_key(path) {
            batch.push(DetectedChange::new(
                change_kind::SCHEDULED_TASK_ADDED,
                nt.path.clone(),
                nt.action.clone(),
                String::new(),
            ));
        }
    }
    for (path, pt) in &prev_by {
        if !next_by.contains_key(path) {
            batch.push(DetectedChange::new(
                change_kind::SCHEDULED_TASK_REMOVED,
                pt.path.clone(),
                pt.action.clone(),
                String::new(),
            ));
        }
    }
    push_sorted(batch, out);
}

/// Power plan: one change when the new plan is known and differs from the old.
fn diff_power_plan(prev: &str, next: &str, out: &mut Vec<DetectedChange>) {
    if !next.is_empty() && prev != next {
        let from = if prev.is_empty() { "(unknown)" } else { prev };
        out.push(DetectedChange::new(
            change_kind::POWER_PLAN_CHANGED,
            next.to_string(),
            format!("{from} → {next}"),
            String::new(),
        ));
    }
}

/// Default apps matched by `kind`: one change when the handler differs and the
/// kind is present on both sides.
fn diff_default_apps(
    prev: &[DefaultAppEntry],
    next: &[DefaultAppEntry],
    out: &mut Vec<DetectedChange>,
) {
    let prev_by: HashMap<&str, &DefaultAppEntry> =
        prev.iter().map(|d| (d.kind.as_str(), d)).collect();

    let mut batch = Vec::new();
    for nd in next {
        if let Some(pd) = prev_by.get(nd.kind.as_str()) {
            if pd.handler != nd.handler {
                batch.push(DetectedChange::new(
                    change_kind::DEFAULT_APP_CHANGED,
                    nd.kind.clone(),
                    format!("{} → {}", pd.handler, nd.handler),
                    String::new(),
                ));
            }
        }
    }
    push_sorted(batch, out);
}

// ---------------------------------------------------------------------------
// Windows Update history via WUA (`IUpdateSearcher::QueryHistory`), late-bound
// through IDispatch. Empty on any failure; never panics.
// ---------------------------------------------------------------------------

/// `DISPPARAMS` — arguments block passed to `IDispatch::Invoke` (args live in
/// `rgvarg` in REVERSE order). Fields are read across the FFI boundary only, so
/// the dead-code lint would otherwise flag them.
#[repr(C)]
#[allow(dead_code)]
struct DispParams {
    rgvarg: *mut VARIANT,
    rgdispid_named_args: *mut i32,
    c_args: u32,
    c_named_args: u32,
}

/// `IDispatch` vtable prefix through `Invoke`. Only `GetIDsOfNames` and
/// `Invoke` are called; the earlier slots fix their positions. `Release` (slot
/// 2) is invoked through [`IUnknownVtbl`] instead, so it is left opaque here.
/// The unused slots exist only to place `get_ids_of_names`/`invoke` at the
/// correct vtable offsets.
#[repr(C)]
#[allow(dead_code)]
struct IDispatchVtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    get_type_info_count: usize,
    get_type_info: usize,
    get_ids_of_names:
        unsafe extern "system" fn(PVOID, *const GUID, *const *const u16, u32, u32, *mut i32) -> i32,
    invoke: unsafe extern "system" fn(
        PVOID,
        i32,
        *const GUID,
        u32,
        u16,
        *mut DispParams,
        *mut VARIANT,
        PVOID,
        *mut u32,
    ) -> i32,
}

/// The all-zero GUID (`IID_NULL`, the reserved riid for `GetIDsOfNames`/`Invoke`).
const IID_NULL: GUID = GUID {
    Data1: 0,
    Data2: 0,
    Data3: 0,
    Data4: [0; 8],
};

/// `IID_IDispatch` — `{00020400-0000-0000-C000-000000000046}`.
const IID_IDISPATCH: GUID = GUID {
    Data1: 0x0002_0400,
    Data2: 0x0000,
    Data3: 0x0000,
    Data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

/// `CLSID_UpdateSession` — `{4CB43D7F-7EEE-4906-8698-60DA1C38F2FE}`.
const CLSID_UPDATE_SESSION: GUID = GUID {
    Data1: 0x4CB4_3D7F,
    Data2: 0x7EEE,
    Data3: 0x4906,
    Data4: [0x86, 0x98, 0x60, 0xDA, 0x1C, 0x38, 0xF2, 0xFE],
};

const LOCALE_SYSTEM_DEFAULT: u32 = 0x0800;
const DISPATCH_METHOD: u16 = 1;
const DISPATCH_PROPERTYGET: u16 = 2;
const VT_BSTR: u16 = 8;
const VT_DISPATCH: u16 = 9;
const VT_DATE: u16 = 7;

/// Reads up to `max` recent WUA history entries as [`change_kind::WINDOWS_UPDATE`]
/// changes. Owns COM init/uninit on the calling thread. Empty on ANY failure.
pub fn windows_update_history(max: i32) -> Vec<DetectedChange> {
    if max <= 0 {
        return Vec::new();
    }
    // SAFETY: init COM for this thread. S_OK/S_FALSE both mean usable and must be
    // balanced with CoUninitialize; RPC_E_CHANGED_MODE means a different apartment
    // is already active (usable, do not balance-uninit).
    let hr = unsafe { CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED) };
    let must_uninit = hr == S_OK || hr == S_FALSE;
    if hr != S_OK && hr != S_FALSE && hr != RPC_E_CHANGED_MODE {
        return Vec::new();
    }

    let out = wua_history_inner(max);

    if must_uninit {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    out
}

/// The WUA walk, with COM already initialized on this thread.
fn wua_history_inner(max: i32) -> Vec<DetectedChange> {
    let mut rows = Vec::new();

    // CoCreateInstance(CLSID_UpdateSession) → IDispatch.
    let mut session: PVOID = null_mut();
    // SAFETY: standard CoCreateInstance; out-param `session` is checked below.
    let hr = unsafe {
        CoCreateInstance(
            &CLSID_UPDATE_SESSION,
            null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_IDISPATCH,
            &mut session,
        )
    };
    if hr != S_OK || session.is_null() {
        return rows;
    }
    let _session = Com(session);

    // session.CreateUpdateSearcher() → searcher (IDispatch).
    let Some(searcher) = invoke_dispatch(session, "CreateUpdateSearcher", DISPATCH_METHOD, &mut [])
    else {
        return rows;
    };
    let _searcher = Com(searcher);

    // searcher.GetTotalHistoryCount() → total.
    let Some(total) = invoke_i4(searcher, "GetTotalHistoryCount", DISPATCH_METHOD, &mut []) else {
        return rows;
    };
    let count = if total < 0 { 0 } else { total.min(max) };
    if count == 0 {
        return rows;
    }

    // searcher.QueryHistory(startIndex=0, count) → collection. Args REVERSED.
    let mut qh_args = [VARIANT::i4(count), VARIANT::i4(0)];
    let Some(coll) = invoke_dispatch(searcher, "QueryHistory", DISPATCH_METHOD, &mut qh_args)
    else {
        return rows;
    };
    let _coll = Com(coll);

    // collection.Count.
    let Some(n) = invoke_i4(coll, "Count", DISPATCH_PROPERTYGET, &mut []) else {
        return rows;
    };

    for i in 0..n {
        let mut item_args = [VARIANT::i4(i)];
        let Some(entry) = invoke_dispatch(coll, "Item", DISPATCH_PROPERTYGET, &mut item_args)
        else {
            continue;
        };
        let _entry = Com(entry);

        let title = invoke_bstr(entry, "Title", DISPATCH_PROPERTYGET, &mut []).unwrap_or_default();
        let result_code =
            invoke_i4(entry, "ResultCode", DISPATCH_PROPERTYGET, &mut []).unwrap_or(0);
        let operation = invoke_i4(entry, "Operation", DISPATCH_PROPERTYGET, &mut []).unwrap_or(0);
        let date = invoke_date(entry, "Date", DISPATCH_PROPERTYGET, &mut []);

        let mut detail = format!(
            "Windows Update {} — {}",
            wua_operation_label(operation),
            wua_result_label(result_code),
        );
        if let Some(ymd) = date.and_then(ole_date_to_ymd) {
            detail.push_str(&format!(" on {ymd}"));
        }
        let subject = if title.is_empty() {
            "(update)".to_string()
        } else {
            title
        };
        rows.push(DetectedChange::new(
            change_kind::WINDOWS_UPDATE,
            subject,
            detail,
            String::new(),
        ));
    }
    rows
}

/// Friendly label for a WUA `UpdateOperation` code.
fn wua_operation_label(op: i32) -> &'static str {
    match op {
        1 => "Installation",
        2 => "Uninstallation",
        _ => "Operation",
    }
}

/// Friendly label for a WUA `OperationResultCode`.
fn wua_result_label(rc: i32) -> &'static str {
    match rc {
        1 => "In progress",
        2 => "Succeeded",
        3 => "Succeeded with errors",
        4 => "Failed",
        5 => "Aborted",
        _ => "Unknown",
    }
}

/// Converts an OLE automation `DATE` to a `YYYY-MM-DD` string (UTC civil date),
/// or `None` for the zero/negative sentinel.
fn ole_date_to_ymd(d: f64) -> Option<String> {
    if d <= 0.0 {
        return None;
    }
    // Days from the OLE epoch (1899-12-30) to the Unix epoch (1970-01-01).
    let unix_days = (d - 25569.0).floor() as i64;
    let (y, m, day) = civil_from_days(unix_days);
    Some(format!("{y:04}-{m:02}-{day:02}"))
}

/// Howard Hinnant's `civil_from_days`: Unix-epoch day count → (year, month,
/// day). Pure integer arithmetic, valid across the proleptic Gregorian range.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// --- IDispatch late-binding helpers -----------------------------------------

/// An owned COM interface pointer, `Release`d on drop via the always-slot-2
/// `IUnknown::Release`.
struct Com(PVOID);

impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: self.0 is a live COM interface; Release once.
            unsafe {
                let v = *(self.0 as *const *const IUnknownVtbl);
                ((*v).Release)(self.0);
            }
        }
    }
}

/// Resolves `name` on `disp` and invokes it with `flags`/`args`, returning the
/// raw out-`VARIANT`. `args` must already be in reverse (last-first) order.
/// `None` on any COM failure (with the out-VARIANT cleared).
fn invoke_named(disp: PVOID, name: &str, flags: u16, args: &mut [VARIANT]) -> Option<VARIANT> {
    if disp.is_null() {
        return None;
    }
    let wname = to_wide(name);
    let names: [*const u16; 1] = [wname.as_ptr()];
    let mut dispid: i32 = 0;
    // SAFETY: disp is a live IDispatch; `names`/`wname` outlive the call.
    let hr = unsafe {
        let v = &**(disp as *const *const IDispatchVtbl);
        (v.get_ids_of_names)(
            disp,
            &IID_NULL,
            names.as_ptr(),
            1,
            LOCALE_SYSTEM_DEFAULT,
            &mut dispid,
        )
    };
    if hr != S_OK {
        return None;
    }

    let mut params = DispParams {
        rgvarg: if args.is_empty() {
            null_mut()
        } else {
            args.as_mut_ptr()
        },
        rgdispid_named_args: null_mut(),
        c_args: args.len() as u32,
        c_named_args: 0,
    };
    let mut result = VARIANT::empty();
    // SAFETY: disp live; `params`/`result` are live locals for the call.
    let hr = unsafe {
        let v = &**(disp as *const *const IDispatchVtbl);
        (v.invoke)(
            disp,
            dispid,
            &IID_NULL,
            LOCALE_SYSTEM_DEFAULT,
            flags,
            &mut params,
            &mut result,
            null_mut(),
            null_mut(),
        )
    };
    if hr != S_OK {
        // SAFETY: Invoke may have partially populated `result`; clear it.
        unsafe { VariantClear(&mut result) };
        return None;
    }
    Some(result)
}

/// Invokes `name` and takes ownership of a `VT_DISPATCH` result pointer (the
/// caller must `Release` it). `None` for any other type (cleared).
fn invoke_dispatch(disp: PVOID, name: &str, flags: u16, args: &mut [VARIANT]) -> Option<PVOID> {
    let mut v = invoke_named(disp, name, flags, args)?;
    if v.vt == VT_DISPATCH {
        let p = v.val as usize as PVOID;
        // Ownership transfers to the caller: do NOT VariantClear (that would
        // Release the interface we are returning).
        if p.is_null() {
            None
        } else {
            Some(p)
        }
    } else {
        // SAFETY: clear an unexpected-type result (may own a resource).
        unsafe { VariantClear(&mut v) };
        None
    }
}

/// Invokes `name` and reads a `VT_I4` result. `None` for any other type.
fn invoke_i4(disp: PVOID, name: &str, flags: u16, args: &mut [VARIANT]) -> Option<i32> {
    let mut v = invoke_named(disp, name, flags, args)?;
    let out = if v.vt == VT_I4 {
        Some(v.val as i32)
    } else {
        None
    };
    // SAFETY: I4 owns nothing; any other type is released here.
    unsafe { VariantClear(&mut v) };
    out
}

/// Invokes `name` and reads a `VT_BSTR` result into an owned `String`, freeing
/// the BSTR. `None` for any other type.
fn invoke_bstr(disp: PVOID, name: &str, flags: u16, args: &mut [VARIANT]) -> Option<String> {
    let mut v = invoke_named(disp, name, flags, args)?;
    if v.vt == VT_BSTR {
        let b = v.val as usize as BSTR;
        let s = bstr_to_string(b);
        if !b.is_null() {
            // SAFETY: b is a valid BSTR we now own; free once (do NOT also
            // VariantClear — that would double-free).
            unsafe { SysFreeString(b) };
        }
        Some(s)
    } else {
        // SAFETY: release an unexpected-type result.
        unsafe { VariantClear(&mut v) };
        None
    }
}

/// Invokes `name` and reads a `VT_DATE` result (OLE automation date). `None`
/// for any other type.
fn invoke_date(disp: PVOID, name: &str, flags: u16, args: &mut [VARIANT]) -> Option<f64> {
    let mut v = invoke_named(disp, name, flags, args)?;
    let out = if v.vt == VT_DATE {
        Some(f64::from_bits(v.val as u64))
    } else {
        None
    };
    // SAFETY: a DATE owns nothing; any other type is released here.
    unsafe { VariantClear(&mut v) };
    out
}

/// Reads a NUL-terminated `BSTR` into a `String` (bounded scan). Empty for null.
fn bstr_to_string(b: BSTR) -> String {
    if b.is_null() {
        return String::new();
    }
    // SAFETY: b is a valid, NUL-terminated BSTR for the duration of the scan.
    unsafe {
        let mut len = 0usize;
        while len < 1_000_000 && *b.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(b, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, version: &str) -> AppEntry {
        AppEntry {
            name: name.to_string(),
            version: version.to_string(),
            publisher: "Acme".to_string(),
        }
    }

    fn svc(name: &str, start_type: &str, account: &str) -> SvcEntry {
        SvcEntry {
            name: name.to_string(),
            display_name: format!("{name} Display"),
            start_type: start_type.to_string(),
            account: account.to_string(),
            binary_path: format!(r"C:\Windows\{name}.exe"),
        }
    }

    fn startup(name: &str, command: &str) -> StartupItem {
        StartupItem {
            name: name.to_string(),
            source: "RunKeyUser".to_string(),
            scope: "user".to_string(),
            command: command.to_string(),
            publisher: String::new(),
        }
    }

    fn task(path: &str) -> TaskItem {
        TaskItem {
            path: path.to_string(),
            action: format!("{path}.exe"),
            enabled: true,
        }
    }

    fn inv_apps(apps: Vec<AppEntry>) -> Inventory {
        Inventory {
            apps,
            ..Default::default()
        }
    }

    fn only(changes: &[DetectedChange]) -> &DetectedChange {
        assert_eq!(changes.len(), 1, "expected exactly one change: {changes:?}");
        &changes[0]
    }

    // --- apps ---------------------------------------------------------------

    #[test]
    fn app_installed() {
        let prev = inv_apps(vec![]);
        let next = inv_apps(vec![app("Foo", "1.0.0")]);
        let c = diff_inventories(&prev, &next);
        let ch = only(&c);
        assert_eq!(ch.kind, change_kind::APP_INSTALLED);
        assert_eq!(ch.subject, "Foo");
        assert_eq!(ch.publisher, "Acme");
        assert!(!ch.reversible);
        assert!(ch.responsible.is_empty());
    }

    #[test]
    fn app_removed() {
        let prev = inv_apps(vec![app("Foo", "1.0.0")]);
        let next = inv_apps(vec![]);
        let c = diff_inventories(&prev, &next);
        let ch = only(&c);
        assert_eq!(ch.kind, change_kind::APP_REMOVED);
        assert_eq!(ch.subject, "Foo");
    }

    #[test]
    fn app_version_bump() {
        let prev = inv_apps(vec![app("Foo", "1.2.3")]);
        let next = inv_apps(vec![app("Foo", "1.2.4")]);
        let c = diff_inventories(&prev, &next);
        let ch = only(&c);
        assert_eq!(ch.kind, change_kind::APP_UPDATED);
        assert_eq!(ch.subject, "Foo");
        assert_eq!(ch.detail, "1.2.3 → 1.2.4");
    }

    #[test]
    fn app_no_change() {
        let prev = inv_apps(vec![app("Foo", "1.2.3"), app("Bar", "2.0")]);
        let next = inv_apps(vec![app("Bar", "2.0"), app("Foo", "1.2.3")]);
        assert!(diff_inventories(&prev, &next).is_empty());
    }

    #[test]
    fn app_name_match_is_case_insensitive() {
        let prev = inv_apps(vec![app("Foo", "1.0")]);
        let next = inv_apps(vec![app("foo", "1.0")]);
        assert!(diff_inventories(&prev, &next).is_empty());
    }

    // --- services -----------------------------------------------------------

    #[test]
    fn service_installed() {
        let prev = Inventory::default();
        let next = Inventory {
            services: vec![svc("Wuauserv", "Auto", "LocalSystem")],
            ..Default::default()
        };
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::SERVICE_INSTALLED);
        assert_eq!(ch.subject, "Wuauserv");
    }

    #[test]
    fn service_removed() {
        let prev = Inventory {
            services: vec![svc("Wuauserv", "Auto", "LocalSystem")],
            ..Default::default()
        };
        let next = Inventory::default();
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::SERVICE_REMOVED);
        assert_eq!(ch.subject, "Wuauserv");
    }

    #[test]
    fn service_config_changed_start_type() {
        let prev = Inventory {
            services: vec![svc("Spooler", "Auto", "LocalSystem")],
            ..Default::default()
        };
        let next = Inventory {
            services: vec![svc("Spooler", "Disabled", "LocalSystem")],
            ..Default::default()
        };
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::SERVICE_CONFIG_CHANGED);
        assert_eq!(ch.subject, "Spooler");
        assert_eq!(ch.detail, "start_type Auto → Disabled");
    }

    #[test]
    fn service_config_changed_account_and_path() {
        let prev = Inventory {
            services: vec![SvcEntry {
                name: "Svc".into(),
                display_name: "Svc".into(),
                start_type: "Auto".into(),
                account: "LocalSystem".into(),
                binary_path: r"C:\a.exe".into(),
            }],
            ..Default::default()
        };
        let next = Inventory {
            services: vec![SvcEntry {
                name: "Svc".into(),
                display_name: "Svc".into(),
                start_type: "Auto".into(),
                account: "NetworkService".into(),
                binary_path: r"C:\b.exe".into(),
            }],
            ..Default::default()
        };
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::SERVICE_CONFIG_CHANGED);
        assert_eq!(
            ch.detail,
            r"account LocalSystem → NetworkService; binary_path C:\a.exe → C:\b.exe"
        );
    }

    #[test]
    fn service_no_change() {
        let inv = Inventory {
            services: vec![svc("A", "Auto", "LocalSystem")],
            ..Default::default()
        };
        assert!(diff_inventories(&inv, &inv).is_empty());
    }

    // --- startup ------------------------------------------------------------

    #[test]
    fn startup_added() {
        let prev = Inventory::default();
        let next = Inventory {
            startup: vec![startup("Steam", r"C:\Steam.exe")],
            ..Default::default()
        };
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::STARTUP_ADDED);
        assert_eq!(ch.subject, "Steam");
        assert_eq!(ch.detail, r"C:\Steam.exe");
    }

    #[test]
    fn startup_removed() {
        let prev = Inventory {
            startup: vec![startup("Steam", r"C:\Steam.exe")],
            ..Default::default()
        };
        let next = Inventory::default();
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::STARTUP_REMOVED);
        assert_eq!(ch.subject, "Steam");
    }

    #[test]
    fn startup_command_change_is_add_plus_remove() {
        // The command is part of the identity key, so editing it is a
        // remove of the old tuple and an add of the new.
        let prev = Inventory {
            startup: vec![startup("App", r"C:\old.exe")],
            ..Default::default()
        };
        let next = Inventory {
            startup: vec![startup("App", r"C:\new.exe")],
            ..Default::default()
        };
        let c = diff_inventories(&prev, &next);
        assert_eq!(c.len(), 2);
        assert!(c.iter().any(|x| x.kind == change_kind::STARTUP_ADDED));
        assert!(c.iter().any(|x| x.kind == change_kind::STARTUP_REMOVED));
    }

    // --- tasks --------------------------------------------------------------

    #[test]
    fn task_added() {
        let prev = Inventory::default();
        let next = Inventory {
            tasks: vec![task(r"\Microsoft\Windows\Foo")],
            ..Default::default()
        };
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::SCHEDULED_TASK_ADDED);
        assert_eq!(ch.subject, r"\Microsoft\Windows\Foo");
    }

    #[test]
    fn task_removed() {
        let prev = Inventory {
            tasks: vec![task(r"\Microsoft\Windows\Foo")],
            ..Default::default()
        };
        let next = Inventory::default();
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::SCHEDULED_TASK_REMOVED);
    }

    // --- power plan ---------------------------------------------------------

    #[test]
    fn power_plan_changed() {
        let prev = Inventory {
            power_plan: "{balanced}".to_string(),
            ..Default::default()
        };
        let next = Inventory {
            power_plan: "{high-perf}".to_string(),
            ..Default::default()
        };
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::POWER_PLAN_CHANGED);
        assert_eq!(ch.subject, "{high-perf}");
        assert_eq!(ch.detail, "{balanced} → {high-perf}");
    }

    #[test]
    fn power_plan_unchanged() {
        let inv = Inventory {
            power_plan: "{balanced}".to_string(),
            ..Default::default()
        };
        assert!(diff_inventories(&inv, &inv).is_empty());
    }

    #[test]
    fn power_plan_empty_next_is_ignored() {
        // A failed read this pass (empty next) must not emit a spurious change.
        let prev = Inventory {
            power_plan: "{balanced}".to_string(),
            ..Default::default()
        };
        let next = Inventory::default();
        assert!(diff_inventories(&prev, &next).is_empty());
    }

    // --- default apps -------------------------------------------------------

    #[test]
    fn default_app_changed() {
        let prev = Inventory {
            default_apps: vec![DefaultAppEntry {
                kind: "http".into(),
                handler: "EdgeHTM".into(),
            }],
            ..Default::default()
        };
        let next = Inventory {
            default_apps: vec![DefaultAppEntry {
                kind: "http".into(),
                handler: "FirefoxURL".into(),
            }],
            ..Default::default()
        };
        let ch = only(&diff_inventories(&prev, &next)).clone();
        assert_eq!(ch.kind, change_kind::DEFAULT_APP_CHANGED);
        assert_eq!(ch.subject, "http");
        assert_eq!(ch.detail, "EdgeHTM → FirefoxURL");
    }

    #[test]
    fn default_app_new_kind_not_reported() {
        // A kind present only in next (not "both known") is not a change.
        let prev = Inventory::default();
        let next = Inventory {
            default_apps: vec![DefaultAppEntry {
                kind: "http".into(),
                handler: "EdgeHTM".into(),
            }],
            ..Default::default()
        };
        assert!(diff_inventories(&prev, &next).is_empty());
    }

    // --- whole-inventory + ordering -----------------------------------------

    #[test]
    fn empty_to_empty_is_nothing() {
        assert!(diff_inventories(&Inventory::default(), &Inventory::default()).is_empty());
    }

    #[test]
    fn identical_inventories_yield_nothing() {
        let inv = Inventory {
            apps: vec![app("Foo", "1.0")],
            services: vec![svc("Svc", "Auto", "LocalSystem")],
            startup: vec![startup("S", r"C:\s.exe")],
            tasks: vec![task(r"\T")],
            power_plan: "{balanced}".to_string(),
            default_apps: vec![DefaultAppEntry {
                kind: "http".into(),
                handler: "EdgeHTM".into(),
            }],
        };
        assert!(diff_inventories(&inv, &inv).is_empty());
    }

    #[test]
    fn categories_emitted_in_fixed_order() {
        let prev = Inventory::default();
        let next = Inventory {
            apps: vec![app("A", "1")],
            services: vec![svc("S", "Auto", "LocalSystem")],
            startup: vec![startup("U", r"C:\u.exe")],
            tasks: vec![task(r"\T")],
            power_plan: "{p}".to_string(),
            default_apps: vec![DefaultAppEntry {
                kind: "http".into(),
                handler: "H".into(),
            }],
        };
        let c = diff_inventories(&prev, &next);
        // default_apps here reports nothing (new kind only), so 5 changes.
        let kinds: Vec<i32> = c.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            vec![
                change_kind::APP_INSTALLED,
                change_kind::SERVICE_INSTALLED,
                change_kind::STARTUP_ADDED,
                change_kind::SCHEDULED_TASK_ADDED,
                change_kind::POWER_PLAN_CHANGED,
            ]
        );
    }

    #[test]
    fn apps_sorted_by_subject_within_category() {
        let prev = Inventory::default();
        let next = inv_apps(vec![
            app("Zebra", "1"),
            app("Apple", "1"),
            app("mango", "1"),
        ]);
        let c = diff_inventories(&prev, &next);
        let subjects: Vec<&str> = c.iter().map(|x| x.subject.as_str()).collect();
        assert_eq!(subjects, vec!["Apple", "mango", "Zebra"]);
    }

    // --- date helper --------------------------------------------------------

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(31), (1970, 2, 1));
    }

    #[test]
    fn ole_date_to_ymd_known_value() {
        // OLE DATE 25569.0 == 1970-01-01.
        assert_eq!(ole_date_to_ymd(25569.0).as_deref(), Some("1970-01-01"));
        // 44197.0 == 2021-01-01.
        assert_eq!(ole_date_to_ymd(44197.0).as_deref(), Some("2021-01-01"));
        // Non-positive sentinel → None.
        assert_eq!(ole_date_to_ymd(0.0), None);
    }
}
