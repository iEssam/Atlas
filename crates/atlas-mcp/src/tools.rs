//! Tool catalog + dispatch: MCP tools → read-only `AtlasQuery` RPCs.
//!
//! Each tool maps 1:1 onto exactly one `AtlasQuery` RPC (the read surface). The
//! [`CATALOG`] is the single source of truth for the tool set; the read-only
//! guarantee is a *structural* property of that table — every `source_rpc` is an
//! `AtlasQuery.*` read call, and there is no tool whose RPC mutates the system
//! (asserted by [`tests::no_tool_maps_to_a_mutating_rpc`]). This crate never
//! constructs an `AtlasControl` or `AtlasRules` client at all.
//!
//! Every result is **self-describing**: alongside the structured payload each
//! tool emits a `grounding` block (source RPC + suggested citation + capture
//! time + redaction mode) and passes through the RPC's own honesty markers
//! (`available` / `unavailable_reason` / `truncated` / `limited`). The client's
//! model writes the answer; this crate supplies cited, redacted evidence.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};
use tonic::transport::Channel;

use atlas_ipc::{
    AtlasQueryClient, Confidence, DiagnoseRequest, GpuAvailabilityReason, GpuSensorKind,
    GpuTelemetrySource, GpuTemperatureKind, GpuThrottleReason, IncidentKind, L4Protocol,
    ListConnectionsRequest, ListEventsRequest, ListIncidentsRequest, ListScheduledTasksRequest,
    ListServicesRequest, ListStartupRequest, MetricKind, ProcessDetailRequest, ProcessRole,
    QueryRangeRequest, SearchRequest, ServiceStartType, ServiceState, Severity, SnapshotRequest,
    StartupSource, TcpState, TimeRange,
};

use crate::redact::Redactor;

/// A declared tool: its MCP name, human description, the single read-only RPC it
/// calls, and a builder for its JSON-Schema input contract.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// The exact `AtlasQuery` RPC this tool invokes. Load-bearing: the read-only
    /// guarantee test asserts every entry here is a non-mutating query call.
    #[allow(dead_code)] // read by the read-only-guarantee test, not at runtime
    pub source_rpc: &'static str,
    pub input_schema: fn() -> Value,
}

/// The complete, read-only tool set. Adding a tool here is the *only* way to
/// grow the MCP surface, so keeping this table read-only keeps the whole server
/// read-only.
pub const CATALOG: &[ToolDef] = &[
    ToolDef {
        name: "top_consumers",
        description: "Top processes by CPU right now, with system gauges and attributed GPU adapter, sensor, memory, provider-health, and fallback evidence. Maps to AtlasQuery.GetSnapshot.",
        source_rpc: "AtlasQuery.GetSnapshot",
        input_schema: schema_top_consumers,
    },
    ToolDef {
        name: "query_timeline",
        description: "Decimated min/max/avg buckets for a metric over a time range (system- or process-scoped). Maps to AtlasQuery.QueryRange.",
        source_rpc: "AtlasQuery.QueryRange",
        input_schema: schema_query_timeline,
    },
    ToolDef {
        name: "find_events",
        description: "Process start/stop events in a time range. Maps to AtlasQuery.ListEvents.",
        source_rpc: "AtlasQuery.ListEvents",
        input_schema: schema_find_events,
    },
    ToolDef {
        name: "search",
        description: "Full-text search over processes, events, and bookmarks. Maps to AtlasQuery.Search.",
        source_rpc: "AtlasQuery.Search",
        input_schema: schema_search,
    },
    ToolDef {
        name: "list_incidents",
        description: "Detected incidents (CPU saturation, memory pressure, disk latency) in a range. Maps to AtlasQuery.ListIncidents.",
        source_rpc: "AtlasQuery.ListIncidents",
        input_schema: schema_list_incidents,
    },
    ToolDef {
        name: "explain_incident",
        description: "Evidence-based diagnosis of an incident (by id) or an ad-hoc range: observed facts, ranked contributing factors, confidence. Maps to AtlasQuery.Diagnose.",
        source_rpc: "AtlasQuery.Diagnose",
        input_schema: schema_explain_incident,
    },
    ToolDef {
        name: "explain_process",
        description: "Deep detail for one process (identity, signature, integrity, resources). Maps to AtlasQuery.GetProcessDetail.",
        source_rpc: "AtlasQuery.GetProcessDetail",
        input_schema: schema_explain_process,
    },
    ToolDef {
        name: "list_services",
        description: "Windows services inventory (state, start type, account). Maps to AtlasQuery.ListServices.",
        source_rpc: "AtlasQuery.ListServices",
        input_schema: schema_list_services,
    },
    ToolDef {
        name: "list_startup",
        description: "Startup inventory (Run keys, Startup folders, tasks, services). Maps to AtlasQuery.ListStartup.",
        source_rpc: "AtlasQuery.ListStartup",
        input_schema: schema_no_args,
    },
    ToolDef {
        name: "list_connections",
        description: "Active TCP/UDP connections (owner pid, endpoints, best-effort DNS domain). Maps to AtlasQuery.ListConnections.",
        source_rpc: "AtlasQuery.ListConnections",
        input_schema: schema_list_connections,
    },
    ToolDef {
        name: "list_scheduled_tasks",
        description: "Scheduled tasks (triggers, action, last/next run). Maps to AtlasQuery.ListScheduledTasks.",
        source_rpc: "AtlasQuery.ListScheduledTasks",
        input_schema: schema_list_scheduled_tasks,
    },
];

/// Lazily-connected read-only client wrapper. Holds the tokio runtime and dials
/// the `AtlasQuery` service on first use; a missing service surfaces as a clean
/// error, never a panic.
pub struct Connection {
    rt: tokio::runtime::Runtime,
    pipe: String,
    client: Option<AtlasQueryClient<Channel>>,
}

impl Connection {
    pub fn new(pipe: String) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;
        Ok(Self {
            rt,
            pipe,
            client: None,
        })
    }

    /// Dials the named pipe on first call and caches the `AtlasQuery` client.
    /// Read-only: this is the ONLY client this crate ever builds.
    fn ensure_client(&mut self) -> Result<&mut AtlasQueryClient<Channel>> {
        if self.client.is_none() {
            let channel = self
                .rt
                .block_on(atlas_ipc::connect(&self.pipe))
                .map_err(|e| {
                    anyhow!(
                        "cannot reach the Atlas service on pipe '{}': {e}. Is `atlas-service serve` running?",
                        self.pipe
                    )
                })?;
            self.client = Some(AtlasQueryClient::new(channel));
        }
        Ok(self.client.as_mut().unwrap())
    }

    /// Runs one RPC on a cloned client handle (cheap — shares the channel),
    /// blocking on the runtime. Kept generic so every tool reuses the same
    /// connect-and-call path.
    fn call<T, F, Fut>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(AtlasQueryClient<Channel>) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    {
        // Clone ends the &mut borrow from ensure_client before we borrow rt.
        let client = self.ensure_client()?.clone();
        let resp = self
            .rt
            .block_on(f(client))
            .map_err(|status| anyhow!("{}: {}", status.code(), status.message()))?;
        Ok(resp.into_inner())
    }
}

/// Dispatch a tool call by name. `Ok(value)` is the structured result payload;
/// `Err` is a tool-execution failure (service down, bad args, RPC error) that
/// the server renders as an MCP `isError` result rather than a protocol error.
pub fn dispatch(conn: &mut Connection, red: &Redactor, name: &str, args: &Value) -> Result<Value> {
    match name {
        "top_consumers" => top_consumers(conn, red, args),
        "query_timeline" => query_timeline(conn, red, args),
        "find_events" => find_events(conn, red, args),
        "search" => search(conn, red, args),
        "list_incidents" => list_incidents(conn, red, args),
        "explain_incident" => explain_incident(conn, red, args),
        "explain_process" => explain_process(conn, red, args),
        "list_services" => list_services(conn, red, args),
        "list_startup" => list_startup(conn, red, args),
        "list_connections" => list_connections(conn, red, args),
        "list_scheduled_tasks" => list_scheduled_tasks(conn, red, args),
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

/// The `tools/list` payload built from [`CATALOG`].
pub fn tools_list() -> Value {
    let tools: Vec<Value> = CATALOG
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": (t.input_schema)(),
            })
        })
        .collect();
    json!({ "tools": tools })
}

// ---------------------------------------------------------------------------
// Grounding
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A machine-readable grounding block attached to every tool result: which RPC
/// produced it, when, a suggested citation string, and the redaction mode (so a
/// downstream reader knows the payload was scrubbed).
fn grounding(source_rpc: &str, citation: String) -> Value {
    json!({
        "source_rpc": source_rpc,
        "captured_at_ms": now_ms(),
        "citation": citation,
        "redaction": "mcp-strict",
        "read_only": true,
    })
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn arg_u32(args: &Value, key: &str, default: u32) -> u32 {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(default)
}

fn arg_i64(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(|v| v.as_i64()).unwrap_or(default)
}

fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// Builds a `TimeRange` from `from_ms` / `to_ms` args (0 = unbounded end).
fn arg_range(args: &Value) -> TimeRange {
    TimeRange {
        from_ms: arg_i64(args, "from_ms", 0),
        to_ms: arg_i64(args, "to_ms", 0),
    }
}

fn range_json(range: Option<&TimeRange>) -> Value {
    match range {
        Some(r) => json!({ "from_ms": r.from_ms, "to_ms": r.to_ms }),
        None => Value::Null,
    }
}

// ---------------------------------------------------------------------------
// Enum → readable name helpers (open enums arrive as i32).
// ---------------------------------------------------------------------------

fn role_name(v: i32) -> &'static str {
    ProcessRole::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("PROCESS_ROLE_UNSPECIFIED")
}
fn incident_kind_name(v: i32) -> &'static str {
    IncidentKind::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("INCIDENT_KIND_UNSPECIFIED")
}
fn severity_name(v: i32) -> &'static str {
    Severity::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("SEVERITY_UNSPECIFIED")
}
fn confidence_name(v: i32) -> &'static str {
    Confidence::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("CONFIDENCE_UNSPECIFIED")
}
fn service_state_name(v: i32) -> &'static str {
    ServiceState::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("SERVICE_STATE_UNSPECIFIED")
}
fn start_type_name(v: i32) -> &'static str {
    ServiceStartType::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("SERVICE_START_TYPE_UNSPECIFIED")
}
fn startup_source_name(v: i32) -> &'static str {
    StartupSource::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("STARTUP_SOURCE_UNSPECIFIED")
}
fn l4_name(v: i32) -> &'static str {
    L4Protocol::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("L4_PROTOCOL_UNSPECIFIED")
}
fn tcp_state_name(v: i32) -> &'static str {
    TcpState::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("TCP_STATE_UNSPECIFIED")
}

/// Maps a metric arg (proto SCREAMING name or a friendly alias) to `MetricKind`.
fn parse_metric(s: &str) -> Result<MetricKind> {
    if let Some(k) = MetricKind::from_str_name(s) {
        return Ok(k);
    }
    let k = match s.to_lowercase().as_str() {
        "cpu" | "cpu_permille" => MetricKind::CpuPermille,
        "working_set" | "ws" => MetricKind::WorkingSet,
        "private_bytes" | "private" => MetricKind::PrivateBytes,
        "read_bps" | "read" => MetricKind::ReadBps,
        "write_bps" | "write" => MetricKind::WriteBps,
        "sys_cpu" | "sys_cpu_permille" => MetricKind::SysCpuPermille,
        "sys_mem" | "sys_mem_used" => MetricKind::SysMemUsed,
        "sys_commit" | "sys_commit_used" => MetricKind::SysCommitUsed,
        "sys_process_count" | "sys_procs" => MetricKind::SysProcessCount,
        other => return Err(anyhow!("unknown metric '{other}'")),
    };
    Ok(k)
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

fn top_consumers(conn: &mut Connection, red: &Redactor, args: &Value) -> Result<Value> {
    let top_n = arg_u32(args, "top_n", 10);
    let reply =
        conn.call(|mut c| async move { c.get_snapshot(SnapshotRequest { top_n }).await })?;

    let system = reply.system.map(|s| {
        json!({
            "ts_ms": s.ts_ms,
            "cpu_percent": s.cpu_permille as f64 / 10.0,
            "mem_used_bytes": s.mem_used,
            "mem_total_bytes": s.mem_total,
            "commit_used_bytes": s.commit_used,
            "commit_limit_bytes": s.commit_limit,
            "process_count": s.process_count,
            "thread_count": s.thread_count,
            "handle_count": s.handle_count,
            "gpu_percent": s.gpu_permille as f64 / 10.0,
            "gpu_dedicated_used_bytes": s.gpu_dedicated_used,
            "gpu_dedicated_budget_bytes": s.gpu_dedicated_budget,
            "gpu_shared_used_bytes": s.gpu_shared_used,
            "gpu_shared_budget_bytes": s.gpu_shared_budget,
        })
    });

    let processes: Vec<Value> = reply
        .processes
        .iter()
        .map(|p| {
            json!({
                "pid": p.pid,
                "parent_pid": p.parent_pid,
                "image_name": red.app_name(&p.image_name),
                "app_group": red.app_name(&p.app_group),
                "role": role_name(p.role),
                "cpu_percent": p.cpu_permille as f64 / 10.0,
                "working_set_bytes": p.working_set,
                "private_bytes": p.private_bytes,
                "read_bps": p.read_bps,
                "write_bps": p.write_bps,
                "thread_count": p.thread_count,
                "handle_count": p.handle_count,
                "gpu_percent": p.gpu_permille as f64 / 10.0,
                "gpu_dedicated_bytes": p.gpu_dedicated_bytes,
                "gpu_shared_bytes": p.gpu_shared_bytes,
            })
        })
        .collect();

    let gpu_adapters: Vec<Value> = reply.gpu_adapters.iter().map(|a| json!({
        "adapter_key": a.adapter_key,
        "name": a.name,
        "driver_version": a.driver_version,
        "driver_date": a.driver_date,
        "physical_adapter_index": a.physical_adapter_index,
        "pci": a.pci_identity_available.then(|| format!("{:04X}:{:02X}:{:02X}.{}", a.pci_domain, a.pci_bus, a.pci_device, a.pci_function)),
        "utilization_percent": a.utilization_permille as f64 / 10.0,
        "temperature_c": a.temperature_c,
        "power_w": a.power_w,
        "power_percent": a.power_percent,
        "core_clock_mhz": a.core_clock_mhz,
        "memory_clock_mhz": a.memory_clock_mhz,
        "fan_rpm": a.fan_rpm,
        "fan_percent": a.fan_percent,
        "temperatures": a.temperatures.iter().map(|t| json!({
            "kind": GpuTemperatureKind::try_from(t.kind).map(|v| v.as_str_name()).unwrap_or("UNKNOWN"),
            "celsius": t.celsius,
            "source": GpuTelemetrySource::try_from(t.source).map(|v| v.as_str_name()).unwrap_or("UNKNOWN"),
            "label": t.label,
        })).collect::<Vec<_>>(),
        "throttle_reasons": a.throttle_reasons.iter().map(|v| GpuThrottleReason::try_from(*v).map(|v| v.as_str_name()).unwrap_or("UNKNOWN")).collect::<Vec<_>>(),
        "sensor_availability": a.sensor_availability.iter().map(|v| json!({
            "metric": GpuSensorKind::try_from(v.kind).map(|v| v.as_str_name()).unwrap_or("UNKNOWN"),
            "available": v.available,
            "source": GpuTelemetrySource::try_from(v.source).map(|v| v.as_str_name()).unwrap_or("UNKNOWN"),
            "reason": GpuAvailabilityReason::try_from(v.reason).map(|v| v.as_str_name()).unwrap_or("UNKNOWN"),
            "detail": v.detail,
        })).collect::<Vec<_>>(),
    })).collect();

    Ok(json!({
        "system": system,
        "gpu_adapters": gpu_adapters,
        "gpu_unavailable_reason": reply.gpu_unavailable_reason,
        "processes": processes,
        "returned": processes.len(),
        "grounding": grounding(
            "AtlasQuery.GetSnapshot",
            format!("Atlas snapshot: top {top_n} processes by CPU, captured {}ms epoch", now_ms()),
        ),
    }))
}

fn query_timeline(conn: &mut Connection, _red: &Redactor, args: &Value) -> Result<Value> {
    let metric = parse_metric(arg_str(args, "metric"))?;
    let scope = arg_i64(args, "scope", 0);
    let buckets = arg_u32(args, "buckets", 0);
    let range = arg_range(args);
    let (from_ms, to_ms) = (range.from_ms, range.to_ms);

    let req = QueryRangeRequest {
        metric: metric as i32,
        scope,
        range: Some(range),
        buckets,
    };
    let reply = conn.call(|mut c| async move { c.query_range(req).await })?;

    let buckets_json: Vec<Value> = reply
        .buckets
        .iter()
        .map(|b| {
            json!({
                "start_ms": b.start_ms,
                "min": b.min,
                "max": b.max,
                "avg": b.avg,
                "samples": b.samples,
            })
        })
        .collect();

    // Numeric-only payload → no redactable identifiers.
    Ok(json!({
        "metric": metric.as_str_name(),
        "scope": scope,
        "range": { "from_ms": from_ms, "to_ms": to_ms },
        "buckets": buckets_json,
        "returned": buckets_json.len(),
        "note": if buckets_json.is_empty() {
            "no samples in range (gap renders as missing data, never zero)"
        } else {
            "empty buckets omitted; gaps are missing data, not zero"
        },
        "grounding": grounding(
            "AtlasQuery.QueryRange",
            format!("Atlas timeline of {} (scope {scope}) over [{from_ms}, {to_ms})", metric.as_str_name()),
        ),
    }))
}

fn find_events(conn: &mut Connection, red: &Redactor, args: &Value) -> Result<Value> {
    let range = arg_range(args);
    let limit = arg_u32(args, "limit", 100);
    let kinds: Vec<u32> = args
        .get("kinds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|k| k.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();
    let range_saved = range;

    let req = ListEventsRequest {
        range: Some(range),
        kinds,
        limit,
    };
    let reply = conn.call(|mut c| async move { c.list_events(req).await })?;

    let events: Vec<Value> = reply.events.iter().map(|e| event_json(e, red)).collect();

    Ok(json!({
        "events": events,
        "returned": events.len(),
        "truncated": reply.truncated,
        "range": range_json(Some(&range_saved)),
        "grounding": grounding(
            "AtlasQuery.ListEvents",
            format!("Atlas process start/stop events over [{}, {})", range_saved.from_ms, range_saved.to_ms),
        ),
    }))
}

fn event_json(e: &atlas_ipc::EventRow, red: &Redactor) -> Value {
    json!({
        "ts_ms": e.ts_ms,
        "kind": if e.kind == 0 { "start" } else if e.kind == 1 { "stop" } else { "other" },
        "pid": e.pid,
        "parent_pid": e.parent_pid,
        "session_id": e.session_id,
        "image_name": red.app_name(&e.image_name),
        "exit_status": if e.has_exit_status { Value::from(e.exit_status) } else { Value::Null },
    })
}

fn search(conn: &mut Connection, red: &Redactor, args: &Value) -> Result<Value> {
    let query = arg_str(args, "query").to_string();
    let limit = arg_u32(args, "limit", 50);
    let query_echo = query.clone();

    let req = SearchRequest { query, limit };
    let reply = conn.call(|mut c| async move { c.search(req).await })?;

    let hits: Vec<Value> = reply
        .hits
        .iter()
        .filter_map(|h| h.entity.as_ref())
        .map(|entity| match entity {
            atlas_ipc::v0::search_hit::Entity::Process(p) => json!({
                "type": "process",
                "proc_row_id": p.proc_row_id,
                "pid": p.pid,
                "image_name": red.app_name(&p.image_name),
                "first_seen_ms": p.first_seen_ms,
                "exit_seen_ms": p.exit_seen_ms,
                "live": p.live,
            }),
            atlas_ipc::v0::search_hit::Entity::Event(e) => {
                let mut o = event_json(e, red);
                o.as_object_mut()
                    .unwrap()
                    .insert("type".into(), json!("event"));
                o
            }
            atlas_ipc::v0::search_hit::Entity::Bookmark(b) => json!({
                "type": "bookmark",
                "id": b.id,
                "ts_ms": b.ts_ms,
                "label": red.scrub(&b.label),
                "created_ms": b.created_ms,
            }),
        })
        .collect();

    Ok(json!({
        "query": red.scrub(&query_echo),
        "hits": hits,
        "returned": hits.len(),
        "grounding": grounding(
            "AtlasQuery.Search",
            format!("Atlas search hits for query (redacted), {} results", hits.len()),
        ),
    }))
}

fn list_incidents(conn: &mut Connection, red: &Redactor, args: &Value) -> Result<Value> {
    let range = arg_range(args);
    let range_saved = range;
    let limit = arg_u32(args, "limit", 50);

    let req = ListIncidentsRequest {
        range: Some(range),
        limit,
    };
    let reply = conn.call(|mut c| async move { c.list_incidents(req).await })?;

    let incidents: Vec<Value> = reply
        .incidents
        .iter()
        .map(|i| {
            json!({
                "id": i.id,
                "kind": incident_kind_name(i.kind),
                "start_ms": i.start_ms,
                "end_ms": i.end_ms,
                "ongoing": i.end_ms == 0,
                "severity": severity_name(i.severity),
                "peak_value": i.peak_value,
                "summary": red.scrub(&i.summary),
            })
        })
        .collect();

    Ok(json!({
        "incidents": incidents,
        "returned": incidents.len(),
        "truncated": reply.truncated,
        "range": range_json(Some(&range_saved)),
        "grounding": grounding(
            "AtlasQuery.ListIncidents",
            format!("Atlas detected incidents over [{}, {})", range_saved.from_ms, range_saved.to_ms),
        ),
    }))
}

fn explain_incident(conn: &mut Connection, red: &Redactor, args: &Value) -> Result<Value> {
    let incident_id = arg_i64(args, "incident_id", 0);
    let range = arg_range(args);

    let req = DiagnoseRequest {
        incident_id,
        range: Some(range),
    };
    let reply = conn.call(|mut c| async move { c.diagnose(req).await })?;

    // Pass the engine's honesty markers straight through.
    if !reply.available {
        return Ok(json!({
            "available": false,
            "unavailable_reason": reply.unavailable_reason,
            "incident_id": incident_id,
            "grounding": grounding(
                "AtlasQuery.Diagnose",
                format!("Atlas diagnosis unavailable for incident {incident_id}: {}", reply.unavailable_reason),
            ),
        }));
    }

    let d = reply.diagnosis.unwrap_or_default();
    let evidence: Vec<Value> = d
        .evidence
        .iter()
        .map(|ev| {
            json!({
                "text": red.scrub(&ev.text),
                "ts_ms": ev.ts_ms,
                "metric": ev.metric,
                "value": ev.value,
            })
        })
        .collect();
    let factors: Vec<Value> = d
        .factors
        .iter()
        .map(|f| {
            json!({
                "description": red.scrub(&f.description),
                "confidence": confidence_name(f.confidence),
                "pid": f.pid,
                "image_name": red.app_name(&f.image_name),
                "attribution": f.attribution,
            })
        })
        .collect();
    let alternatives: Vec<Value> = d.alternatives.iter().map(|a| json!(red.scrub(a))).collect();

    Ok(json!({
        "available": true,
        "observed": red.scrub(&d.observed),
        "range": range_json(d.range.as_ref()),
        "overall_confidence": confidence_name(d.overall_confidence),
        "evidence": evidence,
        "factors": factors,
        "alternatives": alternatives,
        "recommendation": red.scrub(&d.recommendation),
        "risk": red.scrub(&d.risk),
        "reversibility": red.scrub(&d.reversibility),
        "verification_plan": red.scrub(&d.verification_plan),
        "grounding": grounding(
            "AtlasQuery.Diagnose",
            format!("Atlas evidence-based diagnosis (confidence {}) for incident {incident_id}", confidence_name(d.overall_confidence)),
        ),
    }))
}

fn explain_process(conn: &mut Connection, red: &Redactor, args: &Value) -> Result<Value> {
    let pid = arg_u32(args, "pid", 0);
    let create_time_100ns = arg_i64(args, "create_time_100ns", 0);

    let req = ProcessDetailRequest {
        pid,
        create_time_100ns,
    };
    let reply = conn.call(|mut c| async move { c.get_process_detail(req).await })?;

    if !reply.available {
        return Ok(json!({
            "available": false,
            "unavailable_reason": reply.unavailable_reason,
            "pid": pid,
            "grounding": grounding(
                "AtlasQuery.GetProcessDetail",
                format!("Atlas process detail unavailable for pid {pid}: {}", reply.unavailable_reason),
            ),
        }));
    }

    let d = reply.detail.unwrap_or_default();
    Ok(json!({
        "available": true,
        "limited": d.limited,
        "pid": d.pid,
        "parent_pid": d.parent_pid,
        "create_time_100ns": d.create_time_100ns,
        "image_name": red.app_name(&d.image_name),
        "image_path": red.path(&d.image_path),
        "command_line": red.command_line(&d.command_line),
        "working_directory": red.path(&d.working_directory),
        "user_name": red.user(&d.user_name),
        "user_sid": red.user(&d.user_sid),
        "session_id": d.session_id,
        "integrity_level": d.integrity_level,
        "elevated": d.elevated,
        "architecture": d.architecture,
        "signature_status": d.signature_status,
        "publisher": red.scrub(&d.publisher),
        "file_version": d.file_version,
        "product_name": red.app_name(&d.product_name),
        "thread_count": d.thread_count,
        "handle_count": d.handle_count,
        "start_time_ms": d.start_time_ms,
        "package_identity": red.app_name(&d.package_identity),
        "grounding": grounding(
            "AtlasQuery.GetProcessDetail",
            format!("Atlas process detail for pid {pid} (signature: {})", d.signature_status),
        ),
    }))
}

fn list_services(conn: &mut Connection, red: &Redactor, args: &Value) -> Result<Value> {
    let filter = arg_str(args, "filter").to_string();
    let req = ListServicesRequest { filter };
    let reply = conn.call(|mut c| async move { c.list_services(req).await })?;

    let services: Vec<Value> = reply
        .services
        .iter()
        .map(|s| {
            json!({
                "name": red.app_name(&s.name),
                "display_name": red.app_name(&s.display_name),
                "description": red.scrub(&s.description),
                "state": service_state_name(s.state),
                "start_type": start_type_name(s.start_type),
                "pid": s.pid,
                "account": red.user(&s.account),
                "binary_path": red.path(&s.binary_path),
                "delayed_auto_start": s.delayed_auto_start,
            })
        })
        .collect();

    Ok(json!({
        "services": services,
        "returned": services.len(),
        "grounding": grounding(
            "AtlasQuery.ListServices",
            format!("Atlas services inventory, {} entries", services.len()),
        ),
    }))
}

fn list_startup(conn: &mut Connection, red: &Redactor, _args: &Value) -> Result<Value> {
    let reply = conn.call(|mut c| async move { c.list_startup(ListStartupRequest {}).await })?;

    let entries: Vec<Value> = reply
        .entries
        .iter()
        .map(|e| {
            json!({
                "name": red.app_name(&e.name),
                "source": startup_source_name(e.source),
                "command": red.command_line(&e.command),
                "publisher": red.scrub(&e.publisher),
                "enabled": e.enabled,
                "scope": e.scope,
            })
        })
        .collect();

    Ok(json!({
        "entries": entries,
        "returned": entries.len(),
        "grounding": grounding(
            "AtlasQuery.ListStartup",
            format!("Atlas startup inventory, {} entries", entries.len()),
        ),
    }))
}

fn list_connections(conn: &mut Connection, red: &Redactor, args: &Value) -> Result<Value> {
    let include_listening = arg_bool(args, "include_listening", false);
    let req = ListConnectionsRequest { include_listening };
    let reply = conn.call(|mut c| async move { c.list_connections(req).await })?;

    let connections: Vec<Value> = reply
        .connections
        .iter()
        .map(|c| {
            json!({
                "pid": c.pid,
                "image_name": red.app_name(&c.image_name),
                "protocol": l4_name(c.protocol),
                "local_addr": c.local_addr,
                "local_port": c.local_port,
                "remote_addr": c.remote_addr,
                "remote_port": c.remote_port,
                "remote_domain": red.domain(&c.remote_domain),
                "tcp_state": tcp_state_name(c.state),
                "is_ipv6": c.is_ipv6,
            })
        })
        .collect();

    Ok(json!({
        "connections": connections,
        "returned": connections.len(),
        "include_listening": include_listening,
        "grounding": grounding(
            "AtlasQuery.ListConnections",
            format!("Atlas network connections, {} entries", connections.len()),
        ),
    }))
}

fn list_scheduled_tasks(conn: &mut Connection, red: &Redactor, args: &Value) -> Result<Value> {
    let filter = arg_str(args, "filter").to_string();
    let req = ListScheduledTasksRequest { filter };
    let reply = conn.call(|mut c| async move { c.list_scheduled_tasks(req).await })?;

    let tasks: Vec<Value> = reply
        .tasks
        .iter()
        .map(|t| {
            json!({
                "name": red.app_name(&t.name),
                "path": red.scrub(&t.path),
                "folder": red.scrub(&t.folder),
                "enabled": t.enabled,
                "triggers": red.scrub(&t.triggers),
                "action": red.command_line(&t.action),
                "last_run_ms": t.last_run_ms,
                "next_run_ms": t.next_run_ms,
                "last_result": t.last_result,
                "author": red.scrub(&t.author),
                "run_as_highest": t.run_as_highest,
                "runs_on_idle": t.runs_on_idle,
                "wakes_to_run": t.wakes_to_run,
            })
        })
        .collect();

    Ok(json!({
        "tasks": tasks,
        "returned": tasks.len(),
        "grounding": grounding(
            "AtlasQuery.ListScheduledTasks",
            format!("Atlas scheduled tasks, {} entries", tasks.len()),
        ),
    }))
}

// ---------------------------------------------------------------------------
// Input schemas (JSON Schema draft-07 subset)
// ---------------------------------------------------------------------------

fn obj_schema(props: Value, required: &[&str]) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), json!("object"));
    o.insert("properties".into(), props);
    o.insert("required".into(), json!(required));
    o.insert("additionalProperties".into(), json!(false));
    Value::Object(o)
}

fn schema_no_args() -> Value {
    obj_schema(json!({}), &[])
}

fn schema_top_consumers() -> Value {
    obj_schema(
        json!({
            "top_n": { "type": "integer", "minimum": 0, "description": "Max processes to return (0 = all). Default 10." }
        }),
        &[],
    )
}

fn schema_query_timeline() -> Value {
    obj_schema(
        json!({
            "metric": { "type": "string", "description": "Metric id or alias: cpu, working_set, private_bytes, read_bps, write_bps, sys_cpu, sys_mem, sys_commit, sys_process_count (or the proto SCREAMING name)." },
            "scope": { "type": "integer", "description": "Process instance row id; 0 for system-scope metrics. Default 0." },
            "from_ms": { "type": "integer", "description": "Range start (epoch ms)." },
            "to_ms": { "type": "integer", "description": "Range end (epoch ms, exclusive)." },
            "buckets": { "type": "integer", "minimum": 0, "description": "Decimation target; 0 = server default (500)." }
        }),
        &["metric", "from_ms", "to_ms"],
    )
}

fn schema_find_events() -> Value {
    obj_schema(
        json!({
            "from_ms": { "type": "integer", "description": "Range start (epoch ms)." },
            "to_ms": { "type": "integer", "description": "Range end (epoch ms, exclusive)." },
            "kinds": { "type": "array", "items": { "type": "integer" }, "description": "Event kinds to include (0=start, 1=stop); empty = all." },
            "limit": { "type": "integer", "minimum": 0, "description": "Max events. Default 100." }
        }),
        &["from_ms", "to_ms"],
    )
}

fn schema_search() -> Value {
    obj_schema(
        json!({
            "query": { "type": "string", "description": "Full-text query over processes/events/bookmarks." },
            "limit": { "type": "integer", "minimum": 0, "description": "Max hits. Default 50." }
        }),
        &["query"],
    )
}

fn schema_list_incidents() -> Value {
    obj_schema(
        json!({
            "from_ms": { "type": "integer", "description": "Range start (epoch ms)." },
            "to_ms": { "type": "integer", "description": "Range end (epoch ms, exclusive)." },
            "limit": { "type": "integer", "minimum": 0, "description": "Max incidents. Default 50." }
        }),
        &["from_ms", "to_ms"],
    )
}

fn schema_explain_incident() -> Value {
    obj_schema(
        json!({
            "incident_id": { "type": "integer", "description": "Detected incident id; 0 = diagnose the ad-hoc range instead." },
            "from_ms": { "type": "integer", "description": "Ad-hoc range start (epoch ms); used when incident_id = 0." },
            "to_ms": { "type": "integer", "description": "Ad-hoc range end (epoch ms, exclusive)." }
        }),
        &[],
    )
}

fn schema_explain_process() -> Value {
    obj_schema(
        json!({
            "pid": { "type": "integer", "minimum": 0, "description": "Process id to inspect." },
            "create_time_100ns": { "type": "integer", "description": "Identity guard against PID reuse; 0 = best-effort by pid." }
        }),
        &["pid"],
    )
}

fn schema_list_services() -> Value {
    obj_schema(
        json!({
            "filter": { "type": "string", "description": "Case-insensitive substring over name/display_name; empty = all." }
        }),
        &[],
    )
}

fn schema_list_connections() -> Value {
    obj_schema(
        json!({
            "include_listening": { "type": "boolean", "description": "Include listening sockets. Default false." }
        }),
        &[],
    )
}

fn schema_list_scheduled_tasks() -> Value {
    obj_schema(
        json!({
            "filter": { "type": "string", "description": "Case-insensitive substring over name/path; empty = all." }
        }),
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The read-only guarantee, enforced structurally: every tool's `source_rpc`
    /// is an `AtlasQuery.*` read call, and none is a known mutating RPC from the
    /// `AtlasControl` / `AtlasRules` services.
    #[test]
    fn no_tool_maps_to_a_mutating_rpc() {
        // The read-only AtlasQuery RPCs this crate is allowed to call.
        const READ_ONLY_RPCS: &[&str] = &[
            "AtlasQuery.GetSnapshot",
            "AtlasQuery.QueryRange",
            "AtlasQuery.ListEvents",
            "AtlasQuery.Search",
            "AtlasQuery.ListIncidents",
            "AtlasQuery.Diagnose",
            "AtlasQuery.GetProcessDetail",
            "AtlasQuery.ListServices",
            "AtlasQuery.ListStartup",
            "AtlasQuery.ListConnections",
            "AtlasQuery.ListScheduledTasks",
        ];
        // Any RPC name containing one of these verbs would mutate the system.
        const MUTATING_MARKERS: &[&str] = &[
            "Prepare",
            "Execute",
            "Create",
            "Update",
            "Delete",
            "Set",
            "Suspend",
            "Resume",
            "Terminate",
            "Kill",
            "Action",
            "Rule",
            "Profile",
            "Bookmark",
        ];
        for t in CATALOG {
            assert!(
                t.source_rpc.starts_with("AtlasQuery."),
                "tool '{}' calls non-AtlasQuery RPC '{}'",
                t.name,
                t.source_rpc
            );
            assert!(
                READ_ONLY_RPCS.contains(&t.source_rpc),
                "tool '{}' maps to '{}', not in the read-only allowlist",
                t.name,
                t.source_rpc
            );
            for marker in MUTATING_MARKERS {
                assert!(
                    !t.source_rpc.contains(marker),
                    "tool '{}' RPC '{}' contains mutating marker '{}'",
                    t.name,
                    t.source_rpc,
                    marker
                );
            }
        }
    }

    #[test]
    fn tool_names_carry_no_mutating_verbs() {
        const BANNED: &[&str] = &[
            "kill",
            "suspend",
            "terminate",
            "resume",
            "stop",
            "start_process",
            "create",
            "delete",
            "set_",
            "apply",
            "execute",
            "prepare",
        ];
        for t in CATALOG {
            let lname = t.name.to_lowercase();
            for b in BANNED {
                assert!(
                    !lname.contains(b),
                    "tool name '{}' contains banned mutating verb '{}'",
                    t.name,
                    b
                );
            }
        }
    }

    #[test]
    fn catalog_matches_briefed_tool_set() {
        let names: Vec<&str> = CATALOG.iter().map(|t| t.name).collect();
        for expected in [
            "top_consumers",
            "query_timeline",
            "find_events",
            "search",
            "list_incidents",
            "explain_incident",
            "explain_process",
            "list_services",
            "list_startup",
            "list_connections",
            "list_scheduled_tasks",
        ] {
            assert!(names.contains(&expected), "missing tool '{expected}'");
        }
        assert_eq!(names.len(), 11, "unexpected tool count");
    }

    #[test]
    fn tools_list_is_well_formed() {
        let list = tools_list();
        let arr = list["tools"].as_array().expect("tools array");
        assert_eq!(arr.len(), CATALOG.len());
        for t in arr {
            assert!(t["name"].is_string());
            assert!(t["description"].is_string());
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn metric_aliases_resolve() {
        assert_eq!(parse_metric("cpu").unwrap(), MetricKind::CpuPermille);
        assert_eq!(
            parse_metric("SYS_CPU_PERMILLE").unwrap(),
            MetricKind::SysCpuPermille
        );
        assert!(parse_metric("nonsense").is_err());
    }
}
