//! User-mode Windows collectors (tech-stack.md §4.1).
//!
//! Whole-system process snapshots via a single `NtQuerySystemInformation`
//! call per tick, plus cheap system gauges. The M3 slice adds event-driven
//! process start/stop collection via ETW (`events`, docs/phases.md).

pub mod cadence;
#[cfg(windows)]
pub mod events;
pub mod ffi;
pub mod gauges;
pub mod sampler;
pub mod snapshot;

pub use cadence::{CadenceController, Tick};
#[cfg(windows)]
pub use events::{EventError, ProcessEvent, ProcessEventKind, ProcessEventWatcher, WatcherOptions};
pub use gauges::{cpu_times, memory_status, processor_count, CpuTimes, MemoryStatus};
pub use sampler::{ProcKey, ProcSample, SampleSet, Sampler, SystemSample};
pub use snapshot::{snapshot_processes, ProcessSnapshot};
