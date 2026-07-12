//! User-mode Windows collectors (tech-stack.md §4.1).
//!
//! First slice: whole-system process snapshots via a single
//! `NtQuerySystemInformation` call per tick, plus cheap system gauges.
//! ETW event collectors arrive at milestone M3 (docs/phases.md).

pub mod cadence;
pub mod ffi;
pub mod gauges;
pub mod sampler;
pub mod snapshot;

pub use cadence::{CadenceController, Tick};
pub use gauges::{cpu_times, memory_status, processor_count, CpuTimes, MemoryStatus};
pub use sampler::{ProcKey, ProcSample, SampleSet, Sampler, SystemSample};
pub use snapshot::{snapshot_processes, ProcessSnapshot};
