//! End-to-end IPC round-trip over a real Windows named pipe (docs/phases.md
//! M4). Starts the tonic server on a unique pipe name, connects the client,
//! and asserts GetCapabilities + GetSnapshot come back correctly. Runs
//! unprivileged — the pipe DACL grants the current user access.

#![cfg(windows)]

use tonic::{Request, Response, Status};

use atlas_ipc::v0::atlas_query_server::{AtlasQuery, AtlasQueryServer};
use atlas_ipc::{
    CapabilitiesReply, CapabilitiesRequest, ProcessRow, SnapshotReply, SnapshotRequest,
    SystemGauges, CAP_PROCESS_SNAPSHOTS,
};

/// Minimal fixed-response service so the test exercises the transport, not the
/// sampler. Returns three synthetic process rows and honors `top_n`.
#[derive(Default)]
struct FakeQuery;

fn fake_rows() -> Vec<ProcessRow> {
    (0..3)
        .map(|i| ProcessRow {
            pid: 100 + i,
            parent_pid: 4,
            image_name: format!("proc{i}.exe"),
            session_id: 1,
            create_time_100ns: 0,
            // Descending CPU so we can assert ordering survives.
            cpu_permille: 300 - i * 100,
            working_set: 1024,
            private_bytes: 512,
            read_bps: 0,
            write_bps: 0,
            handle_count: 10,
            thread_count: 5,
        })
        .collect()
}

#[tonic::async_trait]
impl AtlasQuery for FakeQuery {
    async fn get_capabilities(
        &self,
        _req: Request<CapabilitiesRequest>,
    ) -> Result<Response<CapabilitiesReply>, Status> {
        Ok(Response::new(CapabilitiesReply {
            service_version: "test-1.2.3".into(),
            capability_flags: vec![CAP_PROCESS_SNAPSHOTS.into()],
        }))
    }

    async fn get_snapshot(
        &self,
        req: Request<SnapshotRequest>,
    ) -> Result<Response<SnapshotReply>, Status> {
        let top_n = req.into_inner().top_n as usize;
        let mut processes = fake_rows();
        if top_n != 0 && top_n < processes.len() {
            processes.truncate(top_n);
        }
        Ok(Response::new(SnapshotReply {
            system: Some(SystemGauges {
                ts_ms: 42,
                cpu_permille: 250,
                mem_used: 8,
                mem_total: 16,
                commit_used: 4,
                commit_limit: 32,
                process_count: processes.len() as u32,
                thread_count: 0,
                handle_count: 0,
            }),
            processes,
        }))
    }

    type StreamSnapshotsStream =
        tokio_stream::wrappers::ReceiverStream<Result<SnapshotReply, Status>>;

    async fn stream_snapshots(
        &self,
        _req: Request<SnapshotRequest>,
    ) -> Result<Response<Self::StreamSnapshotsStream>, Status> {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let _ = tx
            .send(Ok(SnapshotReply {
                system: None,
                processes: fake_rows(),
            }))
            .await;
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_and_snapshot_round_trip() {
    // Unique pipe per test run so parallel/repeat runs never collide.
    let who = format!("test.{}.{}", std::process::id(), fastish_token());
    let name = atlas_ipc::pipe_name(&who);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let router = tonic::transport::Server::builder().add_service(AtlasQueryServer::new(FakeQuery));
    let server_name = name.clone();
    let server = tokio::spawn(async move {
        atlas_ipc::serve(&server_name, router, async {
            let _ = shutdown_rx.await;
        })
        .await
    });

    // Connect (dial retries while the first instance comes up).
    let channel = atlas_ipc::connect(&name)
        .await
        .expect("client connects to pipe");
    let mut client = atlas_ipc::AtlasQueryClient::new(channel);

    let caps = client
        .get_capabilities(CapabilitiesRequest {})
        .await
        .expect("GetCapabilities")
        .into_inner();
    assert_eq!(caps.service_version, "test-1.2.3");
    assert!(caps
        .capability_flags
        .iter()
        .any(|f| f == CAP_PROCESS_SNAPSHOTS));

    // top_n=2 must truncate the 3 synthetic rows.
    let snap = client
        .get_snapshot(SnapshotRequest { top_n: 2 })
        .await
        .expect("GetSnapshot")
        .into_inner();
    assert_eq!(snap.processes.len(), 2);
    assert_eq!(snap.processes[0].pid, 100);
    assert!(snap.system.is_some());

    // top_n=0 returns all.
    let all = client
        .get_snapshot(SnapshotRequest { top_n: 0 })
        .await
        .expect("GetSnapshot all")
        .into_inner();
    assert_eq!(all.processes.len(), 3);

    // Shut the server down cleanly.
    let _ = shutdown_tx.send(());
    let _ = server.await;
}

/// Cheap per-run token without pulling in rand: mix the current time.
fn fastish_token() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
