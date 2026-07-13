//! Safe wrappers over the rules-engine action primitives (docs/phases.md Phase
//! 2, PRD §9.7, tech-stack §4.3). The raw FFI lives in [`crate::ffi`]; this
//! module keeps the `unsafe` blocks small and hands the rules engine a checked,
//! panic-free API for the reversible action set:
//!
//! * **priority class** — [`get_priority_class`] / [`set_priority_class`],
//! * **processor affinity** — [`get_affinity`] / [`set_affinity_mask`],
//! * **P/E-core steering** — [`cpu_topology`] + [`get_default_cpu_sets`] /
//!   [`set_default_cpu_sets`] (soft CPU-set preference, reversible),
//! * **EcoQoS** — [`get_eco_qos`] / [`set_eco_qos`] (ProcessPowerThrottling),
//!
//! plus the trigger inputs [`power_is_ac`] (AC/DC) and [`foreground_pid`]
//! (fullscreen/foreground app).
//!
//! Every apply opens the target with the *minimum* rights and returns a
//! [`PolicyOutcome`] rather than panicking. A cross-user or protected process
//! that denies `OpenProcess` yields `success == false` with a reason and the
//! caller degrades + skips it — the rules engine never escalates and never
//! crashes on an unreachable target. No REALTIME priority is reachable.

#![cfg(windows)]

use crate::ffi::{
    CloseHandle, GetForegroundWindow, GetLastError, GetPriorityClass, GetProcessAffinityMask,
    GetProcessDefaultCpuSets, GetProcessInformation, GetSystemCpuSetInformation,
    GetSystemPowerStatus, GetWindowThreadProcessId, OpenProcess, SetPriorityClass,
    SetProcessAffinityMask, SetProcessDefaultCpuSets, SetProcessInformation,
    ABOVE_NORMAL_PRIORITY_CLASS, AC_LINE_STATUS_ONLINE, BELOW_NORMAL_PRIORITY_CLASS, DWORD, HANDLE,
    HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    PROCESS_INFORMATION_CLASS_POWER_THROTTLING, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_SET_LIMITED_INFORMATION,
    REALTIME_PRIORITY_CLASS, SYSTEM_POWER_STATUS,
};

/// Opaque snapshot of a process's EcoQoS (ProcessPowerThrottling) state. The
/// rules engine stores one in its reversal ledger and hands it back verbatim to
/// [`restore_eco_qos`] — it never inspects the fields.
pub type EcoState = PROCESS_POWER_THROTTLING_STATE;

/// Result of a policy read/apply: whether it succeeded and a human-readable
/// note (for the audit log and dev output).
#[derive(Debug, Clone)]
pub struct PolicyOutcome {
    pub success: bool,
    pub message: String,
}

impl PolicyOutcome {
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

/// RAII guard closing an `OpenProcess` handle on drop (mirrors `actions.rs`).
struct ProcHandle(HANDLE);
impl Drop for ProcHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is a handle we opened and have not closed.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Opens `pid` with `access`, mapping NULL to a descriptive error including
/// `GetLastError` (access-denied on a cross-user/protected target lands here).
fn open_process(pid: u32, access: DWORD, verb: &str) -> Result<ProcHandle, PolicyOutcome> {
    // SAFETY: plain OpenProcess; NULL on failure is detected below.
    let h = unsafe { OpenProcess(access, 0, pid) };
    if h.is_null() {
        let err = unsafe { GetLastError() };
        Err(PolicyOutcome::fail(format!(
            "OpenProcess for {verb} failed (pid {pid}, error {err}; cross-user/protected targets \
             need elevation — skipped)"
        )))
    } else {
        Ok(ProcHandle(h))
    }
}

// ---------------------------------------------------------------------------
// Priority class
// ---------------------------------------------------------------------------

/// Reads `pid`'s Win32 priority-class value (0 if it cannot be read).
pub fn get_priority_class(pid: u32) -> Option<u32> {
    let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION, "priority read").ok()?;
    // SAFETY: `h.0` is a live handle with query rights.
    let v = unsafe { GetPriorityClass(h.0) };
    if v == 0 {
        None
    } else {
        Some(v)
    }
}

/// Sets `pid`'s priority class. REALTIME is refused defensively even though the
/// engine never requests it (belt and braces — PRD §9.7).
pub fn set_priority_class(pid: u32, class: u32) -> PolicyOutcome {
    if class == REALTIME_PRIORITY_CLASS {
        return PolicyOutcome::fail("REALTIME priority is not supported");
    }
    let h = match open_process(pid, PROCESS_SET_INFORMATION, "priority set") {
        Ok(h) => h,
        Err(o) => return o,
    };
    // SAFETY: `h.0` is a live handle with PROCESS_SET_INFORMATION.
    let ok = unsafe { SetPriorityClass(h.0, class) };
    if ok != 0 {
        PolicyOutcome::ok(format!(
            "priority set to {} (pid {pid})",
            priority_class_name(class)
        ))
    } else {
        let err = unsafe { GetLastError() };
        PolicyOutcome::fail(format!("SetPriorityClass failed (pid {pid}, error {err})"))
    }
}

/// Human name for a Win32 priority-class value (for simulation + audit).
pub fn priority_class_name(class: u32) -> &'static str {
    match class {
        IDLE_PRIORITY_CLASS => "Idle",
        BELOW_NORMAL_PRIORITY_CLASS => "Below Normal",
        NORMAL_PRIORITY_CLASS => "Normal",
        ABOVE_NORMAL_PRIORITY_CLASS => "Above Normal",
        HIGH_PRIORITY_CLASS => "High",
        REALTIME_PRIORITY_CLASS => "Realtime",
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// Processor affinity
// ---------------------------------------------------------------------------

/// One process's affinity view: its current process mask and the full system
/// mask (the set of logical processors available to it).
#[derive(Debug, Clone, Copy)]
pub struct AffinityView {
    pub process_mask: u64,
    pub system_mask: u64,
}

/// Reads `pid`'s process + system affinity masks.
pub fn get_affinity(pid: u32) -> Option<AffinityView> {
    let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION, "affinity read").ok()?;
    let mut process_mask: usize = 0;
    let mut system_mask: usize = 0;
    // SAFETY: `h.0` is a live handle; both out pointers are valid stack slots.
    let ok = unsafe { GetProcessAffinityMask(h.0, &mut process_mask, &mut system_mask) };
    if ok == 0 {
        return None;
    }
    Some(AffinityView {
        process_mask: process_mask as u64,
        system_mask: system_mask as u64,
    })
}

/// Sets `pid`'s processor affinity mask. An empty mask (0) is refused — Windows
/// rejects it and it would strand the process on no cores.
pub fn set_affinity_mask(pid: u32, mask: u64) -> PolicyOutcome {
    if mask == 0 {
        return PolicyOutcome::fail("refusing to set an empty affinity mask");
    }
    let h = match open_process(pid, PROCESS_SET_INFORMATION, "affinity set") {
        Ok(h) => h,
        Err(o) => return o,
    };
    // SAFETY: `h.0` is a live handle with PROCESS_SET_INFORMATION.
    let ok = unsafe { SetProcessAffinityMask(h.0, mask as usize) };
    if ok != 0 {
        PolicyOutcome::ok(format!("affinity set to 0x{mask:x} (pid {pid})"))
    } else {
        let err = unsafe { GetLastError() };
        PolicyOutcome::fail(format!(
            "SetProcessAffinityMask failed (pid {pid}, error {err})"
        ))
    }
}

// ---------------------------------------------------------------------------
// P/E-core steering via CPU sets
// ---------------------------------------------------------------------------

/// The machine's CPU topology as seen through `GetSystemCpuSetInformation`.
/// P-cores are the CPU sets with the highest `EfficiencyClass`, E-cores the
/// lowest; on a homogeneous machine the two classes coincide and `heterogeneous`
/// is false (P/E steering then degrades to a no-op — honestly reported).
#[derive(Debug, Clone, Default)]
pub struct CpuTopology {
    /// CPU-set ids of the performance (P) cores.
    pub p_core_ids: Vec<u32>,
    /// CPU-set ids of the efficiency (E) cores.
    pub e_core_ids: Vec<u32>,
    /// Logical-processor bitmask of the P cores (fallback affinity form).
    pub p_core_mask: u64,
    /// Logical-processor bitmask of the E cores.
    pub e_core_mask: u64,
    /// True when the machine actually has ≥2 distinct efficiency classes.
    pub heterogeneous: bool,
}

/// Enumerates the system CPU sets and classifies P vs E cores by their
/// `EfficiencyClass`. Returns an all-empty topology (non-heterogeneous) when the
/// API is unavailable or reports a single class.
pub fn cpu_topology() -> CpuTopology {
    let mut needed: u32 = 0;
    // First call: probe the required buffer length.
    // SAFETY: NULL buffer with 0 length is the documented size-probe form.
    unsafe {
        GetSystemCpuSetInformation(
            std::ptr::null_mut(),
            0,
            &mut needed,
            std::ptr::null_mut(),
            0,
        );
    }
    if needed == 0 {
        return CpuTopology::default();
    }
    let mut buf = vec![0u8; needed as usize];
    let mut returned: u32 = 0;
    // SAFETY: `buf` is `needed` bytes; the API fills up to `returned` bytes.
    let ok = unsafe {
        GetSystemCpuSetInformation(
            buf.as_mut_ptr(),
            needed,
            &mut returned,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 || returned == 0 {
        return CpuTopology::default();
    }

    // Walk the variable-stride entries by each record's `Size` field. Only the
    // Id (offset 8), LogicalProcessorIndex (offset 14) and EfficiencyClass
    // (offset 18) of the CpuSet arm are read, via unaligned loads so the byte
    // buffer needs no special alignment. Type (offset 4) 0 = CpuSet.
    struct Entry {
        id: u32,
        logical_index: u8,
        efficiency: u8,
    }
    let mut entries: Vec<Entry> = Vec::new();
    let bytes = &buf[..returned as usize];
    let mut off = 0usize;
    while off + 20 <= bytes.len() {
        // SAFETY: bounds checked by the `off + 20 <= len` guard; reads are within
        // the record and use unaligned loads.
        let size = u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
            as usize;
        if size < 20 || off + size > bytes.len() {
            break;
        }
        let ty = u32::from_le_bytes([
            bytes[off + 4],
            bytes[off + 5],
            bytes[off + 6],
            bytes[off + 7],
        ]);
        if ty == 0 {
            let id = u32::from_le_bytes([
                bytes[off + 8],
                bytes[off + 9],
                bytes[off + 10],
                bytes[off + 11],
            ]);
            let logical_index = bytes[off + 14];
            let efficiency = bytes[off + 18];
            entries.push(Entry {
                id,
                logical_index,
                efficiency,
            });
        }
        off += size;
    }

    if entries.is_empty() {
        return CpuTopology::default();
    }
    let max_eff = entries.iter().map(|e| e.efficiency).max().unwrap_or(0);
    let min_eff = entries.iter().map(|e| e.efficiency).min().unwrap_or(0);
    let heterogeneous = max_eff != min_eff;

    let mut topo = CpuTopology {
        heterogeneous,
        ..Default::default()
    };
    for e in &entries {
        let bit = if e.logical_index < 64 {
            1u64 << e.logical_index
        } else {
            0
        };
        // Higher EfficiencyClass == more performant (P core).
        if e.efficiency == max_eff {
            topo.p_core_ids.push(e.id);
            topo.p_core_mask |= bit;
        }
        if e.efficiency == min_eff {
            topo.e_core_ids.push(e.id);
            topo.e_core_mask |= bit;
        }
    }
    topo
}

/// Reads `pid`'s current default CPU-set assignment (empty = none assigned, the
/// system default). Used to capture the original for reversal.
pub fn get_default_cpu_sets(pid: u32) -> Option<Vec<u32>> {
    let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION, "cpuset read").ok()?;
    let mut required: u32 = 0;
    // SAFETY: size-probe with a NULL id buffer.
    unsafe {
        GetProcessDefaultCpuSets(h.0, std::ptr::null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Some(Vec::new());
    }
    let mut ids = vec![0u32; required as usize];
    let mut got: u32 = 0;
    // SAFETY: `ids` holds `required` u32 slots.
    let ok = unsafe { GetProcessDefaultCpuSets(h.0, ids.as_mut_ptr(), required, &mut got) };
    if ok == 0 {
        return None;
    }
    ids.truncate(got as usize);
    Some(ids)
}

/// Assigns `pid`'s default CPU sets (P/E steering). An empty slice clears the
/// assignment (restores the system default).
pub fn set_default_cpu_sets(pid: u32, ids: &[u32]) -> PolicyOutcome {
    let h = match open_process(pid, PROCESS_SET_LIMITED_INFORMATION, "cpuset set") {
        Ok(h) => h,
        Err(o) => return o,
    };
    let (ptr, count) = if ids.is_empty() {
        (std::ptr::null(), 0)
    } else {
        (ids.as_ptr(), ids.len() as u32)
    };
    // SAFETY: `h.0` is a live handle with PROCESS_SET_LIMITED_INFORMATION; `ptr`
    // covers `count` ids (or NULL/0 to clear).
    let ok = unsafe { SetProcessDefaultCpuSets(h.0, ptr, count) };
    if ok != 0 {
        if ids.is_empty() {
            PolicyOutcome::ok(format!("cpu sets cleared (pid {pid})"))
        } else {
            PolicyOutcome::ok(format!("cpu sets set ({} ids, pid {pid})", ids.len()))
        }
    } else {
        let err = unsafe { GetLastError() };
        PolicyOutcome::fail(format!(
            "SetProcessDefaultCpuSets failed (pid {pid}, error {err})"
        ))
    }
}

// ---------------------------------------------------------------------------
// EcoQoS (ProcessPowerThrottling)
// ---------------------------------------------------------------------------

/// Reads `pid`'s EcoQoS (execution-speed throttling) state, capturing the raw
/// control/state masks so reversal can write back the exact original. `None` if
/// the state cannot be read (older OS / access denied) — the caller then treats
/// the original as system-managed.
pub fn get_eco_qos(pid: u32) -> Option<EcoState> {
    let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION, "eco read").ok()?;
    let mut state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    let size = std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as DWORD;
    // SAFETY: `h.0` is a live handle; `state` is a valid, correctly-sized slot.
    let ok = unsafe {
        GetProcessInformation(
            h.0,
            PROCESS_INFORMATION_CLASS_POWER_THROTTLING,
            &mut state as *mut _ as *mut std::ffi::c_void,
            size,
        )
    };
    if ok == 0 {
        None
    } else {
        Some(state)
    }
}

/// Whether an EcoQoS state has execution-speed throttling explicitly enabled.
pub fn eco_is_on(state: &EcoState) -> bool {
    state.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
        && state.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
}

/// Applies an EcoQoS execution-speed request to `pid`:
/// * `Some(true)`  — enable EcoQoS (throttle),
/// * `Some(false)` — disable throttling (always run at full speed),
/// * `None`        — return the process to system-managed (the default).
pub fn set_eco_qos(pid: u32, mode: Option<bool>) -> PolicyOutcome {
    let h = match open_process(pid, PROCESS_SET_INFORMATION, "eco set") {
        Ok(h) => h,
        Err(o) => return o,
    };
    let state = match mode {
        Some(true) => PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        },
        Some(false) => PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        },
        None => PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: 0,
            StateMask: 0,
        },
    };
    let size = std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as DWORD;
    // SAFETY: `h.0` is a live handle with PROCESS_SET_INFORMATION; `state` is a
    // valid, correctly-sized input buffer.
    let ok = unsafe {
        SetProcessInformation(
            h.0,
            PROCESS_INFORMATION_CLASS_POWER_THROTTLING,
            &state as *const _ as *mut std::ffi::c_void,
            size,
        )
    };
    if ok != 0 {
        let what = match mode {
            Some(true) => "EcoQoS enabled",
            Some(false) => "EcoQoS disabled (full speed)",
            None => "EcoQoS reset to system-managed",
        };
        PolicyOutcome::ok(format!("{what} (pid {pid})"))
    } else {
        let err = unsafe { GetLastError() };
        PolicyOutcome::fail(format!(
            "SetProcessInformation(EcoQoS) failed (pid {pid}, error {err})"
        ))
    }
}

/// Restores `pid`'s EcoQoS to a captured original state verbatim (used by the
/// reversal ledger). Writing the exact masks back is the faithful undo.
pub fn restore_eco_qos(pid: u32, mut original: EcoState) -> PolicyOutcome {
    let h = match open_process(pid, PROCESS_SET_INFORMATION, "eco restore") {
        Ok(h) => h,
        Err(o) => return o,
    };
    original.Version = PROCESS_POWER_THROTTLING_CURRENT_VERSION;
    let size = std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as DWORD;
    // SAFETY: live handle + valid input buffer.
    let ok = unsafe {
        SetProcessInformation(
            h.0,
            PROCESS_INFORMATION_CLASS_POWER_THROTTLING,
            &original as *const _ as *mut std::ffi::c_void,
            size,
        )
    };
    if ok != 0 {
        PolicyOutcome::ok(format!("EcoQoS restored (pid {pid})"))
    } else {
        let err = unsafe { GetLastError() };
        PolicyOutcome::fail(format!(
            "SetProcessInformation(EcoQoS restore) failed (pid {pid}, error {err})"
        ))
    }
}

// ---------------------------------------------------------------------------
// Trigger inputs: power state + foreground pid
// ---------------------------------------------------------------------------

/// Whether the machine is on AC power (`Some(true)`), on battery (`Some(false)`),
/// or the state is unknown/desktop-with-no-battery (`None`). Desktops typically
/// report AC online, so `None` is rare; the caller maps it as it prefers.
pub fn power_is_ac() -> Option<bool> {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid, correctly-sized out slot.
    let ok = unsafe { GetSystemPowerStatus(&mut status) };
    if ok == 0 {
        return None;
    }
    match status.ACLineStatus {
        AC_LINE_STATUS_ONLINE => Some(true),
        0 => Some(false),
        _ => None,
    }
}

/// Applies a profile power mode by setting the active power overlay scheme
/// (PRD §9.7.4). Accepts `""`/`Balanced`, `PowerSaver`, `HighPerformance`;
/// unknown modes are a no-op success. The API is lightly documented and may be
/// unavailable on some SKUs — any non-zero return degrades to a failed outcome
/// (the caller logs it and continues; a profile is still a rule bundle).
pub fn set_power_overlay(mode: &str) -> PolicyOutcome {
    use crate::ffi::{
        FreeLibrary, GetProcAddress, LoadLibraryW, PowerSetActiveOverlaySchemeFn, OVERLAY_BALANCED,
        OVERLAY_HIGH_PERFORMANCE, OVERLAY_POWER_SAVER,
    };
    let guid = match mode {
        "" | "Balanced" => OVERLAY_BALANCED,
        "PowerSaver" => OVERLAY_POWER_SAVER,
        "HighPerformance" => OVERLAY_HIGH_PERFORMANCE,
        other => {
            return PolicyOutcome::ok(format!("no power overlay mapped for mode '{other}'"));
        }
    };
    // `PowerSetActiveOverlayScheme` is an undocumented powrprof export that is not
    // in the SDK import library, so resolve it at runtime and degrade cleanly if
    // it is unavailable (feature-flag, PRD §9.7.4).
    // SAFETY: standard LoadLibrary/GetProcAddress dance; the resolved pointer is
    // transmuted to the documented signature and only called while the module is
    // loaded, then the module is freed.
    unsafe {
        let dll: Vec<u16> = "powrprof.dll\0".encode_utf16().collect();
        let module = LoadLibraryW(dll.as_ptr());
        if module.is_null() {
            return PolicyOutcome::fail("powrprof.dll could not be loaded (power mode skipped)");
        }
        let addr = GetProcAddress(module, c"PowerSetActiveOverlayScheme".as_ptr() as *const u8);
        if addr.is_null() {
            FreeLibrary(module);
            return PolicyOutcome::fail(
                "PowerSetActiveOverlayScheme unavailable on this OS (power mode skipped)",
            );
        }
        let func: PowerSetActiveOverlaySchemeFn = std::mem::transmute(addr);
        let rc = func(&guid);
        FreeLibrary(module);
        if rc == 0 {
            PolicyOutcome::ok(format!("power overlay set to '{mode}'"))
        } else {
            PolicyOutcome::fail(format!(
                "PowerSetActiveOverlayScheme('{mode}') returned {rc} (degraded)"
            ))
        }
    }
}

/// The pid that owns the current foreground window (the ON_FULLSCREEN /
/// foreground-app trigger). 0 when there is no foreground window.
pub fn foreground_pid() -> u32 {
    // SAFETY: GetForegroundWindow returns a window handle (or NULL); the pid is
    // written out by GetWindowThreadProcessId which tolerates a NULL hwnd.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return 0;
        }
        let mut pid: DWORD = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reading our own priority class succeeds and names a known class.
    #[test]
    fn own_priority_is_readable_and_named() {
        let pid = std::process::id();
        let class = get_priority_class(pid).expect("own priority readable");
        // The name is one of the known set (test runners are Normal-ish).
        let name = priority_class_name(class);
        assert!(
            [
                "Idle",
                "Below Normal",
                "Normal",
                "Above Normal",
                "High",
                "Realtime"
            ]
            .contains(&name),
            "unexpected class name {name}"
        );
    }

    /// Reading our own affinity yields a non-empty process mask that is a subset
    /// of the system mask.
    #[test]
    fn own_affinity_is_readable() {
        let pid = std::process::id();
        let a = get_affinity(pid).expect("own affinity readable");
        assert_ne!(a.process_mask, 0);
        assert_eq!(a.process_mask & a.system_mask, a.process_mask);
    }

    /// The CPU topology is internally consistent: P/E masks are subsets of the
    /// system affinity, and a homogeneous machine reports `heterogeneous=false`.
    #[test]
    fn cpu_topology_is_consistent() {
        let topo = cpu_topology();
        let sys = get_affinity(std::process::id())
            .map(|a| a.system_mask)
            .unwrap_or(u64::MAX);
        assert_eq!(topo.p_core_mask & !sys, 0, "p mask within system affinity");
        assert_eq!(topo.e_core_mask & !sys, 0, "e mask within system affinity");
        if !topo.heterogeneous {
            // On a homogeneous machine both classes are the same set.
            assert_eq!(topo.p_core_mask, topo.e_core_mask);
        }
    }

    /// A bogus pid degrades cleanly (no panic) on every read + apply.
    #[test]
    fn bogus_pid_degrades_cleanly() {
        let bogus = 0xFFFF_FFF0;
        assert!(get_priority_class(bogus).is_none());
        assert!(get_affinity(bogus).is_none());
        assert!(!set_priority_class(bogus, NORMAL_PRIORITY_CLASS).success);
        assert!(!set_affinity_mask(bogus, 0x1).success);
        assert!(!set_eco_qos(bogus, Some(true)).success);
    }

    /// REALTIME is refused defensively.
    #[test]
    fn realtime_priority_is_refused() {
        let out = set_priority_class(std::process::id(), REALTIME_PRIORITY_CLASS);
        assert!(!out.success);
        assert!(out.message.contains("REALTIME"));
    }

    /// Power + foreground reads never panic (values are environment-dependent).
    #[test]
    fn power_and_foreground_do_not_panic() {
        let _ = power_is_ac();
        let _ = foreground_pid();
    }
}
