//! Windows service host: hand-written advapi32 SCM FFI + a pure, unit-tested
//! service-control state machine (docs/phases.md M9; tech-stack §4.1/§13.2).
//!
//! Deliberately no `windows-service` crate: the SCM surface we need is a dozen
//! stable-ABI advapi32 calls, hand-written in the same style as
//! `atlas-collectors::ffi` (which already owns the read-only SCM enumeration
//! path). Owning the definitions keeps the whole unsafe surface reviewable here.
//!
//! ## What is pure vs. what needs elevation
//! The service-control *protocol* — the SERVICE_STATUS transitions, checkpoint
//! bumping while a phase is pending, the accepted-controls mask, and the
//! control-code classification — is a pure [`ServiceStateMachine`] with no FFI,
//! exercised by the unit tests below. The FFI shell ([`run_service`],
//! [`install`], [`uninstall`], [`query_status`]) is thin: it marshals arguments,
//! calls the SCM, and maps `GetLastError` to friendly outcomes. Installing,
//! starting, and stopping a real service all require elevation and the Service
//! Control Manager, so those paths cannot be exercised unprivileged in CI; they
//! are covered by an elevated live run (see the module smoke notes in the M9
//! report). The unprivileged paths that ARE reachable — `service status` on a
//! missing service, and `service install`/`run` without elevation — return the
//! documented access-denied / not-under-SCM outcomes and are smoke-tested.

#![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

// ---------------------------------------------------------------------------
// Pure service-control state machine (no FFI; unit-tested on every platform).
// ---------------------------------------------------------------------------

/// `SERVICE_STATUS.dwCurrentState` values we drive between.
pub const SERVICE_STOPPED: u32 = 1;
pub const SERVICE_START_PENDING: u32 = 2;
pub const SERVICE_STOP_PENDING: u32 = 3;
pub const SERVICE_RUNNING: u32 = 4;

/// `dwControlsAccepted` bits. We accept STOP and SHUTDOWN only (no pause/continue
/// — collection has no meaningful paused state).
pub const SERVICE_ACCEPT_STOP: u32 = 0x0000_0001;
pub const SERVICE_ACCEPT_SHUTDOWN: u32 = 0x0000_0004;
/// The mask reported while RUNNING.
pub const ACCEPTED_CONTROLS: u32 = SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN;

/// Control codes delivered to the handler.
pub const SERVICE_CONTROL_STOP: u32 = 0x0000_0001;
pub const SERVICE_CONTROL_INTERROGATE: u32 = 0x0000_0004;
pub const SERVICE_CONTROL_SHUTDOWN: u32 = 0x0000_0005;

/// Wait hints (ms) reported during the pending phases so the SCM knows how long
/// to allow before treating the service as hung. Start is quick (open the store,
/// spawn the sampler); stop drains the writer, so it is given more slack.
pub const START_WAIT_HINT_MS: u32 = 10_000;
pub const STOP_WAIT_HINT_MS: u32 = 20_000;

/// The SERVICE_STATUS field set the host writes on each `SetServiceStatus`,
/// computed purely by the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusReport {
    pub current_state: u32,
    pub controls_accepted: u32,
    pub checkpoint: u32,
    pub wait_hint_ms: u32,
    pub win32_exit_code: u32,
}

/// Lifecycle events that drive [`ServiceStateMachine::advance`].
#[derive(Debug, Clone, Copy)]
pub enum LifecycleEvent {
    /// ServiceMain entered (or init is still progressing): report START_PENDING.
    /// Repeated events bump the checkpoint to show forward progress.
    StartPending,
    /// Init done, workload serving: report RUNNING.
    Running,
    /// A STOP/SHUTDOWN control arrived (or drain is progressing): report
    /// STOP_PENDING, bumping the checkpoint on repeats.
    StopRequested,
    /// Drain complete: report STOPPED with `win32_exit_code` (0 = clean).
    Stopped { win32_exit_code: u32 },
}

/// Pure state machine computing the next SERVICE_STATUS from a lifecycle event.
/// Holds only the current state + the checkpoint counter; the FFI shell owns the
/// real SERVICE_STATUS_HANDLE and simply writes whatever this returns.
#[derive(Debug, Clone, Copy)]
pub struct ServiceStateMachine {
    state: u32,
    checkpoint: u32,
}

impl Default for ServiceStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceStateMachine {
    pub fn new() -> Self {
        Self {
            state: SERVICE_STOPPED,
            checkpoint: 0,
        }
    }

    /// Current `dwCurrentState`.
    pub fn state(&self) -> u32 {
        self.state
    }

    /// Advance the machine and return the SERVICE_STATUS fields to report.
    ///
    /// Checkpoint discipline: while in a *pending* phase the checkpoint starts at
    /// 1 and increments on each subsequent same-phase event (progress signal);
    /// reaching a terminal/steady phase (RUNNING/STOPPED) resets it to 0. The
    /// accepted-controls mask is only non-empty while RUNNING — a service must
    /// not advertise it accepts STOP until it can actually honour one.
    pub fn advance(&mut self, ev: LifecycleEvent) -> StatusReport {
        match ev {
            LifecycleEvent::StartPending => {
                if self.state == SERVICE_START_PENDING {
                    self.checkpoint += 1;
                } else {
                    self.state = SERVICE_START_PENDING;
                    self.checkpoint = 1;
                }
                StatusReport {
                    current_state: SERVICE_START_PENDING,
                    controls_accepted: 0,
                    checkpoint: self.checkpoint,
                    wait_hint_ms: START_WAIT_HINT_MS,
                    win32_exit_code: 0,
                }
            }
            LifecycleEvent::Running => {
                self.state = SERVICE_RUNNING;
                self.checkpoint = 0;
                StatusReport {
                    current_state: SERVICE_RUNNING,
                    controls_accepted: ACCEPTED_CONTROLS,
                    checkpoint: 0,
                    wait_hint_ms: 0,
                    win32_exit_code: 0,
                }
            }
            LifecycleEvent::StopRequested => {
                if self.state == SERVICE_STOP_PENDING {
                    self.checkpoint += 1;
                } else {
                    self.state = SERVICE_STOP_PENDING;
                    self.checkpoint = 1;
                }
                StatusReport {
                    current_state: SERVICE_STOP_PENDING,
                    controls_accepted: 0,
                    checkpoint: self.checkpoint,
                    wait_hint_ms: STOP_WAIT_HINT_MS,
                    win32_exit_code: 0,
                }
            }
            LifecycleEvent::Stopped { win32_exit_code } => {
                self.state = SERVICE_STOPPED;
                self.checkpoint = 0;
                StatusReport {
                    current_state: SERVICE_STOPPED,
                    controls_accepted: 0,
                    checkpoint: 0,
                    wait_hint_ms: 0,
                    win32_exit_code,
                }
            }
        }
    }
}

/// Whether a control code should begin a drain-and-stop. STOP and SHUTDOWN both
/// stop us; everything else (INTERROGATE, pause/continue we never accept) does
/// not. Pure so the handler's dispatch is testable.
pub fn is_stop_control(control: u32) -> bool {
    control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN
}

/// Human label for a `dwCurrentState`, for `service status` output.
pub fn state_label(state: u32) -> &'static str {
    match state {
        SERVICE_STOPPED => "STOPPED",
        SERVICE_START_PENDING => "START_PENDING",
        SERVICE_STOP_PENDING => "STOP_PENDING",
        SERVICE_RUNNING => "RUNNING",
        5 => "CONTINUE_PENDING",
        6 => "PAUSE_PENDING",
        7 => "PAUSED",
        _ => "UNKNOWN",
    }
}

// ===========================================================================
// FFI shell (Windows only). Thin: marshal, call the SCM, map errors.
// ===========================================================================
#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    type DWORD = u32;
    type BOOL = i32;
    type SC_HANDLE = *mut c_void;
    type SERVICE_STATUS_HANDLE = *mut c_void;

    // --- Access-rights / config constants -------------------------------------
    const SC_MANAGER_CONNECT: DWORD = 0x0001;
    const SC_MANAGER_CREATE_SERVICE: DWORD = 0x0002;
    const SERVICE_QUERY_STATUS: DWORD = 0x0004;
    const SERVICE_START: DWORD = 0x0010;
    const SERVICE_STOP: DWORD = 0x0020;
    const SERVICE_CHANGE_CONFIG: DWORD = 0x0002;
    const DELETE: DWORD = 0x0001_0000;

    const SERVICE_WIN32_OWN_PROCESS: DWORD = 0x0000_0010;
    const SERVICE_AUTO_START: DWORD = 0x0000_0002;
    const SERVICE_ERROR_NORMAL: DWORD = 0x0000_0001;

    const SERVICE_CONFIG_FAILURE_ACTIONS: DWORD = 2;
    const SC_ACTION_RESTART: DWORD = 1;
    const SC_STATUS_PROCESS_INFO: DWORD = 0;

    // --- GetLastError values we branch on -------------------------------------
    const ERROR_ACCESS_DENIED: DWORD = 5;
    const ERROR_SERVICE_ALREADY_RUNNING: DWORD = 1056;
    const ERROR_SERVICE_DOES_NOT_EXIST: DWORD = 1060;
    const ERROR_SERVICE_EXISTS: DWORD = 1073;
    const ERROR_SERVICE_MARKED_FOR_DELETE: DWORD = 1072;
    const ERROR_SERVICE_NOT_ACTIVE: DWORD = 1062;
    const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: DWORD = 1063;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SERVICE_STATUS {
        dwServiceType: DWORD,
        dwCurrentState: DWORD,
        dwControlsAccepted: DWORD,
        dwWin32ExitCode: DWORD,
        dwServiceSpecificExitCode: DWORD,
        dwCheckPoint: DWORD,
        dwWaitHint: DWORD,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SERVICE_STATUS_PROCESS {
        dwServiceType: DWORD,
        dwCurrentState: DWORD,
        dwControlsAccepted: DWORD,
        dwWin32ExitCode: DWORD,
        dwServiceSpecificExitCode: DWORD,
        dwCheckPoint: DWORD,
        dwWaitHint: DWORD,
        dwProcessId: DWORD,
        dwServiceFlags: DWORD,
    }

    #[repr(C)]
    struct SERVICE_TABLE_ENTRYW {
        lpServiceName: *mut u16,
        lpServiceProc: Option<unsafe extern "system" fn(DWORD, *mut *mut u16)>,
    }

    #[repr(C)]
    struct SC_ACTION {
        Type: DWORD,
        Delay: DWORD,
    }

    #[repr(C)]
    struct SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: DWORD,
        lpRebootMsg: *mut u16,
        lpCommand: *mut u16,
        cActions: DWORD,
        lpsaActions: *mut SC_ACTION,
    }

    type LPHANDLER_FUNCTION_EX =
        unsafe extern "system" fn(DWORD, DWORD, *mut c_void, *mut c_void) -> DWORD;

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenSCManagerW(
            lpMachineName: *const u16,
            lpDatabaseName: *const u16,
            dwDesiredAccess: DWORD,
        ) -> SC_HANDLE;
        fn OpenServiceW(
            hSCManager: SC_HANDLE,
            lpServiceName: *const u16,
            dwDesiredAccess: DWORD,
        ) -> SC_HANDLE;
        #[allow(clippy::too_many_arguments)]
        fn CreateServiceW(
            hSCManager: SC_HANDLE,
            lpServiceName: *const u16,
            lpDisplayName: *const u16,
            dwDesiredAccess: DWORD,
            dwServiceType: DWORD,
            dwStartType: DWORD,
            dwErrorControl: DWORD,
            lpBinaryPathName: *const u16,
            lpLoadOrderGroup: *const u16,
            lpdwTagId: *mut DWORD,
            lpDependencies: *const u16,
            lpServiceStartName: *const u16,
            lpPassword: *const u16,
        ) -> SC_HANDLE;
        fn DeleteService(hService: SC_HANDLE) -> BOOL;
        fn ControlService(
            hService: SC_HANDLE,
            dwControl: DWORD,
            lpServiceStatus: *mut SERVICE_STATUS,
        ) -> BOOL;
        fn StartServiceW(
            hService: SC_HANDLE,
            dwNumServiceArgs: DWORD,
            lpServiceArgVectors: *const *const u16,
        ) -> BOOL;
        fn ChangeServiceConfig2W(
            hService: SC_HANDLE,
            dwInfoLevel: DWORD,
            lpInfo: *mut c_void,
        ) -> BOOL;
        fn QueryServiceStatusEx(
            hService: SC_HANDLE,
            InfoLevel: DWORD,
            lpBuffer: *mut u8,
            cbBufSize: DWORD,
            pcbBytesNeeded: *mut DWORD,
        ) -> BOOL;
        fn CloseServiceHandle(hSCObject: SC_HANDLE) -> BOOL;
        fn StartServiceCtrlDispatcherW(lpServiceStartTable: *const SERVICE_TABLE_ENTRYW) -> BOOL;
        fn RegisterServiceCtrlHandlerExW(
            lpServiceName: *const u16,
            lpHandlerProc: LPHANDLER_FUNCTION_EX,
            lpContext: *mut c_void,
        ) -> SERVICE_STATUS_HANDLE;
        fn SetServiceStatus(
            hServiceStatus: SERVICE_STATUS_HANDLE,
            lpServiceStatus: *mut SERVICE_STATUS,
        ) -> BOOL;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetLastError() -> DWORD;
    }

    /// UTF-16 NUL-terminated encoding of `s`.
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// RAII closer for an SCM/service handle.
    struct ScHandle(SC_HANDLE);
    impl Drop for ScHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: handle from Open/CreateServiceW / OpenSCManagerW, closed once.
                unsafe {
                    CloseServiceHandle(self.0);
                }
            }
        }
    }

    // --- install / uninstall / status outcomes --------------------------------

    /// Result of [`install`].
    #[derive(Debug, PartialEq, Eq)]
    pub enum InstallOutcome {
        Created,
        AlreadyExists,
        /// SCM refused for lack of elevation (ERROR_ACCESS_DENIED).
        AccessDenied,
    }

    /// Result of [`uninstall`].
    #[derive(Debug, PartialEq, Eq)]
    pub enum UninstallOutcome {
        Deleted,
        NotInstalled,
        AccessDenied,
    }

    /// Snapshot returned by [`query_status`].
    #[derive(Debug, Clone, Copy)]
    pub struct StatusSnapshot {
        pub current_state: u32,
        pub pid: u32,
        pub win32_exit_code: u32,
    }

    /// Result of [`query_status`].
    #[derive(Debug)]
    pub enum QueryOutcome {
        Status(StatusSnapshot),
        NotInstalled,
        AccessDenied,
    }

    /// Result of [`run_service`] — how the SCM dispatcher connection went.
    #[derive(Debug, PartialEq, Eq)]
    pub enum RunOutcome {
        /// Dispatcher ran and returned after the service stopped.
        Completed,
        /// Not launched by the SCM (console run): ERROR_FAILED_SERVICE_CONTROLLER_CONNECT.
        NotUnderScm,
    }

    /// The command line the service runs: `"<current exe>" service run`.
    fn service_binary_command() -> std::io::Result<String> {
        let exe = std::env::current_exe()?;
        Ok(format!("\"{}\" service run", exe.display()))
    }

    /// Install the service: auto-start, runs `service run`, with failure actions
    /// set to restart after 5 s for the first 3 failures, reset window 1 day.
    /// Needs elevation — a standard-user run returns [`InstallOutcome::AccessDenied`].
    pub fn install(service_name: &str, display_name: &str) -> anyhow::Result<InstallOutcome> {
        let bin = service_binary_command()?;
        // SAFETY: NULL machine/db = local active SCM; create-service access.
        let scm = unsafe {
            OpenSCManagerW(
                std::ptr::null(),
                std::ptr::null(),
                SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE,
            )
        };
        if scm.is_null() {
            let e = last_error();
            if e == ERROR_ACCESS_DENIED {
                return Ok(InstallOutcome::AccessDenied);
            }
            anyhow::bail!("OpenSCManagerW failed (error {e})");
        }
        let scm = ScHandle(scm);

        let wname = to_wide(service_name);
        let wdisp = to_wide(display_name);
        let wbin = to_wide(&bin);
        // SAFETY: all wide strings NUL-terminated and outlive the call; NULL for
        // the optional load-order/tag/deps/account/password (LocalSystem).
        let svc = unsafe {
            CreateServiceW(
                scm.0,
                wname.as_ptr(),
                wdisp.as_ptr(),
                SERVICE_CHANGE_CONFIG | SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                wbin.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(), // LocalSystem
                std::ptr::null(),
            )
        };
        if svc.is_null() {
            let e = last_error();
            return match e {
                ERROR_ACCESS_DENIED => Ok(InstallOutcome::AccessDenied),
                ERROR_SERVICE_EXISTS => Ok(InstallOutcome::AlreadyExists),
                ERROR_SERVICE_MARKED_FOR_DELETE => {
                    anyhow::bail!("service is marked for deletion; retry after it fully stops")
                }
                _ => anyhow::bail!("CreateServiceW failed (error {e})"),
            };
        }
        let svc = ScHandle(svc);

        // Crash-restart: restart after 5 s, 3 attempts, reset window 1 day.
        set_failure_actions(svc.0)?;
        Ok(InstallOutcome::Created)
    }

    /// Configure SERVICE_CONFIG_FAILURE_ACTIONS = restart after 5 s for the first
    /// three failures, then stop retrying; reset the failure counter after 1 day
    /// of health (tech-stack §13.2 crash-restart).
    fn set_failure_actions(svc: SC_HANDLE) -> anyhow::Result<()> {
        let mut actions = [
            SC_ACTION {
                Type: SC_ACTION_RESTART,
                Delay: 5_000,
            },
            SC_ACTION {
                Type: SC_ACTION_RESTART,
                Delay: 5_000,
            },
            SC_ACTION {
                Type: SC_ACTION_RESTART,
                Delay: 5_000,
            },
        ];
        let mut fa = SERVICE_FAILURE_ACTIONSW {
            dwResetPeriod: 86_400, // seconds = 1 day
            lpRebootMsg: std::ptr::null_mut(),
            lpCommand: std::ptr::null_mut(),
            cActions: actions.len() as DWORD,
            lpsaActions: actions.as_mut_ptr(),
        };
        // SAFETY: `fa` + `actions` outlive the call; info level matches the struct.
        let ok = unsafe {
            ChangeServiceConfig2W(
                svc,
                SERVICE_CONFIG_FAILURE_ACTIONS,
                &mut fa as *mut _ as *mut c_void,
            )
        };
        if ok == 0 {
            anyhow::bail!(
                "ChangeServiceConfig2W(FAILURE_ACTIONS) failed (error {})",
                last_error()
            );
        }
        Ok(())
    }

    /// Stop (best-effort) then delete the service. Needs elevation.
    pub fn uninstall(service_name: &str) -> anyhow::Result<UninstallOutcome> {
        // SAFETY: NULL machine/db = local active SCM; connect only.
        let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if scm.is_null() {
            let e = last_error();
            if e == ERROR_ACCESS_DENIED {
                return Ok(UninstallOutcome::AccessDenied);
            }
            anyhow::bail!("OpenSCManagerW failed (error {e})");
        }
        let scm = ScHandle(scm);

        let wname = to_wide(service_name);
        // SAFETY: wname NUL-terminated; stop+delete access.
        let svc = unsafe { OpenServiceW(scm.0, wname.as_ptr(), SERVICE_STOP | DELETE) };
        if svc.is_null() {
            let e = last_error();
            return match e {
                ERROR_SERVICE_DOES_NOT_EXIST => Ok(UninstallOutcome::NotInstalled),
                ERROR_ACCESS_DENIED => Ok(UninstallOutcome::AccessDenied),
                _ => anyhow::bail!("OpenServiceW failed (error {e})"),
            };
        }
        let svc = ScHandle(svc);

        // Best-effort stop; ignore "not active".
        let mut status = SERVICE_STATUS::default();
        // SAFETY: valid service handle + out-param.
        let stopped = unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut status) };
        if stopped == 0 {
            let e = last_error();
            if e != ERROR_SERVICE_NOT_ACTIVE {
                tracing::warn!("ControlService(STOP) during uninstall returned error {e}");
            }
        }

        // SAFETY: valid service handle opened with DELETE.
        let deleted = unsafe { DeleteService(svc.0) };
        if deleted == 0 {
            let e = last_error();
            if e == ERROR_ACCESS_DENIED {
                return Ok(UninstallOutcome::AccessDenied);
            }
            anyhow::bail!("DeleteService failed (error {e})");
        }
        Ok(UninstallOutcome::Deleted)
    }

    /// Query the current status of the service.
    pub fn query_status(service_name: &str) -> anyhow::Result<QueryOutcome> {
        // SAFETY: NULL machine/db = local active SCM; connect only.
        let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if scm.is_null() {
            let e = last_error();
            if e == ERROR_ACCESS_DENIED {
                return Ok(QueryOutcome::AccessDenied);
            }
            anyhow::bail!("OpenSCManagerW failed (error {e})");
        }
        let scm = ScHandle(scm);

        let wname = to_wide(service_name);
        // SAFETY: wname NUL-terminated; query-status access.
        let svc = unsafe { OpenServiceW(scm.0, wname.as_ptr(), SERVICE_QUERY_STATUS) };
        if svc.is_null() {
            let e = last_error();
            return match e {
                ERROR_SERVICE_DOES_NOT_EXIST => Ok(QueryOutcome::NotInstalled),
                ERROR_ACCESS_DENIED => Ok(QueryOutcome::AccessDenied),
                _ => anyhow::bail!("OpenServiceW failed (error {e})"),
            };
        }
        let svc = ScHandle(svc);

        let mut buf = [0u8; std::mem::size_of::<SERVICE_STATUS_PROCESS>()];
        let mut needed: DWORD = 0;
        // SAFETY: buf sized to SERVICE_STATUS_PROCESS; out-param live.
        let ok = unsafe {
            QueryServiceStatusEx(
                svc.0,
                SC_STATUS_PROCESS_INFO,
                buf.as_mut_ptr(),
                buf.len() as DWORD,
                &mut needed,
            )
        };
        if ok == 0 {
            anyhow::bail!("QueryServiceStatusEx failed (error {})", last_error());
        }
        // SAFETY: buffer was filled with a SERVICE_STATUS_PROCESS.
        let sp = unsafe { &*(buf.as_ptr() as *const SERVICE_STATUS_PROCESS) };
        Ok(QueryOutcome::Status(StatusSnapshot {
            current_state: sp.dwCurrentState,
            pid: sp.dwProcessId,
            win32_exit_code: sp.dwWin32ExitCode,
        }))
    }

    // --- The SCM service entry point ------------------------------------------
    //
    // The SCM invokes ServiceMain / the control handler as C callbacks that
    // cannot capture, so the shared bits live in process globals set up before
    // the dispatcher starts:
    //   * WORKLOAD    — the fn the service body runs (blocks until stop flips).
    //   * STOP_FLAG   — flipped by the handler on STOP/SHUTDOWN, watched by the
    //                   workload and by ServiceMain.
    //   * STATE       — the pure state machine, so handler+main report coherent
    //                   checkpoints.
    //   * STATUS_HANDLE — the SERVICE_STATUS_HANDLE (as usize) both callbacks
    //                     write status through.

    /// The service body: runs until the passed stop flag flips (or returns/errs).
    pub type Workload = fn(Arc<AtomicBool>) -> anyhow::Result<()>;

    static WORKLOAD: OnceLock<Workload> = OnceLock::new();
    static STOP_FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    static STATE: OnceLock<Mutex<ServiceStateMachine>> = OnceLock::new();
    static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);
    static SERVICE_NAME_W: OnceLock<Vec<u16>> = OnceLock::new();

    fn report(status: StatusReport) {
        let h = STATUS_HANDLE.load(Ordering::SeqCst) as SERVICE_STATUS_HANDLE;
        if h.is_null() {
            return;
        }
        let mut s = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: status.current_state,
            dwControlsAccepted: status.controls_accepted,
            dwWin32ExitCode: status.win32_exit_code,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: status.checkpoint,
            dwWaitHint: status.wait_hint_ms,
        };
        // SAFETY: `h` came from RegisterServiceCtrlHandlerExW; `s` is a valid
        // SERVICE_STATUS for the duration of the call.
        unsafe {
            SetServiceStatus(h, &mut s);
        }
    }

    /// The control handler (HandlerEx). Runs on an SCM thread.
    unsafe extern "system" fn handler(
        control: DWORD,
        _event_type: DWORD,
        _event_data: *mut c_void,
        _context: *mut c_void,
    ) -> DWORD {
        if is_stop_control(control) {
            if let Some(sm) = STATE.get() {
                if let Ok(mut sm) = sm.lock() {
                    report(sm.advance(LifecycleEvent::StopRequested));
                }
            }
            if let Some(flag) = STOP_FLAG.get() {
                flag.store(true, Ordering::SeqCst);
            }
        } else if control == SERVICE_CONTROL_INTERROGATE {
            // Re-report the current state without advancing.
            if let Some(sm) = STATE.get() {
                if let Ok(sm) = sm.lock() {
                    let state = sm.state();
                    report(StatusReport {
                        current_state: state,
                        controls_accepted: if state == SERVICE_RUNNING {
                            ACCEPTED_CONTROLS
                        } else {
                            0
                        },
                        checkpoint: 0,
                        wait_hint_ms: 0,
                        win32_exit_code: 0,
                    });
                }
            }
        }
        0 // NO_ERROR
    }

    /// ServiceMain: registers the handler, drives START_PENDING → RUNNING, runs
    /// the workload until stop, then reports STOPPED.
    unsafe extern "system" fn service_main(_argc: DWORD, _argv: *mut *mut u16) {
        let name = match SERVICE_NAME_W.get() {
            Some(n) => n,
            None => return,
        };
        // SAFETY: name NUL-terminated; handler is a valid HandlerEx.
        let h = RegisterServiceCtrlHandlerExW(name.as_ptr(), handler, std::ptr::null_mut());
        if h.is_null() {
            return;
        }
        STATUS_HANDLE.store(h as usize, Ordering::SeqCst);

        // START_PENDING → RUNNING.
        if let Some(sm) = STATE.get() {
            if let Ok(mut sm) = sm.lock() {
                report(sm.advance(LifecycleEvent::StartPending));
            }
        }
        let stop = STOP_FLAG
            .get_or_init(|| Arc::new(AtomicBool::new(false)))
            .clone();
        if let Some(sm) = STATE.get() {
            if let Ok(mut sm) = sm.lock() {
                report(sm.advance(LifecycleEvent::Running));
            }
        }

        // Run the hosted workload; it blocks until `stop` flips (set by the
        // handler) or it returns/errs on its own.
        let exit_code = match WORKLOAD.get() {
            Some(w) => match w(stop) {
                Ok(()) => 0u32,
                Err(e) => {
                    tracing::error!("service workload exited with error: {e}");
                    1u32
                }
            },
            None => 1u32,
        };

        if let Some(sm) = STATE.get() {
            if let Ok(mut sm) = sm.lock() {
                report(sm.advance(LifecycleEvent::Stopped {
                    win32_exit_code: exit_code,
                }));
            }
        }
    }

    /// Connect this process to the SCM as `service_name` and run `workload` as the
    /// service body. Returns [`RunOutcome::NotUnderScm`] when launched from a
    /// console (the SCM connect fails with ERROR_FAILED_SERVICE_CONTROLLER_CONNECT).
    pub fn run_service(service_name: &str, workload: Workload) -> anyhow::Result<RunOutcome> {
        let _ = WORKLOAD.set(workload);
        let _ = STATE.set(Mutex::new(ServiceStateMachine::new()));
        let name_w = to_wide(service_name);
        let _ = SERVICE_NAME_W.set(name_w);

        let name_ptr = SERVICE_NAME_W.get().unwrap().as_ptr() as *mut u16;
        let table = [
            SERVICE_TABLE_ENTRYW {
                lpServiceName: name_ptr,
                lpServiceProc: Some(service_main),
            },
            SERVICE_TABLE_ENTRYW {
                lpServiceName: std::ptr::null_mut(),
                lpServiceProc: None,
            },
        ];

        // SAFETY: `table` is a valid, NULL-terminated SERVICE_TABLE_ENTRYW array
        // that outlives the (blocking) call; the name buffer is 'static.
        let ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
        if ok == 0 {
            let e = last_error();
            if e == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT {
                return Ok(RunOutcome::NotUnderScm);
            }
            anyhow::bail!("StartServiceCtrlDispatcherW failed (error {e})");
        }
        Ok(RunOutcome::Completed)
    }

    /// Attempt to start an installed service (used by tooling/tests; the SCM
    /// starts it automatically at boot once installed auto-start). Best-effort.
    #[allow(dead_code)]
    pub fn start(service_name: &str) -> anyhow::Result<bool> {
        // SAFETY: local SCM connect.
        let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if scm.is_null() {
            anyhow::bail!("OpenSCManagerW failed (error {})", last_error());
        }
        let scm = ScHandle(scm);
        let wname = to_wide(service_name);
        // SAFETY: wname NUL-terminated; start access.
        let svc =
            unsafe { OpenServiceW(scm.0, wname.as_ptr(), SERVICE_START | SERVICE_QUERY_STATUS) };
        if svc.is_null() {
            anyhow::bail!("OpenServiceW failed (error {})", last_error());
        }
        let svc = ScHandle(svc);
        // SAFETY: valid service handle; no start args.
        let ok = unsafe { StartServiceW(svc.0, 0, std::ptr::null()) };
        if ok == 0 {
            let e = last_error();
            if e == ERROR_SERVICE_ALREADY_RUNNING {
                return Ok(true);
            }
            anyhow::bail!("StartServiceW failed (error {e})");
        }
        Ok(true)
    }

    fn last_error() -> DWORD {
        // SAFETY: GetLastError has no preconditions.
        unsafe { GetLastError() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_sequence_reaches_running_with_accepted_controls() {
        let mut sm = ServiceStateMachine::new();
        assert_eq!(sm.state(), SERVICE_STOPPED);

        let sp = sm.advance(LifecycleEvent::StartPending);
        assert_eq!(sp.current_state, SERVICE_START_PENDING);
        assert_eq!(sp.checkpoint, 1);
        assert_eq!(
            sp.controls_accepted, 0,
            "must not accept controls while pending"
        );
        assert_eq!(sp.wait_hint_ms, START_WAIT_HINT_MS);

        let run = sm.advance(LifecycleEvent::Running);
        assert_eq!(run.current_state, SERVICE_RUNNING);
        assert_eq!(run.checkpoint, 0, "running resets checkpoint");
        assert_eq!(run.controls_accepted, ACCEPTED_CONTROLS);
        assert_eq!(run.wait_hint_ms, 0);
    }

    #[test]
    fn repeated_pending_bumps_checkpoint() {
        let mut sm = ServiceStateMachine::new();
        assert_eq!(sm.advance(LifecycleEvent::StartPending).checkpoint, 1);
        assert_eq!(sm.advance(LifecycleEvent::StartPending).checkpoint, 2);
        assert_eq!(sm.advance(LifecycleEvent::StartPending).checkpoint, 3);
        // Reaching RUNNING clears it, and a later stop restarts at 1.
        sm.advance(LifecycleEvent::Running);
        assert_eq!(sm.advance(LifecycleEvent::StopRequested).checkpoint, 1);
        assert_eq!(sm.advance(LifecycleEvent::StopRequested).checkpoint, 2);
    }

    #[test]
    fn stop_sequence_drains_then_stops() {
        let mut sm = ServiceStateMachine::new();
        sm.advance(LifecycleEvent::StartPending);
        sm.advance(LifecycleEvent::Running);

        let stop = sm.advance(LifecycleEvent::StopRequested);
        assert_eq!(stop.current_state, SERVICE_STOP_PENDING);
        assert_eq!(stop.checkpoint, 1);
        assert_eq!(stop.controls_accepted, 0);
        assert_eq!(stop.wait_hint_ms, STOP_WAIT_HINT_MS);

        let done = sm.advance(LifecycleEvent::Stopped { win32_exit_code: 0 });
        assert_eq!(done.current_state, SERVICE_STOPPED);
        assert_eq!(done.checkpoint, 0);
        assert_eq!(done.win32_exit_code, 0);
        assert_eq!(sm.state(), SERVICE_STOPPED);
    }

    #[test]
    fn stopped_carries_exit_code() {
        let mut sm = ServiceStateMachine::new();
        sm.advance(LifecycleEvent::StartPending);
        sm.advance(LifecycleEvent::Running);
        sm.advance(LifecycleEvent::StopRequested);
        let done = sm.advance(LifecycleEvent::Stopped {
            win32_exit_code: 42,
        });
        assert_eq!(done.win32_exit_code, 42);
    }

    #[test]
    fn stop_control_classification() {
        assert!(is_stop_control(SERVICE_CONTROL_STOP));
        assert!(is_stop_control(SERVICE_CONTROL_SHUTDOWN));
        assert!(!is_stop_control(SERVICE_CONTROL_INTERROGATE));
        assert!(!is_stop_control(0x99));
    }

    #[test]
    fn state_labels() {
        assert_eq!(state_label(SERVICE_RUNNING), "RUNNING");
        assert_eq!(state_label(SERVICE_STOPPED), "STOPPED");
        assert_eq!(state_label(SERVICE_START_PENDING), "START_PENDING");
        assert_eq!(state_label(SERVICE_STOP_PENDING), "STOP_PENDING");
        assert_eq!(state_label(12345), "UNKNOWN");
    }
}
