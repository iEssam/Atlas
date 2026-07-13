//! IPC host: serves the `AtlasQuery` contract (docs/phases.md M4) over the
//! Windows named pipe from `atlas-ipc`.
//!
//! A background OS thread runs the [`Sampler`] at ~1 Hz and publishes each new
//! [`SampleSet`] into a shared latest-snapshot slot plus a broadcast channel.
//! The gRPC handlers read that slot (`GetSnapshot`) or subscribe to the
//! broadcast (`StreamSnapshots`); the sampler never blocks on a client. Keeping
//! the sampler on its own thread mirrors the `record` path and keeps the
//! blocking `NtQuerySystemInformation` call off the tokio runtime.

#![cfg(windows)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tonic::{Request, Response, Status};

#[cfg(windows)]
use atlas_collectors::{
    enumerate_privacy_usage, enumerate_services, enumerate_startup, Capability,
    CollectorServiceState, CollectorStartupSource, ServiceStartType as CollectorStartType,
};
use atlas_collectors::{
    group_processes, GroupInput, ProcessRole as CollectorRole, SampleSet, Sampler,
};
use atlas_ipc::v0::atlas_query_server::AtlasQuery;
use atlas_ipc::{
    Bookmark, CapabilitiesReply, CapabilitiesRequest, CapabilityKind, CreateBookmarkReply,
    CreateBookmarkRequest, EventRow, ListBookmarksReply, ListBookmarksRequest, ListEventsReply,
    ListEventsRequest, ListPrivacyEventsReply, ListPrivacyEventsRequest, ListPrivacyUsageReply,
    ListPrivacyUsageRequest, ListServicesReply, ListServicesRequest, ListStartupReply,
    ListStartupRequest, MetricKind, PrivacyEvent, PrivacyUsage, ProcessHit, ProcessRole,
    ProcessRow, QueryRangeReply, QueryRangeRequest, RangeBucket, RingUpdate, RingWriter, RowInput,
    SearchHit, SearchReply, SearchRequest, ServiceEntry, ServiceStartType, ServiceState,
    SnapshotReply, SnapshotRequest, StartupEntry, StartupSource, SystemGauges, TimeRange,
    CAP_FTS5_SEARCH, CAP_HISTORY_QUERIES, CAP_PRIVACY_EVENTS, CAP_PROCESS_SNAPSHOTS,
    CAP_SAFE_ACTIONS, CAP_SERVICES_INVENTORY, CAP_STARTUP_INVENTORY, RING_ROWS,
};
use atlas_store::Store;
use atlas_tsdb::Metric;

/// Shared handle to the local store for the read path (history queries and
/// bookmarks) and the broker's audit log. The writer/`record` path owns writes
/// to the same db file; SQLite WAL mode lets this read-path connection coexist
/// with it (the store opens WAL + busy_timeout). Guarded by a `Mutex` because a
/// rusqlite `Connection` is not `Sync`; contention is negligible (queries are
/// interactive, not on the 1 Hz hot path).
pub type SharedStore = Arc<Mutex<Store>>;

/// Latest published snapshot, already converted to the wire shape. `None`
/// until the first sample lands.
type Slot = Arc<RwLock<Option<SnapshotReply>>>;

/// Maps a collector grouping role to the proto `ProcessRole` discriminant.
fn role_to_proto(role: CollectorRole) -> i32 {
    let r = match role {
        CollectorRole::Main => ProcessRole::Main,
        CollectorRole::Helper => ProcessRole::Helper,
        CollectorRole::Service => ProcessRole::Service,
    };
    r as i32
}

/// Converts a collector [`SampleSet`] into the proto [`SnapshotReply`], sorted
/// by CPU descending (working set as a tiebreak, matching the console `top`).
/// Fills each row's application-group key + role from the grouping heuristic
/// (PRD §9.2.1) over the whole snapshot.
fn to_reply(set: &SampleSet) -> SnapshotReply {
    let s = &set.system;
    // Grouping runs over the full process list (parent chains need every row).
    let group_inputs: Vec<GroupInput> = set
        .processes
        .iter()
        .map(|p| GroupInput {
            pid: p.key.pid,
            parent_pid: p.parent_pid,
            image_name: p.image_name.clone(),
            session_id: p.session_id,
        })
        .collect();
    let groups = group_processes(&group_inputs);
    let mut processes: Vec<ProcessRow> = set
        .processes
        .iter()
        .map(|p| {
            let (app_group, role) = groups
                .get(&p.key.pid)
                .map(|g| (g.app_group.clone(), role_to_proto(g.role)))
                .unwrap_or_else(|| (String::new(), ProcessRole::Unspecified as i32));
            ProcessRow {
                pid: p.key.pid,
                parent_pid: p.parent_pid,
                image_name: p.image_name.clone(),
                session_id: p.session_id,
                create_time_100ns: p.key.create_time_100ns,
                cpu_permille: p.cpu_permille,
                working_set: p.working_set,
                private_bytes: p.private_bytes,
                read_bps: p.read_bps,
                write_bps: p.write_bps,
                handle_count: p.handle_count,
                thread_count: p.thread_count,
                app_group,
                role,
            }
        })
        .collect();
    sort_by_cpu_desc(&mut processes);
    SnapshotReply {
        system: Some(SystemGauges {
            ts_ms: set.ts_ms,
            cpu_permille: s.cpu_permille,
            mem_used: s.mem_used,
            mem_total: s.mem_total,
            commit_used: s.commit_used,
            commit_limit: s.commit_limit,
            process_count: s.process_count,
            thread_count: s.thread_count,
            handle_count: s.handle_count,
        }),
        processes,
    }
}

/// Sorts process rows by CPU descending, working set descending as a tiebreak.
/// Extracted so `top_n` truncation logic stays testable without a sampler.
fn sort_by_cpu_desc(rows: &mut [ProcessRow]) {
    rows.sort_by(|a, b| {
        b.cpu_permille
            .cmp(&a.cpu_permille)
            .then(b.working_set.cmp(&a.working_set))
    });
}

/// Applies `top_n` to an already-sorted reply: 0 means all, otherwise keep the
/// first `top_n` rows. Returns a clone honoring the cap.
fn apply_top_n(reply: &SnapshotReply, top_n: u32) -> SnapshotReply {
    let mut out = reply.clone();
    let n = top_n as usize;
    if n != 0 && n < out.processes.len() {
        out.processes.truncate(n);
    }
    out
}

/// The service host: owns the shared slot, a broadcast sender for streaming,
/// the stop flag for the sampler thread, and a shared read-path store handle
/// for history queries + bookmarks.
pub struct QueryService {
    slot: Slot,
    tx: tokio::sync::broadcast::Sender<SnapshotReply>,
    stop: Arc<AtomicBool>,
    store: SharedStore,
    has_fts5: bool,
}

impl QueryService {
    /// The shared store handle (so the broker service can share the same
    /// connection/audit log).
    pub fn store(&self) -> SharedStore {
        self.store.clone()
    }

    /// Spawns the sampler thread and returns the service handle. The thread
    /// runs until [`QueryService::shutdown`] flips the stop flag (or the
    /// process exits).
    ///
    /// `ring_disc` is the shared-memory ring discriminator (typically the same
    /// as the pipe discriminator so a `ring-read` client rendezvous with this
    /// server). Ring creation is best-effort: a failure logs a warning and the
    /// gRPC path continues unaffected (tech-stack §5.1).
    ///
    /// `db_path` is the local store; opened read/write here for the query
    /// surface (range/events/search/bookmarks) and the broker audit log. It is
    /// the same db file the `record` path writes — WAL mode keeps the two
    /// connections coexisting. Opening the store failing is fatal (the query
    /// surface can't be served without it).
    pub fn start(ring_disc: &str, db_path: PathBuf) -> anyhow::Result<Self> {
        let store = Store::open(&db_path)?;
        let has_fts5 = store.has_fts5();
        let store: SharedStore = Arc::new(Mutex::new(store));
        let slot: Slot = Arc::new(RwLock::new(None));
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let stop = Arc::new(AtomicBool::new(false));

        // Best-effort: the ring is a fast-path convenience, never a hard
        // dependency. If the section cannot be created the service still serves
        // snapshots over gRPC.
        let ring = match RingWriter::create(ring_disc) {
            Ok(w) => {
                tracing::info!(ring = %atlas_ipc::section_name(ring_disc), "live ring published");
                Some(w)
            }
            Err(e) => {
                tracing::warn!("live ring unavailable (gRPC continues): {e}");
                None
            }
        };

        let thread_slot = slot.clone();
        let thread_tx = tx.clone();
        let thread_stop = stop.clone();
        std::thread::Builder::new()
            .name("atlas-ipc-sampler".into())
            .spawn(move || sampler_loop(thread_slot, thread_tx, thread_stop, ring))?;

        Ok(Self {
            slot,
            tx,
            stop,
            store,
            has_fts5,
        })
    }

    /// Signals the sampler thread to stop.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Sampler thread body: sample ~1 Hz, publish to the slot, the broadcast, and
/// (best-effort) the shared-memory live ring.
fn sampler_loop(
    slot: Slot,
    tx: tokio::sync::broadcast::Sender<SnapshotReply>,
    stop: Arc<AtomicBool>,
    ring: Option<RingWriter>,
) {
    let mut sampler = match Sampler::new() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("sampler init failed: {e}");
            return;
        }
    };
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(1));
        match sampler.sample() {
            Ok(set) => {
                let reply = to_reply(&set);
                // Publish the top-N rows into the live ring (already CPU-sorted
                // by `to_reply`). Best-effort; the ring only carries RING_ROWS.
                if let Some(w) = ring.as_ref() {
                    publish_ring(w, &reply);
                }
                if let Ok(mut guard) = slot.write() {
                    *guard = Some(reply.clone());
                }
                // Best-effort: no subscribers is fine (send errors ignored).
                let _ = tx.send(reply);
            }
            Err(e) => tracing::warn!("sample failed: {e}"),
        }
    }
    tracing::debug!("ipc sampler thread stopped");
}

/// Publishes a (CPU-sorted) reply into the shared-memory ring: system gauges
/// plus the top [`RING_ROWS`] process rows. The seqlock write is a single
/// [`RingWriter::publish`] call.
fn publish_ring(writer: &RingWriter, reply: &SnapshotReply) {
    let rows: Vec<RowInput> = reply
        .processes
        .iter()
        .take(RING_ROWS)
        .map(|p| RowInput {
            pid: p.pid,
            cpu_permille: p.cpu_permille,
            working_set: p.working_set,
            private_bytes: p.private_bytes,
            read_bps: p.read_bps,
            write_bps: p.write_bps,
            name: &p.image_name,
        })
        .collect();
    let s = reply.system.as_ref();
    writer.publish(&RingUpdate {
        ts_ms: s.map(|g| g.ts_ms).unwrap_or(0),
        cpu_permille: s.map(|g| g.cpu_permille).unwrap_or(0),
        process_count: s.map(|g| g.process_count).unwrap_or(0),
        thread_count: s.map(|g| g.thread_count).unwrap_or(0),
        handle_count: s.map(|g| g.handle_count).unwrap_or(0),
        mem_used: s.map(|g| g.mem_used).unwrap_or(0),
        mem_total: s.map(|g| g.mem_total).unwrap_or(0),
        commit_used: s.map(|g| g.commit_used).unwrap_or(0),
        commit_limit: s.map(|g| g.commit_limit).unwrap_or(0),
        rows: &rows,
    });
}

#[tonic::async_trait]
impl AtlasQuery for QueryService {
    async fn get_capabilities(
        &self,
        _req: Request<CapabilitiesRequest>,
    ) -> Result<Response<CapabilitiesReply>, Status> {
        // M4: process snapshots always available. M6 adds the history-query
        // surface and the safe-action broker (both backed by the store, always
        // open here); FTS5 search is advertised only when the module is present.
        let mut flags = vec![
            CAP_PROCESS_SNAPSHOTS.to_string(),
            CAP_HISTORY_QUERIES.to_string(),
            CAP_SAFE_ACTIONS.to_string(),
        ];
        if self.has_fts5 {
            flags.push(CAP_FTS5_SEARCH.to_string());
        }
        // M7 inventories are live OS reads (startup/services) and store-backed
        // history (privacy). On Windows all three are always available here.
        #[cfg(windows)]
        {
            flags.push(CAP_PRIVACY_EVENTS.to_string());
            flags.push(CAP_STARTUP_INVENTORY.to_string());
            flags.push(CAP_SERVICES_INVENTORY.to_string());
        }
        Ok(Response::new(CapabilitiesReply {
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            capability_flags: flags,
        }))
    }

    async fn get_snapshot(
        &self,
        req: Request<SnapshotRequest>,
    ) -> Result<Response<SnapshotReply>, Status> {
        let top_n = req.into_inner().top_n;
        let latest = self
            .slot
            .read()
            .map_err(|_| Status::internal("snapshot slot poisoned"))?
            .clone();
        match latest {
            Some(reply) => Ok(Response::new(apply_top_n(&reply, top_n))),
            None => Err(Status::unavailable(
                "no snapshot yet; sampler is still warming up",
            )),
        }
    }

    type StreamSnapshotsStream =
        tokio_stream::wrappers::ReceiverStream<Result<SnapshotReply, Status>>;

    async fn stream_snapshots(
        &self,
        req: Request<SnapshotRequest>,
    ) -> Result<Response<Self::StreamSnapshotsStream>, Status> {
        let top_n = req.into_inner().top_n;
        let mut source = self.tx.subscribe();
        let (out_tx, out_rx) = tokio::sync::mpsc::channel(8);

        // Emit the current snapshot immediately (if any) so a subscriber does
        // not wait up to a second for the first line. Clone out of the lock and
        // drop the guard before awaiting (the guard is not Send).
        let current = self.slot.read().ok().and_then(|g| g.clone());
        if let Some(reply) = current {
            let _ = out_tx.send(Ok(apply_top_n(&reply, top_n))).await;
        }

        // Forward each new sample until the client disconnects (out_tx send
        // fails) or the broadcast lags/closes.
        tokio::spawn(async move {
            loop {
                match source.recv().await {
                    Ok(reply) => {
                        if out_tx.send(Ok(apply_top_n(&reply, top_n))).await.is_err() {
                            break; // client gone
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            out_rx,
        )))
    }

    async fn query_range(
        &self,
        req: Request<QueryRangeRequest>,
    ) -> Result<Response<QueryRangeReply>, Status> {
        let r = req.into_inner();
        let metric = map_metric(r.metric)
            .ok_or_else(|| Status::invalid_argument("unknown or unspecified metric"))?;
        let (from_ms, to_ms) = range_bounds(&r.range);
        let store = self.store.lock().map_err(|_| poisoned())?;
        let buckets = store
            .query_range(metric, r.scope, from_ms, to_ms, r.buckets)
            .map_err(|e| Status::internal(format!("query_range: {e}")))?;
        Ok(Response::new(QueryRangeReply {
            buckets: buckets
                .into_iter()
                .map(|b| RangeBucket {
                    start_ms: b.start_ms,
                    min: b.min,
                    max: b.max,
                    avg: b.avg,
                    samples: b.samples,
                })
                .collect(),
        }))
    }

    async fn list_events(
        &self,
        req: Request<ListEventsRequest>,
    ) -> Result<Response<ListEventsReply>, Status> {
        let r = req.into_inner();
        let (from_ms, to_ms) = range_bounds(&r.range);
        let limit = if r.limit == 0 { 1000 } else { r.limit };
        let store = self.store.lock().map_err(|_| poisoned())?;
        let (rows, truncated) = store
            .list_events(from_ms, to_ms, &r.kinds, limit)
            .map_err(|e| Status::internal(format!("list_events: {e}")))?;
        Ok(Response::new(ListEventsReply {
            events: rows.into_iter().map(to_event_row).collect(),
            truncated,
        }))
    }

    async fn search(&self, req: Request<SearchRequest>) -> Result<Response<SearchReply>, Status> {
        let r = req.into_inner();
        let limit = if r.limit == 0 { 50 } else { r.limit };
        let store = self.store.lock().map_err(|_| poisoned())?;
        let hits = store
            .search(&r.query, limit)
            .map_err(|e| Status::internal(format!("search: {e}")))?;
        // Interleave process, event, and bookmark hits into the flat oneof list.
        let mut out: Vec<SearchHit> = Vec::new();
        for p in hits.processes {
            out.push(SearchHit {
                entity: Some(atlas_ipc::v0::search_hit::Entity::Process(ProcessHit {
                    proc_row_id: p.proc_row_id,
                    pid: p.pid,
                    image_name: p.image_name,
                    first_seen_ms: p.first_seen_ms,
                    exit_seen_ms: p.exit_seen_ms,
                    live: p.live,
                })),
            });
        }
        for e in hits.events {
            out.push(SearchHit {
                entity: Some(atlas_ipc::v0::search_hit::Entity::Event(to_event_row(e))),
            });
        }
        for b in hits.bookmarks {
            out.push(SearchHit {
                entity: Some(atlas_ipc::v0::search_hit::Entity::Bookmark(Bookmark {
                    id: b.id,
                    ts_ms: b.ts_ms,
                    label: b.label,
                    created_ms: b.created_ms,
                })),
            });
        }
        Ok(Response::new(SearchReply { hits: out }))
    }

    async fn create_bookmark(
        &self,
        req: Request<CreateBookmarkRequest>,
    ) -> Result<Response<CreateBookmarkReply>, Status> {
        let r = req.into_inner();
        let store = self.store.lock().map_err(|_| poisoned())?;
        let id = store
            .create_bookmark(r.ts_ms, &r.label)
            .map_err(|e| Status::internal(format!("create_bookmark: {e}")))?;
        Ok(Response::new(CreateBookmarkReply { id }))
    }

    async fn list_bookmarks(
        &self,
        req: Request<ListBookmarksRequest>,
    ) -> Result<Response<ListBookmarksReply>, Status> {
        let r = req.into_inner();
        let (from_ms, to_ms) = range_bounds(&r.range);
        let store = self.store.lock().map_err(|_| poisoned())?;
        let rows = store
            .list_bookmarks(from_ms, to_ms)
            .map_err(|e| Status::internal(format!("list_bookmarks: {e}")))?;
        Ok(Response::new(ListBookmarksReply {
            bookmarks: rows
                .into_iter()
                .map(|b| Bookmark {
                    id: b.id,
                    ts_ms: b.ts_ms,
                    label: b.label,
                    created_ms: b.created_ms,
                })
                .collect(),
        }))
    }

    async fn list_privacy_usage(
        &self,
        req: Request<ListPrivacyUsageRequest>,
    ) -> Result<Response<ListPrivacyUsageReply>, Status> {
        let r = req.into_inner();
        // Live point-in-time read of the ConsentStore (PRD §9.10). The optional
        // capability filter maps the proto enum onto the collector's set.
        let usages = list_privacy_usage_impl(&r.capabilities);
        Ok(Response::new(ListPrivacyUsageReply { usages }))
    }

    async fn list_privacy_events(
        &self,
        req: Request<ListPrivacyEventsRequest>,
    ) -> Result<Response<ListPrivacyEventsReply>, Status> {
        let r = req.into_inner();
        let (from_ms, to_ms) = range_bounds(&r.range);
        let limit = if r.limit == 0 { 1000 } else { r.limit };
        let store = self.store.lock().map_err(|_| poisoned())?;
        let (rows, truncated) = store
            .list_privacy_events(from_ms, to_ms, limit)
            .map_err(|e| Status::internal(format!("list_privacy_events: {e}")))?;
        Ok(Response::new(ListPrivacyEventsReply {
            events: rows
                .into_iter()
                .map(|e| PrivacyEvent {
                    ts_ms: e.ts_ms,
                    capability: e.capability,
                    app_id: e.app_id,
                    display_name: e.display_name,
                    started: e.started,
                })
                .collect(),
            truncated,
        }))
    }

    async fn list_startup(
        &self,
        _req: Request<ListStartupRequest>,
    ) -> Result<Response<ListStartupReply>, Status> {
        // Live OS read (Run keys, Startup folders, StartupApproved). Not stored:
        // this is current-state, not history.
        let entries = list_startup_impl();
        Ok(Response::new(ListStartupReply { entries }))
    }

    async fn list_services(
        &self,
        req: Request<ListServicesRequest>,
    ) -> Result<Response<ListServicesReply>, Status> {
        let r = req.into_inner();
        // Live SCM enumeration, filtered by name/display substring. Not stored.
        let services = list_services_impl(&r.filter);
        Ok(Response::new(ListServicesReply { services }))
    }
}

// ---------------------------------------------------------------------------
// M7 collector → proto mapping. The `impl` fns are `#[cfg(windows)]` (the
// collectors are Windows-only); a non-Windows stub returns empty so the crate
// still builds on other targets (the RPCs are unreachable there anyway — the
// pipe transport is Windows-only).
// ---------------------------------------------------------------------------

/// Maps the proto [`CapabilityKind`] discriminant to the collector [`Capability`].
/// Returns `None` for UNSPECIFIED / unknown.
#[cfg(windows)]
fn map_capability(kind: i32) -> Option<Capability> {
    match CapabilityKind::try_from(kind).ok()? {
        CapabilityKind::Unspecified => None,
        CapabilityKind::Camera => Some(Capability::Camera),
        CapabilityKind::Microphone => Some(Capability::Microphone),
        CapabilityKind::Location => Some(Capability::Location),
    }
}

/// The proto discriminant for a collector [`Capability`].
#[cfg(windows)]
fn capability_to_proto(cap: Capability) -> i32 {
    let k = match cap {
        Capability::Camera => CapabilityKind::Camera,
        Capability::Microphone => CapabilityKind::Microphone,
        Capability::Location => CapabilityKind::Location,
    };
    k as i32
}

#[cfg(windows)]
fn list_privacy_usage_impl(capabilities: &[i32]) -> Vec<PrivacyUsage> {
    let wanted: Vec<Capability> = capabilities
        .iter()
        .filter_map(|&k| map_capability(k))
        .collect();
    enumerate_privacy_usage(&wanted)
        .into_iter()
        .map(|u| PrivacyUsage {
            capability: capability_to_proto(u.capability),
            app_id: u.app_id,
            display_name: u.display_name,
            packaged: u.packaged,
            last_start_ms: u.last_start_ms,
            last_stop_ms: u.last_stop_ms,
            in_use: u.in_use,
        })
        .collect()
}

#[cfg(not(windows))]
fn list_privacy_usage_impl(_capabilities: &[i32]) -> Vec<PrivacyUsage> {
    Vec::new()
}

/// The proto discriminant for a collector [`CollectorStartupSource`].
#[cfg(windows)]
fn startup_source_to_proto(source: CollectorStartupSource) -> i32 {
    let s = match source {
        CollectorStartupSource::RunKeyMachine => StartupSource::RunKeyMachine,
        CollectorStartupSource::RunKeyUser => StartupSource::RunKeyUser,
        CollectorStartupSource::StartupFolderMachine => StartupSource::StartupFolderMachine,
        CollectorStartupSource::StartupFolderUser => StartupSource::StartupFolderUser,
        CollectorStartupSource::ScheduledTask => StartupSource::ScheduledTask,
        CollectorStartupSource::Service => StartupSource::Service,
        CollectorStartupSource::PackagedTask => StartupSource::PackagedTask,
    };
    s as i32
}

#[cfg(windows)]
fn list_startup_impl() -> Vec<StartupEntry> {
    enumerate_startup()
        .into_iter()
        .map(|e| StartupEntry {
            name: e.name,
            source: startup_source_to_proto(e.source),
            command: e.command,
            publisher: e.publisher,
            enabled: e.enabled,
            scope: e.scope.as_str().to_string(),
        })
        .collect()
}

#[cfg(not(windows))]
fn list_startup_impl() -> Vec<StartupEntry> {
    Vec::new()
}

/// The proto discriminant for a collector [`CollectorServiceState`].
#[cfg(windows)]
fn service_state_to_proto(state: CollectorServiceState) -> i32 {
    let s = match state {
        CollectorServiceState::Stopped => ServiceState::ServiceStopped,
        CollectorServiceState::StartPending => ServiceState::ServiceStartPending,
        CollectorServiceState::StopPending => ServiceState::ServiceStopPending,
        CollectorServiceState::Running => ServiceState::ServiceRunning,
        CollectorServiceState::ContinuePending => ServiceState::ServiceContinuePending,
        CollectorServiceState::PausePending => ServiceState::ServicePausePending,
        CollectorServiceState::Paused => ServiceState::ServicePaused,
        CollectorServiceState::Unspecified => ServiceState::Unspecified,
    };
    s as i32
}

/// The proto discriminant for a collector [`CollectorStartType`].
#[cfg(windows)]
fn service_start_type_to_proto(start: CollectorStartType) -> i32 {
    let s = match start {
        CollectorStartType::Boot => ServiceStartType::StartBoot,
        CollectorStartType::System => ServiceStartType::StartSystem,
        CollectorStartType::Auto => ServiceStartType::StartAuto,
        CollectorStartType::Manual => ServiceStartType::StartManual,
        CollectorStartType::Disabled => ServiceStartType::StartDisabled,
        CollectorStartType::Unspecified => ServiceStartType::Unspecified,
    };
    s as i32
}

#[cfg(windows)]
fn list_services_impl(filter: &str) -> Vec<ServiceEntry> {
    enumerate_services(filter)
        .into_iter()
        .map(|s| ServiceEntry {
            name: s.name,
            display_name: s.display_name,
            description: s.description,
            state: service_state_to_proto(s.state),
            start_type: service_start_type_to_proto(s.start_type),
            pid: s.pid,
            account: s.account,
            binary_path: s.binary_path,
            delayed_auto_start: s.delayed_auto_start,
        })
        .collect()
}

#[cfg(not(windows))]
fn list_services_impl(_filter: &str) -> Vec<ServiceEntry> {
    Vec::new()
}

/// A poisoned-store-mutex status (a prior handler panicked mid-query).
fn poisoned() -> Status {
    Status::internal("store mutex poisoned")
}

/// Extracts `(from_ms, to_ms)` from an optional proto `TimeRange`. A missing
/// range degenerates to the full i64 span (return everything), matching a
/// caller that left it unset.
fn range_bounds(range: &Option<TimeRange>) -> (i64, i64) {
    match range {
        Some(r) => (r.from_ms, r.to_ms),
        None => (i64::MIN, i64::MAX),
    }
}

/// Maps a proto [`MetricKind`] discriminant to the internal [`Metric`]. Returns
/// `None` for `UNSPECIFIED` or an unknown value.
fn map_metric(kind: i32) -> Option<Metric> {
    let k = MetricKind::try_from(kind).ok()?;
    Some(match k {
        MetricKind::Unspecified => return None,
        MetricKind::CpuPermille => Metric::CpuPermille,
        MetricKind::WorkingSet => Metric::WorkingSet,
        MetricKind::PrivateBytes => Metric::PrivateBytes,
        MetricKind::ReadBps => Metric::ReadBps,
        MetricKind::WriteBps => Metric::WriteBps,
        MetricKind::SysCpuPermille => Metric::SysCpuPermille,
        MetricKind::SysMemUsed => Metric::SysMemUsed,
        MetricKind::SysCommitUsed => Metric::SysCommitUsed,
        MetricKind::SysProcessCount => Metric::SysProcessCount,
    })
}

/// Converts a store [`atlas_store::EventListRow`] into the proto [`EventRow`].
fn to_event_row(e: atlas_store::EventListRow) -> EventRow {
    EventRow {
        ts_ms: e.ts_ms,
        kind: e.kind,
        pid: e.pid,
        parent_pid: e.parent_pid,
        session_id: e.session_id,
        image_name: e.image_name,
        exit_status: e.exit_status,
        has_exit_status: e.has_exit_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, cpu: u32, ws: u64) -> ProcessRow {
        ProcessRow {
            pid,
            parent_pid: 0,
            image_name: format!("p{pid}"),
            session_id: 0,
            create_time_100ns: 0,
            cpu_permille: cpu,
            working_set: ws,
            private_bytes: 0,
            read_bps: 0,
            write_bps: 0,
            handle_count: 0,
            thread_count: 0,
            app_group: String::new(),
            role: 0,
        }
    }

    fn reply(rows: Vec<ProcessRow>) -> SnapshotReply {
        SnapshotReply {
            system: None,
            processes: rows,
        }
    }

    #[test]
    fn sort_orders_by_cpu_then_ws() {
        let mut rows = vec![row(1, 100, 10), row(2, 300, 5), row(3, 300, 50)];
        sort_by_cpu_desc(&mut rows);
        // 300/ws50, 300/ws5, then 100.
        assert_eq!(
            rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
    }

    #[test]
    fn top_n_zero_returns_all() {
        let r = reply(vec![row(1, 1, 1), row(2, 2, 2)]);
        assert_eq!(apply_top_n(&r, 0).processes.len(), 2);
    }

    #[test]
    fn top_n_truncates_and_preserves_order() {
        let r = reply(vec![row(1, 30, 0), row(2, 20, 0), row(3, 10, 0)]);
        let capped = apply_top_n(&r, 2);
        assert_eq!(capped.processes.len(), 2);
        assert_eq!(capped.processes[0].pid, 1);
        assert_eq!(capped.processes[1].pid, 2);
    }

    #[test]
    fn top_n_larger_than_len_is_all() {
        let r = reply(vec![row(1, 1, 1)]);
        assert_eq!(apply_top_n(&r, 99).processes.len(), 1);
    }

    #[test]
    fn map_metric_and_scoping() {
        use atlas_ipc::MetricKind;
        assert_eq!(
            map_metric(MetricKind::CpuPermille as i32),
            Some(Metric::CpuPermille)
        );
        assert_eq!(
            map_metric(MetricKind::SysMemUsed as i32),
            Some(Metric::SysMemUsed)
        );
        assert_eq!(map_metric(MetricKind::Unspecified as i32), None);
        assert_eq!(map_metric(99999), None);
    }

    #[test]
    fn range_bounds_defaults_to_full_span() {
        assert_eq!(range_bounds(&None), (i64::MIN, i64::MAX));
        assert_eq!(
            range_bounds(&Some(TimeRange {
                from_ms: 5,
                to_ms: 10
            })),
            (5, 10)
        );
    }

    /// to_reply fills app_group + role from the grouping heuristic: a chrome
    /// main with a same-image child share a group; the child is a Helper.
    #[test]
    fn to_reply_fills_grouping() {
        use atlas_collectors::{ProcKey, ProcSample, SystemSample};
        fn sample(pid: u32, parent: u32, name: &str) -> ProcSample {
            ProcSample {
                key: ProcKey {
                    pid,
                    create_time_100ns: 0,
                },
                parent_pid: parent,
                image_name: name.to_string(),
                session_id: 1,
                cpu_permille: 0,
                working_set: 0,
                private_working_set: 0,
                private_bytes: 0,
                read_bps: 0,
                write_bps: 0,
                handle_count: 0,
                thread_count: 0,
            }
        }
        let set = SampleSet {
            ts_ms: 0,
            system: SystemSample {
                cpu_permille: 0,
                mem_used: 0,
                mem_total: 0,
                commit_used: 0,
                commit_limit: 0,
                process_count: 0,
                thread_count: 0,
                handle_count: 0,
            },
            processes: vec![
                sample(100, 50, "chrome.exe"),
                sample(101, 100, "chrome.exe"),
                sample(50, 40, "explorer.exe"),
            ],
            started: vec![],
            exited: vec![],
        };
        let reply = to_reply(&set);
        let main = reply.processes.iter().find(|p| p.pid == 100).unwrap();
        let helper = reply.processes.iter().find(|p| p.pid == 101).unwrap();
        assert_eq!(main.role, ProcessRole::Main as i32);
        assert_eq!(helper.role, ProcessRole::Helper as i32);
        assert!(!main.app_group.is_empty());
        assert_eq!(main.app_group, helper.app_group);
    }
}
