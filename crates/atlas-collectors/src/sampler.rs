//! Stateful sampler: turns two consecutive snapshots into rates
//! (CPU share, bytes/s) keyed by (pid, create_time) so PID reuse can never
//! attribute one process's activity to another.

use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::gauges::{cpu_times, memory_status, processor_count, CpuTimes};
use crate::snapshot::snapshot_processes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcKey {
    pub pid: u32,
    pub create_time_100ns: i64,
}

#[derive(Debug, Clone)]
pub struct ProcSample {
    pub key: ProcKey,
    pub parent_pid: u32,
    pub image_name: String,
    pub session_id: u32,
    /// Share of total CPU capacity across all cores, 0..=1000.
    pub cpu_permille: u32,
    pub working_set: u64,
    pub private_working_set: u64,
    pub private_bytes: u64,
    pub read_bps: u64,
    pub write_bps: u64,
    pub handle_count: u32,
    pub thread_count: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct SystemSample {
    pub cpu_permille: u32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub commit_used: u64,
    pub commit_limit: u64,
    pub process_count: u32,
    pub thread_count: u32,
    pub handle_count: u32,
}

#[derive(Debug)]
pub struct SampleSet {
    pub ts_ms: i64,
    pub system: SystemSample,
    pub processes: Vec<ProcSample>,
    /// Processes seen now that were absent from the previous tick. Empty on
    /// the first sample (there is no previous tick to diff against).
    pub started: Vec<ProcKey>,
    /// Processes present in the previous tick but gone now.
    pub exited: Vec<ProcKey>,
}

struct PrevProc {
    cpu_100ns: u64,
    read_bytes: u64,
    write_bytes: u64,
}

pub struct Sampler {
    ncpu: u32,
    prev: HashMap<ProcKey, PrevProc>,
    prev_cpu: CpuTimes,
    prev_tick: Instant,
}

impl Sampler {
    pub fn new() -> Result<Self> {
        Ok(Self {
            ncpu: processor_count(),
            prev: HashMap::new(),
            prev_cpu: cpu_times()?,
            prev_tick: Instant::now(),
        })
    }

    pub fn processor_count(&self) -> u32 {
        self.ncpu
    }

    /// The first call after `new()` reports rates relative to construction
    /// time; unknown (newly seen) processes report zero rates for one tick.
    pub fn sample(&mut self) -> Result<SampleSet> {
        let procs = snapshot_processes()?;
        let now_cpu = cpu_times()?;
        let now = Instant::now();

        let wall_s = now.duration_since(self.prev_tick).as_secs_f64().max(1e-3);
        let capacity_100ns = wall_s * 1e7 * self.ncpu as f64;

        let mut next_prev = HashMap::with_capacity(procs.len());
        let mut out = Vec::with_capacity(procs.len());
        let mut thread_total = 0u32;
        let mut handle_total = 0u32;

        for p in &procs {
            let key = ProcKey {
                pid: p.pid,
                create_time_100ns: p.create_time_100ns,
            };
            thread_total = thread_total.saturating_add(p.thread_count);
            handle_total = handle_total.saturating_add(p.handle_count);

            let (cpu_permille, read_bps, write_bps) = match self.prev.get(&key) {
                Some(prev) => {
                    let dc = p.cpu_time_100ns.saturating_sub(prev.cpu_100ns);
                    let dr = p.read_bytes_total.saturating_sub(prev.read_bytes);
                    let dw = p.write_bytes_total.saturating_sub(prev.write_bytes);
                    (
                        (((dc as f64 / capacity_100ns) * 1000.0).round() as u32).min(1000),
                        (dr as f64 / wall_s) as u64,
                        (dw as f64 / wall_s) as u64,
                    )
                }
                None => (0, 0, 0),
            };

            next_prev.insert(
                key,
                PrevProc {
                    cpu_100ns: p.cpu_time_100ns,
                    read_bytes: p.read_bytes_total,
                    write_bytes: p.write_bytes_total,
                },
            );

            // The idle pseudo-process is accounted for in the system gauge,
            // not listed as a process.
            if p.pid == 0 {
                continue;
            }

            out.push(ProcSample {
                key,
                parent_pid: p.parent_pid,
                image_name: p.image_name.clone(),
                session_id: p.session_id,
                cpu_permille,
                working_set: p.working_set,
                private_working_set: p.private_working_set,
                private_bytes: p.private_bytes,
                read_bps,
                write_bps,
                handle_count: p.handle_count,
                thread_count: p.thread_count,
            });
        }

        // On the very first tick `prev` is empty, so treat nothing as started
        // (we have no baseline to diff against); otherwise a start is a key
        // present now but absent from the previous tick.
        let started: Vec<ProcKey> = if self.prev.is_empty() {
            Vec::new()
        } else {
            next_prev
                .keys()
                .filter(|k| !self.prev.contains_key(*k))
                .copied()
                .collect()
        };
        let exited: Vec<ProcKey> = self
            .prev
            .keys()
            .filter(|k| !next_prev.contains_key(*k))
            .copied()
            .collect();

        let d_total = now_cpu
            .total_100ns()
            .saturating_sub(self.prev_cpu.total_100ns());
        let d_idle = now_cpu.idle_100ns.saturating_sub(self.prev_cpu.idle_100ns);
        let sys_cpu_permille = if d_total == 0 {
            0
        } else {
            ((d_total.saturating_sub(d_idle) as f64 / d_total as f64) * 1000.0).round() as u32
        };

        let mem = memory_status()?;
        let system = SystemSample {
            cpu_permille: sys_cpu_permille.min(1000),
            mem_used: mem.used_phys(),
            mem_total: mem.total_phys,
            commit_used: mem.commit_used(),
            commit_limit: mem.commit_limit,
            process_count: out.len() as u32,
            thread_count: thread_total,
            handle_count: handle_total,
        };

        self.prev = next_prev;
        self.prev_cpu = now_cpu;
        self.prev_tick = now;

        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        Ok(SampleSet {
            ts_ms,
            system,
            processes: out,
            started,
            exited,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_ticks_produce_bounded_rates() {
        let mut s = Sampler::new().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let set = s.sample().unwrap();

        assert!(set.system.cpu_permille <= 1000);
        assert!(set.system.mem_total > 0);
        assert!(!set.processes.is_empty());

        let me = set
            .processes
            .iter()
            .find(|p| p.key.pid == std::process::id())
            .expect("current process present");
        assert!(me.cpu_permille <= 1000);
        assert!(me.working_set > 0);
    }
}
