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
            app_group: format!("app:proc{i}#{}", 100 + i),
            role: 1, // MAIN
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

    // The M6 history/bookmark RPCs are not exercised by this transport test;
    // return empty replies so the fake satisfies the (frozen) trait surface.
    async fn query_range(
        &self,
        _req: Request<atlas_ipc::QueryRangeRequest>,
    ) -> Result<Response<atlas_ipc::QueryRangeReply>, Status> {
        Ok(Response::new(atlas_ipc::QueryRangeReply {
            buckets: vec![],
        }))
    }

    async fn list_events(
        &self,
        _req: Request<atlas_ipc::ListEventsRequest>,
    ) -> Result<Response<atlas_ipc::ListEventsReply>, Status> {
        Ok(Response::new(atlas_ipc::ListEventsReply {
            events: vec![],
            truncated: false,
        }))
    }

    async fn search(
        &self,
        _req: Request<atlas_ipc::SearchRequest>,
    ) -> Result<Response<atlas_ipc::SearchReply>, Status> {
        Ok(Response::new(atlas_ipc::SearchReply { hits: vec![] }))
    }

    async fn create_bookmark(
        &self,
        _req: Request<atlas_ipc::CreateBookmarkRequest>,
    ) -> Result<Response<atlas_ipc::CreateBookmarkReply>, Status> {
        Ok(Response::new(atlas_ipc::CreateBookmarkReply { id: 1 }))
    }

    async fn list_bookmarks(
        &self,
        _req: Request<atlas_ipc::ListBookmarksRequest>,
    ) -> Result<Response<atlas_ipc::ListBookmarksReply>, Status> {
        Ok(Response::new(atlas_ipc::ListBookmarksReply {
            bookmarks: vec![],
        }))
    }

    // The M7 privacy/startup/services RPCs are not exercised by this transport
    // test; return empty replies so the fake satisfies the (frozen) trait.
    async fn list_privacy_usage(
        &self,
        _req: Request<atlas_ipc::ListPrivacyUsageRequest>,
    ) -> Result<Response<atlas_ipc::ListPrivacyUsageReply>, Status> {
        Ok(Response::new(atlas_ipc::ListPrivacyUsageReply {
            usages: vec![],
        }))
    }

    async fn list_privacy_events(
        &self,
        _req: Request<atlas_ipc::ListPrivacyEventsRequest>,
    ) -> Result<Response<atlas_ipc::ListPrivacyEventsReply>, Status> {
        Ok(Response::new(atlas_ipc::ListPrivacyEventsReply {
            events: vec![],
            truncated: false,
        }))
    }

    async fn list_startup(
        &self,
        _req: Request<atlas_ipc::ListStartupRequest>,
    ) -> Result<Response<atlas_ipc::ListStartupReply>, Status> {
        Ok(Response::new(atlas_ipc::ListStartupReply {
            entries: vec![],
        }))
    }

    async fn list_services(
        &self,
        _req: Request<atlas_ipc::ListServicesRequest>,
    ) -> Result<Response<atlas_ipc::ListServicesReply>, Status> {
        Ok(Response::new(atlas_ipc::ListServicesReply {
            services: vec![],
        }))
    }

    // M8 incidents/diagnostics/reports: fixed responses so the transport test
    // exercises the three new RPCs end-to-end (not the detection/diagnosis
    // logic, which is unit-tested in atlas-service).
    async fn list_incidents(
        &self,
        _req: Request<atlas_ipc::ListIncidentsRequest>,
    ) -> Result<Response<atlas_ipc::ListIncidentsReply>, Status> {
        Ok(Response::new(atlas_ipc::ListIncidentsReply {
            incidents: vec![atlas_ipc::Incident {
                id: 5,
                kind: atlas_ipc::IncidentKind::CpuSaturation as i32,
                start_ms: 1_000,
                end_ms: 0, // ongoing
                severity: atlas_ipc::Severity::Critical as i32,
                peak_value: 96.0,
                summary: "CPU saturation (ongoing)".into(),
            }],
            truncated: false,
        }))
    }

    async fn diagnose(
        &self,
        req: Request<atlas_ipc::DiagnoseRequest>,
    ) -> Result<Response<atlas_ipc::DiagnoseReply>, Status> {
        let id = req.into_inner().incident_id;
        Ok(Response::new(atlas_ipc::DiagnoseReply {
            available: true,
            unavailable_reason: String::new(),
            diagnosis: Some(atlas_ipc::Diagnosis {
                observed: format!("diagnosis for incident {id}"),
                range: Some(atlas_ipc::TimeRange {
                    from_ms: 1_000,
                    to_ms: 20_000,
                }),
                evidence: vec![atlas_ipc::EvidenceItem {
                    text: "Peak system CPU 96%".into(),
                    ts_ms: 5_000,
                    metric: "sys_cpu_pct".into(),
                    value: 96.0,
                }],
                factors: vec![atlas_ipc::ContributingFactor {
                    description: "proc1.exe (pid 100) averaged 80% CPU".into(),
                    confidence: atlas_ipc::Confidence::High as i32,
                    pid: 100,
                    image_name: "proc1.exe".into(),
                    attribution: 0.8,
                }],
                overall_confidence: atlas_ipc::Confidence::High as i32,
                alternatives: vec![],
                recommendation: "close proc1.exe".into(),
                risk: "loses unsaved work".into(),
                reversibility: "reversible".into(),
                verification_plan: "watch CPU".into(),
            }),
        }))
    }

    async fn generate_report(
        &self,
        req: Request<atlas_ipc::GenerateReportRequest>,
    ) -> Result<Response<atlas_ipc::GenerateReportReply>, Status> {
        let fmt = req.into_inner().format;
        Ok(Response::new(atlas_ipc::GenerateReportReply {
            content: "Atlas incident report".into(),
            content_type: if fmt == atlas_ipc::ReportFormat::ReportHtml as i32 {
                "text/html".into()
            } else {
                "text/plain".into()
            },
        }))
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

    // M8: ListIncidents returns the ongoing CPU incident.
    let incidents = client
        .list_incidents(atlas_ipc::ListIncidentsRequest {
            range: None,
            limit: 0,
        })
        .await
        .expect("ListIncidents")
        .into_inner();
    assert_eq!(incidents.incidents.len(), 1);
    assert_eq!(incidents.incidents[0].id, 5);
    assert_eq!(incidents.incidents[0].end_ms, 0, "ongoing incident");

    // M8: Diagnose an incident by id round-trips the structured diagnosis.
    let diag = client
        .diagnose(atlas_ipc::DiagnoseRequest {
            incident_id: 5,
            range: None,
        })
        .await
        .expect("Diagnose")
        .into_inner();
    assert!(diag.available);
    let d = diag.diagnosis.expect("diagnosis present");
    assert_eq!(d.overall_confidence, atlas_ipc::Confidence::High as i32);
    assert_eq!(d.factors.len(), 1);
    assert_eq!(d.evidence.len(), 1);

    // M8: GenerateReport returns content + a matching content type.
    let report = client
        .generate_report(atlas_ipc::GenerateReportRequest {
            incident_id: 5,
            range: None,
            format: atlas_ipc::ReportFormat::ReportHtml as i32,
            redaction: Some(atlas_ipc::RedactionOptions {
                redact_user_names: true,
                redact_computer_name: false,
                redact_paths: true,
                redact_command_lines: false,
            }),
        })
        .await
        .expect("GenerateReport")
        .into_inner();
    assert_eq!(report.content_type, "text/html");
    assert!(!report.content.is_empty());

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
