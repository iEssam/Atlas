//! Subcommands: each maps 1:1 onto exactly one read-only RPC.
//!
//! # Read-only by construction
//! [`COMMAND_RPCS`] is the single source of truth for the command→RPC mapping.
//! Every entry is a read call on `AtlasQuery` or the read-only `AtlasRules.
//! ListRules`. The guarantee is enforced structurally by
//! [`tests::no_command_maps_to_a_mutating_rpc`]: no command's RPC carries a
//! mutating verb. The CLI has no subcommand that creates, updates, deletes,
//! enables, suspends, terminates, or otherwise changes anything — mutations are
//! performed in the Atlas app, not here.
//!
//! Each command produces two shapes of the same data:
//! * a `serde_json::Value` for `--json` (scriptable, machine-readable), and
//! * a human table for the default terminal view.
//!
//! The JSON/table builders are pure functions of the reply structs so they are
//! unit-testable against fixtures without a live service.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use atlas_ipc::{
    CapabilitiesReply, DiagnoseReply, DiagnoseRequest, FindResourceOwnersReply,
    FindResourceOwnersRequest, ListConnectionsReply, ListConnectionsRequest, ListIncidentsReply,
    ListIncidentsRequest, ListListeningPortsReply, ListListeningPortsRequest, ListRulesReply,
    ListRulesRequest, ListScheduledTasksReply, ListScheduledTasksRequest, ListServicesReply,
    ListServicesRequest, ListStartupReply, ListStartupRequest, MetricKind, QueryRangeReply,
    QueryRangeRequest, SearchReply, SearchRequest, SnapshotReply, SnapshotRequest, StartupSource,
    TimeRange,
};

use crate::client::Connection;
use crate::render::{
    bytes, confidence_name, incident_kind_name, l4_name, permille_pct, priority_name, role_name,
    service_state_name, severity_name, start_type_name, tcp_state_name, ts, Table,
};

/// Command name → the exact RPC it invokes. Load-bearing: the read-only
/// guarantee test asserts each is a non-mutating query call.
#[allow(dead_code)] // read by the read-only-guarantee test, not at runtime
pub const COMMAND_RPCS: &[(&str, &str)] = &[
    ("top", "AtlasQuery.GetSnapshot"),
    ("ports", "AtlasQuery.ListListeningPorts"),
    ("connections", "AtlasQuery.ListConnections"),
    ("locks", "AtlasQuery.FindResourceOwners"),
    ("history", "AtlasQuery.QueryRange"),
    ("incidents", "AtlasQuery.ListIncidents"),
    ("diagnose", "AtlasQuery.Diagnose"),
    ("services", "AtlasQuery.ListServices"),
    ("startup", "AtlasQuery.ListStartup"),
    ("tasks", "AtlasQuery.ListScheduledTasks"),
    ("search", "AtlasQuery.Search"),
    ("rules", "AtlasRules.ListRules"),
    ("capabilities", "AtlasQuery.GetCapabilities"),
];

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Emits a result either as pretty JSON (`--json`) or the human table.
fn emit(json_mode: bool, value: Value, table: String) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        );
    } else {
        print!("{table}");
    }
}

// ===========================================================================
// top → GetSnapshot
// ===========================================================================

pub fn top(conn: &mut Connection, json_mode: bool, limit: u32) -> Result<()> {
    let reply =
        conn.query(|mut c| async move { c.get_snapshot(SnapshotRequest { top_n: limit }).await })?;
    emit(json_mode, snapshot_json(&reply), snapshot_table(&reply));
    Ok(())
}

pub fn snapshot_json(reply: &SnapshotReply) -> Value {
    let system = reply.system.as_ref().map(|s| {
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
        })
    });
    let processes: Vec<Value> = reply
        .processes
        .iter()
        .map(|p| {
            json!({
                "pid": p.pid,
                "parent_pid": p.parent_pid,
                "image_name": p.image_name,
                "app_group": p.app_group,
                "role": role_name(p.role),
                "cpu_percent": p.cpu_permille as f64 / 10.0,
                "working_set_bytes": p.working_set,
                "private_bytes": p.private_bytes,
                "read_bps": p.read_bps,
                "write_bps": p.write_bps,
                "thread_count": p.thread_count,
                "handle_count": p.handle_count,
            })
        })
        .collect();
    json!({ "system": system, "processes": processes, "returned": processes.len() })
}

pub fn snapshot_table(reply: &SnapshotReply) -> String {
    let mut out = String::new();
    if let Some(s) = reply.system.as_ref() {
        out.push_str(&format!(
            "system  cpu {}  mem {}/{}  commit {}/{}  procs {}  threads {}  handles {}\n\n",
            permille_pct(s.cpu_permille),
            bytes(s.mem_used),
            bytes(s.mem_total),
            bytes(s.commit_used),
            bytes(s.commit_limit),
            s.process_count,
            s.thread_count,
            s.handle_count,
        ));
    }
    let mut t = Table::new(&[
        "PID",
        "CPU",
        "WORKING SET",
        "PRIVATE",
        "THR",
        "HND",
        "IMAGE",
    ]);
    for p in &reply.processes {
        t.push(vec![
            p.pid.to_string(),
            permille_pct(p.cpu_permille),
            bytes(p.working_set),
            bytes(p.private_bytes),
            p.thread_count.to_string(),
            p.handle_count.to_string(),
            p.image_name.clone(),
        ]);
    }
    out.push_str(&t.render());
    out
}

// ===========================================================================
// ports → ListListeningPorts
// ===========================================================================

pub fn ports(conn: &mut Connection, json_mode: bool) -> Result<()> {
    let reply = conn
        .query(|mut c| async move { c.list_listening_ports(ListListeningPortsRequest {}).await })?;
    emit(json_mode, ports_json(&reply), ports_table(&reply));
    Ok(())
}

pub fn ports_json(reply: &ListListeningPortsReply) -> Value {
    let ports: Vec<Value> = reply
        .ports
        .iter()
        .map(|p| {
            json!({
                "protocol": l4_name(p.protocol),
                "bind_addr": p.bind_addr,
                "port": p.port,
                "pid": p.pid,
                "image_name": p.image_name,
                "is_ipv6": p.is_ipv6,
            })
        })
        .collect();
    json!({ "ports": ports, "returned": ports.len() })
}

pub fn ports_table(reply: &ListListeningPortsReply) -> String {
    let mut t = Table::new(&["PROTO", "BIND", "PORT", "PID", "IMAGE"]);
    for p in &reply.ports {
        t.push(vec![
            l4_name(p.protocol).to_string(),
            p.bind_addr.clone(),
            p.port.to_string(),
            p.pid.to_string(),
            p.image_name.clone(),
        ]);
    }
    t.render()
}

// ===========================================================================
// connections → ListConnections
// ===========================================================================

pub fn connections(conn: &mut Connection, json_mode: bool) -> Result<()> {
    let reply = conn.query(|mut c| async move {
        c.list_connections(ListConnectionsRequest {
            include_listening: false,
        })
        .await
    })?;
    emit(
        json_mode,
        connections_json(&reply),
        connections_table(&reply),
    );
    Ok(())
}

pub fn connections_json(reply: &ListConnectionsReply) -> Value {
    let conns: Vec<Value> = reply
        .connections
        .iter()
        .map(|c| {
            json!({
                "pid": c.pid,
                "image_name": c.image_name,
                "protocol": l4_name(c.protocol),
                "local_addr": c.local_addr,
                "local_port": c.local_port,
                "remote_addr": c.remote_addr,
                "remote_port": c.remote_port,
                "remote_domain": c.remote_domain,
                "tcp_state": tcp_state_name(c.state),
                "is_ipv6": c.is_ipv6,
            })
        })
        .collect();
    json!({ "connections": conns, "returned": conns.len() })
}

pub fn connections_table(reply: &ListConnectionsReply) -> String {
    let mut t = Table::new(&["PROTO", "LOCAL", "REMOTE", "STATE", "PID", "IMAGE"]);
    for c in &reply.connections {
        t.push(vec![
            l4_name(c.protocol).to_string(),
            format!("{}:{}", c.local_addr, c.local_port),
            format!("{}:{}", c.remote_addr, c.remote_port),
            tcp_state_name(c.state).to_string(),
            c.pid.to_string(),
            c.image_name.clone(),
        ]);
    }
    t.render()
}

// ===========================================================================
// locks <path> → FindResourceOwners
// ===========================================================================

pub fn locks(conn: &mut Connection, json_mode: bool, path: String) -> Result<()> {
    let req = FindResourceOwnersRequest { path: path.clone() };
    let reply = conn.query(|mut c| async move { c.find_resource_owners(req).await })?;
    emit(
        json_mode,
        locks_json(&reply, &path),
        locks_table(&reply, &path),
    );
    Ok(())
}

pub fn locks_json(reply: &FindResourceOwnersReply, path: &str) -> Value {
    let owners: Vec<Value> = reply
        .owners
        .iter()
        .map(|o| {
            json!({
                "pid": o.pid,
                "image_name": o.image_name,
                "image_path": o.image_path,
                "description": o.description,
                "is_service": o.is_service,
            })
        })
        .collect();
    json!({
        "path": path,
        "available": reply.available,
        "unavailable_reason": reply.unavailable_reason,
        "owners": owners,
        "returned": owners.len(),
    })
}

pub fn locks_table(reply: &FindResourceOwnersReply, path: &str) -> String {
    if !reply.available {
        return format!(
            "resource ownership unavailable for {path}: {}\n",
            reply.unavailable_reason
        );
    }
    if reply.owners.is_empty() {
        return format!("no process is holding {path}\n");
    }
    let mut t = Table::new(&["PID", "IMAGE", "SVC", "DESCRIPTION"]);
    for o in &reply.owners {
        t.push(vec![
            o.pid.to_string(),
            o.image_name.clone(),
            if o.is_service { "yes" } else { "no" }.to_string(),
            o.description.clone(),
        ]);
    }
    t.render()
}

// ===========================================================================
// history --metric M --minutes N → QueryRange
// ===========================================================================

/// Parses a metric id, accepting the proto SCREAMING name or a friendly alias
/// with either hyphens or underscores (e.g. `sys-cpu`, `sys_cpu`, `cpu`).
pub fn parse_metric(s: &str) -> Result<MetricKind> {
    if let Some(k) = MetricKind::from_str_name(s) {
        return Ok(k);
    }
    let norm = s.to_lowercase().replace('-', "_");
    let k = match norm.as_str() {
        "cpu" | "cpu_permille" => MetricKind::CpuPermille,
        "working_set" | "ws" => MetricKind::WorkingSet,
        "private_bytes" | "private" => MetricKind::PrivateBytes,
        "read_bps" | "read" => MetricKind::ReadBps,
        "write_bps" | "write" => MetricKind::WriteBps,
        "sys_cpu" | "sys_cpu_permille" => MetricKind::SysCpuPermille,
        "sys_mem" | "sys_mem_used" => MetricKind::SysMemUsed,
        "sys_commit" | "sys_commit_used" => MetricKind::SysCommitUsed,
        "sys_process_count" | "sys_procs" => MetricKind::SysProcessCount,
        other => return Err(anyhow!("unknown metric '{other}' (try: sys-cpu, sys-mem, sys-commit, cpu, working-set, private-bytes, read-bps, write-bps)")),
    };
    Ok(k)
}

pub fn history(
    conn: &mut Connection,
    json_mode: bool,
    metric: String,
    minutes: i64,
    scope: i64,
    buckets: u32,
) -> Result<()> {
    let kind = parse_metric(&metric)?;
    let to_ms = now_ms();
    let from_ms = to_ms - minutes.max(0) * 60_000;
    let req = QueryRangeRequest {
        metric: kind as i32,
        scope,
        range: Some(TimeRange { from_ms, to_ms }),
        buckets,
    };
    let reply = conn.query(|mut c| async move { c.query_range(req).await })?;
    emit(
        json_mode,
        history_json(&reply, kind, scope, from_ms, to_ms),
        history_table(&reply, kind),
    );
    Ok(())
}

pub fn history_json(
    reply: &QueryRangeReply,
    kind: MetricKind,
    scope: i64,
    from_ms: i64,
    to_ms: i64,
) -> Value {
    let buckets: Vec<Value> = reply
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
    json!({
        "metric": kind.as_str_name(),
        "scope": scope,
        "range": { "from_ms": from_ms, "to_ms": to_ms },
        "buckets": buckets,
        "returned": buckets.len(),
        "note": "empty buckets omitted; gaps are missing data, not zero",
    })
}

pub fn history_table(reply: &QueryRangeReply, kind: MetricKind) -> String {
    if reply.buckets.is_empty() {
        return format!(
            "no samples for {} in range (gaps render as missing data, never zero)\n",
            kind.as_str_name()
        );
    }
    let mut t = Table::new(&["START_MS", "MIN", "MAX", "AVG", "SAMPLES"]);
    for b in &reply.buckets {
        t.push(vec![
            b.start_ms.to_string(),
            format!("{:.2}", b.min),
            format!("{:.2}", b.max),
            format!("{:.2}", b.avg),
            b.samples.to_string(),
        ]);
    }
    t.render()
}

// ===========================================================================
// incidents --minutes N → ListIncidents
// ===========================================================================

pub fn incidents(conn: &mut Connection, json_mode: bool, minutes: i64, limit: u32) -> Result<()> {
    let to_ms = now_ms();
    let from_ms = to_ms - minutes.max(0) * 60_000;
    let req = ListIncidentsRequest {
        range: Some(TimeRange { from_ms, to_ms }),
        limit,
    };
    let reply = conn.query(|mut c| async move { c.list_incidents(req).await })?;
    emit(json_mode, incidents_json(&reply), incidents_table(&reply));
    Ok(())
}

pub fn incidents_json(reply: &ListIncidentsReply) -> Value {
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
                "summary": i.summary,
            })
        })
        .collect();
    json!({ "incidents": incidents, "returned": incidents.len(), "truncated": reply.truncated })
}

pub fn incidents_table(reply: &ListIncidentsReply) -> String {
    if reply.incidents.is_empty() {
        return "no incidents in range\n".to_string();
    }
    let mut t = Table::new(&[
        "ID", "KIND", "SEVERITY", "START_MS", "END_MS", "PEAK", "SUMMARY",
    ]);
    for i in &reply.incidents {
        t.push(vec![
            i.id.to_string(),
            incident_kind_name(i.kind).to_string(),
            severity_name(i.severity).to_string(),
            i.start_ms.to_string(),
            if i.end_ms == 0 {
                "ongoing".to_string()
            } else {
                i.end_ms.to_string()
            },
            format!("{:.2}", i.peak_value),
            i.summary.clone(),
        ]);
    }
    t.render()
}

// ===========================================================================
// diagnose --incident ID → Diagnose
// ===========================================================================

pub fn diagnose(conn: &mut Connection, json_mode: bool, incident_id: i64) -> Result<()> {
    let req = DiagnoseRequest {
        incident_id,
        range: None,
    };
    let reply = conn.query(|mut c| async move { c.diagnose(req).await })?;
    emit(
        json_mode,
        diagnose_json(&reply, incident_id),
        diagnose_table(&reply, incident_id),
    );
    Ok(())
}

pub fn diagnose_json(reply: &DiagnoseReply, incident_id: i64) -> Value {
    if !reply.available {
        return json!({
            "available": false,
            "unavailable_reason": reply.unavailable_reason,
            "incident_id": incident_id,
        });
    }
    let d = reply.diagnosis.clone().unwrap_or_default();
    let evidence: Vec<Value> = d
        .evidence
        .iter()
        .map(|ev| json!({ "text": ev.text, "ts_ms": ev.ts_ms, "metric": ev.metric, "value": ev.value }))
        .collect();
    let factors: Vec<Value> = d
        .factors
        .iter()
        .map(|f| {
            json!({
                "description": f.description,
                "confidence": confidence_name(f.confidence),
                "pid": f.pid,
                "image_name": f.image_name,
                "attribution": f.attribution,
            })
        })
        .collect();
    json!({
        "available": true,
        "incident_id": incident_id,
        "observed": d.observed,
        "overall_confidence": confidence_name(d.overall_confidence),
        "evidence": evidence,
        "factors": factors,
        "alternatives": d.alternatives,
        "recommendation": d.recommendation,
        "risk": d.risk,
        "reversibility": d.reversibility,
        "verification_plan": d.verification_plan,
    })
}

pub fn diagnose_table(reply: &DiagnoseReply, incident_id: i64) -> String {
    if !reply.available {
        return format!(
            "diagnosis unavailable for incident {incident_id}: {}\n",
            reply.unavailable_reason
        );
    }
    let d = reply.diagnosis.clone().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!(
        "incident {incident_id}  confidence {}\n",
        confidence_name(d.overall_confidence)
    ));
    out.push_str(&format!("observed: {}\n", d.observed));
    if !d.factors.is_empty() {
        out.push_str("\ncontributing factors (correlation, not proof):\n");
        for f in &d.factors {
            out.push_str(&format!(
                "  - [{}] pid {} {} — attribution {:.1}%: {}\n",
                confidence_name(f.confidence),
                f.pid,
                f.image_name,
                f.attribution * 100.0,
                f.description,
            ));
        }
    }
    if !d.recommendation.is_empty() {
        out.push_str(&format!("\nrecommendation: {}\n", d.recommendation));
        out.push_str(&format!("risk: {}\n", d.risk));
        out.push_str(&format!("reversibility: {}\n", d.reversibility));
        out.push_str(&format!("verification: {}\n", d.verification_plan));
    }
    out
}

// ===========================================================================
// services --filter X → ListServices
// ===========================================================================

pub fn services(conn: &mut Connection, json_mode: bool, filter: String) -> Result<()> {
    let req = ListServicesRequest { filter };
    let reply = conn.query(|mut c| async move { c.list_services(req).await })?;
    emit(json_mode, services_json(&reply), services_table(&reply));
    Ok(())
}

pub fn services_json(reply: &ListServicesReply) -> Value {
    let services: Vec<Value> = reply
        .services
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "display_name": s.display_name,
                "description": s.description,
                "state": service_state_name(s.state),
                "start_type": start_type_name(s.start_type),
                "pid": s.pid,
                "account": s.account,
                "binary_path": s.binary_path,
                "delayed_auto_start": s.delayed_auto_start,
            })
        })
        .collect();
    json!({ "services": services, "returned": services.len() })
}

pub fn services_table(reply: &ListServicesReply) -> String {
    let mut t = Table::new(&["NAME", "STATE", "START", "PID", "DISPLAY"]);
    for s in &reply.services {
        t.push(vec![
            s.name.clone(),
            service_state_name(s.state).to_string(),
            start_type_name(s.start_type).to_string(),
            if s.pid == 0 {
                "-".to_string()
            } else {
                s.pid.to_string()
            },
            s.display_name.clone(),
        ]);
    }
    t.render()
}

// ===========================================================================
// startup → ListStartup
// ===========================================================================

pub fn startup(conn: &mut Connection, json_mode: bool) -> Result<()> {
    let reply = conn.query(|mut c| async move { c.list_startup(ListStartupRequest {}).await })?;
    emit(json_mode, startup_json(&reply), startup_table(&reply));
    Ok(())
}

fn startup_source_name(v: i32) -> &'static str {
    StartupSource::try_from(v)
        .map(|e| e.as_str_name())
        .unwrap_or("STARTUP_SOURCE_UNSPECIFIED")
}

pub fn startup_json(reply: &ListStartupReply) -> Value {
    let entries: Vec<Value> = reply
        .entries
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "source": startup_source_name(e.source),
                "command": e.command,
                "publisher": e.publisher,
                "enabled": e.enabled,
                "scope": e.scope,
            })
        })
        .collect();
    json!({ "entries": entries, "returned": entries.len() })
}

pub fn startup_table(reply: &ListStartupReply) -> String {
    let mut t = Table::new(&["NAME", "SOURCE", "ENABLED", "SCOPE", "COMMAND"]);
    for e in &reply.entries {
        t.push(vec![
            e.name.clone(),
            startup_source_name(e.source).to_string(),
            if e.enabled { "yes" } else { "no" }.to_string(),
            e.scope.clone(),
            e.command.clone(),
        ]);
    }
    t.render()
}

// ===========================================================================
// tasks --filter X → ListScheduledTasks
// ===========================================================================

pub fn tasks(conn: &mut Connection, json_mode: bool, filter: String) -> Result<()> {
    let req = ListScheduledTasksRequest { filter };
    let reply = conn.query(|mut c| async move { c.list_scheduled_tasks(req).await })?;
    emit(json_mode, tasks_json(&reply), tasks_table(&reply));
    Ok(())
}

pub fn tasks_json(reply: &ListScheduledTasksReply) -> Value {
    let tasks: Vec<Value> = reply
        .tasks
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "path": t.path,
                "folder": t.folder,
                "enabled": t.enabled,
                "triggers": t.triggers,
                "action": t.action,
                "last_run_ms": t.last_run_ms,
                "next_run_ms": t.next_run_ms,
                "last_result": t.last_result,
                "author": t.author,
            })
        })
        .collect();
    json!({ "tasks": tasks, "returned": tasks.len() })
}

pub fn tasks_table(reply: &ListScheduledTasksReply) -> String {
    let mut t = Table::new(&["ENABLED", "NEXT_RUN", "PATH"]);
    for task in &reply.tasks {
        t.push(vec![
            if task.enabled { "yes" } else { "no" }.to_string(),
            ts(task.next_run_ms),
            task.path.clone(),
        ]);
    }
    t.render()
}

// ===========================================================================
// search <query> → Search
// ===========================================================================

pub fn search(conn: &mut Connection, json_mode: bool, query: String, limit: u32) -> Result<()> {
    let req = SearchRequest {
        query: query.clone(),
        limit,
    };
    let reply = conn.query(|mut c| async move { c.search(req).await })?;
    emit(json_mode, search_json(&reply, &query), search_table(&reply));
    Ok(())
}

pub fn search_json(reply: &SearchReply, query: &str) -> Value {
    let hits: Vec<Value> = reply
        .hits
        .iter()
        .filter_map(|h| h.entity.as_ref())
        .map(|entity| match entity {
            atlas_ipc::v0::search_hit::Entity::Process(p) => json!({
                "type": "process",
                "proc_row_id": p.proc_row_id,
                "pid": p.pid,
                "image_name": p.image_name,
                "first_seen_ms": p.first_seen_ms,
                "exit_seen_ms": p.exit_seen_ms,
                "live": p.live,
            }),
            atlas_ipc::v0::search_hit::Entity::Event(e) => json!({
                "type": "event",
                "ts_ms": e.ts_ms,
                "kind": if e.kind == 0 { "start" } else if e.kind == 1 { "stop" } else { "other" },
                "pid": e.pid,
                "image_name": e.image_name,
            }),
            atlas_ipc::v0::search_hit::Entity::Bookmark(b) => json!({
                "type": "bookmark",
                "id": b.id,
                "ts_ms": b.ts_ms,
                "label": b.label,
            }),
        })
        .collect();
    json!({ "query": query, "hits": hits, "returned": hits.len() })
}

pub fn search_table(reply: &SearchReply) -> String {
    if reply.hits.is_empty() {
        return "no results\n".to_string();
    }
    let mut t = Table::new(&["TYPE", "ID/PID", "DETAIL"]);
    for h in reply.hits.iter().filter_map(|h| h.entity.as_ref()) {
        let (ty, id, detail) = match h {
            atlas_ipc::v0::search_hit::Entity::Process(p) => (
                "process",
                p.pid.to_string(),
                format!("{} (live: {})", p.image_name, p.live),
            ),
            atlas_ipc::v0::search_hit::Entity::Event(e) => (
                "event",
                e.pid.to_string(),
                format!(
                    "{} {}",
                    if e.kind == 0 { "start" } else { "stop" },
                    e.image_name
                ),
            ),
            atlas_ipc::v0::search_hit::Entity::Bookmark(b) => {
                ("bookmark", b.id.to_string(), b.label.clone())
            }
        };
        t.push(vec![ty.to_string(), id, detail]);
    }
    t.render()
}

// ===========================================================================
// rules → AtlasRules.ListRules (READ ONLY)
// ===========================================================================

pub fn rules(conn: &mut Connection, json_mode: bool) -> Result<()> {
    // The ONLY AtlasRules call the CLI makes — a read. No create/update/delete/
    // enable, no profile mutation.
    let reply = conn.rules(|mut c| async move { c.list_rules(ListRulesRequest {}).await })?;
    emit(json_mode, rules_json(&reply), rules_table(&reply));
    Ok(())
}

pub fn rules_json(reply: &ListRulesReply) -> Value {
    let rules: Vec<Value> = reply
        .rules
        .iter()
        .map(|r| {
            let action = r.action.as_ref();
            json!({
                "id": r.id,
                "name": r.name,
                "enabled": r.enabled,
                "match_image": r.match_image,
                "trigger": r.trigger,
                "precedence": r.precedence,
                "created_ms": r.created_ms,
                "action": action.map(|a| json!({
                    "priority": priority_name(a.priority),
                    "affinity_mode": a.affinity_mode,
                    "affinity_mask": a.affinity_mask,
                    "eco_qos": a.eco_qos,
                })),
            })
        })
        .collect();
    json!({ "rules": rules, "returned": rules.len() })
}

pub fn rules_table(reply: &ListRulesReply) -> String {
    if reply.rules.is_empty() {
        return "no rules defined (create rules in the Atlas app)\n".to_string();
    }
    let mut t = Table::new(&["ID", "ENABLED", "MATCH", "PRIORITY", "ECOQOS", "NAME"]);
    for r in &reply.rules {
        let (prio, eco) = match r.action.as_ref() {
            Some(a) => (
                priority_name(a.priority).to_string(),
                if a.eco_qos { "yes" } else { "no" }.to_string(),
            ),
            None => ("-".to_string(), "-".to_string()),
        };
        t.push(vec![
            r.id.to_string(),
            if r.enabled { "yes" } else { "no" }.to_string(),
            r.match_image.clone(),
            prio,
            eco,
            r.name.clone(),
        ]);
    }
    t.render()
}

// ===========================================================================
// capabilities → GetCapabilities
// ===========================================================================

pub fn capabilities(conn: &mut Connection, json_mode: bool) -> Result<()> {
    let reply =
        conn.query(
            |mut c| async move { c.get_capabilities(atlas_ipc::CapabilitiesRequest {}).await },
        )?;
    emit(
        json_mode,
        capabilities_json(&reply),
        capabilities_table(&reply),
    );
    Ok(())
}

pub fn capabilities_json(reply: &CapabilitiesReply) -> Value {
    json!({
        "service_version": reply.service_version,
        "capability_flags": reply.capability_flags,
    })
}

pub fn capabilities_table(reply: &CapabilitiesReply) -> String {
    let mut out = format!("service version: {}\n", reply.service_version);
    out.push_str("capabilities:\n");
    if reply.capability_flags.is_empty() {
        out.push_str("  (none advertised)\n");
    } else {
        for f in &reply.capability_flags {
            out.push_str(&format!("  - {f}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_ipc::{ProcessRow, SystemGauges};

    /// The read-only guarantee, enforced structurally: no command maps to an RPC
    /// carrying a mutating verb, and each maps to an allow-listed read RPC.
    #[test]
    fn no_command_maps_to_a_mutating_rpc() {
        const ALLOWED: &[&str] = &[
            "AtlasQuery.GetSnapshot",
            "AtlasQuery.ListListeningPorts",
            "AtlasQuery.ListConnections",
            "AtlasQuery.FindResourceOwners",
            "AtlasQuery.QueryRange",
            "AtlasQuery.ListIncidents",
            "AtlasQuery.Diagnose",
            "AtlasQuery.ListServices",
            "AtlasQuery.ListStartup",
            "AtlasQuery.ListScheduledTasks",
            "AtlasQuery.Search",
            "AtlasRules.ListRules",
            "AtlasQuery.GetCapabilities",
        ];
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
            "Enable",
            "Bookmark",
            "Report",
        ];
        for (cmd, rpc) in COMMAND_RPCS {
            assert!(
                ALLOWED.contains(rpc),
                "command '{cmd}' maps to '{rpc}', not in the read-only allow-list"
            );
            for m in MUTATING_MARKERS {
                assert!(
                    !rpc.contains(m),
                    "command '{cmd}' RPC '{rpc}' contains mutating marker '{m}'"
                );
            }
        }
    }

    #[test]
    fn command_catalog_is_complete() {
        let names: Vec<&str> = COMMAND_RPCS.iter().map(|(c, _)| *c).collect();
        for expected in [
            "top",
            "ports",
            "connections",
            "locks",
            "history",
            "incidents",
            "diagnose",
            "services",
            "startup",
            "tasks",
            "search",
            "rules",
            "capabilities",
        ] {
            assert!(names.contains(&expected), "missing command '{expected}'");
        }
        assert_eq!(names.len(), 13);
    }

    fn fixture_snapshot() -> SnapshotReply {
        SnapshotReply {
            system: Some(SystemGauges {
                ts_ms: 1_700_000_000_000,
                cpu_permille: 234,
                mem_used: 8 * 1024 * 1024 * 1024,
                mem_total: 16 * 1024 * 1024 * 1024,
                commit_used: 9 * 1024 * 1024 * 1024,
                commit_limit: 20 * 1024 * 1024 * 1024,
                process_count: 250,
                thread_count: 3400,
                handle_count: 120_000,
                ..Default::default()
            }),
            processes: vec![
                ProcessRow {
                    pid: 1234,
                    parent_pid: 1,
                    image_name: "chrome.exe".into(),
                    session_id: 1,
                    create_time_100ns: 0,
                    cpu_permille: 155,
                    working_set: 512 * 1024 * 1024,
                    private_bytes: 400 * 1024 * 1024,
                    read_bps: 1000,
                    write_bps: 2000,
                    handle_count: 900,
                    thread_count: 45,
                    app_group: "chrome".into(),
                    role: 0,
                    ..Default::default()
                },
                ProcessRow {
                    pid: 4,
                    parent_pid: 0,
                    image_name: "System".into(),
                    session_id: 0,
                    create_time_100ns: 0,
                    cpu_permille: 5,
                    working_set: 1024 * 1024,
                    private_bytes: 512 * 1024,
                    read_bps: 0,
                    write_bps: 0,
                    handle_count: 3000,
                    thread_count: 200,
                    app_group: String::new(),
                    role: 0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_json_serializes_fixture() {
        let reply = fixture_snapshot();
        let v = snapshot_json(&reply);
        assert_eq!(v["returned"], 2);
        assert_eq!(v["system"]["cpu_percent"], 23.4);
        assert_eq!(v["system"]["process_count"], 250);
        assert_eq!(v["processes"][0]["pid"], 1234);
        assert_eq!(v["processes"][0]["image_name"], "chrome.exe");
        assert_eq!(v["processes"][0]["cpu_percent"], 15.5);
        // Round-trips through a string (scriptability contract).
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"chrome.exe\""));
    }

    #[test]
    fn snapshot_table_renders_fixture() {
        let reply = fixture_snapshot();
        let t = snapshot_table(&reply);
        assert!(t.contains("system  cpu 23.4%"));
        assert!(t.contains("chrome.exe"));
        assert!(t.contains("512.0 MiB"));
        // Header present.
        assert!(t.contains("PID"));
        assert!(t.contains("WORKING SET"));
    }

    #[test]
    fn capabilities_render_both_shapes() {
        let reply = CapabilitiesReply {
            service_version: "0.1.0".into(),
            capability_flags: vec!["process_snapshots".into(), "history_queries".into()],
        };
        let v = capabilities_json(&reply);
        assert_eq!(v["service_version"], "0.1.0");
        assert_eq!(v["capability_flags"][0], "process_snapshots");
        let t = capabilities_table(&reply);
        assert!(t.contains("service version: 0.1.0"));
        assert!(t.contains("- process_snapshots"));
    }

    #[test]
    fn metric_aliases_resolve_with_hyphens_and_underscores() {
        assert_eq!(parse_metric("sys-cpu").unwrap(), MetricKind::SysCpuPermille);
        assert_eq!(parse_metric("sys_cpu").unwrap(), MetricKind::SysCpuPermille);
        assert_eq!(parse_metric("cpu").unwrap(), MetricKind::CpuPermille);
        assert_eq!(parse_metric("working-set").unwrap(), MetricKind::WorkingSet);
        assert_eq!(
            parse_metric("SYS_MEM_USED").unwrap(),
            MetricKind::SysMemUsed
        );
        assert!(parse_metric("nonsense").is_err());
    }
}
