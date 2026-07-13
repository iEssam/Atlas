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

// ---------------------------------------------------------------------------
// R2 deep-process-inspector FFI (docs/phases.md Phase 2, PRD §9.4/§9.5). Backs
// the on-demand inspector: process detail (identity/path/command line/token/
// architecture/signature/version), handles, modules, threads, and Restart-
// Manager file-lock ownership. Hand-written in the collector style — stable-ABI
// Win32/NT calls, no `windows-sys` dependency; struct layouts are locked by the
// offset tests in `inspector.rs` / `handles.rs` / `resources.rs`. Every call is
// a user-mode read; cross-user/protected targets degrade (coverage flags) but
// never escalate.
// ---------------------------------------------------------------------------

/// Pointer-sized opaque types used across the inspector FFI.
pub type PVOID = *mut c_void;
pub type LPCWSTR = *const u16;
pub type LPWSTR = *mut u16;
pub type LPCSTR = *const u8;
pub type UINT = u32;
pub type USHORT = u16;
pub type ULONG = u32;
pub type LONG = i32;
/// A loaded-module handle (`HMODULE`).
pub type HMODULE = *mut c_void;
/// A local-heap handle (`HLOCAL`), from `ConvertSidToStringSidW`/`LocalFree`.
pub type HLOCAL = *mut c_void;
/// A security identifier pointer (`PSID`); opaque — only handed back to the API.
pub type PSID = *mut c_void;

// --- OpenProcess desired-access rights (inspector reads) --------------------
/// Query a limited identity set (path, times, token); works cross-user without
/// elevation for most user processes.
pub const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x1000;
/// Full query rights; required (with VM_READ) by `EnumProcessModulesEx`.
pub const PROCESS_QUERY_INFORMATION: DWORD = 0x0400;
/// Read the target's virtual memory (module enum, PEB command line/cwd read).
pub const PROCESS_VM_READ: DWORD = 0x0010;
/// Duplicate a handle out of the target (handle-name resolution).
pub const PROCESS_DUP_HANDLE: DWORD = 0x0040;

// --- Token access + information classes -------------------------------------
pub const TOKEN_QUERY: DWORD = 0x0008;
/// `TokenUser` — the token's user SID.
pub const TOKEN_USER_CLASS: DWORD = 1;
/// `TokenElevation` — whether the token is elevated.
pub const TOKEN_ELEVATION_CLASS: DWORD = 20;
/// `TokenIntegrityLevel` — the mandatory integrity label SID.
pub const TOKEN_INTEGRITY_LEVEL_CLASS: DWORD = 25;

/// Mandatory integrity-level RIDs (last SID sub-authority of the label).
pub const SECURITY_MANDATORY_UNTRUSTED_RID: u32 = 0x0000;
pub const SECURITY_MANDATORY_LOW_RID: u32 = 0x1000;
pub const SECURITY_MANDATORY_MEDIUM_RID: u32 = 0x2000;
pub const SECURITY_MANDATORY_HIGH_RID: u32 = 0x3000;
pub const SECURITY_MANDATORY_SYSTEM_RID: u32 = 0x4000;

// --- NtQuerySystemInformation / NtQueryObject / NtQueryInformationProcess ----
/// `SystemExtendedHandleInformation` — every open handle system-wide (with the
/// owning pid), as `SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX`.
pub const SYSTEM_EXTENDED_HANDLE_INFORMATION_CLASS: u32 = 0x40;
/// `ObjectNameInformation` — the object's name (`UNICODE_STRING` + buffer).
pub const OBJECT_NAME_INFORMATION_CLASS: u32 = 1;
/// `ObjectTypesInformation` — the whole object-type table (index → name).
pub const OBJECT_TYPES_INFORMATION_CLASS: u32 = 3;
/// `ProcessBasicInformation` — PEB base address (class 0).
pub const PROCESS_BASIC_INFORMATION_CLASS: u32 = 0;

pub const STATUS_SUCCESS: NTSTATUS = 0;
/// `NtQueryObject` buffer-too-small statuses (retry with a larger buffer).
pub const STATUS_BUFFER_OVERFLOW: NTSTATUS = 0x8000_0005_u32 as i32;
pub const STATUS_BUFFER_TOO_SMALL: NTSTATUS = 0xC000_0023_u32 as i32;

/// `DuplicateHandle` option: grant the same access the source handle had.
pub const DUPLICATE_SAME_ACCESS: DWORD = 0x0002;

/// `EnumProcessModulesEx` filter: list every module (32- and 64-bit).
pub const LIST_MODULES_ALL: DWORD = 0x03;

// --- IsWow64Process2 image-machine values -----------------------------------
pub const IMAGE_FILE_MACHINE_UNKNOWN: USHORT = 0x0000;
pub const IMAGE_FILE_MACHINE_I386: USHORT = 0x014C;
pub const IMAGE_FILE_MACHINE_AMD64: USHORT = 0x8664;
pub const IMAGE_FILE_MACHINE_ARM64: USHORT = 0xAA64;

/// `GetPackageFullName` returns this when the process has no package identity
/// (i.e. a plain desktop app, not MSIX/AppX).
pub const APPMODEL_ERROR_NO_PACKAGE: LONG = 15700;
pub const ERROR_INSUFFICIENT_BUFFER: DWORD = 122;

/// `client id` pair carried by `SYSTEM_THREAD_INFORMATION`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CLIENT_ID {
    pub UniqueProcess: HANDLE,
    pub UniqueThread: HANDLE,
}

/// One thread's snapshot, packed by the kernel immediately after each
/// `SYSTEM_PROCESS_INFORMATION` record (there are `NumberOfThreads` of them).
/// Layout is DDK/phnt-stable on 64-bit; locked by the offset test in
/// `inspector.rs`.
#[repr(C)]
pub struct SYSTEM_THREAD_INFORMATION {
    pub KernelTime: i64,
    pub UserTime: i64,
    pub CreateTime: i64,
    pub WaitTime: u32,
    pub StartAddress: PVOID,
    pub ClientId: CLIENT_ID,
    pub Priority: i32,
    pub BasePriority: i32,
    pub ContextSwitches: u32,
    pub ThreadState: u32,
    pub WaitReason: u32,
}

/// One entry of `SystemExtendedHandleInformation`. Layout locked by the offset
/// test in `handles.rs`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX {
    pub Object: usize,
    pub UniqueProcessId: usize,
    pub HandleValue: usize,
    pub GrantedAccess: u32,
    pub CreatorBackTraceIndex: u16,
    pub ObjectTypeIndex: u16,
    pub HandleAttributes: u32,
    pub Reserved: u32,
}

/// Fixed header of `SystemExtendedHandleInformation`; the entry array follows.
#[repr(C)]
pub struct SYSTEM_HANDLE_INFORMATION_EX {
    pub NumberOfHandles: usize,
    pub Reserved: usize,
    // SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX Handles[NumberOfHandles] follows.
}

/// One object-type descriptor from `ObjectTypesInformation`. Only `TypeName`
/// and `TypeIndex` are consumed; the rest fixes the stride to the next entry.
/// Layout locked by the offset test in `handles.rs`.
#[repr(C)]
pub struct OBJECT_TYPE_INFORMATION {
    pub TypeName: UNICODE_STRING,
    pub TotalNumberOfObjects: u32,
    pub TotalNumberOfHandles: u32,
    pub TotalPagedPoolUsage: u32,
    pub TotalNonPagedPoolUsage: u32,
    pub TotalNamePoolUsage: u32,
    pub TotalHandleTableUsage: u32,
    pub HighWaterNumberOfObjects: u32,
    pub HighWaterNumberOfHandles: u32,
    pub HighWaterPagedPoolUsage: u32,
    pub HighWaterNonPagedPoolUsage: u32,
    pub HighWaterNamePoolUsage: u32,
    pub HighWaterHandleTableUsage: u32,
    pub InvalidAttributes: u32,
    pub GenericMapping: [u32; 4],
    pub ValidAccessMask: u32,
    pub SecurityRequired: u8,
    pub MaintainHandleCount: u8,
    pub TypeIndex: u8,
    pub ReservedByte: u8,
    pub PoolType: u32,
    pub DefaultPagedPoolCharge: u32,
    pub DefaultNonPagedPoolCharge: u32,
}

/// `PROCESS_BASIC_INFORMATION` (subset): only `PebBaseAddress` and the ids are
/// used. The reserved words keep the struct at its documented 64-bit size.
#[repr(C)]
pub struct PROCESS_BASIC_INFORMATION {
    pub ExitStatus: NTSTATUS,
    pub _pad0: u32,
    pub PebBaseAddress: usize,
    pub AffinityMask: usize,
    pub BasePriority: i32,
    pub _pad1: u32,
    pub UniqueProcessId: usize,
    pub InheritedFromUniqueProcessId: usize,
}

/// `MODULEINFO` from `GetModuleInformation`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MODULEINFO {
    pub lpBaseOfDll: usize,
    pub SizeOfImage: u32,
    pub EntryPoint: usize,
}

/// `SID_AND_ATTRIBUTES` — a SID pointer plus its attribute flags.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SID_AND_ATTRIBUTES {
    pub Sid: PSID,
    pub Attributes: u32,
}

/// `TOKEN_USER` — the token's user SID (from `TokenUser`).
#[repr(C)]
pub struct TOKEN_USER {
    pub User: SID_AND_ATTRIBUTES,
}

/// `TOKEN_MANDATORY_LABEL` — the integrity-level SID (from `TokenIntegrityLevel`).
#[repr(C)]
pub struct TOKEN_MANDATORY_LABEL {
    pub Label: SID_AND_ATTRIBUTES,
}

/// `TOKEN_ELEVATION` — nonzero `TokenIsElevated` means the token is elevated.
#[repr(C)]
pub struct TOKEN_ELEVATION {
    pub TokenIsElevated: u32,
}

/// A COM `GUID` (used by `WinVerifyTrust`'s action id).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GUID {
    pub Data1: u32,
    pub Data2: u16,
    pub Data3: u16,
    pub Data4: [u8; 8],
}

/// `WINTRUST_ACTION_GENERIC_VERIFY_V2` — the Authenticode verify action.
pub const WINTRUST_ACTION_GENERIC_VERIFY_V2: GUID = GUID {
    Data1: 0x00AA_C56B,
    Data2: 0xCD44,
    Data3: 0x11D0,
    Data4: [0x8C, 0xC2, 0x00, 0xC0, 0x4F, 0xC2, 0x95, 0xEE],
};

pub const WTD_UI_NONE: DWORD = 2;
pub const WTD_REVOKE_NONE: DWORD = 0;
pub const WTD_CHOICE_FILE: DWORD = 1;
pub const WTD_CHOICE_CATALOG: DWORD = 2;
pub const WTD_STATEACTION_VERIFY: DWORD = 1;
pub const WTD_STATEACTION_CLOSE: DWORD = 2;
pub const WTD_REVOCATION_CHECK_NONE: DWORD = 0x0000_0010;
pub const CERT_NAME_ATTR_TYPE: DWORD = 3;
pub const CERT_NAME_SIMPLE_DISPLAY_TYPE: DWORD = 4;
pub const LOAD_LIBRARY_SEARCH_SYSTEM32: DWORD = 0x0000_0800;
/// `WinVerifyTrust` result: the file carries no signature at all.
pub const TRUST_E_NOSIGNATURE: LONG = 0x800B_0100_u32 as i32;
pub const TRUST_E_SUBJECT_FORM_UNKNOWN: LONG = 0x800B_0003_u32 as i32;
pub const TRUST_E_PROVIDER_UNKNOWN: LONG = 0x800B_0001_u32 as i32;

/// `WINTRUST_FILE_INFO` — the file to verify. Layout locked by an offset test.
#[repr(C)]
pub struct WINTRUST_FILE_INFO {
    pub cbStruct: DWORD,
    pub pcwszFilePath: LPCWSTR,
    pub hFile: HANDLE,
    pub pgKnownSubject: *const GUID,
}

/// Maximum Win32 path carried inline by `CATALOG_INFO`.
pub const MAX_PATH: usize = 260;

/// Catalog location returned for an `HCATINFO` membership match.
#[repr(C)]
pub struct CATALOG_INFO {
    pub cbStruct: DWORD,
    pub wszCatalogFile: [u16; MAX_PATH],
}

/// `WINTRUST_CATALOG_INFO` — a catalog and one member file to verify.
#[repr(C)]
pub struct WINTRUST_CATALOG_INFO {
    pub cbStruct: DWORD,
    pub dwCatalogVersion: DWORD,
    pub pcwszCatalogFilePath: LPCWSTR,
    pub pcwszMemberTag: LPCWSTR,
    pub pcwszMemberFilePath: LPCWSTR,
    pub hMemberFile: HANDLE,
    pub pbCalculatedFileHash: *mut u8,
    pub cbCalculatedFileHash: DWORD,
    pub pcCatalogContext: PVOID,
    pub hCatAdmin: HANDLE,
}

/// Prefix of `CRYPT_PROVIDER_CERT`; only the signing certificate context is
/// consumed before `WinVerifyTrust` releases the provider state.
#[repr(C)]
pub struct CRYPT_PROVIDER_CERT_PREFIX {
    pub cbStruct: DWORD,
    pub pCert: *const c_void,
}

/// `WINTRUST_DATA` — the verify request. Unused union/callback slots are typed
/// as opaque pointers/DWORDs to keep the documented 64-bit layout.
#[repr(C)]
pub struct WINTRUST_DATA {
    pub cbStruct: DWORD,
    pub pPolicyCallbackData: PVOID,
    pub pSIPClientData: PVOID,
    pub dwUIChoice: DWORD,
    pub fdwRevocationChecks: DWORD,
    pub dwUnionChoice: DWORD,
    /// Active member of the native `pFile`/`pCatalog` union, selected by
    /// `dwUnionChoice`.
    pub pInfo: PVOID,
    pub dwStateAction: DWORD,
    pub hWVTStateData: HANDLE,
    pub pwszURLReference: LPWSTR,
    pub dwProvFlags: DWORD,
    pub dwUIContext: DWORD,
    pub pSignatureSettings: PVOID,
}

/// `VS_FIXEDFILEINFO` — the fixed block inside a version resource (fallback for
/// file/product version when the string table lacks them).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VS_FIXEDFILEINFO {
    pub dwSignature: u32,
    pub dwStrucVersion: u32,
    pub dwFileVersionMS: u32,
    pub dwFileVersionLS: u32,
    pub dwProductVersionMS: u32,
    pub dwProductVersionLS: u32,
    pub dwFileFlagsMask: u32,
    pub dwFileFlags: u32,
    pub dwFileOS: u32,
    pub dwFileType: u32,
    pub dwFileSubtype: u32,
    pub dwFileDateMS: u32,
    pub dwFileDateLS: u32,
}

#[link(name = "ntdll")]
extern "system" {
    /// Queries an object's information (name/type). May block indefinitely on
    /// certain handle types (some named pipes) — the caller runs the name query
    /// on a killable worker thread with a timeout.
    pub fn NtQueryObject(
        Handle: HANDLE,
        ObjectInformationClass: u32,
        ObjectInformation: PVOID,
        ObjectInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> NTSTATUS;

    /// Queries process information (basic info → PEB base, command line).
    pub fn NtQueryInformationProcess(
        ProcessHandle: HANDLE,
        ProcessInformationClass: u32,
        ProcessInformation: PVOID,
        ProcessInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> NTSTATUS;
}

#[link(name = "kernel32")]
extern "system" {
    /// Loads a DLL from a constrained search location. Used for Wintrust
    /// helpers that Microsoft exposes only through dynamic linking.
    pub fn LoadLibraryExW(lpLibFileName: LPCWSTR, hFile: HANDLE, dwFlags: DWORD) -> HMODULE;

    /// Resolves an exported function in a loaded module.
    pub fn GetProcAddress(hModule: HMODULE, lpProcName: LPCSTR) -> PVOID;

    /// Full image path of a running process. `dwFlags` 0 = Win32 path form.
    pub fn QueryFullProcessImageNameW(
        hProcess: HANDLE,
        dwFlags: DWORD,
        lpExeName: LPWSTR,
        lpdwSize: *mut DWORD,
    ) -> BOOL;

    /// WOW64 emulation query: `*pProcessMachine` is `IMAGE_FILE_MACHINE_UNKNOWN`
    /// for a native process, else the emulated machine; `*pNativeMachine` is the
    /// host machine. Together they give the process architecture.
    pub fn IsWow64Process2(
        hProcess: HANDLE,
        pProcessMachine: *mut USHORT,
        pNativeMachine: *mut USHORT,
    ) -> BOOL;

    /// MSIX/AppX package full name of `hProcess`; `APPMODEL_ERROR_NO_PACKAGE`
    /// for a plain desktop app. Two-call size pattern.
    pub fn GetPackageFullName(
        hProcess: HANDLE,
        packageFullNameLength: *mut u32,
        packageFullName: LPWSTR,
    ) -> LONG;

    /// The current process pseudo-handle (`(HANDLE)-1`), the duplicate target.
    pub fn GetCurrentProcess() -> HANDLE;

    /// Duplicates a handle from a source process into ours (name resolution).
    pub fn DuplicateHandle(
        hSourceProcessHandle: HANDLE,
        hSourceHandle: HANDLE,
        hTargetProcessHandle: HANDLE,
        lpTargetHandle: *mut HANDLE,
        dwDesiredAccess: DWORD,
        bInheritHandle: BOOL,
        dwOptions: DWORD,
    ) -> BOOL;

    /// Reads the target's virtual memory (PEB command line / working directory).
    pub fn ReadProcessMemory(
        hProcess: HANDLE,
        lpBaseAddress: usize,
        lpBuffer: PVOID,
        nSize: usize,
        lpNumberOfBytesRead: *mut usize,
    ) -> BOOL;

    /// Frees a `LocalAlloc`/`ConvertSidToStringSidW` buffer.
    pub fn LocalFree(hMem: HLOCAL) -> HLOCAL;
}

#[link(name = "psapi")]
extern "system" {
    /// Enumerates a process's loaded modules (needs QUERY_INFORMATION|VM_READ).
    pub fn EnumProcessModulesEx(
        hProcess: HANDLE,
        lphModule: *mut HMODULE,
        cb: DWORD,
        lpcbNeeded: *mut DWORD,
        dwFilterFlag: DWORD,
    ) -> BOOL;

    /// Full path of one loaded module.
    pub fn GetModuleFileNameExW(
        hProcess: HANDLE,
        hModule: HMODULE,
        lpFilename: LPWSTR,
        nSize: DWORD,
    ) -> DWORD;

    /// Base address + image size of one loaded module.
    pub fn GetModuleInformation(
        hProcess: HANDLE,
        hModule: HMODULE,
        lpmodinfo: *mut MODULEINFO,
        cb: DWORD,
    ) -> BOOL;
}

#[link(name = "advapi32")]
extern "system" {
    /// Opens the access token of `ProcessHandle` (TOKEN_QUERY here).
    pub fn OpenProcessToken(
        ProcessHandle: HANDLE,
        DesiredAccess: DWORD,
        TokenHandle: *mut HANDLE,
    ) -> BOOL;

    /// Reads a class of token information (user SID, integrity, elevation).
    pub fn GetTokenInformation(
        TokenHandle: HANDLE,
        TokenInformationClass: u32,
        TokenInformation: PVOID,
        TokenInformationLength: DWORD,
        ReturnLength: *mut DWORD,
    ) -> BOOL;

    /// Resolves a SID to `DOMAIN\name`. Two-call size pattern.
    pub fn LookupAccountSidW(
        lpSystemName: LPCWSTR,
        Sid: PSID,
        Name: LPWSTR,
        cchName: *mut DWORD,
        ReferencedDomainName: LPWSTR,
        cchReferencedDomainName: *mut DWORD,
        peUse: *mut u32,
    ) -> BOOL;

    /// Formats a SID as its `S-1-…` string (caller frees via `LocalFree`).
    pub fn ConvertSidToStringSidW(Sid: PSID, StringSid: *mut LPWSTR) -> BOOL;

    /// Pointer to a SID's sub-authority count byte.
    pub fn GetSidSubAuthorityCount(pSid: PSID) -> *mut u8;

    /// Pointer to a SID's `nSubAuthority`-th sub-authority DWORD.
    pub fn GetSidSubAuthority(pSid: PSID, nSubAuthority: DWORD) -> *mut u32;
}

#[link(name = "version")]
extern "system" {
    /// Size of `lptstrFilename`'s version resource (0 if none).
    pub fn GetFileVersionInfoSizeW(lptstrFilename: LPCWSTR, lpdwHandle: *mut DWORD) -> DWORD;

    /// Reads the version resource into `lpData`.
    pub fn GetFileVersionInfoW(
        lptstrFilename: LPCWSTR,
        dwHandle: DWORD,
        dwLen: DWORD,
        lpData: PVOID,
    ) -> BOOL;

    /// Extracts a sub-block (translation table / string value / fixed info).
    pub fn VerQueryValueW(
        pBlock: *const c_void,
        lpSubBlock: LPCWSTR,
        lplpBuffer: *mut PVOID,
        puLen: *mut UINT,
    ) -> BOOL;
}

#[link(name = "wintrust")]
extern "system" {
    /// Authenticode trust verification. 0 = trusted; `TRUST_E_NOSIGNATURE` etc.
    /// otherwise. Called once to verify and once more to release state.
    pub fn WinVerifyTrust(hwnd: HANDLE, pgActionID: *const GUID, pWVTData: PVOID) -> LONG;
}

#[link(name = "crypt32")]
extern "system" {
    /// Reads a subject/issuer display attribute from a certificate context.
    pub fn CertGetNameStringW(
        pCertContext: *const c_void,
        dwType: DWORD,
        dwFlags: DWORD,
        pvTypePara: PVOID,
        pszNameString: LPWSTR,
        cchNameString: DWORD,
    ) -> DWORD;
}

#[link(name = "rstrtmgr")]
extern "system" {
    /// Starts a Restart Manager session; fills `strSessionKey` (>= 33 WCHARs).
    pub fn RmStartSession(
        pSessionHandle: *mut DWORD,
        dwSessionFlags: DWORD,
        strSessionKey: LPWSTR,
    ) -> DWORD;

    /// Registers the resources (file paths here) to analyse for the session.
    pub fn RmRegisterResources(
        dwSessionHandle: DWORD,
        nFiles: UINT,
        rgsFilenames: *const LPCWSTR,
        nApplications: UINT,
        rgApplications: *const RM_UNIQUE_PROCESS,
        nServices: UINT,
        rgsServiceNames: *const LPCWSTR,
    ) -> DWORD;

    /// Returns the processes/services currently using the registered resources.
    /// Two-call size pattern via `pnProcInfoNeeded`.
    pub fn RmGetList(
        dwSessionHandle: DWORD,
        pnProcInfoNeeded: *mut UINT,
        pnProcInfo: *mut UINT,
        rgAffectedApps: *mut RM_PROCESS_INFO,
        lpdwRebootReasons: *mut DWORD,
    ) -> DWORD;

    /// Ends a Restart Manager session and frees its resources.
    pub fn RmEndSession(dwSessionHandle: DWORD) -> DWORD;
}

/// `RM_UNIQUE_PROCESS` — a Restart Manager process identity (pid + start time).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RM_UNIQUE_PROCESS {
    pub dwProcessId: DWORD,
    pub ProcessStartTime: FILETIME,
}

/// Max character counts for the `RM_PROCESS_INFO` name arrays (RstrtMgr.h).
pub const CCH_RM_MAX_APP_NAME: usize = 255;
pub const CCH_RM_MAX_SVC_NAME: usize = 63;
/// `RmStartSession` writes a session key of `CCH_RM_SESSION_KEY` chars + NUL.
pub const CCH_RM_SESSION_KEY: usize = 32;

/// `RmGetList` application types; `RmService` marks a Windows service owner.
pub const RM_APP_TYPE_SERVICE: i32 = 3;

/// `RM_PROCESS_INFO` — one affected app from `RmGetList`. Layout locked by the
/// offset test in `resources.rs`.
#[repr(C)]
pub struct RM_PROCESS_INFO {
    pub Process: RM_UNIQUE_PROCESS,
    pub strAppName: [u16; CCH_RM_MAX_APP_NAME + 1],
    pub strServiceShortName: [u16; CCH_RM_MAX_SVC_NAME + 1],
    pub ApplicationType: i32,
    pub AppStatus: u32,
    pub TSSessionId: DWORD,
    pub bRestartable: BOOL,
}

// ---------------------------------------------------------------------------
// R2 rules-engine action FFI (docs/phases.md Phase 2, PRD §9.7, tech-stack
// §4.3). The reversible action set the rules engine applies to matching
// processes: priority class, processor affinity / P-E-core steering (CPU sets),
// and EcoQoS (ProcessPowerThrottling). Plus the trigger inputs: AC/DC power
// (GetSystemPowerStatus) and the foreground window pid (GetForegroundWindow +
// GetWindowThreadProcessId, the latter already declared above). Hand-written in
// the collector style — stable-ABI Win32 calls, no `windows-sys` dependency.
//
// Every apply is a same-user, unprivileged user-mode call; a cross-user or
// protected target simply fails `OpenProcess` and the caller degrades + skips
// (never crashes, never escalates). No REALTIME priority is exposed.
// ---------------------------------------------------------------------------

/// `OpenProcess` rights for the rules-engine action set. Reads (GetPriorityClass
/// / GetProcessAffinityMask / GetProcessInformation / GetProcessDefaultCpuSets)
/// need query rights; writes need set rights. CPU-set assignment additionally
/// needs `PROCESS_SET_LIMITED_INFORMATION`.
pub const PROCESS_SET_INFORMATION: DWORD = 0x0200;
pub const PROCESS_SET_LIMITED_INFORMATION: DWORD = 0x2000;

/// `SetPriorityClass` / `GetPriorityClass` process-priority-class values. No
/// `REALTIME_PRIORITY_CLASS` — deliberately unsupported (unsafe, PRD §9.7).
pub const IDLE_PRIORITY_CLASS: DWORD = 0x0000_0040;
pub const BELOW_NORMAL_PRIORITY_CLASS: DWORD = 0x0000_4000;
pub const NORMAL_PRIORITY_CLASS: DWORD = 0x0000_0020;
pub const ABOVE_NORMAL_PRIORITY_CLASS: DWORD = 0x0000_8000;
pub const HIGH_PRIORITY_CLASS: DWORD = 0x0000_0080;
pub const REALTIME_PRIORITY_CLASS: DWORD = 0x0000_0100;

/// `SetProcessInformation` / `GetProcessInformation` class `ProcessPowerThrottling`.
pub const PROCESS_INFORMATION_CLASS_POWER_THROTTLING: u32 = 4;
/// `PROCESS_POWER_THROTTLING_STATE.Version`.
pub const PROCESS_POWER_THROTTLING_CURRENT_VERSION: u32 = 1;
/// `PROCESS_POWER_THROTTLING_STATE` execution-speed control bit (EcoQoS).
pub const PROCESS_POWER_THROTTLING_EXECUTION_SPEED: u32 = 0x1;

/// `GetSystemPowerStatus.ACLineStatus` values: 0 = on battery (DC), 1 = plugged
/// in (AC), 255 = unknown.
pub const AC_LINE_STATUS_OFFLINE: u8 = 0;
pub const AC_LINE_STATUS_ONLINE: u8 = 1;
pub const AC_LINE_STATUS_UNKNOWN: u8 = 255;

// (SYSTEM_POWER_STATUS is defined with the battery/thermal FFI below; the
// ON_AC_POWER/ON_DC_POWER rule triggers consume its ACLineStatus field.)

// R2 network-inspector FFI (docs/phases.md Phase 2, PRD §9.12). Backs the
// connection / listening-port collector: iphlpapi's `GetExtendedTcpTable` /
// `GetExtendedUdpTable` (owner-pid variants, both AF_INET and AF_INET6) plus a
// best-effort DNS-resolver-cache read (dnsapi `DnsGetCacheDataTable` +
// cache-only `DnsQuery_W`) to attach domains to remote addresses. Hand-written
// in the collector style — stable-ABI reads, no `windows-sys` dependency; the
// MIB row layouts are locked by offset tests in `network.rs`. Every call is an
// unprivileged read; nothing opens a socket or emits a packet.
// ---------------------------------------------------------------------------

/// Address families passed to the extended-table calls.
pub const AF_INET: ULONG = 2;
pub const AF_INET6: ULONG = 23;

/// `TCP_TABLE_CLASS::TCP_TABLE_OWNER_PID_ALL` — every TCP row with its owner pid.
pub const TCP_TABLE_OWNER_PID_ALL: ULONG = 5;
/// `UDP_TABLE_CLASS::UDP_TABLE_OWNER_PID` — every UDP bind with its owner pid.
pub const UDP_TABLE_OWNER_PID: ULONG = 1;

/// `MIB_TCP_STATE` values (map 1:1 to the proto `TcpState` discriminants).
pub const MIB_TCP_STATE_LISTEN: DWORD = 2;

/// One IPv4 TCP row from `GetExtendedTcpTable(TCP_TABLE_OWNER_PID_ALL)`.
/// Ports are network-byte-order in the low 16 bits; addresses are `in_addr`
/// (network order). Layout locked by an offset test in `network.rs`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MIB_TCPROW_OWNER_PID {
    pub dwState: DWORD,
    pub dwLocalAddr: DWORD,
    pub dwLocalPort: DWORD,
    pub dwRemoteAddr: DWORD,
    pub dwRemotePort: DWORD,
    pub dwOwningPid: DWORD,
}

/// Fixed header of `MIB_TCPTABLE_OWNER_PID`; `dwNumEntries` rows follow.
#[repr(C)]
pub struct MIB_TCPTABLE_OWNER_PID {
    pub dwNumEntries: DWORD,
    // MIB_TCPROW_OWNER_PID table[dwNumEntries] follows.
}

/// One IPv6 TCP row. Addresses are raw 16-byte `in6_addr` (network order).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MIB_TCP6ROW_OWNER_PID {
    pub ucLocalAddr: [u8; 16],
    pub dwLocalScopeId: DWORD,
    pub dwLocalPort: DWORD,
    pub ucRemoteAddr: [u8; 16],
    pub dwRemoteScopeId: DWORD,
    pub dwRemotePort: DWORD,
    pub dwState: DWORD,
    pub dwOwningPid: DWORD,
}

/// Fixed header of `MIB_TCP6TABLE_OWNER_PID`; `dwNumEntries` rows follow.
#[repr(C)]
pub struct MIB_TCP6TABLE_OWNER_PID {
    pub dwNumEntries: DWORD,
}

/// One IPv4 UDP bind from `GetExtendedUdpTable(UDP_TABLE_OWNER_PID)`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MIB_UDPROW_OWNER_PID {
    pub dwLocalAddr: DWORD,
    pub dwLocalPort: DWORD,
    pub dwOwningPid: DWORD,
}

/// Fixed header of `MIB_UDPTABLE_OWNER_PID`; `dwNumEntries` rows follow.
#[repr(C)]
pub struct MIB_UDPTABLE_OWNER_PID {
    pub dwNumEntries: DWORD,
}

/// One IPv6 UDP bind.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MIB_UDP6ROW_OWNER_PID {
    pub ucLocalAddr: [u8; 16],
    pub dwLocalScopeId: DWORD,
    pub dwLocalPort: DWORD,
    pub dwOwningPid: DWORD,
}

/// Fixed header of `MIB_UDP6TABLE_OWNER_PID`; `dwNumEntries` rows follow.
#[repr(C)]
pub struct MIB_UDP6TABLE_OWNER_PID {
    pub dwNumEntries: DWORD,
}

#[link(name = "iphlpapi")]
extern "system" {
    /// Fills `pTcpTable` with the TCP connection table for `ulAf`
    /// (AF_INET / AF_INET6) at `TableClass`. Two-call size pattern: a too-small
    /// buffer returns ERROR_INSUFFICIENT_BUFFER with the needed size in
    /// `*pdwSize`.
    pub fn GetExtendedTcpTable(
        pTcpTable: PVOID,
        pdwSize: *mut DWORD,
        bOrder: BOOL,
        ulAf: ULONG,
        TableClass: ULONG,
        Reserved: ULONG,
    ) -> DWORD;

    /// Fills `pUdpTable` with the UDP bind table for `ulAf` at `TableClass`.
    pub fn GetExtendedUdpTable(
        pUdpTable: PVOID,
        pdwSize: *mut DWORD,
        bOrder: BOOL,
        ulAf: ULONG,
        TableClass: ULONG,
        Reserved: ULONG,
    ) -> DWORD;
}

// --- DNS resolver-cache read (dnsapi) ---------------------------------------

/// DNS record types we consume from the cache (forward A / AAAA only).
pub const DNS_TYPE_A: u16 = 0x0001;
pub const DNS_TYPE_AAAA: u16 = 0x001C;

/// `DnsQuery_W` option: answer only from the resolver cache — never a wire
/// query. This keeps domain resolution passive (no packets, no reverse DNS).
pub const DNS_QUERY_NO_WIRE_QUERY: DWORD = 0x10;

/// `DNS_FREE_TYPE::DnsFreeRecordList` — frees a `DnsQuery_W` record list.
pub const DNS_FREE_RECORD_LIST: u32 = 1;

/// One entry of the resolver cache from `DnsGetCacheDataTable` (undocumented but
/// ABI-stable since XP): a singly linked list carrying the cached name + record
/// type. The address data is not here — a cache-only `DnsQuery_W` per name
/// yields it. Layout locked by an offset test in `network.rs`.
#[repr(C)]
pub struct DNS_CACHE_ENTRY {
    pub pNext: *mut DNS_CACHE_ENTRY,
    pub pszName: LPWSTR,
    pub wType: u16,
    pub wDataLength: u16,
    pub dwFlags: ULONG,
}

/// Fixed head of a `DnsQuery_W` result record. The trailing `Data` union starts
/// at offset 32 on 64-bit (A = a 4-byte `IP4_ADDRESS`, AAAA = a 16-byte
/// `IP6_ADDRESS`). Only `pNext`, `pName`, `wType` and the address data are read;
/// layout locked by an offset test in `network.rs`.
#[repr(C)]
pub struct DNS_RECORD_HEAD {
    pub pNext: *mut DNS_RECORD_HEAD,
    pub pName: LPWSTR,
    pub wType: u16,
    pub wDataLength: u16,
    pub Flags: ULONG,
    pub dwTtl: ULONG,
    pub dwReserved: ULONG,
    // union { IP4_ADDRESS A; IP6_ADDRESS AAAA; ... } Data follows at offset 32.
}

/// Byte offset of the `Data` union inside `DNS_RECORD` on 64-bit Windows.
pub const DNS_RECORD_DATA_OFFSET: usize = 32;

#[link(name = "dnsapi")]
extern "system" {
    /// Returns the resolver cache as a linked list of [`DNS_CACHE_ENTRY`].
    /// Nonzero on success. The list is not freed here (a small, bounded
    /// per-call cost on an on-demand read — see `network.rs`), avoiding an
    /// undocumented free path that could corrupt the heap.
    pub fn DnsGetCacheDataTable(ppTable: *mut *mut DNS_CACHE_ENTRY) -> BOOL;

    /// Resolves `pszName` of `wType` from cache only (with
    /// `DNS_QUERY_NO_WIRE_QUERY`). 0 (`ERROR_SUCCESS`) on a cache hit; the
    /// records are freed with `DnsRecordListFree`.
    pub fn DnsQuery_W(
        pszName: LPCWSTR,
        wType: u16,
        Options: DWORD,
        pExtra: PVOID,
        ppQueryResults: *mut *mut DNS_RECORD_HEAD,
        pReserved: PVOID,
    ) -> LONG;

    /// Frees a `DnsQuery_W` record list (`freeType` = DNS_FREE_RECORD_LIST).
    pub fn DnsRecordListFree(pRecordList: *mut DNS_RECORD_HEAD, freeType: u32);
}

// ---------------------------------------------------------------------------
// R2 scheduled-tasks COM FFI (docs/phases.md Phase 2, PRD §9.9.2). Backs the
// Task Scheduler 2.0 collector. Hand-written COM: `CoCreateInstance` of
// `CLSID_TaskScheduler` yields an `ITaskService`; the folder/task collection
// interfaces are walked via explicit vtable calls (each interface's methods sit
// after the 7 `IDispatch` slots in IDL order). Only the interfaces actually
// walked are declared, and only up to the last method called — the unused
// leading slots (QueryInterface/AddRef and any skipped methods) are typed as
// opaque `usize` so the vtable offsets stay exact without importing signatures
// we never invoke. The static task definition (author, run level, idle/wake,
// actions, triggers) is read from each task's XML (`get_Xml`) rather than the
// deep `ITaskDefinition` interface tree — far less vtable surface for the same
// data. COM is confined to the collector thread (CoInitializeEx + CoUninitialize
// around the walk). Read-only throughout.
// ---------------------------------------------------------------------------

/// `HRESULT` — COM call status (S_OK == 0).
pub type HRESULT = LONG;
/// `BSTR` — a length-prefixed, NUL-terminated OLE string pointer.
pub type BSTR = *mut u16;
/// `VARIANT_BOOL` — VARIANT_TRUE (-1) / VARIANT_FALSE (0).
pub type VARIANT_BOOL = i16;
/// `DATE` — OLE automation date (days since 1899-12-30, fractional = time).
pub type DATE = f64;

pub const S_OK: HRESULT = 0;
pub const S_FALSE: HRESULT = 1;
/// `CoInitializeEx` returns this when the thread already has a different
/// apartment model — COM is usable, but we must not balance-uninit.
pub const RPC_E_CHANGED_MODE: HRESULT = 0x8001_0106_u32 as i32;
/// `COINIT_APARTMENTTHREADED` — an STA worker (Task Scheduler is happy in either).
pub const COINIT_APARTMENTTHREADED: DWORD = 0x2;
/// `CLSCTX_INPROC_SERVER` — taskschd.dll serves the object in-process.
pub const CLSCTX_INPROC_SERVER: DWORD = 0x1;

/// `TASK_STATE` values from `IRegisteredTask::get_State`.
pub const TASK_STATE_UNKNOWN: i32 = 0;
pub const TASK_STATE_DISABLED: i32 = 1;
pub const TASK_STATE_QUEUED: i32 = 2;
pub const TASK_STATE_READY: i32 = 3;
pub const TASK_STATE_RUNNING: i32 = 4;

/// `CLSID_TaskScheduler` = {0F87369F-A4E5-4CFC-BD3E-73E6154572DD}.
pub const CLSID_TASK_SCHEDULER: GUID = GUID {
    Data1: 0x0F87_369F,
    Data2: 0xA4E5,
    Data3: 0x4CFC,
    Data4: [0xBD, 0x3E, 0x73, 0xE6, 0x15, 0x45, 0x72, 0xDD],
};

/// `IID_ITaskService` = {2FABA4C7-4DA9-4013-9697-20CC3FD40F85}.
pub const IID_ITASK_SERVICE: GUID = GUID {
    Data1: 0x2FAB_A4C7,
    Data2: 0x4DA9,
    Data3: 0x4013,
    Data4: [0x96, 0x97, 0x20, 0xCC, 0x3F, 0xD4, 0x0F, 0x85],
};

/// A 24-byte `VARIANT` (64-bit ABI). Only VT_EMPTY (Connect args) and VT_I4
/// (collection `get_Item` index) are constructed; the value union is exposed as
/// a raw `i64` slot at offset 8 which comfortably holds a LONG.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VARIANT {
    pub vt: u16,
    pub wReserved1: u16,
    pub wReserved2: u16,
    pub wReserved3: u16,
    /// The value union (8-byte aligned). `llVal` for the widest simple case.
    pub val: i64,
    /// Padding so the union spans the full 16-byte VARIANT tail.
    pub val_hi: i64,
}

pub const VT_EMPTY: u16 = 0;
pub const VT_I4: u16 = 3;

impl VARIANT {
    /// A VT_EMPTY variant (the "not supplied" argument, e.g. local Connect).
    pub fn empty() -> Self {
        VARIANT {
            vt: VT_EMPTY,
            wReserved1: 0,
            wReserved2: 0,
            wReserved3: 0,
            val: 0,
            val_hi: 0,
        }
    }

    /// A VT_I4 variant carrying `n` (a 1-based collection index).
    pub fn i4(n: i32) -> Self {
        VARIANT {
            vt: VT_I4,
            wReserved1: 0,
            wReserved2: 0,
            wReserved3: 0,
            val: n as i64,
            val_hi: 0,
        }
    }
}

/// Minimal `IUnknown` vtable prefix — used to `Release` any COM interface (its
/// `Release` is always slot 2). The interface pointer's first field is a
/// `*const IUnknownVtbl`.
#[repr(C)]
pub struct IUnknownVtbl {
    pub QueryInterface: usize,
    pub AddRef: usize,
    pub Release: unsafe extern "system" fn(this: PVOID) -> ULONG,
}

/// `ITaskService` vtable, declared through `Connect` (the last method called).
/// Slots 0..=6 are IDispatch; 7=GetFolder, 8=GetRunningTasks, 9=NewTask,
/// 10=Connect.
#[repr(C)]
pub struct ITaskServiceVtbl {
    pub QueryInterface: usize,
    pub AddRef: usize,
    pub Release: unsafe extern "system" fn(this: PVOID) -> ULONG,
    pub GetTypeInfoCount: usize,
    pub GetTypeInfo: usize,
    pub GetIDsOfNames: usize,
    pub Invoke: usize,
    pub GetFolder:
        unsafe extern "system" fn(this: PVOID, path: BSTR, ppFolder: *mut PVOID) -> HRESULT,
    pub GetRunningTasks: usize,
    pub NewTask: usize,
    pub Connect: unsafe extern "system" fn(
        this: PVOID,
        serverName: VARIANT,
        user: VARIANT,
        domain: VARIANT,
        password: VARIANT,
    ) -> HRESULT,
}

/// `ITaskFolder` vtable through `GetTasks`. 7=get_Name, 8=get_Path, 9=GetFolder,
/// 10=GetFolders, 11=CreateFolder, 12=DeleteFolder, 13=GetTask, 14=GetTasks.
#[repr(C)]
pub struct ITaskFolderVtbl {
    pub QueryInterface: usize,
    pub AddRef: usize,
    pub Release: unsafe extern "system" fn(this: PVOID) -> ULONG,
    pub GetTypeInfoCount: usize,
    pub GetTypeInfo: usize,
    pub GetIDsOfNames: usize,
    pub Invoke: usize,
    pub get_Name: usize,
    pub get_Path: unsafe extern "system" fn(this: PVOID, pPath: *mut BSTR) -> HRESULT,
    pub GetFolder: usize,
    pub GetFolders:
        unsafe extern "system" fn(this: PVOID, flags: LONG, ppFolders: *mut PVOID) -> HRESULT,
    pub CreateFolder: usize,
    pub DeleteFolder: usize,
    pub GetTask: usize,
    pub GetTasks:
        unsafe extern "system" fn(this: PVOID, flags: LONG, ppTasks: *mut PVOID) -> HRESULT,
}

/// A collection vtable (shared shape for `ITaskFolderCollection` and
/// `IRegisteredTaskCollection`): 7=get_Count, 8=get_Item(VARIANT index).
#[repr(C)]
pub struct ICollectionVtbl {
    pub QueryInterface: usize,
    pub AddRef: usize,
    pub Release: unsafe extern "system" fn(this: PVOID) -> ULONG,
    pub GetTypeInfoCount: usize,
    pub GetTypeInfo: usize,
    pub GetIDsOfNames: usize,
    pub Invoke: usize,
    pub get_Count: unsafe extern "system" fn(this: PVOID, pCount: *mut LONG) -> HRESULT,
    pub get_Item:
        unsafe extern "system" fn(this: PVOID, index: VARIANT, ppItem: *mut PVOID) -> HRESULT,
}

/// `IRegisteredTask` vtable through `get_Xml`. 7=get_Name, 8=get_Path,
/// 9=get_State, 10=get_Enabled, 11=put_Enabled, 12=Run, 13=RunEx,
/// 14=GetInstances, 15=get_LastRunTime, 16=get_LastTaskResult,
/// 17=get_NumberOfMissedRuns, 18=get_NextRunTime, 19=get_Definition, 20=get_Xml.
#[repr(C)]
pub struct IRegisteredTaskVtbl {
    pub QueryInterface: usize,
    pub AddRef: usize,
    pub Release: unsafe extern "system" fn(this: PVOID) -> ULONG,
    pub GetTypeInfoCount: usize,
    pub GetTypeInfo: usize,
    pub GetIDsOfNames: usize,
    pub Invoke: usize,
    pub get_Name: unsafe extern "system" fn(this: PVOID, pName: *mut BSTR) -> HRESULT,
    pub get_Path: unsafe extern "system" fn(this: PVOID, pPath: *mut BSTR) -> HRESULT,
    pub get_State: unsafe extern "system" fn(this: PVOID, pState: *mut i32) -> HRESULT,
    pub get_Enabled: unsafe extern "system" fn(this: PVOID, pEnabled: *mut VARIANT_BOOL) -> HRESULT,
    pub put_Enabled: usize,
    pub Run: usize,
    pub RunEx: usize,
    pub GetInstances: usize,
    pub get_LastRunTime: unsafe extern "system" fn(this: PVOID, pLastRun: *mut DATE) -> HRESULT,
    pub get_LastTaskResult: unsafe extern "system" fn(this: PVOID, pResult: *mut LONG) -> HRESULT,
    pub get_NumberOfMissedRuns: usize,
    pub get_NextRunTime: unsafe extern "system" fn(this: PVOID, pNextRun: *mut DATE) -> HRESULT,
    pub get_Definition: usize,
    pub get_Xml: unsafe extern "system" fn(this: PVOID, pXml: *mut BSTR) -> HRESULT,
}

#[link(name = "ole32")]
extern "system" {
    /// Initializes COM on the calling thread with `dwCoInit`. S_FALSE means
    /// already initialized (still balance with CoUninitialize);
    /// RPC_E_CHANGED_MODE means a different model is active (do not uninit).
    pub fn CoInitializeEx(pvReserved: PVOID, dwCoInit: DWORD) -> HRESULT;

    /// Balances a successful `CoInitializeEx` on this thread.
    pub fn CoUninitialize();

    /// Creates a COM object of `rclsid` and returns its `riid` interface.
    pub fn CoCreateInstance(
        rclsid: *const GUID,
        pUnkOuter: PVOID,
        dwClsContext: DWORD,
        riid: *const GUID,
        ppv: *mut PVOID,
    ) -> HRESULT;
}

#[link(name = "oleaut32")]
extern "system" {
    /// Allocates a `BSTR` from a NUL-terminated UTF-16 string (for the Connect /
    /// GetFolder path arguments).
    pub fn SysAllocString(psz: *const u16) -> BSTR;

    /// Frees a `BSTR` (from `SysAllocString` or returned by a COM getter).
    pub fn SysFreeString(bstr: BSTR);
}

// ---------------------------------------------------------------------------
// R2 boot-analysis FFI (docs/phases.md Phase 2, PRD §9.8.4). Backs the boot
// collector via the Windows Event Log (wevtapi): `EvtQuery` the
// `Microsoft-Windows-Diagnostics-Performance/Operational` channel for event 100
// (boot performance), newest-first, then `EvtNext` + `EvtRender` each event to
// XML and parse the boot timings out of its `EventData`. All read-only.
// `EVT_HANDLE` is an opaque handle closed with `EvtClose`.
// ---------------------------------------------------------------------------

/// `EVT_HANDLE` — an opaque event-log query/result/event handle.
pub type EVT_HANDLE = HANDLE;

/// `EvtQuery` flags: interpret the path as a channel name, read newest-first.
pub const EVT_QUERY_CHANNEL_PATH: DWORD = 0x1;
pub const EVT_QUERY_REVERSE_DIRECTION: DWORD = 0x200;
/// `EvtRender` flag: render the event as an XML string.
pub const EVT_RENDER_EVENT_XML: DWORD = 1;
/// `EvtQuery`/`EvtOpenChannel` error: the named channel does not exist.
pub const ERROR_EVT_CHANNEL_NOT_FOUND: DWORD = 15007;
/// Generic access-denied (channel readable only elevated / not in the group).
pub const ERROR_ACCESS_DENIED: DWORD = 5;

#[link(name = "wevtapi")]
extern "system" {
    /// Runs `query` (an XPath filter) against `path` (a channel or log-file),
    /// returning a result-set handle. NULL on failure (check `GetLastError`).
    pub fn EvtQuery(Session: EVT_HANDLE, Path: LPCWSTR, Query: LPCWSTR, Flags: DWORD)
        -> EVT_HANDLE;

    /// Fetches up to `EventsSize` event handles from a result set into `Events`,
    /// writing the count to `*Returned`. FALSE + ERROR_NO_MORE_ITEMS at the end.
    pub fn EvtNext(
        ResultSet: EVT_HANDLE,
        EventsSize: DWORD,
        Events: *mut EVT_HANDLE,
        Timeout: DWORD,
        Flags: DWORD,
        Returned: *mut DWORD,
    ) -> BOOL;

    /// Renders an event (`Fragment`) per `Flags` (EVT_RENDER_EVENT_XML here) into
    /// `Buffer`. Two-call size pattern via `BufferUsed`.
    pub fn EvtRender(
        Context: EVT_HANDLE,
        Fragment: EVT_HANDLE,
        Flags: DWORD,
        BufferSize: DWORD,
        Buffer: PVOID,
        BufferUsed: *mut DWORD,
        PropertyCount: *mut DWORD,
    ) -> BOOL;

    /// Closes any `EVT_HANDLE` (query, result set, or event).
    pub fn EvtClose(Object: EVT_HANDLE) -> BOOL;
}

// ---------------------------------------------------------------------------
// R2 battery + thermal FFI (docs/phases.md Phase 2, PRD §9.6.6/§9.6.7). Battery:
// `GetSystemPowerStatus` for AC/charge, then the battery device interface
// (`SetupDiGetClassDevs(GUID_DEVCLASS_BATTERY)` +
// `DeviceIoControl(IOCTL_BATTERY_*)`) for design/full-charge capacity, rate and
// cycle count. Thermal is served through WMI (`MSAcpi_ThermalZoneTemperature`)
// in `power.rs` via the same COM primitives declared for scheduled tasks. All
// read-only; absent hardware degrades honestly (available=false).
// ---------------------------------------------------------------------------

/// `SYSTEM_POWER_STATUS` from `GetSystemPowerStatus`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SYSTEM_POWER_STATUS {
    pub ACLineStatus: u8,
    pub BatteryFlag: u8,
    /// 0..=100, or 255 when unknown.
    pub BatteryLifePercent: u8,
    pub SystemStatusFlag: u8,
    /// Seconds of remaining runtime, or -1 (0xFFFFFFFF) when unknown.
    pub BatteryLifeTime: u32,
    pub BatteryFullLifeTime: u32,
}

/// `PROCESS_POWER_THROTTLING_STATE` — the EcoQoS request/response. Enabling
/// EcoQoS sets both masks to `EXECUTION_SPEED`; disabling throttling sets
/// `ControlMask = EXECUTION_SPEED, StateMask = 0`; both zero returns the process
/// to system-managed (the default).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PROCESS_POWER_THROTTLING_STATE {
    pub Version: u32,
    pub ControlMask: u32,
    pub StateMask: u32,
}

#[link(name = "kernel32")]
extern "system" {
    /// Sets the priority class of `hProcess` (needs PROCESS_SET_INFORMATION).
    pub fn SetPriorityClass(hProcess: HANDLE, dwPriorityClass: DWORD) -> BOOL;

    /// Reads the priority class of `hProcess` (needs query rights). 0 on failure.
    pub fn GetPriorityClass(hProcess: HANDLE) -> DWORD;

    /// Sets the processor affinity mask of `hProcess` (needs SET_INFORMATION).
    pub fn SetProcessAffinityMask(hProcess: HANDLE, dwProcessAffinityMask: usize) -> BOOL;

    /// Reads the process + system affinity masks of `hProcess` (query rights).
    pub fn GetProcessAffinityMask(
        hProcess: HANDLE,
        lpProcessAffinityMask: *mut usize,
        lpSystemAffinityMask: *mut usize,
    ) -> BOOL;

    /// Sets a process information class (here `ProcessPowerThrottling` for
    /// EcoQoS). Needs PROCESS_SET_INFORMATION.
    pub fn SetProcessInformation(
        hProcess: HANDLE,
        ProcessInformationClass: u32,
        ProcessInformation: PVOID,
        ProcessInformationSize: DWORD,
    ) -> BOOL;

    /// Reads a process information class (here `ProcessPowerThrottling`, to
    /// capture the original EcoQoS state for reversal). Needs query rights.
    pub fn GetProcessInformation(
        hProcess: HANDLE,
        ProcessInformationClass: u32,
        ProcessInformation: PVOID,
        ProcessInformationSize: DWORD,
    ) -> BOOL;

    /// Sets `hProcess`'s default CPU sets (P/E-core steering). NULL/0 clears the
    /// assignment. Needs PROCESS_SET_LIMITED_INFORMATION.
    pub fn SetProcessDefaultCpuSets(
        Process: HANDLE,
        CpuSetIds: *const u32,
        CpuSetIdCount: u32,
    ) -> BOOL;

    /// Reads `hProcess`'s default CPU set assignment (to capture the original for
    /// reversal). `RequiredIdCount` receives the count; 0 = none assigned.
    pub fn GetProcessDefaultCpuSets(
        Process: HANDLE,
        CpuSetIds: *mut u32,
        CpuSetIdCount: u32,
        RequiredIdCount: *mut u32,
    ) -> BOOL;

    /// Enumerates the system's CPU sets (for P vs E core detection via each
    /// set's `EfficiencyClass`). Two-call size pattern via `ReturnedLength`.
    /// `Process` NULL = system-wide; `Flags` reserved 0.
    pub fn GetSystemCpuSetInformation(
        Information: *mut u8,
        BufferLength: u32,
        ReturnedLength: *mut u32,
        Process: HANDLE,
        Flags: u32,
    ) -> BOOL;

    /// AC/DC + battery power state (ON_AC_POWER / ON_DC_POWER triggers).
    pub fn GetSystemPowerStatus(lpSystemPowerStatus: *mut SYSTEM_POWER_STATUS) -> BOOL;

    /// Loads a DLL by name (used to resolve the lightly-documented
    /// `PowerSetActiveOverlayScheme` at runtime; it is not in the import lib).
    /// `GetProcAddress` for symbol resolution is declared once above (shared
    /// with the Wintrust dynamic-link helpers).
    pub fn LoadLibraryW(lpLibFileName: *const u16) -> HMODULE;

    /// Releases a module handle from `LoadLibraryW`.
    pub fn FreeLibrary(hLibModule: HMODULE) -> BOOL;
}

#[link(name = "user32")]
extern "system" {
    /// Handle of the current foreground window (ON_FULLSCREEN trigger). NULL when
    /// no window has focus; the owning pid comes from `GetWindowThreadProcessId`.
    pub fn GetForegroundWindow() -> HWND;
}

/// The `PowerSetActiveOverlayScheme(*const GUID) -> DWORD` signature, resolved
/// dynamically from `powrprof.dll` (the export is not in the SDK import library).
/// Sets the active power *overlay* scheme (the "power mode" slider: Better
/// Battery / Balanced / Best Performance). Feature-flagged: an unresolved export
/// degrades to a no-op (PRD §9.7.4).
pub type PowerSetActiveOverlaySchemeFn = unsafe extern "system" fn(*const GUID) -> DWORD;

/// Power-overlay GUIDs (the "power mode" slider). The all-zero GUID selects the
/// recommended/Balanced overlay.
pub const OVERLAY_BALANCED: GUID = GUID {
    Data1: 0,
    Data2: 0,
    Data3: 0,
    Data4: [0; 8],
};
/// "Better Battery" (power-saver) overlay.
pub const OVERLAY_POWER_SAVER: GUID = GUID {
    Data1: 0x961c_c777,
    Data2: 0x2547,
    Data3: 0x4f9d,
    Data4: [0x81, 0x74, 0x7d, 0x86, 0x18, 0x1b, 0x8a, 0x7a],
};
/// "Best Performance" overlay.
pub const OVERLAY_HIGH_PERFORMANCE: GUID = GUID {
    Data1: 0xded5_74b5,
    Data2: 0x45a0,
    Data3: 0x4f42,
    Data4: [0x87, 0x37, 0x46, 0x34, 0x5c, 0x09, 0xc2, 0x38],
};
/// `GUID_DEVCLASS_BATTERY` = {72631E54-78A4-11D0-BCF7-00AA00B7B32A}.
pub const GUID_DEVCLASS_BATTERY: GUID = GUID {
    Data1: 0x7263_1E54,
    Data2: 0x78A4,
    Data3: 0x11D0,
    Data4: [0xBC, 0xF7, 0x00, 0xAA, 0x00, 0xB7, 0xB3, 0x2A],
};

/// `SetupDiGetClassDevs` flags: present devices exposing the interface.
pub const DIGCF_PRESENT: DWORD = 0x02;
pub const DIGCF_DEVICEINTERFACE: DWORD = 0x10;
/// Sentinel handle value returned by `SetupDiGetClassDevs` on failure.
pub const INVALID_HANDLE_VALUE: HANDLE = usize::MAX as *mut c_void;

/// `SP_DEVICE_INTERFACE_DATA`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SP_DEVICE_INTERFACE_DATA {
    pub cbSize: DWORD,
    pub InterfaceClassGuid: GUID,
    pub Flags: DWORD,
    pub Reserved: usize,
}

/// `SP_DEVICE_INTERFACE_DETAIL_DATA_W` header. The device path (a WCHAR array)
/// follows `DevicePath[0]`; we over-allocate a byte buffer and read the tail.
#[repr(C)]
pub struct SP_DEVICE_INTERFACE_DETAIL_DATA_W {
    pub cbSize: DWORD,
    pub DevicePath: [u16; 1],
}

/// `CreateFile` sharing/access constants for opening the battery device.
pub const GENERIC_READ: DWORD = 0x8000_0000;
pub const GENERIC_WRITE: DWORD = 0x4000_0000;
pub const FILE_SHARE_READ: DWORD = 0x1;
pub const FILE_SHARE_WRITE: DWORD = 0x2;
pub const OPEN_EXISTING: DWORD = 3;

/// Battery IOCTLs (from batclass.h / winioctl.h).
pub const IOCTL_BATTERY_QUERY_TAG: DWORD = 0x0029_4040;
pub const IOCTL_BATTERY_QUERY_INFORMATION: DWORD = 0x0029_4044;
pub const IOCTL_BATTERY_QUERY_STATUS: DWORD = 0x0029_404C;

/// `BATTERY_QUERY_INFORMATION_LEVEL` values.
pub const BATTERY_INFORMATION_LEVEL: u32 = 0;

/// `BATTERY_INFORMATION.Capabilities` flag: capacities are relative (%), not mWh.
pub const BATTERY_CAPACITY_RELATIVE: u32 = 0x4000_0000;

/// `BATTERY_QUERY_INFORMATION` input for `IOCTL_BATTERY_QUERY_INFORMATION`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BATTERY_QUERY_INFORMATION {
    pub BatteryTag: u32,
    pub InformationLevel: u32,
    pub AtRate: i32,
}

/// `BATTERY_INFORMATION` (InformationLevel = BatteryInformation).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BATTERY_INFORMATION {
    pub Capabilities: u32,
    pub Technology: u8,
    pub Reserved: [u8; 3],
    pub Chemistry: [u8; 4],
    pub DesignedCapacity: u32,
    pub FullChargedCapacity: u32,
    pub DefaultAlert1: u32,
    pub DefaultAlert2: u32,
    pub CriticalBias: u32,
    pub CycleCount: u32,
}

/// `BATTERY_WAIT_STATUS` input for `IOCTL_BATTERY_QUERY_STATUS`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BATTERY_WAIT_STATUS {
    pub BatteryTag: u32,
    pub Timeout: u32,
    pub PowerState: u32,
    pub LowCapacity: u32,
    pub HighCapacity: u32,
}

/// `BATTERY_STATUS` output for `IOCTL_BATTERY_QUERY_STATUS`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BATTERY_STATUS {
    pub PowerState: u32,
    /// Remaining capacity in mWh (or BATTERY_UNKNOWN_CAPACITY).
    pub Capacity: u32,
    /// Battery voltage in mV (or BATTERY_UNKNOWN_VOLTAGE).
    pub Voltage: u32,
    /// Charge/discharge rate in mW; negative = discharging.
    pub Rate: i32,
}

/// `BATTERY_STATUS.PowerState` bits.
pub const BATTERY_POWER_ON_LINE: u32 = 0x0000_0001;
pub const BATTERY_CHARGING: u32 = 0x0000_0004;
pub const BATTERY_DISCHARGING: u32 = 0x0000_0008;
/// Sentinel for an unknown capacity/rate reading.
pub const BATTERY_UNKNOWN_CAPACITY: u32 = 0xFFFF_FFFF;
pub const BATTERY_UNKNOWN_RATE: i32 = 0x8000_0000_u32 as i32;

#[link(name = "kernel32")]
extern "system" {
    // GetSystemPowerStatus is declared once above (shared with the rules-engine
    // AC/DC trigger); the battery collector reuses that declaration.

    /// Opens a device/file by path (the battery device interface path here).
    pub fn CreateFileW(
        lpFileName: LPCWSTR,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: PVOID,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;

    /// Issues a device control request (the battery IOCTLs).
    pub fn DeviceIoControl(
        hDevice: HANDLE,
        dwIoControlCode: DWORD,
        lpInBuffer: PVOID,
        nInBufferSize: DWORD,
        lpOutBuffer: PVOID,
        nOutBufferSize: DWORD,
        lpBytesReturned: *mut DWORD,
        lpOverlapped: PVOID,
    ) -> BOOL;
}

#[link(name = "setupapi")]
extern "system" {
    /// Returns a device information set for the given interface class present on
    /// the machine. `INVALID_HANDLE_VALUE` on failure.
    pub fn SetupDiGetClassDevsW(
        ClassGuid: *const GUID,
        Enumerator: LPCWSTR,
        hwndParent: HANDLE,
        Flags: DWORD,
    ) -> HANDLE;

    /// Enumerates the `MemberIndex`-th device interface in the set.
    pub fn SetupDiEnumDeviceInterfaces(
        DeviceInfoSet: HANDLE,
        DeviceInfoData: PVOID,
        InterfaceClassGuid: *const GUID,
        MemberIndex: DWORD,
        DeviceInterfaceData: *mut SP_DEVICE_INTERFACE_DATA,
    ) -> BOOL;

    /// Retrieves the device interface detail (the device path). Two-call size
    /// pattern via `RequiredSize`.
    pub fn SetupDiGetDeviceInterfaceDetailW(
        DeviceInfoSet: HANDLE,
        DeviceInterfaceData: *const SP_DEVICE_INTERFACE_DATA,
        DeviceInterfaceDetailData: *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W,
        DeviceInterfaceDetailDataSize: DWORD,
        RequiredSize: *mut DWORD,
        DeviceInfoData: PVOID,
    ) -> BOOL;

    /// Frees a device information set from `SetupDiGetClassDevsW`.
    pub fn SetupDiDestroyDeviceInfoList(DeviceInfoSet: HANDLE) -> BOOL;
}

// ---------------------------------------------------------------------------
// R2 thermal WMI FFI (docs/phases.md Phase 2, PRD §9.6.7). Backs the thermal
// collector via WMI's `MSAcpi_ThermalZoneTemperature` class in the `root\WMI`
// namespace. Hand-written COM over the WBEM interfaces (IWbemLocator →
// IWbemServices → IEnumWbemClassObject → IWbemClassObject), all of which derive
// directly from IUnknown (no IDispatch prefix). Only the methods walked are
// typed; earlier slots are opaque `usize` placeholders to keep vtable offsets
// exact. Read-only queries; absent sensors degrade honestly (available=false).
// ---------------------------------------------------------------------------

/// `CLSID_WbemLocator` = {4590F811-1D3A-11D0-891F-00AA004B2E24}.
pub const CLSID_WBEM_LOCATOR: GUID = GUID {
    Data1: 0x4590_F811,
    Data2: 0x1D3A,
    Data3: 0x11D0,
    Data4: [0x89, 0x1F, 0x00, 0xAA, 0x00, 0x4B, 0x2E, 0x24],
};

/// `IID_IWbemLocator` = {DC12A687-737F-11CF-884D-00AA004B2E24}.
pub const IID_IWBEM_LOCATOR: GUID = GUID {
    Data1: 0xDC12_A687,
    Data2: 0x737F,
    Data3: 0x11CF,
    Data4: [0x88, 0x4D, 0x00, 0xAA, 0x00, 0x4B, 0x2E, 0x24],
};

/// `ExecQuery`/enumeration flags: a fast forward-only enumerator.
pub const WBEM_FLAG_FORWARD_ONLY: LONG = 0x20;
pub const WBEM_FLAG_RETURN_IMMEDIATELY: LONG = 0x10;
/// `IEnumWbemClassObject::Next` timeout: block indefinitely.
pub const WBEM_INFINITE: LONG = 0xFFFF_FFFF_u32 as i32;

/// COM security constants for `CoInitializeSecurity` / `CoSetProxyBlanket`.
pub const RPC_C_AUTHN_WINNT: DWORD = 10;
pub const RPC_C_AUTHZ_NONE: DWORD = 0;
pub const RPC_C_AUTHN_LEVEL_DEFAULT: DWORD = 0;
pub const RPC_C_AUTHN_LEVEL_CALL: DWORD = 3;
pub const RPC_C_IMP_LEVEL_IMPERSONATE: DWORD = 3;
pub const EOAC_NONE: DWORD = 0;
/// `CoInitializeSecurity` when already called this process — benign to ignore.
pub const RPC_E_TOO_LATE: HRESULT = 0x8001_0119_u32 as i32;
/// Sentinel for the "use default" cAuthSvc argument.
pub const COLE_DEFAULT_AUTHINFO: isize = -1;

/// `IWbemLocator` vtable through `ConnectServer` (slot 3; derives from IUnknown).
#[repr(C)]
pub struct IWbemLocatorVtbl {
    pub QueryInterface: usize,
    pub AddRef: usize,
    pub Release: unsafe extern "system" fn(this: PVOID) -> ULONG,
    pub ConnectServer: unsafe extern "system" fn(
        this: PVOID,
        strNetworkResource: BSTR,
        strUser: BSTR,
        strPassword: BSTR,
        strLocale: BSTR,
        lSecurityFlags: LONG,
        strAuthority: BSTR,
        pCtx: PVOID,
        ppNamespace: *mut PVOID,
    ) -> HRESULT,
}

/// `IWbemServices` vtable through `ExecQuery` (slot 20). Slots 3..=19 are the
/// namespace/class/instance methods we never call, kept as opaque placeholders.
#[repr(C)]
pub struct IWbemServicesVtbl {
    pub QueryInterface: usize,
    pub AddRef: usize,
    pub Release: unsafe extern "system" fn(this: PVOID) -> ULONG,
    pub OpenNamespace: usize,
    pub CancelAsyncCall: usize,
    pub QueryObjectSink: usize,
    pub GetObject: usize,
    pub GetObjectAsync: usize,
    pub PutClass: usize,
    pub PutClassAsync: usize,
    pub DeleteClass: usize,
    pub DeleteClassAsync: usize,
    pub CreateClassEnum: usize,
    pub CreateClassEnumAsync: usize,
    pub PutInstance: usize,
    pub PutInstanceAsync: usize,
    pub DeleteInstance: usize,
    pub DeleteInstanceAsync: usize,
    pub CreateInstanceEnum: usize,
    pub CreateInstanceEnumAsync: usize,
    pub ExecQuery: unsafe extern "system" fn(
        this: PVOID,
        strQueryLanguage: BSTR,
        strQuery: BSTR,
        lFlags: LONG,
        pCtx: PVOID,
        ppEnum: *mut PVOID,
    ) -> HRESULT,
}

/// `IEnumWbemClassObject` vtable through `Next` (slot 4).
#[repr(C)]
pub struct IEnumWbemClassObjectVtbl {
    pub QueryInterface: usize,
    pub AddRef: usize,
    pub Release: unsafe extern "system" fn(this: PVOID) -> ULONG,
    pub Reset: usize,
    pub Next: unsafe extern "system" fn(
        this: PVOID,
        lTimeout: LONG,
        uCount: ULONG,
        apObjects: *mut PVOID,
        puReturned: *mut ULONG,
    ) -> HRESULT,
}

/// `IWbemClassObject` vtable through `Get` (slot 4).
#[repr(C)]
pub struct IWbemClassObjectVtbl {
    pub QueryInterface: usize,
    pub AddRef: usize,
    pub Release: unsafe extern "system" fn(this: PVOID) -> ULONG,
    pub GetQualifierSet: usize,
    pub Get: unsafe extern "system" fn(
        this: PVOID,
        wszName: LPCWSTR,
        lFlags: LONG,
        pVal: *mut VARIANT,
        pType: *mut LONG,
        plFlavor: *mut LONG,
    ) -> HRESULT,
}

#[link(name = "ole32")]
extern "system" {
    /// Registers process-wide default COM security (WMI needs it before the
    /// first call). RPC_E_TOO_LATE if already set — benign.
    pub fn CoInitializeSecurity(
        pSecDesc: PVOID,
        cAuthSvc: isize,
        asAuthSvc: PVOID,
        pReserved1: PVOID,
        dwAuthnLevel: DWORD,
        dwImpLevel: DWORD,
        pAuthList: PVOID,
        dwCapabilities: DWORD,
        pReserved3: PVOID,
    ) -> HRESULT;

    /// Sets the authentication blanket on a proxy (the IWbemServices proxy).
    pub fn CoSetProxyBlanket(
        pProxy: PVOID,
        dwAuthnSvc: DWORD,
        dwAuthzSvc: DWORD,
        pServerPrincName: PVOID,
        dwAuthnLevel: DWORD,
        dwImpLevel: DWORD,
        pAuthInfo: PVOID,
        dwCapabilities: DWORD,
    ) -> HRESULT;
}

#[link(name = "oleaut32")]
extern "system" {
    /// Clears a VARIANT, freeing any owned BSTR/interface (after reading Get()).
    pub fn VariantClear(pvarg: *mut VARIANT) -> HRESULT;
}
