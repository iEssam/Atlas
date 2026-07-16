//! End-to-end IPC round-trip over a real Windows named pipe (docs/phases.md
//! M4). Starts the tonic server on a unique pipe name, connects the client,
//! and asserts GetCapabilities + GetSnapshot come back correctly. Runs
//! unprivileged — the pipe DACL grants the current user access.

#![cfg(windows)]

use prost::Message;
use tonic::{Request, Response, Status};

use atlas_ipc::v0::atlas_plugins_server::{AtlasPlugins, AtlasPluginsServer};
use atlas_ipc::v0::atlas_query_server::{AtlasQuery, AtlasQueryServer};
use atlas_ipc::v0::atlas_rules_server::{AtlasRules, AtlasRulesServer};
use atlas_ipc::{
    CapabilitiesReply, CapabilitiesRequest, GpuAdapterTelemetry, GpuAvailabilityReason,
    GpuSensorAvailability, GpuSensorKind, GpuTelemetrySource, ProcessRow, SnapshotReply,
    SnapshotRequest, SystemGauges, CAP_PROCESS_SNAPSHOTS,
};

#[derive(Clone, PartialEq, Message)]
struct LegacyGpuAdapter {
    #[prost(string, tag = "1")]
    adapter_key: String,
    #[prost(string, tag = "2")]
    name: String,
    #[prost(double, optional, tag = "11")]
    temperature_c: Option<f64>,
    #[prost(double, optional, tag = "12")]
    power_w: Option<f64>,
}

#[test]
fn gpu_contract_is_additive_for_legacy_clients_and_services() {
    let current = GpuAdapterTelemetry {
        adapter_key: "adapter".into(),
        name: "GPU".into(),
        temperature_c: Some(59.0),
        power_w: Some(125.0),
        power_percent: Some(73.0),
        fan_percent: Some(44.0),
        driver_date: "2026-05-19".into(),
        ..Default::default()
    };
    let legacy = LegacyGpuAdapter::decode(current.encode_to_vec().as_slice()).unwrap();
    assert_eq!(legacy.adapter_key, "adapter");
    assert_eq!(legacy.temperature_c, Some(59.0));

    let old_wire = LegacyGpuAdapter {
        adapter_key: "old".into(),
        name: "Old GPU".into(),
        temperature_c: Some(50.0),
        power_w: None,
    };
    let decoded = GpuAdapterTelemetry::decode(old_wire.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.name, "Old GPU");
    assert_eq!(decoded.power_percent, None);
    assert!(decoded.sensor_availability.is_empty());
}

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
            ..Default::default()
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
                ..Default::default()
            }),
            processes,
            gpu_adapters: vec![GpuAdapterTelemetry {
                adapter_key: "00000000:00000001".into(),
                name: "Test GPU".into(),
                temperature_c: Some(59.0),
                power_w: Some(120.5),
                power_percent: Some(61.0),
                fan_percent: Some(44.0),
                sensor_availability: vec![GpuSensorAvailability {
                    kind: GpuSensorKind::GpuSensorCoreTemperature as i32,
                    available: true,
                    source: GpuTelemetrySource::GpuSourceNvidiaNvml as i32,
                    reason: GpuAvailabilityReason::GpuAvailabilityNone as i32,
                    detail: String::new(),
                }],
                ..Default::default()
            }],
            ..Default::default()
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
                ..Default::default()
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

    // R2 advanced privacy alerts: fixed responses so the transport test exercises
    // the five new RPCs end-to-end (the real watcher/evaluator logic is
    // unit-tested in atlas-service + atlas-collectors).
    async fn list_privacy_alert_rules(
        &self,
        _req: Request<atlas_ipc::ListPrivacyAlertRulesRequest>,
    ) -> Result<Response<atlas_ipc::ListPrivacyAlertRulesReply>, Status> {
        Ok(Response::new(atlas_ipc::ListPrivacyAlertRulesReply {
            rules: vec![atlas_ipc::PrivacyAlertRule {
                id: 1,
                name: "mic any-use".into(),
                enabled: true,
                capability: atlas_ipc::CapabilityKind::Microphone as i32,
                condition: atlas_ipc::PrivacyAlertCondition::AlertAnyUse as i32,
                threshold_seconds: 0,
                created_ms: 1_700_000_000_000,
            }],
        }))
    }

    async fn create_privacy_alert_rule(
        &self,
        req: Request<atlas_ipc::CreatePrivacyAlertRuleRequest>,
    ) -> Result<Response<atlas_ipc::CreatePrivacyAlertRuleReply>, Status> {
        let _ = req.into_inner().rule; // id ignored
        Ok(Response::new(atlas_ipc::CreatePrivacyAlertRuleReply {
            id: 99,
        }))
    }

    async fn update_privacy_alert_rule(
        &self,
        _req: Request<atlas_ipc::UpdatePrivacyAlertRuleRequest>,
    ) -> Result<Response<atlas_ipc::UpdatePrivacyAlertRuleReply>, Status> {
        Ok(Response::new(atlas_ipc::UpdatePrivacyAlertRuleReply {
            ok: true,
        }))
    }

    async fn delete_privacy_alert_rule(
        &self,
        _req: Request<atlas_ipc::DeletePrivacyAlertRuleRequest>,
    ) -> Result<Response<atlas_ipc::DeletePrivacyAlertRuleReply>, Status> {
        Ok(Response::new(atlas_ipc::DeletePrivacyAlertRuleReply {
            ok: true,
        }))
    }

    async fn list_fired_alerts(
        &self,
        _req: Request<atlas_ipc::ListFiredAlertsRequest>,
    ) -> Result<Response<atlas_ipc::ListFiredAlertsReply>, Status> {
        Ok(Response::new(atlas_ipc::ListFiredAlertsReply {
            alerts: vec![atlas_ipc::FiredAlert {
                id: 7,
                rule_id: 1,
                rule_name: "mic any-use".into(),
                ts_ms: 1_700_000_000_500,
                capability: atlas_ipc::CapabilityKind::Microphone as i32,
                app_id: "C:#app.exe".into(),
                display_name: "app.exe".into(),
                detail: "microphone used by app.exe while not in the foreground".into(),
            }],
            truncated: false,
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

    // R3 forensics: fixed responses so the transport test exercises both new RPCs
    // end-to-end (the detection/correlation logic is unit-tested in the crates).
    async fn list_system_changes(
        &self,
        _req: Request<atlas_ipc::ListSystemChangesRequest>,
    ) -> Result<Response<atlas_ipc::ListSystemChangesReply>, Status> {
        Ok(Response::new(atlas_ipc::ListSystemChangesReply {
            changes: vec![atlas_ipc::SystemChange {
                id: 42,
                ts_ms: 1_700_000_100_000,
                kind: atlas_ipc::SystemChangeKind::AppUpdated as i32,
                subject: "Acme Reader".into(),
                detail: "1.2.3 → 1.2.4".into(),
                publisher: "Acme".into(),
                responsible: String::new(),
                reversible: false,
            }],
            truncated: false,
        }))
    }

    async fn list_crashes(
        &self,
        _req: Request<atlas_ipc::ListCrashesRequest>,
    ) -> Result<Response<atlas_ipc::ListCrashesReply>, Status> {
        Ok(Response::new(atlas_ipc::ListCrashesReply {
            available: true,
            unavailable_reason: String::new(),
            crashes: vec![atlas_ipc::CrashRecord {
                id: 9,
                ts_ms: 1_700_000_200_000,
                kind: atlas_ipc::CrashKind::AppCrash as i32,
                subject: "app.exe".into(),
                fault: "app.dll".into(),
                exception_code: "0xc0000005".into(),
                context: vec![
                    "peak system memory 82% in the 5 min before this event (correlation, not proof)"
                        .into(),
                    "'Acme Reader' app_updated 2h before this event (correlation, not proof)".into(),
                ],
            }],
            truncated: false,
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

    async fn list_insights(
        &self,
        _req: Request<atlas_ipc::ListInsightsRequest>,
    ) -> Result<Response<atlas_ipc::ListInsightsReply>, Status> {
        Ok(Response::new(atlas_ipc::ListInsightsReply {
            insights: vec![atlas_ipc::Insight {
                fingerprint: "cpu-pressure:incident:5".into(),
                kind: atlas_ipc::InsightKind::CpuPressure as i32,
                status: atlas_ipc::InsightStatus::Active as i32,
                severity: atlas_ipc::Severity::Warning as i32,
                confidence: atlas_ipc::Confidence::High as i32,
                title: "proc0.exe is driving sustained CPU pressure".into(),
                observation: "CPU saturation has remained active for 3 minutes.".into(),
                significance: "Foreground work may be delayed.".into(),
                range: Some(atlas_ipc::TimeRange {
                    from_ms: 1_000,
                    to_ms: 20_000,
                }),
                evidence: vec![atlas_ipc::EvidenceItem {
                    text: "System CPU is 96%.".into(),
                    ts_ms: 5_000,
                    metric: "sys_cpu_percent".into(),
                    value: 96.0,
                }],
                factors: vec![],
                alternatives: vec![],
                limitations: vec!["Attribution is a current reading.".into()],
                recommendation: Some(atlas_ipc::InsightRecommendation {
                    text: "Inspect proc0.exe before deciding whether to close it.".into(),
                    risk: "Closing it can discard unsaved work.".into(),
                    reversibility: "Inspection makes no change.".into(),
                    verification_plan: "Watch CPU for two minutes.".into(),
                    destination: "process:100:0:proc0.exe".into(),
                }),
                updated_ms: 20_000,
            }],
            truncated: false,
            coverage_summary: "Current insight coverage: CPU pressure.".into(),
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

    // R3 remote support bundle: a fixed self-contained reply so the transport
    // test exercises the new RPC end-to-end (the real assembly/redaction/format
    // logic is unit-tested in atlas-service::support_bundle). Echoes the format
    // in the content type + filename and the applied redaction categories.
    async fn generate_support_bundle(
        &self,
        req: Request<atlas_ipc::SupportBundleRequest>,
    ) -> Result<Response<atlas_ipc::SupportBundleReply>, Status> {
        let r = req.into_inner();
        let is_json = r.format == atlas_ipc::ReportFormat::ReportJson as i32;
        let mut redaction_applied = Vec::new();
        if let Some(opts) = &r.redaction {
            if opts.redact_paths {
                redaction_applied.push("paths".to_string());
            }
            if opts.redact_user_names {
                redaction_applied.push("user_names".to_string());
            }
        }
        Ok(Response::new(atlas_ipc::SupportBundleReply {
            content: if is_json {
                "{\"device\":{}}".into()
            } else {
                "<!doctype html><title>Atlas support bundle</title>".into()
            },
            content_type: if is_json {
                "application/json".into()
            } else {
                "text/html".into()
            },
            filename: if is_json {
                "atlas-support-2023-11-15.json".into()
            } else {
                "atlas-support-2023-11-15.html".into()
            },
            redaction_applied,
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

    // R3 expert security metadata: a fixed reply so the transport test exercises
    // GetSecurityMetadata end-to-end (the real collection logic is unit-tested
    // against live processes in atlas-collectors::security_meta).
    async fn get_security_metadata(
        &self,
        req: Request<atlas_ipc::GetSecurityMetadataRequest>,
    ) -> Result<Response<atlas_ipc::GetSecurityMetadataReply>, Status> {
        let _ = req.into_inner();
        Ok(Response::new(atlas_ipc::GetSecurityMetadataReply {
            available: true,
            unavailable_reason: String::new(),
            metadata: Some(atlas_ipc::SecurityMetadata {
                file_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                    .into(),
                signature_status: "Signed (Microsoft)".into(),
                cert_chain: vec![atlas_ipc::CertInfo {
                    subject: "Microsoft Windows".into(),
                    issuer: "Microsoft Windows Production PCA 2011".into(),
                    thumbprint_sha1: "AABBCCDDEEFF00112233445566778899AABBCCDD".into(),
                    not_before_ms: 1_600_000_000_000,
                    not_after_ms: 1_800_000_000_000,
                }],
                user_sid: "S-1-5-21-1".into(),
                integrity_level: "Medium".into(),
                elevated: false,
                app_container: false,
                privileges: vec![atlas_ipc::TokenPrivilege {
                    name: "SeChangeNotifyPrivilege".into(),
                    enabled: true,
                }],
                groups: vec!["BUILTIN\\Users".into()],
                capabilities: vec![],
                mitigations: vec!["DEP".into(), "ASLR (high-entropy)".into(), "CFG".into()],
                limited: false,
            }),
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

    // (list_system_changes / list_crashes are implemented with rich fixtures
    // above; the dynamic-protection branch's empty stubs were dropped at merge.)
}

/// Minimal fixed-response AtlasRules service so the transport test exercises the
/// R2 rules-engine RPCs end-to-end (the real resolver/applier logic is
/// unit-tested in atlas-service). Echoes enough to assert the wire shape.
#[derive(Default)]
struct FakeRules;

fn fake_rule(id: i64, image: &str) -> atlas_ipc::Rule {
    atlas_ipc::Rule {
        id,
        name: format!("throttle {image}"),
        enabled: true,
        match_image: image.to_string(),
        trigger: atlas_ipc::RuleTrigger::WhileRunning as i32,
        action: Some(atlas_ipc::RuleAction {
            priority: atlas_ipc::PriorityClass::PriorityBelowNormal as i32,
            affinity_mode: atlas_ipc::CoreAffinityMode::CoreAffinityUnspecified as i32,
            affinity_mask: 0,
            eco_qos: true,
        }),
        precedence: 10,
        created_ms: 1_700_000_000_000,
        gpu_threshold_permille: 800,
        gpu_duration_seconds: 5,
    }
}

#[tonic::async_trait]
impl AtlasRules for FakeRules {
    async fn list_rules(
        &self,
        _req: Request<atlas_ipc::ListRulesRequest>,
    ) -> Result<Response<atlas_ipc::ListRulesReply>, Status> {
        Ok(Response::new(atlas_ipc::ListRulesReply {
            rules: vec![fake_rule(1, "chrome.exe")],
        }))
    }

    async fn get_rule(
        &self,
        req: Request<atlas_ipc::GetRuleRequest>,
    ) -> Result<Response<atlas_ipc::GetRuleReply>, Status> {
        let id = req.into_inner().id;
        Ok(Response::new(atlas_ipc::GetRuleReply {
            found: id == 1,
            rule: (id == 1).then(|| fake_rule(1, "chrome.exe")),
        }))
    }

    async fn create_rule(
        &self,
        req: Request<atlas_ipc::CreateRuleRequest>,
    ) -> Result<Response<atlas_ipc::CreateRuleReply>, Status> {
        // Echo a fixed id regardless of the (id-ignored) input rule.
        let _ = req.into_inner().rule;
        Ok(Response::new(atlas_ipc::CreateRuleReply { id: 42 }))
    }

    async fn update_rule(
        &self,
        _req: Request<atlas_ipc::UpdateRuleRequest>,
    ) -> Result<Response<atlas_ipc::UpdateRuleReply>, Status> {
        Ok(Response::new(atlas_ipc::UpdateRuleReply {
            ok: true,
            message: String::new(),
        }))
    }

    async fn delete_rule(
        &self,
        _req: Request<atlas_ipc::DeleteRuleRequest>,
    ) -> Result<Response<atlas_ipc::DeleteRuleReply>, Status> {
        Ok(Response::new(atlas_ipc::DeleteRuleReply { ok: true }))
    }

    async fn set_rule_enabled(
        &self,
        _req: Request<atlas_ipc::SetRuleEnabledRequest>,
    ) -> Result<Response<atlas_ipc::SetRuleEnabledReply>, Status> {
        Ok(Response::new(atlas_ipc::SetRuleEnabledReply { ok: true }))
    }

    async fn simulate_rule(
        &self,
        _req: Request<atlas_ipc::SimulateRuleRequest>,
    ) -> Result<Response<atlas_ipc::SimulateRuleReply>, Status> {
        Ok(Response::new(atlas_ipc::SimulateRuleReply {
            targets: vec![
                atlas_ipc::SimulatedTarget {
                    pid: 1234,
                    image_name: "chrome.exe".into(),
                    current_priority: "Normal".into(),
                    new_priority: "Below Normal".into(),
                    current_affinity: "0xff".into(),
                    new_affinity: "0xff".into(),
                    eco_qos_change: true,
                    blocked: false,
                    blocked_reason: String::new(),
                },
                atlas_ipc::SimulatedTarget {
                    pid: 700,
                    image_name: "lsass.exe".into(),
                    current_priority: String::new(),
                    new_priority: String::new(),
                    current_affinity: String::new(),
                    new_affinity: String::new(),
                    eco_qos_change: false,
                    blocked: true,
                    blocked_reason: "protected-critical".into(),
                },
            ],
            conflicts: vec![
                "priority conflicts with rule #2 (precedence 5) — this rule wins".into(),
            ],
        }))
    }

    async fn list_interventions(
        &self,
        _req: Request<atlas_ipc::ListInterventionsRequest>,
    ) -> Result<Response<atlas_ipc::ListInterventionsReply>, Status> {
        Ok(Response::new(atlas_ipc::ListInterventionsReply {
            interventions: vec![atlas_ipc::Intervention {
                rule_id: 1,
                rule_name: "throttle chrome.exe".into(),
                pid: 1234,
                image_name: "chrome.exe".into(),
                applied: "priority + EcoQoS".into(),
                since_ms: 1_700_000_000_000,
            }],
        }))
    }

    async fn list_profiles(
        &self,
        _req: Request<atlas_ipc::ListProfilesRequest>,
    ) -> Result<Response<atlas_ipc::ListProfilesReply>, Status> {
        Ok(Response::new(atlas_ipc::ListProfilesReply {
            profiles: vec![atlas_ipc::Profile {
                id: 1,
                name: "Gaming".into(),
                rule_ids: vec![1],
                power_mode: "HighPerformance".into(),
                active: false,
            }],
        }))
    }

    async fn create_profile(
        &self,
        _req: Request<atlas_ipc::CreateProfileRequest>,
    ) -> Result<Response<atlas_ipc::CreateProfileReply>, Status> {
        Ok(Response::new(atlas_ipc::CreateProfileReply { id: 7 }))
    }

    async fn update_profile(
        &self,
        _req: Request<atlas_ipc::UpdateProfileRequest>,
    ) -> Result<Response<atlas_ipc::UpdateProfileReply>, Status> {
        Ok(Response::new(atlas_ipc::UpdateProfileReply { ok: true }))
    }

    async fn delete_profile(
        &self,
        _req: Request<atlas_ipc::DeleteProfileRequest>,
    ) -> Result<Response<atlas_ipc::DeleteProfileReply>, Status> {
        Ok(Response::new(atlas_ipc::DeleteProfileReply { ok: true }))
    }

    async fn set_profile_active(
        &self,
        req: Request<atlas_ipc::SetProfileActiveRequest>,
    ) -> Result<Response<atlas_ipc::SetProfileActiveReply>, Status> {
        let active = req.into_inner().active;
        Ok(Response::new(atlas_ipc::SetProfileActiveReply {
            ok: true,
            message: format!("profile active={active}"),
        }))
    }

    // R3 dynamic responsiveness protection: fixed responses so the transport test
    // exercises the two new RPCs end-to-end (the watchdog decision core + applier
    // are unit-tested in atlas-service).
    async fn get_dynamic_protection(
        &self,
        _req: Request<atlas_ipc::GetDynamicProtectionRequest>,
    ) -> Result<Response<atlas_ipc::GetDynamicProtectionReply>, Status> {
        Ok(Response::new(atlas_ipc::GetDynamicProtectionReply {
            config: Some(atlas_ipc::DynamicProtectionConfig {
                enabled: false,
                cpu_threshold_permille: 800,
                sustain_seconds: 30,
                max_intervention_seconds: 300,
            }),
        }))
    }

    async fn set_dynamic_protection(
        &self,
        req: Request<atlas_ipc::SetDynamicProtectionRequest>,
    ) -> Result<Response<atlas_ipc::SetDynamicProtectionReply>, Status> {
        let cfg = req.into_inner().config.unwrap_or_default();
        Ok(Response::new(atlas_ipc::SetDynamicProtectionReply {
            ok: true,
            message: format!("enabled={}", cfg.enabled),
        }))
    }
}

/// Minimal AtlasPlugins fake so the transport carries the R3 plugin
/// registry/session surface. Echoes a registered plugin and a session token. The
/// real capability interceptor lives in atlas-service (unit-tested there); this
/// only proves the contract round-trips over the pipe.
#[derive(Default)]
struct FakePlugins;

#[tonic::async_trait]
impl AtlasPlugins for FakePlugins {
    async fn list_plugins(
        &self,
        _req: Request<atlas_ipc::ListPluginsRequest>,
    ) -> Result<Response<atlas_ipc::ListPluginsReply>, Status> {
        Ok(Response::new(atlas_ipc::ListPluginsReply {
            plugins: vec![atlas_ipc::Plugin {
                id: 7,
                name: "Example".into(),
                version: "1.0.0".into(),
                publisher: "Contoso".into(),
                exe_path: "C:\\x\\example.exe".into(),
                signature: atlas_ipc::PluginSignature::PluginSigned as i32,
                enabled: true,
                granted: vec![atlas_ipc::PluginCapability::PluginCapSnapshot as i32],
                registered_ms: 111,
                description: String::new(),
            }],
        }))
    }

    async fn register_plugin(
        &self,
        req: Request<atlas_ipc::RegisterPluginRequest>,
    ) -> Result<Response<atlas_ipc::RegisterPluginReply>, Status> {
        let r = req.into_inner();
        // Mirror the real refusal contract: unsigned is refused unless overridden.
        if !r.allow_unsigned {
            return Ok(Response::new(atlas_ipc::RegisterPluginReply {
                ok: false,
                message: "refused: executable is not signed".into(),
                plugin: None,
            }));
        }
        Ok(Response::new(atlas_ipc::RegisterPluginReply {
            ok: true,
            message: "registered".into(),
            plugin: Some(atlas_ipc::Plugin {
                id: 7,
                name: "Example".into(),
                version: "1.0.0".into(),
                publisher: String::new(),
                exe_path: r.exe_path,
                signature: atlas_ipc::PluginSignature::PluginUnsigned as i32,
                enabled: false,
                granted: r.requested,
                registered_ms: 111,
                description: String::new(),
            }),
        }))
    }

    async fn set_plugin_enabled(
        &self,
        req: Request<atlas_ipc::SetPluginEnabledRequest>,
    ) -> Result<Response<atlas_ipc::SetPluginEnabledReply>, Status> {
        let r = req.into_inner();
        Ok(Response::new(atlas_ipc::SetPluginEnabledReply {
            ok: true,
            message: format!("enabled={}", r.enabled),
        }))
    }

    async fn grant_plugin_capabilities(
        &self,
        _req: Request<atlas_ipc::GrantPluginCapabilitiesRequest>,
    ) -> Result<Response<atlas_ipc::GrantPluginCapabilitiesReply>, Status> {
        Ok(Response::new(atlas_ipc::GrantPluginCapabilitiesReply {
            ok: true,
        }))
    }

    async fn remove_plugin(
        &self,
        _req: Request<atlas_ipc::RemovePluginRequest>,
    ) -> Result<Response<atlas_ipc::RemovePluginReply>, Status> {
        Ok(Response::new(atlas_ipc::RemovePluginReply { ok: true }))
    }

    async fn open_plugin_session(
        &self,
        req: Request<atlas_ipc::OpenPluginSessionRequest>,
    ) -> Result<Response<atlas_ipc::OpenPluginSessionReply>, Status> {
        let r = req.into_inner();
        Ok(Response::new(atlas_ipc::OpenPluginSessionReply {
            ok: !r.launch_nonce.is_empty(),
            message: String::new(),
            session_token: "fake-token".into(),
            granted: vec![atlas_ipc::PluginCapability::PluginCapSnapshot as i32],
        }))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_and_snapshot_round_trip() {
    // Unique pipe per test run so parallel/repeat runs never collide.
    let who = format!("test.{}.{}", std::process::id(), fastish_token());
    let name = atlas_ipc::pipe_name(&who);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let router = tonic::transport::Server::builder()
        .add_service(AtlasQueryServer::new(FakeQuery))
        .add_service(AtlasRulesServer::new(FakeRules))
        .add_service(AtlasPluginsServer::new(FakePlugins));
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
    assert_eq!(snap.gpu_adapters[0].temperature_c, Some(59.0));
    assert_eq!(snap.gpu_adapters[0].power_percent, Some(61.0));
    assert_eq!(
        snap.gpu_adapters[0].sensor_availability[0].source,
        GpuTelemetrySource::GpuSourceNvidiaNvml as i32
    );

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

    // Insights preserve the evidence, confidence, and investigation target.
    let insights = client
        .list_insights(atlas_ipc::ListInsightsRequest {
            active_only: false,
            limit: 3,
        })
        .await
        .expect("ListInsights")
        .into_inner();
    assert_eq!(insights.insights.len(), 1);
    assert_eq!(
        insights.insights[0].status,
        atlas_ipc::InsightStatus::Active as i32
    );
    assert_eq!(insights.insights[0].evidence.len(), 1);
    assert_eq!(
        insights.insights[0]
            .recommendation
            .as_ref()
            .expect("recommendation")
            .destination,
        "process:100:0:proc0.exe"
    );

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

    // R3: GetSecurityMetadata round-trips the file hash, cert chain, token
    // privileges/groups, and mitigations.
    let sec = client
        .get_security_metadata(atlas_ipc::GetSecurityMetadataRequest {
            pid: 1234,
            create_time_100ns: 0,
        })
        .await
        .expect("GetSecurityMetadata")
        .into_inner();
    assert!(sec.available);
    let sm = sec.metadata.expect("security metadata present");
    assert_eq!(sm.file_sha256.len(), 64);
    assert_eq!(sm.cert_chain.len(), 1);
    assert_eq!(sm.cert_chain[0].thumbprint_sha1.len(), 40);
    assert!(sm
        .privileges
        .iter()
        .any(|p| p.name == "SeChangeNotifyPrivilege" && p.enabled));
    assert!(sm.mitigations.iter().any(|s| s == "DEP"));
    assert!(!sm.limited);

    // R2: the AtlasRules service round-trips on the same pipe. CreateRule echoes
    // an id; ListRules returns the rule with its flattened action.
    let mut rules = atlas_ipc::AtlasRulesClient::new(
        atlas_ipc::connect(&name)
            .await
            .expect("rules client connects"),
    );
    let created = rules
        .create_rule(atlas_ipc::CreateRuleRequest {
            rule: Some(fake_rule(0, "chrome.exe")),
        })
        .await
        .expect("CreateRule")
        .into_inner();
    assert_eq!(created.id, 42);

    let listed = rules
        .list_rules(atlas_ipc::ListRulesRequest {})
        .await
        .expect("ListRules")
        .into_inner();
    assert_eq!(listed.rules.len(), 1);
    let action = listed.rules[0].action.as_ref().expect("action present");
    assert_eq!(
        action.priority,
        atlas_ipc::PriorityClass::PriorityBelowNormal as i32
    );
    assert!(action.eco_qos);

    // SimulateRule returns a normal target and a blocked (protected) target,
    // plus a conflict note — applying nothing.
    let sim = rules
        .simulate_rule(atlas_ipc::SimulateRuleRequest {
            rule: Some(fake_rule(0, "chrome.exe")),
        })
        .await
        .expect("SimulateRule")
        .into_inner();
    assert_eq!(sim.targets.len(), 2);
    assert!(sim.targets.iter().any(|t| !t.blocked && t.eco_qos_change));
    assert!(sim
        .targets
        .iter()
        .any(|t| t.blocked && t.image_name == "lsass.exe"));
    assert_eq!(sim.conflicts.len(), 1);

    // ListInterventions surfaces the live ledger entry.
    let interventions = rules
        .list_interventions(atlas_ipc::ListInterventionsRequest {})
        .await
        .expect("ListInterventions")
        .into_inner();
    assert_eq!(interventions.interventions.len(), 1);
    assert_eq!(interventions.interventions[0].pid, 1234);

    // Profiles round-trip: SetProfileActive echoes the toggle.
    let activated = rules
        .set_profile_active(atlas_ipc::SetProfileActiveRequest {
            id: 1,
            active: true,
        })
        .await
        .expect("SetProfileActive")
        .into_inner();
    assert!(activated.ok);
    assert!(activated.message.contains("active=true"));

    // R3: dynamic responsiveness protection. GetDynamicProtection returns the
    // disabled-by-default config; SetDynamicProtection round-trips the toggle.
    let dp = rules
        .get_dynamic_protection(atlas_ipc::GetDynamicProtectionRequest {})
        .await
        .expect("GetDynamicProtection")
        .into_inner()
        .config
        .expect("config present");
    assert!(!dp.enabled, "disabled by default");
    assert_eq!(dp.cpu_threshold_permille, 800);
    assert_eq!(dp.max_intervention_seconds, 300);

    let set_dp = rules
        .set_dynamic_protection(atlas_ipc::SetDynamicProtectionRequest {
            config: Some(atlas_ipc::DynamicProtectionConfig {
                enabled: true,
                cpu_threshold_permille: 500,
                sustain_seconds: 10,
                max_intervention_seconds: 60,
            }),
        })
        .await
        .expect("SetDynamicProtection")
        .into_inner();
    assert!(set_dp.ok);
    assert!(set_dp.message.contains("enabled=true"));
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

    // R2: advanced privacy alerts. CreatePrivacyAlertRule echoes an id;
    // ListPrivacyAlertRules returns the rule; ListFiredAlerts reads a factual
    // fired-alert row.
    let created_alert = client
        .create_privacy_alert_rule(atlas_ipc::CreatePrivacyAlertRuleRequest {
            rule: Some(atlas_ipc::PrivacyAlertRule {
                id: 0,
                name: "mic any-use".into(),
                enabled: true,
                capability: atlas_ipc::CapabilityKind::Microphone as i32,
                condition: atlas_ipc::PrivacyAlertCondition::AlertAnyUse as i32,
                threshold_seconds: 0,
                created_ms: 0,
            }),
        })
        .await
        .expect("CreatePrivacyAlertRule")
        .into_inner();
    assert_eq!(created_alert.id, 99);

    let alert_rules = client
        .list_privacy_alert_rules(atlas_ipc::ListPrivacyAlertRulesRequest {})
        .await
        .expect("ListPrivacyAlertRules")
        .into_inner();
    assert_eq!(alert_rules.rules.len(), 1);
    assert_eq!(
        alert_rules.rules[0].condition,
        atlas_ipc::PrivacyAlertCondition::AlertAnyUse as i32
    );

    let updated_alert = client
        .update_privacy_alert_rule(atlas_ipc::UpdatePrivacyAlertRuleRequest {
            rule: Some(alert_rules.rules[0].clone()),
        })
        .await
        .expect("UpdatePrivacyAlertRule")
        .into_inner();
    assert!(updated_alert.ok);

    let fired = client
        .list_fired_alerts(atlas_ipc::ListFiredAlertsRequest {
            range: None,
            limit: 0,
        })
        .await
        .expect("ListFiredAlerts")
        .into_inner();
    assert_eq!(fired.alerts.len(), 1);
    assert_eq!(fired.alerts[0].rule_name, "mic any-use");
    assert!(fired.alerts[0].detail.contains("microphone"));

    let deleted_alert = client
        .delete_privacy_alert_rule(atlas_ipc::DeletePrivacyAlertRuleRequest { id: 1 })
        .await
        .expect("DeletePrivacyAlertRule")
        .into_inner();
    assert!(deleted_alert.ok);

    // R3 forensics: system changes + crashes round-trip over the pipe.
    let changes = client
        .list_system_changes(atlas_ipc::ListSystemChangesRequest {
            range: None,
            kinds: vec![],
            limit: 0,
        })
        .await
        .expect("ListSystemChanges")
        .into_inner();
    assert_eq!(changes.changes.len(), 1);
    assert_eq!(
        changes.changes[0].kind,
        atlas_ipc::SystemChangeKind::AppUpdated as i32
    );
    assert_eq!(changes.changes[0].detail, "1.2.3 → 1.2.4");

    let crashes = client
        .list_crashes(atlas_ipc::ListCrashesRequest {
            range: None,
            kinds: vec![],
            limit: 0,
        })
        .await
        .expect("ListCrashes")
        .into_inner();
    assert!(crashes.available);
    assert_eq!(crashes.crashes.len(), 1);
    assert_eq!(crashes.crashes[0].exception_code, "0xc0000005");
    assert!(crashes.crashes[0].context[1].contains("not proof"));

    // R3: GenerateSupportBundle round-trips a self-contained document with a
    // content type, a dated filename, and the applied redaction categories.
    let bundle = client
        .generate_support_bundle(atlas_ipc::SupportBundleRequest {
            range: None,
            redaction: Some(atlas_ipc::RedactionOptions {
                redact_user_names: true,
                redact_computer_name: false,
                redact_paths: true,
                redact_command_lines: false,
            }),
            sections: vec![],
            format: atlas_ipc::ReportFormat::ReportHtml as i32,
        })
        .await
        .expect("GenerateSupportBundle")
        .into_inner();
    assert_eq!(bundle.content_type, "text/html");
    assert!(bundle.content.contains("support bundle"));
    assert!(bundle.filename.ends_with(".html"));
    assert_eq!(
        bundle.redaction_applied,
        vec!["paths".to_string(), "user_names".to_string()]
    );

    // R3 signed plugin framework: the AtlasPlugins registry/session surface
    // round-trips on the same pipe. RegisterPlugin honors the unsigned-refusal
    // contract; OpenPluginSession returns a capability-scoped token.
    let mut plugins = atlas_ipc::AtlasPluginsClient::new(
        atlas_ipc::connect(&name)
            .await
            .expect("plugins client connects"),
    );

    // Unsigned is refused unless explicitly overridden.
    let refused = plugins
        .register_plugin(atlas_ipc::RegisterPluginRequest {
            exe_path: "C:\\x\\example.exe".into(),
            requested: vec![atlas_ipc::PluginCapability::PluginCapSnapshot as i32],
            allow_unsigned: false,
        })
        .await
        .expect("RegisterPlugin")
        .into_inner();
    assert!(!refused.ok);
    assert!(refused.message.contains("not signed"));

    let registered = plugins
        .register_plugin(atlas_ipc::RegisterPluginRequest {
            exe_path: "C:\\x\\example.exe".into(),
            requested: vec![atlas_ipc::PluginCapability::PluginCapSnapshot as i32],
            allow_unsigned: true,
        })
        .await
        .expect("RegisterPlugin override")
        .into_inner();
    assert!(registered.ok);
    assert_eq!(registered.plugin.expect("plugin").id, 7);

    let listed = plugins
        .list_plugins(atlas_ipc::ListPluginsRequest {})
        .await
        .expect("ListPlugins")
        .into_inner();
    assert_eq!(listed.plugins.len(), 1);
    assert_eq!(
        listed.plugins[0].granted,
        vec![atlas_ipc::PluginCapability::PluginCapSnapshot as i32]
    );

    let session = plugins
        .open_plugin_session(atlas_ipc::OpenPluginSessionRequest {
            plugin_id: 7,
            launch_nonce: "nonce-123".into(),
        })
        .await
        .expect("OpenPluginSession")
        .into_inner();
    assert!(session.ok);
    assert!(!session.session_token.is_empty());
    assert_eq!(
        session.granted,
        vec![atlas_ipc::PluginCapability::PluginCapSnapshot as i32]
    );

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
