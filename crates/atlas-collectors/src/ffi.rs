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
pub const WTD_STATEACTION_VERIFY: DWORD = 1;
pub const WTD_STATEACTION_CLOSE: DWORD = 2;
pub const WTD_REVOCATION_CHECK_NONE: DWORD = 0x0000_0010;
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
    pub pFile: *const WINTRUST_FILE_INFO,
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
