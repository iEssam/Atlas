//! Deep process inspector (docs/phases.md Phase 2, PRD §9.4): on-demand process
//! detail, module list, and thread list.
//!
//! `process_detail` assembles a full identity from the whole-system snapshot
//! (pid/parent/times/session/thread+handle counts — always available) plus
//! on-demand augmentation through a `PROCESS_QUERY_LIMITED_INFORMATION` handle:
//! image path, architecture, package identity, token user/integrity/elevation,
//! command line (`ProcessCommandLineInformation`) and working directory (PEB
//! read), and version/signature of the image. Any augmentation that a
//! cross-user/protected process denies sets `limited=true` and is skipped — the
//! detail is never failed wholesale (PRD §9.6.7 honesty). A pid absent from the
//! snapshot is reported `available=false` ("process exited").
//!
//! `list_modules` uses `EnumProcessModulesEx` (needs QUERY_INFORMATION|VM_READ;
//! same-user unprivileged) and reports `available=false` when access is denied.
//! `list_threads` maps the snapshot's trailing thread array (no extra rights).

#![cfg(windows)]

use crate::ffi::{
    CloseHandle, ConvertSidToStringSidW, EnumProcessModulesEx, GetLastError, GetModuleFileNameExW,
    GetModuleInformation, GetPackageFullName, GetSidSubAuthority, GetSidSubAuthorityCount,
    GetTokenInformation, IsWow64Process2, LocalFree, LookupAccountSidW, NtQueryInformationProcess,
    OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, ReadProcessMemory,
    APPMODEL_ERROR_NO_PACKAGE, DWORD, HANDLE, HMODULE, IMAGE_FILE_MACHINE_AMD64,
    IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_UNKNOWN,
    LIST_MODULES_ALL, MODULEINFO, PROCESS_BASIC_INFORMATION, PROCESS_BASIC_INFORMATION_CLASS,
    PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    SECURITY_MANDATORY_HIGH_RID, SECURITY_MANDATORY_LOW_RID, SECURITY_MANDATORY_MEDIUM_RID,
    SECURITY_MANDATORY_SYSTEM_RID, TOKEN_ELEVATION, TOKEN_ELEVATION_CLASS,
    TOKEN_INTEGRITY_LEVEL_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TOKEN_USER_CLASS,
    UNICODE_STRING, USHORT,
};
use crate::snapshot::{snapshot_processes, snapshot_thread_infos};
use crate::winver::{read_version_info, verify_signature};

/// `ProcessCommandLineInformation` info class for `NtQueryInformationProcess`.
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;

/// 64-bit PEB / process-parameters field offsets (ntdll-stable). Used to read
/// the working directory out of the target's process parameters.
const PEB_PROCESS_PARAMETERS_OFFSET: usize = 0x20;
const RTL_UPP_CURRENT_DIRECTORY_OFFSET: usize = 0x38;

/// 100 ns intervals between the FILETIME epoch (1601) and the Unix epoch (1970).
const FILETIME_UNIX_EPOCH_DELTA_100NS: i64 = 116_444_736_000_000_000;

/// Full process detail — mirrors the proto `ProcessDetail` field-for-field so
/// the service mapping is a straight copy. Every augmented field is best-effort;
/// `limited` is set when a cross-user/protected field could not be read.
#[derive(Debug, Clone, Default)]
pub struct ProcessDetail {
    pub pid: u32,
    pub parent_pid: u32,
    pub create_time_100ns: i64,
    pub image_name: String,
    pub image_path: String,
    pub command_line: String,
    pub working_directory: String,
    pub user_sid: String,
    pub user_name: String,
    pub session_id: u32,
    pub integrity_level: String,
    pub elevated: bool,
    pub architecture: String,
    pub signature_status: String,
    pub publisher: String,
    pub file_version: String,
    pub product_name: String,
    pub thread_count: u32,
    pub handle_count: u32,
    pub start_time_ms: i64,
    pub package_identity: String,
    pub limited: bool,
}

/// Outcome of a detail request: mirrors the proto reply (identity always comes
/// from the snapshot, so `available=false` means the pid is gone).
#[derive(Debug, Clone)]
pub struct ProcessDetailResult {
    pub available: bool,
    pub unavailable_reason: String,
    pub detail: Option<ProcessDetail>,
}

/// One loaded module — mirrors the proto `ModuleRow`.
#[derive(Debug, Clone, Default)]
pub struct ModuleInfo {
    pub name: String,
    pub path: String,
    pub base_address: u64,
    pub size: u64,
    pub version: String,
    pub publisher: String,
    pub signed: bool,
}

/// Result of a module request — mirrors the proto `ListModulesReply`.
#[derive(Debug, Clone)]
pub struct ModulesResult {
    pub available: bool,
    pub unavailable_reason: String,
    pub modules: Vec<ModuleInfo>,
}

/// One thread — mirrors the proto `ThreadRow`.
#[derive(Debug, Clone)]
pub struct ThreadDetail {
    pub tid: u32,
    pub start_address: u64,
    pub state: String,
    pub wait_reason: String,
    pub priority: i32,
    pub cpu_permille: u32,
    pub user_time_100ns: i64,
    pub kernel_time_100ns: i64,
    pub context_switches: u32,
}

/// RAII wrapper closing an `OpenProcess`/token handle on drop.
struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: opened by us (OpenProcess/OpenProcessToken), closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Converts a FILETIME-units create time (100 ns since 1601) to Unix epoch ms.
/// A zero/negative delta (e.g. the idle process) clamps to 0.
pub fn filetime_100ns_to_unix_ms(create_time_100ns: i64) -> i64 {
    let delta = create_time_100ns - FILETIME_UNIX_EPOCH_DELTA_100NS;
    if delta <= 0 {
        0
    } else {
        delta / 10_000
    }
}

/// Maps a mandatory integrity RID to its label (PRD §9.4 wording).
pub fn integrity_label(rid: u32) -> &'static str {
    if rid >= SECURITY_MANDATORY_SYSTEM_RID {
        "System"
    } else if rid >= SECURITY_MANDATORY_HIGH_RID {
        "High"
    } else if rid >= SECURITY_MANDATORY_MEDIUM_RID {
        "Medium"
    } else if rid >= SECURITY_MANDATORY_LOW_RID {
        "Low"
    } else {
        "Untrusted"
    }
}

/// Maps `IsWow64Process2`'s (process, native) machine pair to an architecture
/// label. A non-UNKNOWN process machine means the process is emulated (its own
/// bitness differs from the host); otherwise it runs natively.
pub fn architecture_label(process_machine: u16, native_machine: u16) -> &'static str {
    let machine = if process_machine == IMAGE_FILE_MACHINE_UNKNOWN {
        native_machine
    } else {
        process_machine
    };
    match machine {
        IMAGE_FILE_MACHINE_I386 => "x86",
        IMAGE_FILE_MACHINE_AMD64 => "x64",
        IMAGE_FILE_MACHINE_ARM64 => "Arm64",
        _ => "",
    }
}

/// Maps a `KTHREAD_STATE` code to text.
pub fn thread_state_label(state: u32) -> &'static str {
    match state {
        0 => "Initialized",
        1 => "Ready",
        2 => "Running",
        3 => "Standby",
        4 => "Terminated",
        5 => "Waiting",
        6 => "Transition",
        7 => "DeferredReady",
        8 => "GateWait",
        _ => "Unknown",
    }
}

/// Maps a `KWAIT_REASON` code to text (common reasons named; rest generic). Only
/// meaningful while a thread is Waiting.
pub fn wait_reason_label(reason: u32) -> &'static str {
    match reason {
        0 => "Executive",
        1 => "FreePage",
        2 => "PageIn",
        3 => "PoolAllocation",
        4 => "DelayExecution",
        5 => "Suspended",
        6 => "UserRequest",
        7 => "WrExecutive",
        8 => "WrFreePage",
        9 => "WrPageIn",
        11 => "WrDelayExecution",
        13 => "WrUserRequest",
        15 => "WrQueue",
        27 => "WrDispatchInt",
        _ => "Other",
    }
}

/// Assembles the full [`ProcessDetail`] for `pid`, guarding PID reuse with
/// `create_time_100ns` when it is nonzero. Never escalates; unreadable fields
/// set `limited` and are skipped.
pub fn process_detail(pid: u32, create_time_100ns: i64) -> ProcessDetailResult {
    // Identity comes from the whole-system snapshot (lists every process,
    // unprivileged). A pid absent here has exited.
    let procs = match snapshot_processes() {
        Ok(p) => p,
        Err(e) => {
            return ProcessDetailResult {
                available: false,
                unavailable_reason: format!("snapshot failed: {e}"),
                detail: None,
            }
        }
    };
    let row = procs.iter().find(|p| {
        p.pid == pid && (create_time_100ns == 0 || p.create_time_100ns == create_time_100ns)
    });
    let row = match row {
        Some(r) => r,
        None => {
            return ProcessDetailResult {
                available: false,
                unavailable_reason: "process exited".to_string(),
                detail: None,
            }
        }
    };

    let mut detail = ProcessDetail {
        pid: row.pid,
        parent_pid: row.parent_pid,
        create_time_100ns: row.create_time_100ns,
        image_name: row.image_name.clone(),
        session_id: row.session_id,
        thread_count: row.thread_count,
        handle_count: row.handle_count,
        start_time_ms: filetime_100ns_to_unix_ms(row.create_time_100ns),
        ..Default::default()
    };

    // Augment via a limited-query handle. Its failure (protected/cross-user
    // without elevation) leaves the snapshot identity intact but limited.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        detail.limited = true;
        return ProcessDetailResult {
            available: true,
            unavailable_reason: String::new(),
            detail: Some(detail),
        };
    }
    let h = OwnedHandle(h);

    // Image path (QUERY_LIMITED_INFORMATION is enough).
    match query_full_image_name(h.0) {
        Some(path) => detail.image_path = path,
        None => detail.limited = true,
    }

    // Architecture.
    match query_architecture(h.0) {
        Some(arch) => detail.architecture = arch.to_string(),
        None => detail.limited = true,
    }

    // Package identity (empty for desktop apps — not a limitation).
    detail.package_identity = query_package_identity(h.0);

    // Token: user SID/name, integrity, elevation.
    if !fill_token_fields(h.0, &mut detail) {
        detail.limited = true;
    }

    // Command line via ProcessCommandLineInformation (no VM read needed).
    match query_command_line(h.0) {
        Some(cmd) => detail.command_line = cmd,
        None => detail.limited = true,
    }

    // Working directory via a PEB read (needs VM_READ; best-effort).
    match query_working_directory(pid) {
        Some(cwd) => detail.working_directory = cwd,
        None => detail.limited = true,
    }

    // Version + signature from the on-disk image (once the path is known).
    if !detail.image_path.is_empty() {
        if let Some(vi) = read_version_info(&detail.image_path) {
            detail.file_version = vi.file_version.clone();
            detail.product_name = vi.product_name.clone();
            detail.publisher = vi.company_name.clone();
            detail.signature_status =
                verify_signature(&detail.image_path).to_label(&vi.company_name);
        } else {
            detail.signature_status = verify_signature(&detail.image_path).to_label("");
        }
    }

    ProcessDetailResult {
        available: true,
        unavailable_reason: String::new(),
        detail: Some(detail),
    }
}

/// `QueryFullProcessImageNameW` (Win32 path form).
fn query_full_image_name(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = buf.len() as DWORD;
    // SAFETY: buf/size are live; the API writes at most `size` units and updates it.
    let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size) };
    if ok == 0 || size == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..size as usize]))
}

/// Architecture via `IsWow64Process2`.
fn query_architecture(h: HANDLE) -> Option<&'static str> {
    let mut process_machine: USHORT = 0;
    let mut native_machine: USHORT = 0;
    // SAFETY: both out-params are live locals.
    let ok = unsafe { IsWow64Process2(h, &mut process_machine, &mut native_machine) };
    if ok == 0 {
        return None;
    }
    let label = architecture_label(process_machine, native_machine);
    if label.is_empty() {
        None
    } else {
        Some(label)
    }
}

/// MSIX/AppX package full name, or empty for a desktop app.
fn query_package_identity(h: HANDLE) -> String {
    let mut len: u32 = 0;
    // SAFETY: probe with a null buffer to learn the length.
    let rc = unsafe { GetPackageFullName(h, &mut len, std::ptr::null_mut()) };
    if rc == APPMODEL_ERROR_NO_PACKAGE || len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: buf sized to the reported length; len updated in place.
    let rc = unsafe { GetPackageFullName(h, &mut len, buf.as_mut_ptr()) };
    if rc != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Fills the token-derived fields (user SID/name, integrity, elevation). Returns
/// false when the token could not be opened/read (a limitation).
fn fill_token_fields(h: HANDLE, detail: &mut ProcessDetail) -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: token out-param is live; TOKEN_QUERY is read-only.
    let ok = unsafe { OpenProcessToken(h, TOKEN_QUERY, &mut token) };
    if ok == 0 || token.is_null() {
        return false;
    }
    let token = OwnedHandle(token);

    let mut any = false;

    // TokenUser → SID string + friendly name.
    if let Some(user_buf) = get_token_information(token.0, TOKEN_USER_CLASS) {
        // SAFETY: buffer head is a TOKEN_USER whose Sid points into the buffer.
        let tu = unsafe { &*(user_buf.as_ptr() as *const TOKEN_USER) };
        let sid = tu.User.Sid;
        if !sid.is_null() {
            if let Some(s) = sid_to_string(sid) {
                detail.user_sid = s;
                any = true;
            }
            if let Some(n) = lookup_account_name(sid) {
                detail.user_name = n;
                any = true;
            }
        }
    }

    // TokenIntegrityLevel → the label SID's last sub-authority (the RID).
    if let Some(label_buf) = get_token_information(token.0, TOKEN_INTEGRITY_LEVEL_CLASS) {
        // SAFETY: buffer head is a TOKEN_MANDATORY_LABEL whose Sid points into it.
        let ml = unsafe { &*(label_buf.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
        let sid = ml.Label.Sid;
        if !sid.is_null() {
            // SAFETY: sid is a valid PSID from the token buffer.
            let count = unsafe { *GetSidSubAuthorityCount(sid) };
            if count > 0 {
                // SAFETY: last sub-authority index is count-1, in range.
                let rid = unsafe { *GetSidSubAuthority(sid, (count - 1) as DWORD) };
                detail.integrity_level = integrity_label(rid).to_string();
                any = true;
            }
        }
    }

    // TokenElevation → elevated flag.
    if let Some(elev_buf) = get_token_information(token.0, TOKEN_ELEVATION_CLASS) {
        // SAFETY: buffer head is a TOKEN_ELEVATION.
        let te = unsafe { &*(elev_buf.as_ptr() as *const TOKEN_ELEVATION) };
        detail.elevated = te.TokenIsElevated != 0;
        any = true;
    }

    any
}

/// Two-call `GetTokenInformation` for `class`, returning the filled byte buffer.
fn get_token_information(token: HANDLE, class: u32) -> Option<Vec<u8>> {
    let mut needed: DWORD = 0;
    // SAFETY: probe with a null buffer to learn the size.
    unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u8; needed as usize];
    let mut got: DWORD = 0;
    // SAFETY: buf sized to the probed length.
    let ok = unsafe {
        GetTokenInformation(
            token,
            class,
            buf.as_mut_ptr().cast(),
            buf.len() as DWORD,
            &mut got,
        )
    };
    if ok == 0 {
        None
    } else {
        Some(buf)
    }
}

/// `ConvertSidToStringSidW` → an owned `S-1-…` string.
fn sid_to_string(sid: *mut std::ffi::c_void) -> Option<String> {
    let mut pstr: *mut u16 = std::ptr::null_mut();
    // SAFETY: sid is a valid PSID; pstr receives a LocalAlloc'd buffer we free.
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut pstr) };
    if ok == 0 || pstr.is_null() {
        return None;
    }
    // SAFETY: pstr is a NUL-terminated wide string owned by the local heap.
    let s = unsafe { wide_ptr_to_string(pstr) };
    // SAFETY: pstr came from ConvertSidToStringSidW; free it once.
    unsafe { LocalFree(pstr.cast()) };
    Some(s)
}

/// `LookupAccountSidW` → `DOMAIN\name` (best-effort; empty on failure).
fn lookup_account_name(sid: *mut std::ffi::c_void) -> Option<String> {
    let mut name_len: DWORD = 0;
    let mut dom_len: DWORD = 0;
    let mut use_ty: u32 = 0;
    // First probe: learn the two buffer sizes.
    // SAFETY: null name/domain buffers → only the lengths are written.
    unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            std::ptr::null_mut(),
            &mut name_len,
            std::ptr::null_mut(),
            &mut dom_len,
            &mut use_ty,
        )
    };
    if name_len == 0 {
        return None;
    }
    let mut name = vec![0u16; name_len as usize];
    let mut dom = vec![0u16; dom_len.max(1) as usize];
    // SAFETY: buffers sized to the probed lengths; lengths passed in/out.
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            name.as_mut_ptr(),
            &mut name_len,
            dom.as_mut_ptr(),
            &mut dom_len,
            &mut use_ty,
        )
    };
    if ok == 0 {
        return None;
    }
    let name_s = String::from_utf16_lossy(&name[..name_len as usize]);
    let dom_s = String::from_utf16_lossy(&dom[..dom_len as usize]);
    if dom_s.is_empty() {
        Some(name_s)
    } else {
        Some(format!("{dom_s}\\{name_s}"))
    }
}

/// Command line via `NtQueryInformationProcess(ProcessCommandLineInformation)`.
fn query_command_line(h: HANDLE) -> Option<String> {
    let mut ret: u32 = 0;
    // SAFETY: null buffer probe → STATUS_INFO_LENGTH_MISMATCH with needed size.
    unsafe {
        NtQueryInformationProcess(
            h,
            PROCESS_COMMAND_LINE_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut ret,
        )
    };
    if ret == 0 {
        return None;
    }
    let mut buf = vec![0u8; ret as usize];
    // SAFETY: buf sized to the probed length.
    let status = unsafe {
        NtQueryInformationProcess(
            h,
            PROCESS_COMMAND_LINE_INFORMATION,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut ret,
        )
    };
    if status < 0 {
        return None;
    }
    // The buffer starts with a UNICODE_STRING whose Buffer points inside it.
    let us: UNICODE_STRING = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
    read_unicode_string_in_buffer(&us, &buf)
}

/// Working directory: read the target's PEB → ProcessParameters → CurrentDirectory.
/// Needs VM_READ; a separate handle is opened so the detail path degrades
/// gracefully if only limited-query access is available.
fn query_working_directory(pid: u32) -> Option<String> {
    // SAFETY: plain OpenProcess; NULL on failure.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if h.is_null() {
        return None;
    }
    let h = OwnedHandle(h);

    // PEB base via ProcessBasicInformation.
    let mut pbi = PROCESS_BASIC_INFORMATION {
        ExitStatus: 0,
        _pad0: 0,
        PebBaseAddress: 0,
        AffinityMask: 0,
        BasePriority: 0,
        _pad1: 0,
        UniqueProcessId: 0,
        InheritedFromUniqueProcessId: 0,
    };
    let mut ret: u32 = 0;
    // SAFETY: pbi is a live, correctly sized out buffer.
    let status = unsafe {
        NtQueryInformationProcess(
            h.0,
            PROCESS_BASIC_INFORMATION_CLASS,
            (&mut pbi as *mut PROCESS_BASIC_INFORMATION).cast(),
            std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut ret,
        )
    };
    if status < 0 || pbi.PebBaseAddress == 0 {
        return None;
    }

    // PEB.ProcessParameters (a pointer).
    let params_ptr = read_remote_usize(h.0, pbi.PebBaseAddress + PEB_PROCESS_PARAMETERS_OFFSET)?;
    if params_ptr == 0 {
        return None;
    }
    // RTL_USER_PROCESS_PARAMETERS.CurrentDirectory.DosPath (a UNICODE_STRING).
    read_remote_unicode_string(h.0, params_ptr + RTL_UPP_CURRENT_DIRECTORY_OFFSET)
}

/// Reads a `usize` from the target's memory.
fn read_remote_usize(h: HANDLE, addr: usize) -> Option<usize> {
    let mut val: usize = 0;
    let mut read: usize = 0;
    // SAFETY: &val is a live 8-byte sink; ReadProcessMemory bounds by nSize.
    let ok = unsafe {
        ReadProcessMemory(
            h,
            addr,
            (&mut val as *mut usize).cast(),
            std::mem::size_of::<usize>(),
            &mut read,
        )
    };
    if ok == 0 || read != std::mem::size_of::<usize>() {
        None
    } else {
        Some(val)
    }
}

/// Reads a remote `UNICODE_STRING` header at `addr`, then its buffer, to a String.
fn read_remote_unicode_string(h: HANDLE, addr: usize) -> Option<String> {
    // UNICODE_STRING header: Length (u16) @0, MaximumLength (u16) @2, Buffer @8.
    let len = read_remote_u16(h, addr)?;
    let buffer_ptr = read_remote_usize(h, addr + 8)?;
    if len == 0 || buffer_ptr == 0 {
        return None;
    }
    let units = (len / 2) as usize;
    if units == 0 || units > 32_768 {
        return None;
    }
    let mut wbuf = vec![0u16; units];
    let mut read: usize = 0;
    // SAFETY: wbuf sized to `units` u16; ReadProcessMemory bounds by nSize.
    let ok = unsafe {
        ReadProcessMemory(
            h,
            buffer_ptr,
            wbuf.as_mut_ptr().cast(),
            units * 2,
            &mut read,
        )
    };
    if ok == 0 || read < units * 2 {
        return None;
    }
    Some(String::from_utf16_lossy(&wbuf))
}

/// Reads a `u16` from the target's memory.
fn read_remote_u16(h: HANDLE, addr: usize) -> Option<u16> {
    let mut val: u16 = 0;
    let mut read: usize = 0;
    // SAFETY: &val is a live 2-byte sink.
    let ok = unsafe { ReadProcessMemory(h, addr, (&mut val as *mut u16).cast(), 2, &mut read) };
    if ok == 0 || read != 2 {
        None
    } else {
        Some(val)
    }
}

/// Decodes a `UNICODE_STRING` whose `Buffer` points inside `buf` (the
/// self-referential form NT query buffers use), bounds-checked against `buf`.
fn read_unicode_string_in_buffer(us: &UNICODE_STRING, buf: &[u8]) -> Option<String> {
    let units = (us.Length / 2) as usize;
    if units == 0 {
        return None;
    }
    let base = buf.as_ptr() as usize;
    let ptr = us.Buffer as usize;
    if ptr < base || ptr + units * 2 > base + buf.len() {
        return None;
    }
    // SAFETY: bounds checked against `buf`; ptr aliases within it.
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u16, units) };
    Some(String::from_utf16_lossy(slice))
}

/// Reads a NUL-terminated wide string from a raw pointer (local heap string).
///
/// # Safety
/// `ptr` must point to a NUL-terminated UTF-16 string that outlives the read.
unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
    let mut len = 0usize;
    while len < 4096 && *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

/// Lists the loaded modules of `pid`. `available=false` (with a reason) when the
/// process denies QUERY_INFORMATION|VM_READ (cross-user without elevation).
pub fn list_modules(pid: u32) -> ModulesResult {
    // SAFETY: plain OpenProcess; NULL on failure.
    let h = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if h.is_null() {
        let err = unsafe { GetLastError() };
        return ModulesResult {
            available: false,
            unavailable_reason: format!("access denied (elevation may help) (error {err})"),
            modules: Vec::new(),
        };
    }
    let h = OwnedHandle(h);

    let handles = match enum_module_handles(h.0) {
        Some(v) => v,
        None => {
            return ModulesResult {
                available: false,
                unavailable_reason: "module enumeration failed".to_string(),
                modules: Vec::new(),
            }
        }
    };

    let mut modules = Vec::with_capacity(handles.len());
    for hm in handles {
        let path = module_file_name(h.0, hm);
        let mut info = ModuleInfo {
            name: file_name_component(&path),
            path: path.clone(),
            ..Default::default()
        };
        if let Some(mi) = module_information(h.0, hm) {
            info.base_address = mi.lpBaseOfDll as u64;
            info.size = mi.SizeOfImage as u64;
        }
        if !path.is_empty() {
            if let Some(vi) = read_version_info(&path) {
                info.version = vi.file_version;
                info.publisher = vi.company_name;
            }
            info.signed = verify_signature(&path) == crate::winver::SignatureStatus::Signed;
        }
        modules.push(info);
    }

    ModulesResult {
        available: true,
        unavailable_reason: String::new(),
        modules,
    }
}

/// Two-call `EnumProcessModulesEx(LIST_MODULES_ALL)`, growing until it fits.
fn enum_module_handles(h: HANDLE) -> Option<Vec<HMODULE>> {
    let mut cap = 1024usize;
    for _ in 0..6 {
        let mut buf: Vec<HMODULE> = vec![std::ptr::null_mut(); cap];
        let mut needed: DWORD = 0;
        let cb = (cap * std::mem::size_of::<HMODULE>()) as DWORD;
        // SAFETY: buf holds `cap` HMODULE slots; needed out-param live.
        let ok =
            unsafe { EnumProcessModulesEx(h, buf.as_mut_ptr(), cb, &mut needed, LIST_MODULES_ALL) };
        if ok == 0 {
            return None;
        }
        let count = needed as usize / std::mem::size_of::<HMODULE>();
        if count <= cap {
            buf.truncate(count);
            return Some(buf);
        }
        // The module set grew past our buffer; retry with the reported size.
        cap = count;
    }
    None
}

/// Full path of one module via `GetModuleFileNameExW`.
fn module_file_name(h: HANDLE, hm: HMODULE) -> String {
    let mut buf = vec![0u16; 1024];
    // SAFETY: buf is a live sink of `len` units; the API returns the count written.
    let n = unsafe { GetModuleFileNameExW(h, hm, buf.as_mut_ptr(), buf.len() as DWORD) };
    if n == 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// Base/size of one module via `GetModuleInformation`.
fn module_information(h: HANDLE, hm: HMODULE) -> Option<MODULEINFO> {
    let mut mi = MODULEINFO::default();
    // SAFETY: mi is a live, correctly sized out buffer.
    let ok =
        unsafe { GetModuleInformation(h, hm, &mut mi, std::mem::size_of::<MODULEINFO>() as DWORD) };
    if ok == 0 {
        None
    } else {
        Some(mi)
    }
}

/// The file-name component of a Windows path (after the last `\` or `/`).
fn file_name_component(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// Lists `pid`'s threads from the snapshot's trailing thread array. Empty when
/// the pid is gone. Unprivileged (no per-thread `OpenThread`). `cpu_permille` is
/// 0 (a single snapshot carries no deltas — this is the on-demand inspector,
/// not the sampler).
pub fn list_threads(pid: u32) -> Vec<ThreadDetail> {
    let raw = match snapshot_thread_infos(pid) {
        Ok(Some(v)) => v,
        _ => return Vec::new(),
    };
    raw.into_iter()
        .map(|t| ThreadDetail {
            tid: t.tid,
            start_address: t.start_address,
            state: thread_state_label(t.state).to_string(),
            wait_reason: wait_reason_label(t.wait_reason).to_string(),
            priority: t.priority,
            cpu_permille: 0,
            user_time_100ns: t.user_time_100ns,
            kernel_time_100ns: t.kernel_time_100ns,
            context_switches: t.context_switches,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_thresholds() {
        assert_eq!(integrity_label(SECURITY_MANDATORY_SYSTEM_RID), "System");
        assert_eq!(integrity_label(SECURITY_MANDATORY_HIGH_RID), "High");
        assert_eq!(integrity_label(SECURITY_MANDATORY_MEDIUM_RID), "Medium");
        assert_eq!(integrity_label(SECURITY_MANDATORY_LOW_RID), "Low");
        assert_eq!(integrity_label(0), "Untrusted");
        // A between-threshold RID rounds down to the lower band.
        assert_eq!(
            integrity_label(SECURITY_MANDATORY_MEDIUM_RID + 0x100),
            "Medium"
        );
    }

    #[test]
    fn architecture_native_vs_emulated() {
        // Native x64 (process machine UNKNOWN → use native).
        assert_eq!(
            architecture_label(IMAGE_FILE_MACHINE_UNKNOWN, IMAGE_FILE_MACHINE_AMD64),
            "x64"
        );
        // 32-bit process on x64 host (process machine I386).
        assert_eq!(
            architecture_label(IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_AMD64),
            "x86"
        );
        // Native Arm64.
        assert_eq!(
            architecture_label(IMAGE_FILE_MACHINE_UNKNOWN, IMAGE_FILE_MACHINE_ARM64),
            "Arm64"
        );
        // Unknown machine → empty.
        assert_eq!(architecture_label(0x1234, 0x5678), "");
    }

    #[test]
    fn filetime_conversion() {
        // The FILETIME epoch delta itself maps to Unix 0.
        assert_eq!(
            filetime_100ns_to_unix_ms(FILETIME_UNIX_EPOCH_DELTA_100NS),
            0
        );
        // One second past the Unix epoch = 1000 ms.
        assert_eq!(
            filetime_100ns_to_unix_ms(FILETIME_UNIX_EPOCH_DELTA_100NS + 10_000_000),
            1000
        );
        // A pre-1970 (or zero) time clamps to 0.
        assert_eq!(filetime_100ns_to_unix_ms(0), 0);
    }

    #[test]
    fn thread_state_and_wait_labels() {
        assert_eq!(thread_state_label(2), "Running");
        assert_eq!(thread_state_label(5), "Waiting");
        assert_eq!(thread_state_label(99), "Unknown");
        assert_eq!(wait_reason_label(6), "UserRequest");
        assert_eq!(wait_reason_label(5), "Suspended");
        assert_eq!(wait_reason_label(9999), "Other");
    }

    #[test]
    fn file_name_component_extracts_leaf() {
        assert_eq!(
            file_name_component("C:\\Windows\\System32\\ntdll.dll"),
            "ntdll.dll"
        );
        assert_eq!(file_name_component("kernel32.dll"), "kernel32.dll");
        assert_eq!(file_name_component(""), "");
    }

    /// Self-target smoke: our own process detail is always fully accessible.
    /// Asserts path, command line, and a nonzero thread count come back.
    #[test]
    fn own_process_detail_is_complete() {
        let me = std::process::id();
        let res = process_detail(me, 0);
        assert!(res.available, "own process must be available");
        let d = res.detail.expect("detail present");
        assert_eq!(d.pid, me);
        assert!(!d.image_path.is_empty(), "own image path resolvable");
        assert!(
            d.image_path.to_ascii_lowercase().contains("atlas")
                || d.image_name.to_ascii_lowercase().contains("atlas"),
            "own image should be the test/service binary, got {:?}",
            d.image_path
        );
        assert!(!d.command_line.is_empty(), "own command line resolvable");
        assert!(d.thread_count > 0);
        assert!(!d.architecture.is_empty(), "architecture resolvable");
        // Not elevated in the dev shell; integrity is at least Medium.
        assert!(!d.integrity_level.is_empty());
    }

    #[test]
    fn absent_pid_detail_unavailable() {
        let res = process_detail(0xFFFF_FFF0, 0);
        assert!(!res.available);
        assert!(res.unavailable_reason.contains("exited"));
    }

    /// Self-target: our own modules include the executable and ntdll.
    #[test]
    fn own_process_modules_present() {
        let me = std::process::id();
        let res = list_modules(me);
        assert!(res.available, "own modules must be enumerable");
        assert!(
            res.modules
                .iter()
                .any(|m| m.name.eq_ignore_ascii_case("ntdll.dll")),
            "ntdll.dll should be loaded"
        );
    }

    /// Self-target: our own threads come back with tids and states.
    #[test]
    fn own_process_threads_present() {
        let me = std::process::id();
        let threads = list_threads(me);
        assert!(!threads.is_empty());
        assert!(threads.iter().all(|t| !t.state.is_empty()));
    }
}
