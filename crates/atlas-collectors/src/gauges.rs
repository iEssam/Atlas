//! System-wide gauges: CPU time accumulators and memory/commit status.

use anyhow::{bail, Result};

use crate::ffi::{
    GetActiveProcessorCount, GetSystemTimes, GlobalMemoryStatusEx, ALL_PROCESSOR_GROUPS, FILETIME,
    MEMORYSTATUSEX,
};

/// Cumulative CPU times in 100 ns units. `kernel` includes `idle`
/// (Windows semantics), so busy = (kernel - idle) + user.
#[derive(Debug, Clone, Copy)]
pub struct CpuTimes {
    pub idle_100ns: u64,
    pub kernel_100ns: u64,
    pub user_100ns: u64,
}

impl CpuTimes {
    pub fn total_100ns(&self) -> u64 {
        self.kernel_100ns.saturating_add(self.user_100ns)
    }
}

pub fn cpu_times() -> Result<CpuTimes> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    if ok == 0 {
        bail!("GetSystemTimes failed: {}", std::io::Error::last_os_error());
    }
    Ok(CpuTimes {
        idle_100ns: idle.as_u64(),
        kernel_100ns: kernel.as_u64(),
        user_100ns: user.as_u64(),
    })
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryStatus {
    pub load_pct: u32,
    pub total_phys: u64,
    pub avail_phys: u64,
    pub commit_limit: u64,
    pub commit_available: u64,
}

impl MemoryStatus {
    pub fn used_phys(&self) -> u64 {
        self.total_phys.saturating_sub(self.avail_phys)
    }

    pub fn commit_used(&self) -> u64 {
        self.commit_limit.saturating_sub(self.commit_available)
    }
}

pub fn memory_status() -> Result<MemoryStatus> {
    let mut m = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        dwMemoryLoad: 0,
        ullTotalPhys: 0,
        ullAvailPhys: 0,
        ullTotalPageFile: 0,
        ullAvailPageFile: 0,
        ullTotalVirtual: 0,
        ullAvailVirtual: 0,
        ullAvailExtendedVirtual: 0,
    };
    let ok = unsafe { GlobalMemoryStatusEx(&mut m) };
    if ok == 0 {
        bail!(
            "GlobalMemoryStatusEx failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(MemoryStatus {
        load_pct: m.dwMemoryLoad,
        total_phys: m.ullTotalPhys,
        avail_phys: m.ullAvailPhys,
        commit_limit: m.ullTotalPageFile,
        commit_available: m.ullAvailPageFile,
    })
}

pub fn processor_count() -> u32 {
    let n = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
    n.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauges_report_sane_values() {
        let mem = memory_status().unwrap();
        assert!(mem.total_phys > 0);
        assert!(mem.avail_phys <= mem.total_phys);
        assert!(mem.commit_limit > 0);
        assert!(processor_count() >= 1);
    }

    #[test]
    fn cpu_times_accumulate() {
        let a = cpu_times().unwrap();
        let b = cpu_times().unwrap();
        assert!(b.total_100ns() >= a.total_100ns());
        assert!(b.idle_100ns >= a.idle_100ns);
    }
}
