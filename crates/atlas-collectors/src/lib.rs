//! User-mode Windows collectors (tech-stack.md §4.1).
//!
//! Whole-system process snapshots via a single `NtQuerySystemInformation`
//! call per tick, plus cheap system gauges. The M3 slice adds event-driven
//! process start/stop collection via ETW (`events`, docs/phases.md).

#[cfg(windows)]
pub mod actions;
pub mod cadence;
#[cfg(windows)]
pub mod events;
pub mod ffi;
pub mod gauges;
pub mod grouping;
pub mod sampler;
pub mod snapshot;

#[cfg(windows)]
pub use actions::{
    count_visible_top_level_windows, post_close_to_windows, resume_process, suspend_process,
    terminate_process, ActionOutcome,
};
pub use cadence::{CadenceController, Tick};
#[cfg(windows)]
pub use events::{EventError, ProcessEvent, ProcessEventKind, ProcessEventWatcher, WatcherOptions};
pub use gauges::{cpu_times, memory_status, processor_count, CpuTimes, MemoryStatus};
pub use grouping::{group_processes, image_family, GroupInput, GroupOutput, ProcessRole};
pub use sampler::{ProcKey, ProcSample, SampleSet, Sampler, SystemSample};
pub use snapshot::{snapshot_processes, ProcessSnapshot};
