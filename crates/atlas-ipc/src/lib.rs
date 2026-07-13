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
    ActionRisk, Bookmark, CapabilitiesReply, CapabilitiesRequest, CreateBookmarkReply,
    CreateBookmarkRequest, EventRow, ExecuteActionReply, ExecuteActionRequest, ListBookmarksReply,
    ListBookmarksRequest, ListEventsReply, ListEventsRequest, MetricKind, PrepareActionReply,
    PrepareActionRequest, ProcessActionKind, ProcessHit, ProcessRole, ProcessRow, QueryRangeReply,
    QueryRangeRequest, RangeBucket, SearchHit, SearchReply, SearchRequest, SnapshotReply,
    SnapshotRequest, SystemGauges, TimeRange,
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
