//! System Atlas IPC layer (tech-stack.md §5, docs/phases.md M4).
//!
//! Compiles the `proto/atlas.proto` contract (package `atlas.v0`) into Rust
//! with tonic/prost (see `build.rs`) and provides the Windows named-pipe
//! transport that carries it — no TCP ports are ever opened (§5). The generated
//! types plus tonic server/client stubs are re-exported under [`v0`].
//!
//! # Security
//! On Windows the server pipe is created with a DACL granting full access to
//! SYSTEM and Administrators and read/write/connect to the current user's SID
//! only (full SDDL wiring in [`security`]). The pipe ACL is the actual
//! unprivileged→privileged boundary (tech-stack §4.5); per-connection client
//! authentication (client PID / signature) is future work.

/// Generated protobuf/gRPC types for package `atlas.v0`.
pub mod v0 {
    tonic::include_proto!("atlas.v0");
}

#[cfg(windows)]
pub mod security;
#[cfg(windows)]
pub mod shm;
#[cfg(windows)]
pub mod transport;

#[cfg(windows)]
pub use shm::{
    section_name, RingReader, RingRow, RingSnapshot, RingUpdate, RingWriter, RowInput, RowSnapshot,
    LAYOUT_VERSION, RING_MAGIC, RING_NAME_LEN, RING_ROWS, RING_SIZE,
};
#[cfg(windows)]
pub use transport::{connect, default_pipe_name, pipe_name, serve, PipeConnectInfo};

// Convenience re-exports so downstream crates depend on the contract through
// atlas-ipc rather than pinning tonic/prost versions themselves.
pub use v0::atlas_control_client::AtlasControlClient;
pub use v0::atlas_control_server::{AtlasControl, AtlasControlServer};
pub use v0::atlas_plugins_client::AtlasPluginsClient;
pub use v0::atlas_plugins_server::{AtlasPlugins, AtlasPluginsServer};
pub use v0::atlas_query_client::AtlasQueryClient;
pub use v0::atlas_query_server::{AtlasQuery, AtlasQueryServer};
pub use v0::atlas_rules_client::AtlasRulesClient;
pub use v0::atlas_rules_server::{AtlasRules, AtlasRulesServer};
pub use v0::{
    ActionRisk, BatteryStatus, Bookmark, BootRecord, CapabilitiesReply, CapabilitiesRequest,
    CapabilityKind, Confidence, Connection, ContributingFactor, CreateBookmarkReply,
    CreateBookmarkRequest, DiagnoseReply, DiagnoseRequest, Diagnosis, EventRow, EvidenceItem,
    ExecuteActionReply, ExecuteActionRequest, FindResourceOwnersReply, FindResourceOwnersRequest,
    GenerateReportReply, GenerateReportRequest, GetBatteryStatusReply, GetBatteryStatusRequest,
    GpuAdapterTelemetry, GpuEngineClass, GpuEngineTelemetry,
    GetThermalReply, GetThermalRequest, HandleRow, Incident, IncidentKind, L4Protocol,
    ListBookmarksReply, ListBookmarksRequest, ListBootsReply, ListBootsRequest,
    ListConnectionsReply, ListConnectionsRequest, ListEventsReply, ListEventsRequest,
    ListHandlesReply, ListHandlesRequest, ListIncidentsReply, ListIncidentsRequest,
    ListListeningPortsReply, ListListeningPortsRequest, ListModulesReply, ListModulesRequest,
    ListPrivacyEventsReply, ListPrivacyEventsRequest, ListPrivacyUsageReply,
    ListPrivacyUsageRequest, ListScheduledTasksReply, ListScheduledTasksRequest, ListServicesReply,
    ListServicesRequest, ListStartupReply, ListStartupRequest, ListThreadsReply,
    ListThreadsRequest, ListeningPort, MetricKind, ModuleRow, PrepareActionReply,
    PrepareActionRequest, PrivacyEvent, PrivacyUsage, ProcessActionKind, ProcessDetail,
    ProcessDetailReply, ProcessDetailRequest, ProcessHit, ProcessRole, ProcessRow, QueryRangeReply,
    QueryRangeRequest, RangeBucket, RedactionOptions, ReportFormat, ResourceOwner, ScheduledTask,
    SearchHit, SearchReply, SearchRequest, ServiceEntry, ServiceStartType, ServiceState, Severity,
    SnapshotReply, SnapshotRequest, StartupEntry, StartupSource, SystemGauges, TcpState,
    ThermalSensor, ThreadRow, TimeRange,
};
// R2 advanced privacy alerts (AtlasQuery, PRD §9.10.3).
pub use v0::{
    CreatePrivacyAlertRuleReply, CreatePrivacyAlertRuleRequest, DeletePrivacyAlertRuleReply,
    DeletePrivacyAlertRuleRequest, FiredAlert, ListFiredAlertsReply, ListFiredAlertsRequest,
    ListPrivacyAlertRulesReply, ListPrivacyAlertRulesRequest, PrivacyAlertCondition,
    PrivacyAlertRule, UpdatePrivacyAlertRuleReply, UpdatePrivacyAlertRuleRequest,
};
// R3 forensics: system-change tracking + crash correlation (AtlasQuery, PRD
// §9.13/§9.14).
pub use v0::{
    CrashKind, CrashRecord, ListCrashesReply, ListCrashesRequest, ListSystemChangesReply,
    ListSystemChangesRequest, SystemChange, SystemChangeKind,
};
// R3 remote support bundle (AtlasQuery, PRD §9.18/§18.3). One redacted, self-
// contained diagnostic document assembled from data Atlas already has.
pub use v0::{SupportBundleReply, SupportBundleRequest, SupportBundleSection};
// R3 expert security metadata (AtlasQuery, PRD §9.4.1/§9.4.6). On-demand deep
// security detail for one process: file hash, signing cert chain, token
// privileges/groups/capabilities, and process mitigation policies.
pub use v0::{
    CertInfo, GetSecurityMetadataReply, GetSecurityMetadataRequest, SecurityMetadata,
    TokenPrivilege,
};
// R2 rules engine + profiles (AtlasRules service, PRD §9.7).
pub use v0::{
    CoreAffinityMode, CreateProfileReply, CreateProfileRequest, CreateRuleReply, CreateRuleRequest,
    DeleteProfileReply, DeleteProfileRequest, DeleteRuleReply, DeleteRuleRequest, GetRuleReply,
    GetRuleRequest, Intervention, ListInterventionsReply, ListInterventionsRequest,
    ListProfilesReply, ListProfilesRequest, ListRulesReply, ListRulesRequest, PriorityClass,
    Profile, Rule, RuleAction, RuleTrigger, SetProfileActiveReply, SetProfileActiveRequest,
    SetRuleEnabledReply, SetRuleEnabledRequest, SimulateRuleReply, SimulateRuleRequest,
    SimulatedTarget, UpdateProfileReply, UpdateProfileRequest, UpdateRuleReply, UpdateRuleRequest,
};
// R3 dynamic responsiveness protection (AtlasRules service, PRD §9.7.3).
pub use v0::{
    DynamicProtectionConfig, GetDynamicProtectionReply, GetDynamicProtectionRequest,
    SetDynamicProtectionReply, SetDynamicProtectionRequest,
};
// R3 signed plugin framework (AtlasPlugins service, PRD §18.3, tech-stack §4.6).
// Out-of-process, Authenticode-signed, capability-scoped READ-ONLY extensions.
pub use v0::{
    GrantPluginCapabilitiesReply, GrantPluginCapabilitiesRequest, ListPluginsReply,
    ListPluginsRequest, OpenPluginSessionReply, OpenPluginSessionRequest, Plugin, PluginCapability,
    PluginSignature, RegisterPluginReply, RegisterPluginRequest, RemovePluginReply,
    RemovePluginRequest, SetPluginEnabledReply, SetPluginEnabledRequest,
};

/// gRPC metadata key a plugin presents on every AtlasQuery call. The server
/// interceptor rejects any call outside the token's granted capabilities and
/// rejects the key outright on the AtlasControl / AtlasRules / AtlasPlugins
/// (mutating / management) surfaces — plugins are read-only, full stop.
pub const PLUGIN_TOKEN_METADATA_KEY: &str = "atlas-plugin-token";

/// Capability flag advertised by [`v0::CapabilitiesReply`] when the service can
/// serve process snapshots. Always present in M4; sensor/ETW flags follow in
/// later milestones (degraded-mode propagation, tech-stack §5).
pub const CAP_PROCESS_SNAPSHOTS: &str = "process_snapshots";
pub const CAP_GPU_CORE_TELEMETRY: &str = "gpu_core_telemetry";
pub const CAP_GPU_PROCESS_MEMORY: &str = "gpu_process_memory";
pub const CAP_GPU_VENDOR_SENSORS: &str = "gpu_vendor_sensors";
pub const CAP_GPU_CRASH_MONITORING: &str = "gpu_crash_monitoring";
pub const CAP_GPU_RULE_TRIGGERS: &str = "gpu_rule_triggers";

/// M6: the service answers historical range/event/search/bookmark queries from
/// the local store (AtlasQuery's read surface).
pub const CAP_HISTORY_QUERIES: &str = "history_queries";

/// M6: the service exposes the safe-action broker (AtlasControl). Present only
/// when the store is available (audit trail) — the UI hides the action ladder
/// otherwise.
pub const CAP_SAFE_ACTIONS: &str = "safe_actions";

/// M6: full-text search is backed by SQLite FTS5 (prefix matching). When absent
/// the service still answers Search via a LIKE substring scan.
pub const CAP_FTS5_SEARCH: &str = "fts5_search";

/// M7: the service records + serves camera/mic/location usage history from the
/// CapabilityAccessManager ConsentStore (`ListPrivacyUsage` is the live snapshot;
/// `ListPrivacyEvents` reads recorded transitions, PRD §9.10).
pub const CAP_PRIVACY_EVENTS: &str = "privacy_events";

/// M7: the service enumerates the startup inventory (Run keys, Startup folders,
/// StartupApproved) live from the OS (`ListStartup`, PRD §9.8.1).
pub const CAP_STARTUP_INVENTORY: &str = "startup_inventory";

/// M7: the service enumerates the Win32 services inventory via the SCM live from
/// the OS (`ListServices`, PRD §9.9.1).
pub const CAP_SERVICES_INVENTORY: &str = "services_inventory";

/// M8: the service detects threshold+duration incidents (CPU saturation, memory
/// pressure) over the recorded series and serves them (`ListIncidents`,
/// PRD §9.3.7).
pub const CAP_INCIDENT_DETECTION: &str = "incident_detection";

/// M8: the service builds evidence-based diagnoses of incidents from recorded
/// data — no LLM, no fabrication (`Diagnose`, PRD §9.15).
pub const CAP_DIAGNOSTICS: &str = "diagnostics";

/// M8: the service exports diagnosis reports (text/JSON/CSV/HTML) with a
/// redaction pass (`GenerateReport`, PRD §9.18).
pub const CAP_REPORTS: &str = "reports";

/// R2: the service answers the on-demand deep process inspector — process
/// detail, handles, modules, threads (`GetProcessDetail` / `ListHandles` /
/// `ListModules` / `ListThreads`, PRD §9.4). Cross-user handle/module coverage
/// may be limited without elevation; replies carry explicit coverage flags.
pub const CAP_PROCESS_INSPECTOR: &str = "process_inspector";

/// R2: the service answers resource-ownership ("what is using this file")
/// queries via the Restart Manager (`FindResourceOwners`, PRD §9.5).
pub const CAP_RESOURCE_OWNERSHIP: &str = "resource_ownership";

/// R2: the service runs the performance rules engine (AtlasRules) — persistent,
/// reversible, audited priority/affinity/EcoQoS policies over matching
/// processes, with a pure resolver + dry-run simulation (PRD §9.7). Present only
/// when the store is available (rule persistence + audit trail).
pub const CAP_RULES_ENGINE: &str = "rules_engine";

/// R2: the service supports rule profiles — named, activatable bundles of rules
/// plus a power mode (`SetProfileActive`, PRD §9.7.4).
pub const CAP_PROFILES: &str = "profiles";
/// R2: the service enumerates TCP/UDP connections and listening ports (owner-pid
/// tables + best-effort DNS-cache domains) — `ListConnections` /
/// `ListListeningPorts` (PRD §9.12). A live OS read, always available on Windows.
pub const CAP_NETWORK_INSPECTOR: &str = "network_inspector";

/// R2: the service enumerates scheduled tasks via the Task Scheduler COM API
/// (`ListScheduledTasks`, PRD §9.9.2). Advertised only when the enumeration
/// actually returns tasks (COM/connection reachable).
pub const CAP_SCHEDULED_TASKS: &str = "scheduled_tasks";

/// R2: the service reports boot performance from the Diagnostics-Performance
/// event log (`ListBoots`, PRD §9.8.4). Advertised only when that channel is
/// readable (it often needs elevation); the reply also carries available +
/// unavailable_reason for a precise per-call answer.
pub const CAP_BOOT_ANALYSIS: &str = "boot_analysis";

/// R2: the service reports battery status/health (`GetBatteryStatus`,
/// PRD §9.6.6). Advertised only on a machine that has a battery.
pub const CAP_BATTERY_STATUS: &str = "battery_status";

/// R2: the service reports ACPI thermal-zone temperatures via WMI
/// (`GetThermal`, PRD §9.6.7). Advertised only when at least one thermal sensor
/// is exposed.
pub const CAP_THERMAL_SENSORS: &str = "thermal_sensors";

/// R3: the service runs the dynamic responsiveness protection watchdog
/// (PRD §9.7.3) — a safety-gated background-CPU-monopolizer damper with automatic
/// restoration, shared reversal ledger, and audited interventions surfaced
/// through `ListInterventions` (rule_id = 0). Config via `GetDynamicProtection` /
/// `SetDynamicProtection`; store-backed and disabled by default.
pub const CAP_DYNAMIC_PROTECTION: &str = "dynamic_protection";

/// R2: the service runs the advanced-privacy-alerts engine (PRD §9.10.3) — the
/// ConsentStore change-watcher + rule evaluator recording fired alerts, plus the
/// alert-rule CRUD + `ListFiredAlerts` surface on AtlasQuery. Store-backed
/// (rule persistence + fired-alert history); always available on Windows here.
pub const CAP_PRIVACY_ALERTS: &str = "privacy_alerts";

/// R3: the service tracks system changes (PRD §9.13) — a periodic detector diffs
/// its own app/service/startup/task/power/default-app inventories (the reliable,
/// unprivileged core) and augments with WUA update history + driver events where
/// available; `ListSystemChanges` reads the recorded changes. Store-backed;
/// always available on Windows here.
pub const CAP_SYSTEM_CHANGES: &str = "system_changes";

/// R3: the service correlates crashes/hangs/bugchecks/service-failures with the
/// resource + change context around each event (PRD §9.14, `ListCrashes`).
/// Advertised only when the WER/reliability event-log channels are readable; the
/// reply also carries available + unavailable_reason for a precise per-call answer.
pub const CAP_CRASH_ANALYSIS: &str = "crash_analysis";

/// R3: the service can assemble a single redacted, self-contained diagnostic
/// support bundle (device info, health, incidents+diagnoses, system changes,
/// crashes, service/startup inventories, own overhead) from data Atlas already
/// has, passed through the shared redactor (PRD §9.18/§18.3,
/// `GenerateSupportBundle`). Store-backed + live OS reads; always available on
/// Windows here.
pub const CAP_SUPPORT_BUNDLE: &str = "support_bundle";

/// R3: the service runs the signed plugin framework (PRD §18.3, tech-stack
/// §4.6) — the AtlasPlugins registry/management surface plus the server-side
/// capability interceptor that scopes plugin sessions to their granted read-only
/// slice of AtlasQuery. Store-backed (the `plugin` registry + audit); off by
/// default (no plugin runs until the user registers, grants, and enables one).
pub const CAP_PLUGINS: &str = "plugins";

/// R3: the service answers the on-demand expert security metadata query for one
/// process (`GetSecurityMetadata`, PRD §9.4.1/§9.4.6) — the on-disk image
/// SHA-256, the signing certificate chain, the token privileges/groups/
/// capabilities, and the readable process mitigation policies. A live OS read
/// (no store); cross-user/protected fields degrade honestly (`limited`). Always
/// available on Windows here.
pub const CAP_SECURITY_METADATA: &str = "security_metadata";
