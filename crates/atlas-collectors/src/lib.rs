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
#[cfg(windows)]
pub mod handles;
#[cfg(windows)]
pub mod inspector;
#[cfg(windows)]
pub mod privacy;
#[cfg(windows)]
pub mod reg;
#[cfg(windows)]
pub mod resources;
pub mod sampler;
#[cfg(windows)]
pub mod services;
pub mod snapshot;
#[cfg(windows)]
pub mod startup;
#[cfg(windows)]
pub mod winver;

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
#[cfg(windows)]
pub use handles::{list_handles, HandleRow, HandlesResult};
#[cfg(windows)]
pub use inspector::{
    list_modules, list_threads, process_detail, ModuleInfo, ModulesResult, ProcessDetail,
    ProcessDetailResult, ThreadDetail,
};
#[cfg(windows)]
pub use privacy::{enumerate_privacy_usage, Capability, PrivacyUsage};
#[cfg(windows)]
pub use resources::{find_resource_owners, ResourceOwner, ResourceOwnersResult};
pub use sampler::{ProcKey, ProcSample, SampleSet, Sampler, SystemSample};
#[cfg(windows)]
pub use services::{
    enumerate_services, ServiceEntry, ServiceStartType, ServiceState as CollectorServiceState,
};
pub use snapshot::{snapshot_processes, snapshot_thread_infos, ProcessSnapshot, ThreadSample};
#[cfg(windows)]
pub use startup::{
    enumerate_startup, Scope as StartupScope, StartupEntry, StartupSource as CollectorStartupSource,
};
#[cfg(windows)]
pub use winver::{
    read_version_info, verify_signature, verify_signature_info, FileVersionInfo, SignatureInfo,
    SignatureStatus,
};
