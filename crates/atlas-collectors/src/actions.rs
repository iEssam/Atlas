//! Safe wrappers over the process-action Win32/NT primitives (docs/phases.md
//! M6, the safe-action broker). The raw FFI lives in [`crate::ffi`]; this module
//! keeps the `unsafe` blocks small and hands the broker a checked, panic-free
//! API. Every function is a no-privilege-escalation user-mode call — the broker
//! policy (protected-critical list, consent tokens) is enforced above this layer
//! in `atlas-service`, not here.

#![cfg(windows)]

use std::cell::RefCell;

use crate::ffi::{
    CloseHandle, EnumWindows, GetLastError, GetWindowThreadProcessId, IsWindowVisible,
    NtResumeProcess, NtSuspendProcess, OpenProcess, PostMessageW, TerminateProcess, BOOL, DWORD,
    HANDLE, HWND, LPARAM, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE, WM_CLOSE,
};

/// Result of a process action: whether it succeeded and a human-readable note.
#[derive(Debug, Clone)]
pub struct ActionOutcome {
    pub success: bool,
    pub message: String,
}

impl ActionOutcome {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
        }
    }
    fn fail(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
        }
    }
}

thread_local! {
    /// Scratch buffer the `EnumWindows` callback appends to. `EnumWindows` runs
    /// the callback synchronously on the calling thread, so a thread-local
    /// collector avoids passing a raw `&mut Vec` pointer through `LPARAM` while
    /// staying single-threaded-safe.
    static ENUM_ACC: RefCell<EnumAcc> = const { RefCell::new(EnumAcc {
        target_pid: 0,
        windows: Vec::new(),
    }) };
}

struct EnumAcc {
    target_pid: u32,
    windows: Vec<HWND>,
}

/// `EnumWindows` callback: records visible top-level windows owned by the target
/// pid into the thread-local accumulator. Always returns TRUE to continue the
/// enumeration to the end.
unsafe extern "system" fn enum_proc(hwnd: HWND, _l: LPARAM) -> BOOL {
    let mut owner_pid: DWORD = 0;
    GetWindowThreadProcessId(hwnd, &mut owner_pid);
    ENUM_ACC.with(|acc| {
        let mut acc = acc.borrow_mut();
        if owner_pid == acc.target_pid && IsWindowVisible(hwnd) != 0 {
            acc.windows.push(hwnd);
        }
    });
    1 // TRUE — keep enumerating
}

/// Returns the handles of `pid`'s visible top-level windows. Empty for a
/// process with no UI (a service, a console child, ...).
pub fn visible_top_level_windows(pid: u32) -> Vec<HWND> {
    ENUM_ACC.with(|acc| {
        let mut acc = acc.borrow_mut();
        acc.target_pid = pid;
        acc.windows.clear();
    });
    // SAFETY: `enum_proc` is a valid callback; it only touches the thread-local
    // accumulator populated above, on this same thread, before we read it back.
    unsafe {
        EnumWindows(enum_proc, 0);
    }
    ENUM_ACC.with(|acc| acc.borrow().windows.clone())
}

/// Count of `pid`'s visible top-level windows (risk assembly input).
pub fn count_visible_top_level_windows(pid: u32) -> u32 {
    visible_top_level_windows(pid).len() as u32
}

/// Posts `WM_CLOSE` to every visible top-level window of `pid` (the close-
/// normally verb). Returns the number of windows a message was successfully
/// posted to. Zero visible windows → an outcome noting nothing to close.
pub fn post_close_to_windows(pid: u32) -> ActionOutcome {
    let windows = visible_top_level_windows(pid);
    if windows.is_empty() {
        return ActionOutcome::fail("no visible top-level windows to close");
    }
    let mut posted = 0u32;
    for hwnd in windows {
        // SAFETY: `hwnd` came from EnumWindows this instant; PostMessageW is
        // async and never blocks. A failure (window already gone) is tolerated.
        let ok = unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
        if ok != 0 {
            posted += 1;
        }
    }
    if posted > 0 {
        ActionOutcome::ok(format!("posted WM_CLOSE to {posted} window(s)"))
    } else {
        ActionOutcome::fail("PostMessage failed for all windows")
    }
}

/// RAII guard for an OpenProcess handle so every early return closes it.
struct ProcHandle(HANDLE);
impl Drop for ProcHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is a handle we opened and have not closed yet.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Opens `pid` with `access`, mapping a NULL result to a descriptive error that
/// includes `GetLastError`.
fn open_process(pid: u32, access: DWORD, verb: &str) -> Result<ProcHandle, ActionOutcome> {
    // SAFETY: plain OpenProcess call; returns NULL on failure which we detect.
    let h = unsafe { OpenProcess(access, 0, pid) };
    if h.is_null() {
        let err = unsafe { GetLastError() };
        Err(ActionOutcome::fail(format!(
            "OpenProcess for {verb} failed (pid {pid}, error {err})"
        )))
    } else {
        Ok(ProcHandle(h))
    }
}

/// Suspends every thread of `pid` via `NtSuspendProcess`.
pub fn suspend_process(pid: u32) -> ActionOutcome {
    let h = match open_process(pid, PROCESS_SUSPEND_RESUME, "suspend") {
        Ok(h) => h,
        Err(o) => return o,
    };
    // SAFETY: `h.0` is a live handle with PROCESS_SUSPEND_RESUME.
    let status = unsafe { NtSuspendProcess(h.0) };
    if status >= 0 {
        ActionOutcome::ok(format!("suspended pid {pid}"))
    } else {
        ActionOutcome::fail(format!(
            "NtSuspendProcess failed (pid {pid}, status 0x{:08X})",
            status as u32
        ))
    }
}

/// Resumes every thread of `pid` via `NtResumeProcess`.
pub fn resume_process(pid: u32) -> ActionOutcome {
    let h = match open_process(pid, PROCESS_SUSPEND_RESUME, "resume") {
        Ok(h) => h,
        Err(o) => return o,
    };
    // SAFETY: `h.0` is a live handle with PROCESS_SUSPEND_RESUME.
    let status = unsafe { NtResumeProcess(h.0) };
    if status >= 0 {
        ActionOutcome::ok(format!("resumed pid {pid}"))
    } else {
        ActionOutcome::fail(format!(
            "NtResumeProcess failed (pid {pid}, status 0x{:08X})",
            status as u32
        ))
    }
}

/// Terminates `pid` via `TerminateProcess` (exit code 1).
pub fn terminate_process(pid: u32) -> ActionOutcome {
    let h = match open_process(pid, PROCESS_TERMINATE, "terminate") {
        Ok(h) => h,
        Err(o) => return o,
    };
    // SAFETY: `h.0` is a live handle with PROCESS_TERMINATE.
    let ok = unsafe { TerminateProcess(h.0, 1) };
    if ok != 0 {
        ActionOutcome::ok(format!("terminated pid {pid}"))
    } else {
        let err = unsafe { GetLastError() };
        ActionOutcome::fail(format!("TerminateProcess failed (pid {pid}, error {err})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The current test process has no visible top-level windows (it's a console
    /// test runner), so the count is 0 and close reports nothing to do. This
    /// exercises the EnumWindows plumbing without side effects.
    #[test]
    fn own_process_has_no_visible_windows() {
        let pid = std::process::id();
        assert_eq!(count_visible_top_level_windows(pid), 0);
        let outcome = post_close_to_windows(pid);
        assert!(!outcome.success);
    }

    /// OpenProcess for a pid that cannot exist fails cleanly (no panic), with a
    /// descriptive message.
    #[test]
    fn action_on_bogus_pid_fails_gracefully() {
        // 0xFFFF_FFF0 is not a valid pid (pids are multiples of 4 and far lower).
        let outcome = suspend_process(0xFFFF_FFF0);
        assert!(!outcome.success);
        assert!(outcome.message.contains("OpenProcess"));
    }
}
