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

    // R2 inspector/resource-ownership: fixed responses so the transport test
    // exercises the five new RPCs end-to-end (the real inspection logic is
    // unit-tested in atlas-collectors against live processes).
    async fn get_process_detail(
        &self,
        req: Request<atlas_ipc::ProcessDetailRequest>,
    ) -> Result<Response<atlas_ipc::ProcessDetailReply>, Status> {
        let pid = req.into_inner().pid;
        Ok(Response::new(atlas_ipc::ProcessDetailReply {
            available: true,
            unavailable_reason: String::new(),
            detail: Some(atlas_ipc::ProcessDetail {
                pid,
                parent_pid: 4,
                create_time_100ns: 132_000_000_000_000_000,
                image_name: "proc0.exe".into(),
                image_path: "C:\\proc0.exe".into(),
                command_line: "proc0.exe --run".into(),
                working_directory: "C:\\".into(),
                user_sid: "S-1-5-21-1".into(),
                user_name: "HOST\\user".into(),
                session_id: 1,
                integrity_level: "Medium".into(),
                elevated: false,
                architecture: "x64".into(),
                signature_status: "Signed (Microsoft)".into(),
                publisher: "Microsoft Corporation".into(),
                file_version: "10.0.1.1".into(),
                product_name: "Windows".into(),
                thread_count: 7,
                handle_count: 42,
                start_time_ms: 1_700_000_000_000,
                package_identity: String::new(),
                limited: false,
            }),
        }))
    }

    async fn list_handles(
        &self,
        req: Request<atlas_ipc::ListHandlesRequest>,
    ) -> Result<Response<atlas_ipc::ListHandlesReply>, Status> {
        let _ = req.into_inner();
        Ok(Response::new(atlas_ipc::ListHandlesReply {
            handles: vec![atlas_ipc::HandleRow {
                handle: 0x1a4,
                r#type: "File".into(),
                name: "\\Device\\HarddiskVolume3\\Windows".into(),
                granted_access: 0x0012_0089,
            }],
            truncated: false,
            names_limited: false,
        }))
    }

    async fn list_modules(
        &self,
        req: Request<atlas_ipc::ListModulesRequest>,
    ) -> Result<Response<atlas_ipc::ListModulesReply>, Status> {
        let _ = req.into_inner();
        Ok(Response::new(atlas_ipc::ListModulesReply {
            available: true,
            unavailable_reason: String::new(),
            modules: vec![atlas_ipc::ModuleRow {
                name: "ntdll.dll".into(),
                path: "C:\\Windows\\System32\\ntdll.dll".into(),
                base_address: 0x7fff_0000_0000,
                size: 0x20_0000,
                version: "10.0.22621.1".into(),
                publisher: "Microsoft Corporation".into(),
                signed: true,
            }],
        }))
    }

    async fn list_threads(
        &self,
        req: Request<atlas_ipc::ListThreadsRequest>,
    ) -> Result<Response<atlas_ipc::ListThreadsReply>, Status> {
        let _ = req.into_inner();
        Ok(Response::new(atlas_ipc::ListThreadsReply {
            threads: vec![atlas_ipc::ThreadRow {
                tid: 1000,
                start_address: 0x7fff_1234_5678,
                state: "Waiting".into(),
                wait_reason: "UserRequest".into(),
                priority: 8,
                cpu_permille: 0,
                user_time_100ns: 10_000,
                kernel_time_100ns: 20_000,
                context_switches: 55,
            }],
        }))
    }

    async fn find_resource_owners(
        &self,
        req: Request<atlas_ipc::FindResourceOwnersRequest>,
    ) -> Result<Response<atlas_ipc::FindResourceOwnersReply>, Status> {
        let _ = req.into_inner();
        Ok(Response::new(atlas_ipc::FindResourceOwnersReply {
            available: true,
            unavailable_reason: String::new(),
            owners: vec![atlas_ipc::ResourceOwner {
                pid: 4321,
                image_name: "notepad.exe".into(),
                image_path: "C:\\Windows\\System32\\notepad.exe".into(),
                description: "Notepad".into(),
                is_service: false,
            }],
        }))
    }

    // R2 monitors: fixed responses so the transport test exercises the six new
    // RPCs end-to-end (the real collection logic is unit-tested against the live
    // OS in atlas-collectors).
    async fn list_connections(
        &self,
        req: Request<atlas_ipc::ListConnectionsRequest>,
    ) -> Result<Response<atlas_ipc::ListConnectionsReply>, Status> {
        let include_listening = req.into_inner().include_listening;
        let mut connections = vec![atlas_ipc::Connection {
            pid: 1000,
            image_name: "svc.exe".into(),
            protocol: atlas_ipc::L4Protocol::Tcp as i32,
            local_addr: "192.168.1.10".into(),
            local_port: 52000,
            remote_addr: "93.184.216.34".into(),
            remote_port: 443,
            remote_domain: "example.com".into(),
            state: atlas_ipc::TcpState::TcpEstablished as i32,
            is_ipv6: false,
        }];
        if include_listening {
            connections.push(atlas_ipc::Connection {
                pid: 4,
                image_name: "System".into(),
                protocol: atlas_ipc::L4Protocol::Tcp as i32,
                local_addr: "0.0.0.0".into(),
                local_port: 445,
                remote_addr: "0.0.0.0".into(),
                remote_port: 0,
                remote_domain: String::new(),
                state: atlas_ipc::TcpState::TcpListen as i32,
                is_ipv6: false,
            });
        }
        Ok(Response::new(atlas_ipc::ListConnectionsReply {
            connections,
        }))
    }

    async fn list_listening_ports(
        &self,
        _req: Request<atlas_ipc::ListListeningPortsRequest>,
    ) -> Result<Response<atlas_ipc::ListListeningPortsReply>, Status> {
        Ok(Response::new(atlas_ipc::ListListeningPortsReply {
            ports: vec![atlas_ipc::ListeningPort {
                protocol: atlas_ipc::L4Protocol::Tcp as i32,
                bind_addr: "0.0.0.0".into(),
                port: 445,
                pid: 4,
                image_name: "System".into(),
                is_ipv6: false,
            }],
        }))
    }

    async fn list_scheduled_tasks(
        &self,
        req: Request<atlas_ipc::ListScheduledTasksRequest>,
    ) -> Result<Response<atlas_ipc::ListScheduledTasksReply>, Status> {
        let _ = req.into_inner();
        Ok(Response::new(atlas_ipc::ListScheduledTasksReply {
            tasks: vec![atlas_ipc::ScheduledTask {
                name: "MyTask".into(),
                path: "\\Microsoft\\Windows\\MyTask".into(),
                folder: "\\Microsoft\\Windows".into(),
                enabled: true,
                triggers: "At logon".into(),
                action: "C:\\foo.exe /run".into(),
                last_run_ms: 1_700_000_000_000,
                next_run_ms: 1_700_000_600_000,
                last_result: 0,
                author: "Microsoft Corporation".into(),
                run_as_highest: true,
                runs_on_idle: false,
                wakes_to_run: false,
            }],
        }))
    }

    async fn list_boots(
        &self,
        req: Request<atlas_ipc::ListBootsRequest>,
    ) -> Result<Response<atlas_ipc::ListBootsReply>, Status> {
        let _ = req.into_inner();
        Ok(Response::new(atlas_ipc::ListBootsReply {
            available: true,
            unavailable_reason: String::new(),
            boots: vec![atlas_ipc::BootRecord {
                boot_ms: 1_700_000_000_000,
                boot_duration_ms: 45_231,
                main_path_ms: 21_000,
                post_boot_ms: 24_231,
                degraded: false,
            }],
        }))
    }

    async fn get_battery_status(
        &self,
        _req: Request<atlas_ipc::GetBatteryStatusRequest>,
    ) -> Result<Response<atlas_ipc::GetBatteryStatusReply>, Status> {
        Ok(Response::new(atlas_ipc::GetBatteryStatusReply {
            available: true,
            unavailable_reason: String::new(),
            status: Some(atlas_ipc::BatteryStatus {
                present: true,
                charging: false,
                on_ac: false,
                percent: 82,
                rate_mw: -12_000,
                remaining_mwh: 41_000,
                full_charge_mwh: 50_000,
                design_mwh: 60_000,
                health_percent: 83,
                cycle_count: 120,
                est_runtime_s: 9_000,
            }),
        }))
    }

    async fn get_thermal(
        &self,
        _req: Request<atlas_ipc::GetThermalRequest>,
    ) -> Result<Response<atlas_ipc::GetThermalReply>, Status> {
        Ok(Response::new(atlas_ipc::GetThermalReply {
            available: true,
            unavailable_reason: String::new(),
            sensors: vec![atlas_ipc::ThermalSensor {
                name: "ACPI\\ThermalZone\\TZ00".into(),
                celsius: 42.5,
                source: "ACPI thermal zone (WMI)".into(),
            }],
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

    // R2: GetProcessDetail round-trips the identity + coverage flag.
    let detail = client
        .get_process_detail(atlas_ipc::ProcessDetailRequest {
            pid: 1234,
            create_time_100ns: 0,
        })
        .await
        .expect("GetProcessDetail")
        .into_inner();
    assert!(detail.available);
    let d = detail.detail.expect("detail present");
    assert_eq!(d.pid, 1234);
    assert_eq!(d.architecture, "x64");
    assert!(!d.limited);

    // R2: ListHandles carries the coverage flags.
    let handles = client
        .list_handles(atlas_ipc::ListHandlesRequest {
            pid: 1234,
            type_filter: String::new(),
            limit: 0,
        })
        .await
        .expect("ListHandles")
        .into_inner();
    assert_eq!(handles.handles.len(), 1);
    assert_eq!(handles.handles[0].r#type, "File");
    assert!(!handles.names_limited);

    // R2: ListModules reports availability + a signed module.
    let modules = client
        .list_modules(atlas_ipc::ListModulesRequest { pid: 1234 })
        .await
        .expect("ListModules")
        .into_inner();
    assert!(modules.available);
    assert_eq!(modules.modules.len(), 1);
    assert!(modules.modules[0].signed);

    // R2: ListThreads round-trips a thread row.
    let threads = client
        .list_threads(atlas_ipc::ListThreadsRequest { pid: 1234 })
        .await
        .expect("ListThreads")
        .into_inner();
    assert_eq!(threads.threads.len(), 1);
    assert_eq!(threads.threads[0].state, "Waiting");

    // R2: FindResourceOwners names the owning process.
    let owners = client
        .find_resource_owners(atlas_ipc::FindResourceOwnersRequest {
            path: "C:\\some\\file.txt".into(),
        })
        .await
        .expect("FindResourceOwners")
        .into_inner();
    assert!(owners.available);
    assert_eq!(owners.owners.len(), 1);
    assert_eq!(owners.owners[0].pid, 4321);
    assert!(!owners.owners[0].is_service);

    // R2: ListConnections returns the established connection; with listening
    // requested it also folds in the LISTEN row.
    let conns = client
        .list_connections(atlas_ipc::ListConnectionsRequest {
            include_listening: true,
        })
        .await
        .expect("ListConnections")
        .into_inner();
    assert_eq!(conns.connections.len(), 2);
    assert_eq!(conns.connections[0].remote_domain, "example.com");
    assert_eq!(
        conns.connections[0].state,
        atlas_ipc::TcpState::TcpEstablished as i32
    );

    // R2: ListListeningPorts names the owning pid + bind.
    let ports = client
        .list_listening_ports(atlas_ipc::ListListeningPortsRequest {})
        .await
        .expect("ListListeningPorts")
        .into_inner();
    assert_eq!(ports.ports.len(), 1);
    assert_eq!(ports.ports[0].port, 445);

    // R2: ListScheduledTasks round-trips the task fields.
    let tasks = client
        .list_scheduled_tasks(atlas_ipc::ListScheduledTasksRequest {
            filter: String::new(),
        })
        .await
        .expect("ListScheduledTasks")
        .into_inner();
    assert_eq!(tasks.tasks.len(), 1);
    assert_eq!(tasks.tasks[0].author, "Microsoft Corporation");
    assert!(tasks.tasks[0].run_as_highest);

    // R2: ListBoots reports availability + a boot record.
    let boots = client
        .list_boots(atlas_ipc::ListBootsRequest { limit: 0 })
        .await
        .expect("ListBoots")
        .into_inner();
    assert!(boots.available);
    assert_eq!(boots.boots.len(), 1);
    assert_eq!(boots.boots[0].boot_duration_ms, 45_231);

    // R2: GetBatteryStatus round-trips the status + health.
    let battery = client
        .get_battery_status(atlas_ipc::GetBatteryStatusRequest {})
        .await
        .expect("GetBatteryStatus")
        .into_inner();
    assert!(battery.available);
    let bs = battery.status.expect("battery status present");
    assert_eq!(bs.percent, 82);
    assert_eq!(bs.health_percent, 83);

    // R2: GetThermal reports a sensor with a source label.
    let thermal = client
        .get_thermal(atlas_ipc::GetThermalRequest {})
        .await
        .expect("GetThermal")
        .into_inner();
    assert!(thermal.available);
    assert_eq!(thermal.sensors.len(), 1);
    assert!((thermal.sensors[0].celsius - 42.5).abs() < 0.001);

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
