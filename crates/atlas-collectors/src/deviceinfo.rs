//! Cheap device facts for the remote support bundle (docs/phases.md R3, PRD
//! §9.18): OS build, host name, CPU count + P/E topology, physical RAM, and
//! current uptime. Every value is a single stable-ABI Windows call or an
//! environment read — no snapshot syscall, no store access.
//!
//! The host name is returned raw; the support bundle passes it through the
//! shared redactor (`<HOST>`) before formatting, exactly like every other
//! textual field. Nothing here is machine-identifying beyond the host name.

/// A flat bundle of device facts. Numbers only, except `hostname` (redactable).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub os_major: u32,
    pub os_minor: u32,
    pub os_build: u32,
    /// `COMPUTERNAME` (redactable to `<HOST>`); empty when unknown.
    pub hostname: String,
    /// Active logical processors across all groups.
    pub logical_cpus: u32,
    /// Performance-core count (0 on a homogeneous machine).
    pub p_core_count: u32,
    /// Efficiency-core count (0 on a homogeneous machine).
    pub e_core_count: u32,
    /// True when the machine has ≥2 distinct efficiency classes (P/E split).
    pub heterogeneous: bool,
    /// Total physical memory, bytes.
    pub ram_total_bytes: u64,
    /// Milliseconds since boot.
    pub uptime_ms: u64,
}

/// Reads the current device facts. Best-effort: any individual probe that fails
/// leaves its field at the default (0 / empty), never panics.
#[cfg(windows)]
pub fn device_info() -> DeviceInfo {
    use crate::ffi::{RtlGetVersion, RTL_OSVERSIONINFOW};
    use crate::gauges::{memory_status, processor_count};
    use crate::policy::cpu_topology;

    let (os_major, os_minor, os_build) = os_version();
    let topo = cpu_topology();
    let ram_total_bytes = memory_status().map(|m| m.total_phys).unwrap_or(0);
    let uptime_ms = unsafe { crate::ffi::GetTickCount64() };

    return DeviceInfo {
        os_major,
        os_minor,
        os_build,
        hostname: std::env::var("COMPUTERNAME").unwrap_or_default(),
        logical_cpus: processor_count(),
        p_core_count: topo.p_core_ids.len() as u32,
        e_core_count: topo.e_core_ids.len() as u32,
        heterogeneous: topo.heterogeneous,
        ram_total_bytes,
        uptime_ms,
    };

    /// (major, minor, build) via `RtlGetVersion`; zeros if the call fails.
    fn os_version() -> (u32, u32, u32) {
        let mut info = RTL_OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<RTL_OSVERSIONINFOW>() as u32,
            dwMajorVersion: 0,
            dwMinorVersion: 0,
            dwBuildNumber: 0,
            dwPlatformId: 0,
            szCSDVersion: [0u16; 128],
        };
        // SAFETY: `info` is a correctly-sized, initialized RTL_OSVERSIONINFOW;
        // RtlGetVersion only writes into it.
        let status = unsafe { RtlGetVersion(&mut info) };
        if status != 0 {
            return (0, 0, 0);
        }
        (info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber)
    }
}

/// Non-Windows stub: the support bundle is a Windows product; other targets get
/// an all-default record so the crate still builds.
#[cfg(not(windows))]
pub fn device_info() -> DeviceInfo {
    DeviceInfo {
        hostname: std::env::var("COMPUTERNAME").unwrap_or_default(),
        ..DeviceInfo::default()
    }
}
