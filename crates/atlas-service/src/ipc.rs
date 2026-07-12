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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tonic::{Request, Response, Status};

use atlas_collectors::{SampleSet, Sampler};
use atlas_ipc::v0::atlas_query_server::AtlasQuery;
use atlas_ipc::{
    CapabilitiesReply, CapabilitiesRequest, ProcessRow, SnapshotReply, SnapshotRequest,
    SystemGauges, CAP_PROCESS_SNAPSHOTS,
};

/// Latest published snapshot, already converted to the wire shape. `None`
/// until the first sample lands.
type Slot = Arc<RwLock<Option<SnapshotReply>>>;

/// Converts a collector [`SampleSet`] into the proto [`SnapshotReply`], sorted
/// by CPU descending (working set as a tiebreak, matching the console `top`).
fn to_reply(set: &SampleSet) -> SnapshotReply {
    let s = &set.system;
    let mut processes: Vec<ProcessRow> = set
        .processes
        .iter()
        .map(|p| ProcessRow {
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
/// and the stop flag for the sampler thread.
pub struct QueryService {
    slot: Slot,
    tx: tokio::sync::broadcast::Sender<SnapshotReply>,
    stop: Arc<AtomicBool>,
}

impl QueryService {
    /// Spawns the sampler thread and returns the service handle. The thread
    /// runs until [`QueryService::shutdown`] flips the stop flag (or the
    /// process exits).
    pub fn start() -> anyhow::Result<Self> {
        let slot: Slot = Arc::new(RwLock::new(None));
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let stop = Arc::new(AtomicBool::new(false));

        let thread_slot = slot.clone();
        let thread_tx = tx.clone();
        let thread_stop = stop.clone();
        std::thread::Builder::new()
            .name("atlas-ipc-sampler".into())
            .spawn(move || sampler_loop(thread_slot, thread_tx, thread_stop))?;

        Ok(Self { slot, tx, stop })
    }

    /// Signals the sampler thread to stop.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Sampler thread body: sample ~1 Hz, publish to the slot and broadcast.
fn sampler_loop(
    slot: Slot,
    tx: tokio::sync::broadcast::Sender<SnapshotReply>,
    stop: Arc<AtomicBool>,
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

#[tonic::async_trait]
impl AtlasQuery for QueryService {
    async fn get_capabilities(
        &self,
        _req: Request<CapabilitiesRequest>,
    ) -> Result<Response<CapabilitiesReply>, Status> {
        Ok(Response::new(CapabilitiesReply {
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            // M4: process snapshots always available. ETW / sensor flags land
            // in later milestones (degraded-mode propagation, tech-stack §5).
            capability_flags: vec![CAP_PROCESS_SNAPSHOTS.to_string()],
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
}
