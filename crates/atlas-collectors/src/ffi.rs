//! Hand-written Win32/NT bindings for the first collector slice.
//!
//! Deliberately no `windows-sys` dependency yet: five stable-ABI functions
//! don't justify it, and owning the definitions keeps the whole unsafe
//! surface reviewable in one file. Struct layouts are locked by the offset
//! tests in `snapshot.rs`. Migration to `windows-sys` is planned once the
//! collector set grows (docs/phases.md, M3).

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

pub type NTSTATUS = i32;
pub type BOOL = i32;
pub type HANDLE = *mut c_void;

pub const STATUS_INFO_LENGTH_MISMATCH: NTSTATUS = 0xC000_0004_u32 as i32;
pub const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
pub const ALL_PROCESSOR_GROUPS: u16 = 0xFFFF;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FILETIME {
    pub dwLowDateTime: u32,
    pub dwHighDateTime: u32,
}

impl FILETIME {
    pub fn as_u64(self) -> u64 {
        ((self.dwHighDateTime as u64) << 32) | self.dwLowDateTime as u64
    }
}

#[repr(C)]
pub struct UNICODE_STRING {
    /// Length in bytes, not UTF-16 units.
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: *mut u16,
}

#[repr(C)]
pub struct MEMORYSTATUSEX {
    pub dwLength: u32,
    pub dwMemoryLoad: u32,
    pub ullTotalPhys: u64,
    pub ullAvailPhys: u64,
    pub ullTotalPageFile: u64,
    pub ullAvailPageFile: u64,
    pub ullTotalVirtual: u64,
    pub ullAvailVirtual: u64,
    pub ullAvailExtendedVirtual: u64,
}

/// Layout from the Windows DDK / phnt headers; stable on 64-bit Windows
/// since Vista and relied upon by every Task Manager-class tool.
/// The trailing SYSTEM_THREAD_INFORMATION array is not represented — this
/// slice does not consume per-thread data.
#[repr(C)]
pub struct SYSTEM_PROCESS_INFORMATION {
    pub NextEntryOffset: u32,
    pub NumberOfThreads: u32,
    pub WorkingSetPrivateSize: i64,
    pub HardFaultCount: u32,
    pub NumberOfThreadsHighWatermark: u32,
    pub CycleTime: u64,
    pub CreateTime: i64,
    pub UserTime: i64,
    pub KernelTime: i64,
    pub ImageName: UNICODE_STRING,
    pub BasePriority: i32,
    pub UniqueProcessId: HANDLE,
    pub InheritedFromUniqueProcessId: HANDLE,
    pub HandleCount: u32,
    pub SessionId: u32,
    pub UniqueProcessKey: usize,
    pub PeakVirtualSize: usize,
    pub VirtualSize: usize,
    pub PageFaultCount: u32,
    pub PeakWorkingSetSize: usize,
    pub WorkingSetSize: usize,
    pub QuotaPeakPagedPoolUsage: usize,
    pub QuotaPagedPoolUsage: usize,
    pub QuotaPeakNonPagedPoolUsage: usize,
    pub QuotaNonPagedPoolUsage: usize,
    pub PagefileUsage: usize,
    pub PeakPagefileUsage: usize,
    pub PrivatePageCount: usize,
    pub ReadOperationCount: i64,
    pub WriteOperationCount: i64,
    pub OtherOperationCount: i64,
    pub ReadTransferCount: i64,
    pub WriteTransferCount: i64,
    pub OtherTransferCount: i64,
}

#[link(name = "ntdll")]
extern "system" {
    pub fn NtQuerySystemInformation(
        SystemInformationClass: u32,
        SystemInformation: *mut c_void,
        SystemInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> NTSTATUS;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetSystemTimes(
        lpIdleTime: *mut FILETIME,
        lpKernelTime: *mut FILETIME,
        lpUserTime: *mut FILETIME,
    ) -> BOOL;

    pub fn GlobalMemoryStatusEx(lpBuffer: *mut MEMORYSTATUSEX) -> BOOL;

    pub fn GetActiveProcessorCount(GroupNumber: u16) -> u32;
}

// ---------------------------------------------------------------------------
// Safe-action broker FFI (docs/phases.md M6). Hand-written in the same style as
// the collectors above: a handful of stable-ABI Win32/NT calls, no
// `windows-sys` dependency. These back the AtlasControl broker's risk assembly
// (visible top-level window count) and the four action verbs (close / suspend /
// resume / terminate).
// ---------------------------------------------------------------------------

pub type DWORD = u32;
/// `WPARAM`/`LPARAM`/`LRESULT` — pointer-sized message parameters.
pub type WPARAM = usize;
pub type LPARAM = isize;
/// Window handle.
pub type HWND = *mut c_void;

/// `OpenProcess` desired-access rights we use.
/// `PROCESS_TERMINATE` — required by `TerminateProcess`.
pub const PROCESS_TERMINATE: DWORD = 0x0001;
/// `PROCESS_SUSPEND_RESUME` — required by `NtSuspendProcess`/`NtResumeProcess`.
pub const PROCESS_SUSPEND_RESUME: DWORD = 0x0800;

/// `WM_CLOSE` — the polite "close this window" message (close-normally verb).
pub const WM_CLOSE: DWORD = 0x0010;

/// The ETW/GDI callback signature for `EnumWindows`.
pub type WNDENUMPROC = unsafe extern "system" fn(HWND, LPARAM) -> BOOL;

#[link(name = "user32")]
extern "system" {
    /// Enumerates all top-level windows, invoking `lpEnumFunc` for each until it
    /// returns FALSE or the enumeration is exhausted.
    pub fn EnumWindows(lpEnumFunc: WNDENUMPROC, lParam: LPARAM) -> BOOL;

    /// Writes the pid of the process that owns `hWnd` into `*lpdwProcessId` and
    /// returns the owning thread id.
    pub fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;

    /// Whether `hWnd` is visible (WS_VISIBLE on it and all ancestors).
    pub fn IsWindowVisible(hWnd: HWND) -> BOOL;

    /// Posts a message to `hWnd`'s thread queue and returns immediately (does
    /// not wait for the window to process it). Used to deliver `WM_CLOSE`.
    pub fn PostMessageW(hWnd: HWND, Msg: DWORD, wParam: WPARAM, lParam: LPARAM) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    /// Opens an existing process object with `dwDesiredAccess`. Returns NULL on
    /// failure (check `GetLastError`).
    pub fn OpenProcess(dwDesiredAccess: DWORD, bInheritHandle: BOOL, dwProcessId: DWORD) -> HANDLE;

    /// Closes an open object handle.
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;

    /// Terminates `hProcess` with `uExitCode`. Requires PROCESS_TERMINATE.
    pub fn TerminateProcess(hProcess: HANDLE, uExitCode: u32) -> BOOL;

    pub fn GetLastError() -> DWORD;
}

#[link(name = "ntdll")]
extern "system" {
    /// Suspends every thread of `ProcessHandle` (documented in ReactOS/phnt;
    /// stable since XP). Requires PROCESS_SUSPEND_RESUME.
    pub fn NtSuspendProcess(ProcessHandle: HANDLE) -> NTSTATUS;
    /// Resumes every thread of `ProcessHandle`. Requires PROCESS_SUSPEND_RESUME.
    pub fn NtResumeProcess(ProcessHandle: HANDLE) -> NTSTATUS;
}
