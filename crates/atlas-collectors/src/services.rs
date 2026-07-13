//! Services inventory via the Service Control Manager (PRD §9.9.1,
//! docs/phases.md M7).
//!
//! `OpenSCManagerW` (enumerate access — no elevation) + `EnumServicesStatusExW`
//! (SC_ENUM_PROCESS_INFO) give every Win32 service's current state and pid in
//! one buffer. For each we then `OpenServiceW` (query access) and read:
//! `QueryServiceConfigW` for start type / binary path / account / display name,
//! and `QueryServiceConfig2W` for the description and the delayed-auto-start
//! flag. A per-service config read that fails degrades that field only — the
//! service still appears in the list.
//!
//! Everything here is a standard-user read: SC_MANAGER_ENUMERATE_SERVICE +
//! SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS. No elevation.

#![cfg(windows)]

use std::ptr;

use crate::ffi::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QueryServiceConfig2W,
    QueryServiceConfigW, DWORD, ENUM_SERVICE_STATUS_PROCESSW, ERROR_MORE_DATA,
    QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_CONNECT,
    SC_MANAGER_ENUMERATE_SERVICE, SERVICE_AUTO_START, SERVICE_BOOT_START,
    SERVICE_CONFIG_DELAYED_AUTO_START_INFO, SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTINUE_PENDING,
    SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW, SERVICE_DISABLED,
    SERVICE_PAUSED, SERVICE_PAUSE_PENDING, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS,
    SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATE_ALL, SERVICE_STOPPED,
    SERVICE_STOP_PENDING, SERVICE_SYSTEM_START, SERVICE_WIN32,
};
use crate::reg::to_wide;

/// Current run state of a service. Mirrors the proto `ServiceState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    /// An unrecognised state value.
    Unspecified,
}

/// How a service starts. Mirrors the proto `ServiceStartType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStartType {
    Boot,
    System,
    Auto,
    Manual,
    Disabled,
    Unspecified,
}

/// One service inventory row.
#[derive(Debug, Clone)]
pub struct ServiceEntry {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub state: ServiceState,
    pub start_type: ServiceStartType,
    /// 0 when not running.
    pub pid: u32,
    pub account: String,
    pub binary_path: String,
    pub delayed_auto_start: bool,
}

/// Maps an SCM `dwCurrentState` to [`ServiceState`].
pub fn map_state(raw: DWORD) -> ServiceState {
    match raw {
        SERVICE_STOPPED => ServiceState::Stopped,
        SERVICE_START_PENDING => ServiceState::StartPending,
        SERVICE_STOP_PENDING => ServiceState::StopPending,
        SERVICE_RUNNING => ServiceState::Running,
        SERVICE_CONTINUE_PENDING => ServiceState::ContinuePending,
        SERVICE_PAUSE_PENDING => ServiceState::PausePending,
        SERVICE_PAUSED => ServiceState::Paused,
        _ => ServiceState::Unspecified,
    }
}

/// Maps an SCM `dwStartType` to [`ServiceStartType`].
pub fn map_start_type(raw: DWORD) -> ServiceStartType {
    match raw {
        SERVICE_BOOT_START => ServiceStartType::Boot,
        SERVICE_SYSTEM_START => ServiceStartType::System,
        SERVICE_AUTO_START => ServiceStartType::Auto,
        SERVICE_DEMAND_START => ServiceStartType::Manual,
        SERVICE_DISABLED => ServiceStartType::Disabled,
        _ => ServiceStartType::Unspecified,
    }
}

/// Case-insensitive substring filter over a service's name and display name.
/// Empty filter matches everything.
pub fn matches_filter(filter: &str, name: &str, display_name: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_ascii_lowercase();
    name.to_ascii_lowercase().contains(&f) || display_name.to_ascii_lowercase().contains(&f)
}

/// RAII wrapper closing an SCM/service handle on drop.
struct ScHandle(SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle came from Open{SCManager,Service}W and is closed once.
            unsafe {
                CloseServiceHandle(self.0);
            }
        }
    }
}

/// Reads a NUL-terminated UTF-16 string from a raw `*mut u16` that points into a
/// live buffer. Returns empty for a null pointer. `len_cap` bounds the scan so a
/// non-terminated pointer cannot run away.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated UTF-16 string that stays
/// valid for the duration of the read.
unsafe fn wide_ptr_to_string(ptr: *const u16, len_cap: usize) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < len_cap && *ptr.add(len) != 0 {
        len += 1;
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    String::from_utf16_lossy(slice)
}

/// Enumerates Win32 services and returns those matching `filter`. Best-effort:
/// SCM/enumeration failure yields an empty list (logged by the caller), and any
/// single service whose config cannot be read still appears with the fields that
/// did resolve.
pub fn enumerate_services(filter: &str) -> Vec<ServiceEntry> {
    // SAFETY: NULL machine/database = local active SCM; read-enum access only.
    let scm = unsafe {
        OpenSCManagerW(
            ptr::null(),
            ptr::null(),
            SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE,
        )
    };
    if scm.is_null() {
        return Vec::new();
    }
    let scm = ScHandle(scm);

    let raw = match enumerate_raw(scm.0) {
        Some(r) => r,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    // Walk the buffer as a slice of ENUM_SERVICE_STATUS_PROCESSW. The name
    // pointers point back into `raw`, so it must stay alive for this scope.
    let count = raw.count;
    let entries =
        // SAFETY: `enumerate_raw` guarantees `raw.buf` holds `count` structs at
        // its head (Windows packs the struct array first, strings after).
        unsafe { std::slice::from_raw_parts(raw.buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW, count) };

    for e in entries {
        // SAFETY: the name pointers point into `raw.buf`, valid this scope.
        let name = unsafe { wide_ptr_to_string(e.lpServiceName, 512) };
        let display_name = unsafe { wide_ptr_to_string(e.lpDisplayName, 512) };
        if name.is_empty() {
            continue;
        }
        if !matches_filter(filter, &name, &display_name) {
            continue;
        }
        let state = map_state(e.ServiceStatusProcess.dwCurrentState);
        let pid = e.ServiceStatusProcess.dwProcessId;

        // Per-service config (best-effort). Start with the enum-provided
        // name/display/state/pid so a config failure still yields a row.
        let mut entry = ServiceEntry {
            name: name.clone(),
            display_name,
            description: String::new(),
            state,
            start_type: ServiceStartType::Unspecified,
            pid,
            account: String::new(),
            binary_path: String::new(),
            delayed_auto_start: false,
        };
        fill_config(scm.0, &name, &mut entry);
        out.push(entry);
    }
    out
}

/// Owns the enum output buffer plus the count of structs at its head.
struct RawServices {
    buf: Vec<u8>,
    count: usize,
}

/// Runs the two-call `EnumServicesStatusExW` size dance and returns the filled
/// buffer + returned count. `None` on failure.
fn enumerate_raw(scm: SC_HANDLE) -> Option<RawServices> {
    let mut bytes_needed: DWORD = 0;
    let mut returned: DWORD = 0;
    let mut resume: DWORD = 0;

    // First call: empty buffer to learn the size.
    // SAFETY: null/zero buffer is the documented probe form; out-params live.
    let _ = unsafe {
        EnumServicesStatusExW(
            scm,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            ptr::null_mut(),
            0,
            &mut bytes_needed,
            &mut returned,
            &mut resume,
            ptr::null(),
        )
    };
    if bytes_needed == 0 {
        return None;
    }

    // Allocate and read. The API can still page results via the resume handle;
    // one generous buffer usually gets everything, but loop to be safe.
    let mut buf = vec![0u8; bytes_needed as usize];
    let total: DWORD;
    resume = 0;
    loop {
        let mut chunk_needed: DWORD = 0;
        let mut chunk_returned: DWORD = 0;
        // Offset into the buffer where this page should land. Since we sized the
        // buffer for the full set, we read into the head each time and append the
        // returned structs; but Windows fills contiguous structs+strings, so we
        // read the whole thing in one shot and break unless it asks for more.
        // SAFETY: buf sized to bytes_needed; out-params live.
        let ok = unsafe {
            EnumServicesStatusExW(
                scm,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buf.as_mut_ptr(),
                buf.len() as DWORD,
                &mut chunk_needed,
                &mut chunk_returned,
                &mut resume,
                ptr::null(),
            )
        };
        if ok != 0 {
            // Success: all remaining services fit this call.
            total = chunk_returned;
            break;
        }
        // ERROR_MORE_DATA: grow and retry from the top (resume reset). This is
        // rare with a correctly sized buffer; grow generously to converge.
        let err = last_error();
        if err == ERROR_MORE_DATA as DWORD {
            let new_len = (buf.len() * 2).max((chunk_needed as usize) + buf.len());
            buf = vec![0u8; new_len];
            resume = 0;
            continue;
        }
        return None;
    }

    Some(RawServices {
        buf,
        count: total as usize,
    })
}

/// Opens one service and fills its config-derived fields on `entry`. Each query
/// is independent and best-effort: a failure leaves that field at its default.
fn fill_config(scm: SC_HANDLE, name: &str, entry: &mut ServiceEntry) {
    let wname = to_wide(name);
    // SAFETY: wname NUL-terminated; query-only access.
    let svc = unsafe {
        OpenServiceW(
            scm,
            wname.as_ptr(),
            SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS,
        )
    };
    if svc.is_null() {
        return;
    }
    let svc = ScHandle(svc);

    // QueryServiceConfigW: start type, binary path, account, display name.
    if let Some(cfg_buf) = query_config(svc.0) {
        // SAFETY: query_config returns a buffer whose head is a valid
        // QUERY_SERVICE_CONFIGW with string pointers into the same buffer.
        let cfg = unsafe { &*(cfg_buf.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
        entry.start_type = map_start_type(cfg.dwStartType);
        // SAFETY: pointers point into cfg_buf, valid this scope.
        entry.binary_path = unsafe { wide_ptr_to_string(cfg.lpBinaryPathName, 4096) };
        entry.account = unsafe { wide_ptr_to_string(cfg.lpServiceStartName, 512) };
        let disp = unsafe { wide_ptr_to_string(cfg.lpDisplayName, 512) };
        if entry.display_name.is_empty() && !disp.is_empty() {
            entry.display_name = disp;
        }
    }

    // QueryServiceConfig2W: description.
    if let Some(desc_buf) = query_config2(svc.0, SERVICE_CONFIG_DESCRIPTION) {
        // SAFETY: buffer head is a SERVICE_DESCRIPTIONW with a pointer into it.
        let d = unsafe { &*(desc_buf.as_ptr() as *const SERVICE_DESCRIPTIONW) };
        entry.description = unsafe { wide_ptr_to_string(d.lpDescription, 8192) };
    }

    // QueryServiceConfig2W: delayed-auto-start flag.
    if let Some(dl_buf) = query_config2(svc.0, SERVICE_CONFIG_DELAYED_AUTO_START_INFO) {
        // SAFETY: buffer head is a SERVICE_DELAYED_AUTO_START_INFO.
        let dl = unsafe { &*(dl_buf.as_ptr() as *const SERVICE_DELAYED_AUTO_START_INFO) };
        entry.delayed_auto_start = dl.fDelayedAutostart != 0;
    }
}

/// Two-call `QueryServiceConfigW`, returning the filled byte buffer.
fn query_config(svc: SC_HANDLE) -> Option<Vec<u8>> {
    let mut needed: DWORD = 0;
    // SAFETY: null buffer probe.
    let _ = unsafe { QueryServiceConfigW(svc, ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u8; needed as usize];
    let mut needed2: DWORD = 0;
    // SAFETY: buf sized to `needed`.
    let ok =
        unsafe { QueryServiceConfigW(svc, buf.as_mut_ptr(), buf.len() as DWORD, &mut needed2) };
    if ok != 0 {
        Some(buf)
    } else {
        None
    }
}

/// Two-call `QueryServiceConfig2W` for `info_level`, returning the byte buffer.
fn query_config2(svc: SC_HANDLE, info_level: DWORD) -> Option<Vec<u8>> {
    let mut needed: DWORD = 0;
    // SAFETY: null buffer probe.
    let _ = unsafe { QueryServiceConfig2W(svc, info_level, ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u8; needed as usize];
    let mut needed2: DWORD = 0;
    // SAFETY: buf sized to `needed`.
    let ok = unsafe {
        QueryServiceConfig2W(
            svc,
            info_level,
            buf.as_mut_ptr(),
            buf.len() as DWORD,
            &mut needed2,
        )
    };
    if ok != 0 {
        Some(buf)
    } else {
        None
    }
}

/// Thin `GetLastError` wrapper (the FFI symbol lives in the broker section).
fn last_error() -> DWORD {
    // SAFETY: GetLastError has no preconditions.
    unsafe { crate::ffi::GetLastError() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_mapping() {
        assert_eq!(map_state(SERVICE_RUNNING), ServiceState::Running);
        assert_eq!(map_state(SERVICE_STOPPED), ServiceState::Stopped);
        assert_eq!(map_state(SERVICE_PAUSED), ServiceState::Paused);
        assert_eq!(map_state(999), ServiceState::Unspecified);
    }

    #[test]
    fn start_type_mapping() {
        assert_eq!(map_start_type(SERVICE_AUTO_START), ServiceStartType::Auto);
        assert_eq!(
            map_start_type(SERVICE_DEMAND_START),
            ServiceStartType::Manual
        );
        assert_eq!(map_start_type(SERVICE_DISABLED), ServiceStartType::Disabled);
        assert_eq!(map_start_type(SERVICE_BOOT_START), ServiceStartType::Boot);
        assert_eq!(
            map_start_type(SERVICE_SYSTEM_START),
            ServiceStartType::System
        );
        assert_eq!(map_start_type(42), ServiceStartType::Unspecified);
    }

    #[test]
    fn filter_empty_matches_all() {
        assert!(matches_filter("", "Anything", "Any Display"));
    }

    #[test]
    fn filter_case_insensitive_name() {
        assert!(matches_filter("dns", "Dnscache", "DNS Client"));
        assert!(matches_filter("DNS", "Dnscache", "DNS Client"));
    }

    #[test]
    fn filter_matches_display_name() {
        assert!(matches_filter("client", "Dnscache", "DNS Client"));
    }

    #[test]
    fn filter_no_match() {
        assert!(!matches_filter("spooler", "Dnscache", "DNS Client"));
    }
}
