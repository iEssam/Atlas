//! User-mode Windows collectors (tech-stack.md §4.1).
//!
//! Whole-system process snapshots via a single `NtQuerySystemInformation`
//! call per tick, plus cheap system gauges. The M3 slice adds event-driven
//! process start/stop collection via ETW (`events`, docs/phases.md).

#[cfg(windows)]
pub mod actions;
#[cfg(windows)]
pub mod boot;
pub mod cadence;
#[cfg(windows)]
pub mod changes;
#[cfg(windows)]
pub mod crashes;
pub mod deviceinfo;
#[cfg(windows)]
pub mod events;
pub mod ffi;
#[cfg(windows)]
pub mod gaming;
pub mod gauges;
pub mod gpu;
pub mod gpu_vendor;
pub mod grouping;
#[cfg(windows)]
pub mod handles;
#[cfg(windows)]
pub mod inspector;
#[cfg(windows)]
pub mod network;
#[cfg(windows)]
pub mod policy;
#[cfg(windows)]
pub mod power;
#[cfg(windows)]
pub mod privacy;
#[cfg(windows)]
pub mod reg;
#[cfg(windows)]
pub mod resources;
pub mod sampler;
#[cfg(windows)]
pub mod security_meta;
#[cfg(windows)]
pub mod services;
pub mod snapshot;
#[cfg(windows)]
pub mod startup;
#[cfg(windows)]
pub mod tasks;
#[cfg(windows)]
pub mod winver;

#[cfg(windows)]
pub use actions::{
    count_visible_top_level_windows, post_close_to_windows, resume_process, suspend_process,
    terminate_process, ActionOutcome,
};
#[cfg(windows)]
pub use boot::{analyze_boots, BootAnalysis, BootRecord};
pub use cadence::{CadenceController, Tick};
#[cfg(windows)]
pub use changes::{
    collect_inventory, diff_inventories, windows_update_history, AppEntry, DefaultAppEntry,
    DetectedChange, Inventory, StartupItem, SvcEntry, TaskItem,
};
#[cfg(windows)]
pub use crashes::{
    count_repeated_restarts, read_crashes, recent_change_notes, CrashScan, RawCrash,
};
pub use deviceinfo::{device_info, DeviceInfo};
#[cfg(windows)]
pub use events::{EventError, ProcessEvent, ProcessEventKind, ProcessEventWatcher, WatcherOptions};
#[cfg(windows)]
pub use gaming::{
    discover_games, primary_display, DiscoveredGame, DiscoveryCapability, GameDiscoveryReport,
    GamePlatform as DiscoveredGamePlatform, GameSupportLevel as DiscoveredGameSupportLevel,
    PrimaryDisplayReading, GAMING_ADAPTER_VERSION,
};
pub use gauges::{cpu_times, memory_status, processor_count, CpuTimes, MemoryStatus};
pub use gpu::{
    AdapterId, AdapterLuid, AvailabilityReason as GpuAvailabilityReason,
    EngineClass as GpuEngineClass, GpuAdapterSample, GpuCollector, GpuEngineSample,
    GpuProcessSample, GpuSnapshot, SensorAvailability as GpuSensorAvailability,
    SensorKind as GpuSensorKind, TelemetrySource as GpuTelemetrySource,
    TemperatureKind as GpuTemperatureKind, TemperatureSample as GpuTemperatureSample,
    ThrottleReason as GpuThrottleReason,
};
pub use grouping::{group_processes, image_family, GroupInput, GroupOutput, ProcessRole};
#[cfg(windows)]
pub use handles::{list_handles, HandleRow, HandlesResult};
#[cfg(windows)]
pub use inspector::{
    list_modules, list_threads, process_detail, ModuleInfo, ModulesResult, ProcessDetail,
    ProcessDetailResult, ThreadDetail,
};
#[cfg(windows)]
pub use network::{
    list_connections, list_listening_ports, Connection, L4Protocol as NetL4Protocol, ListeningPort,
    TcpState as NetTcpState,
};
#[cfg(windows)]
pub use policy::{
    cpu_topology, eco_is_on, foreground_pid, get_affinity, get_default_cpu_sets, get_eco_qos,
    get_power_overlay_state, get_priority_class, power_is_ac, priority_class_name, restore_eco_qos,
    restore_power_overlay_state, set_affinity_mask, set_default_cpu_sets, set_eco_qos,
    set_power_overlay, set_priority_class, AffinityView, CpuTopology, EcoState, PolicyOutcome,
};
#[cfg(windows)]
pub use power::{battery_status, thermal_status, BatteryReading, ThermalReading, ThermalSensor};
#[cfg(windows)]
pub use privacy::{
    diff_transitions, enumerate_privacy_usage, exe_basename, foreground_matches,
    unmunge_nonpackaged, Capability, PrivacyTransition, PrivacyUsage, PrivacyWatcher,
};
#[cfg(windows)]
pub use resources::{find_resource_owners, ResourceOwner, ResourceOwnersResult};
pub use sampler::{ProcKey, ProcSample, SampleSet, Sampler, SystemSample};
#[cfg(windows)]
pub use security_meta::{
    security_metadata, SecurityMetadata, SecurityMetadataResult, TokenPrivilegeInfo,
};
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
pub use tasks::{enumerate_tasks, ScheduledTask};
#[cfg(windows)]
pub use winver::{
    read_version_info, verify_signature, verify_signature_detail, verify_signature_info,
    CertDetail, FileVersionInfo, SignatureDetail, SignatureInfo, SignatureStatus,
};
