//! Whole-system process snapshot: one `NtQuerySystemInformation` syscall
//! returns every process with CPU times, memory, I/O totals, handles and
//! threads — the cheapest possible 1 Hz heartbeat (tech-stack.md §4.1).

use std::mem::size_of;

use anyhow::{bail, Result};

use crate::ffi::{
    NtQuerySystemInformation, STATUS_INFO_LENGTH_MISMATCH, SYSTEM_PROCESS_INFORMATION,
    SYSTEM_PROCESS_INFORMATION_CLASS,
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub parent_pid: u32,
    pub image_name: String,
    pub session_id: u32,
    /// FILETIME units (100 ns since 1601-01-01). Part of process identity:
    /// (pid, create_time) survives PID reuse.
    pub create_time_100ns: i64,
    /// Kernel + user time, 100 ns units, monotonically increasing.
    pub cpu_time_100ns: u64,
    pub cycle_time: u64,
    pub working_set: u64,
    pub private_working_set: u64,
    pub private_bytes: u64,
    pub virtual_size: u64,
    pub page_faults: u32,
    pub hard_faults: u32,
    pub handle_count: u32,
    pub thread_count: u32,
    pub base_priority: i32,
    pub read_bytes_total: u64,
    pub write_bytes_total: u64,
    pub other_bytes_total: u64,
}

const INITIAL_BUF_BYTES: usize = 1 << 20;
const GROW_SLACK_BYTES: usize = 256 << 10;
const MAX_ATTEMPTS: usize = 8;

pub fn snapshot_processes() -> Result<Vec<ProcessSnapshot>> {
    let mut buf: Vec<u8> = vec![0u8; INITIAL_BUF_BYTES];
    let mut used: usize = 0;
    let mut filled = false;

    for _ in 0..MAX_ATTEMPTS {
        let mut ret_len: u32 = 0;
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_INFORMATION_CLASS,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut ret_len,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            // The process list can grow between the size probe and the fill,
            // so always retry with slack on top of what the kernel asked for.
            let need = (ret_len as usize).max(buf.len()) + GROW_SLACK_BYTES;
            buf.resize(need, 0);
            continue;
        }
        if status < 0 {
            bail!("NtQuerySystemInformation failed: 0x{:08X}", status as u32);
        }
        used = (ret_len as usize).min(buf.len());
        filled = true;
        break;
    }
    if !filled {
        bail!("process list kept growing after {MAX_ATTEMPTS} attempts");
    }

    let base = buf.as_ptr() as usize;
    let mut out = Vec::with_capacity(512);
    let mut offset = 0usize;

    loop {
        if offset + size_of::<SYSTEM_PROCESS_INFORMATION>() > used {
            break;
        }
        // Copy out via read_unaligned: Vec<u8> only guarantees 1-byte
        // alignment, and entries are only as aligned as the kernel packed them.
        let rec: SYSTEM_PROCESS_INFORMATION =
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(offset).cast()) };

        let pid = rec.UniqueProcessId as usize as u32;

        let name_units = (rec.ImageName.Length / 2) as usize;
        let name_ptr = rec.ImageName.Buffer as usize;
        let mut image_name = String::new();
        // The name buffer points back into `buf`; trust it only after a
        // bounds check against the region the kernel actually filled.
        if name_units > 0 && name_ptr >= base && name_ptr + name_units * 2 <= base + used {
            let units = unsafe { std::slice::from_raw_parts(name_ptr as *const u16, name_units) };
            image_name = String::from_utf16_lossy(units);
        }
        if image_name.is_empty() {
            image_name = match pid {
                0 => "System Idle Process".to_string(),
                4 => "System".to_string(),
                _ => "<unknown>".to_string(),
            };
        }

        out.push(ProcessSnapshot {
            pid,
            parent_pid: rec.InheritedFromUniqueProcessId as usize as u32,
            image_name,
            session_id: rec.SessionId,
            create_time_100ns: rec.CreateTime,
            cpu_time_100ns: (rec.KernelTime as u64).saturating_add(rec.UserTime as u64),
            cycle_time: rec.CycleTime,
            working_set: rec.WorkingSetSize as u64,
            private_working_set: rec.WorkingSetPrivateSize.max(0) as u64,
            private_bytes: rec.PrivatePageCount as u64,
            virtual_size: rec.VirtualSize as u64,
            page_faults: rec.PageFaultCount,
            hard_faults: rec.HardFaultCount,
            handle_count: rec.HandleCount,
            thread_count: rec.NumberOfThreads,
            base_priority: rec.BasePriority,
            read_bytes_total: rec.ReadTransferCount.max(0) as u64,
            write_bytes_total: rec.WriteTransferCount.max(0) as u64,
            other_bytes_total: rec.OtherTransferCount.max(0) as u64,
        });

        if rec.NextEntryOffset == 0 {
            break;
        }
        offset += rec.NextEntryOffset as usize;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::UNICODE_STRING;
    use std::mem::offset_of;

    /// Locks the hand-written SYSTEM_PROCESS_INFORMATION layout to the
    /// documented 64-bit offsets (phnt/DDK). A failure here means the FFI
    /// definition drifted — never ship past this.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn system_process_information_layout() {
        assert_eq!(offset_of!(SYSTEM_PROCESS_INFORMATION, CycleTime), 0x18);
        assert_eq!(offset_of!(SYSTEM_PROCESS_INFORMATION, CreateTime), 0x20);
        assert_eq!(offset_of!(SYSTEM_PROCESS_INFORMATION, ImageName), 0x38);
        assert_eq!(
            offset_of!(SYSTEM_PROCESS_INFORMATION, UniqueProcessId),
            0x50
        );
        assert_eq!(offset_of!(SYSTEM_PROCESS_INFORMATION, HandleCount), 0x60);
        assert_eq!(
            offset_of!(SYSTEM_PROCESS_INFORMATION, PeakVirtualSize),
            0x70
        );
        assert_eq!(
            offset_of!(SYSTEM_PROCESS_INFORMATION, ReadOperationCount),
            0xD0
        );
        assert_eq!(size_of::<SYSTEM_PROCESS_INFORMATION>(), 0x100);
        assert_eq!(size_of::<UNICODE_STRING>(), 16);
    }

    #[test]
    fn snapshot_contains_current_process() {
        let procs = snapshot_processes().expect("snapshot should succeed unprivileged");
        assert!(procs.len() > 10, "expected a realistic process count");
        let me = procs
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("current process must be present");
        assert!(me.working_set > 0);
        assert!(me.thread_count > 0);
        assert!(!me.image_name.is_empty());
    }

    #[test]
    fn cpu_times_are_monotonic() {
        let a = snapshot_processes().unwrap();
        let b = snapshot_processes().unwrap();
        let me = std::process::id();
        let ta = a.iter().find(|p| p.pid == me).unwrap().cpu_time_100ns;
        let tb = b.iter().find(|p| p.pid == me).unwrap().cpu_time_100ns;
        assert!(tb >= ta);
    }
}
