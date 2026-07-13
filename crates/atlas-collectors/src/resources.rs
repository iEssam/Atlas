//! Resource-ownership search — "what is using this file" (docs/phases.md Phase 2,
//! PRD §9.5). Uses the Restart Manager (`RstrtMgr.dll`): start a session,
//! register the target path as a resource, and ask `RmGetList` which processes
//! (and services) currently hold it. Works unprivileged for user-accessible
//! files; a failure (path not found, access denied) is reported
//! `available=false` with a reason rather than an empty success.

#![cfg(windows)]

use crate::ffi::{
    CloseHandle, OpenProcess, QueryFullProcessImageNameW, RmEndSession, RmGetList,
    RmRegisterResources, RmStartSession, DWORD, ERROR_MORE_DATA, HANDLE, LPCWSTR,
    PROCESS_QUERY_LIMITED_INFORMATION, RM_APP_TYPE_SERVICE, RM_PROCESS_INFO, UINT,
};
use crate::reg::to_wide;

/// One process/service holding the resource — mirrors the proto `ResourceOwner`.
#[derive(Debug, Clone)]
pub struct ResourceOwner {
    pub pid: u32,
    pub image_name: String,
    pub image_path: String,
    /// Restart Manager friendly application name / description.
    pub description: String,
    pub is_service: bool,
}

/// Result of a resource-ownership search — mirrors `FindResourceOwnersReply`.
#[derive(Debug, Clone)]
pub struct ResourceOwnersResult {
    pub available: bool,
    pub unavailable_reason: String,
    pub owners: Vec<ResourceOwner>,
}

/// Finds the processes/services using `path` via the Restart Manager. An empty
/// owner list with `available=true` means nothing holds the file; `available=
/// false` means the search itself could not run (path invalid / access denied).
pub fn find_resource_owners(path: &str) -> ResourceOwnersResult {
    if path.trim().is_empty() {
        return unavailable("empty path");
    }

    // Session key buffer: CCH_RM_SESSION_KEY chars + NUL.
    let mut session: DWORD = 0;
    let mut key = [0u16; 33];
    // SAFETY: session/key are live out buffers of the documented size.
    let rc = unsafe { RmStartSession(&mut session, 0, key.as_mut_ptr()) };
    if rc != 0 {
        return unavailable(&format!("RmStartSession failed (error {rc})"));
    }

    let result = register_and_list(session, path);

    // SAFETY: session came from a successful RmStartSession; ended once.
    unsafe {
        RmEndSession(session);
    }
    result
}

/// Registers `path` and reads the affected-app list. Split out so the session is
/// always ended by the caller regardless of which step fails.
fn register_and_list(session: DWORD, path: &str) -> ResourceOwnersResult {
    let wpath = to_wide(path);
    let files: [LPCWSTR; 1] = [wpath.as_ptr()];
    // SAFETY: one file path registered; the array + string outlive the call.
    let rc = unsafe {
        RmRegisterResources(
            session,
            1,
            files.as_ptr(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return unavailable(&format!("RmRegisterResources failed (error {rc})"));
    }

    // Two-call size dance: probe for the needed count, then read.
    let mut needed: UINT = 0;
    let mut have: UINT = 0;
    let mut reasons: DWORD = 0;
    // SAFETY: null app array probe → count into `needed`.
    let rc = unsafe {
        RmGetList(
            session,
            &mut needed,
            &mut have,
            std::ptr::null_mut(),
            &mut reasons,
        )
    };
    // The probe returns ERROR_MORE_DATA when there are owners, or SUCCESS(0)
    // with needed==0 when there are none.
    if rc != 0 && rc != ERROR_MORE_DATA as u32 {
        return unavailable(&format!("RmGetList probe failed (error {rc})"));
    }
    if needed == 0 {
        return ResourceOwnersResult {
            available: true,
            unavailable_reason: String::new(),
            owners: Vec::new(),
        };
    }

    // Allocate with slack and retry until it fits (the set can change between
    // the probe and the read).
    for _ in 0..4 {
        let cap = needed as usize;
        let mut apps: Vec<RM_PROCESS_INFO> = Vec::with_capacity(cap);
        let mut count: UINT = cap as UINT;
        let mut proc_needed: UINT = 0;
        // SAFETY: apps has `cap` capacity; RmGetList fills up to `count` and
        // writes the actual/needed counts back.
        let rc = unsafe {
            RmGetList(
                session,
                &mut proc_needed,
                &mut count,
                apps.as_mut_ptr(),
                &mut reasons,
            )
        };
        if rc == ERROR_MORE_DATA as u32 {
            needed = proc_needed.max(needed + 1);
            continue;
        }
        if rc != 0 {
            return unavailable(&format!("RmGetList failed (error {rc})"));
        }
        // SAFETY: RmGetList populated `count` entries.
        unsafe { apps.set_len(count as usize) };
        let owners = apps.iter().map(map_owner).collect();
        return ResourceOwnersResult {
            available: true,
            unavailable_reason: String::new(),
            owners,
        };
    }

    unavailable("RmGetList did not converge")
}

/// Maps one `RM_PROCESS_INFO` to a [`ResourceOwner`], resolving the image path
/// best-effort from the pid.
fn map_owner(info: &RM_PROCESS_INFO) -> ResourceOwner {
    let pid = info.Process.dwProcessId;
    let description = wide_array_to_string(&info.strAppName);
    let is_service = info.ApplicationType == RM_APP_TYPE_SERVICE;
    let image_path = image_path_for_pid(pid).unwrap_or_default();
    let image_name = if image_path.is_empty() {
        // Fall back to the RM friendly name when the path is unavailable.
        description.clone()
    } else {
        image_path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&image_path)
            .to_string()
    };
    ResourceOwner {
        pid,
        image_name,
        image_path,
        description,
        is_service,
    }
}

/// Best-effort full image path for `pid` (QUERY_LIMITED_INFORMATION; empty on
/// failure — e.g. the pid already exited or a protected process).
fn image_path_for_pid(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: plain OpenProcess; NULL on failure.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let path = query_full_image_name(h);
    // SAFETY: h came from OpenProcess; closed once.
    unsafe {
        CloseHandle(h);
    }
    path
}

/// `QueryFullProcessImageNameW` wrapper (Win32 path form).
fn query_full_image_name(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = buf.len() as DWORD;
    // SAFETY: buf/size are live; the API bounds by `size` and updates it.
    let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size) };
    if ok == 0 || size == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// Decodes a fixed-size NUL-terminated UTF-16 array (the RM name fields).
fn wide_array_to_string(arr: &[u16]) -> String {
    let end = arr.iter().position(|&u| u == 0).unwrap_or(arr.len());
    String::from_utf16_lossy(&arr[..end])
}

/// Builds an `available=false` result with `reason`.
fn unavailable(reason: &str) -> ResourceOwnersResult {
    ResourceOwnersResult {
        available: false,
        unavailable_reason: reason.to_string(),
        owners: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::RM_UNIQUE_PROCESS;
    use std::mem::{offset_of, size_of};

    /// Locks the RM_PROCESS_INFO layout — the ApplicationType read (service
    /// detection) and the overall stride depend on the name-array sizes.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn rm_process_info_layout() {
        assert_eq!(size_of::<RM_UNIQUE_PROCESS>(), 12);
        assert_eq!(offset_of!(RM_PROCESS_INFO, strAppName), 0x0C);
        assert_eq!(offset_of!(RM_PROCESS_INFO, strServiceShortName), 0x20C);
        assert_eq!(offset_of!(RM_PROCESS_INFO, ApplicationType), 0x28C);
        assert_eq!(offset_of!(RM_PROCESS_INFO, bRestartable), 0x298);
        assert_eq!(size_of::<RM_PROCESS_INFO>(), 0x29C);
    }

    #[test]
    fn wide_array_trims_at_nul() {
        let mut arr = [0u16; 8];
        for (i, c) in "abc".encode_utf16().enumerate() {
            arr[i] = c;
        }
        assert_eq!(wide_array_to_string(&arr), "abc");
    }

    #[test]
    fn empty_path_is_unavailable() {
        let res = find_resource_owners("   ");
        assert!(!res.available);
        assert!(res.unavailable_reason.contains("empty"));
    }

    /// Self-target smoke: hold a temp file open and confirm the search names our
    /// own process as an owner. Exercises the whole RM round-trip unprivileged.
    #[test]
    fn locking_a_file_finds_this_process() {
        use std::io::Write;
        let mut path = std::env::temp_dir();
        path.push(format!("atlas_rm_test_{}.tmp", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("create temp file");
        f.write_all(b"atlas").unwrap();
        f.flush().unwrap();
        // Keep `f` open across the search so we are a live owner.

        let res = find_resource_owners(path.to_str().unwrap());
        assert!(
            res.available,
            "RM search should run: {}",
            res.unavailable_reason
        );
        let me = std::process::id();
        assert!(
            res.owners.iter().any(|o| o.pid == me),
            "this process should own the open file; owners: {:?}",
            res.owners.iter().map(|o| o.pid).collect::<Vec<_>>()
        );

        drop(f);
        let _ = std::fs::remove_file(&path);
    }
}
