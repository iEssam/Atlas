//! Scheduled tasks via the Task Scheduler 2.0 COM API (PRD §9.9.2,
//! docs/phases.md Phase 2).
//!
//! `CoCreateInstance(CLSID_TaskScheduler)` yields an `ITaskService`; after
//! `Connect` (local, current user) we walk the folder tree from the root,
//! reading each `IRegisteredTask`'s live properties (name, path, enabled,
//! last-run, last-result, next-run) via hand-written vtable calls. The *static*
//! definition — author, run level, idle/wake settings, the primary action, and
//! the trigger set — is parsed from the task's registration XML
//! (`IRegisteredTask::get_Xml`) rather than the deep `ITaskDefinition` interface
//! tree: the XML carries exactly those fields and needs no extra COM surface.
//!
//! COM is confined to the calling thread: `CoInitializeEx` at entry,
//! `CoUninitialize` at exit (skipped only when the thread already had a
//! different apartment model). Every call is a read; nothing is registered,
//! deleted, or run. SCM-style two-call sizing does not apply — COM getters
//! allocate their own BSTRs, which we copy out and `SysFreeString`.

#![cfg(windows)]

use std::ptr::null_mut;

use crate::ffi::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, ICollectionVtbl, IRegisteredTaskVtbl,
    ITaskFolderVtbl, ITaskServiceVtbl, IUnknownVtbl, SysAllocString, SysFreeString, BSTR,
    CLSCTX_INPROC_SERVER, CLSID_TASK_SCHEDULER, COINIT_APARTMENTTHREADED, DATE, IID_ITASK_SERVICE,
    LONG, PVOID, RPC_E_CHANGED_MODE, S_FALSE, S_OK, VARIANT, VARIANT_BOOL,
};

/// One scheduled-task row. Mirrors the proto `ScheduledTask` field-for-field.
#[derive(Debug, Clone, Default)]
pub struct ScheduledTask {
    pub name: String,
    /// Full task path incl. folder, e.g. `\Microsoft\Windows\Foo`.
    pub path: String,
    /// The containing folder path, e.g. `\Microsoft\Windows`.
    pub folder: String,
    pub enabled: bool,
    /// Human-readable trigger summary.
    pub triggers: String,
    /// Executable + arguments of the primary (Exec) action.
    pub action: String,
    pub last_run_ms: i64,
    pub next_run_ms: i64,
    pub last_result: i32,
    pub author: String,
    pub run_as_highest: bool,
    pub runs_on_idle: bool,
    pub wakes_to_run: bool,
}

/// Case-insensitive substring filter over a task's name and full path. Empty
/// filter matches everything.
pub fn matches_filter(filter: &str, name: &str, path: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_ascii_lowercase();
    name.to_ascii_lowercase().contains(&f) || path.to_ascii_lowercase().contains(&f)
}

/// Enumerates scheduled tasks matching `filter`. Best-effort: if COM init or the
/// Task Scheduler connection fails, returns an empty list (the caller degrades
/// honestly via the capability flag). Never panics — a task whose XML or a live
/// property cannot be read still appears with the fields that resolved.
pub fn enumerate_tasks(filter: &str) -> Vec<ScheduledTask> {
    // SAFETY: initialize COM for this thread. S_OK/S_FALSE both mean usable and
    // must be balanced by CoUninitialize; RPC_E_CHANGED_MODE means a different
    // apartment is already active (usable, do not balance-uninit).
    let hr = unsafe { CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED) };
    let must_uninit = hr == S_OK || hr == S_FALSE;
    if hr != S_OK && hr != S_FALSE && hr != RPC_E_CHANGED_MODE {
        return Vec::new();
    }

    let out = enumerate_inner(filter);

    if must_uninit {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    out
}

/// The COM walk, run with COM already initialized on this thread.
fn enumerate_inner(filter: &str) -> Vec<ScheduledTask> {
    // Create the Task Scheduler service object.
    let mut svc: PVOID = null_mut();
    // SAFETY: standard CoCreateInstance; out-param `svc` is live.
    let hr = unsafe {
        CoCreateInstance(
            &CLSID_TASK_SCHEDULER,
            null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ITASK_SERVICE,
            &mut svc,
        )
    };
    if hr != S_OK || svc.is_null() {
        return Vec::new();
    }
    let _svc_guard = ComPtr(svc);

    // Connect to the local scheduler as the current user (all-empty VARIANTs).
    // SAFETY: svc is a valid ITaskService; Connect takes four VT_EMPTY args.
    let hr = unsafe {
        let v = *(svc as *const *const ITaskServiceVtbl);
        ((*v).Connect)(
            svc,
            VARIANT::empty(),
            VARIANT::empty(),
            VARIANT::empty(),
            VARIANT::empty(),
        )
    };
    if hr != S_OK {
        return Vec::new();
    }

    // Root folder "\".
    let root = match svc_get_folder(svc, "\\") {
        Some(f) => f,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    // Iterative folder walk (explicit stack — task trees can nest a few levels).
    let mut stack: Vec<ComPtr> = vec![ComPtr(root)];
    while let Some(folder) = stack.pop() {
        collect_folder_tasks(folder.0, filter, &mut out);
        // Push subfolders.
        for sub in folder_subfolders(folder.0) {
            stack.push(ComPtr(sub));
        }
        // `folder` (ComPtr) releases here on drop.
    }
    out
}

/// Reads every task in `folder`, appending those matching `filter` to `out`.
fn collect_folder_tasks(folder: PVOID, filter: &str, out: &mut Vec<ScheduledTask>) {
    let folder_path = folder_path(folder);
    let tasks = match folder_get_tasks(folder) {
        Some(c) => ComPtr(c),
        None => return,
    };
    let count = coll_count(tasks.0);
    for i in 1..=count {
        let item = match coll_item(tasks.0, i) {
            Some(t) => ComPtr(t),
            None => continue,
        };
        if let Some(task) = read_task(item.0, &folder_path) {
            if matches_filter(filter, &task.name, &task.path) {
                out.push(task);
            }
        }
    }
}

/// Reads one `IRegisteredTask` into a [`ScheduledTask`]. Live properties come
/// from the interface; the static definition from the task XML.
fn read_task(task: PVOID, folder_path: &str) -> Option<ScheduledTask> {
    // SAFETY: `task` is a valid IRegisteredTask for the duration of these calls.
    let v = unsafe { &**(task as *const *const IRegisteredTaskVtbl) };

    let name = take_bstr_out(|p| unsafe { (v.get_Name)(task, p) });
    let path = take_bstr_out(|p| unsafe { (v.get_Path)(task, p) });
    if name.is_empty() && path.is_empty() {
        return None;
    }

    // Enabled (VARIANT_BOOL: -1 true / 0 false).
    let mut enabled_vb: VARIANT_BOOL = 0;
    // SAFETY: out-param is a live VARIANT_BOOL.
    let enabled = unsafe { (v.get_Enabled)(task, &mut enabled_vb) == S_OK && enabled_vb != 0 };

    // Last/next run DATEs and results (best-effort; failures leave defaults).
    let mut last_run: DATE = 0.0;
    let mut next_run: DATE = 0.0;
    let mut last_result: LONG = 0;
    // SAFETY: each out-param is a live local of the matching type.
    unsafe {
        let _ = (v.get_LastRunTime)(task, &mut last_run);
        let _ = (v.get_NextRunTime)(task, &mut next_run);
        let _ = (v.get_LastTaskResult)(task, &mut last_result);
    }

    // Static definition from the registration XML.
    let xml = take_bstr_out(|p| unsafe { (v.get_Xml)(task, p) });
    let def = parse_definition(&xml);

    Some(ScheduledTask {
        name,
        path,
        folder: folder_path.to_string(),
        enabled,
        triggers: def.triggers,
        action: def.action,
        last_run_ms: date_to_ms(last_run),
        next_run_ms: date_to_ms(next_run),
        last_result,
        author: def.author,
        run_as_highest: def.run_as_highest,
        runs_on_idle: def.runs_on_idle,
        wakes_to_run: def.wakes_to_run,
    })
}

// --- ITaskService / ITaskFolder / collection thin wrappers ------------------

/// `ITaskService::GetFolder(path)`.
fn svc_get_folder(svc: PVOID, path: &str) -> Option<PVOID> {
    let bpath = SysString::new(path);
    let mut out: PVOID = null_mut();
    // SAFETY: svc valid; bpath is a live BSTR; out is a live out-param.
    let hr = unsafe {
        let v = *(svc as *const *const ITaskServiceVtbl);
        ((*v).GetFolder)(svc, bpath.0, &mut out)
    };
    if hr == S_OK && !out.is_null() {
        Some(out)
    } else {
        None
    }
}

/// `ITaskFolder::get_Path`.
fn folder_path(folder: PVOID) -> String {
    // SAFETY: folder is a valid ITaskFolder.
    let v = unsafe { &**(folder as *const *const ITaskFolderVtbl) };
    take_bstr_out(|p| unsafe { (v.get_Path)(folder, p) })
}

/// `ITaskFolder::GetTasks(0)` → an `IRegisteredTaskCollection`.
fn folder_get_tasks(folder: PVOID) -> Option<PVOID> {
    let mut out: PVOID = null_mut();
    // SAFETY: folder valid; out live. Flags 0 = do not include hidden tasks
    // beyond the default (TASK_ENUM_HIDDEN would be 1; the default view suffices).
    let hr = unsafe {
        let v = &**(folder as *const *const ITaskFolderVtbl);
        (v.GetTasks)(folder, 1, &mut out) // 1 = TASK_ENUM_HIDDEN: include hidden
    };
    if hr == S_OK && !out.is_null() {
        Some(out)
    } else {
        None
    }
}

/// `ITaskFolder::GetFolders(0)` → the subfolders as owned interface pointers.
fn folder_subfolders(folder: PVOID) -> Vec<PVOID> {
    let mut coll: PVOID = null_mut();
    // SAFETY: folder valid; out live.
    let hr = unsafe {
        let v = &**(folder as *const *const ITaskFolderVtbl);
        (v.GetFolders)(folder, 0, &mut coll)
    };
    if hr != S_OK || coll.is_null() {
        return Vec::new();
    }
    let coll = ComPtr(coll);
    let count = coll_count(coll.0);
    let mut out = Vec::new();
    for i in 1..=count {
        if let Some(sub) = coll_item(coll.0, i) {
            out.push(sub);
        }
    }
    out
}

/// `get_Count` on a Task Scheduler collection (folder or task collection).
fn coll_count(coll: PVOID) -> i32 {
    let mut n: LONG = 0;
    // SAFETY: coll is a valid collection; n is live.
    let hr = unsafe {
        let v = &**(coll as *const *const ICollectionVtbl);
        (v.get_Count)(coll, &mut n)
    };
    if hr == S_OK {
        n
    } else {
        0
    }
}

/// `get_Item(VARIANT index)` (1-based) on a Task Scheduler collection.
fn coll_item(coll: PVOID, index: i32) -> Option<PVOID> {
    let mut out: PVOID = null_mut();
    // SAFETY: coll valid; VT_I4 index variant; out live.
    let hr = unsafe {
        let v = &**(coll as *const *const ICollectionVtbl);
        (v.get_Item)(coll, VARIANT::i4(index), &mut out)
    };
    if hr == S_OK && !out.is_null() {
        Some(out)
    } else {
        None
    }
}

// --- COM RAII + BSTR helpers ------------------------------------------------

/// An owned COM interface pointer, `Release`d on drop (via the always-slot-2
/// `IUnknown::Release`).
struct ComPtr(PVOID);

impl Drop for ComPtr {
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

/// An owned `BSTR` built from a Rust string, freed on drop.
struct SysString(BSTR);

impl SysString {
    fn new(s: &str) -> Self {
        let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide is NUL-terminated UTF-16.
        SysString(unsafe { SysAllocString(wide.as_ptr()) })
    }
}

impl Drop for SysString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: self.0 came from SysAllocString; free once.
            unsafe { SysFreeString(self.0) };
        }
    }
}

/// Runs a getter that writes a `BSTR` out-param, copies it to a `String`, and
/// frees the BSTR. Returns empty on failure.
fn take_bstr_out(getter: impl FnOnce(*mut BSTR) -> crate::ffi::HRESULT) -> String {
    let mut b: BSTR = null_mut();
    let hr = getter(&mut b);
    if hr != S_OK || b.is_null() {
        if !b.is_null() {
            // SAFETY: free even on a non-S_OK that still allocated.
            unsafe { SysFreeString(b) };
        }
        return String::new();
    }
    // SAFETY: b is a valid BSTR (NUL-terminated); read then free once.
    let s = unsafe { bstr_to_string(b) };
    unsafe { SysFreeString(b) };
    s
}

/// Reads a NUL-terminated `BSTR` into a `String` (bounded scan).
///
/// # Safety
/// `b` must be a valid, NUL-terminated BSTR pointer.
unsafe fn bstr_to_string(b: BSTR) -> String {
    if b.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < 1_000_000 && *b.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(b, len))
}

/// Converts an OLE automation `DATE` to Unix-epoch milliseconds. A DATE at or
/// before the 1899-12-30 epoch (the "never" sentinel Task Scheduler returns for
/// an unrun/next-less task) maps to 0.
fn date_to_ms(d: DATE) -> i64 {
    if d <= 0.0 {
        return 0;
    }
    // Days between 1899-12-30 (OLE epoch) and 1970-01-01 (Unix epoch).
    const OLE_UNIX_DAY_OFFSET: f64 = 25569.0;
    let secs = (d - OLE_UNIX_DAY_OFFSET) * 86_400.0;
    if secs <= 0.0 {
        return 0;
    }
    (secs * 1000.0).round() as i64
}

// --- Registration-XML parsing (static definition) ---------------------------

/// The static fields pulled from a task's registration XML.
#[derive(Default)]
struct XmlDefinition {
    author: String,
    action: String,
    triggers: String,
    run_as_highest: bool,
    runs_on_idle: bool,
    wakes_to_run: bool,
}

/// Parses the registration XML for the static definition fields. Tolerant of
/// namespaces/attributes: it matches on the local tag name and takes the first
/// occurrence, which is unambiguous for the fields read here.
fn parse_definition(xml: &str) -> XmlDefinition {
    let author = tag_text(xml, "Author").unwrap_or_default();
    let run_level = tag_text(xml, "RunLevel").unwrap_or_default();
    let run_as_highest = run_level.to_ascii_lowercase().contains("highest");
    let runs_on_idle = tag_bool(xml, "RunOnlyIfIdle");
    let wakes_to_run = tag_bool(xml, "WakeToRun");
    let action = exec_action(xml);
    let triggers = trigger_summary(xml);
    XmlDefinition {
        author,
        action,
        triggers,
        run_as_highest,
        runs_on_idle,
        wakes_to_run,
    }
}

/// Returns the trimmed inner text of the first `<tag ...>...</tag>` (local name
/// match, attributes allowed), or `None`.
fn tag_text(xml: &str, tag: &str) -> Option<String> {
    // Find `<tag` followed by a delimiter (space, > or /) so `<Author` does not
    // match `<AuthorX`.
    let open_lt = format!("<{tag}");
    let mut search = 0;
    loop {
        let rel = xml[search..].find(&open_lt)?;
        let start = search + rel;
        let after = start + open_lt.len();
        let next = xml.as_bytes().get(after).copied();
        // Must be a real tag boundary.
        if !matches!(
            next,
            Some(b' ') | Some(b'>') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b'/')
        ) {
            search = after;
            continue;
        }
        // End of the open tag.
        let gt = xml[start..].find('>')? + start;
        // Self-closing (`<tag/>`) → empty.
        if xml.as_bytes().get(gt.saturating_sub(1)).copied() == Some(b'/') {
            return Some(String::new());
        }
        let content_start = gt + 1;
        let close = format!("</{tag}>");
        let close_rel = xml[content_start..].find(&close)?;
        let inner = &xml[content_start..content_start + close_rel];
        return Some(inner.trim().to_string());
    }
}

/// Parses a boolean-valued tag (`<Tag>true</Tag>`); missing/other → false.
fn tag_bool(xml: &str, tag: &str) -> bool {
    matches!(
        tag_text(xml, tag).as_deref().map(str::trim),
        Some("true") | Some("1")
    )
}

/// Builds the primary action string from the first `<Exec>` block:
/// `Command [Arguments]`. Empty when there is no Exec action (e.g. a
/// COM-handler or e-mail action, which we summarise generically).
fn exec_action(xml: &str) -> String {
    // Scope to the first <Exec>...</Exec> so we take that action's Command, not
    // some other element's.
    if let Some(exec_start) = xml.find("<Exec") {
        let rest = &xml[exec_start..];
        let exec_end = rest
            .find("</Exec>")
            .map(|e| e + "</Exec>".len())
            .unwrap_or(rest.len());
        let block = &rest[..exec_end];
        let cmd = tag_text(block, "Command").unwrap_or_default();
        let args = tag_text(block, "Arguments").unwrap_or_default();
        if cmd.is_empty() {
            return String::new();
        }
        return if args.is_empty() {
            cmd
        } else {
            format!("{cmd} {args}")
        };
    }
    // Non-Exec actions: name the action type if we can see one.
    if xml.contains("<ComHandler") {
        return "(COM handler action)".to_string();
    }
    if xml.contains("<SendEmail") {
        return "(e-mail action)".to_string();
    }
    if xml.contains("<ShowMessage") {
        return "(show-message action)".to_string();
    }
    String::new()
}

/// Summarises the trigger set from the `<Triggers>` block as a human list of
/// trigger kinds. `On demand` when there are no triggers.
fn trigger_summary(xml: &str) -> String {
    let block = between(xml, "<Triggers>", "</Triggers>").unwrap_or("");
    // (element tag, friendly label) in the Task Scheduler schema.
    const KINDS: &[(&str, &str)] = &[
        ("BootTrigger", "At startup"),
        ("LogonTrigger", "At logon"),
        ("TimeTrigger", "One time"),
        ("CalendarTrigger", "On a schedule"),
        ("IdleTrigger", "On idle"),
        ("EventTrigger", "On an event"),
        ("RegistrationTrigger", "On registration"),
        ("SessionStateChangeTrigger", "On session change"),
        ("WnfStateChangeTrigger", "On system-state change"),
    ];
    let mut labels: Vec<&str> = Vec::new();
    for (tag, label) in KINDS {
        if block.contains(&format!("<{tag}")) && !labels.contains(label) {
            labels.push(label);
        }
    }
    if labels.is_empty() {
        "On demand".to_string()
    } else {
        labels.join(", ")
    }
}

/// The substring strictly between the first `open` and the following `close`.
fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let a = s.find(open)? + open.len();
    let b = s[a..].find(close)? + a;
    Some(&s[a..b])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_name_and_path_case_insensitive() {
        assert!(matches_filter("", "Anything", "\\Any"));
        assert!(matches_filter("micro", "Foo", "\\Microsoft\\Windows\\Foo"));
        assert!(matches_filter("FOO", "Foo", "\\Bar\\Foo"));
        assert!(!matches_filter("zzz", "Foo", "\\Bar\\Foo"));
    }

    #[test]
    fn date_conversion_and_never_sentinel() {
        assert_eq!(date_to_ms(0.0), 0, "1899-12-30 sentinel → 0");
        assert_eq!(date_to_ms(-5.0), 0, "negative → 0");
        // 1970-01-01 00:00 is DATE 25569 → 0 ms.
        assert_eq!(date_to_ms(25569.0), 0);
        // 1970-01-02 00:00 → 86_400_000 ms.
        assert_eq!(date_to_ms(25570.0), 86_400_000);
    }

    #[test]
    fn xml_tag_text_extracts_first_and_trims() {
        let xml = r#"<Task><RegistrationInfo><Author> Microsoft Corporation </Author>
            <Description>x</Description></RegistrationInfo></Task>"#;
        assert_eq!(
            tag_text(xml, "Author").as_deref(),
            Some("Microsoft Corporation")
        );
        assert_eq!(tag_text(xml, "Missing"), None);
    }

    #[test]
    fn xml_tag_boundary_not_prefix() {
        // `<AuthorX>` must not satisfy a search for `Author`.
        let xml = "<AuthorExtra>nope</AuthorExtra><Author>yes</Author>";
        assert_eq!(tag_text(xml, "Author").as_deref(), Some("yes"));
    }

    #[test]
    fn xml_run_level_and_bools() {
        let xml = r#"<Principals><Principal id="Author">
            <RunLevel>HighestAvailable</RunLevel></Principal></Principals>
            <Settings><RunOnlyIfIdle>true</RunOnlyIfIdle>
            <WakeToRun>false</WakeToRun></Settings>"#;
        let def = parse_definition(xml);
        assert!(def.run_as_highest);
        assert!(def.runs_on_idle);
        assert!(!def.wakes_to_run);
    }

    #[test]
    fn xml_exec_action_command_and_args() {
        let xml = r#"<Actions Context="Author"><Exec>
            <Command>C:\Windows\System32\foo.exe</Command>
            <Arguments>/run --now</Arguments></Exec></Actions>"#;
        assert_eq!(exec_action(xml), r"C:\Windows\System32\foo.exe /run --now");

        let no_args = r#"<Exec><Command>bar.exe</Command></Exec>"#;
        assert_eq!(exec_action(no_args), "bar.exe");

        let com = r#"<Actions><ComHandler><ClassId>{...}</ClassId></ComHandler></Actions>"#;
        assert_eq!(exec_action(com), "(COM handler action)");
    }

    #[test]
    fn xml_trigger_summary_lists_kinds() {
        let xml = r#"<Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger>
            <CalendarTrigger><ScheduleByDay/></CalendarTrigger></Triggers>"#;
        assert_eq!(trigger_summary(xml), "At logon, On a schedule");

        assert_eq!(trigger_summary("<Task></Task>"), "On demand");
    }
}
