//! Hand-written Win32/NT bindings for the first collector slice.
//!
//! Deliberately no `windows-sys` dependency yet: five stable-ABI functions
//! don't justify it, and owning the definitions keeps the whole unsafe
//! surface reviewable in one file. Struct layouts are locked by the offset
//! tests in `snapshot.rs`. Migration to `windows-sys` is planned once the
//! collector set grows (docs/phases.md, M3).

#![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

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

// ---------------------------------------------------------------------------
// Registry FFI (docs/phases.md M7). Backs the privacy (ConsentStore) and
// startup (Run keys / StartupApproved) collectors. Hand-written advapi32 in the
// same style as the collectors above — a handful of stable-ABI calls, no
// `windows-sys` dependency. Reads only: no key is ever opened for write.
// ---------------------------------------------------------------------------

/// Registry key handle (`HKEY`). Predefined roots are constant pseudo-handles.
pub type HKEY = *mut c_void;
/// `LSTATUS` — registry API return code (ERROR_SUCCESS == 0 on success).
pub type LSTATUS = i32;

pub const ERROR_SUCCESS: LSTATUS = 0;
pub const ERROR_NO_MORE_ITEMS: LSTATUS = 259;
pub const ERROR_MORE_DATA: LSTATUS = 234;

/// Predefined registry roots (pseudo-handles; never closed).
pub const HKEY_CLASSES_ROOT: HKEY = 0x8000_0000_u32 as usize as HKEY;
pub const HKEY_CURRENT_USER: HKEY = 0x8000_0001_u32 as usize as HKEY;
pub const HKEY_LOCAL_MACHINE: HKEY = 0x8000_0002_u32 as usize as HKEY;

/// `RegOpenKeyExW` desired-access rights.
pub const KEY_READ: DWORD = 0x2_0019;
/// Force the 64-bit view of the registry (ignore WOW6432 redirection).
pub const KEY_WOW64_64KEY: DWORD = 0x0100;
/// Force the 32-bit (WOW6432Node) view of the registry.
pub const KEY_WOW64_32KEY: DWORD = 0x0200;

/// Registry value types we care about.
pub const REG_SZ: DWORD = 1;
pub const REG_EXPAND_SZ: DWORD = 2;
pub const REG_BINARY: DWORD = 3;
pub const REG_DWORD: DWORD = 4;
pub const REG_QWORD: DWORD = 11;

#[link(name = "advapi32")]
extern "system" {
    /// Opens `lpSubKey` under `hKey` with `samDesired` access. On success writes
    /// the opened handle to `*phkResult`; caller closes it with `RegCloseKey`.
    pub fn RegOpenKeyExW(
        hKey: HKEY,
        lpSubKey: *const u16,
        ulOptions: DWORD,
        samDesired: DWORD,
        phkResult: *mut HKEY,
    ) -> LSTATUS;

    /// Enumerates the `dwIndex`-th subkey of `hKey`. `lpName`/`lpcchName` receive
    /// the subkey name (in UTF-16 units, excluding the NUL). Returns
    /// ERROR_NO_MORE_ITEMS when the index is past the last subkey.
    pub fn RegEnumKeyExW(
        hKey: HKEY,
        dwIndex: DWORD,
        lpName: *mut u16,
        lpcchName: *mut DWORD,
        lpReserved: *mut DWORD,
        lpClass: *mut u16,
        lpcchClass: *mut DWORD,
        lpftLastWriteTime: *mut FILETIME,
    ) -> LSTATUS;

    /// Enumerates the `dwIndex`-th value of `hKey`: name into `lpValueName`,
    /// type into `lpType`, raw bytes into `lpData` (with `lpcbData` the byte
    /// length in/out). Returns ERROR_NO_MORE_ITEMS past the last value.
    pub fn RegEnumValueW(
        hKey: HKEY,
        dwIndex: DWORD,
        lpValueName: *mut u16,
        lpcchValueName: *mut DWORD,
        lpReserved: *mut DWORD,
        lpType: *mut DWORD,
        lpData: *mut u8,
        lpcbData: *mut DWORD,
    ) -> LSTATUS;

    /// Reads the named value under `hKey`: type into `lpType`, raw bytes into
    /// `lpData` (`lpcbData` byte length in/out). Passing a NULL `lpData` returns
    /// the required size in `*lpcbData`.
    pub fn RegQueryValueExW(
        hKey: HKEY,
        lpValueName: *const u16,
        lpReserved: *mut DWORD,
        lpType: *mut DWORD,
        lpData: *mut u8,
        lpcbData: *mut DWORD,
    ) -> LSTATUS;

    /// Closes a key handle opened by `RegOpenKeyExW`.
    pub fn RegCloseKey(hKey: HKEY) -> LSTATUS;
}

// ---------------------------------------------------------------------------
// Service Control Manager FFI (docs/phases.md M7). Backs the services inventory.
// Enumeration + config queries only — all satisfied by SC_MANAGER_ENUMERATE_-
// SERVICE / GENERIC_READ, no elevation. Hand-written advapi32 in the collector
// style. Structs are `#[repr(C)]`; the enum-status buffer is walked as a slice
// of `ENUM_SERVICE_STATUS_PROCESSW` and the config blobs cast from a byte buffer.
// ---------------------------------------------------------------------------

/// `SC_HANDLE` — an open SCM or service handle.
pub type SC_HANDLE = *mut c_void;

/// `OpenSCManagerW` access: enumerate + query the active database (read-only).
pub const SC_MANAGER_ENUMERATE_SERVICE: DWORD = 0x0004;
pub const SC_MANAGER_CONNECT: DWORD = 0x0001;
/// `OpenServiceW` access: query config + status (read-only).
pub const SERVICE_QUERY_CONFIG: DWORD = 0x0001;
pub const SERVICE_QUERY_STATUS: DWORD = 0x0004;

/// `EnumServicesStatusExW` service-type mask: all Win32 services (own + shared
/// process). Drivers are excluded — the inventory is Win32 services (PRD §9.9.1).
pub const SERVICE_WIN32: DWORD = 0x0000_0030;
/// `EnumServicesStatusExW` state mask: every state (active + inactive).
pub const SERVICE_STATE_ALL: DWORD = 0x0000_0003;
/// `EnumServicesStatusExW` info level: SC_ENUM_PROCESS_INFO.
pub const SC_ENUM_PROCESS_INFO: DWORD = 0;

/// `QueryServiceConfig2W` info levels.
pub const SERVICE_CONFIG_DESCRIPTION: DWORD = 1;
pub const SERVICE_CONFIG_DELAYED_AUTO_START_INFO: DWORD = 3;

/// `SERVICE_STATUS_PROCESS.dwCurrentState` values (map to proto `ServiceState`).
pub const SERVICE_STOPPED: DWORD = 1;
pub const SERVICE_START_PENDING: DWORD = 2;
pub const SERVICE_STOP_PENDING: DWORD = 3;
pub const SERVICE_RUNNING: DWORD = 4;
pub const SERVICE_CONTINUE_PENDING: DWORD = 5;
pub const SERVICE_PAUSE_PENDING: DWORD = 6;
pub const SERVICE_PAUSED: DWORD = 7;

/// `QUERY_SERVICE_CONFIGW.dwStartType` values (map to proto `ServiceStartType`).
pub const SERVICE_BOOT_START: DWORD = 0;
pub const SERVICE_SYSTEM_START: DWORD = 1;
pub const SERVICE_AUTO_START: DWORD = 2;
pub const SERVICE_DEMAND_START: DWORD = 3;
pub const SERVICE_DISABLED: DWORD = 4;

/// `SERVICE_STATUS_PROCESS` — the process-aware status returned by
/// `EnumServicesStatusExW` (carries the pid, unlike plain `SERVICE_STATUS`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SERVICE_STATUS_PROCESS {
    pub dwServiceType: DWORD,
    pub dwCurrentState: DWORD,
    pub dwControlsAccepted: DWORD,
    pub dwWin32ExitCode: DWORD,
    pub dwServiceSpecificExitCode: DWORD,
    pub dwCheckPoint: DWORD,
    pub dwWaitHint: DWORD,
    pub dwProcessId: DWORD,
    pub dwServiceFlags: DWORD,
}

/// `ENUM_SERVICE_STATUS_PROCESSW` — one entry in the `EnumServicesStatusExW`
/// output buffer. The two name pointers point back into the same buffer.
#[repr(C)]
pub struct ENUM_SERVICE_STATUS_PROCESSW {
    pub lpServiceName: *mut u16,
    pub lpDisplayName: *mut u16,
    pub ServiceStatusProcess: SERVICE_STATUS_PROCESS,
}

/// `QUERY_SERVICE_CONFIGW` — variable-length config blob; the string pointers
/// point into the tail of the same buffer `QueryServiceConfigW` filled.
#[repr(C)]
pub struct QUERY_SERVICE_CONFIGW {
    pub dwServiceType: DWORD,
    pub dwStartType: DWORD,
    pub dwErrorControl: DWORD,
    pub lpBinaryPathName: *mut u16,
    pub lpLoadOrderGroup: *mut u16,
    pub dwTagId: DWORD,
    pub lpDependencies: *mut u16,
    pub lpServiceStartName: *mut u16,
    pub lpDisplayName: *mut u16,
}

/// `SERVICE_DESCRIPTIONW` — the description blob from `QueryServiceConfig2W`.
#[repr(C)]
pub struct SERVICE_DESCRIPTIONW {
    pub lpDescription: *mut u16,
}

/// `SERVICE_DELAYED_AUTO_START_INFO` — the delayed-auto-start flag from
/// `QueryServiceConfig2W`.
#[repr(C)]
pub struct SERVICE_DELAYED_AUTO_START_INFO {
    pub fDelayedAutostart: BOOL,
}

#[link(name = "advapi32")]
extern "system" {
    /// Opens the service control manager on `lpMachineName` (NULL = local) /
    /// `lpDatabaseName` (NULL = active). Read enumeration needs only
    /// SC_MANAGER_ENUMERATE_SERVICE | SC_MANAGER_CONNECT — no elevation.
    pub fn OpenSCManagerW(
        lpMachineName: *const u16,
        lpDatabaseName: *const u16,
        dwDesiredAccess: DWORD,
    ) -> SC_HANDLE;

    /// Opens a service by name for `dwDesiredAccess` (query rights here).
    pub fn OpenServiceW(
        hSCManager: SC_HANDLE,
        lpServiceName: *const u16,
        dwDesiredAccess: DWORD,
    ) -> SC_HANDLE;

    /// Enumerates services of `dwServiceType`/`dwServiceState`. On the first
    /// call with a too-small buffer it sets `*pcbBytesNeeded` and fails with
    /// ERROR_MORE_DATA; the caller re-allocates and retries.
    pub fn EnumServicesStatusExW(
        hSCManager: SC_HANDLE,
        InfoLevel: DWORD,
        dwServiceType: DWORD,
        dwServiceState: DWORD,
        lpServices: *mut u8,
        cbBufSize: DWORD,
        pcbBytesNeeded: *mut DWORD,
        lpServicesReturned: *mut DWORD,
        lpResumeHandle: *mut DWORD,
        pszGroupName: *const u16,
    ) -> BOOL;

    /// Queries a service's static config (start type, binary path, account,
    /// display name). Two-call size pattern like the enum above.
    pub fn QueryServiceConfigW(
        hService: SC_HANDLE,
        lpServiceConfig: *mut u8,
        cbBufSize: DWORD,
        pcbBytesNeeded: *mut DWORD,
    ) -> BOOL;

    /// Queries an extended config attribute (`dwInfoLevel`): description or the
    /// delayed-auto-start flag. Two-call size pattern.
    pub fn QueryServiceConfig2W(
        hService: SC_HANDLE,
        dwInfoLevel: DWORD,
        lpBuffer: *mut u8,
        cbBufSize: DWORD,
        pcbBytesNeeded: *mut DWORD,
    ) -> BOOL;

    /// Closes an SCM or service handle.
    pub fn CloseServiceHandle(hSCObject: SC_HANDLE) -> BOOL;
}
