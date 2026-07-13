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
pub use v0::atlas_query_client::AtlasQueryClient;
pub use v0::atlas_query_server::{AtlasQuery, AtlasQueryServer};
pub use v0::{
    ActionRisk, Bookmark, CapabilitiesReply, CapabilitiesRequest, CapabilityKind, Confidence,
    ContributingFactor, CreateBookmarkReply, CreateBookmarkRequest, DiagnoseReply, DiagnoseRequest,
    Diagnosis, EventRow, EvidenceItem, ExecuteActionReply, ExecuteActionRequest,
    GenerateReportReply, GenerateReportRequest, Incident, IncidentKind, ListBookmarksReply,
    ListBookmarksRequest, ListEventsReply, ListEventsRequest, ListIncidentsReply,
    ListIncidentsRequest, ListPrivacyEventsReply, ListPrivacyEventsRequest, ListPrivacyUsageReply,
    ListPrivacyUsageRequest, ListServicesReply, ListServicesRequest, ListStartupReply,
    ListStartupRequest, MetricKind, PrepareActionReply, PrepareActionRequest, PrivacyEvent,
    PrivacyUsage, ProcessActionKind, ProcessHit, ProcessRole, ProcessRow, QueryRangeReply,
    QueryRangeRequest, RangeBucket, RedactionOptions, ReportFormat, SearchHit, SearchReply,
    SearchRequest, ServiceEntry, ServiceStartType, ServiceState, Severity, SnapshotReply,
    SnapshotRequest, StartupEntry, StartupSource, SystemGauges, TimeRange,
};

/// Capability flag advertised by [`v0::CapabilitiesReply`] when the service can
/// serve process snapshots. Always present in M4; sensor/ETW flags follow in
/// later milestones (degraded-mode propagation, tech-stack §5).
pub const CAP_PROCESS_SNAPSHOTS: &str = "process_snapshots";

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
