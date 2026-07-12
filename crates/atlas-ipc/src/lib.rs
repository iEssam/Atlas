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
pub mod transport;

#[cfg(windows)]
pub use transport::{connect, default_pipe_name, pipe_name, serve, PipeConnectInfo};

// Convenience re-exports so downstream crates depend on the contract through
// atlas-ipc rather than pinning tonic/prost versions themselves.
pub use v0::atlas_query_client::AtlasQueryClient;
pub use v0::atlas_query_server::{AtlasQuery, AtlasQueryServer};
pub use v0::{
    CapabilitiesReply, CapabilitiesRequest, ProcessRow, SnapshotReply, SnapshotRequest,
    SystemGauges,
};

/// Capability flag advertised by [`v0::CapabilitiesReply`] when the service can
/// serve process snapshots. Always present in M4; sensor/ETW flags follow in
/// later milestones (degraded-mode propagation, tech-stack §5).
pub const CAP_PROCESS_SNAPSHOTS: &str = "process_snapshots";
