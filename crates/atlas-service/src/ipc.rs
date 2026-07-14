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
    analyze_boots, battery_status, enumerate_privacy_usage, enumerate_services, enumerate_startup,
    enumerate_tasks, list_connections, list_listening_ports, thermal_status, BatteryReading,
    BootRecord as CollectorBootRecord, Capability, CollectorServiceState, CollectorStartupSource,
    Connection as CollectorConnection, ListeningPort as CollectorListeningPort, NetL4Protocol,
    NetTcpState, PrivacyTransition, PrivacyWatcher, ScheduledTask as CollectorScheduledTask,
    ServiceStartType as CollectorStartType, ThermalReading,
};
use atlas_collectors::{
    group_processes, GroupInput, ProcessRole as CollectorRole, SampleSet, Sampler,
};
use atlas_ipc::v0::atlas_query_server::AtlasQuery;
use atlas_ipc::{
    BatteryStatus as ProtoBatteryStatus, Bookmark, BootRecord as ProtoBootRecord,
    CapabilitiesReply, CapabilitiesRequest, CapabilityKind, Connection as ProtoConnection,
    CreateBookmarkReply, CreateBookmarkRequest, CreatePrivacyAlertRuleReply,
    CreatePrivacyAlertRuleRequest, DeletePrivacyAlertRuleReply, DeletePrivacyAlertRuleRequest,
    DiagnoseReply, DiagnoseRequest, EventRow, FindResourceOwnersReply, FindResourceOwnersRequest,
    FiredAlert, GenerateReportReply, GenerateReportRequest, GetBatteryStatusReply,
    GetBatteryStatusRequest, GetThermalReply, GetThermalRequest, HandleRow, Incident, L4Protocol,
    ListBookmarksReply, ListBookmarksRequest, ListBootsReply, ListBootsRequest,
    ListConnectionsReply, ListConnectionsRequest, ListEventsReply, ListEventsRequest,
    ListFiredAlertsReply, ListFiredAlertsRequest, ListHandlesReply, ListHandlesRequest,
    ListIncidentsReply, ListIncidentsRequest, ListListeningPortsReply, ListListeningPortsRequest,
    ListModulesReply, ListModulesRequest, ListPrivacyAlertRulesReply, ListPrivacyAlertRulesRequest,
    ListPrivacyEventsReply, ListPrivacyEventsRequest, ListPrivacyUsageReply,
    ListPrivacyUsageRequest, ListScheduledTasksReply, ListScheduledTasksRequest, ListServicesReply,
    ListServicesRequest, ListStartupReply, ListStartupRequest, ListThreadsReply,
    ListThreadsRequest, ListeningPort as ProtoListeningPort, MetricKind, ModuleRow,
    PrivacyAlertRule, PrivacyEvent, PrivacyUsage, ProcessDetail as ProtoProcessDetail,
    ProcessDetailReply, ProcessDetailRequest, ProcessHit, ProcessRole, ProcessRow, QueryRangeReply,
    QueryRangeRequest, RangeBucket, ReportFormat, ResourceOwner as ProtoResourceOwner, RingUpdate,
    RingWriter, RowInput, ScheduledTask as ProtoScheduledTask, SearchHit, SearchReply,
    SearchRequest, ServiceEntry, ServiceStartType, ServiceState, SnapshotReply, SnapshotRequest,
    StartupEntry, StartupSource, SystemGauges, TcpState, ThermalSensor as ProtoThermalSensor,
    ThreadRow, TimeRange, UpdatePrivacyAlertRuleReply, UpdatePrivacyAlertRuleRequest,
    CAP_BATTERY_STATUS, CAP_BOOT_ANALYSIS, CAP_DIAGNOSTICS, CAP_DYNAMIC_PROTECTION,
    CAP_FTS5_SEARCH, CAP_HISTORY_QUERIES, CAP_INCIDENT_DETECTION, CAP_NETWORK_INSPECTOR,
    CAP_PRIVACY_ALERTS, CAP_PRIVACY_EVENTS, CAP_PROCESS_INSPECTOR, CAP_PROCESS_SNAPSHOTS,
    CAP_PROFILES, CAP_REPORTS, CAP_RESOURCE_OWNERSHIP, CAP_RULES_ENGINE, CAP_SAFE_ACTIONS,
    CAP_SCHEDULED_TASKS, CAP_SERVICES_INVENTORY, CAP_STARTUP_INVENTORY, CAP_THERMAL_SENSORS,
    RING_ROWS,
};

use crate::diagnostics::{self, DiagnoseContext};
use crate::report;
use crate::rules::RulesEngine;
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
    /// The R2 rules engine: shared with the sampler thread (the applier runs on
    /// each tick) and with the AtlasRules service (interventions + simulation).
    engine: Arc<RulesEngine>,
    /// The sampler thread handle, so a clean shutdown can join it and guarantee
    /// the rules engine's restore-all has finished before the process exits.
    sampler: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The R2 privacy change-watcher + alert-evaluator thread handles. The
    /// watcher arms `RegNotifyChangeKeyValue` on the ConsentStore and emits
    /// transitions; the evaluator scores them against enabled alert rules and
    /// records fired alerts + privacy-event history. Both stop on the shared flag.
    privacy_watch: Mutex<Option<std::thread::JoinHandle<()>>>,
    privacy_eval: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl QueryService {
    /// The shared store handle (so the broker service can share the same
    /// connection/audit log).
    pub fn store(&self) -> SharedStore {
        self.store.clone()
    }

    /// The shared rules engine (so the AtlasRules service reads the same live
    /// intervention ledger the sampler-thread applier writes).
    pub fn rules_engine(&self) -> Arc<RulesEngine> {
        self.engine.clone()
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
        // The rules engine shares the store (rule reads + audit); it detects CPU
        // topology once here and owns the reversal ledger.
        let engine = Arc::new(RulesEngine::new(store.clone()));
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
        let thread_engine = engine.clone();
        let sampler = std::thread::Builder::new()
            .name("atlas-ipc-sampler".into())
            .spawn(move || {
                sampler_loop(thread_slot, thread_tx, thread_stop, ring, thread_engine)
            })?;

        // R2 advanced privacy alerts: the ConsentStore change-watcher feeds
        // transitions to the evaluator over a channel; the evaluator scores them
        // against enabled alert rules and records fired alerts + privacy history.
        // Both threads stop on the shared flag (flipped by `shutdown`).
        let (ptx, prx) = std::sync::mpsc::channel::<PrivacyTransition>();
        let privacy_watch = PrivacyWatcher::spawn(stop.clone(), ptx);
        let eval_store = store.clone();
        let eval_stop = stop.clone();
        let privacy_eval = std::thread::Builder::new()
            .name("atlas-privacy-eval".into())
            .spawn(move || crate::privacy_alerts::Evaluator::new(eval_store).run(prx, eval_stop))?;

        Ok(Self {
            slot,
            tx,
            stop,
            store,
            has_fts5,
            engine,
            sampler: Mutex::new(Some(sampler)),
            privacy_watch: Mutex::new(Some(privacy_watch)),
            privacy_eval: Mutex::new(Some(privacy_eval)),
        })
    }

    /// Signals the sampler thread to stop.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Joins the sampler thread, blocking until it has exited — which is *after*
    /// the rules engine's `restore_all` runs (reversibility on shutdown). Call
    /// after [`QueryService::shutdown`]. Idempotent: a second call is a no-op.
    pub fn join_sampler(&self) {
        let handle = self.sampler.lock().ok().and_then(|mut g| g.take());
        if let Some(h) = handle {
            if h.join().is_err() {
                tracing::warn!("ipc sampler thread panicked during shutdown");
            }
        }
        // Join the privacy watcher + evaluator too so their store handles are
        // released before the process exits. Both observe the same stop flag.
        for (slot, label) in [
            (&self.privacy_watch, "privacy-watch"),
            (&self.privacy_eval, "privacy-eval"),
        ] {
            let handle = slot.lock().ok().and_then(|mut g| g.take());
            if let Some(h) = handle {
                if h.join().is_err() {
                    tracing::warn!("{label} thread panicked during shutdown");
                }
            }
        }
    }
}

/// Sampler thread body: sample ~1 Hz, publish to the slot, the broadcast, and
/// (best-effort) the shared-memory live ring.
fn sampler_loop(
    slot: Slot,
    tx: tokio::sync::broadcast::Sender<SnapshotReply>,
    stop: Arc<AtomicBool>,
    ring: Option<RingWriter>,
    engine: Arc<RulesEngine>,
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
                // R2 rules engine: apply/undo policy deltas against the live
                // process set before we hand it on. The applier owns the reversal
                // ledger; it never blocks the publish path meaningfully (same-user
                // handle ops on a handful of matched processes).
                engine.apply_tick(&set);

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
    // Reversibility (PRD §3.3): on shutdown the same thread that applied
    // interventions restores every original, so nothing is left modified.
    engine.restore_all();
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
        // M8: incidents are detected on the record/writer path and served from
        // the store; diagnostics + reports are computed on demand from recorded
        // data. All three are store-backed and always available here.
        flags.push(CAP_INCIDENT_DETECTION.to_string());
        flags.push(CAP_DIAGNOSTICS.to_string());
        flags.push(CAP_REPORTS.to_string());
        // M7 inventories are live OS reads (startup/services) and store-backed
        // history (privacy). On Windows all three are always available here.
        #[cfg(windows)]
        {
            flags.push(CAP_PRIVACY_EVENTS.to_string());
            flags.push(CAP_STARTUP_INVENTORY.to_string());
            flags.push(CAP_SERVICES_INVENTORY.to_string());
            // R2: the on-demand deep inspector + resource-ownership search are
            // live OS reads (no store), always available on Windows here.
            flags.push(CAP_PROCESS_INSPECTOR.to_string());
            flags.push(CAP_RESOURCE_OWNERSHIP.to_string());
            // R2: the performance rules engine + profiles (AtlasRules). Store-
            // backed persistence + audit; the applier runs on the sampler thread.
            flags.push(CAP_RULES_ENGINE.to_string());
            flags.push(CAP_PROFILES.to_string());
            // R3: dynamic responsiveness protection (the watchdog runs on the
            // sampler tick, store-backed config, disabled by default).
            flags.push(CAP_DYNAMIC_PROTECTION.to_string());
            // R2: advanced privacy alerts — the ConsentStore change-watcher +
            // evaluator run on their own threads; rule CRUD + fired-alert history
            // are store-backed and always available on Windows here.
            flags.push(CAP_PRIVACY_ALERTS.to_string());
            // R2 monitors. Network inspection and scheduled-task enumeration are
            // supported live reads (always advertised). The three hardware/log-
            // dependent monitors are advertised only when they actually resolve
            // on this machine — a battery is present, a thermal sensor is
            // exposed, or the boot-performance log is readable — so a UI hides
            // what the box cannot provide (degraded-mode propagation, §5). The
            // probes block (syscalls / WMI / event log), so they run off the
            // runtime on a blocking thread.
            flags.push(CAP_NETWORK_INSPECTOR.to_string());
            flags.push(CAP_SCHEDULED_TASKS.to_string());
            let (has_battery, has_thermal, has_boots) = tokio::task::spawn_blocking(|| {
                (
                    battery_status().present,
                    thermal_status().available,
                    analyze_boots(1).available,
                )
            })
            .await
            .unwrap_or((false, false, false));
            if has_battery {
                flags.push(CAP_BATTERY_STATUS.to_string());
            }
            if has_thermal {
                flags.push(CAP_THERMAL_SENSORS.to_string());
            }
            if has_boots {
                flags.push(CAP_BOOT_ANALYSIS.to_string());
            }
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

    // -----------------------------------------------------------------------
    // R2: advanced privacy alerts (PRD §9.10.3). Alert-rule CRUD + fired-alert
    // history, all store-backed. The ConsentStore change-watcher + evaluator
    // (background threads) produce the fired alerts these read back.
    // -----------------------------------------------------------------------

    async fn list_privacy_alert_rules(
        &self,
        _req: Request<ListPrivacyAlertRulesRequest>,
    ) -> Result<Response<ListPrivacyAlertRulesReply>, Status> {
        let store = self.store.lock().map_err(|_| poisoned())?;
        let rows = store
            .list_privacy_alert_rules()
            .map_err(|e| Status::internal(format!("list_privacy_alert_rules: {e}")))?;
        Ok(Response::new(ListPrivacyAlertRulesReply {
            rules: rows.iter().map(alert_rule_row_to_proto).collect(),
        }))
    }

    async fn create_privacy_alert_rule(
        &self,
        req: Request<CreatePrivacyAlertRuleRequest>,
    ) -> Result<Response<CreatePrivacyAlertRuleReply>, Status> {
        let rule = req
            .into_inner()
            .rule
            .ok_or_else(|| Status::invalid_argument("rule is required"))?;
        let row = alert_rule_proto_to_row(&rule);
        let store = self.store.lock().map_err(|_| poisoned())?;
        let id = store
            .create_privacy_alert_rule(&row)
            .map_err(|e| Status::internal(format!("create_privacy_alert_rule: {e}")))?;
        Ok(Response::new(CreatePrivacyAlertRuleReply { id }))
    }

    async fn update_privacy_alert_rule(
        &self,
        req: Request<UpdatePrivacyAlertRuleRequest>,
    ) -> Result<Response<UpdatePrivacyAlertRuleReply>, Status> {
        let rule = req
            .into_inner()
            .rule
            .ok_or_else(|| Status::invalid_argument("rule is required"))?;
        let row = alert_rule_proto_to_row(&rule);
        let store = self.store.lock().map_err(|_| poisoned())?;
        let ok = store
            .update_privacy_alert_rule(&row)
            .map_err(|e| Status::internal(format!("update_privacy_alert_rule: {e}")))?;
        Ok(Response::new(UpdatePrivacyAlertRuleReply { ok }))
    }

    async fn delete_privacy_alert_rule(
        &self,
        req: Request<DeletePrivacyAlertRuleRequest>,
    ) -> Result<Response<DeletePrivacyAlertRuleReply>, Status> {
        let id = req.into_inner().id;
        let store = self.store.lock().map_err(|_| poisoned())?;
        let ok = store
            .delete_privacy_alert_rule(id)
            .map_err(|e| Status::internal(format!("delete_privacy_alert_rule: {e}")))?;
        Ok(Response::new(DeletePrivacyAlertRuleReply { ok }))
    }

    async fn list_fired_alerts(
        &self,
        req: Request<ListFiredAlertsRequest>,
    ) -> Result<Response<ListFiredAlertsReply>, Status> {
        let r = req.into_inner();
        let (from_ms, to_ms) = range_bounds(&r.range);
        let limit = if r.limit == 0 { 1000 } else { r.limit };
        let store = self.store.lock().map_err(|_| poisoned())?;
        let (rows, truncated) = store
            .list_fired_alerts(from_ms, to_ms, limit)
            .map_err(|e| Status::internal(format!("list_fired_alerts: {e}")))?;
        Ok(Response::new(ListFiredAlertsReply {
            alerts: rows.iter().map(fired_alert_row_to_proto).collect(),
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

    async fn list_incidents(
        &self,
        req: Request<ListIncidentsRequest>,
    ) -> Result<Response<ListIncidentsReply>, Status> {
        // Pure read of the incidents the record/writer path detected. Detection
        // itself runs where the samples are written, not on the query path.
        let r = req.into_inner();
        let (from_ms, to_ms) = range_bounds(&r.range);
        let limit = if r.limit == 0 { 100 } else { r.limit };
        let store = self.store.lock().map_err(|_| poisoned())?;
        let (rows, truncated) = store
            .list_incidents(from_ms, to_ms, limit)
            .map_err(|e| Status::internal(format!("list_incidents: {e}")))?;
        Ok(Response::new(ListIncidentsReply {
            incidents: rows.iter().map(incident_row_to_proto).collect(),
            truncated,
        }))
    }

    async fn diagnose(
        &self,
        req: Request<DiagnoseRequest>,
    ) -> Result<Response<DiagnoseReply>, Status> {
        let r = req.into_inner();
        let now = now_ms();
        let mem_total = self.slot_mem_total();
        let store = self.store.lock().map_err(|_| poisoned())?;
        let reply = self
            .diagnose_inner(&store, r.incident_id, &r.range, now, mem_total)
            .map_err(|e| Status::internal(format!("diagnose: {e}")))?;
        Ok(Response::new(reply))
    }

    async fn generate_report(
        &self,
        req: Request<GenerateReportRequest>,
    ) -> Result<Response<GenerateReportReply>, Status> {
        let r = req.into_inner();
        let now = now_ms();
        let mem_total = self.slot_mem_total();
        let format = ReportFormat::try_from(r.format).unwrap_or(ReportFormat::ReportText);
        let redaction = r.redaction.unwrap_or_default();
        let store = self.store.lock().map_err(|_| poisoned())?;
        let (incident, reply) = self
            .resolve_and_diagnose(&store, r.incident_id, &r.range, now, mem_total)
            .map_err(|e| Status::internal(format!("generate_report: {e}")))?;
        let (content, content_type) = report::render_report(&incident, &reply, format, &redaction);
        Ok(Response::new(GenerateReportReply {
            content,
            content_type,
        }))
    }

    // -----------------------------------------------------------------------
    // R2: deep process inspector + resource ownership (PRD §9.4/§9.5). All
    // unary, on-demand, read-only live OS reads. The collectors block (snapshot
    // syscalls, WinVerifyTrust disk hashing, the handle-name worker threads), so
    // each runs on a blocking thread to keep the async runtime responsive.
    // Coverage flags (limited / names_limited / available) pass straight through.
    // -----------------------------------------------------------------------

    async fn get_process_detail(
        &self,
        req: Request<ProcessDetailRequest>,
    ) -> Result<Response<ProcessDetailReply>, Status> {
        let r = req.into_inner();
        let res = tokio::task::spawn_blocking(move || {
            atlas_collectors::process_detail(r.pid, r.create_time_100ns)
        })
        .await
        .map_err(|e| Status::internal(format!("process_detail task: {e}")))?;
        Ok(Response::new(ProcessDetailReply {
            available: res.available,
            unavailable_reason: res.unavailable_reason,
            detail: res.detail.map(process_detail_to_proto),
        }))
    }

    async fn list_handles(
        &self,
        req: Request<ListHandlesRequest>,
    ) -> Result<Response<ListHandlesReply>, Status> {
        let r = req.into_inner();
        let res = tokio::task::spawn_blocking(move || {
            atlas_collectors::list_handles(r.pid, &r.type_filter, r.limit)
        })
        .await
        .map_err(|e| Status::internal(format!("list_handles task: {e}")))?;
        Ok(Response::new(ListHandlesReply {
            handles: res
                .handles
                .into_iter()
                .map(|h| HandleRow {
                    handle: h.handle,
                    r#type: h.type_name,
                    name: h.name,
                    granted_access: h.granted_access,
                })
                .collect(),
            truncated: res.truncated,
            names_limited: res.names_limited,
        }))
    }

    async fn list_modules(
        &self,
        req: Request<ListModulesRequest>,
    ) -> Result<Response<ListModulesReply>, Status> {
        let r = req.into_inner();
        let res = tokio::task::spawn_blocking(move || atlas_collectors::list_modules(r.pid))
            .await
            .map_err(|e| Status::internal(format!("list_modules task: {e}")))?;
        Ok(Response::new(ListModulesReply {
            available: res.available,
            unavailable_reason: res.unavailable_reason,
            modules: res
                .modules
                .into_iter()
                .map(|m| ModuleRow {
                    name: m.name,
                    path: m.path,
                    base_address: m.base_address,
                    size: m.size,
                    version: m.version,
                    publisher: m.publisher,
                    signed: m.signed,
                })
                .collect(),
        }))
    }

    async fn list_threads(
        &self,
        req: Request<ListThreadsRequest>,
    ) -> Result<Response<ListThreadsReply>, Status> {
        let r = req.into_inner();
        let threads = tokio::task::spawn_blocking(move || atlas_collectors::list_threads(r.pid))
            .await
            .map_err(|e| Status::internal(format!("list_threads task: {e}")))?;
        Ok(Response::new(ListThreadsReply {
            threads: threads
                .into_iter()
                .map(|t| ThreadRow {
                    tid: t.tid,
                    start_address: t.start_address,
                    state: t.state,
                    wait_reason: t.wait_reason,
                    priority: t.priority,
                    cpu_permille: t.cpu_permille,
                    user_time_100ns: t.user_time_100ns,
                    kernel_time_100ns: t.kernel_time_100ns,
                    context_switches: t.context_switches,
                })
                .collect(),
        }))
    }

    async fn find_resource_owners(
        &self,
        req: Request<FindResourceOwnersRequest>,
    ) -> Result<Response<FindResourceOwnersReply>, Status> {
        let r = req.into_inner();
        let res =
            tokio::task::spawn_blocking(move || atlas_collectors::find_resource_owners(&r.path))
                .await
                .map_err(|e| Status::internal(format!("find_resource_owners task: {e}")))?;
        Ok(Response::new(FindResourceOwnersReply {
            available: res.available,
            unavailable_reason: res.unavailable_reason,
            owners: res
                .owners
                .into_iter()
                .map(|o| ProtoResourceOwner {
                    pid: o.pid,
                    image_name: o.image_name,
                    image_path: o.image_path,
                    description: o.description,
                    is_service: o.is_service,
                })
                .collect(),
        }))
    }

    // -----------------------------------------------------------------------
    // R2 monitors (PRD §9.12, §9.9.2, §9.8.4, §9.6.6/§9.6.7). All read-only
    // live OS reads that block (extended-table syscalls, DNS-cache queries,
    // Task Scheduler COM, the event-log query, battery IOCTLs, WMI), so each
    // runs on a blocking thread to keep the async runtime responsive. The
    // hardware/log-dependent replies carry available + unavailable_reason so
    // absent sensors degrade honestly rather than returning fabricated data.
    // -----------------------------------------------------------------------

    async fn list_connections(
        &self,
        req: Request<ListConnectionsRequest>,
    ) -> Result<Response<ListConnectionsReply>, Status> {
        let include_listening = req.into_inner().include_listening;
        let conns = tokio::task::spawn_blocking(move || list_connections_impl(include_listening))
            .await
            .map_err(|e| Status::internal(format!("list_connections task: {e}")))?;
        Ok(Response::new(ListConnectionsReply { connections: conns }))
    }

    async fn list_listening_ports(
        &self,
        _req: Request<ListListeningPortsRequest>,
    ) -> Result<Response<ListListeningPortsReply>, Status> {
        let ports = tokio::task::spawn_blocking(list_listening_ports_impl)
            .await
            .map_err(|e| Status::internal(format!("list_listening_ports task: {e}")))?;
        Ok(Response::new(ListListeningPortsReply { ports }))
    }

    async fn list_scheduled_tasks(
        &self,
        req: Request<ListScheduledTasksRequest>,
    ) -> Result<Response<ListScheduledTasksReply>, Status> {
        let filter = req.into_inner().filter;
        let tasks = tokio::task::spawn_blocking(move || list_scheduled_tasks_impl(&filter))
            .await
            .map_err(|e| Status::internal(format!("list_scheduled_tasks task: {e}")))?;
        Ok(Response::new(ListScheduledTasksReply { tasks }))
    }

    async fn list_boots(
        &self,
        req: Request<ListBootsRequest>,
    ) -> Result<Response<ListBootsReply>, Status> {
        let limit = req.into_inner().limit;
        let reply = tokio::task::spawn_blocking(move || list_boots_impl(limit))
            .await
            .map_err(|e| Status::internal(format!("list_boots task: {e}")))?;
        Ok(Response::new(reply))
    }

    async fn get_battery_status(
        &self,
        _req: Request<GetBatteryStatusRequest>,
    ) -> Result<Response<GetBatteryStatusReply>, Status> {
        let reply = tokio::task::spawn_blocking(get_battery_status_impl)
            .await
            .map_err(|e| Status::internal(format!("get_battery_status task: {e}")))?;
        Ok(Response::new(reply))
    }

    async fn get_thermal(
        &self,
        _req: Request<GetThermalRequest>,
    ) -> Result<Response<GetThermalReply>, Status> {
        let reply = tokio::task::spawn_blocking(get_thermal_impl)
            .await
            .map_err(|e| Status::internal(format!("get_thermal task: {e}")))?;
        Ok(Response::new(reply))
    }

    // R3 system-change tracking (PRD §9.13) + crash correlation (PRD §9.14) are
    // separate R3 workstreams. The frozen contract (be67835) declares their RPCs;
    // this build does not yet implement the collectors, so both answer honestly
    // in degraded mode — empty / not-available — until that work lands. No
    // capability flag is advertised for either, so a UI hides them.
    async fn list_system_changes(
        &self,
        _req: Request<atlas_ipc::ListSystemChangesRequest>,
    ) -> Result<Response<atlas_ipc::ListSystemChangesReply>, Status> {
        Ok(Response::new(atlas_ipc::ListSystemChangesReply {
            changes: vec![],
            truncated: false,
        }))
    }

    async fn list_crashes(
        &self,
        _req: Request<atlas_ipc::ListCrashesRequest>,
    ) -> Result<Response<atlas_ipc::ListCrashesReply>, Status> {
        Ok(Response::new(atlas_ipc::ListCrashesReply {
            available: false,
            unavailable_reason: "crash correlation is not implemented in this build (R3)"
                .to_string(),
            crashes: vec![],
            truncated: false,
        }))
    }
}

/// Maps a collector [`atlas_collectors::ProcessDetail`] to the proto message
/// (field-for-field; the coverage `limited` flag rides along).
fn process_detail_to_proto(d: atlas_collectors::ProcessDetail) -> ProtoProcessDetail {
    ProtoProcessDetail {
        pid: d.pid,
        parent_pid: d.parent_pid,
        create_time_100ns: d.create_time_100ns,
        image_name: d.image_name,
        image_path: d.image_path,
        command_line: d.command_line,
        working_directory: d.working_directory,
        user_sid: d.user_sid,
        user_name: d.user_name,
        session_id: d.session_id,
        integrity_level: d.integrity_level,
        elevated: d.elevated,
        architecture: d.architecture,
        signature_status: d.signature_status,
        publisher: d.publisher,
        file_version: d.file_version,
        product_name: d.product_name,
        thread_count: d.thread_count,
        handle_count: d.handle_count,
        start_time_ms: d.start_time_ms,
        package_identity: d.package_identity,
        limited: d.limited,
    }
}

impl QueryService {
    /// Total physical memory (bytes) from the latest published snapshot, for the
    /// memory-pressure percent-of-total threshold. 0 until the first sample or if
    /// gauges are absent (memory diagnosis then degrades to CPU only).
    fn slot_mem_total(&self) -> u64 {
        self.slot
            .read()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .and_then(|r| r.system.as_ref().map(|s| s.mem_total))
            })
            .unwrap_or(0)
    }

    /// Resolves a `DiagnoseRequest`/`GenerateReportRequest` target — a detected
    /// incident by id, or the ad-hoc range — into a `Diagnosis`.
    fn diagnose_inner(
        &self,
        store: &Store,
        incident_id: i64,
        range: &Option<TimeRange>,
        now: i64,
        mem_total: u64,
    ) -> anyhow::Result<DiagnoseReply> {
        Ok(self
            .resolve_and_diagnose(store, incident_id, range, now, mem_total)?
            .1)
    }

    /// Shared resolution for diagnose + report: returns the proto incident (a
    /// real row, or a synthetic ad-hoc incident) alongside its diagnosis.
    fn resolve_and_diagnose(
        &self,
        store: &Store,
        incident_id: i64,
        range: &Option<TimeRange>,
        now: i64,
        mem_total: u64,
    ) -> anyhow::Result<(Incident, DiagnoseReply)> {
        if incident_id != 0 {
            match store.get_incident(incident_id)? {
                Some(row) => {
                    let ctx = DiagnoseContext {
                        kind: row.kind,
                        start_ms: row.start_ms,
                        end_ms: row.end_ms.unwrap_or(0),
                        peak_value: row.peak_value,
                    };
                    let reply = diagnostics::diagnose(store, &ctx, now, mem_total)?;
                    Ok((incident_row_to_proto(&row), reply))
                }
                None => {
                    let reply = DiagnoseReply {
                        available: false,
                        unavailable_reason: format!("no incident #{incident_id}"),
                        diagnosis: None,
                    };
                    Ok((Incident::default(), reply))
                }
            }
        } else {
            // Ad-hoc range: a missing range degenerates to the last 10 minutes.
            let (from, to) = match range {
                Some(tr) if tr.to_ms > tr.from_ms => (tr.from_ms, tr.to_ms),
                _ => (now - 10 * 60_000, now),
            };
            let ctx = DiagnoseContext {
                kind: 0,
                start_ms: from,
                end_ms: to,
                peak_value: 0.0,
            };
            let reply = diagnostics::diagnose(store, &ctx, now, mem_total)?;
            let inc = Incident {
                id: 0,
                kind: 0,
                start_ms: from,
                end_ms: to,
                severity: 0,
                peak_value: 0.0,
                summary: "Ad-hoc range diagnosis".to_string(),
            };
            Ok((inc, reply))
        }
    }
}

/// Converts a store incident row to the proto `Incident` (0 end = ongoing).
fn incident_row_to_proto(r: &atlas_store::IncidentRow) -> Incident {
    Incident {
        id: r.id,
        kind: r.kind,
        start_ms: r.start_ms,
        end_ms: r.end_ms.unwrap_or(0),
        severity: r.severity,
        peak_value: r.peak_value,
        summary: r.summary.clone(),
    }
}

/// Wall-clock Unix-epoch milliseconds (for closing an open diagnosis window).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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

/// Maps a store [`atlas_store::PrivacyAlertRuleRow`] to the proto
/// [`PrivacyAlertRule`]. The store keeps the capability/condition as their proto
/// discriminants, so the mapping is a straight field copy.
fn alert_rule_row_to_proto(r: &atlas_store::PrivacyAlertRuleRow) -> PrivacyAlertRule {
    PrivacyAlertRule {
        id: r.id,
        name: r.name.clone(),
        enabled: r.enabled,
        capability: r.capability,
        condition: r.condition,
        threshold_seconds: r.threshold_seconds,
        created_ms: r.created_ms,
    }
}

/// Maps a proto [`PrivacyAlertRule`] to a store row (id/created_ms honored as
/// given; create/update stamp them as needed).
fn alert_rule_proto_to_row(r: &PrivacyAlertRule) -> atlas_store::PrivacyAlertRuleRow {
    atlas_store::PrivacyAlertRuleRow {
        id: r.id,
        name: r.name.clone(),
        enabled: r.enabled,
        capability: r.capability,
        condition: r.condition,
        threshold_seconds: r.threshold_seconds,
        created_ms: r.created_ms,
    }
}

/// Maps a store [`atlas_store::FiredAlertRow`] to the proto [`FiredAlert`].
fn fired_alert_row_to_proto(a: &atlas_store::FiredAlertRow) -> FiredAlert {
    FiredAlert {
        id: a.id,
        rule_id: a.rule_id,
        rule_name: a.rule_name.clone(),
        ts_ms: a.ts_ms,
        capability: a.capability,
        app_id: a.app_id.clone(),
        display_name: a.display_name.clone(),
        detail: a.detail.clone(),
    }
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

// ---------------------------------------------------------------------------
// R2 monitor collector → proto mapping (Windows). Non-Windows stubs keep the
// crate building on other targets (the RPCs are unreachable there — the pipe
// transport is Windows-only).
// ---------------------------------------------------------------------------

/// The proto `L4Protocol` discriminant for a collector protocol.
#[cfg(windows)]
fn net_protocol_to_proto(p: NetL4Protocol) -> i32 {
    let v = match p {
        NetL4Protocol::Tcp => L4Protocol::Tcp,
        NetL4Protocol::Udp => L4Protocol::Udp,
    };
    v as i32
}

/// The proto `TcpState` discriminant for a collector TCP state.
#[cfg(windows)]
fn net_state_to_proto(s: NetTcpState) -> i32 {
    let v = match s {
        NetTcpState::Unspecified => TcpState::Unspecified,
        NetTcpState::Closed => TcpState::TcpClosed,
        NetTcpState::Listen => TcpState::TcpListen,
        NetTcpState::SynSent => TcpState::TcpSynSent,
        NetTcpState::SynRcvd => TcpState::TcpSynRcvd,
        NetTcpState::Established => TcpState::TcpEstablished,
        NetTcpState::FinWait1 => TcpState::TcpFinWait1,
        NetTcpState::FinWait2 => TcpState::TcpFinWait2,
        NetTcpState::CloseWait => TcpState::TcpCloseWait,
        NetTcpState::Closing => TcpState::TcpClosing,
        NetTcpState::LastAck => TcpState::TcpLastAck,
        NetTcpState::TimeWait => TcpState::TcpTimeWait,
        NetTcpState::DeleteTcb => TcpState::TcpDeleteTcb,
    };
    v as i32
}

#[cfg(windows)]
fn connection_to_proto(c: CollectorConnection) -> ProtoConnection {
    ProtoConnection {
        pid: c.pid,
        image_name: c.image_name,
        protocol: net_protocol_to_proto(c.protocol),
        local_addr: c.local_addr,
        local_port: c.local_port as u32,
        remote_addr: c.remote_addr,
        remote_port: c.remote_port as u32,
        remote_domain: c.remote_domain,
        state: net_state_to_proto(c.state),
        is_ipv6: c.is_ipv6,
    }
}

#[cfg(windows)]
fn listening_port_to_proto(p: CollectorListeningPort) -> ProtoListeningPort {
    ProtoListeningPort {
        protocol: net_protocol_to_proto(p.protocol),
        bind_addr: p.bind_addr,
        port: p.port as u32,
        pid: p.pid,
        image_name: p.image_name,
        is_ipv6: p.is_ipv6,
    }
}

#[cfg(windows)]
fn list_connections_impl(include_listening: bool) -> Vec<ProtoConnection> {
    list_connections(include_listening)
        .into_iter()
        .map(connection_to_proto)
        .collect()
}

#[cfg(not(windows))]
fn list_connections_impl(_include_listening: bool) -> Vec<ProtoConnection> {
    Vec::new()
}

#[cfg(windows)]
fn list_listening_ports_impl() -> Vec<ProtoListeningPort> {
    list_listening_ports()
        .into_iter()
        .map(listening_port_to_proto)
        .collect()
}

#[cfg(not(windows))]
fn list_listening_ports_impl() -> Vec<ProtoListeningPort> {
    Vec::new()
}

#[cfg(windows)]
fn task_to_proto(t: CollectorScheduledTask) -> ProtoScheduledTask {
    ProtoScheduledTask {
        name: t.name,
        path: t.path,
        folder: t.folder,
        enabled: t.enabled,
        triggers: t.triggers,
        action: t.action,
        last_run_ms: t.last_run_ms,
        next_run_ms: t.next_run_ms,
        last_result: t.last_result,
        author: t.author,
        run_as_highest: t.run_as_highest,
        runs_on_idle: t.runs_on_idle,
        wakes_to_run: t.wakes_to_run,
    }
}

#[cfg(windows)]
fn list_scheduled_tasks_impl(filter: &str) -> Vec<ProtoScheduledTask> {
    enumerate_tasks(filter)
        .into_iter()
        .map(task_to_proto)
        .collect()
}

#[cfg(not(windows))]
fn list_scheduled_tasks_impl(_filter: &str) -> Vec<ProtoScheduledTask> {
    Vec::new()
}

#[cfg(windows)]
fn boot_to_proto(b: CollectorBootRecord) -> ProtoBootRecord {
    ProtoBootRecord {
        boot_ms: b.boot_ms,
        boot_duration_ms: b.boot_duration_ms,
        main_path_ms: b.main_path_ms,
        post_boot_ms: b.post_boot_ms,
        degraded: b.degraded,
    }
}

#[cfg(windows)]
fn list_boots_impl(limit: u32) -> ListBootsReply {
    let a = analyze_boots(limit);
    ListBootsReply {
        available: a.available,
        unavailable_reason: a.unavailable_reason,
        boots: a.boots.into_iter().map(boot_to_proto).collect(),
    }
}

#[cfg(not(windows))]
fn list_boots_impl(_limit: u32) -> ListBootsReply {
    ListBootsReply {
        available: false,
        unavailable_reason: "boot analysis is Windows-only".to_string(),
        boots: Vec::new(),
    }
}

#[cfg(windows)]
fn battery_to_proto(r: BatteryReading) -> GetBatteryStatusReply {
    GetBatteryStatusReply {
        available: r.available,
        unavailable_reason: r.unavailable_reason,
        status: if r.available {
            Some(ProtoBatteryStatus {
                present: r.present,
                charging: r.charging,
                on_ac: r.on_ac,
                percent: r.percent,
                rate_mw: r.rate_mw,
                remaining_mwh: r.remaining_mwh,
                full_charge_mwh: r.full_charge_mwh,
                design_mwh: r.design_mwh,
                health_percent: r.health_percent,
                cycle_count: r.cycle_count,
                est_runtime_s: r.est_runtime_s,
            })
        } else {
            None
        },
    }
}

#[cfg(windows)]
fn get_battery_status_impl() -> GetBatteryStatusReply {
    battery_to_proto(battery_status())
}

#[cfg(not(windows))]
fn get_battery_status_impl() -> GetBatteryStatusReply {
    GetBatteryStatusReply {
        available: false,
        unavailable_reason: "battery status is Windows-only".to_string(),
        status: None,
    }
}

#[cfg(windows)]
fn thermal_to_proto(r: ThermalReading) -> GetThermalReply {
    GetThermalReply {
        available: r.available,
        unavailable_reason: r.unavailable_reason,
        sensors: r
            .sensors
            .into_iter()
            .map(|s| ProtoThermalSensor {
                name: s.name,
                celsius: s.celsius,
                source: s.source,
            })
            .collect(),
    }
}

#[cfg(windows)]
fn get_thermal_impl() -> GetThermalReply {
    thermal_to_proto(thermal_status())
}

#[cfg(not(windows))]
fn get_thermal_impl() -> GetThermalReply {
    GetThermalReply {
        available: false,
        unavailable_reason: "thermal sensors are Windows-only".to_string(),
        sensors: Vec::new(),
    }
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
